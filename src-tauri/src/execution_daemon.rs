//! Resident-service integration. Device execution uses the same grant and lease
//! checks as the existing device WebSocket, without a CLI-owned connection.
use super::*;
use crate::execution::{self, Entry, Envelope, Operation, Output};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    request: Value,
    output: Option<String>,
}

impl Daemon {
    pub(super) async fn exec_route(
        self: &Arc<Self>,
        method: &Method,
        rest: &[&str],
        uri: &hyper::Uri,
        body: Value,
        caller: Caller,
    ) -> Reply {
        // Local desktop authority is deliberately retained. Gateway credentials
        // never gain local filesystem or privileged desktop access through exec.
        if caller != Caller::App {
            return Err(forbidden());
        }
        match (method, rest) {
            (&Method::POST, []) => {
                let submission: Submission =
                    serde_json::from_value(body).map_err(|e| bad(e.to_string()))?;
                self.exec_submit(submission).await
            }
            (&Method::GET, ["jobs"]) => Ok((StatusCode::OK, self.execution.list())),
            (&Method::GET, ["jobs", id]) => {
                let seconds = query_str(uri, "waitSeconds")
                    .map(|s| s.parse::<u64>().map_err(|_| bad("Invalid waitSeconds")))
                    .transpose()?
                    .unwrap_or(0);
                if seconds > 120 {
                    return Err(bad("waitSeconds must be at most 120"));
                }
                let entry = self
                    .execution
                    .get(id)
                    .map_err(|e| (StatusCode::NOT_FOUND, e))?;
                Ok((StatusCode::OK, entry.wait(seconds).await))
            }
            (&Method::POST, ["jobs", id, "cancel"]) => {
                let request = json!({"apiVersion":execution::VERSION,"kind":"Request","metadata":{},"spec":{"operation":"jobs.cancel","id":id}});
                self.exec_submit(Submission {
                    request,
                    output: None,
                })
                .await?;
                Ok((
                    StatusCode::ACCEPTED,
                    json!({"job":self.execution.get(id).map_err(bad)?.receipt()["job"]}),
                ))
            }
            (&Method::POST, ["jobs", id, "reveal"]) => {
                let entry = self
                    .execution
                    .get(id)
                    .map_err(|e| (StatusCode::NOT_FOUND, e))?;
                let path = entry
                    .job
                    .lock()
                    .unwrap()
                    .output
                    .clone()
                    .ok_or_else(|| bad("This job has no recorded output file"))?;
                if !Path::new(&path).is_file() {
                    return Err(bad("The recorded output file does not exist"));
                }
                let reveal = path.clone();
                tokio::task::spawn_blocking(move || {
                    #[cfg(target_os = "macos")]
                    let result = std::process::Command::new("open")
                        .args(["-R", &reveal])
                        .status();
                    #[cfg(target_os = "windows")]
                    let result = std::process::Command::new("explorer.exe")
                        .arg(format!("/select,{reveal}"))
                        .status();
                    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                    let result = std::process::Command::new("xdg-open")
                        .arg(Path::new(&reveal).parent().unwrap())
                        .status();
                    match result {
                        Ok(status) if status.success() => Ok(()),
                        Ok(_) => Err(bad("Could not reveal output")),
                        Err(e) => Err(bad(e.to_string())),
                    }
                })
                .await
                .map_err(|e| bad(e.to_string()))??;
                Ok((StatusCode::OK, json!({"output":path})))
            }
            _ => Err(not_found()),
        }
    }

    async fn exec_submit(self: &Arc<Self>, submission: Submission) -> Reply {
        let (request, operation) = Envelope::parse(submission.request).map_err(bad)?;
        let output = submission
            .output
            .as_deref()
            .map(Output::prepare)
            .transpose()
            .map_err(bad)?;
        // Resolve observation targets before accepting a cancellation action.
        if let Operation::Cancel { id } = &operation {
            self.execution
                .get(id)
                .map_err(|e| (StatusCode::NOT_FOUND, e))?;
        }
        if !operation.action() {
            drop(self.admitted(Caller::App)?);
            let result = self.exec_observe(operation).await?;
            let response = json!({"result":result});
            if let Some(output) = output {
                output.write(&response).map_err(bad)?;
            }
            return Ok((StatusCode::OK, response));
        }
        let entry = {
            let _admission = self.admitted(Caller::App)?;
            self.execution
                .accept(request, output.as_ref())
                .map_err(bad)?
        };
        let receipt = entry.receipt();
        let daemon = self.clone();
        tokio::spawn(async move {
            let outcome = match daemon.execution.update(&entry, |job| {
                job.status = "running".into();
                job.started_at = Some(now_ms());
            }) {
                Ok(()) => daemon.exec_action(entry.clone(), operation).await,
                Err(error) => Err(error),
            };
            daemon.execution.finish(&entry, outcome, output);
        });
        Ok((StatusCode::ACCEPTED, receipt))
    }

    async fn exec_observe(
        self: &Arc<Self>,
        operation: Operation,
    ) -> Result<Value, (StatusCode, String)> {
        match operation {
            Operation::Describe => Ok(execution::describe()),
            Operation::SerialList => {
                Ok(json!({"service":"running","ports":self.snapshot()["ports"]}))
            }
            Operation::SerialPorts => {
                Ok(json!({"service":"running","ports":*self.system.lock().unwrap()}))
            }
            Operation::SerialStatus { port } => {
                let ports = self.ports.lock().unwrap();
                let port = ports
                    .iter()
                    .find(|p| p.config.id == port)
                    .ok_or_else(not_found)?;
                Ok(self.view(port))
            }
            Operation::List => Ok(self.execution.list()),
            Operation::Get { id, wait } => Ok(self
                .execution
                .get(&id)
                .map_err(|e| (StatusCode::NOT_FOUND, e))?
                .wait(wait)
                .await),
            Operation::Logs {
                source,
                id,
                cursor,
                limit,
                wait,
            } => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
                loop {
                    let value = if source == "compile" {
                        #[cfg(unix)]
                        {
                            // Accept either an executor ID or an underlying compile ID.
                            let source_id = self
                                .execution
                                .get(&id)
                                .ok()
                                .and_then(|e| e.job.lock().unwrap().source_job_id.clone())
                                .unwrap_or_else(|| id.clone());
                            let job = self.compile.job(&source_id).ok_or_else(not_found)?;
                            let (bytes, next, truncated) =
                                job.log.lock().unwrap().read(cursor, limit as usize);
                            json!({"text":String::from_utf8_lossy(&bytes),"next":next,"truncated":truncated,"jobId":source_id,"finished":job.status().finished()})
                        }
                        #[cfg(not(unix))]
                        return Err((
                            StatusCode::NOT_IMPLEMENTED,
                            "Compiling is not available".into(),
                        ));
                    } else if remote::split(&id).is_some() {
                        self.remotes.logs(&id, cursor, limit.min(2000)).await?.1
                    } else {
                        self.log_page(&id, cursor, limit.min(2000))?.1
                    };
                    let has_data = if source == "compile" {
                        value["text"].as_str().is_some_and(|s| !s.is_empty())
                    } else {
                        value["entries"].as_array().is_some_and(|a| !a.is_empty())
                    };
                    if has_data
                        || value["finished"] == true
                        || tokio::time::Instant::now() >= deadline
                    {
                        return Ok(value);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            _ => Err(bad("Expected an observation operation")),
        }
    }

    async fn exec_action(
        self: &Arc<Self>,
        entry: Arc<Entry>,
        operation: Operation,
    ) -> Result<Value, String> {
        if entry.cancelled() {
            return Err("Cancelled".into());
        }
        match operation {
            Operation::Cancel { id } => {
                let target = self.execution.get(&id)?;
                if !target.job.lock().unwrap().finished() {
                    target.cancel.store(true, Ordering::SeqCst);
                }
                Ok(target.receipt())
            }
            #[cfg(unix)]
            Operation::Compile(build) => {
                let request = serde_json::from_value(build).map_err(|e| e.to_string())?;
                let title = entry.job.lock().unwrap().name.clone();
                let (job, _events) = {
                    let _admission = self.admitted(Caller::App).map_err(|(_, e)| e)?;
                    self.compile
                        .submit("GizMoth request".into(), title, request)?
                };
                if let Err(error) = self
                    .execution
                    .update(&entry, |entry| entry.source_job_id = Some(job.id.clone()))
                {
                    self.compile.cancel_job(&job);
                    return Err(error);
                }
                loop {
                    if entry.cancelled() {
                        self.compile.cancel_job(&job);
                    }
                    if job.status().finished() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                match job.status() {
                    crate::compile::jobs::Status::Succeeded => Ok(job.summary()),
                    crate::compile::jobs::Status::Cancelled => {
                        entry.cancel.store(true, Ordering::SeqCst);
                        Err("Compile cancelled".into())
                    }
                    _ => Err(job
                        .state
                        .lock()
                        .unwrap()
                        .error
                        .clone()
                        .unwrap_or_else(|| "Compile failed".into())),
                }
            }
            Operation::Device {
                port,
                command,
                params,
            } => self.exec_device(entry, port, command, params).await,
            _ => Err("Expected an action operation".into()),
        }
    }

    async fn exec_device(
        self: &Arc<Self>,
        entry: Arc<Entry>,
        port: String,
        command: String,
        params: Value,
    ) -> Result<Value, String> {
        // Imported ports require their owning node's remote Session lifecycle.
        // The local queue is the supported executor transport; do not elevate
        // imported commands to desktop HTTP to bypass the owning node's grants.
        if remote::split(&port).is_some() {
            return Err("Execution commands currently require a local serial port; use the existing remote device session for imported ports".into());
        }
        let title = entry.job.lock().unwrap().name.clone();
        let mut joined = self
            .join_pipe(
                &port,
                "GizMoth request".into(),
                title,
                "Execution request".into(),
                Caller::App,
                false,
            )
            .map_err(|(_, e)| e)?;
        let outcome = self
            .exec_granted_device(&entry, &port, &command, params, &mut joined)
            .await;
        self.leave_pipe(&port, &joined.shared, joined.pipe_id, "Execution finished");
        outcome
    }

    async fn exec_granted_device(
        self: &Arc<Self>,
        entry: &Arc<Entry>,
        port: &str,
        command: &str,
        params: Value,
        joined: &mut Joined,
    ) -> Result<Value, String> {
        self.execution
            .update(entry, |job| job.status = "awaiting_approval".into())?;
        loop {
            if entry.cancelled() {
                return Err("Cancelled while waiting for device approval".into());
            }
            let control = joined.notices.borrow_and_update().clone();
            match control {
                Control::Granted => break,
                Control::End(_, reason) => return Err(reason),
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.execution
            .update(entry, |job| job.status = "running".into())?;
        if entry.cancelled() {
            return Err("Cancelled".into());
        }
        let request = ws::parse(&json!({"id":"exec","method":command,"params":params}).to_string())
            .map_err(|(_, e)| e)?;
        if command == "send" {
            let (completion, mut completed) = tokio::sync::oneshot::channel();
            self.send_owned_confirmed(
                port,
                SendText {
                    text: request.params["text"].as_str().unwrap().into(),
                    line_ending: request.params["lineEnding"].as_str().map(str::to_owned),
                },
                Caller::App,
                Some(joined.pipe_id),
                Some(completion),
            )
            .map_err(|(_, error)| error)?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                tokio::select! {
                    result = &mut completed => {
                        result.map_err(|_| "Serial worker ended before confirming the write".to_string())??;
                        return Ok(json!({"sent":true}));
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if entry.cancelled() { return Err("Serial send cancelled".into()); }
                        if tokio::time::Instant::now() >= deadline { return Err("Serial write confirmation timed out".into()); }
                    }
                }
            }
        }
        let (_, value) = self
            .ws_command(port, joined.pipe_id, Caller::App, &request)
            .map_err(|(_, e)| e)?;
        let id = value["id"]
            .as_str()
            .or_else(|| value["job"]["id"].as_str())
            .ok_or("Device command did not return a job ID")?
            .to_string();
        if let Err(error) = self
            .execution
            .update(entry, |job| job.source_job_id = Some(id.clone()))
        {
            let _ = self.cancel_job(&id);
            return Err(error);
        }
        loop {
            if entry.cancelled() {
                let _ = self.cancel_job(&id);
            }
            let job = {
                let live = joined.shared.live.lock().unwrap();
                live.job
                    .as_ref()
                    .filter(|j| j.id == id)
                    .or_else(|| live.history.iter().find(|j| j.id == id))
                    .cloned()
            };
            if let Some(job) = job {
                match job.state {
                    JobState::Succeeded => return Ok(json!(job)),
                    JobState::Failed => {
                        return Err(job.error.unwrap_or_else(|| "Device job failed".into()))
                    }
                    JobState::Cancelled => {
                        entry.cancel.store(true, Ordering::SeqCst);
                        return Err("Device job cancelled".into());
                    }
                    _ => {}
                }
            } else {
                return Err("Device job no longer available".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

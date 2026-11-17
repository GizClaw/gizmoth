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

async fn exec_blocking<T: Send + 'static>(
    action: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|error| error.to_string())
}

// History updates hold the job mutex during fsync. Observe it on the blocking
// pool as well, while keeping bounded waits on the asynchronous timer queue.
async fn exec_wait(entry: Arc<Entry>, seconds: u64) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds.min(120));
    loop {
        let current = entry.clone();
        let expired = tokio::time::Instant::now() >= deadline;
        let receipt = exec_blocking(move || {
            let finished = current.job.lock().unwrap().finished();
            (finished || expired).then(|| current.receipt())
        })
        .await?;
        if let Some(receipt) = receipt {
            return Ok(receipt);
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
        )
        .await;
    }
}

impl Daemon {
    async fn exec_update(
        self: &Arc<Self>,
        entry: &Arc<Entry>,
        update: impl FnOnce(&mut execution::ExecJob) + Send + 'static,
    ) -> Result<(), String> {
        let service = self.execution.clone();
        let entry = entry.clone();
        exec_blocking(move || service.update(&entry, update)).await?
    }
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
            (&Method::GET, ["jobs"]) => {
                let service = self.execution.clone();
                Ok((
                    StatusCode::OK,
                    exec_blocking(move || service.list()).await.map_err(bad)?,
                ))
            }
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
                Ok((
                    StatusCode::OK,
                    exec_wait(entry, seconds).await.map_err(bad)?,
                ))
            }
            (&Method::POST, ["jobs", id, "cancel"]) => {
                let request = json!({"apiVersion":execution::VERSION,"kind":"Request","metadata":{},"spec":{"operation":"jobs.cancel","id":id}});
                self.exec_submit(Submission {
                    request,
                    output: None,
                })
                .await?;
                let entry = self.execution.get(id).map_err(bad)?;
                let receipt = exec_blocking(move || entry.receipt()).await.map_err(bad)?;
                Ok((StatusCode::ACCEPTED, json!({"job":receipt["job"]})))
            }
            (&Method::POST, ["jobs", id, "reveal"]) => {
                let entry = self
                    .execution
                    .get(id)
                    .map_err(|e| (StatusCode::NOT_FOUND, e))?;
                let path = exec_blocking(move || {
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
                        Ok(status) if status.success() => Ok(path),
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
        let output = exec_blocking(move || {
            submission
                .output
                .as_deref()
                .map(Output::prepare)
                .transpose()
        })
        .await
        .map_err(bad)?
        .map_err(bad)?;
        // Resolve observation targets before accepting a cancellation action.
        if let Operation::Cancel { id } = &operation {
            self.execution
                .get(id)
                .map_err(|e| (StatusCode::NOT_FOUND, e))?;
        }
        if !operation.action() {
            let daemon = self.clone();
            exec_blocking(move || daemon.admitted(Caller::App).map(drop))
                .await
                .map_err(bad)??;
            let result = self.exec_observe(operation).await?;
            let response = json!({"result":result});
            let response = exec_blocking(move || {
                if let Some(output) = output {
                    output.write(&response).map_err(bad)?;
                }
                Ok::<_, (StatusCode, String)>(response)
            })
            .await
            .map_err(bad)??;
            return Ok((StatusCode::OK, response));
        }
        let daemon = self.clone();
        let (entry, receipt, output) = exec_blocking(move || {
            // Acquire admission here: a history fsync must never hold this
            // mutex on a Tokio worker, including while another task accepts.
            let _admission = daemon.admitted(Caller::App)?;
            let entry = daemon
                .execution
                .accept(request, output.as_ref())
                .map_err(bad)?;
            let receipt = entry.receipt();
            Ok::<_, (StatusCode, String)>((entry, receipt, output))
        })
        .await
        .map_err(bad)??;
        let daemon = self.clone();
        tokio::spawn(async move {
            let outcome = match daemon
                .exec_update(&entry, |job| {
                    job.status = "running".into();
                    job.started_at = Some(now_ms());
                })
                .await
            {
                Ok(()) => daemon.exec_action(entry.clone(), operation).await,
                Err(error) => Err(error),
            };
            let _ = exec_blocking(move || daemon.execution.finish(&entry, outcome, output)).await;
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
                let daemon = self.clone();
                exec_blocking(
                    move || json!({"service":"running","ports":daemon.snapshot()["ports"]}),
                )
                .await
                .map_err(bad)
            }
            Operation::SerialPorts => {
                let daemon = self.clone();
                exec_blocking(
                    move || json!({"service":"running","ports":*daemon.system.lock().unwrap()}),
                )
                .await
                .map_err(bad)
            }
            Operation::SerialStatus { port } => {
                let daemon = self.clone();
                exec_blocking(move || {
                    let ports = daemon.ports.lock().unwrap();
                    let port = ports
                        .iter()
                        .find(|p| p.config.id == port)
                        .ok_or_else(not_found)?;
                    Ok(daemon.view(port))
                })
                .await
                .map_err(bad)?
            }
            Operation::List => {
                let service = self.execution.clone();
                exec_blocking(move || service.list()).await.map_err(bad)
            }
            Operation::Get { id, wait } => {
                let entry = self
                    .execution
                    .get(&id)
                    .map_err(|e| (StatusCode::NOT_FOUND, e))?;
                exec_wait(entry, wait).await.map_err(bad)
            }
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
                            let daemon = self.clone();
                            let id = id.clone();
                            exec_blocking(move || {
                                // Accept either an executor ID or an underlying compile ID.
                                let source_id = daemon.execution.get(&id).ok()
                                    .and_then(|e| e.job.lock().unwrap().source_job_id.clone())
                                    .unwrap_or(id);
                                let job = daemon.compile.job(&source_id).ok_or_else(not_found)?;
                                let (bytes, next, truncated) = job.log.lock().unwrap().read(cursor, limit as usize);
                                Ok::<_, (StatusCode, String)>(json!({"text":String::from_utf8_lossy(&bytes),"next":next,"truncated":truncated,"jobId":source_id,"finished":job.status().finished()}))
                            }).await.map_err(bad)??
                        }
                        #[cfg(not(unix))]
                        return Err((
                            StatusCode::NOT_IMPLEMENTED,
                            "Compiling is not available".into(),
                        ));
                    } else if remote::split(&id).is_some() {
                        self.remotes.logs(&id, cursor, limit.min(2000)).await?.1
                    } else {
                        let daemon = self.clone();
                        let id = id.clone();
                        exec_blocking(move || daemon.log_page(&id, cursor, limit.min(2000)))
                            .await
                            .map_err(bad)??
                            .1
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
                exec_blocking(move || {
                    if !target.job.lock().unwrap().finished() {
                        target.cancel.store(true, Ordering::SeqCst);
                    }
                    target.receipt()
                })
                .await
            }
            #[cfg(unix)]
            Operation::Compile(build) => {
                let request = serde_json::from_value(build).map_err(|e| e.to_string())?;
                let daemon = self.clone();
                let current = entry.clone();
                let (job, _events) = exec_blocking(move || {
                    let title = current.job.lock().unwrap().name.clone();
                    let _admission = daemon.admitted(Caller::App).map_err(|(_, e)| e)?;
                    daemon
                        .compile
                        .submit("GizMoth request".into(), title, request)
                })
                .await??;
                let id = job.id.clone();
                if let Err(error) = self
                    .exec_update(&entry, move |entry| entry.source_job_id = Some(id))
                    .await
                {
                    let compile = self.compile.clone();
                    let _ = exec_blocking(move || compile.cancel_job(&job)).await;
                    return Err(error);
                }
                loop {
                    let current = job.clone();
                    let compile = self.compile.clone();
                    let cancelled = entry.cancelled();
                    let finished = exec_blocking(move || {
                        if cancelled {
                            compile.cancel_job(&current);
                        }
                        current.status().finished()
                    })
                    .await?;
                    if finished {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                exec_blocking(move || match job.status() {
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
                })
                .await?
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
        let daemon = self.clone();
        let current = entry.clone();
        let target = port.clone();
        let mut joined = exec_blocking(move || {
            let title = current.job.lock().unwrap().name.clone();
            daemon
                .join_pipe(
                    &target,
                    "GizMoth request".into(),
                    title,
                    "Execution request".into(),
                    Caller::App,
                    false,
                )
                .map_err(|(_, e)| e)
        })
        .await??;
        let outcome = self
            .exec_granted_device(&entry, &port, &command, params, &mut joined)
            .await;
        let daemon = self.clone();
        exec_blocking(move || {
            daemon.leave_pipe(&port, &joined.shared, joined.pipe_id, "Execution finished");
        })
        .await?;
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
        self.exec_update(entry, |job| job.status = "awaiting_approval".into())
            .await?;
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
        self.exec_update(entry, |job| job.status = "running".into())
            .await?;
        if entry.cancelled() {
            return Err("Cancelled".into());
        }
        let request = ws::parse(&json!({"id":"exec","method":command,"params":params}).to_string())
            .map_err(|(_, e)| e)?;
        if command == "send" {
            let (completion, mut completed) = tokio::sync::oneshot::channel();
            let daemon = self.clone();
            let port = port.to_string();
            let pipe_id = joined.pipe_id;
            exec_blocking(move || {
                daemon
                    .send_owned_confirmed(
                        &port,
                        SendText {
                            text: request.params["text"].as_str().unwrap().into(),
                            line_ending: request.params["lineEnding"].as_str().map(str::to_owned),
                        },
                        Caller::App,
                        Some(pipe_id),
                        Some(completion),
                    )
                    .map_err(|(_, error)| error)
            })
            .await??;
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
        let daemon = self.clone();
        let target = port.to_string();
        let pipe_id = joined.pipe_id;
        let (_, value) = exec_blocking(move || {
            daemon
                .ws_command(&target, pipe_id, Caller::App, &request)
                .map_err(|(_, e)| e)
        })
        .await??;
        let id = value["id"]
            .as_str()
            .or_else(|| value["job"]["id"].as_str())
            .ok_or("Device command did not return a job ID")?
            .to_string();
        let source_id = id.clone();
        if let Err(error) = self
            .exec_update(entry, move |job| job.source_job_id = Some(source_id))
            .await
        {
            let daemon = self.clone();
            let _ = exec_blocking(move || daemon.cancel_job(&id)).await;
            return Err(error);
        }
        loop {
            let daemon = self.clone();
            let shared = joined.shared.clone();
            let id = id.clone();
            let cancelled = entry.cancelled();
            let job = exec_blocking(move || {
                if cancelled {
                    let _ = daemon.cancel_job(&id);
                }
                let live = shared.live.lock().unwrap();
                live.job
                    .as_ref()
                    .filter(|j| j.id == id)
                    .or_else(|| live.history.iter().find(|j| j.id == id))
                    .cloned()
            })
            .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn observing_a_locked_history_job_keeps_the_runtime_responsive() {
        let directory = tempfile::tempdir().unwrap();
        let service = execution::Service::new(directory.path()).unwrap();
        let (request, _) = Envelope::parse(json!({
            "apiVersion": execution::VERSION,
            "kind": "Request",
            "metadata": {},
            "spec": {"operation": "serial.refresh", "port": "test"}
        }))
        .unwrap();
        let entry = service.accept(request, None).unwrap();
        let writer = entry.clone();
        let (locked, ready) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _job = writer.job.lock().unwrap();
            locked.send(()).unwrap();
            // Bound a regression's failure rather than hanging the test suite.
            let _ = released.recv_timeout(Duration::from_secs(2));
        });
        ready.recv().unwrap();
        let started = std::time::Instant::now();
        let waiting = tokio::spawn(exec_wait(entry, 0));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let elapsed = started.elapsed();
        release.send(()).ok();
        let receipt = waiting.await.unwrap().unwrap();
        thread.join().unwrap();
        assert!(
            elapsed < Duration::from_secs(1),
            "History observation blocked the runtime: {elapsed:?}"
        );
        assert_eq!(receipt["job"]["status"], "queued");
    }

    #[tokio::test]
    async fn history_wait_returns_completion_and_preserves_observation_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let service = execution::Service::new(directory.path()).unwrap();
        let (request, _) = Envelope::parse(json!({
            "apiVersion": execution::VERSION,
            "kind": "Request",
            "metadata": {},
            "spec": {"operation": "serial.refresh", "port": "test"}
        }))
        .unwrap();
        let entry = service.accept(request, None).unwrap();
        assert_eq!(exec_wait(entry.clone(), 0).await.unwrap(), entry.receipt());
        let current = entry.clone();
        let finishing = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            exec_blocking(move || service.finish(&current, Ok(json!({"done":true})), None))
                .await
                .unwrap();
        });
        let receipt = exec_wait(entry, 1).await.unwrap();
        finishing.await.unwrap();
        assert_eq!(receipt["job"]["status"], "succeeded");
        assert_eq!(receipt["result"]["done"], true);
    }
}

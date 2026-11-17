//! The single JSON execution entry point. The CLI only submits/observes; the
//! resident service retains every action after this process exits.
use super::*;
use std::io::{Read, Write};
use std::time::Duration;

const MAX_REQUEST: u64 = 64 * 1024;
#[derive(Debug, PartialEq)]
pub struct Args {
    pub input: String,
    pub output: Option<String>,
    pub detach: bool,
}

pub fn parse(args: &[String]) -> R<(Args, Option<PathBuf>)> {
    let mut input = None;
    let mut output = None;
    let mut detach = false;
    let mut data = None;
    let mut request = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "request" if !request => request = true,
            "--data-dir" if data.is_none() => {
                data = Some(PathBuf::from(
                    iter.next()
                        .filter(|s| !s.is_empty() && !s.starts_with('-'))
                        .ok_or("--data-dir requires a directory")?,
                ))
            }
            "-f" if request && input.is_none() => {
                input = Some(
                    iter.next()
                        .filter(|s| !s.is_empty() && (*s == "-" || !s.starts_with('-')))
                        .ok_or("-f requires a file path or -")?
                        .clone(),
                )
            }
            "--output" if request && output.is_none() => {
                output = Some(
                    iter.next()
                        .filter(|s| !s.is_empty() && !s.starts_with('-'))
                        .ok_or("--output requires a file path")?
                        .clone(),
                )
            }
            "--detach" if request && !detach => detach = true,
            _ => return Err(format!("Unexpected or duplicate request argument: {arg}")),
        }
    }
    if !request {
        return Err("Expected request".into());
    }
    Ok((
        Args {
            input: input
                .ok_or("Usage: gizmoth request -f <request.json|-> [--output <path>] [--detach]")?,
            output,
            detach,
        },
        data,
    ))
}

fn read_request(mut reader: impl Read) -> R<Value> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_REQUEST + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_REQUEST {
        return Err("Execution request exceeds 64 KiB".into());
    }
    let value = serde_json::from_slice(&bytes).map_err(|e| format!("Invalid request JSON: {e}"))?;
    Envelope::parse(value).map(|(request, _)| serde_json::to_value(request).unwrap())
}

pub fn run(data: &Path, args: Args) -> i32 {
    let outcome = (|| {
        let request = if args.input == "-" {
            read_request(std::io::stdin().lock())?
        } else {
            read_request(fs::File::open(&args.input).map_err(|e| e.to_string())?)?
        };
        let output = args
            .output
            .map(|path| {
                std::path::absolute(path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        // Validate before launching a daemon or accepting a history entry.
        if let Some(path) = &output {
            let _ = Output::prepare(path)?;
        }
        let connection = crate::serial::client::ensure(data)?;
        let mut response = connection.request(
            "POST",
            "/v1/exec",
            Some(&json!({"request":request,"output":output})),
            Duration::from_secs(125),
        )?;
        if !args.detach {
            while response.get("job").is_some() && !terminal(&response) {
                let id = response["job"]["id"]
                    .as_str()
                    .ok_or("Service did not return an execution ID")?;
                response = connection.request(
                    "GET",
                    &format!("/v1/exec/jobs/{id}?waitSeconds=20"),
                    None,
                    Duration::from_secs(25),
                )?;
            }
        }
        Ok(stdout_response(response, output.as_deref()))
    })();
    print(outcome, std::io::stdout().lock(), std::io::stderr().lock())
}
fn stdout_response(mut response: Value, output: Option<&str>) -> Value {
    if let Some(path) = output {
        if let Some(object) = response
            .as_object_mut()
            .filter(|object| object.contains_key("job"))
        {
            object.remove("result");
        } else {
            response = json!({"output": path});
        }
    }
    response
}

pub fn error(message: String) -> i32 {
    print(
        Err(message),
        std::io::stdout().lock(),
        std::io::stderr().lock(),
    )
}
fn terminal(response: &Value) -> bool {
    matches!(
        response["job"]["status"].as_str(),
        Some("succeeded" | "failed" | "cancelled" | "interrupted")
    )
}
fn print(outcome: R<Value>, mut stdout: impl Write, mut stderr: impl Write) -> i32 {
    let (value, code) = match outcome {
        Ok(value) => {
            let failed = matches!(
                value["job"]["status"].as_str(),
                Some("failed" | "cancelled" | "interrupted")
            );
            (value, if failed { 1 } else { 0 })
        }
        Err(error) => (json!({"error":{"message":error}}), 2),
    };
    if serde_json::to_writer(&mut stdout, &value).is_err() || stdout.write_all(b"\n").is_err() {
        let _ = writeln!(stderr, "Unable to write execution response");
        return 1;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(input: &[&str]) -> Vec<String> {
        input.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn cli_accepts_request_file_stdin_output_and_detach_without_data_directory() {
        for input in ["request.json", "-", "exec"] {
            let (parsed, data) = parse(&args(&[
                "request",
                "-f",
                input,
                "--detach",
                "--output",
                "result.json",
            ]))
            .unwrap();
            assert_eq!(
                parsed,
                Args {
                    input: input.into(),
                    output: Some("result.json".into()),
                    detach: true
                }
            );
            assert_eq!(data, None);
        }
    }
    #[test]
    fn cli_accepts_data_directory_before_or_after_request() {
        for input in [
            vec![
                "--data-dir",
                "/tmp/isolated",
                "request",
                "-f",
                "request.json",
            ],
            vec![
                "request",
                "--data-dir",
                "/tmp/isolated",
                "-f",
                "request.json",
            ],
            vec![
                "request",
                "-f",
                "request.json",
                "--data-dir",
                "/tmp/isolated",
            ],
        ] {
            let (parsed, data) = parse(&args(&input)).unwrap();
            assert_eq!(
                parsed,
                Args {
                    input: "request.json".into(),
                    output: None,
                    detach: false
                }
            );
            assert_eq!(data, Some(PathBuf::from("/tmp/isolated")));
        }
    }
    #[test]
    fn cli_rejects_exec_alias_and_malformed_or_duplicate_request_arguments() {
        for invalid in [
            vec!["exec", "-f", "-"],
            vec!["--data-dir", "/tmp/isolated", "exec", "-f", "-"],
            vec!["request"],
            vec!["request", "request", "-f", "-"],
            vec!["request", "-f"],
            vec!["request", "-f", ""],
            vec!["request", "-f", "--detach"],
            vec!["request", "-f", "-", "-f", "other.json"],
            vec!["request", "-f", "-", "--detach", "--detach"],
            vec!["request", "-f", "-", "--shell"],
            vec!["request", "-f", "-", "--output"],
            vec!["request", "-f", "-", "--output", "--detach"],
            vec!["request", "-f", "-", "--output", "a", "--output", "b"],
            vec!["request", "-f", "-", "--data-dir"],
            vec!["--data-dir", "", "request", "-f", "-"],
            vec!["--data-dir", "--detach", "request", "-f", "-"],
            vec!["--data-dir", "a", "request", "-f", "-", "--data-dir", "b"],
            vec!["-f", "-", "request"],
        ] {
            assert!(parse(&args(&invalid)).is_err(), "{invalid:?}");
        }
    }
    #[test]
    fn output_file_prints_only_receipt_or_path_without_duplicating_result_body() {
        let response = json!({"job":{"id":"job-id","status":"succeeded","output":"/tmp/result.json"},"result":{"large":"payload"}});
        assert_eq!(stdout_response(response.clone(), None), response);
        let receipt = stdout_response(response, Some("/tmp/result.json"));
        assert!(receipt.get("result").is_none());
        assert_eq!(receipt["job"]["id"], "job-id");
        assert_eq!(
            stdout_response(
                json!({"result":{"large":"payload"}}),
                Some("/tmp/read.json")
            ),
            json!({"output":"/tmp/read.json"})
        );
        let receipt = stdout_response(
            json!({"job":{"id":"detached","status":"queued"}}),
            Some("/tmp/later.json"),
        );
        assert_eq!(receipt["job"]["id"], "detached");
    }
    #[test]
    fn request_argument_errors_use_structured_stdout_and_exit_code_two() {
        for (input, message) in [
            (vec![], "Expected request"),
            (
                vec!["request"],
                "Usage: gizmoth request -f <request.json|-> [--output <path>] [--detach]",
            ),
            (
                vec!["request", "request"],
                "Unexpected or duplicate request argument: request",
            ),
            (vec!["request", "-f"], "-f requires a file path or -"),
            (
                vec!["--data-dir", "", "request", "-f", "-"],
                "--data-dir requires a directory",
            ),
        ] {
            let mut output = Vec::new();
            let mut errors = Vec::new();
            assert_eq!(
                print(
                    parse(&args(&input)).map(|_| json!({})),
                    &mut output,
                    &mut errors
                ),
                2
            );
            let value: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(value, json!({"error":{"message":message}}));
            assert!(output.ends_with(b"\n"));
            assert!(errors.is_empty());
        }
    }
    #[test]
    fn request_json_rejects_invalid_envelopes_and_oversized_input() {
        assert!(read_request(&b"not json"[..]).is_err());
        assert!(read_request(&b"{}"[..]).is_err());
        assert!(read_request(&vec![b' '; 65537][..]).is_err());
    }
}

use crate::{
    cli::Check,
    control::{self, Operation},
    error::{Error, Result},
    state::{self, Paths, Session, Status},
};
use serde::{Deserialize, Serialize};
use std::{
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TargetCheck {
    pub checked_at: u64,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum Outcome {
    Reachable {
        http_status: u16,
    },
    Ok {
        http_status: u16,
    },
    UnexpectedStatus {
        http_status: u16,
        expected_status: u16,
    },
    Timeout,
    ConnectionError {
        curl_exit: Option<i32>,
    },
}

impl TargetCheck {
    pub fn result(&self) -> &'static str {
        match self.outcome {
            Outcome::Reachable { .. } => "reachable",
            Outcome::Ok { .. } => "ok",
            Outcome::UnexpectedStatus { .. } => "unexpected_status",
            Outcome::Timeout => "timeout",
            Outcome::ConnectionError { .. } => "connection_error",
        }
    }

    pub fn label(&self) -> String {
        let description = match self.outcome {
            Outcome::Reachable { http_status } => format!("reachable (HTTP {http_status})"),
            Outcome::Ok { http_status } => format!("ok (HTTP {http_status})"),
            Outcome::UnexpectedStatus {
                http_status,
                expected_status,
            } => {
                format!("HTTP {http_status} (expected {expected_status})")
            }
            Outcome::Timeout => "timeout".into(),
            Outcome::ConnectionError { curl_exit: Some(0) } => "no HTTP response".into(),
            Outcome::ConnectionError {
                curl_exit: Some(code),
            } => {
                format!("connection error (curl exit {code})")
            }
            Outcome::ConnectionError { curl_exit: None } => {
                "connection error (curl interrupted)".into()
            }
        };
        let age = state::now().saturating_sub(self.checked_at);
        format!("{description} [checked {age}s ago]")
    }
}

pub fn run(paths: &Paths, stored: &Session, args: &Check) -> Result<i32> {
    if !args.http.starts_with('/')
        || args.http.len() > 2048
        || !args.http.bytes().all(|b| b.is_ascii_graphic() && b != b'#')
    {
        return Err(Error::with_code(
            2,
            "--http requires a path beginning with '/' without spaces, control characters, or fragments",
        ));
    }
    if stored.terminal() {
        return Err(Error::new("only running sessions can be checked"));
    }
    let live = control::query(paths, stored, Operation::Status, Duration::from_millis(250))?;
    let pid = live
        .ssh_pid
        .filter(|_| live.state == Status::Running)
        .ok_or_else(|| Error::new("only running sessions can be checked"))?;
    let port = match (args.port, live.mappings.as_slice()) {
        (Some(port), mappings) if mappings.iter().any(|mapping| mapping.local == port) => port,
        (Some(port), _) => {
            return Err(Error::with_code(
                2,
                format!("no mapping for local port {port}"),
            ));
        }
        (None, [mapping]) => mapping.local,
        (None, _) => {
            return Err(Error::with_code(
                2,
                "--port is required for multiple mappings",
            ));
        }
    };

    // Checking both sides of the request prevents attributing another local listener's response
    // to SSH if the session has not bound its port yet or shuts down during the request.
    verify_listener(pid, port)?;
    let check = TargetCheck {
        outcome: http_check(port, &args.http, args.expect_status)?,
        checked_at: state::now(),
    };
    verify_listener(pid, port)?;
    let code = i32::from(!matches!(
        check.outcome,
        Outcome::Reachable { .. } | Outcome::Ok { .. }
    ));
    let label = check.label();
    control::query(
        paths,
        &live,
        Operation::RecordCheck {
            port,
            ssh_pid: pid,
            retry_count: live.retry_count,
            check,
        },
        Duration::from_secs(1),
    )
    .map_err(|error| Error::new(format!("cannot record target check: {error}")))?;
    println!("127.0.0.1:{port}: {label}");
    Ok(code)
}

fn verify_listener(pid: u32, port: u16) -> Result<()> {
    let address = format!("127.0.0.1:{port}");
    let output = run_tool(
        "lsof",
        &[
            "-b",
            "-O",
            "-w",
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            &format!("-iTCP@{address}"),
            "-sTCP:LISTEN",
            "-Fpn",
        ],
        Duration::from_secs(2),
    )?;
    if let Some(output) = output
        && output.status.success()
    {
        let fields = String::from_utf8_lossy(&output.stdout);
        let owner = format!("p{pid}");
        let listener = format!("n{address}");
        let mut owned = false;
        for field in fields.lines() {
            if field.starts_with('p') {
                owned = field == owner;
            } else if owned && field == listener {
                return Ok(());
            }
        }
    }
    Err(Error::new(format!(
        "cannot verify that this session's SSH process owns listener {address}; check 'ford logs' or try again"
    )))
}

fn http_check(port: u16, path: &str, expected: Option<u16>) -> Result<Outcome> {
    let output = run_tool(
        "curl",
        &[
            "--disable",
            "--silent",
            "--globoff",
            "--path-as-is",
            "--noproxy",
            "*",
            "--proto",
            "=http",
            "--max-time",
            "3",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            "--url",
            &format!("http://127.0.0.1:{port}{path}"),
        ],
        Duration::from_secs(4),
    )?;
    let Some(output) = output else {
        return Ok(Outcome::Timeout);
    };
    if output.status.code() == Some(28) {
        return Ok(Outcome::Timeout);
    }
    let status = std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|text| text.parse::<u16>().ok())
        .filter(|status| (100..=599).contains(status));
    if let Some(http_status) = status
        && output.status.success()
    {
        return Ok(match expected {
            None => Outcome::Reachable { http_status },
            Some(expected_status) if expected_status == http_status => Outcome::Ok { http_status },
            Some(expected_status) => Outcome::UnexpectedStatus {
                http_status,
                expected_status,
            },
        });
    }
    Ok(Outcome::ConnectionError {
        curl_exit: output.status.code(),
    })
}

fn run_tool(tool: &str, args: &[&str], timeout: Duration) -> Result<Option<Output>> {
    let mut child = Command::new(tool)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| Error::new(format!("ford check requires {tool} on PATH: {error}")))?;
    // curl emits only the status code and lsof is restricted to one PID/listener with no fork.
    let end = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(Some(child.wait_with_output()?)),
            Ok(None) if Instant::now() < end => thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                child.wait()?;
                result?;
                return Ok(None);
            }
        }
    }
}

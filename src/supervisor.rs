use crate::{
    control::{self, Operation, Request, VERSION},
    error::{Error, Result},
    ssh::OwnedSsh,
    state::{self, Paths, Session, Status},
};
use nix::{
    poll::{PollFd, PollFlags, poll},
    sys::signal::Signal,
    unistd::setsid,
};
use serde::{Deserialize, Serialize};
use signal_hook::consts::{SIGINT, SIGTERM};
use std::{
    fs, io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{
            net::{UnixListener, UnixStream},
            process::ExitStatusExt,
        },
    },
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::Receiver,
    },
    time::{Duration, Instant},
};

const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize, Serialize)]
struct Handshake {
    version: u32,
    session_id: String,
    stage: String,
}

#[derive(Deserialize, Serialize)]
pub struct Startup {
    version: u32,
    session_id: String,
    pub session: Option<Session>,
    pub error: Option<String>,
    pub code: i32,
}

fn startup(session: &Session, error: Option<&Error>) -> Startup {
    Startup {
        version: VERSION,
        session_id: session.id.clone(),
        session: Some(session.clone()),
        error: error.map(|e| e.to_string()),
        code: error.map_or(0, |e| e.code),
    }
}

pub fn launch(paths: &Paths, session: &mut Session, foreground: bool) -> Result<i32> {
    if foreground {
        return supervise(paths, session, true, None);
    }
    let (mut stream, child_stream) = UnixStream::pair()?;
    let input: OwnedFd = child_stream.try_clone()?.into();
    let output: OwnedFd = child_stream.into();
    let mut child = match Command::new(std::env::current_exe()?)
        .args(["--internal-supervise", &session.id])
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            session.state = Status::Failed;
            session.error = Some(format!("cannot start supervisor: {e}"));
            session.cleanup_complete = true;
            paths.save(session)?;
            return Err(e.into());
        }
    };
    let result = (|| -> Result<Startup> {
        control::send(
            &mut stream,
            &Handshake {
                version: VERSION,
                session_id: session.id.clone(),
                stage: "launch".into(),
            },
            START_TIMEOUT,
        )?;
        let ready: Startup = control::receive(&mut stream, START_TIMEOUT)?;
        check_startup(&ready, &session.id)?;
        control::send(
            &mut stream,
            &Handshake {
                version: VERSION,
                session_id: session.id.clone(),
                stage: "commit".into(),
            },
            START_TIMEOUT,
        )?;
        let committed: Startup = control::receive(&mut stream, START_TIMEOUT)?;
        check_startup(&committed, &session.id)?;
        Ok(committed)
    })();
    drop(stream);
    match result {
        Ok(response) => {
            *session = response
                .session
                .ok_or_else(|| Error::new("missing startup state"))?;
            print_session(session);
            // The acknowledged supervisor now owns the session independently of this CLI.
            Ok(0)
        }
        Err(error) => {
            // Closing the startup channel cancels an uncommitted launch. Never signal a PID from disk.
            let _ = child.try_wait();
            Err(Error::with_code(
                error.code,
                format!(
                    "{error}; session {}; use 'ford list --all' to confirm the outcome; log: {}",
                    session.id,
                    session.log_path.display()
                ),
            ))
        }
    }
}

fn check_startup(response: &Startup, id: &str) -> Result<()> {
    if response.version != VERSION
        || response.session_id != id
        || response.session.as_ref().is_none_or(|s| s.id != id)
    {
        return Err(Error::new("startup identity or protocol mismatch"));
    }
    if let Some(error) = &response.error {
        return Err(Error::with_code(response.code, error.clone()));
    }
    Ok(())
}

pub fn internal(id: &str) -> Result<i32> {
    if !state::valid_id(id) {
        return Err(Error::new("invalid internal session ID"));
    }
    setsid()?;
    let mut stream = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
    let paths = Paths::discover()?;
    let mut session = paths.read(id)?;
    let hello: Handshake = control::receive(&mut stream, START_TIMEOUT)?;
    if hello.version != VERSION || hello.session_id != id || hello.stage != "launch" {
        return Err(Error::new("invalid startup request"));
    }
    supervise(&paths, &mut session, false, Some(stream))
}

pub fn print_session(session: &Session) {
    println!(
        "Session: {}{}",
        session.id,
        session
            .name
            .as_ref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default()
    );
    for mapping in &session.mappings {
        println!(
            "Requested: 127.0.0.1:{} -> {}:127.0.0.1:{}",
            mapping.local, session.host, mapping.remote
        );
    }
    println!(
        "State: {} (SSH process alive; forwarding unverified)",
        session.state.label()
    );
}

fn supervise(
    paths: &Paths,
    session: &mut Session,
    foreground: bool,
    mut startup_stream: Option<UnixStream>,
) -> Result<i32> {
    let _session_lock = state::lock(&paths.session_lock(&session.id), Duration::from_secs(2))?;
    let stored = paths.read(&session.id)?;
    if stored.state != Status::Starting || stored.supervisor_pid.is_some() {
        return Err(Error::new("session has already been supervised"));
    }
    let signal = Arc::new(AtomicUsize::new(0));
    signal_hook::flag::register_usize(SIGINT, Arc::clone(&signal), SIGINT as usize)?;
    signal_hook::flag::register_usize(SIGTERM, Arc::clone(&signal), SIGTERM as usize)?;
    session.supervisor_pid = Some(std::process::id());
    session.updated_at = state::now();
    paths.save(session)?;
    let socket_path = paths.socket(&session.id);
    let result = (|| -> Result<i32> {
        let listener = UnixListener::bind(&socket_path)?;
        state::socket_permissions(&socket_path)?;
        listener.set_nonblocking(true)?;
        let mut ssh = OwnedSsh::spawn(session)?;
        session.ssh_pid = Some(ssh.pid());
        let logs = ssh.logs(paths, session, foreground)?;
        let result = run(
            paths,
            session,
            &listener,
            &mut ssh,
            &logs,
            &signal,
            foreground,
            &mut startup_stream,
        );
        if result.is_err() {
            let status = ssh.reap()?;
            session.exit_code = status.code();
            session.exit_signal = status.signal();
            // Finish draining diagnostics before the supervisor process exits on an error path.
            let _ = logs.recv_timeout(Duration::from_secs(2));
        }
        result
    })();
    if let Err(error) = &result {
        session.state = Status::Failed;
        session.error = Some(error.to_string());
        session.cleanup_complete = true;
        session.updated_at = state::now();
        let _ = paths.save(session);
        if let Some(mut stream) = startup_stream {
            let _ = control::send(&mut stream, &startup(session, Some(error)), START_TIMEOUT);
        }
    }
    let _ = fs::remove_file(socket_path);
    // Keep the lock file: unlinking a lock inode can let two owners lock different files for the same ID.
    result
}

#[allow(clippy::too_many_arguments)]
fn run(
    paths: &Paths,
    session: &mut Session,
    listener: &UnixListener,
    ssh: &mut OwnedSsh,
    logs: &Receiver<Result<()>>,
    signal: &AtomicUsize,
    foreground: bool,
    startup_stream: &mut Option<UnixStream>,
) -> Result<i32> {
    session.state = Status::Running;
    session.updated_at = state::now();
    paths.save(session)?;
    if ssh.exited()? {
        let status = ssh.reap()?;
        return Err(Error::with_code(
            status.code().filter(|c| *c > 0).unwrap_or(1),
            format!("SSH exited before startup acknowledgment: {status}; inspect session logs"),
        ));
    }
    if let Some(stream) = startup_stream.as_mut() {
        control::send(stream, &startup(session, None), START_TIMEOUT)?;
        let commit: Handshake = control::receive(stream, START_TIMEOUT)?;
        if commit.version != VERSION || commit.session_id != session.id || commit.stage != "commit"
        {
            return Err(Error::new("invalid startup commit"));
        }
        control::send(stream, &startup(session, None), START_TIMEOUT)?;
        startup_stream.take();
    } else if foreground {
        print_session(session);
    }

    let mut waiting: Vec<(UnixStream, Request)> = Vec::new();
    let mut deadline = None;
    let mut killed = false;
    let mut failure = None;
    let mut log_result = None;
    loop {
        let received_signal = signal.load(Ordering::Relaxed);
        if received_signal != 0 && session.state != Status::Closing {
            begin_close(paths, session, ssh, &mut deadline)?;
        }
        if log_result.is_none()
            && let Ok(result) = logs.try_recv()
        {
            if let Err(error) = &result {
                failure = Some(format!("cannot write SSH logs: {error}"));
                if session.state != Status::Closing {
                    begin_close(paths, session, ssh, &mut deadline)?;
                }
            }
            log_result = Some(result);
        }
        if ssh.exited()? {
            let expected = session.state == Status::Closing;
            let status = ssh.reap()?;
            session.exit_code = status.code();
            session.exit_signal = status.signal();
            if log_result.is_none() {
                log_result = Some(
                    logs.recv_timeout(Duration::from_secs(2))
                        .unwrap_or_else(|_| Err(Error::new("SSH log drain did not finish"))),
                );
            }
            if let Some(Err(error)) = log_result {
                failure = Some(error.to_string());
            }
            session.cleanup_complete = true;
            session.state = if expected && failure.is_none() {
                Status::Closed
            } else {
                Status::Failed
            };
            session.error = failure
                .or_else(|| (!expected).then(|| format!("SSH exited unexpectedly: {status}")));
            session.updated_at = state::now();
            paths.save(session)?;
            for (mut stream, request) in waiting.drain(..) {
                let _ = control::respond(&mut stream, &request, session, None);
            }
            let until = Instant::now() + Duration::from_millis(150);
            while Instant::now() < until {
                accept_requests(listener, session, None, &mut waiting)?;
                wait_listener(listener, 25)?;
            }
            return Ok(if received_signal == SIGINT as usize {
                130
            } else if received_signal == SIGTERM as usize {
                143
            } else if session.state == Status::Closed {
                0
            } else {
                status.code().filter(|c| *c > 0).unwrap_or(1)
            });
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            if !killed {
                ssh.signal(Signal::SIGKILL)?;
                killed = true;
                deadline = Some(Instant::now() + Duration::from_secs(2));
            } else {
                session.error =
                    Some("SSH has not exited after SIGKILL; shutdown remains unconfirmed".into());
                paths.save(session)?;
                for (mut stream, request) in waiting.drain(..) {
                    let _ = control::respond(&mut stream, &request, session, session.error.clone());
                }
                deadline = None;
            }
        }
        let mut requested_close = false;
        accept_requests(listener, session, Some(&mut requested_close), &mut waiting)?;
        if requested_close && session.state != Status::Closing {
            begin_close(paths, session, ssh, &mut deadline)?;
        }
        wait_listener(listener, 100)?;
    }
}

fn begin_close(
    paths: &Paths,
    session: &mut Session,
    ssh: &OwnedSsh,
    deadline: &mut Option<Instant>,
) -> Result<()> {
    session.state = Status::Closing;
    session.updated_at = state::now();
    paths.save(session)?;
    ssh.signal(Signal::SIGTERM)?;
    *deadline = Some(Instant::now() + Duration::from_secs(5));
    Ok(())
}

fn accept_requests(
    listener: &UnixListener,
    session: &Session,
    mut requested_close: Option<&mut bool>,
    waiting: &mut Vec<(UnixStream, Request)>,
) -> Result<()> {
    for _ in 0..8 {
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        let request: Request = match control::receive(&mut stream, Duration::from_millis(100)) {
            Ok(request) => request,
            Err(_) => continue,
        };
        if request.version != VERSION || request.session_id != session.id {
            let _ = control::respond(
                &mut stream,
                &request,
                session,
                Some("protocol or identity mismatch".into()),
            );
            continue;
        }
        match request.operation {
            Operation::Close if requested_close.is_some() => {
                if waiting.len() >= 64 {
                    let _ = control::respond(
                        &mut stream,
                        &request,
                        session,
                        Some("too many pending close requests".into()),
                    );
                } else {
                    **requested_close.as_mut().expect("present") = true;
                    waiting.push((stream, request));
                }
            }
            _ => {
                let _ = control::respond(&mut stream, &request, session, None);
            }
        }
    }
    Ok(())
}

fn wait_listener(listener: &UnixListener, milliseconds: u16) -> Result<()> {
    let mut fds = [PollFd::new(listener.as_fd(), PollFlags::POLLIN)];
    match poll(&mut fds, milliseconds) {
        Ok(_) | Err(nix::errno::Errno::EINTR) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

use crate::{
    check::TargetCheck,
    control::{self, Operation, Request, VERSION},
    error::{Error, Result},
    ssh::{self, OwnedSsh},
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
    if session.jump.is_some() {
        println!("SSH route: {}", session.ssh_route());
    }
    for mapping in &session.mappings {
        println!(
            "Requested: 127.0.0.1:{} -> {}:127.0.0.1:{}",
            mapping.local, session.host, mapping.remote
        );
    }
    let detail = match session.state {
        Status::Running => "SSH process alive; forwarding unverified",
        Status::Retrying => "waiting to restart SSH; forwarding unverified",
        _ => "forwarding unverified",
    };
    println!("State: {} ({detail})", session.state.label());
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
    let mut cleanup_complete = true;
    let result = (|| -> Result<i32> {
        let listener = UnixListener::bind(&socket_path)?;
        state::socket_permissions(&socket_path)?;
        listener.set_nonblocking(true)?;
        let mut ssh = Some(OwnedSsh::spawn(session)?);
        session.ssh_pid = ssh.as_ref().map(OwnedSsh::pid);
        let mut logs = None;
        let result = (|| {
            logs = Some(
                ssh.as_mut()
                    .expect("SSH was spawned")
                    .logs(paths, session, foreground)?,
            );
            run(
                paths,
                session,
                &listener,
                &mut ssh,
                &mut logs,
                &signal,
                foreground,
                &mut startup_stream,
            )
        })();
        if result.is_err() {
            if let Some(ssh) = ssh.as_mut() {
                let status = ssh.reap().inspect_err(|_| cleanup_complete = false)?;
                session.exit_code = status.code();
                session.exit_signal = status.signal();
            }
            // Finish draining diagnostics before the supervisor process exits on an error path.
            if let Some(logs) = logs.as_ref() {
                let _ = logs.recv_timeout(Duration::from_secs(2));
            }
        }
        result
    })();
    if let Err(error) = &result {
        session.state = Status::Failed;
        session.error = Some(error.to_string());
        session.cleanup_complete = cleanup_complete;
        session.retry_at = None;
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
    ssh: &mut Option<OwnedSsh>,
    logs: &mut Option<Receiver<Result<()>>>,
    signal: &AtomicUsize,
    foreground: bool,
    startup_stream: &mut Option<UnixStream>,
) -> Result<i32> {
    session.state = Status::Running;
    session.updated_at = state::now();
    paths.save(session)?;
    let mut startup_pending = true;
    let mut waiting = Vec::new();
    let mut shutdown_deadline = None;
    let mut retry_deadline = None;
    let mut killed = false;
    let mut failure = None;
    let mut log_result = None;
    loop {
        if signal.load(Ordering::Relaxed) != 0 && session.state != Status::Closing {
            begin_close(paths, session, ssh.as_ref(), &mut shutdown_deadline)?;
            retry_deadline = None;
        }
        if log_result.is_none()
            && let Some(logs) = logs.as_ref()
            && let Ok(result) = logs.try_recv()
        {
            if let Err(error) = &result {
                failure = Some(format!("cannot write SSH logs: {error}"));
                if session.state != Status::Closing {
                    begin_close(paths, session, ssh.as_ref(), &mut shutdown_deadline)?;
                }
            }
            log_result = Some(result);
        }
        if let Some(child) = ssh.as_mut()
            && child.exited()?
        {
            let expected = session.state == Status::Closing || signal.load(Ordering::Relaxed) != 0;
            let status = child.reap()?;
            session.exit_code = status.code();
            session.exit_signal = status.signal();
            *ssh = None;
            if log_result.is_none() {
                log_result = Some(
                    logs.as_ref()
                        .expect("SSH has a log receiver")
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap_or_else(|_| Err(Error::new("SSH log drain did not finish"))),
                );
            }
            *logs = None;
            if let Some(Err(error)) = log_result.take() {
                failure = Some(error.to_string());
            }
            if !expected
                && failure.is_none()
                && session.retry_count < session.retry.unwrap_or(0)
                && signal.load(Ordering::Relaxed) == 0
            {
                let delay = Duration::from_secs((1u64 << session.retry_count.min(5)).min(30));
                session.state = Status::Retrying;
                session.ssh_pid = None;
                session.retry_at = Some(state::now() + delay.as_secs());
                session.target_checks.clear();
                session.target = "unverified".into();
                session.forwarding = "unverified".into();
                let message = format!(
                    "SSH exited: {status}; retry {}/{} in {}s",
                    session.retry_count + 1,
                    session.retry.unwrap_or(0),
                    delay.as_secs()
                );
                session.error = Some(message.clone());
                session.updated_at = state::now();
                ssh::note(paths, session, &message, foreground)?;
                paths.save(session)?;
                retry_deadline = Some(Instant::now() + delay);
            } else {
                session.state = if expected && failure.is_none() {
                    Status::Closed
                } else {
                    Status::Failed
                };
                session.error = failure.take().or_else(|| {
                    (!expected).then(|| {
                        let limit = if session.retry.is_some() {
                            "; retry limit reached"
                        } else {
                            ""
                        };
                        format!("SSH exited unexpectedly: {status}{limit}")
                    })
                });
                if startup_pending && session.state == Status::Failed {
                    return Err(Error::with_code(
                        status.code().filter(|code| *code > 0).unwrap_or(1),
                        session.error.clone().expect("failed SSH has an error"),
                    ));
                }
                break;
            }
        }
        if startup_pending {
            acknowledge(session, foreground, startup_stream)?;
            startup_pending = false;
        }
        let mut requested_close = false;
        accept_requests(
            paths,
            listener,
            session,
            ssh.as_ref(),
            Some(&mut requested_close),
            &mut waiting,
        )?;
        if requested_close && session.state != Status::Closing {
            begin_close(paths, session, ssh.as_ref(), &mut shutdown_deadline)?;
            retry_deadline = None;
        }
        if session.state == Status::Closing && ssh.is_none() {
            session.state = if failure.is_none() {
                Status::Closed
            } else {
                Status::Failed
            };
            session.error = failure.take();
            break;
        }
        if retry_deadline.is_some_and(|end| Instant::now() >= end)
            && session.state == Status::Retrying
        {
            // Process cancellation before replacing the child, including signals received during IPC.
            if signal.load(Ordering::Relaxed) != 0 {
                continue;
            }
            session.retry_count += 1;
            ssh::note(
                paths,
                session,
                &format!(
                    "starting SSH retry {}/{}",
                    session.retry_count,
                    session.retry.unwrap_or(0)
                ),
                foreground,
            )?;
            let mut child = OwnedSsh::spawn(session)?;
            session.ssh_pid = Some(child.pid());
            // Retain ownership before opening logs so error cleanup still reaps this child.
            let child_logs = child.logs(paths, session, foreground);
            *ssh = Some(child);
            *logs = Some(child_logs?);
            session.state = Status::Running;
            session.retry_at = None;
            session.error = None;
            session.exit_code = None;
            session.exit_signal = None;
            session.updated_at = state::now();
            paths.save(session)?;
            retry_deadline = None;
            log_result = None;
        }
        if shutdown_deadline.is_some_and(|end| Instant::now() >= end) {
            if !killed {
                if let Some(child) = ssh.as_ref() {
                    child.signal(Signal::SIGKILL)?;
                }
                killed = true;
                shutdown_deadline = Some(Instant::now() + Duration::from_secs(2));
            } else {
                session.error =
                    Some("SSH has not exited after SIGKILL; shutdown remains unconfirmed".into());
                paths.save(session)?;
                for (mut stream, request) in waiting.drain(..) {
                    let _ = control::respond(&mut stream, &request, session, session.error.clone());
                }
                shutdown_deadline = None;
            }
        }
        wait_listener(listener, 100)?;
    }
    session.cleanup_complete = true;
    session.retry_at = None;
    session.updated_at = state::now();
    paths.save(session)?;
    for (mut stream, request) in waiting.drain(..) {
        let _ = control::respond(&mut stream, &request, session, None);
    }
    let until = Instant::now() + Duration::from_millis(150);
    while Instant::now() < until {
        accept_requests(paths, listener, session, None, None, &mut waiting)?;
        wait_listener(listener, 25)?;
    }
    Ok(match signal.load(Ordering::Relaxed) as i32 {
        SIGINT => 130,
        SIGTERM => 143,
        _ if session.state == Status::Closed => 0,
        _ => session.exit_code.filter(|code| *code > 0).unwrap_or(1),
    })
}

fn acknowledge(session: &Session, foreground: bool, stream: &mut Option<UnixStream>) -> Result<()> {
    if let Some(stream) = stream.as_mut() {
        control::send(stream, &startup(session, None), START_TIMEOUT)?;
        let commit: Handshake = control::receive(stream, START_TIMEOUT)?;
        if commit.version != VERSION || commit.session_id != session.id || commit.stage != "commit"
        {
            return Err(Error::new("invalid startup commit"));
        }
        control::send(stream, &startup(session, None), START_TIMEOUT)?;
    } else if foreground {
        print_session(session);
    }
    stream.take();
    Ok(())
}

fn begin_close(
    paths: &Paths,
    session: &mut Session,
    ssh: Option<&OwnedSsh>,
    deadline: &mut Option<Instant>,
) -> Result<()> {
    session.state = Status::Closing;
    session.retry_at = None;
    session.updated_at = state::now();
    paths.save(session)?;
    if let Some(ssh) = ssh {
        ssh.signal(Signal::SIGTERM)?;
        *deadline = Some(Instant::now() + Duration::from_secs(5));
    }
    Ok(())
}

fn accept_requests(
    paths: &Paths,
    listener: &UnixListener,
    session: &mut Session,
    ssh: Option<&OwnedSsh>,
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
            Operation::RecordCheck {
                port,
                ssh_pid,
                retry_count,
                ref check,
            } => {
                let result = if let Some(ssh) = ssh
                    && session.state == Status::Running
                    && ssh.pid() == ssh_pid
                    && session.retry_count == retry_count
                    && !requested_close.as_deref().copied().unwrap_or(false)
                    && !ssh.exited()?
                {
                    record_check(paths, session, port, check)
                } else {
                    Err(Error::new(
                        "SSH attempt changed or session stopped before the target check could be recorded",
                    ))
                };
                let _ = control::respond(
                    &mut stream,
                    &request,
                    session,
                    result.err().map(|error| error.to_string()),
                );
            }
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

fn record_check(
    paths: &Paths,
    session: &mut Session,
    port: u16,
    check: &TargetCheck,
) -> Result<()> {
    if !session.mappings.iter().any(|mapping| mapping.local == port) {
        return Err(Error::new("target check does not match a session mapping"));
    }
    if session
        .target_checks
        .get(&port)
        .is_some_and(|previous| previous.checked_at > check.checked_at)
    {
        return Ok(());
    }
    let mut updated = session.clone();
    updated.target_checks.insert(port, check.clone());
    updated.target = if updated.target_checks.len() == updated.mappings.len()
        && updated
            .target_checks
            .values()
            .all(|value| value.result() == check.result())
    {
        check.result().into()
    } else {
        "mixed".into()
    };
    updated.updated_at = state::now();
    // Leave room for the control response envelope as well as the persisted record.
    if serde_json::to_vec(&updated)?.len() > control::LIMIT - 256 {
        return Err(Error::new(
            "session record is too large to store this target check",
        ));
    }
    paths.save(&updated)?;
    *session = updated;
    Ok(())
}

fn wait_listener(listener: &UnixListener, milliseconds: u16) -> Result<()> {
    let mut fds = [PollFd::new(listener.as_fd(), PollFlags::POLLIN)];
    match poll(&mut fds, milliseconds) {
        Ok(_) | Err(nix::errno::Errno::EINTR) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

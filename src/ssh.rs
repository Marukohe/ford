use crate::{
    error::{Error, Result},
    state::{Paths, Session, open_private},
};
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{Receiver, channel},
    thread,
    time::{Duration, Instant},
};

pub fn arguments(session: &Session, supports_fork: bool) -> Vec<String> {
    let mut args = vec!["-N".into(), "-T".into()];
    for option in [
        "BatchMode=yes",
        "ExitOnForwardFailure=yes",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
        "ConnectTimeout=10",
        "Compression=no",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "ClearAllForwardings=no",
    ] {
        args.extend(["-o".into(), option.into()]);
    }
    if supports_fork {
        args.extend(["-o".into(), "ForkAfterAuthentication=no".into()]);
    }
    for mapping in &session.mappings {
        args.extend([
            "-L".into(),
            format!("127.0.0.1:{}:127.0.0.1:{}", mapping.local, mapping.remote),
        ]);
    }
    for port in &session.socks {
        args.extend(["-D".into(), format!("127.0.0.1:{port}")]);
    }
    for port in &session.reverse_socks {
        args.extend(["-R".into(), format!("127.0.0.1:{port}")]);
    }
    if let Some(jump) = &session.jump {
        args.extend(["-J".into(), jump.clone()]);
    }
    args.extend(["--".into(), session.host.clone()]);
    args
}

pub struct OwnedSsh {
    child: Child,
    reaped: bool,
}

pub fn executable() -> Result<PathBuf> {
    let search = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let mut denied = false;
    for directory in std::env::split_paths(&search) {
        let candidate = directory.join("ssh");
        if let Ok(metadata) = candidate.metadata() {
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return Ok(fs::canonicalize(candidate)?);
            }
            denied = true;
        }
    }
    Err(Error::with_code(
        if denied { 126 } else { 127 },
        "cannot execute ssh; install OpenSSH and check PATH and executable permissions",
    ))
}

impl OwnedSsh {
    pub fn spawn(paths: &Paths, session: &Session, program: &Path) -> Result<Self> {
        note(
            paths,
            session,
            &format!(
                "ford {} supervisor {}: SSH attempt {} using {}",
                env!("CARGO_PKG_VERSION"),
                std::process::id(),
                session.retry_count + 1,
                program.display()
            ),
            false,
        )?;
        let supports_fork = Self::supports_fork(paths, session, program)?;
        let args = arguments(session, supports_fork);
        note(
            paths,
            session,
            &format!("SSH arguments: {}", serde_json::to_string(&args)?),
            false,
        )?;
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| {
                Error::with_code(
                    if e.kind() == std::io::ErrorKind::NotFound {
                        127
                    } else {
                        126
                    },
                    format!("cannot execute ssh: {e}; install OpenSSH and check PATH"),
                )
            })?;
        Ok(Self {
            child,
            reaped: false,
        })
    }

    fn supports_fork(paths: &Paths, session: &Session, program: &Path) -> Result<bool> {
        // -G only prints configuration. -F /dev/null excludes user/system configuration and commands.
        // Inspect a known key instead of passing a possibly unsupported option or parsing a version string.
        let child = Command::new(program)
            .args(["-G", "-T", "-F", "/dev/null", "ford-config-probe.invalid"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| {
                Error::with_code(
                    if error.kind() == std::io::ErrorKind::NotFound {
                        127
                    } else {
                        126
                    },
                    format!(
                        "cannot inspect SSH capabilities at {}: {error}",
                        program.display()
                    ),
                )
            })?;
        let mut probe = Self {
            child,
            reaped: false,
        };
        let diagnostics = probe.logs(paths, session, false)?;
        let stdout = probe
            .child
            .stdout
            .take()
            .ok_or_else(|| Error::new("SSH configuration output unavailable"))?;
        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes);
            let _ = tx.send(result);
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if probe.exited()? {
                let status = probe.reap()?;
                diagnostics
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| Error::new("SSH configuration diagnostics did not finish"))??;
                if !status.success() {
                    return Err(Error::with_code(
                        status.code().filter(|code| *code > 0).unwrap_or(1),
                        format!(
                            "SSH configuration inspection failed: {status}; OpenSSH 7.6 or newer is required"
                        ),
                    ));
                }
                let bytes = rx
                    .recv_timeout(Duration::from_secs(1))
                    .map_err(|_| Error::new("SSH configuration output did not finish"))??;
                if bytes.len() > 65536 {
                    return Err(Error::new("SSH configuration output exceeds 64 KiB"));
                }
                let config = String::from_utf8_lossy(&bytes);
                if !config.lines().any(|line| line.starts_with("hostname ")) {
                    return Err(Error::new("SSH client did not return a configuration dump"));
                }
                return Ok(config
                    .lines()
                    .any(|line| line.starts_with("forkafterauthentication ")));
            }
            if Instant::now() >= deadline {
                probe.reap()?;
                diagnostics
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| Error::new("SSH configuration diagnostics did not finish"))??;
                return Err(Error::new("SSH configuration inspection timed out"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn signal(&self, signal: Signal) -> Result<()> {
        if self.reaped {
            return Err(Error::new("refusing to signal a reaped SSH process group"));
        }
        match killpg(Pid::from_raw(self.pid() as i32), signal) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            // Darwin excludes zombies from group signalling and returns EPERM when none remain.
            // Keep the unreaped leader as the PGID guard, and require confirmed exit first.
            #[cfg(target_os = "macos")]
            Err(Errno::EPERM) if self.exited()? => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn exited(&self) -> Result<bool> {
        // WNOWAIT retains the leader's PID until group cleanup, preventing PGID reuse races.
        use rustix::process::{Pid as WaitPid, WaitId, WaitIdOptions, waitid};
        let pid = WaitPid::from_raw(self.pid() as i32).expect("SSH has a positive PID");
        waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map(|status| status.is_some())
        .map_err(|e| Error::new(format!("cannot observe SSH exit: {e}")))
    }

    pub fn reap(&mut self) -> Result<ExitStatus> {
        if !self.reaped {
            self.signal(Signal::SIGKILL)?;
        }
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    pub fn logs(
        &mut self,
        paths: &Paths,
        session: &Session,
        foreground: bool,
    ) -> Result<Receiver<Result<()>>> {
        let mut stderr = self
            .child
            .stderr
            .take()
            .ok_or_else(|| Error::new("SSH stderr unavailable"))?;
        let mut log = SshLog::open(paths.log(&session.id))?;
        let (tx, rx) = channel();
        thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut buffer = [0; 8192];
                loop {
                    let count = stderr.read(&mut buffer)?;
                    if count == 0 {
                        return Ok(());
                    }
                    log.write(&buffer[..count])?;
                    if foreground {
                        let _ = std::io::stderr().write_all(&buffer[..count]);
                    }
                }
            })();
            let _ = tx.send(result);
        });
        Ok(rx)
    }
}

pub fn note(paths: &Paths, session: &Session, message: &str, foreground: bool) -> Result<()> {
    let bytes = format!("ford: {message}\n");
    SshLog::open(paths.log(&session.id))?.write(bytes.as_bytes())?;
    if foreground {
        let _ = std::io::stderr().write_all(bytes.as_bytes());
    }
    Ok(())
}

struct SshLog {
    path: PathBuf,
    file: File,
    size: u64,
}

impl SshLog {
    fn open(path: PathBuf) -> Result<Self> {
        let (file, size) = Self::append(&path)?;
        Ok(Self { path, file, size })
    }

    fn append(path: &Path) -> Result<(File, u64)> {
        let mut file = open_private(path, true, true)?;
        let size = file.seek(SeekFrom::End(0))?;
        Ok((file, size))
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.size + bytes.len() as u64 > 1024 * 1024 {
            let backup = self.path.with_extension("log.1");
            if backup.try_exists()? {
                drop(open_private(&backup, false, false)?);
                fs::remove_file(&backup)?;
            }
            fs::rename(&self.path, &backup)?;
            (self.file, self.size) = Self::append(&self.path)?;
        }
        self.file.write_all(bytes)?;
        self.size += bytes.len() as u64;
        Ok(())
    }
}

impl Drop for OwnedSsh {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.signal(Signal::SIGKILL);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

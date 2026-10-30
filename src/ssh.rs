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
    fs,
    io::{Read, Write},
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{Receiver, channel},
    thread,
};

pub fn arguments(session: &Session) -> Vec<String> {
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
        "ForkAfterAuthentication=no",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "ClearAllForwardings=no",
    ] {
        args.extend(["-o".into(), option.into()]);
    }
    for mapping in &session.mappings {
        args.extend([
            "-L".into(),
            format!("127.0.0.1:{}:127.0.0.1:{}", mapping.local, mapping.remote),
        ]);
    }
    args.extend(["--".into(), session.host.clone()]);
    args
}

pub struct OwnedSsh {
    child: Child,
    reaped: bool,
}

impl OwnedSsh {
    pub fn spawn(session: &Session) -> Result<Self> {
        let child = Command::new("ssh")
            .args(arguments(session))
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

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn signal(&self, signal: Signal) -> Result<()> {
        if self.reaped {
            return Err(Error::new("refusing to signal a reaped SSH process group"));
        }
        match killpg(Pid::from_raw(self.pid() as i32), signal) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
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
        let path = paths.log(&session.id);
        let mut file = open_private(&path, true, true)?;
        let (tx, rx) = channel();
        thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut size = file.metadata()?.len();
                let mut buffer = [0; 8192];
                loop {
                    let count = stderr.read(&mut buffer)?;
                    if count == 0 {
                        return Ok(());
                    }
                    if size + count as u64 > 1024 * 1024 {
                        let backup = path.with_extension("log.1");
                        if backup.try_exists()? {
                            let old = open_private(&backup, false, false)?;
                            drop(old);
                            fs::remove_file(&backup)?;
                        }
                        fs::rename(&path, &backup)?;
                        file = open_private(&path, true, true)?;
                        size = 0;
                    }
                    file.write_all(&buffer[..count])?;
                    size += count as u64;
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

impl Drop for OwnedSsh {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.signal(Signal::SIGKILL);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

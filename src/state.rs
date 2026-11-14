use crate::{
    check::TargetCheck,
    cli::Open,
    error::{Error, Result},
    mapping::Mapping,
};
use nix::unistd::geteuid;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Starting,
    Running,
    Retrying,
    Closing,
    Closed,
    Failed,
    Unreachable,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Retrying => "retrying",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
            Self::Unreachable => "unreachable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub schema: u32,
    pub id: String,
    pub name: Option<String>,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u32>,
    #[serde(default, skip_serializing_if = "zero")]
    pub retry_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<u64>,
    pub mappings: Vec<Mapping>,
    pub state: Status,
    pub created_at: u64,
    pub updated_at: u64,
    pub supervisor_pid: Option<u32>,
    pub ssh_pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub error: Option<String>,
    pub cleanup_complete: bool,
    pub forwarding: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub target_checks: BTreeMap<u16, TargetCheck>,
    pub log_path: PathBuf,
}

impl Session {
    pub fn ssh_route(&self) -> String {
        match &self.jump {
            Some(jump) => format!("{jump} -> {}", self.host),
            None => self.host.clone(),
        }
    }

    pub fn terminal(&self) -> bool {
        matches!(self.state, Status::Closed | Status::Failed) && self.cleanup_complete
    }

    pub fn target_label(&self) -> String {
        if self.target_checks.is_empty() {
            return self.target.clone();
        }
        self.mappings
            .iter()
            .map(|mapping| {
                let label = self
                    .target_checks
                    .get(&mapping.local)
                    .map_or_else(|| "unverified".into(), TargetCheck::label);
                if self.mappings.len() == 1 {
                    label
                } else {
                    format!("{}: {label}", mapping.local)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Clone)]
pub struct Paths {
    pub run: PathBuf,
    pub state: PathBuf,
}

fn zero(value: &u32) -> bool {
    *value == 0
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn random_id() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::new(format!("cannot generate session ID: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn reject_symlinks(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::new(format!(
            "expected an absolute path without '..': {}",
            path.display()
        )));
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::new(format!(
                    "refusing symbolic link: {}",
                    ancestor.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

pub fn private_dir(path: &Path) -> Result<()> {
    reject_symlinks(path)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != geteuid().as_raw() || meta.mode() & 0o077 != 0 {
        return Err(Error::new(format!(
            "directory must be owned by you with mode 0700: {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn check_file(file: &File, path: &Path) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(Error::new(format!(
            "refusing unsafe file: {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn open_private(path: &Path, create: bool, write: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    check_file(&file, path)?;
    Ok(file)
}

pub fn lock(path: &Path, timeout: Duration) -> Result<File> {
    let file = open_private(path, true, true)?;
    let end = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < end => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(TryLockError::WouldBlock) => {
                return Err(Error::new(format!("lock timed out: {}", path.display())));
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| Error::new("HOME is not set"))?;
        let run = if let Some(root) = env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            let root = PathBuf::from(root);
            reject_symlinks(&root)?;
            let meta = fs::metadata(&root)?;
            if !meta.is_dir() || meta.uid() != geteuid().as_raw() || meta.mode() & 0o077 != 0 {
                return Err(Error::new(
                    "XDG_RUNTIME_DIR must be a private directory owned by you",
                ));
            }
            root.join("ford")
        } else {
            home.join(".ford/run")
        };
        let state = env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("ford");
        for dir in [&run, &state, &state.join("sessions"), &state.join("logs")] {
            private_dir(dir)?;
        }
        let paths = Self { run, state };
        // macOS sockaddr_un paths are shorter than Linux paths; use a portable bound.
        if paths.socket(&"0".repeat(32)).as_os_str().as_bytes().len() > 103 {
            return Err(Error::new(
                "runtime directory is too long for Unix sockets; choose a shorter XDG_RUNTIME_DIR",
            ));
        }
        Ok(paths)
    }

    pub fn socket(&self, id: &str) -> PathBuf {
        self.run.join(format!("{id}.sock"))
    }
    pub fn record(&self, id: &str) -> PathBuf {
        self.state.join("sessions").join(format!("{id}.json"))
    }
    pub fn session_lock(&self, id: &str) -> PathBuf {
        self.run.join(format!("{id}.lock"))
    }
    pub fn log(&self, id: &str) -> PathBuf {
        self.state.join("logs").join(format!("{id}.log"))
    }

    pub fn read(&self, id: &str) -> Result<Session> {
        if !valid_id(id) {
            return Err(Error::new("invalid session ID"));
        }
        let file = open_private(&self.record(id), false, false)?;
        if file.metadata()?.len() > 65536 {
            return Err(Error::new("oversized session record"));
        }
        let session: Session = serde_json::from_reader(file)?;
        if session.schema != SCHEMA || session.id != id || session.log_path != self.log(id) {
            return Err(Error::new(format!(
                "incompatible or mismatched session record: {id}"
            )));
        }
        Ok(session)
    }

    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();
        for entry in fs::read_dir(self.state.join("sessions"))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let id = path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .ok_or_else(|| Error::new("invalid record name"))?;
                sessions.push(self.read(id)?);
            }
        }
        sessions.sort_by_key(|s| (s.created_at, s.id.clone()));
        Ok(sessions)
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        let target = self.record(&session.id);
        let temp = target.with_extension(format!("{}.tmp", random_id()?));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            serde_json::to_writer(&mut file, session)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp, target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }

    pub fn reserve(&self, open: Open) -> Result<Session> {
        let _registry = lock(&self.state.join("registry.lock"), Duration::from_secs(2))?;
        if let Some(name) = &open.name
            && self
                .sessions()?
                .iter()
                .any(|s| s.name.as_ref() == Some(name) && !s.terminal())
        {
            return Err(Error::new(format!(
                "session name '{name}' is already reserved"
            )));
        }
        let mut id = random_id()?;
        while self.record(&id).try_exists()? {
            id = random_id()?;
        }
        let session = Session {
            schema: SCHEMA,
            log_path: self.log(&id),
            id,
            name: open.name,
            host: open.host,
            jump: open.jump,
            retry: open.retry.filter(|limit| *limit != 0),
            retry_count: 0,
            retry_at: None,
            mappings: open.ports,
            state: Status::Starting,
            created_at: now(),
            updated_at: now(),
            supervisor_pid: None,
            ssh_pid: None,
            exit_code: None,
            exit_signal: None,
            error: None,
            cleanup_complete: false,
            forwarding: "unverified".into(),
            target: "unverified".into(),
            target_checks: Default::default(),
        };
        self.save(&session)?;
        Ok(session)
    }

    pub fn resolve(&self, target: &str) -> Result<Session> {
        let sessions = self.sessions()?;
        if let Some(session) = sessions.iter().find(|s| s.id == target) {
            return Ok(session.clone());
        }
        let active_names: Vec<_> = sessions
            .iter()
            .filter(|s| s.name.as_deref() == Some(target) && !s.terminal())
            .collect();
        if active_names.len() == 1 {
            return Ok(active_names[0].clone());
        }
        let matches: Vec<_> = sessions
            .into_iter()
            .filter(|s| s.name.as_deref() == Some(target) || s.id.starts_with(target))
            .collect();
        match matches.len() {
            0 => Err(Error::new(format!("session not found: {target}"))),
            1 => Ok(matches.into_iter().next().expect("one match")),
            _ => Err(Error::new(format!(
                "ambiguous session: {target}; use a full ID"
            ))),
        }
    }
}

pub fn read_logs(paths: &Paths, id: &str, count: usize) -> Result<String> {
    let mut bytes = Vec::new();
    for path in [paths.log(id).with_extension("log.1"), paths.log(id)] {
        match open_private(&path, false, false) {
            Ok(file) => {
                file.take(1024 * 1024).read_to_end(&mut bytes)?;
            }
            Err(_) if !path.try_exists()? => {}
            Err(e) => return Err(e),
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<_> = text.lines().collect();
    Ok(lines[lines.len().saturating_sub(count)..].join("\n"))
}

pub fn socket_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

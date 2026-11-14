use crate::{
    cli::{GroupCommand, Open},
    error::{Error, Result},
    mapping::{Mapping, validate_name},
    state::{self, Paths, Session, Status},
    supervisor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const LIMIT: u64 = 65536;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Group {
    schema: u32,
    name: String,
    saved_at: u64,
    sessions: Vec<SavedSession>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SavedSession {
    host: String,
    mappings: Vec<Mapping>,
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jump: Option<String>,
}

impl SavedSession {
    fn from_session(session: Session) -> Self {
        Self {
            host: session.host,
            mappings: session.mappings,
            name: session.name,
            jump: session.jump,
        }
    }

    fn open(&self) -> Open {
        Open {
            host: self.host.clone(),
            ports: self.mappings.clone(),
            name: self.name.clone(),
            jump: self.jump.clone(),
            foreground: false,
        }
    }

    fn matches(&self, session: &Session) -> bool {
        self.host == session.host
            && self.name == session.name
            && self.jump == session.jump
            && self.mappings.len() == session.mappings.len()
            && self
                .mappings
                .iter()
                .all(|mapping| session.mappings.contains(mapping))
    }
}

impl Group {
    fn validate(&self) -> Result<()> {
        validate_name(&self.name).map_err(|error| Error::with_code(2, error))?;
        if self.schema != 1 {
            return Err(Error::new("unsupported saved group format"));
        }
        if self.sessions.is_empty() {
            return Err(Error::new(
                "no sessions to save; select sessions or use --all with running sessions",
            ));
        }
        let mut names = HashSet::new();
        let mut ports = HashSet::new();
        for session in &self.sessions {
            session.open().validate()?;
            // Leave room for lifecycle metadata and the startup protocol envelope when reopening.
            if serde_json::to_vec(session)?.len() as u64 > LIMIT / 2 {
                return Err(Error::new("saved session configuration exceeds 32 KiB"));
            }
            if let Some(name) = &session.name
                && !names.insert(name)
            {
                return Err(Error::new(format!(
                    "duplicate session name in group: {name}"
                )));
            }
            for mapping in &session.mappings {
                if !ports.insert(mapping.local) {
                    return Err(Error::new(format!(
                        "duplicate local port in group: {}",
                        mapping.local
                    )));
                }
            }
        }
        Ok(())
    }
}

pub fn run(command: GroupCommand) -> Result<i32> {
    let name = match &command {
        GroupCommand::Save { name, .. }
        | GroupCommand::Open { name }
        | GroupCommand::Close { name }
        | GroupCommand::Show { name }
        | GroupCommand::Remove { name } => Some(name),
        GroupCommand::List { .. } => None,
    };
    if let Some(name) = name {
        validate_name(name).map_err(|error| Error::with_code(2, error))?;
    }
    let paths = Paths::discover()?;
    let directory = paths.state.join("groups");
    state::private_dir(&directory)?;
    // Retain lock files after removal: unlinking them would permit two owners on different inodes.
    let _lock = name
        .map(|name| {
            state::lock(
                &directory.join(format!("{name}.lock")),
                Duration::from_secs(2),
            )
        })
        .transpose()?;
    match command {
        GroupCommand::Save {
            name,
            sessions,
            all,
            force,
        } => {
            let sessions = if all {
                crate::live_sessions(&paths, paths.sessions()?)
                    .into_iter()
                    .filter(|session| session.state == Status::Running)
                    .collect()
            } else {
                sessions
                    .iter()
                    .map(|target| paths.resolve(target))
                    .collect::<Result<Vec<_>>>()?
            };
            let group = Group {
                schema: 1,
                name,
                saved_at: state::now(),
                sessions: sessions
                    .into_iter()
                    .map(SavedSession::from_session)
                    .collect(),
            };
            group.validate()?;
            save(&directory, &group, force)?;
            println!(
                "Saved group {} ({} sessions)",
                group.name,
                group.sessions.len()
            );
            Ok(0)
        }
        GroupCommand::Open { name } => open(&paths, &read(&directory, &name)?),
        GroupCommand::Close { name } => {
            let group = read(&directory, &name)?;
            let mut closed = 0;
            let mut failures = 0;
            for session in paths
                .sessions()?
                .into_iter()
                .filter(|session| !session.terminal())
            {
                if group.sessions.iter().any(|saved| saved.matches(&session)) {
                    match crate::close(&paths, &session) {
                        Ok(()) => {
                            println!("Closed: {}", session.id);
                            closed += 1;
                        }
                        Err(error) => {
                            eprintln!("ford: {}: {error}", session.id);
                            failures += 1;
                        }
                    }
                }
            }
            println!("Group {name}: closed {closed}, failed {failures}");
            Ok(i32::from(failures != 0))
        }
        GroupCommand::List { json } => {
            let mut groups = Vec::new();
            for entry in fs::read_dir(&directory)? {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    let name = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| Error::new("invalid group filename"))?;
                    match read(&directory, name) {
                        Ok(group) => groups.push(group),
                        Err(_)
                            if fs::symlink_metadata(&path).is_err_and(|error| {
                                error.kind() == std::io::ErrorKind::NotFound
                            }) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            groups.sort_by(|left, right| left.name.cmp(&right.name));
            if json {
                println!("{}", serde_json::to_string_pretty(&groups)?);
            } else {
                println!("{:<20} {:<10} MAPPINGS", "GROUP", "SESSIONS");
                for group in groups {
                    let count: usize = group
                        .sessions
                        .iter()
                        .map(|session| session.mappings.len())
                        .sum();
                    println!("{:<20} {:<10} {count}", group.name, group.sessions.len());
                }
            }
            Ok(0)
        }
        GroupCommand::Show { name } => {
            let group = read(&directory, &name)?;
            println!("Group: {name}");
            for saved in group.sessions {
                let route = match saved.jump {
                    Some(jump) => format!("{jump} -> {}", saved.host),
                    None => saved.host,
                };
                let mappings = saved
                    .mappings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "  {}  {route}  {mappings}",
                    saved.name.as_deref().unwrap_or("(unnamed)")
                );
            }
            Ok(0)
        }
        GroupCommand::Remove { name } => {
            let path = directory.join(format!("{name}.json"));
            drop(state::open_private(&path, false, false)?);
            fs::remove_file(path)?;
            println!("Removed group: {name}");
            Ok(0)
        }
    }
}

fn read(directory: &Path, name: &str) -> Result<Group> {
    validate_name(name).map_err(Error::new)?;
    let path = directory.join(format!("{name}.json"));
    let file = state::open_private(&path, false, false)
        .map_err(|error| Error::new(format!("cannot read group '{name}': {error}")))?;
    if file.metadata()?.len() > LIMIT {
        return Err(Error::new("saved group exceeds 64 KiB"));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::new("saved group exceeds 64 KiB"));
    }
    let group: Group = serde_json::from_slice(&bytes)?;
    if group.name != name {
        return Err(Error::new("saved group name does not match its filename"));
    }
    group.validate()?;
    Ok(group)
}

fn save(directory: &Path, group: &Group, force: bool) -> Result<()> {
    let path = directory.join(format!("{}.json", group.name));
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            drop(state::open_private(&path, false, false)?);
            if !force {
                return Err(Error::new(
                    "group already exists; use --force to replace its saved mappings",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut bytes = serde_json::to_vec_pretty(group)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > LIMIT {
        return Err(Error::new("saved group exceeds 64 KiB"));
    }
    let temporary: PathBuf = path.with_extension(format!("{}.tmp", state::random_id()?));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn open(paths: &Paths, group: &Group) -> Result<i32> {
    let active = crate::live_sessions(paths, paths.sessions()?)
        .into_iter()
        .filter(|session| !session.terminal())
        .collect::<Vec<_>>();
    let mut pending = Vec::new();
    let mut skipped = 0;
    // Reject known conflicts before creating anything. SSH still owns the actual port bind.
    for saved in &group.sessions {
        if let Some(session) = active.iter().find(|session| saved.matches(session)) {
            if session.state != Status::Running {
                return Err(Error::new(format!(
                    "session {} is {}; cannot safely reopen its mappings",
                    session.id,
                    session.state.label()
                )));
            }
            skipped += 1;
        } else if let Some(session) = active.iter().find(|session| {
            (saved.name.is_some() && saved.name == session.name)
                || saved.mappings.iter().any(|mapping| {
                    session
                        .mappings
                        .iter()
                        .any(|existing| existing.local == mapping.local)
                })
        }) {
            return Err(Error::new(format!(
                "group {} conflicts with session {}; close or resolve it before reopening the group",
                group.name, session.id
            )));
        } else {
            pending.push(saved.open());
        }
    }
    let mut opened = 0;
    let mut failures = 0;
    for options in pending {
        let result = paths
            .reserve(options)
            .and_then(|mut session| supervisor::launch(paths, &mut session, false));
        match result {
            Ok(_) => opened += 1,
            Err(error) => {
                eprintln!("ford: group {}: {error}", group.name);
                failures += 1;
            }
        }
    }
    println!(
        "Group {}: opened {opened}, already running {skipped}, failed {failures}",
        group.name
    );
    Ok(i32::from(failures != 0))
}

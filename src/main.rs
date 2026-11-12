#![forbid(unsafe_code)]

#[cfg(not(unix))]
compile_error!("ford currently supports Linux and macOS only");

mod check;
mod cli;
mod control;
mod error;
mod mapping;
mod ssh;
mod state;
mod supervisor;

use cli::Action;
use control::Operation;
use error::{Error, Result};
use state::{Paths, Session, Status};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

fn main() {
    let result = execute();
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ford: {error}");
            error.code
        }
    };
    std::process::exit(code);
}

fn execute() -> Result<i32> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|v| v == "--internal-supervise") {
        if args.len() != 3 {
            return Err(Error::new("invalid internal supervisor invocation"));
        }
        let id = args[2]
            .to_str()
            .ok_or_else(|| Error::with_code(2, "session ID must be UTF-8"))?;
        return supervisor::internal(id);
    }
    let cli = cli::parse();
    match cli.command {
        Action::Open(open) => {
            open.validate()?;
            let paths = Paths::discover()?;
            let foreground = open.foreground;
            let mut session = paths.reserve(open)?;
            supervisor::launch(&paths, &mut session, foreground)
        }
        Action::List { all, json } => {
            let paths = Paths::discover()?;
            let sessions = live_sessions(&paths, paths.sessions()?);
            let visible: Vec<_> = sessions
                .into_iter()
                .filter(|s| all || !s.terminal())
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&visible)?);
            } else {
                println!(
                    "{:<12} {:<16} {:<24} {:<24} {:<12} TARGET",
                    "ID", "NAME", "HOST", "MAPPINGS", "STATE"
                );
                for session in visible {
                    let mappings = session
                        .mappings
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",");
                    println!(
                        "{:<12} {:<16} {:<24} {:<24} {:<12} {}",
                        &session.id[..12],
                        session.name.as_deref().unwrap_or("-"),
                        session.ssh_route(),
                        mappings,
                        session.state.label(),
                        session.target_label()
                    );
                    if let Some(error) = session.error {
                        println!("  {error}");
                    }
                }
            }
            Ok(0)
        }
        Action::Close { target, all } => {
            let paths = Paths::discover()?;
            let sessions = if all {
                paths
                    .sessions()?
                    .into_iter()
                    .filter(|s| !s.terminal())
                    .collect()
            } else {
                vec![paths.resolve(target.as_deref().expect("clap requires target"))?]
            };
            let mut failures = 0;
            for session in sessions {
                match close(&paths, &session) {
                    Ok(()) => println!(
                        "Closed: {}{}",
                        session.id,
                        session
                            .name
                            .as_ref()
                            .map(|n| format!(" ({n})"))
                            .unwrap_or_default()
                    ),
                    Err(error) => {
                        failures += 1;
                        eprintln!("ford: {}: {error}", session.id);
                    }
                }
            }
            Ok(i32::from(failures > 0))
        }
        Action::Check(args) => {
            let paths = Paths::discover()?;
            let session = paths.resolve(&args.target)?;
            check::run(&paths, &session, &args)
        }
        Action::Logs { target, lines } => {
            let paths = Paths::discover()?;
            let session = paths.resolve(&target)?;
            let text = state::read_logs(&paths, &session.id, lines as usize)?;
            if !text.is_empty() {
                println!("{text}");
            }
            Ok(0)
        }
    }
}

fn observe(paths: &Paths, mut session: Session) -> Session {
    if session.terminal() {
        return session;
    }
    match control::query(
        paths,
        &session,
        Operation::Status,
        Duration::from_millis(250),
    ) {
        Ok(current) => current,
        Err(error) => {
            if let Ok(current) = paths.read(&session.id)
                && current.terminal()
            {
                return current;
            }
            session.state = Status::Unreachable;
            session.error = Some(format!(
                "supervisor unreachable: {error}; log: {}",
                session.log_path.display()
            ));
            session
        }
    }
}

fn live_sessions(paths: &Paths, sessions: Vec<Session>) -> Vec<Session> {
    let next = AtomicUsize::new(0);
    let deadline = Instant::now() + Duration::from_secs(2);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..sessions.len().min(8) {
            let tx = tx.clone();
            let sessions = &sessions;
            let next = &next;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(session) = sessions.get(index) else {
                        break;
                    };
                    let mut session = session.clone();
                    if Instant::now() >= deadline && !session.terminal() {
                        session.state = Status::Unreachable;
                        session.error = Some("list query deadline exceeded".into());
                    } else {
                        session = observe(paths, session);
                    }
                    let _ = tx.send((index, session));
                }
            });
        }
    });
    drop(tx);
    let mut results: Vec<_> = rx.into_iter().collect();
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, session)| session).collect()
}

fn close(paths: &Paths, session: &Session) -> Result<()> {
    if session.terminal() {
        return Ok(());
    }
    match control::query(paths, session, Operation::Close, Duration::from_secs(10)) {
        Ok(current) if current.terminal() => Ok(()),
        Ok(_) => Err(Error::new("supervisor has not confirmed shutdown")),
        Err(error) => {
            if let Ok(current) = paths.read(&session.id)
                && current.terminal()
            {
                return Ok(());
            }
            Err(Error::new(format!(
                "cannot confirm shutdown: {error}; last known supervisor PID {:?}, SSH PID {:?}; log: {}",
                session.supervisor_pid,
                session.ssh_pid,
                session.log_path.display()
            )))
        }
    }
}

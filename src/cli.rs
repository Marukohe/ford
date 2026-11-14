use crate::{
    error::{Error, Result},
    mapping::{Mapping, validate_host, validate_jump, validate_mappings, validate_name},
};
use clap::{Args, Parser, Subcommand};
use std::ffi::OsString;

#[derive(Parser)]
#[command(
    version,
    about = "Forward remote ports through supervised SSH sessions"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Action,
}

#[derive(Subcommand)]
pub enum Action {
    /// Open a session (also: ford HOST PORT...)
    Open(Open),
    /// Query live session state
    List {
        /// Include completed sessions
        #[arg(long)]
        all: bool,
        /// Emit structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Close sessions and wait for SSH cleanup
    Close {
        /// Session name, ID, or unique ID prefix
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        target: Option<String>,
        /// Close sessions present when this command starts
        #[arg(long)]
        all: bool,
    },
    /// Check an HTTP endpoint through an existing session
    Check(Check),
    /// Save and reopen named sets of sessions
    Group {
        #[command(subcommand)]
        command: GroupCommand,
    },
    /// Show recent SSH diagnostics
    Logs {
        target: String,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u32).range(1..=10000))]
        lines: u32,
    },
}

#[derive(Subcommand)]
pub enum GroupCommand {
    /// Save session mappings for later use
    Save {
        name: String,
        /// Session names, IDs, or unique ID prefixes (completed sessions are accepted)
        #[arg(required_unless_present = "all", conflicts_with = "all", num_args = 1..)]
        sessions: Vec<String>,
        /// Save all running or reconnecting sessions
        #[arg(long)]
        all: bool,
        /// Replace an existing saved group
        #[arg(long)]
        force: bool,
    },
    /// Reopen saved sessions in the background; skip matching running sessions
    Open { name: String },
    /// Close current sessions that match the saved mappings
    Close { name: String },
    /// List saved groups
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show the sessions saved in a group
    Show { name: String },
    /// Delete saved mappings without closing sessions
    Remove { name: String },
}

#[derive(Args)]
pub struct Check {
    /// Session name, ID, or unique ID prefix
    pub target: String,
    /// Local port (required for sessions with multiple mappings)
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,
    /// HTTP path to request, such as / or /health
    #[arg(long, value_name = "PATH")]
    pub http: String,
    /// Require a specific status; otherwise any HTTP response proves reachability
    #[arg(long, value_parser = clap::value_parser!(u16).range(100..=599))]
    pub expect_status: Option<u16>,
}

#[derive(Args)]
pub struct Open {
    pub host: String,
    #[arg(required = true, num_args = 1..)]
    pub ports: Vec<Mapping>,
    #[arg(long)]
    pub name: Option<String>,
    /// SSH through a jump host: alias or user@host[:port]
    #[arg(short = 'J', long, value_name = "HOST")]
    pub jump: Option<String>,
    /// Retry unexpected SSH exits (default: 5 retries; use --retry=N to choose 0..=100)
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "5",
          value_parser = clap::value_parser!(u32).range(0..=100))]
    pub retry: Option<u32>,
    /// Stream logs and close on Ctrl+C instead of detaching
    #[arg(long)]
    pub foreground: bool,
}

impl Open {
    pub fn validate(&self) -> Result<()> {
        validate_host(&self.host).map_err(|e| Error::with_code(2, e))?;
        validate_mappings(&self.ports).map_err(|e| Error::with_code(2, e))?;
        if self.retry.is_some_and(|limit| limit > 100) {
            return Err(Error::with_code(2, "retry limit must be in 0..=100"));
        }
        if let Some(jump) = &self.jump {
            validate_jump(jump).map_err(|e| Error::with_code(2, e))?;
        }
        if let Some(name) = &self.name {
            validate_name(name).map_err(|e| Error::with_code(2, e))?;
        }
        Ok(())
    }
}

pub fn parse() -> Cli {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if let Some(first) = args.get(1).and_then(|s| s.to_str())
        && !first.starts_with('-')
        && !["open", "list", "close", "check", "group", "logs", "help"].contains(&first)
    {
        args.insert(1, OsString::from("open"));
    }
    Cli::parse_from(args)
}

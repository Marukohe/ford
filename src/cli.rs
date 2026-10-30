use crate::{
    error::{Error, Result},
    mapping::{Mapping, validate_host, validate_mappings, validate_name},
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
    /// Show recent SSH diagnostics
    Logs {
        target: String,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u32).range(1..=10000))]
        lines: u32,
    },
}

#[derive(Args)]
pub struct Open {
    pub host: String,
    #[arg(required = true, num_args = 1..)]
    pub ports: Vec<Mapping>,
    #[arg(long)]
    pub name: Option<String>,
    /// Stream logs and close on Ctrl+C instead of detaching
    #[arg(long)]
    pub foreground: bool,
}

impl Open {
    pub fn validate(&self) -> Result<()> {
        validate_host(&self.host).map_err(|e| Error::with_code(2, e))?;
        validate_mappings(&self.ports).map_err(|e| Error::with_code(2, e))?;
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
        && !["open", "list", "close", "logs", "help"].contains(&first)
    {
        args.insert(1, OsString::from("open"));
    }
    Cli::parse_from(args)
}

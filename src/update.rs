use crate::{
    error::{Error, Result},
    state,
};
use std::{
    fs::{self, DirBuilder},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

const INSTALLER: &str = include_str!("../scripts/install.sh");

pub fn run(check: bool) -> Result<i32> {
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let directory = executable
        .parent()
        .ok_or_else(|| Error::new("cannot locate installation directory"))?;
    if !check && executable.file_name().is_none_or(|name| name != "ford") {
        return Err(Error::new(
            "in-place updates require an executable named ford; reinstall with scripts/install.sh",
        ));
    }
    // Serialize replacements in this installation without touching session state or PATH configuration.
    let _lock = if check {
        None
    } else {
        Some(
            state::lock(&directory.join(".ford-update.lock"), Duration::from_secs(2)).map_err(
                |error| {
                    Error::new(format!(
                        "cannot lock installation at {}: {error}; check directory permissions",
                        directory.display()
                    ))
                },
            )?,
        )
    };
    let work = WorkDir::new(if check {
        std::env::temp_dir()
    } else {
        directory.to_path_buf()
    })?;
    let script = work.0.join("install.sh");
    fs::write(&script, INSTALLER)?;
    let output = Command::new("sh")
        .arg(&script)
        .arg("--resolve-version")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| Error::new(format!("cannot run bundled installer with sh: {error}")))?;
    if !output.status.success() {
        return Err(Error::new(
            "cannot check the latest release; existing installation was not changed",
        ));
    }
    let tag = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::new("release tag is not valid UTF-8"))?
        .trim();
    let latest = tag
        .strip_prefix('v')
        .ok_or_else(|| Error::new("invalid release tag"))?;
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(latest, current)? {
        println!("ford {current} is up to date (latest stable: {latest}).");
        return Ok(0);
    }
    println!("Update available: ford {current} -> {latest}");
    if check {
        return Ok(0);
    }
    let status = Command::new("sh")
        .arg(&script)
        .args(["--version", tag, "--update", "--bin-dir"])
        .arg(directory)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(Error::new(
            "update failed; inspect the diagnostics and run ford --version to confirm the installed version",
        ));
    }
    Ok(0)
}

fn is_newer(latest: &str, current: &str) -> Result<bool> {
    fn core(version: &str) -> Result<([u64; 3], bool)> {
        let (version, build) = version
            .split_once('+')
            .map_or((version, None), |(v, b)| (v, Some(b)));
        let (version, prerelease) = version
            .split_once('-')
            .map_or((version, None), |(v, p)| (v, Some(p)));
        for suffix in [build, prerelease].into_iter().flatten() {
            if suffix.split('.').any(|part| {
                part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }) {
                return Err(Error::new("invalid release version"));
            }
        }
        let mut parts = version.split('.');
        let mut values = [0; 3];
        for value in &mut values {
            let part = parts
                .next()
                .ok_or_else(|| Error::new("invalid release version"))?;
            if part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(Error::new("invalid release version"));
            }
            *value = part
                .parse()
                .map_err(|_| Error::new("invalid release version"))?;
        }
        if parts.next().is_some() {
            return Err(Error::new("invalid release version"));
        }
        Ok((values, prerelease.is_some()))
    }
    let (latest, latest_prerelease) = core(latest)?;
    let (current, current_prerelease) = core(current)?;
    if latest_prerelease {
        return Err(Error::new("latest release must be stable"));
    }
    Ok(latest > current || (latest == current && current_prerelease))
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(parent: PathBuf) -> Result<Self> {
        let path = parent.join(format!(".ford-update-{}", state::random_id()?));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|error| {
                Error::new(format!(
                    "cannot stage update in {}: {error}; check directory permissions",
                    parent.display()
                ))
            })?;
        Ok(Self(path))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_versions_never_downgrade_or_reinstall_build_metadata() {
        for (latest, current, expected) in [
            ("0.10.0", "0.9.9", true),
            ("1.0.1", "1.0.0", true),
            ("1.0.0", "1.0.0-rc.1", true),
            ("1.0.0", "1.1.0-rc.1", false),
            ("1.0.0", "1.0.0+local", false),
            ("1.0.0+build", "1.0.0", false),
            ("0.2.0", "0.3.0", false),
        ] {
            assert_eq!(is_newer(latest, current).unwrap(), expected);
        }
        for invalid in [
            "",
            "1.0",
            "01.0.0",
            "1.0.0.0",
            "1.0.0-rc.1",
            "1.0.0+",
            "1.0.0/evil",
        ] {
            assert!(is_newer(invalid, "1.0.0").is_err(), "{invalid}");
        }
    }
}

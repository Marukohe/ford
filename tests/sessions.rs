use nix::{
    sys::signal::{Signal, kill, killpg},
    unistd::Pid,
};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::OwnedFd,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ford-")
            .tempdir_in("/tmp")
            .unwrap();
        for name in ["home", "run", "state", "bin"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.path().join(name))
                .unwrap();
        }
        let ssh = root.path().join("bin/ssh");
        fs::write(
            &ssh,
            r#"#!/bin/sh
# Publish fixture files only after their contents are complete.
publish() {
  fixture_file=$1
  fixture_temp=$fixture_file.$$.tmp
  shift
  printf '%s\n' "$@" > "$fixture_temp" && mv "$fixture_temp" "$fixture_file"
}
printf 'SSH fixture diagnostic\n' >&2
publish "$FORD_TEST_ARGS" "$@"
case "$FORD_TEST_MODE" in
  fail) exit 255 ;;
  orphan)
    sleep 300 &
    publish "$FORD_TEST_HELPER" "$!"
    exit 255
    ;;
  stubborn)
    trap '' TERM
    sleep 300 &
    publish "$FORD_TEST_HELPER" "$!"
    wait
    ;;
  logs)
    head -c 2200000 /dev/zero | tr '\000' x >&2
    printf '\nlog-tail-marker\n' >&2
    exec sleep 300
    ;;
  *) exec sleep 300 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let root = self.root.path().canonicalize().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ford"));
        command
            .env("HOME", root.join("home"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("FORD_TEST_ARGS", root.join("args"))
            .env("FORD_TEST_HELPER", root.join("helper"));
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn sessions(&self) -> Vec<Value> {
        serde_json::from_slice(&self.ok(&["list", "--all", "--json"]).stdout).unwrap()
    }

    fn session(&self, name: &str) -> Value {
        self.sessions()
            .into_iter()
            .find(|s| s["name"] == name)
            .unwrap()
    }

    fn wait_state(&self, name: &str, state: &str) -> Value {
        let mut found = None;
        wait_for(|| {
            let current = self.session(name);
            if current["state"] == state {
                found = Some(current);
                true
            } else {
                false
            }
        });
        found.unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.output(&["close", "--all"]);
    }
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "condition did not become true before deadline"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn background_list_arguments_close_and_name_reuse() {
    let f = Fixture::new();
    f.ok(&["devbox", "8080:300", "5173", "--name", "web"]);
    let session = f.wait_state("web", "running");
    assert_eq!(session["target"], "unverified");
    assert_eq!(session["forwarding"], "unverified");
    wait_for(|| f.root.path().join("args").exists());
    let args = fs::read_to_string(f.root.path().join("args")).unwrap();
    assert!(args.contains("-L\n127.0.0.1:8080:127.0.0.1:300\n-L\n127.0.0.1:5173:127.0.0.1:5173\n"));
    assert!(args.ends_with("--\ndevbox\n"));
    for option in [
        "BatchMode=yes",
        "ControlPath=none",
        "ExitOnForwardFailure=yes",
        "ForkAfterAuthentication=no",
    ] {
        assert!(args.contains(option));
    }
    assert!(
        !f.output(&["devbox", "9090", "--name", "web"])
            .status
            .success()
    );
    f.ok(&["close", "web"]);
    let closed = f.wait_state("web", "closed");
    assert_eq!(closed["cleanup_complete"], true);
    f.ok(&["close", session["id"].as_str().unwrap()]);
    assert!(
        serde_json::from_slice::<Vec<Value>>(&f.ok(&["list", "--json"]).stdout)
            .unwrap()
            .is_empty()
    );
    wait_for(|| {
        String::from_utf8_lossy(&f.ok(&["logs", "web"]).stdout).contains("SSH fixture diagnostic")
    });
    f.ok(&["devbox", "9090", "--name", "web"]);
    f.ok(&["close", "web"]);
    assert_eq!(f.sessions().len(), 2);
}

#[test]
fn invalid_input_has_no_side_effects() {
    let f = Fixture::new();
    for args in [
        vec!["devbox", "0"],
        vec!["devbox", "65536"],
        vec!["devbox", "80", "80:90"],
        vec!["open", "bad;command", "3000"],
        vec!["devbox", "3000", "--name", "../bad"],
        vec!["devbox", "3000", "--jump=-oProxyCommand=bad"],
        vec!["devbox", "3000", "--jump", "bob@-bad"],
        vec!["devbox", "3000", "--jump", "jump;bad"],
        vec!["devbox", "3000", "--jump", "one,two"],
        vec!["devbox", "3000", "--jump", "bastion:0"],
        vec!["devbox", "3000", "--jump", "bastion:65536"],
        vec!["devbox", "3000", "--jump", "bastion:"],
        vec!["devbox", "3000", "--jump", "::1"],
    ] {
        assert_eq!(f.output(&args).status.code(), Some(2), "{args:?}");
    }
    assert!(!f.root.path().join("args").exists());
    use std::os::unix::ffi::OsStringExt;
    let invalid_utf8 = std::ffi::OsString::from_vec(vec![0xff]);
    assert_eq!(
        f.command()
            .arg(invalid_utf8)
            .arg("3000")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert!(f.sessions().is_empty());
    f.ok(&["--help"]);
    f.ok(&["--version"]);
    assert!(!f.output(&["close"]).status.success());
    f.ok(&["open", "list", "3000", "--name", "reserved"]);
    f.ok(&["close", "reserved"]);
    f.ok(&["open", "group", "3001", "--name", "reserved-group"]);
    f.ok(&["close", "reserved-group"]);
}

#[test]
fn jump_route_is_passed_to_ssh_and_retained_in_session_state() {
    for (option, jump) in [("--jump", "bob@bastion:2222"), ("-J", "bastion")] {
        let f = Fixture::new();
        let output = f.ok(&["internal", "8080:3000", option, jump, "--name", "jump"]);
        let route = format!("{jump} -> internal");
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("SSH route: {route}")));
        let session = f.wait_state("jump", "running");
        assert_eq!(session["host"], "internal");
        assert_eq!(session["jump"], jump);
        assert!(String::from_utf8_lossy(&f.ok(&["list"]).stdout).contains(&route));
        wait_for(|| f.root.path().join("args").exists());
        let args = fs::read_to_string(f.root.path().join("args")).unwrap();
        assert!(args.contains(&format!("-J\n{jump}\n--\ninternal\n")));
        assert!(args.contains("-L\n127.0.0.1:8080:127.0.0.1:3000\n"));
        f.ok(&["close", "jump"]);
        assert_eq!(f.session("jump")["jump"], jump);
        assert_eq!(f.session("jump")["state"], "closed");
    }
}

#[test]
fn concurrent_creation_reserves_one_name() {
    let f = Fixture::new();
    let one = f
        .command()
        .args(["devbox", "3000", "--name", "race"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let two = f
        .command()
        .args(["devbox", "3001", "--name", "race"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let successes = [one, two]
        .into_iter()
        .filter_map(|mut child| child.wait().ok())
        .filter(|status| status.success())
        .count();
    assert_eq!(successes, 1);
    assert_eq!(f.sessions().len(), 1);
    f.ok(&["close", "race"]);
}

#[test]
fn concurrent_close_is_idempotent_and_cleans_helpers() {
    let f = Fixture::new();
    let output = f
        .command()
        .env("FORD_TEST_MODE", "stubborn")
        .args(["devbox", "3000", "--name", "stubborn"])
        .output()
        .unwrap();
    assert!(output.status.success());
    wait_for(|| f.root.path().join("helper").exists());
    let helper: i32 = fs::read_to_string(f.root.path().join("helper"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let start = Instant::now();
    let one = f
        .command()
        .args(["close", "stubborn"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let two = f
        .command()
        .args(["close", "stubborn"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    for mut child in [one, two] {
        assert!(child.wait().unwrap().success());
    }
    assert!(start.elapsed() >= Duration::from_secs(4));
    assert_eq!(f.session("stubborn")["exit_signal"], Signal::SIGKILL as i32);
    #[cfg(target_os = "linux")]
    wait_for(|| {
        fs::read_to_string(format!("/proc/{helper}/stat")).map_or(true, |s| s.contains(") Z "))
    });
    #[cfg(not(target_os = "linux"))]
    wait_for(|| kill(Pid::from_raw(helper), None).is_err());
}

#[test]
fn failure_keeps_diagnostics_and_original_exit() {
    let f = Fixture::new();
    let _ = f
        .command()
        .env("FORD_TEST_MODE", "fail")
        .args(["devbox", "3000", "--name", "bad"])
        .output()
        .unwrap();
    let failed = f.wait_state("bad", "failed");
    assert_eq!(failed["exit_code"], 255);
    assert_eq!(failed["cleanup_complete"], true);
    let logs = f.ok(&["logs", "bad"]);
    assert!(String::from_utf8_lossy(&logs.stdout).contains("SSH fixture diagnostic"));
    f.ok(&["close", "bad"]);
}

#[test]
fn exited_ssh_still_cleans_its_live_process_group() {
    let f = Fixture::new();
    let _ = f
        .command()
        .env("FORD_TEST_MODE", "orphan")
        .args(["devbox", "3000", "--name", "orphan"])
        .output()
        .unwrap();
    let failed = f.wait_state("orphan", "failed");
    assert_eq!(failed["exit_code"], 255);
    assert_eq!(failed["cleanup_complete"], true);
    let helper: i32 = fs::read_to_string(f.root.path().join("helper"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    #[cfg(target_os = "linux")]
    wait_for(|| {
        fs::read_to_string(format!("/proc/{helper}/stat")).map_or(true, |s| s.contains(") Z "))
    });
    #[cfg(not(target_os = "linux"))]
    wait_for(|| kill(Pid::from_raw(helper), None).is_err());
    f.ok(&["close", "orphan"]);
}

#[test]
fn foreground_signal_closes_owned_child() {
    let f = Fixture::new();
    let mut child = f
        .command()
        .args(["devbox", "3000", "--name", "fg", "--foreground"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(|| {
        f.sessions()
            .iter()
            .any(|s| s["name"] == "fg" && s["state"] == "running")
    });
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert_eq!(f.session("fg")["state"], "closed");
}

#[test]
fn unreachable_supervisor_is_never_closed_by_disk_pid() {
    let f = Fixture::new();
    f.ok(&["devbox", "3000", "--name", "lost"]);
    let session = f.wait_state("lost", "running");
    f.ok(&["group", "save", "lost-group", "lost"]);
    let supervisor = session["supervisor_pid"].as_i64().unwrap() as i32;
    let ssh = session["ssh_pid"].as_i64().unwrap() as i32;
    // The fixture publishes args after its startup diagnostic. Killing the reader earlier can SIGPIPE the fixture.
    wait_for(|| f.root.path().join("args").exists());
    kill(Pid::from_raw(supervisor), Signal::SIGKILL).unwrap();
    let lost = f.wait_state("lost", "unreachable");
    assert_eq!(lost["cleanup_complete"], false);
    assert!(!f.output(&["close", "lost"]).status.success());
    assert!(!f.output(&["group", "open", "lost-group"]).status.success());
    assert!(!f.output(&["group", "close", "lost-group"]).status.success());
    assert_eq!(f.sessions().len(), 1);
    assert!(
        kill(Pid::from_raw(ssh), None).is_ok(),
        "close killed an unowned PID"
    );
    assert!(
        !f.output(&["devbox", "3001", "--name", "lost"])
            .status
            .success()
    );
    // This test deliberately killed its supervisor. Clean up the fixture's known live process group.
    killpg(Pid::from_raw(ssh), Signal::SIGKILL).unwrap();
}

#[test]
fn logs_are_bounded_and_available_after_closure() {
    let f = Fixture::new();
    let output = f
        .command()
        .env("FORD_TEST_MODE", "logs")
        .args(["devbox", "3000", "--name", "logs"])
        .output()
        .unwrap();
    assert!(output.status.success());
    wait_for(|| {
        String::from_utf8_lossy(&f.ok(&["logs", "logs"]).stdout).contains("log-tail-marker")
    });
    f.ok(&["close", "logs"]);
    let session = f.session("logs");
    let path = PathBuf::from(session["log_path"].as_str().unwrap());
    assert!(fs::metadata(&path).unwrap().len() <= 1024 * 1024);
    assert!(fs::metadata(path.with_extension("log.1")).unwrap().len() <= 1024 * 1024);
}

#[test]
fn unsafe_paths_and_missing_ssh_fail_explicitly() {
    let f = Fixture::new();
    fs::remove_file(f.root.path().join("bin/ssh")).unwrap();
    let output = f
        .command()
        .env("PATH", f.root.path().join("bin"))
        .args(["devbox", "3000", "--name", "missing"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(127),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.session("missing")["state"], "failed");
    let victim = f.root.path().join("victim");
    fs::write(&victim, "unchanged").unwrap();
    let run = f.root.path().join("run/ford");
    let symlink_path = f.root.path().join("unsafe-run");
    symlink(&run, &symlink_path).unwrap();
    assert!(
        !f.command()
            .env("XDG_RUNTIME_DIR", &symlink_path)
            .arg("list")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(victim).unwrap(), "unchanged");
}

#[test]
fn abandoned_startup_stops_the_uncommitted_session() {
    let f = Fixture::new();
    f.ok(&["devbox", "3000", "--name", "template"]);
    f.ok(&["close", "template"]);
    let mut record = f.session("template");
    let id = "00000000000000000000000000000000";
    record["id"] = id.into();
    record["name"] = "abandoned".into();
    record["state"] = "starting".into();
    record["supervisor_pid"] = Value::Null;
    record["ssh_pid"] = Value::Null;
    record["cleanup_complete"] = false.into();
    record["log_path"] = f
        .root
        .path()
        .canonicalize()
        .unwrap()
        .join(format!("state/ford/logs/{id}.log"))
        .to_str()
        .unwrap()
        .into();
    let path = f.root.path().join(format!("state/ford/sessions/{id}.json"));
    assert!(!path.exists());
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let (mut client, worker) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let input: OwnedFd = worker.try_clone().unwrap().into();
    let output: OwnedFd = worker.into();
    let mut child = f
        .command()
        .args(["--internal-supervise", id])
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    writeln!(
        client,
        "{}",
        serde_json::json!({"version": 1, "session_id": id, "stage": "launch"})
    )
    .unwrap();
    let mut ready = String::new();
    BufReader::new(client.try_clone().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let response: Value = serde_json::from_str(&ready).unwrap();
    assert!(response["error"].is_null(), "{ready}");
    drop(client);
    assert!(!child.wait().unwrap().success());
    let failed = f.wait_state("abandoned", "failed");
    assert_eq!(failed["cleanup_complete"], true);
    assert!(!f.root.path().join(format!("run/ford/{id}.sock")).exists());
}

#[test]
fn invalid_control_messages_do_not_close_the_session() {
    let f = Fixture::new();
    f.ok(&["devbox", "3000", "--name", "protocol"]);
    let session = f.wait_state("protocol", "running");
    let id = session["id"].as_str().unwrap();
    let socket = f.root.path().join(format!("run/ford/{id}.sock"));
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "version": 999, "session_id": id, "request_id": "invalid", "operation": "close"
        })
    )
    .unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert!(response["error"].as_str().unwrap().contains("mismatch"));
    let mut oversized = UnixStream::connect(&socket).unwrap();
    let _ = oversized.write_all(&vec![b'x'; 65536]);
    drop(oversized);
    assert_eq!(f.session("protocol")["state"], "running");
    f.ok(&["close", "protocol"]);
}

#[test]
fn saved_group_restores_routes_without_reusing_sessions_or_closing_unrelated_ones() {
    let f = Fixture::new();
    f.ok(&["devbox", "8080:3000", "5173", "--name", "web"]);
    f.ok(&[
        "internal",
        "9000:8000",
        "--jump",
        "bastion",
        "--name",
        "api",
    ]);
    f.ok(&["other", "9999", "--name", "outside"]);
    let old_ids: Vec<_> = f
        .sessions()
        .into_iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    f.ok(&["group", "save", "work", "web", "api"]);
    let file = f.root.path().join("state/ford/groups/work.json");
    let bytes = fs::read(&file).unwrap();
    let group: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(group["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(group["sessions"][1]["jump"], "bastion");
    assert!(group["sessions"][0].get("ssh_pid").is_none());
    assert!(group["sessions"][0].get("target_checks").is_none());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        String::from_utf8_lossy(&f.ok(&["group", "show", "work"]).stdout)
            .contains("bastion -> internal")
    );
    let skipped = f.ok(&["group", "open", "work"]);
    assert!(String::from_utf8_lossy(&skipped.stdout).contains("already running 2"));
    assert_eq!(f.sessions().len(), 3);
    f.ok(&["group", "close", "work"]);
    assert_eq!(f.session("outside")["state"], "running");
    assert_eq!(fs::read(&file).unwrap(), bytes);
    f.ok(&["group", "open", "work"]);
    let restored: Vec<_> = f
        .sessions()
        .into_iter()
        .filter(|s| s["state"] == "running" && s["name"] != "outside")
        .collect();
    assert_eq!(restored.len(), 2);
    for session in &restored {
        assert!(!old_ids.iter().any(|id| session["id"] == id.as_str()));
        assert_eq!(session["target"], "unverified");
    }
    assert_eq!(
        restored.iter().find(|s| s["name"] == "api").unwrap()["jump"],
        "bastion"
    );
    f.ok(&["group", "remove", "work"]);
    assert!(!file.exists());
    assert_eq!(
        f.sessions()
            .iter()
            .filter(|s| s["state"] == "running")
            .count(),
        3
    );
    assert_eq!(
        serde_json::from_slice::<Vec<Value>>(&f.ok(&["group", "list", "--json"]).stdout)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn saved_groups_can_be_replaced_and_created_from_running_or_completed_sessions() {
    let f = Fixture::new();
    f.ok(&["devbox", "8080", "--name", "web"]);
    f.ok(&["devbox", "9000", "--name", "api"]);
    f.ok(&["group", "save", "work", "web"]);
    let file = f.root.path().join("state/ford/groups/work.json");
    let before = fs::read(&file).unwrap();
    assert!(!f.output(&["group", "save", "work", "api"]).status.success());
    assert_eq!(fs::read(&file).unwrap(), before);
    f.ok(&["group", "save", "work", "api", "--force"]);
    f.ok(&["close", "web"]);
    f.ok(&["group", "save", "history", "web"]);
    f.ok(&["group", "save", "current", "--all"]);
    let groups: Vec<Value> =
        serde_json::from_slice(&f.ok(&["group", "list", "--json"]).stdout).unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|g| g["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["current", "history", "work"]
    );
    assert_eq!(groups[0]["sessions"][0]["name"], "api");
    assert_eq!(groups[0]["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(f.session("api")["state"], "running");
    assert!(
        String::from_utf8_lossy(&f.ok(&["group", "show", "work"]).stdout).contains("9000:9000")
    );
}

#[test]
fn group_open_rejects_conflicts_before_starting_other_entries() {
    let f = Fixture::new();
    f.ok(&["devbox", "8080", "--name", "web"]);
    f.ok(&["devbox", "9000", "--name", "api"]);
    f.ok(&["group", "save", "work", "web", "api"]);
    f.ok(&["group", "close", "work"]);
    f.ok(&["different", "9001", "--name", "api"]);
    let count = f.sessions().len();
    let result = f.output(&["group", "open", "work"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("conflicts with session"));
    assert_eq!(f.sessions().len(), count);
    f.ok(&["group", "close", "work"]);
    assert_eq!(
        f.sessions()
            .iter()
            .filter(|s| s["state"] == "running")
            .count(),
        1
    );
    f.ok(&["close", "api"]);
    f.ok(&["other", "8080", "--name", "occupied"]);
    assert!(!f.output(&["group", "open", "work"]).status.success());
    assert_eq!(f.sessions().len(), count + 1);
}

#[test]
fn concurrent_group_opens_do_not_duplicate_tunnels() {
    let f = Fixture::new();
    f.ok(&["devbox", "8080", "--name", "web"]);
    f.ok(&["devbox", "9000"]);
    f.ok(&["group", "save", "work", "--all"]);
    f.ok(&["group", "close", "work"]);
    let one = f
        .command()
        .args(["group", "open", "work"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let two = f
        .command()
        .args(["group", "open", "work"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    for mut child in [one, two] {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(f.sessions().len(), 4);
    assert_eq!(
        f.sessions()
            .iter()
            .filter(|s| s["state"] == "running")
            .count(),
        2
    );
    let one = f
        .command()
        .args(["group", "save", "new", "web"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let two = f
        .command()
        .args(["group", "save", "new", "web"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let successes = [one, two]
        .into_iter()
        .map(|mut child| child.wait().unwrap().success())
        .filter(|success| *success)
        .count();
    assert_eq!(successes, 1);
}

#[test]
fn invalid_and_unsafe_groups_do_not_start_sessions_or_overwrite_other_files() {
    let f = Fixture::new();
    assert_eq!(
        f.output(&["group", "open", "../bad"]).status.code(),
        Some(2)
    );
    assert!(!f.root.path().join("state/ford").exists());
    f.ok(&["devbox", "8080", "--name", "web"]);
    f.ok(&["other", "8080", "--name", "duplicate"]);
    assert!(
        !f.output(&["group", "save", "bad", "web", "duplicate"])
            .status
            .success()
    );
    assert!(
        !f.output(&["group", "save", "bad", "web", "missing"])
            .status
            .success()
    );
    assert!(!f.root.path().join("state/ford/groups/bad.json").exists());
    f.ok(&["group", "save", "safe", "web"]);
    let file = f.root.path().join("state/ford/groups/safe.json");
    let original = fs::read(&file).unwrap();
    let mut group: Value = serde_json::from_slice(&original).unwrap();
    group["sessions"][0]["host"] = "host;bad".into();
    fs::write(&file, serde_json::to_vec(&group).unwrap()).unwrap();
    assert!(!f.output(&["group", "open", "safe"]).status.success());
    assert_eq!(f.sessions().len(), 2);
    fs::write(&file, &original).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!f.output(&["group", "open", "safe"]).status.success());
    fs::remove_file(&file).unwrap();
    let victim = f.root.path().join("victim.json");
    fs::write(&victim, "unchanged").unwrap();
    symlink(&victim, &file).unwrap();
    for command in [
        vec!["group", "open", "safe"],
        vec!["group", "remove", "safe"],
        vec!["group", "save", "safe", "web", "--force"],
    ] {
        assert!(!f.output(&command).status.success());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "unchanged");
    }
    let directory = file.parent().unwrap();
    fs::remove_dir_all(directory).unwrap();
    let outside = f.root.path().join("outside-groups");
    fs::DirBuilder::new().mode(0o700).create(&outside).unwrap();
    symlink(&outside, directory).unwrap();
    assert!(!f.output(&["group", "save", "safe", "web"]).status.success());
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

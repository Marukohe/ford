use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    executable: PathBuf,
    asset: PathBuf,
}

impl Fixture {
    fn new(version: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        for directory in [
            "bin with 'quotes' and $(touch injected)",
            "tools",
            "assets",
            "unpack",
            "home/.local/share/ford",
        ] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        let executable = root
            .path()
            .join("bin with 'quotes' and $(touch injected)/ford");
        fs::copy(env!("CARGO_BIN_EXE_ford"), &executable).unwrap();
        fs::write(
            root.path().join("home/.bashrc"),
            "existing shell settings\n",
        )
        .unwrap();
        fs::write(
            root.path().join("home/.local/share/ford/env"),
            "existing PATH loader\n",
        )
        .unwrap();
        let curl = root.path().join("tools/curl");
        fs::write(
            &curl,
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        https://*) url=$1 ;;
        --output) shift; destination=$1 ;;
    esac
    shift
done
printf '%s\n' "$url" >> "$FORD_TEST_ROOT/urls"
asset=$FORD_TEST_ROOT/assets/${url##*/}
[ -f "$asset" ] || exit 22
cp "$asset" "$destination"
"#,
        )
        .unwrap();
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
        let platform = if cfg!(target_os = "macos") {
            "apple-darwin"
        } else {
            "unknown-linux-musl"
        };
        let asset = root.path().join(format!(
            "assets/ford-v{version}-{}-{platform}.tar.gz",
            std::env::consts::ARCH
        ));
        let fixture = Self {
            root,
            executable,
            asset,
        };
        fixture.release(version, &format!("#!/bin/sh\nprintf 'ford {version}\\n'\n"));
        fixture
    }

    fn release(&self, version: &str, binary: &str) {
        let source = self.root.path().join("unpack/ford");
        fs::write(&source, binary).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&self.asset)
                .arg("-C")
                .arg(source.parent().unwrap())
                .arg("ford")
                .status()
                .unwrap()
                .success()
        );
        assert!(
            self.asset
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains(&format!("ford-v{version}-"))
        );
        self.manifest();
    }

    fn manifest(&self) {
        let hash = Command::new("sha256sum")
            .arg(&self.asset)
            .output()
            .or_else(|_| {
                Command::new("shasum")
                    .args(["-a", "256"])
                    .arg(&self.asset)
                    .output()
            })
            .unwrap();
        assert!(hash.status.success());
        let digest = String::from_utf8(hash.stdout).unwrap();
        let manifest = format!(
            "{}  {}\n",
            digest.split_whitespace().next().unwrap(),
            self.asset.file_name().unwrap().to_str().unwrap()
        );
        fs::write(self.root.path().join("assets/SHA256SUMS"), manifest).unwrap();
    }

    fn update(&self, check: bool) -> Output {
        let mut command = Command::new(&self.executable);
        command.arg("update");
        if check {
            command.arg("--check");
        }
        let output = command
            .env("HOME", self.root.path().join("home"))
            .env("SHELL", "/bin/bash")
            .env("FORD_TEST_ROOT", self.root.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.path().join("tools").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap();
        assert_eq!(
            fs::read_to_string(self.root.path().join("home/.bashrc")).unwrap(),
            "existing shell settings\n"
        );
        assert_eq!(
            fs::read_to_string(self.root.path().join("home/.local/share/ford/env")).unwrap(),
            "existing PATH loader\n"
        );
        assert!(!self.root.path().join("home/.local/state/ford").exists());
        for entry in fs::read_dir(self.executable.parent().unwrap()).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(!name.to_string_lossy().starts_with(".ford-update-"));
        }
        output
    }
}

#[test]
fn update_replaces_the_running_binary_in_a_custom_directory() {
    let fixture = Fixture::new("9.0.0");
    let output = fixture.update(false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Updated ford v9.0.0"));
    assert_eq!(
        Command::new(&fixture.executable)
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
        b"ford 9.0.0\n"
    );
    assert!(
        fs::metadata(&fixture.executable)
            .unwrap()
            .permissions()
            .mode()
            & 0o111
            != 0
    );
    let urls = fs::read_to_string(fixture.root.path().join("urls")).unwrap();
    assert!(urls.contains("/releases/latest/download/SHA256SUMS"));
    assert!(urls.contains("/releases/download/v9.0.0/ford-v9.0.0-"));
    assert!(!PathBuf::from("injected").exists());
}

#[test]
fn check_only_and_current_or_older_releases_never_replace_the_binary() {
    for (version, check) in [
        ("9.0.0", true),
        (env!("CARGO_PKG_VERSION"), false),
        ("0.0.1", false),
    ] {
        let fixture = Fixture::new(version);
        let original = fs::read(&fixture.executable).unwrap();
        let output = fixture.update(check);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&fixture.executable).unwrap(), original);
        let urls = fs::read_to_string(fixture.root.path().join("urls")).unwrap();
        assert_eq!(urls.lines().count(), 1, "{urls}");
        let message = String::from_utf8_lossy(&output.stdout);
        assert!(message.contains(if check {
            "Update available"
        } else {
            "is up to date"
        }));
    }
}

#[test]
fn update_failures_preserve_the_existing_executable() {
    for failure in [
        "checksum",
        "version",
        "executable",
        "archive",
        "download",
        "manifest",
    ] {
        let fixture = Fixture::new("9.0.0");
        match failure {
            "checksum" => fs::write(&fixture.asset, "tampered archive").unwrap(),
            "version" => fixture.release("9.0.0", "#!/bin/sh\necho 'ford 0.0.1'\n"),
            "executable" => fixture.release("9.0.0", "#!/bin/sh\nexit 1\n"),
            "archive" => {
                fs::write(&fixture.asset, "not a gzip archive").unwrap();
                fixture.manifest();
            }
            "download" => fs::remove_file(&fixture.asset).unwrap(),
            "manifest" => fs::remove_file(fixture.root.path().join("assets/SHA256SUMS")).unwrap(),
            _ => unreachable!(),
        }
        let original = fs::read(&fixture.executable).unwrap();
        let output = fixture.update(false);
        assert!(!output.status.success(), "{failure}");
        assert_eq!(
            fs::read(&fixture.executable).unwrap(),
            original,
            "{failure}"
        );
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn updating_through_a_symlink_preserves_the_link() {
    let fixture = Fixture::new("9.0.0");
    let alias = fixture.root.path().join("ford");
    symlink(&fixture.executable, &alias).unwrap();
    let actual = fs::canonicalize(&alias).unwrap();
    let fixture = Fixture {
        executable: alias,
        ..fixture
    };
    assert!(fixture.update(false).status.success());
    assert!(fixture.executable.is_symlink());
    assert_eq!(
        Command::new(actual)
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
        b"ford 9.0.0\n"
    );
}

#[test]
fn an_existing_update_lock_prevents_another_replacement() {
    let fixture = Fixture::new("9.0.0");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(
            fixture
                .executable
                .parent()
                .unwrap()
                .join(".ford-update.lock"),
        )
        .unwrap();
    lock.lock().unwrap();
    let original = fs::read(&fixture.executable).unwrap();
    let output = fixture.update(false);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot lock installation"));
    assert!(!fixture.root.path().join("urls").exists());
    assert_eq!(fs::read(&fixture.executable).unwrap(), original);
}

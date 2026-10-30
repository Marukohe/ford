# ford

A lightweight Rust CLI for forwarding remote TCP ports over system OpenSSH, with background sessions and live control.

```bash
cargo build --release

./target/release/ford devbox 3000 --name web
./target/release/ford list
./target/release/ford close web
```

Open `http://127.0.0.1:3000` to access the service on `devbox` at `127.0.0.1:3000`. Only the remote SSH port needs to be
accessible. Nothing needs to be installed remotely beyond an SSH server that permits forwarding.

## Requirements

- Linux or macOS. Linux is tested; macOS ARM64 is build-checked but has not been tested at runtime.
- Rust 1.93.1, selected by `rust-toolchain.toml`, to build from source.
- An OpenSSH client on PATH, configured keys or an SSH agent, and a verified remote host key.

ford uses noninteractive authentication. Before first use, run ordinary `ssh devbox` to confirm the host key and verify
login. ford reads your existing SSH aliases, identities, login ports, and jump-host configuration.

## Commands

```bash
# Local 8080 -> remote 127.0.0.1:300
ford user@example.com 8080:300 --name web

# Multiple mappings share one SSH connection
ford devbox 3000 5173 8080:8000 --name dev

# Stream diagnostics; Ctrl+C closes the session
ford devbox 3000 --foreground

ford list
ford list --all                 # Include completed sessions
ford list --json                # Structured live state
ford close web                  # Name, full ID, or unique ID prefix
ford close --all
ford logs web --lines 100

# Explicit creation syntax, including hosts named after subcommands
ford open list 3000
```

Creation runs in the background by default and returns a session ID. `running` means that the SSH process is alive;
forwarding and target availability remain `unverified`. Creation acknowledgment does not prove the web service is ready.

Each session has one Rust supervisor and one SSH child; the Rust process does not carry web traffic. `close` asks the
supervisor to terminate and reap its SSH child before reporting success. It escalates to SIGKILL if SIGTERM takes more
than five seconds. Once sessions finish, their supervisors exit and leave metadata and bounded logs for inspection.

An unreachable supervisor is reported as `unreachable`. ford does not kill a PID from a stale state file. Automatic
reconnection and orphan-session recovery are not implemented. See [the design](docs/design.md) for behavior and limits.

## Local Files

- Runtime sockets and locks: `$XDG_RUNTIME_DIR/ford`, or `~/.ford/run` when unset.
- State and logs: `$XDG_STATE_HOME/ford`, or `~/.local/state/ford` when unset.
- ford directories are private (0700); files and control sockets are restricted to the current user (0600).
- Each log is capped at 1 MiB with one rotated backup. Completed session records are retained.

Invalid runtime directory ownership, permissions, or symbolic links cause an explicit error. Existing SSH forwarding
directives are also inherited; use an SSH alias without extra forwards if you want only the mappings requested by ford.

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Requires Python 3, ssh, ssh-keygen, and sshd; uses isolated keys and loopback listeners
python3 scripts/test-ssh.py

# Release-build integration checks and Linux management overhead measurements
cargo build --release
python3 scripts/test-ssh.py --binary target/release/ford --bench --report target/ssh-validation.json

# Compile check only; this does not establish macOS runtime correctness
rustup target add aarch64-apple-darwin --toolchain 1.93.1
cargo check --target aarch64-apple-darwin
```

Default Cargo tests use private temporary directories and a controlled SSH substitute; they do not use personal SSH
aliases or credentials. The SSH script creates temporary keys and configuration under your home directory so sshd's
strict path checks remain enabled. It does not modify your SSH configuration or keys.

For architecture and validation details, see [docs/design.md](docs/design.md).

## Releases

Tag-triggered GitHub Actions build native Linux and macOS archives for x86_64 and ARM64, attach SHA256 checksums, and
publish version-specific release notes. See [CHANGELOG.md](CHANGELOG.md) for changes and
[docs/releasing.md](docs/releasing.md) for first-release setup, version updates, and local packaging previews.

The workflow is configured locally; a GitHub remote and the first hosted run are still needed before downloads exist.

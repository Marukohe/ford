# v0.1 Validation

Validated on 2026-10-31 with Rust 1.93.1, Linux x86_64 (kernel 7.0.0-38-generic), and OpenSSH 9.6p1.
These are local observations, not performance guarantees for remote machines or other operating systems.

## Correctness

- `cargo fmt --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo test --locked`: 14 passed (2 unit tests and 12 session integration tests).
- `cargo clippy --locked --all-targets --target aarch64-apple-darwin -- -D warnings`: passed.
- Real SSH validation used isolated sshd instances, temporary keys, strict host key verification, and loopback services.

The Cargo tests cover mappings, hostile/invalid inputs, argument boundaries, background creation, live status, name
reservation, concurrent creation and closure, forced shutdown, foreground SIGINT, failed SSH exits, bounded logs,
unreachable supervisors, unsafe paths, missing SSH, protocol errors, and abandoned startup before commit.

Native [CI run #3](https://github.com/Marukohe/ford/actions/runs/41095366317) passed at commit `b82b076` on Linux x86_64
and ARM64 and macOS x86_64 and ARM64. All four runners passed formatting, Clippy, Cargo tests, and release-helper tests.
The macOS tests use the controlled SSH substitute; real OpenSSH forwarding tests run on the two Linux runners.

The macOS fix handles `killpg` returning `EPERM` for a group containing only exited processes, after confirming the
owned SSH child has exited without reaping it. Tests cover original SSH exit codes, concurrent close, foreground
interruption, and cleanup of a live helper after its SSH group leader exits.

Real SSH checks passed for:

- HTTP byte round trips through multiple forwarded ports and a WebSocket upgrade with an echoed frame.
- Live list, close by ID prefix, SSH/supervisor termination, and port release.
- ProxyJump forwarding and ordinary proxy-child cleanup.
- Local port conflicts without silently changing the requested port.
- Authentication and changed host key failures with retained diagnostics.
- Unavailable remote targets and server forwarding denial while retaining honest lifecycle/health distinctions.
- Foreground interruption and cleanup.
- Creation, listing, and closure at 1, 10, and 50 real SSH sessions without live process residue.

## Release Build

The release binary was 1,658,648 bytes (about 1.58 MiB). Its warm `--help` startup median was 0.64 ms over 30
invocations
in the integration measurement. Startup includes the measuring Python process's subprocess-launch overhead.

A separate 30-invocation comparison measured the default release configuration against the selected thin-LTO build
with debug information stripped:

| Build | Binary bytes | Warm help median (ms) |
| --- | ---: | ---: |
| No LTO, no stripping | 5,169,664 | 0.67 |
| Thin LTO, debug information stripped | 1,658,648 | 0.68 |

This supports the size choice; the startup samples do not show a meaningful speed difference. No cold-cache result
is claimed, and this comparison does not isolate the effects of LTO from stripping.

## Management Overhead

Each session used two processes and three threads total: a supervisor with a log-draining thread and one SSH process.
RSS includes both processes and can count shared pages more than once. sshd and test HTTP server memory are excluded.

| Sessions | Processes | Total RSS (KiB) | Mean create ack (ms) | List (ms) | Close all (ms) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2 | 11,348 | 5.6 | 2.8 | 106 |
| 10 | 20 | 112,720 | 7.5 | 4.2 | 1,026 |
| 50 | 100 | 565,020 | 12.1 | 11.8 | 5,120 |

Mean time from launching creation to the first successful HTTP response was 167.9, 169.5, and 174.0 ms respectively.
This includes SSH authentication, handshake, and readiness polling at 20 ms intervals. Creation acknowledgment alone
is not readiness. `close --all` currently processes sessions sequentially; shutdown observation uses a 100 ms poll
bound.

No process CPU ticks accumulated in each one-second idle sample. This is limited by the OS accounting resolution and
does not establish zero CPU consumption.

## Transport Sample

The script copied the live SSH argv and ran direct SSH with identical arguments except for the local forwarding port.
It measured 20 small HTTP GETs and five 1 MiB GETs through each connection:

| Connection | Median small GET (ms) | Bulk transfer (MiB/s) |
| --- | ---: | ---: |
| ford | 40.50 | 603 |
| Direct SSH | 40.99 | 510 |

This single sequential loopback sample includes HTTP-server and client behavior and is sensitive to scheduling and
cache effects. It does not establish a throughput advantage for ford or predict WAN performance. Both connections use
OpenSSH for the actual data path.

## Reproduction

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
python3 scripts/test-ssh.py --binary target/release/ford --bench --report target/ssh-validation.json

CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_STRIP=none \
  cargo build --release --target-dir target/baseline

rustup target add aarch64-apple-darwin --toolchain 1.93.1
cargo check --all-targets --target aarch64-apple-darwin
```

The SSH script requires Python 3, ssh, ssh-keygen, sshd, curl, and lsof. It creates a private temporary configuration
under the current user's home directory so sshd's strict directory checks remain enabled. It leaves existing SSH
settings and keys untouched. Benchmark mode uses Linux `/proc`; ordinary SSH checks do not require it.

Automatic reconnection, orphan adoption after supervisor failure, native Windows support, and remote-health detection
remain outside v0.1. See [design.md](design.md) for lifecycle and recovery boundaries.

## Unreleased HTTP target checks

The isolated real SSH suite covers successful responses, missing health paths, expected status mismatches, timeouts,
and per-port results. It verifies that stopping the app leaves the session running, a different local listener cannot
verify the target, and list and close remain responsive during a check. These checks run on Linux in CI.

All 12 real SSH checks passed locally on Linux x86_64 with OpenSSH 9.6p1. The macOS ARM64 cross-Clippy check passed;
native macOS execution of the HTTP checks has not been tested.

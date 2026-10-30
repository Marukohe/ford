# v0.1 Validation

Validated on 2026-10-31 with Rust 1.93.1, Linux x86_64 (kernel 7.0.0-38-generic), and OpenSSH 9.6p1.
These are local observations, not performance guarantees for remote machines or other operating systems.

## Correctness

- `cargo fmt --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo test`: 13 passed (2 unit tests and 11 session integration tests).
- `cargo check --all-targets --target aarch64-apple-darwin`: passed. No macOS runtime testing was performed.
- Real SSH validation used isolated sshd instances, temporary keys, strict host key verification, and loopback services.

The Cargo tests cover mappings, hostile/invalid inputs, argument boundaries, background creation, live status, name
reservation, concurrent creation and closure, forced shutdown, foreground SIGINT, failed SSH exits, bounded logs,
unreachable supervisors, unsafe paths, missing SSH, protocol errors, and abandoned startup before commit.

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

The SSH script requires Python 3, ssh, ssh-keygen, and sshd. It creates private temporary configuration under the
current
user's home directory so sshd's strict directory checks remain enabled. It leaves existing SSH configuration and keys
untouched. The benchmark mode uses Linux `/proc`; ordinary SSH checks do not require `/proc`.

Automatic reconnection, orphan adoption after supervisor failure, native Windows support, and remote-health detection
remain outside v0.1. See [design.md](design.md) for lifecycle and recovery boundaries.

# ford Design

Status: implemented through v0.2, with saved groups and bounded reconnection added for the next release.
Updated: 2026-11-14. The architecture uses spawn with a Rust supervisor. Historical platform checks are recorded in
[validation.md](validation.md); they do not establish validation of later changes.

## 1. Goals and Prerequisites

Forward a remote web service to the local machine with one short command. Prioritize low persistent overhead, fast
startup, compatibility with existing SSH workflows, and predictable shutdown behavior.

The local machine must have an OpenSSH client. The remote machine must be accessible over SSH and allow the requested
TCP forwarding. Only its SSH port needs to be exposed; the web service port can remain private, and ford need not be
installed remotely.

```text
Local browser → 127.0.0.1:8080 → encrypted SSH connection → remote 127.0.0.1:300
```

The first release supports Linux and macOS. Native Windows process and signal handling require a separate design and are
outside the initial compatibility commitment. The remote OS need not be Unix, provided its SSH server supports the
requested forwarding.

## 2. Technology and Architecture

- **Component: Language and build**
  - Choice: Rust 1.93.1, Cargo, committed Cargo.lock
  - Reason: Control resource use and keep build and dependency management consistent
- **Component: CLI**
  - Choice: `clap`
  - Reason: Argument validation, help, and errors; enable features as needed
- **Component: Forwarding**
  - Choice: Local `ssh -N -T -L`
  - Reason: Reuse established transport, authentication, and SSH configuration support
- **Component: Process management**
  - Choice: `std::process::Command::spawn`, Rust supervisor
  - Reason: Own the SSH lifecycle, state, and shutdown requests
- **Component: Local control**
  - Choice: Standard-library Unix domain sockets
  - Reason: Handle list/close without a TCP listener or killing processes by bare PID
- **Component: State and protocol**
  - Choice: `serde` / `serde_json`
  - Reason: Small structured messages and state snapshots without a database
- **Component: Unix facilities**
  - Choice: `nix`, `signal-hook`, and `rustix`, with features enabled as needed
  - Reason: Sessions, process groups, socket flags, signals, and portable non-reaping waits without application unsafe
    code. File locks use the standard library.
- **Component: Session IDs**
  - Choice: `getrandom`
  - Reason: Obtain unique random IDs from the OS without a larger random-number framework
- **Component: Output**
  - Choice: Concise text and per-session logs
  - Reason: Preserve SSH diagnostics in background mode

```text
ford CLI ──create──→ ford session supervisor ──spawn──→ ssh → remote service
   │                         ↑
   └── list / close ─────────┘ private Unix socket

Web traffic: browser → ssh → remote service; bypasses Rust
```

Each active session has one Rust supervisor and at most one direct SSH child process. Multiple ports within it share
the SSH connection. The supervisor owns the `Child` handle, live state, and control socket, and explicitly waits for,
terminates, and reaps SSH.[1]

Use one supervisor per session rather than a global persistent daemon. No Rust background process remains after the last
session closes. The list/close commands also exit after completing their work. For a small number of tunnels, this
avoids the installation and recovery complexity of a global service. Consider a shared supervisor only if measurements
show that managing many sessions requires it.

In background mode, the CLI starts an internal supervisor entry point in the same ford executable. Startup pipes carry
the request and acknowledgment. The supervisor detaches from the launching terminal, redirects standard I/O, and spawns
SSH. In foreground mode, the current ford process acts as the supervisor. The internal entry point is not a stable
public command.

The first release has no async runtime, named forwarding configuration files, database, HTTP proxy, or embedded SSH
library. It ships as one ford executable but still requires system OpenSSH at runtime; it is not fully self-contained.

Multiple `-L` options forward multiple ports over a single SSH connection. HTTP, HTTPS, and WebSocket traffic is carried
as TCP bytes. ford does not modify headers, TLS, or page content.[2]

## 3. First-Release Command Contract

```bash
# Create a named background session: local 3000 → remote 127.0.0.1:3000
ford devbox 3000 --name web

# Local 8080 → remote 127.0.0.1:300
ford user@example.com 8080:300

# Forward multiple ports over one SSH connection
ford devbox 3000 5173 8080:8000

# Run the supervisor in the foreground; Ctrl+C closes this session
ford devbox 3000 --foreground

ford list
ford list --all
ford close web
ford close <SESSION_ID>
ford close --all
ford logs web
ford logs web --lines 100
ford list --json

ford --help
ford --version
```

Creation syntax: `ford <HOST> <PORT|LOCAL:REMOTE>... [--jump HOST] [--name NAME] [--retry[=N]] [--foreground]`.
The explicit form is `ford open <HOST> ...`. The names `open`, `list`, `close`, `check`, `group`, and `logs` are
reserved.
For an SSH alias with a reserved name, use the explicit form, such as `ford open list 3000`.

- `HOST` accepts SSH configuration aliases, hostnames, IPv4 addresses, and `user@host`. Use an SSH configuration alias
  for IPv6 destinations in the first release to avoid introducing additional address syntax.
- `PORT` means `PORT:PORT`. The remote target address for every mapping is fixed to `127.0.0.1` on the remote machine.
- Ports must be decimal integers in `1..=65535`. Reject zero, missing fields, extra separators, and duplicate local
  ports. Different local ports may map to the same remote port.
- Do not bind and release a port in advance as an availability guarantee. The actual SSH bind determines the outcome.
  Report conflicts without silently choosing another port.
- The OS determines permission to bind low-numbered ports. ford does not elevate privileges automatically.
- Use SSH configuration for target login ports, keys, and complex routes. `--jump` (`-J`) selects one SSH jump host,
  accepting an alias, hostname, IPv4 address, or `user@host[:port]`. Use an SSH alias for IPv6 jump addresses.
  This option is passed as one `ssh -J` argument and overrides configured `ProxyJump`/`ProxyCommand` routes.
  Both the jump host and target require SSH access. The target remains the SSH destination, and port mappings still
  address its `127.0.0.1`. No remote shell commands, configuration edits, or agent forwarding are needed.
  ford does not pass through arbitrary SSH arguments.
- Run in the background by default and return a session ID. `--foreground` streams logs to the terminal. Both modes
  support list/close from another terminal.
- `--name` is optional; every session has a unique ID that is never reused. Restrict the name character set and never
  use names to construct paths. Reject an active name conflict for the current user instead of overwriting the session.
- `--retry` enables up to five SSH restarts; `--retry=N` selects 0 through 100. Absence or zero disables retrying.
  Browser opening, automatic port selection, and LAN binding options remain outside the current implementation.

Example background creation output:

```text
Session: 7f0a9c... (web)
Requested: 127.0.0.1:8080 -> devbox:127.0.0.1:300
State: running (SSH process alive; forwarding unverified)
```

The user can then try `http://127.0.0.1:8080` in a browser. ford does not detect HTTP versus HTTPS. Use `https://` for
an HTTPS service; certificate validation remains the browser's responsibility.

## 4. SSH Arguments and Configuration Boundaries

The intended arguments for `ford devbox 8080:300` are equivalent to:

```bash
ssh -N -T \
  -o BatchMode=yes \
  -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=15 \
  -o ServerAliveCountMax=3 \
  -o ConnectTimeout=10 \
  -o Compression=no \
  -o ControlMaster=no \
  -o ControlPath=none \
  -o ControlPersist=no \
  -o ForkAfterAuthentication=no \
  -o ForwardAgent=no \
  -o ForwardX11=no \
  -o PermitLocalCommand=no \
  -o RemoteCommand=none \
  -o ClearAllForwardings=no \
  -L 127.0.0.1:8080:127.0.0.1:300 \
  -- devbox
```

This shell representation is for review only. Execute an argument array without a shell. Pass the host as a separate
final argument. Reject hosts beginning with `-` or containing whitespace or control characters, and do not append a
remote command.

Read normal SSH configuration and preserve `HostName`, `User`, `Port`, `IdentityFile`, `ProxyJump`, `ProxyCommand`,
agent, and known_hosts behavior. SSH obtains values from command-line options, user configuration, and system
configuration in that order.[3]

ford explicitly overrides the options shown above to fix keepalive and compression settings, retain supervisor control
over the lifecycle, and disable agent/X11 forwarding and `LocalCommand`/`RemoteCommand`. SSH itself remains in the
foreground of the supervisor's execution model; the Rust supervisor handles background operation. Neither mode uses `ssh
-f`.

The first release uses `BatchMode=yes` in both modes, with SSH stdin redirected to the null device. It supports
configured keys and agent authentication without waiting for passwords, passphrases, or host key confirmation in the
background. Before first use, run a normal `ssh devbox` to establish trust and prepare the agent. Foreground mode
provides observation and control, not interactive authentication. Preserve host key verification; do not automatically
accept unknown or changed host keys.[3]

Disable connection sharing in the first release so the current supervisor owns the SSH connection and forwarding cannot
remain attached to another master after session closure. `ControlMaster=no` alone does not disable sharing, so also set
`ControlPath=none`.[3]

Configuration compatibility has a specific limitation: OpenSSH also reads existing `LocalForward`, `RemoteForward`, and
`DynamicForward` directives. They may add listeners or cause the entire command to fail on a conflict. The first release
does not isolate these directives; use an SSH alias without additional forwarding configuration.
`ClearAllForwardings=yes` would also remove forwarding requested on the command line, so it cannot selectively filter
inherited forwards.[3]

SSH configuration is trusted user configuration. Directives such as `ProxyCommand` and `Match exec` can still execute
configured programs. ford neither interpolates the supplied host into a command string nor modifies that configuration.

## 5. State, Failures, and Lifecycle

### Lifecycle States

```text
starting → running → closing → closed
              ↕          ↑
           retrying ──────┘
starting / running / retrying / closing → failed
```

- **State: `starting`**
  - Meaning supported by evidence: The supervisor is registered and is preparing or starting SSH
- **State: `running`**
  - Meaning supported by evidence: The supervisor owns an SSH child that has not exited. This state does not establish
    forwarding or target availability.
- **State: `retrying`**
  - Meaning supported by evidence: The previous SSH attempt was cleaned up and reaped; the same supervisor is waiting
    to start another attempt. There is no current SSH PID. Names and requested ports remain reserved in ford's index,
    although the OS listener is absent and another process could bind it.
- **State: `closing`**
  - Meaning supported by evidence: Shutdown has been accepted; child termination and reaping are in progress
- **State: `closed`**
  - Meaning supported by evidence: The user requested closure, the owned SSH child has exited and been reaped, and
    normal cleanup is complete
- **State: `failed`**
  - Meaning supported by evidence: Startup failed, retrying is disabled or exhausted, or cleanup failed; exit details
    and logs are retained
- **State: `unreachable`**
  - Meaning supported by evidence: The CLI could not contact the supervisor within the deadline; live state is unknown.
    This is an observation, not proof of process exit

An SSH exit without a user shutdown request is unexpected even if its exit code is 0. Store the original exit code or
signal separately. Do not mark a session `closed` before cleanup completes.

The state machine describes lifecycle, not service health. `forwarding` stays `unverified`. The `target` result is
`unverified` until an on-demand HTTP check records an observation. Do not infer readiness from SSH log messages or
from a process surviving a fixed sleep.

`ExitOnForwardFailure=yes` detects forwarding setup failures such as a local port conflict. It does not guarantee target
reachability or server permission for every subsequent forwarded connection. A successful local TCP handshake alone also
does not prove the remote web service is available.[3]

### On-demand HTTP checks

`ford check SESSION [--port LOCAL] --http PATH [--expect-status CODE]` performs one HTTP GET through a selected
mapping. The port is required for multiple mappings. Any complete HTTP response establishes HTTP reachability;
`--expect-status` additionally requires an exact status code. Missing responses are recorded as timeouts or
connection errors. Probe results do not change the lifecycle state or cause reconnection.

The CLI queries the live supervisor for the SSH PID and mapping. It uses `lsof` to confirm SSH owns the exact loopback
listener before and after the request. `curl` makes the HTTP request with a three-second transfer timeout, without
proxies, user curl configuration, or redirect following. These system tools supply listener ownership and HTTP handling
that the Rust standard library does not provide. They are needed only for `ford check` and add no work to idle sessions.

The CLI submits the observation through the existing private control socket, including the SSH PID and retry count
observed before the probe. The supervisor accepts it only for the same attempt and a matching mapping in a running
session, and persists the result atomically. The retry count rejects stale observations even if a PID is reused.
The probe itself runs outside the
supervisor, so a slow HTTP response does not delay status or close requests. The result and Unix timestamp are kept
per local port in `target_checks`; the request path and response body are not stored. Text output shows the result age.
Completed sessions retain check history. An unverified listener produces an error and leaves that history intact.

### Creation and Shutdown

Background creation first allocates an ID, reserves the name, records `starting`, and launches the supervisor. The
supervisor establishes its control socket, spawns SSH, updates state, and acknowledges through the startup pipe. The CLI
returns the ID after acknowledgment. Exit 0 means the session creation request was accepted, not that the tunnel is
ready. If SSH has already failed and no retry is scheduled, return a failure and the log location. With retrying
enabled, creation may acknowledge a session already waiting in `retrying`.

Startup exchanges have a 10-second timeout. On timeout, print the allocated session ID and direct the user to list
to verify the outcome rather than blindly creating another session. If the startup pipe disconnects before
acknowledgment, the supervisor
cancels the incomplete startup and cleans up; do not leave an unreported active session behind.

`ford close <ID|NAME>` sends a control request. The supervisor that owns the child performs shutdown:

1. Enter `closing` through serialized state handling and cancel pending retries. Merge concurrent close requests
   into the same shutdown operation. During backoff, close directly after confirming no child remains.
2. Send SIGTERM to the owned SSH process group and wait up to 5 seconds.
3. If it has not exited, send SIGKILL to the still-owned process group, then wait for and reap the direct SSH child.
4. Confirm cleanup, persist the terminal state, respond to requests, remove the control socket, and exit the supervisor.

Place SSH and its ordinary jump/proxy children in a dedicated process group. Never signal the user's shell or unrelated
SSH process groups. Signals must rely on ownership held by the live supervisor; do not signal a historical PGID after
reaping. If a custom `ProxyCommand` deliberately leaves the process group, the first release cannot guarantee control
over that external program's lifecycle.

Send a successful close response only after SSH has been reaped and required cleanup is complete. If the final wait also
times out, return nonzero and retain `closing` or failure details rather than reporting closure. If the client times
out, the supervisor continues shutdown and the client can query again.

Briefly retain the response channel after normal shutdown so accepted concurrent requests receive the result. Completed
sessions can return an idempotent "already closed" response based on a complete terminal snapshot. Resolve names to
active sessions first. If only historical records exist and the name is ambiguous, require an ID. A name may be reused
after the previous session is confirmed terminated; an `unreachable` session still reserves its name.

Foreground Ctrl+C and SIGTERM to either a foreground or background supervisor enter the same shutdown flow. Do not
perform complex I/O or exit directly inside a signal handler. Signal handling only notifies the normal control flow to
perform cleanup.

### Exit Contract and Failure Boundaries

- **Situation: Help, version, or list completes normally**
  - CLI behavior: Exit 0; display individual unreachable entries honestly
- **Situation: Invalid input**
  - CLI behavior: Exit 2 without creating a session
- **Situation: SSH not found / cannot execute**
  - CLI behavior: Creation exits 127 / 126 and retains a recognizable failure reason
- **Situation: Background creation accepted**
  - CLI behavior: Return the ID and exit 0; show later errors through list/logs
- **Situation: Foreground SSH exits unexpectedly**
  - CLI behavior: Apply the configured retry policy, preserving diagnostics. On exhaustion, return the last SSH exit
    code, or 1 for a signal or unexpected exit 0; user Ctrl+C exits 130 after cleanup
- **Situation: Close cannot find a session, lacks permission, times out over IPC, or fails to shut down**
  - CLI behavior: Return nonzero with a specific reason; unknown state is not successful closure

Keepalives use a 15-second interval and a maximum of 3 unanswered probes to detect an unresponsive SSH connection.
They do not establish HTTP health. Reconnection starts only after SSH exits.[3]

### Bounded Reconnection

Retrying is opt-in. `--retry` allows five replacement SSH processes, and `--retry=N` selects 0 through 100.
The budget covers the entire session lifetime; it does not reset when a child remains alive. Process liveness alone
cannot establish a successful SSH connection. Backoff waits are 1, 2, 4, 8, 16, then 30 seconds, capped at 30 seconds.
Use monotonic deadlines for scheduling and a Unix timestamp only for display.

An unexpected SSH exit, including exit 0 or 255, is eligible while budget remains. Do not classify network,
authentication, host key, or port bind errors by parsing stderr. All remain visible and consume the same finite
budget. No trust or authentication settings change between attempts. Spawn errors, log failures, or uncertain
cleanup stop the session without retrying.

Before scheduling another attempt, terminate remaining owned process-group helpers, reap the direct child, and drain
its logs. Keep the same session ID, supervisor, and socket. In `retrying`, clear `ssh_pid`, clear target observations,
reset `target` and `forwarding` to `unverified`, and retain the previous exit details and retry reason. Increment
`retry_count` when starting a replacement; set its PID and return to `running` without claiming connection readiness.
On exhaustion, persist `failed` with the final SSH exit and stop the supervisor.

Status and close remain available during backoff. Process close requests and termination signals before starting a
replacement. Explicit shutdown never triggers reconnection. Existing TCP and WebSocket connections break; only new
connections can use the replacement tunnel. Existing sessions must be restarted with a retry policy to enable it.

SIGKILL, a system crash, or an inability to write state can prevent cleanup, and SSH may remain running. List reports
`unreachable`; close returns an uncertainty error instead of forcibly killing a PID from disk. The first release does
not automatically adopt orphaned sessions. Report the session ID, last known PIDs, and log location for manual
inspection. Deleting a socket or metadata is not equivalent to stopping forwarding.

The target service may not be running, may live in another container network namespace, or may listen on a different
address. These are remote reachability issues. TCP tunneling also does not automatically fix absolute URLs, OAuth
callbacks, TLS certificates, or extra development-server ports.

## 6. List, Control Channel, and Local State

`ford list` enumerates the current user's session index and requests status from each active supervisor. By default,
show active and unreachable sessions; `--all` also includes completed sessions. Example:

```text
ID         NAME  HOST    MAPPINGS        STATE        TARGET
7f0a9c...  web   devbox  8080:300        running      unverified
ac13e2...  api   devbox  9000:9000       unreachable  unverified
```

IDs may be abbreviated for display. Commands accept unique prefixes and must reject ambiguous matches. Record the host,
mappings, name, creation/update times, supervisor PID, SSH PID, exit code/signal, error summary, and log path. Optional
`retry` holds the lifetime limit, `retry_count` counts replacements started, and `retry_at` gives the next attempt's
Unix timestamp. Default values are omitted from JSON. PIDs are
diagnostic information, not proof of control authority.

### Control Protocol

- Use one Unix socket per session through standard-library `UnixListener`/`UnixStream`.[4]
- The protocol supports `status`, `close`, and recording target checks, carrying a protocol version, full session ID,
  request ID, and result. Encode messages as JSON lines. Older sessions remain readable; restart them to record checks.
- Limit request sizes and set read/write timeouts. List uses bounded concurrency and an overall deadline so one stuck
  session cannot block other results.
- Restrict access to the current user: parent directories use mode 0700, sockets and metadata use 0600. Check directory
  ownership and reject unsafe paths or symbolic links. Do not expose the control protocol through a public or LAN TCP
  listener.
- The session ID returned by the socket must match the index. Reject incompatible protocols or identity mismatches;
  never fall back to control by bare PID.

### Storage and Concurrency

Use `$XDG_RUNTIME_DIR/ford` for runtime files when set; reject invalid ownership, permissions, or symbolic links.
When unset, use `~/.ford/run`. Store persistent state under `$XDG_STATE_HOME/ford`, falling back to
`~/.local/state/ford`
when unset. Validate Unix socket path lengths and report excessive lengths rather than silently falling back to an
unsafe shared directory.

Use a random session ID that is never reused and include a schema version in metadata. Update indexes and snapshots
through a temporary file in the same directory followed by atomic rename, so readers never see partial JSON. Use
short-lived file locks for name reservations and shared index changes; never wait for network operations or SSH
authentication while holding those locks.

The supervisor holds the session lock and writes its live and terminal state. The CLI must not infer live state from
modification times or PID liveness tests. Old unresponsive records can be identified as stale, but cannot be deleted and
have their names released without evidence. Recovery and cleanup of abnormal sessions require a later design.

`ford close --all` closes only active sessions enumerated when the command begins. Report each result separately and
return nonzero overall if any closure cannot be confirmed. Do not affect sessions created during execution or the user's
other SSH connections.

### Logs

Continuously drain SSH stderr into bounded session logs; in foreground mode, also stream it to the terminal. A full pipe
must not stall SSH. `ford logs <ID|NAME>` shows the latest 200 lines by default and works after the supervisor exits.
Cap each session log at 1 MiB with one rotated backup to prevent unlimited growth.
Append diagnostics across SSH attempts and include retry scheduling and start markers. Only append markers between
attempts after the old log writer has finished; never interleave independent rotating writers.

Do not log passwords, private key contents, the complete environment, or agent data. Preserve original SSH diagnostics.
Structured state supplements exit information; unreliable classification of stderr text must not replace the original
errors.

## 7. Saved Mapping Groups

`ford group save NAME SESSION...` snapshots selected configurations, including completed sessions. `--all` saves
only sessions confirmed running or retrying through live status queries. Existing groups require `--force` for
replacement.
`group list [--json]` and `group show NAME` inspect saved groups; `group remove NAME` deletes a group without closing
its sessions. Group names follow session-name validation.

Groups contain a schema version, name, save timestamp, and saved sessions. Each entry contains only its SSH host,
port mappings, optional session name, optional jump host, and optional retry limit. Process IDs, session IDs, retry
counters, logs, and health observations are excluded. Reopening uses the current SSH configuration and creates fresh
session IDs and retry budgets.

Store groups under the private state directory in `groups/NAME.json`. Use owner-only files, reject symlinks and unsafe
ownership or permissions, and publish updates by atomic rename. Limit group files to 64 KiB and each saved session
configuration to 32 KiB, leaving room for lifecycle metadata and protocol envelopes. Revalidate all fields on read.
Reject empty groups, duplicate session names, and overlapping local ports within a group.

`ford group open NAME` queries current sessions before launching anything. Matching running or retrying configurations
are reused; names, host, jump host, retry limits, and mappings must match. Known conflicting names or local ports,
and matching unreachable/closing sessions, cause an error before new sessions are created. SSH still determines actual
bind success.
New sessions launch independently; synchronous launch errors are reported with a nonzero aggregate exit code, and
successful launches remain running. SSH can fail after startup acknowledgment; report that through ordinary session
state and logs. This is not an atomic transaction or automatic orphan recovery.

`ford group close NAME` closes only current sessions whose configuration matches a saved entry, using the existing
supervisor control channel. Groups are reusable templates, so the same matching session can be represented in several
groups. Closing it affects that shared session. Removing or updating a group changes only its saved configuration.

Serialize operations on one group with a retained private lock file. Keep these locks separate from the session
registry lock, which is released before supervisor startup. Concurrent opens of the same group cannot duplicate
anonymous mappings; different groups can launch independently and ordinary SSH bind failures remain possible.

## 8. Future Releases

- **Stage: v0.1**
  - Scope: Spawn-based supervision, foreground/background modes, multiple ports, list, close, logs
  - Requirements: Implement truthful state, process ownership, concurrency, and cleanup together
- **Stage: v0.2**
  - Scope: On-demand HTTP target checks and explicit SSH jump hosts
  - Requirements: Keep health separate from liveness and retain final-target semantics
- **Stage: Next release**
  - Scope: Saved mapping groups and bounded `--retry`
  - Requirements: Preserve ownership, cancel retries promptly, and invalidate checks across attempts
- **Stage: Future options**
  - Scope: Optional `--open`, remote target address, local bind options, and periodic checks
  - Requirements: Define exposure boundaries and health policies before implementation
- **Stage: Later**
  - Scope: Abnormal session recovery and history cleanup
  - Requirements: Never treat metadata cleanup as process shutdown
- **Stage: As needed**
  - Scope: ford-owned SSH connection sharing, native Windows support
  - Requirements: Validate lifecycle and platform behavior independently

If connection sharing is introduced, use ford-owned control sockets and never terminate
masters shared by the user's other terminals.

## 9. Project Structure and Validation

Start implementation with one Cargo binary crate. Split into `cli`, `mapping`, `ssh`, `supervisor`, `control`, and
`state` modules as needed. Do not build a multi-crate workspace or generic backend abstraction in advance. Commit the
application's `Cargo.lock`; select, pin, and validate the toolchain and dependency versions when initializing code.

Required checks are `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`. Validate at two
levels:

1. Local automated tests: mapping syntax, dangerous host inputs, and SSH argument boundaries. Use a controlled SSH
   substitute to verify background startup acknowledgments, live list results, close by name/ID, idempotent and
   concurrent closure, timeout escalation, error exits, stale sockets, abnormal supervisor termination, log limits, and
   atomic metadata updates.
2. Isolated SSH integration tests: a temporary sshd and test service with test keys and an independent known_hosts file.
   Verify real byte round trips for single/multiple ports, an unavailable target, port conflicts, authentication
   failures, and forwarding permission restrictions. Check process reaping, port release, and ordinary jump-child
   cleanup after foreground interruption and background close. Do not bypass configuration by disabling host key
   verification.

Default tests must not require personal host aliases or public network access. Report Linux and macOS validation
separately. Static inspection or a test on one platform does not establish cross-platform success.

## 10. Performance Validation

Do not state timings, memory use, binary sizes, or throughput figures before measuring them. For the initial
implementation, record the platform, Rust/OpenSSH versions, build configuration, and test conditions. At minimum,
compare:

- Repeated cold/warm startup times for `ford --help` and release binary size.
- Time to background creation acknowledgment, first successful HTTP request, list completion, and close completion
  separately. Acceptance by a management process is not web service readiness.
- Throughput and request latency for direct `ssh` versus ford with identical options, including actual HTTP and
  WebSocket behavior.
- The full process tree, total RSS, idle CPU, and thread count during steady-state forwarding. Measure 1, 10, and 50
  sessions, including Rust supervisors and SSH/jump processes. Confirm no management process remains after the last
  session closes.

Establish a default release baseline before measuring the size and startup effects of LTO, stripping, or optimization
levels. Do not sacrifice authentication, trust verification, truthful state, or cleanup to improve performance figures.

## 11. Agreed Scope

Rust with system SSH and a retained spawn-based supervisor is agreed. List/close belong in the first release and must
not be removed to reduce initial scope or overhead.

The implementation uses one supervisor per session, background operation by default with `--foreground`
available, noninteractive authentication in both modes, Linux/macOS support only, and local bind and remote target
addresses initially fixed to `127.0.0.1`. The running state means only that the SSH process is alive; an on-demand
check records HTTP reachability separately.

## References

1. [Rust Child](https://doc.rust-lang.org/std/process/struct.Child.html): spawned child process management, exit, and
   reaping responsibilities.
2. [OpenSSH ssh(1)](https://man.openbsd.org/ssh): `-N`, `-T`, `-L`, and multiple port forwards.
3. [OpenSSH ssh_config(5)](https://man.openbsd.org/ssh_config): BatchMode, configuration precedence, keepalives,
   forwarding failures, and connection sharing.
4. [Rust UnixStream](https://doc.rust-lang.org/std/os/unix/net/struct.UnixStream.html): the local Unix socket control
   channel.

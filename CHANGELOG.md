# Changelog

Notable changes in each version of ford.

## [Unreleased]

### Changed

- Keep automatic reconnection alive through longer outages: `--retry` now allows ten restarts, with three quick
  attempts followed by increasing waits capped at five minutes. The default waits total about 19 minutes.
- Keep explicit `--retry=N` limits and saved group limits while applying the longer retry schedule to new sessions.

## [0.4.3]

### Fixed

- Omit `ForkAfterAuthentication` completely for SSH clients that do not advertise support, including during retries.
- Keep one resolved SSH executable path across reconnects and log the supervisor version and arguments for each attempt.
- Reject an unmapped HTTP check port before contacting the supervisor, keeping invalid-input errors consistent.

## [0.4.2]

### Fixed

- Start tunnels with OpenSSH clients older than 8.7 while still disabling configured SSH backgrounding on newer clients.
## [0.4.1]

### Added

- Update to the latest stable release in place with `ford update`, or check availability with `ford update --check`.
- Verify update checksums and binary versions before replacement, preserving custom paths and shell configuration.
- Proxy selected outbound requests through an SSH host with `--socks PORT`.
- Expose a SOCKS proxy on an SSH host with `--reverse-socks PORT`, using the machine running ford as the exit.
- Save and restore SOCKS ports and directions in groups, with separate local and remote port conflict checks.
- Manage proxy sessions with the existing list, close, logs, jump hosts, and bounded reconnection options.
- Publish static musl binaries for Linux x86_64 and ARM64 alongside the GNU and macOS archives.

### Fixed

- Prefer musl automatically in the Linux installer and self-updater, avoiding newer glibc requirements on older systems.
- Keep atomically replaced state snapshots readable during concurrent list requests without relaxing write protections.

### Upgrade Notes

- Rerun the install command to upgrade from older releases that do not have `ford update`.
- The installer falls back to GNU only when an older release has no musl archive. Use `--libc musl` to require musl,
  or `--libc gnu` to select GNU explicitly.
- System OpenSSH is still required. Static linking applies to ford itself.

## [0.3.0]

### Added

- Save named groups of session mappings and reopen them with one command, including jump hosts.
- List, inspect, update, close, and remove groups without replacing unrelated or unreachable sessions.
- Reconnect SSH sessions with opt-in `--retry[=N]`, bounded exponential backoff, and cancellable retry waits.
- Keep retry policies in saved groups and invalidate target checks when replacing an SSH attempt.

### Upgrade Notes

- Retry support is opt-in. Restart existing sessions with `--retry` or `--retry=N` to enable it.
- Restart sessions created by older versions before using `ford check` with the updated CLI.
- Reconnection restores new connections; existing TCP and WebSocket connections must reopen.

## [0.2.0]

### Added

- Check an HTTP path through a tunnel with `ford check` and show the latest result per mapping in `ford list`.
- Reach a target SSH server through an explicit jump host with `--jump` (`-J`), including a custom jump login port.

### Changed

- Show a progress bar while the installer downloads a release archive.

### Upgrade Notes

- HTTP target checks require `curl` and `lsof` on PATH.
- Restart sessions created by v0.1.0 before using `ford check`. Target checks are on demand.

## [0.1.0]

### Added

- Forward remote TCP services to local ports through system OpenSSH, including different local and remote ports.
- Share one SSH connection across multiple port mappings.
- Create named background sessions or run a supervisor in the foreground.
- Query live sessions with `ford list`, including structured JSON output and completed-session history.
- Close sessions by name, ID, unique ID prefix, or `--all`, with confirmed SSH cleanup and forced termination if needed.
- Inspect bounded, rotating SSH diagnostics with `ford logs`.
- Reuse SSH aliases, configured identities, agents, login ports, and jump hosts.
- Report unreachable supervisors without killing processes from stale PID records.
- Build native Linux and macOS release archives for x86_64 and ARM64, with SHA256 checksums.
- Install release binaries with platform detection, SHA256 verification, and automatic shell PATH setup.

### Requirements and Limits

- System OpenSSH with configured key or agent authentication and an established host key.
- Local listeners and remote target addresses are fixed to `127.0.0.1`.
- `running` reports SSH process liveness; target availability remains `unverified`.
- Automatic reconnection, orphan adoption, and native Windows support are not included.

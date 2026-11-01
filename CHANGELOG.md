# Changelog

Notable changes in each version of ford.

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

# ford: Critical Engineering Rules

ford is a lightweight Rust CLI that forwards remote TCP services to the local machine through system OpenSSH. See
[docs/design.md](docs/design.md) for the design.

## Current Stage

- The user has reviewed the design and authorized Git initialization and implementation. Continue within that scope.
- Routine implementation, fixes, and validation do not require additional permission.
- Do not describe planned features as implemented. Update the design when implementation changes command semantics or
  architecture.

## Critical Rules

1. **Rust manages sessions; OpenSSH carries traffic.** Do not implement SSH, encryption, or an HTTP proxy. Do not add a
   Rust forwarding layer to the data path.
2. **Keep it lightweight.** Use the synchronous standard library and a few dependencies with concrete purposes. Do not
   introduce an async runtime, a global persistent daemon, a database, a TUI, or a plugin framework in advance. Each
   active session may have one Rust supervisor process; retain no supervisor processes when no sessions are active.
   Explain why each new dependency is needed and why the standard library is insufficient.
3. **The supervisor owns the lifecycle.** The first release uses `Command::spawn` and retains a Rust supervisor for
   state, control requests, signals, and SSH termination and reaping. Do not replace ford with SSH through exec. Disable
   implicit SSH backgrounding and shared connections. Explicitly terminate and reap owned child processes; dropping a
   child handle is not cleanup.
4. **Pass structured arguments.** Use `Command` argument arrays. Do not use `sh -c`, assemble shell command strings, or
   use `eval`. Validate hosts and ports so a host cannot be interpreted as an SSH option.
5. **Make network exposure explicit.** Listeners created by ford bind to `127.0.0.1` by default. Any future LAN access
   must require an explicit user option; never silently bind all interfaces.
6. **Preserve SSH identity and trust handling.** Use existing SSH configuration, agents, and known_hosts. Do not store
   passwords, private key contents, or passphrases; automatically disable host key verification; automatically enable
   agent forwarding; or modify the user's SSH configuration.
7. **Report only verified states.** Process startup, listener establishment, target TCP reachability, and HTTP
   availability are different facts. Do not print success messages such as "connected" or "web service ready" without
   corresponding evidence.
8. **Keep failures visible.** Preserve SSH diagnostics and exit semantics. Do not hide port conflicts, authentication
   failures, or host key changes. Do not mask errors by silently changing ports or retrying indefinitely.
9. **Measure performance.** Separate CLI startup, SSH handshakes, steady-state forwarding throughput, and memory use.
   Compare identical SSH options, build configurations, and network conditions. Do not promise specific timings, sizes,
   or performance advantages without measurements.
10. **Keep implementation direct.** Prefer concrete types and small functions. Do not introduce trait layers for
    nonexistent backends. Use safe Rust by default; any new `unsafe` requires a concrete justification and documented
    safety conditions.
11. **Keep state and control trustworthy.** `ford list` queries live supervisor state; persisted files serve only as
    indexes or history. Mark an unreachable supervisor as `unreachable`, not closed. `ford close` requests shutdown
    through a private Unix socket; never kill a process based solely on a PID from a state file.
12. **Include concurrency and cleanup in the first release.** Never reuse session IDs. Define behavior for name
    conflicts, concurrent creation and closure, stale sockets, atomic state updates, and directory permissions.
    Successful shutdown requires confirmed SSH exit and reaping; report timeouts or uncertainty honestly.

## Implementation and Validation

- Use English for documentation, development notes, code comments, CLI help, and error messages.
- Keep Markdown source lines at 120 characters or fewer; reformat wide tables when necessary.
- Use Conventional Commit subjects so commit-based release notes remain useful.
- Keep release tags, Cargo.toml, Cargo.lock, and CHANGELOG.md consistent. Release procedures are in docs/releasing.md.
- For release tooling changes, run `python3 -m unittest discover -s scripts -p 'test_release.py' -v`, lint workflows,
  and validate a native package when packaging behavior changes.
- Commit `Cargo.lock` when the application project is created. Select dependency and toolchain versions based on actual
  validation.
- For code changes, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
- Test observable behavior: argument parsing, SSH argument boundaries, failure exits, port conflicts, real byte
  forwarding, list/close, concurrency, and supervisor failures. Verify that ports are released after shutdown. Avoid
  tests that merely repeat implementation details.
- Default tests must not depend on personal SSH aliases, existing credentials, or public network services. Use isolated
  configuration for real SSH integration tests and record which platforms were actually tested.
- Do not build a test framework for documentation-only changes. Report changed files, checks actually performed, and
  unverified limitations.

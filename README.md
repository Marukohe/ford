# ford

Access remote web apps, dashboards, and APIs from your own machine through SSH.

ford brings a remote port to `localhost` and keeps the tunnel running in the background. Give each tunnel a name,
list your sessions, and close them when you are done.

```bash
ford user@server 8080:3000 --name web
# Open http://127.0.0.1:8080 in your browser

ford list
ford close web
```

Your server only needs an SSH port accessible to you, usually port 22. The web app can stay on the server's
`127.0.0.1:3000`; you do not need to expose that port publicly or install ford on the server.

## Install

ford supports Linux and macOS on Intel/AMD x86_64 and ARM64, including Apple Silicon. You need an OpenSSH client
(`ssh`) on your machine and an SSH server that allows TCP forwarding on the remote machine.

### Quick install

In Bash or zsh, run:

```bash
curl -fsSL https://raw.githubusercontent.com/Marukohe/ford/main/scripts/install.sh | sh
```

The installer downloads the latest stable release for your machine, verifies its SHA256 checksum, and installs
`ford` in `~/.local/bin`. It configures PATH for future Bash or zsh terminals. Open a new terminal or run the command
printed by the installer to activate PATH in your current terminal. Rust and sudo are not required. Run the same
installation command again to upgrade.

Quick installation requires a published GitHub Release. If no release is available yet, use the source installation
below.

### Download manually

Download the archive matching your machine from [GitHub Releases](https://github.com/Marukohe/ford/releases).
Choose the compiled archive rather than GitHub's source-code downloads:

| Machine | Archive filename ends with |
| --- | --- |
| Linux Intel/AMD x86_64 | `x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu.tar.gz` |
| macOS Intel | `x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin.tar.gz` |

For example, after downloading the Linux x86_64 archive and `SHA256SUMS` from the same release:

```bash
grep '  ford-v0.2.0-x86_64-unknown-linux-gnu.tar.gz$' SHA256SUMS | sha256sum --check
tar -xzf ford-v0.2.0-x86_64-unknown-linux-gnu.tar.gz ford
mkdir -p "$HOME/.local/bin"
install -m 755 ford "$HOME/.local/bin/ford"
export PATH="$HOME/.local/bin:$PATH"
ford --version
```

On macOS, use `shasum -a 256 --check` for checksum verification and substitute the appropriate archive filename.
For manual installation, also add the PATH export to `~/.zshrc` or `~/.bashrc` to keep it in future terminals.
Linux archives require glibc: x86_64 builds use Ubuntu 22.04 and ARM64 builds use Ubuntu 24.04.

### Build from source

Install Git and Rust through rustup, then run:

```bash
git clone https://github.com/Marukohe/ford.git
cd ford
cargo install --path . --locked
ford --version
```

The repository selects Rust 1.93.1 automatically. Cargo installs `ford` in `~/.cargo/bin`; make sure that directory is
on your `PATH`.

## Your first tunnel

First, check that you can log in to the server:

```bash
ssh user@server
```

Confirm its host key and set up an SSH key or agent before using ford. ford uses your existing SSH configuration and
does not prompt for passwords or key passphrases. Exit the remote shell to return to your local terminal.

Suppose a web app runs on port 3000 on that server:

```bash
ford user@server 8080:3000 --name web
```

Open `http://127.0.0.1:8080` in your local browser. The tunnel runs in the background, so you can keep using your
terminal. When you are finished:

```bash
ford close web
```

Local ports bind to `127.0.0.1`, so other machines on your network cannot use your tunnel.

## Choose your ports

Use `LOCAL:REMOTE` to choose a local port. A single port number uses the same port on both machines.

| Command | Local address | Remote service |
| --- | --- | --- |
| `ford devbox 3000` | `127.0.0.1:3000` | `devbox` at `127.0.0.1:3000` |
| `ford devbox 8080:3000` | `127.0.0.1:8080` | `devbox` at `127.0.0.1:3000` |
| `ford devbox 8080:300` | `127.0.0.1:8080` | `devbox` at `127.0.0.1:300` |

Forward several services through one session:

```bash
ford devbox 3000 5173 8080:8000 --name dev
```

If a local port is already in use, choose another one, such as `8081:3000`.

## Reach a server through a jump host

When the target server is reachable only through another SSH machine, use `--jump` (or `-J`):

```bash
ford internal-server 8080:3000 --jump bastion --name web
```

Open `http://127.0.0.1:8080` locally to reach port 3000 on `internal-server`. ford connects through `bastion` and
logs into `internal-server`; both machines need SSH access. The jump host must be able to reach the target's SSH port.
Your local SSH keys or agent authenticate both connections.

You can use an SSH alias for the jump host or specify its username and SSH port:

```bash
ford alice@internal.example 8080:3000 -J bob@bastion.example:2222 --name web
```

`ford list` shows the route, and the session uses the same `check`, `logs`, and `close` commands. Use SSH configuration
for each machine's keys and target login port. Existing `ProxyJump` settings still work; an explicit `--jump` overrides
them. Each `--jump` accepts one host; use an SSH alias for an IPv6 jump address.

## Manage your tunnels

```bash
ford list                       # List current sessions
ford list --all                 # Also show completed sessions
ford close web                  # Close a named session
ford close --all                 # Close all current sessions
ford logs web                   # Read recent SSH diagnostics
ford logs web --lines 100        # Show the last 100 lines
ford list --json                 # Get session details for scripts
```

You can use a session's full ID or a unique ID prefix in place of its name. Completed sessions retain their logs,
and you can reuse a name after its session has finished.

For a temporary tunnel that shows SSH diagnostics in your terminal, use foreground mode:

```bash
ford devbox 3000 --foreground
```

Press Ctrl+C to close it.

## Automatic reconnection

Enable reconnection when you create a session:

```bash
ford devbox 8080:3000 --name web --retry
ford internal 9000:8000 --jump bastion --name api --retry=10
```

`--retry` allows five SSH restarts over the session's lifetime. Use `--retry=N` to choose a limit from 0 to 100;
0 disables retries. Retrying is off by default. Waits increase from 1 to 2, 4, 8, 16, then 30 seconds and stay capped
at 30 seconds. Starting a new SSH process does not reset the budget.

`ford list` shows `retrying` while waiting and `running` when a replacement SSH process is alive. The session keeps
its name, ID, and mappings. `ford logs web` retains SSH errors and shows retry attempts. `ford list --json` includes
the retry limit, attempts used, and the next retry's Unix timestamp when applicable; zero counts are omitted.
`ford close web` cancels pending retries;
Ctrl+C does the same in foreground mode.

Reconnection starts after SSH exits. Keepalives already detect an unresponsive SSH connection using a 15-second
interval and three unanswered probes; there is no fixed session duration. A failed HTTP check alone does not restart
SSH. After reconnection, existing TCP or WebSocket connections need to be reopened, and `TARGET` returns to
`unverified` until you run another check.

Retries also cover authentication, host key, and port bind failures: SSH exit codes cannot reliably distinguish them
from network failures. Trust checks stay enabled, diagnostics remain available, and the session becomes `failed`
when the budget is exhausted. A missing SSH executable, log failure, or uncertain process cleanup stops the session
without retrying. Reconnection requires a live supervisor; it does not resume sessions after a reboot or supervisor
failure. Restart existing sessions with `--retry` to enable it.

## Save and reopen mapping groups

Save selected sessions together so you can reopen their mappings with one command:

```bash
ford devbox 8080:3000 --name web
ford internal 9000:8000 --jump bastion --name api
ford group save work web api

ford group close work
ford group open work
```

A group keeps the hosts, port mappings, session names, jump hosts, and retry limits. `group open` creates new background
sessions with fresh retry budgets and skips matching sessions that are already running or retrying. Other sessions
are left alone. Known name or port conflicts
must be resolved first; an unreachable session is never replaced automatically.

```bash
ford group save work --all --force   # Replace the group with all running or retrying sessions
ford group list                     # List saved groups
ford group show work                # Inspect saved routes and ports
ford group remove work              # Delete the saved group; keep its running sessions
```

You can save completed sessions by name or ID, and use IDs for unnamed sessions. Groups survive session closure and
ford upgrades. They are stored privately under `$XDG_STATE_HOME/ford/groups`, or `~/.local/state/ford/groups`.
Reopening uses your current SSH configuration. Health results are checked again only when you run `ford check`.

Sessions open independently: if a launch fails, successful launches stay running and the command reports the failure.
Creation acknowledgment confirms SSH process startup; check the service separately for readiness.

## Check a web app

Run an HTTP check through an existing tunnel to update its `TARGET` result:

```bash
ford check web --port 8080 --http /
ford check web --port 8080 --http / --expect-status 200
ford list
```

Use any read-only path your app already serves. Without `--expect-status`, an HTTP response such as 401 or 404
reports `reachable` and shows the status code. With an expected status, ford reports `ok` only when it matches.
Timeouts and connection errors are also recorded. The command exits with a nonzero status on failure.

`TARGET` shows the last result and its age for each checked port. It stays `unverified` until you run a check.
You can omit `--port` for a session with one mapping. A failed check does not close the tunnel.

Checks support plain HTTP and require `curl` and `lsof` on PATH. Restart sessions created by older versions of ford
before checking them.

## Use your SSH configuration

ford accepts SSH aliases, so you can keep usernames, custom SSH ports, keys, and jump hosts in `~/.ssh/config`.
For example:

```sshconfig
Host devbox
    HostName server.example.com
    User alice
    Port 2222
    IdentityFile ~/.ssh/id_ed25519
```

Then run `ford devbox 8080:3000 --name web`.

Existing port forwards in your SSH configuration also apply. Use an alias without extra forwards if you only want
the ports specified in your ford command.

## Troubleshooting

**The session says `running`, but the page does not load.** `running` means the SSH process is alive. Use
`ford check web --http /` for a single HTTP mapping. Check the app on the server, confirm the port mapping, and run
`ford logs web` for SSH errors.

**Authentication or host key verification fails.** Try `ssh devbox` and resolve the SSH error there. ford needs key
or agent authentication that works without a password or passphrase prompt.

**A session failed or disappeared from the list.** Run `ford list --all` and `ford logs web`. Logs remain available
after a session ends. Use `--retry` when creating a session to recover from temporary connection loss. If retries
are disabled or exhausted, start a new session after resolving the problem.

**A session says `unreachable`.** ford cannot contact the process managing that tunnel, so it cannot confirm whether
the tunnel is still active or close it automatically. Check your local processes before starting a replacement on
the same local port.

## More information

- [Changelog](CHANGELOG.md)
- [Releases and release notes](https://github.com/Marukohe/ford/releases)
- [Design and behavior](docs/design.md)
- [Validation results](docs/validation.md)
- [Release maintenance](docs/releasing.md)

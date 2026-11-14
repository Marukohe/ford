#!/usr/bin/env python3
"""Run isolated loopback SSH integration checks, optionally measuring session overhead."""

import argparse
from contextlib import ExitStack
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import pwd
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import urlopen


PAYLOAD = b"ford SSH integration\n"
BULK = bytes(range(256)) * 4096


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for(check, description, seconds=8):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (OSError, http.client.HTTPException):
            pass
        time.sleep(0.02)
    raise AssertionError(f"timed out: {description}")


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.path == "/health":
            self.send_error(404)
            return
        if self.path == "/slow":
            release = self.server.slow_release
            self.server.slow_started.set()
            release.wait(10)
            return
        if self.path == "/ws":
            accept = base64.b64encode(hashlib.sha1(
                (self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
            ).digest()).decode()
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
            header = self.rfile.read(2)
            if not header:
                return
            mask = self.rfile.read(4)
            data = self.rfile.read(header[1] & 0x7f)
            data = bytes(byte ^ mask[index % 4] for index, byte in enumerate(data))
            self.wfile.write(bytes([0x81, len(data)]) + data)
            self.wfile.flush()
            return
        data = BULK if self.path == "/bulk" else PAYLOAD
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def fetch(port, path="/"):
    with urlopen(f"http://127.0.0.1:{port}{path}", timeout=1) as response:
        return response.read()


def websocket(port):
    with socket.create_connection(("127.0.0.1", port), timeout=2) as sock:
        key = base64.b64encode(b"ford-test-key-123").decode()
        sock.sendall((f"GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
                      f"Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
                      f"Sec-WebSocket-Key: {key}\r\n\r\n").encode())
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            chunk = sock.recv(1)
            assert chunk, "EOF during WebSocket upgrade"
            header += chunk
        assert b" 101 " in header, header
        payload = b"websocket over SSH"
        mask = b"ford"
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        sock.sendall(bytes([0x81, 0x80 | len(payload)]) + mask + masked)
        received = b""
        while len(received) < 2 + len(payload):
            chunk = sock.recv(2 + len(payload) - len(received))
            assert chunk, "EOF during WebSocket frame"
            received += chunk
        assert received == bytes([0x81, len(payload)]) + payload


def released(port):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.2):
            return False
    except ConnectionRefusedError:
        return True


def alive(pid):
    status = Path(f"/proc/{pid}/stat")
    if status.exists():
        return ") Z " not in status.read_text()
    return False


def stop_process(process):
    if process.poll() is None:
        process.terminate()
        process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ford"))
    parser.add_argument("--bench", action="store_true", help="measure 1, 10, and 50 real SSH sessions on Linux")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), f"build ford first: {binary}"
    ssh = shutil.which("ssh")
    sshd = shutil.which("sshd") or "/usr/sbin/sshd"
    assert ssh and Path(sshd).is_file(), "OpenSSH client, ssh-keygen, and sshd are required"
    assert shutil.which("curl") and shutil.which("lsof"), "curl and lsof are required for target checks"
    report = {"checks": [], "benchmarks": [], "platform": os.uname().sysname,
              "ssh": subprocess.run([ssh, "-V"], capture_output=True, text=True).stderr.strip()}
    daemons = []
    daemon_logs = []
    foreground = None
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.slow_started = threading.Event()
    server.slow_release = threading.Event()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    with tempfile.TemporaryDirectory(prefix=".ford-ssh-", dir=Path.home()) as tmp, ExitStack() as cleanup:
        root = Path(tmp)
        for name in ["home", "run", "state", "bin"]:
            (root / name).mkdir(mode=0o700)
        for name in ["server", "client", "wrong"]:
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True)
        (root / "authorized_keys").write_text((root / "client.pub").read_text())
        (root / "authorized_keys").chmod(0o600)
        ports = [free_port(), free_port(), free_port()]
        for index, port in enumerate(ports):
            config = root / f"sshd-{index}.conf"
            config.write_text(f"""Port {port}
ListenAddress 127.0.0.1
HostKey {root / 'server'}
PidFile {root / f'sshd-{index}.pid'}
AuthorizedKeysFile {root / 'authorized_keys'}
StrictModes yes
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
AllowTcpForwarding {'no' if index == 1 else 'yes'}
MaxStartups 100
LogLevel VERBOSE
""")
            subprocess.run([sshd, "-t", "-f", str(config)], check=True)
            log = (root / f"sshd-{index}.log").open("w+")
            cleanup.enter_context(log)
            daemon_logs.append(log)
            daemons.append(subprocess.Popen([sshd, "-D", "-e", "-f", str(config)], stdout=log, stderr=log))
            cleanup.callback(stop_process, daemons[-1])
        host_key = " ".join((root / "server.pub").read_text().split()[:2])
        (root / "known_hosts").write_text("".join(f"[127.0.0.1]:{port} {host_key}\n" for port in ports))
        wrong_key = " ".join((root / "wrong.pub").read_text().split()[:2])
        (root / "wrong_hosts").write_text(f"[127.0.0.1]:{ports[0]} {wrong_key}\n")
        config = root / "ssh.conf"
        config.write_text(f"""Host denied
    Port {ports[1]}
Host noauth
    IdentityFile {root / 'wrong'}
Host badkey
    UserKnownHostsFile {root / 'wrong_hosts'}
Host 127.0.0.1
    Port {ports[1]}
Host internal jumped
    Port {ports[2]}
Host internal
    ProxyCommand false
Host jumped
    ProxyJump good
Host good denied badkey jumped internal 127.0.0.1
    IdentityFile {root / 'client'}
Host *
    HostName 127.0.0.1
    User {pwd.getpwuid(os.getuid()).pw_name}
    Port {ports[0]}
    IdentitiesOnly yes
    BatchMode yes
    UserKnownHostsFile {root / 'known_hosts'}
    StrictHostKeyChecking yes
""")
        wrapper = root / "bin/ssh"
        wrapper.write_text(
            '#!/bin/sh\nif [ "${FORD_TEST_NO_LISTENER:-}" = 1 ]; then exec sleep 300; fi\n'
            f"exec {shlex.quote(ssh)} -F {shlex.quote(str(config))} \"$@\"\n"
        )
        wrapper.chmod(0o700)
        env = dict(os.environ, HOME=str(root / "home"), XDG_RUNTIME_DIR=str(root / "run"),
                   XDG_STATE_HOME=str(root / "state"), PATH=f"{root / 'bin'}:{os.environ['PATH']}", LC_ALL="C")

        def ford(*items, check=True):
            return subprocess.run([str(binary), *map(str, items)], env=env, capture_output=True,
                                  text=True, check=check, timeout=20)

        def sessions():
            return json.loads(ford("list", "--all", "--json").stdout)

        def state(name):
            return next(item for item in sessions() if item["name"] == name)

        def failed(name):
            return state(name)["state"] == "failed"

        def check(name):
            report["checks"].append(name)
            print(f"PASS {name}", flush=True)

        try:
            for port in ports:
                wait_for(lambda: socket.create_connection(("127.0.0.1", port), timeout=0.2).close() is None,
                         "isolated sshd startup")
            target = server.server_port
            local, second = free_port(), free_port()
            ford("good", f"{local}:{target}", f"{second}:{target}", "--name", "web")
            wait_for(lambda: fetch(local) == PAYLOAD and fetch(second) == PAYLOAD, "real multi-port HTTP forwarding")
            websocket(local)
            check("multi-port HTTP and WebSocket byte forwarding")
            web = state("web")
            assert web["state"] == "running" and web["target"] == "unverified"
            assert ford("check", "web", "--http", "/", check=False).returncode == 2
            assert ford("check", "web", "--port", local, "--http", "http://127.0.0.1/", check=False).returncode == 2
            assert "reachable (HTTP 404)" in ford("check", "web", "--port", local, "--http", "/health").stdout
            result = ford("check", "web", "--port", local, "--http", "/health", "--expect-status", 200, check=False)
            assert result.returncode == 1, result
            assert state("web")["target_checks"][str(local)]["result"] == "unexpected_status"
            assert "ok (HTTP 200)" in ford("check", "web", "--port", second, "--http", "/", "--expect-status", 200).stdout
            checks = state("web")["target_checks"]
            assert checks[str(local)]["http_status"] == 404 and checks[str(second)]["http_status"] == 200
            assert "checked" in ford("list").stdout
            check("per-mapping HTTP results, missing health endpoint, and explicit expected status")

            result = ford("check", "web", "--port", second, "--http", "/slow", check=False)
            assert result.returncode == 1 and "timeout" in result.stdout, result
            checks = state("web")["target_checks"]
            assert checks[str(second)]["result"] == "timeout"
            server.slow_release.set()
            server.slow_release = threading.Event()
            server.slow_started.clear()
            check("a stalled HTTP endpoint records a timeout")

            slow_check = subprocess.Popen(
                [str(binary), "check", "web", "--port", str(second), "--http", "/slow"],
                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            try:
                assert server.slow_started.wait(3), "slow target did not receive the check"
                assert state("web")["state"] == "running" and slow_check.poll() is None
                ford("close", web["id"][:12])
                stdout, stderr = slow_check.communicate(timeout=10)
                assert slow_check.returncode == 1 and not stdout, (stdout, stderr)
                assert state("web")["target_checks"] == checks, "a check in flight overwrote history after close"
            finally:
                server.slow_release.set()
                stop_process(slow_check)
            check("list and close work during a pending target check")
            wait_for(lambda: released(local) and released(second), "ports released after close")
            assert state("web")["state"] == "closed"
            if os.uname().sysname == "Linux":
                wait_for(lambda: not alive(web["supervisor_pid"]) and not alive(web["ssh_pid"]), "process cleanup")
            check("live list, close by ID prefix, process cleanup, and port release")

            local = free_port()
            with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as app:
                app_thread = threading.Thread(target=app.serve_forever, daemon=True)
                app_thread.start()
                try:
                    ford("good", f"{local}:{app.server_port}", "--name", "stoppable")
                    wait_for(lambda: fetch(local) == PAYLOAD, "stoppable target readiness")
                    ford("check", "stoppable", "--http", "/", "--expect-status", 200)
                    assert state("stoppable")["target"] == "ok"
                finally:
                    app.shutdown()
                    app_thread.join()
            result = ford("check", "stoppable", "--http", "/", check=False)
            assert result.returncode == 1 and "connection error" in result.stdout, result
            stopped = state("stoppable")
            assert stopped["state"] == "running" and stopped["target"] == "connection_error"
            ford("close", "stoppable")
            check("stopping the remote app changes TARGET while SSH stays running")

            # A live SSH substitute without a listener must not get credit for another service.
            env["FORD_TEST_NO_LISTENER"] = "1"
            try:
                ford("good", f"{target}:{target}", "--name", "wrong-listener")
            finally:
                del env["FORD_TEST_NO_LISTENER"]
            assert fetch(target) == PAYLOAD
            result = ford("check", "wrong-listener", "--http", "/", check=False)
            assert result.returncode == 1 and "owns listener" in result.stderr, result
            assert state("wrong-listener")["target"] == "unverified"
            ford("close", "wrong-listener")
            check("an unrelated listener cannot verify a target")

            local = free_port()
            ford("jumped", f"{local}:{target}", "--name", "jump")
            wait_for(lambda: fetch(local) == PAYLOAD, "ProxyJump forwarding")
            jump = state("jump")
            children_file = Path(f"/proc/{jump['ssh_pid']}/task/{jump['ssh_pid']}/children")
            helpers = list(map(int, children_file.read_text().split())) if children_file.exists() else []
            ford("close", "jump")
            wait_for(lambda: released(local), "jump port release")
            if helpers:
                wait_for(lambda: all(not alive(pid) for pid in helpers), "jump helper cleanup")
            check("ProxyJump forwarding and helper cleanup")

            # internal has no usable direct route in the fixture. -J must override ProxyCommand.
            local = free_port()
            ford("internal", f"{local}:{target}", "--name", "no-jump", check=False)
            wait_for(lambda: failed("no-jump"), "target requires a jump host")
            for name, jump_option, jump_host in [
                ("jump-alias", "--jump", "good"),
                ("jump-port", "-J", f"{pwd.getpwuid(os.getuid()).pw_name}@127.0.0.1:{ports[0]}"),
            ]:
                local = free_port()
                ford("internal", f"{local}:{target}", jump_option, jump_host, "--name", name)
                wait_for(lambda: fetch(local) == PAYLOAD, "explicit jump-host forwarding")
                jumped = state(name)
                assert jumped["jump"] == jump_host and jumped["host"] == "internal"
                assert "ok (HTTP 200)" in ford("check", name, "--http", "/",
                                               "--expect-status", 200).stdout
                children_file = Path(f"/proc/{jumped['ssh_pid']}/task/{jumped['ssh_pid']}/children")
                helpers = list(map(int, children_file.read_text().split())) if children_file.exists() else []
                if children_file.exists():
                    assert helpers, "expected an active SSH jump helper"
                ford("close", name)
                wait_for(lambda: released(local), "explicit jump port release")
                if helpers:
                    wait_for(lambda: all(not alive(pid) for pid in helpers), "explicit jump helper cleanup")
            check("explicit jump alias and user@host:port reach a separate SSH server and clean up")

            ford("internal", f"{free_port()}:{target}", "--jump", "noauth",
                 "--name", "jump-noauth", check=False)
            wait_for(lambda: failed("jump-noauth"), "jump authentication failure")
            assert ford("logs", "jump-noauth").stdout.strip()
            check("jump authentication failure retains SSH diagnostics")

            group_local, group_remote = free_port(), free_port()
            ford("good", f"{group_local}:{target}", "--name", "group-web")
            ford("internal", f"{group_remote}:{target}", "--jump", "good", "--name", "group-api")
            wait_for(lambda: fetch(group_local) == PAYLOAD and fetch(group_remote) == PAYLOAD,
                     "group source forwarding")
            ford("check", "group-web", "--http", "/", "--expect-status", 200)
            before = {item["id"] for item in sessions()}
            ford("group", "save", "work", "group-web", "group-api")
            ford("group", "close", "work")
            wait_for(lambda: released(group_local) and released(group_remote), "group port release")
            ford("group", "open", "work")
            wait_for(lambda: fetch(group_local) == PAYLOAD and fetch(group_remote) == PAYLOAD,
                     "restored group forwarding")
            restored = {item["name"]: item for item in sessions()
                        if item["name"] in ["group-web", "group-api"] and item["state"] == "running"}
            assert len(restored) == 2 and all(item["id"] not in before for item in restored.values())
            assert restored["group-api"]["jump"] == "good" and restored["group-web"]["target"] == "unverified"
            count = len(sessions())
            assert "already running 2" in ford("group", "open", "work").stdout
            assert len(sessions()) == count
            ford("group", "close", "work")
            wait_for(lambda: released(group_local) and released(group_remote), "restored group cleanup")
            ford("group", "remove", "work")
            check("saved groups restore multiple routes with new session IDs and no stale health results")

            failed_local = free_port()
            ford("noauth", f"{failed_local}:{target}", "--name", "group-bad", check=False)
            wait_for(lambda: failed("group-bad"), "failed group template")
            ford("group", "save", "partial", state("group-bad")["id"], restored["group-web"]["id"])
            before = {item["id"] for item in sessions()}
            ford("group", "open", "partial", check=False)
            wait_for(lambda: fetch(group_local) == PAYLOAD, "successful entry in partial group")
            wait_for(lambda: any(item["id"] not in before and item["name"] == "group-bad"
                                 and item["state"] == "failed" for item in sessions()),
                     "failed entry in partial group")
            ford("group", "close", "partial")
            wait_for(lambda: released(group_local), "partial group cleanup")
            ford("group", "remove", "partial")
            check("one failed group entry does not stop successful entries")

            with socket.socket() as occupied:
                occupied.bind(("127.0.0.1", 0))
                occupied.listen()
                busy = occupied.getsockname()[1]
                ford("good", f"{busy}:{target}", "--name", "conflict", check=False)
                wait_for(lambda: failed("conflict"), "local port conflict")
                assert state("conflict")["mappings"][0]["local"] == busy
            check("port conflict fails without changing the requested port")

            for alias in ["noauth", "badkey"]:
                ford(alias, f"{free_port()}:{target}", "--name", alias, check=False)
                wait_for(lambda: failed(alias), f"{alias} failure")
                assert ford("logs", alias).stdout.strip()
            check("authentication and changed host key failures retain diagnostics")

            for alias, remote in [("good", free_port()), ("denied", target)]:
                local = free_port()
                name = f"unavailable-{alias}"
                ford(alias, f"{local}:{remote}", "--name", name)
                wait_for(lambda: bool(ford("logs", name).stdout) or not released(local), "forward listener")
                try:
                    fetch(local)
                    raise AssertionError("unexpected target availability")
                except (OSError, http.client.HTTPException):
                    pass
                assert state(name)["state"] == "running"
                ford("close", name)
                wait_for(lambda: released(local), "unavailable target port release")
            check("unavailable target and server forwarding denial do not falsely imply HTTP health")

            local = free_port()
            foreground = subprocess.Popen([str(binary), "good", f"{local}:{target}", "--name", "fg",
                                           "--foreground"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: fetch(local) == PAYLOAD, "foreground forwarding")
            foreground.send_signal(signal.SIGINT)
            assert foreground.wait(timeout=10) == 130
            foreground = None
            wait_for(lambda: released(local), "foreground port release")
            check("foreground Ctrl+C cleanup")

            if args.bench:
                assert os.uname().sysname == "Linux", "RSS/CPU benchmark currently requires Linux /proc"
                help_times = []
                for _ in range(30):
                    start = time.perf_counter()
                    ford("--help")
                    help_times.append((time.perf_counter() - start) * 1000)
                report["binary_bytes"] = binary.stat().st_size
                report["help_warm_median_ms"] = sorted(help_times)[len(help_times) // 2]
                report["rust"] = subprocess.run(["rustc", "--version"], capture_output=True, text=True,
                                                 check=True).stdout.strip()
                report["kernel"] = os.uname().release
                baseline_port = free_port()
                ford("good", f"{baseline_port}:{target}", "--name", "comparison")
                wait_for(lambda: fetch(baseline_port) == PAYLOAD, "comparison tunnel")
                comparison = state("comparison")
                argv = Path(f"/proc/{comparison['ssh_pid']}/cmdline").read_bytes().rstrip(b"\0").split(b"\0")
                argv = [os.fsdecode(item) for item in argv]
                direct_port = free_port()
                argv[argv.index("-L") + 1] = f"127.0.0.1:{direct_port}:127.0.0.1:{target}"
                direct = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

                def transfer_measurements(port):
                    latencies = []
                    for _ in range(20):
                        start = time.perf_counter()
                        assert fetch(port) == PAYLOAD
                        latencies.append((time.perf_counter() - start) * 1000)
                    start = time.perf_counter()
                    for _ in range(5):
                        assert fetch(port, "/bulk") == BULK
                    duration = time.perf_counter() - start
                    return {"http_median_ms": sorted(latencies)[len(latencies) // 2],
                            "bulk_mib_per_second": 5 / duration}

                try:
                    wait_for(lambda: fetch(direct_port) == PAYLOAD, "direct SSH comparison")
                    report["transport_comparison"] = {
                        "ford": transfer_measurements(baseline_port),
                        "direct_ssh": transfer_measurements(direct_port),
                        "method": "identical SSH argv except local port; 20 small HTTP GETs and 5 x 1 MiB GETs",
                    }
                finally:
                    stop_process(direct)
                    ford("close", "comparison")
                for count in [1, 10, 50]:
                    creation, readiness = [], []
                    current = []
                    for index in range(count):
                        local = free_port()
                        name = f"bench-{count}-{index}"
                        start = time.perf_counter()
                        ford("good", f"{local}:{target}", "--name", name)
                        creation.append((time.perf_counter() - start) * 1000)
                        wait_for(lambda: fetch(local) == PAYLOAD, "benchmark forwarding")
                        readiness.append((time.perf_counter() - start) * 1000)
                        current.append(state(name))
                    pids = [item[key] for item in current for key in ["supervisor_pid", "ssh_pid"]]

                    def totals():
                        rss, threads, ticks = 0, 0, 0
                        for pid in pids:
                            status_text = Path(f"/proc/{pid}/status").read_text()
                            fields = dict(line.split(":", 1) for line in status_text.splitlines())
                            rss += int(fields["VmRSS"].split()[0])
                            threads += int(fields["Threads"].strip())
                            stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
                            ticks += int(stat[11]) + int(stat[12])
                        return rss, threads, ticks

                    rss, threads, before_ticks = totals()
                    time.sleep(1)
                    idle_ticks = totals()[2] - before_ticks
                    start = time.perf_counter()
                    ford("list", "--json")
                    list_ms = (time.perf_counter() - start) * 1000
                    start = time.perf_counter()
                    ford("close", "--all")
                    close_ms = (time.perf_counter() - start) * 1000
                    wait_for(lambda: all(not alive(pid) for pid in pids), "benchmark process cleanup")
                    report["benchmarks"].append({
                        "sessions": count, "processes": len(pids), "rss_kib": rss, "threads": threads,
                        "idle_cpu_percent_one_core": idle_ticks / os.sysconf("SC_CLK_TCK") * 100,
                        "mean_creation_ack_ms": sum(creation) / count,
                        "mean_first_http_ms": sum(readiness) / count, "list_ms": list_ms, "close_all_ms": close_ms,
                    })
                check("1/10/50-session management overhead and cleanup measured")
        except Exception:
            print(ford("list", "--all", "--json", check=False).stdout)
            for item in sessions():
                print(f"LOG {item['id']}:\n{ford('logs', item['id'], check=False).stdout[-4000:]}")
            for log in daemon_logs:
                log.flush()
                log.seek(0)
                print(log.read()[-4000:])
            raise
        finally:
            if foreground and foreground.poll() is None:
                foreground.terminate()
                foreground.wait(timeout=10)
            ford("close", "--all", check=False)
            server.shutdown()
            server.server_close()
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()

"""Exercise installation and PATH setup with isolated homes and local release downloads."""

import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

from release import TARGETS, archive_name


ROOT = Path(__file__).resolve().parent.parent
REPOSITORY = "https://github.com/Marukohe/ford"


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ford-installer-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        tools = self.root / "tools"
        tools.mkdir()
        self.env = dict(os.environ, HOME=str(self.home), SHELL="/bin/bash", FORD_TEST_OS="Linux",
                        FORD_TEST_ARCH="x86_64", FORD_TEST_ASSETS=str(self.assets),
                        FORD_TEST_URLS=str(self.root / "urls"), PATH=f"{tools}:{os.environ['PATH']}",
                        ZDOTDIR=str(self.home))
        self.write_tool(tools / "uname", "#!/bin/sh\n"
                        'case "$1" in -s) echo "$FORD_TEST_OS";; -m) echo "$FORD_TEST_ARCH";; esac\n')
        self.write_tool(tools / "curl", f"#!{sys.executable}\n" + '''
import os
from pathlib import Path
import shutil
import sys

args = sys.argv[1:]
url = next(arg for arg in args if arg.startswith("https://"))
with open(os.environ["FORD_TEST_URLS"], "a") as output:
    output.write(url + "\\n")
asset = Path(os.environ["FORD_TEST_ASSETS"]) / url.rsplit("/", 1)[-1]
if not asset.is_file():
    print("curl: release asset not found", file=sys.stderr)
    sys.exit(22)
shutil.copyfile(asset, args[args.index("--output") + 1])
''')
        self.make_release()

    @staticmethod
    def write_tool(path, text):
        path.write_text(text)
        path.chmod(0o755)

    def make_release(self, binary=b"#!/bin/sh\nprintf 'ford 0.1.0\\n'\n"):
        manifest = []
        for target in TARGETS:
            name = archive_name("v0.1.0", target)
            path = self.assets / name
            with tarfile.open(path, "w:gz") as archive:
                info = tarfile.TarInfo("ford")
                info.size = len(binary)
                info.mode = 0o755
                archive.addfile(info, io.BytesIO(binary))
            manifest.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {name}\n")
        (self.assets / "SHA256SUMS").write_text("".join(manifest))

    def install(self, *args, success=True):
        result = subprocess.run(["sh", str(ROOT / "scripts/install.sh"), *args], env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def shell(self, script, executable="sh"):
        result = subprocess.run([executable], input=script, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def test_platform_downloads_and_path_setup_are_repeatable(self):
        for system, machine, target, shell, profile in [
            ("Linux", "x86_64", TARGETS[0], "bash", ".bashrc"),
            ("Linux", "aarch64", TARGETS[1], "bash", ".bashrc"),
            ("Darwin", "x86_64", TARGETS[2], "zsh", ".zshrc"),
            ("Darwin", "arm64", TARGETS[3], "zsh", ".zshrc"),
        ]:
            with self.subTest(system=system, machine=machine):
                self.env.update(FORD_TEST_OS=system, FORD_TEST_ARCH=machine, SHELL=f"/bin/{shell}")
                startup = self.home / profile
                startup.write_text("# existing shell settings\n")
                self.install()
                once = startup.read_text()
                self.install()
                self.assertEqual(startup.read_text(), once)
                self.assertTrue(once.startswith("# existing shell settings\n"))
                self.assertEqual(once.count("# ford"), 1)
                urls = (self.root / "urls").read_text().splitlines()
                self.assertEqual(urls[-2], f"{REPOSITORY}/releases/latest/download/SHA256SUMS")
                self.assertEqual(urls[-1], f"{REPOSITORY}/releases/download/v0.1.0/"
                                 + archive_name("v0.1.0", target))
                output = self.shell('. "$HOME/.local/share/ford/env"\n'
                                    '. "$HOME/.local/share/ford/env"\nford --version\nprintf "%s" "$PATH"\n')
                self.assertTrue(output.startswith("ford 0.1.0\n"))
                self.assertEqual(output.splitlines()[-1].split(":").count(str(self.home / ".local/bin")), 1)
                executable = shell if shutil.which(shell) else "sh"
                self.assertEqual(self.shell(f'. "$HOME/{profile}"\nford --version\n', executable),
                                 "ford 0.1.0\n")

    def test_custom_directory_is_quoted_and_version_is_pinned(self):
        directory = self.home / "bin with 'quotes' and $(touch injected)"
        self.install("--version", "0.1.0", "--bin-dir", str(directory), "--no-modify-path")
        for shell in ["sh", "bash", "zsh"]:
            if shutil.which(shell):
                with self.subTest(shell=shell):
                    self.assertEqual(self.shell('. "$HOME/.local/share/ford/env"\nford --version\n', shell),
                                     "ford 0.1.0\n")
        self.assertFalse((ROOT / "injected").exists())
        self.assertFalse((self.home / ".bashrc").exists())
        self.assertNotIn("/latest/", (self.root / "urls").read_text())

    def test_piped_installation_prints_current_terminal_path_setup(self):
        result = subprocess.run(["sh"], input=(ROOT / "scripts/install.sh").read_text(), env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('. "$HOME/.local/share/ford/env"', result.stdout)
        self.assertEqual(self.shell('. "$HOME/.bashrc"\nford --version\n'), "ford 0.1.0\n")

    def test_login_profiles_and_custom_zsh_directory(self):
        for name in [".profile", ".bash_login", ".bash_profile"]:
            with self.subTest(profile=name):
                path = self.home / name
                path.write_text("# login settings\n")
                self.install()
                self.assertIn("# ford", path.read_text())
        self.env.update(SHELL="/bin/zsh", ZDOTDIR=str(self.home / "zsh config"))
        self.install()
        self.assertIn("# ford", (self.home / "zsh config/.zshrc").read_text())

    def test_unset_shell_defaults_to_profile(self):
        del self.env["SHELL"]
        self.install()
        self.assertIn("# ford", (self.home / ".profile").read_text())

    def test_shasum_verification_without_sha256sum(self):
        tools = self.root / "tools"
        for tool in ["sh", "gzip", "tar", "awk", "sed", "mktemp", "shasum", "grep", "chmod",
                     "mkdir", "cp", "mv", "rm"]:
            (tools / tool).symlink_to(shutil.which(tool))
        self.env["PATH"] = str(tools)
        self.install()
        self.assertTrue((self.home / ".local/bin/ford").is_file())

    def assert_old_install_preserved(self, *args):
        binary = self.home / ".local/bin/ford"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(b"previous installation")
        result = self.install(*args, success=False)
        self.assertEqual(binary.read_bytes(), b"previous installation")
        self.assertFalse((self.home / ".bashrc").exists())
        self.assertFalse((self.home / ".local/share/ford/env").exists())
        return result

    def test_checksum_failure_keeps_previous_installation(self):
        (self.assets / archive_name("v0.1.0", TARGETS[0])).write_bytes(b"tampered download")
        result = self.assert_old_install_preserved()
        self.assertIn("SHA256 verification failed", result.stderr)

    def test_existing_directory_is_not_used_as_an_installation_file(self):
        directory = self.home / ".local/bin/ford"
        directory.mkdir(parents=True)
        result = self.install(success=False)
        self.assertIn("installation path is a directory", result.stderr)
        self.assertEqual(list(directory.iterdir()), [])

    def test_failed_download_and_wrong_manifest_version(self):
        result = self.assert_old_install_preserved("--version", "v0.2.0")
        self.assertIn("do not match the requested version", result.stderr)
        (self.assets / "SHA256SUMS").unlink()
        result = self.assert_old_install_preserved()
        self.assertIn("check that the requested release is published", result.stderr)

    def test_ambiguous_manifest_is_rejected(self):
        path = self.assets / "SHA256SUMS"
        path.write_text(path.read_text() * 2)
        self.assertIn("exactly one archive", self.assert_old_install_preserved().stderr)

    def test_unusable_or_wrong_binary_keeps_previous_installation(self):
        for binary in [b"#!/bin/sh\nexit 1\n", b"#!/bin/sh\necho 'ford 9.9.9'\n"]:
            with self.subTest(binary=binary):
                self.make_release(binary)
                self.assert_old_install_preserved()

    def test_unsupported_hosts_and_invalid_options_do_not_download(self):
        for system, machine, args in [
            ("FreeBSD", "x86_64", []), ("Linux", "riscv64", []),
            ("Linux", "x86_64", ["--version", "../bad"]),
            ("Linux", "x86_64", ["--bin-dir", "relative"]),
            ("Linux", "x86_64", ["--version"]),
        ]:
            with self.subTest(system=system, machine=machine, args=args):
                self.env.update(FORD_TEST_OS=system, FORD_TEST_ARCH=machine)
                self.install(*args, success=False)
                self.assertFalse((self.root / "urls").exists())


if __name__ == "__main__":
    unittest.main()

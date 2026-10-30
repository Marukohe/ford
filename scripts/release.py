#!/usr/bin/env python3
"""Validate release versions, package native binaries, and prepare notes and checksums."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parent.parent
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)


def cargo_version():
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    )
    packages = json.loads(output.stdout)["packages"]
    return next(package["version"] for package in packages if Path(package["manifest_path"]) == ROOT / "Cargo.toml")


def check_version(tag, version):
    if tag != f"v{version}":
        raise ValueError(f"tag {tag!r} must match Cargo.toml version: v{version}")
    return {"version": version, "prerelease": "-" in version.split("+", 1)[0]}


def changelog_notes(text, version):
    sections = list(re.finditer(r"^## \[([^\]]+)\].*$", text, re.MULTILINE))
    for index, section in enumerate(sections):
        if section.group(1) == version:
            end = sections[index + 1].start() if index + 1 < len(sections) else len(text)
            notes = text[section.end():end].strip()
            if not notes:
                raise ValueError(f"empty changelog section for {version}")
            return notes + "\n"
    return None


def archive_name(tag, target):
    if target not in TARGETS:
        raise ValueError(f"unsupported release target: {target}")
    return f"ford-{tag}-{target}.tar.gz"


def package(tag, version, target, output):
    name = archive_name(tag, target)
    host = subprocess.run(["rustc", "-vV"], cwd=ROOT, check=True, capture_output=True, text=True).stdout
    if f"host: {target}\n" not in host:
        raise ValueError("release packaging requires a native build; refusing to mislabel the current host")
    binary = ROOT / "target" / target / "release" / "ford"
    actual = subprocess.run([str(binary), "--version"], check=True, capture_output=True, text=True).stdout.strip()
    if actual != f"ford {version}":
        raise ValueError(f"binary version mismatch: {actual!r}")
    subprocess.run([str(binary), "--help"], check=True, stdout=subprocess.DEVNULL)
    timestamp = int(subprocess.run(["git", "show", "-s", "--format=%ct", "HEAD"], cwd=ROOT,
                                   check=True, capture_output=True, text=True).stdout.strip())
    output.mkdir(parents=True, exist_ok=True)
    files = [(binary, "ford"), (ROOT / "README.md", "README.md"), (ROOT / "CHANGELOG.md", "CHANGELOG.md")]
    files.extend((path, f"docs/{path.name}") for path in sorted((ROOT / "docs").glob("*.md")))
    with tempfile.NamedTemporaryFile(dir=output, delete=False) as temporary:
        temp_path = Path(temporary.name)
    try:
        with temp_path.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for path, filename in files:
                    info = archive.gettarinfo(str(path), arcname=filename)
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mode = 0o755 if filename == "ford" else 0o644
                    info.mtime = timestamp
                    with path.open("rb") as source:
                        archive.addfile(info, source)
        destination = output / name
        os.replace(temp_path, destination)
        return destination
    finally:
        temp_path.unlink(missing_ok=True)


def checksums(tag, directory, targets=TARGETS):
    expected = {archive_name(tag, target) for target in targets}
    found = {path.name for path in directory.iterdir() if path.name != "SHA256SUMS"}
    if found != expected:
        raise ValueError(
            f"release asset mismatch; missing={sorted(expected - found)}, unexpected={sorted(found - expected)}"
        )
    lines = []
    for name in sorted(expected):
        digest = hashlib.sha256()
        with (directory / name).open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        lines.append(f"{digest.hexdigest()}  {name}\n")
    manifest = directory / "SHA256SUMS"
    manifest.write_text("".join(lines))
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    version_parser = commands.add_parser("check-version")
    version_parser.add_argument("tag")
    version_parser.add_argument("--github-output", type=Path)
    package_parser = commands.add_parser("package")
    package_parser.add_argument("tag")
    package_parser.add_argument("target", choices=TARGETS)
    package_parser.add_argument("--output", type=Path, default=ROOT / "target/dist")
    notes_parser = commands.add_parser("notes")
    notes_parser.add_argument("tag")
    notes_parser.add_argument("--output", type=Path, required=True)
    sums_parser = commands.add_parser("checksums")
    sums_parser.add_argument("tag")
    sums_parser.add_argument("--directory", type=Path, default=ROOT / "target/dist")
    sums_parser.add_argument(
        "--target", action="append", choices=TARGETS, help="limit local checks to specified targets"
    )
    args = parser.parse_args()
    version = cargo_version()
    info = check_version(args.tag, version)
    if args.command == "check-version":
        if args.github_output:
            with args.github_output.open("a") as output:
                output.write(f"version={version}\nprerelease={str(info['prerelease']).lower()}\n")
        print(json.dumps(info))
    elif args.command == "package":
        print(package(args.tag, version, args.target, args.output))
    elif args.command == "checksums":
        print(checksums(args.tag, args.directory, args.target or TARGETS))
    elif args.command == "notes":
        notes = changelog_notes((ROOT / "CHANGELOG.md").read_text(), version)
        if notes is None:
            notes = subprocess.run(
                ["git-cliff", "--config", str(ROOT / "cliff.toml"), "--current", "--strip", "header"],
                cwd=ROOT, check=True, capture_output=True, text=True,
            ).stdout
        if not notes.strip():
            raise ValueError("refusing to publish empty release notes")
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(notes)
        print(args.output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"release: {error}") from error

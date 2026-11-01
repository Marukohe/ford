# Releasing ford

The repository is hosted at [Marukohe/ford](https://github.com/Marukohe/ford). CI runs on pushes to main and pull
requests; pushing a version tag triggers the release workflow. Check [Actions](https://github.com/Marukohe/ford/actions)
for hosted validation results and [Releases](https://github.com/Marukohe/ford/releases) for published downloads.

## One-Time Setup

For a fresh local checkout without a remote, connect it to the GitHub repository and push the main branch:

```bash
git remote add origin git@github.com:Marukohe/ford.git
git push -u origin main
```

Enable GitHub Actions if repository or organization settings require it. The workflow uses GitHub's built-in token;
no personal access token or release secret is required. Only the publishing job requests `contents: write`.

CI runs on pushes to main and pull requests. It checks formatting, Clippy, Cargo tests, and release helpers on four
native runners. Linux runners also run the isolated real SSH checks. The release workflow calls the same CI before
building release archives.

## Installer

The `scripts/install.sh` script downloads compiled release archives. It reads the latest release's `SHA256SUMS`
through GitHub's stable download URL, selects the native target, then downloads that exact version's archive. A
specific version can be selected with `--version v0.1.0`. It does not use the GitHub API or require Rust on the user's
machine. See
[GitHub's release links](https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases).

The script verifies the checksum and runs `ford --version` before replacing an existing executable. Its default
destination is `~/.local/bin`, with an optional absolute `--bin-dir`. It writes a PATH loader to
`~/.local/share/ford/env` and adds it once to the relevant shell startup files. Bash configures `.bashrc` and the
active login profile; zsh configures `.zshrc` under `$ZDOTDIR` or `$HOME`. `--no-modify-path` leaves startup files
alone. The installer prints a source command for the current terminal, since a child process cannot change its
parent's environment. Users can run that command or open a new terminal after installing.

Publish `scripts/install.sh` on `main` before advertising the raw GitHub installer URL. Publish the first release
before downloads can succeed. Keep archive names and `SHA256SUMS` compatible with the installer when changing assets.
CI tests the installer on all four native runners using isolated homes, controlled downloads, and real checksum tools:

```bash
sh -n scripts/install.sh
python3 -m unittest discover -s scripts -p 'test_install.py' -v
```

For a specific version or installation directory, run the script with options:

```bash
sh scripts/install.sh --version v0.1.0 --bin-dir "$HOME/bin"
```

## First Release

`Cargo.toml` and `Cargo.lock` currently identify version 0.1.0. Its curated release notes are already in CHANGELOG.md.
Review that entry, commit any edits, and push the branch before the tag:

```bash
python3 scripts/release.py check-version v0.1.0
python3 scripts/release.py notes v0.1.0 --output target/release-notes.md
cat target/release-notes.md

git push origin main
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

Pushing the tag triggers CI, builds, notes, checksums, and publication. A tag must exactly match `v` followed by the
Cargo package version. Prerelease versions such as `0.2.0-rc.1` are published as prereleases and are not marked latest.
Do not move an existing published tag to a different commit; prepare a new version instead.

## Release Assets

Each release has four archives plus SHA256SUMS:

```text
ford-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
ford-v0.1.0-aarch64-unknown-linux-gnu.tar.gz
ford-v0.1.0-x86_64-apple-darwin.tar.gz
ford-v0.1.0-aarch64-apple-darwin.tar.gz
SHA256SUMS
```

Every archive contains `ford`, README.md, CHANGELOG.md, and the linked documentation under `docs/`. The executable is
built and smoke-tested on its native runner. Linux GNU binaries use system glibc; x86_64 builds use Ubuntu 22.04 and
ARM64 builds use Ubuntu 24.04.
macOS builds use macOS 15 runners. System OpenSSH remains a runtime requirement.

Artifacts, downloaded tools, package previews, and generated notes stay under `target/`, outside Git history.
The publishing job requires all four exact archive names before writing the checksum manifest, so incomplete or mixed
versions fail before release creation. An existing GitHub Release is not overwritten by a rerun.

Users can verify an individual downloaded archive using its line in SHA256SUMS, then extract it:

```bash
# Linux; when all four archives are present
sha256sum --check SHA256SUMS

# macOS equivalent
shasum -a 256 --check SHA256SUMS

tar -xzf ford-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
./ford --version
```

## Future Release Notes

Use meaningful Conventional Commit subjects. `cliff.toml` groups them into Added, Fixed, Performance, Changed,
Documentation, Build and Maintenance, and Other. Breaking changes receive an explicit marker. Examples:

```text
feat: add automatic reconnect with bounded retries
fix: release all owned listeners before confirming close
perf: reduce session-list overhead
feat!: change the port-mapping syntax
```

Install the same git-cliff version used by CI when preparing the next changelog:

```bash
cargo install git-cliff --version 2.13.0 --locked
```

To prepare 0.1.1:

1. Update `package.version` in Cargo.toml to `0.1.1`.
2. Run `cargo check` to update the root package version in Cargo.lock.
3. Generate only the new entry, preserving previously curated release history:

   ```bash
   git-cliff --config cliff.toml --unreleased --tag v0.1.1 --prepend CHANGELOG.md
   ```

4. Review the generated entry and edit it for users. Commit the version files and changelog:

   ```bash
   git add Cargo.toml Cargo.lock CHANGELOG.md
   git commit -m "chore(release): prepare v0.1.1"
   git push origin main
   git tag -a v0.1.1 -m "Release v0.1.1"
   git push origin v0.1.1
   ```

The release workflow extracts the matching CHANGELOG.md section, preserving edits. If no matching section exists,
it generates notes for the current tag with git-cliff using the complete fetched history. It refuses empty notes.
Avoid regenerating the entire changelog with `--output CHANGELOG.md` when preserving curated historical entries.

## Local Packaging Preview

Run the native build and helpers without creating a tag or contacting GitHub. This example uses Linux x86_64:

```bash
cargo build --release --locked --target x86_64-unknown-linux-gnu
python3 scripts/release.py package v0.1.0 x86_64-unknown-linux-gnu --output target/dist
python3 scripts/release.py checksums v0.1.0 --directory target/dist --target x86_64-unknown-linux-gnu
tar -tzf target/dist/ford-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
python3 -m unittest discover -s scripts -p 'test_release.py' -v
```

Use the target matching your native Rust host. The helper rejects a mismatched host, stale executable version,
unsupported target, incomplete asset set, or tag/package version mismatch.

Local checks do not establish native macOS or Linux ARM64 correctness; use the corresponding hosted job results when
assessing those platforms.

## References

- [GitHub release CLI](https://cli.github.com/manual/gh_release_create)
- [GitHub-hosted runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [git-cliff examples, including prepend](https://git-cliff.org/docs/usage/examples/)

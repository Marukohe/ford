#!/bin/sh
# Install a verified GitHub release without requiring Rust or administrator access.
set -eu

fail() { printf 'ford installer: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Usage: sh scripts/install.sh [--version VERSION] [--bin-dir DIRECTORY] [--no-modify-path]

Install the latest stable ford release to ~/.local/bin by default.
  --version VERSION   Select a release, such as v0.1.0 or 0.1.0
  --bin-dir DIRECTORY Use an absolute installation directory
  --no-modify-path    Leave shell startup files unchanged
EOF
}

version=latest
bin_dir=${HOME:?HOME must be set}/.local/bin
modify_path=true
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version|--bin-dir)
            [ "$#" -ge 2 ] || fail "missing value for $1"
            case "$1" in
                --version) version=$2 ;;
                --bin-dir) bin_dir=$2 ;;
            esac
            shift 2 ;;
        --no-modify-path) modify_path=false; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown option: $1" ;;
    esac
done

if [ "$version" != latest ]; then
    version=v${version#v}
    printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$' ||
        fail "invalid release version: $version"
fi
case "$bin_dir" in /*) ;; *) fail '--bin-dir must be an absolute path' ;; esac
case "$bin_dir" in *:*|*'
'*) fail 'installation directory cannot contain colons or newlines' ;; esac

case "$(uname -s)" in
    Linux) platform=unknown-linux-gnu ;;
    Darwin) platform=apple-darwin ;;
    *) fail 'supported operating systems are Linux and macOS' ;;
esac
case "$(uname -m)" in
    x86_64|amd64) architecture=x86_64 ;;
    arm64|aarch64) architecture=aarch64 ;;
    *) fail 'supported architectures are x86_64 and ARM64' ;;
esac
target=$architecture-$platform
for tool in curl tar awk sed mktemp; do
    command -v "$tool" >/dev/null 2>&1 || fail "required command not found: $tool"
done
if command -v sha256sum >/dev/null 2>&1; then
    checksum_tool=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    checksum_tool=shasum
else
    fail 'SHA256 verification requires sha256sum or shasum'
fi

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ford-install.XXXXXX")
staged_file=
cleanup() {
    rm -rf "$work_dir"
    if [ -n "$staged_file" ]; then rm -f "$staged_file"; fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
repository=https://github.com/Marukohe/ford
if [ "$version" = latest ]; then
    manifest_url=$repository/releases/latest/download/SHA256SUMS
else
    manifest_url=$repository/releases/download/$version/SHA256SUMS
fi
download() {
    curl --fail --silent --show-error --location --retry 3 --connect-timeout 15 --max-time 300 \
        --proto '=https' --proto-redir '=https' "$1" --output "$2"
}
printf 'Finding ford %s for %s...\n' "$version" "$target"
download "$manifest_url" "$work_dir/SHA256SUMS" ||
    fail "cannot download release checksums; check that the requested release is published: $manifest_url"

# Resolve the tag from the manifest, then pin the archive URL to that tag even if latest changes.
entry=$(awk -v target="$target" '
    NF == 2 && $2 ~ "^ford-v[0-9][0-9A-Za-z.+-]*-" target "\\.tar\\.gz$" {
        if (length($1) != 64 || $1 ~ /[^0-9a-f]/) exit 1
        count++; result = $1 " " $2
    }
    END { if (count != 1) exit 1; print result }
' "$work_dir/SHA256SUMS") || fail "release checksums must contain exactly one archive for $target"
expected=${entry%% *}
asset=${entry#* }
resolved_version=${asset#ford-}
resolved_version=${resolved_version%-$target.tar.gz}
if [ "$version" != latest ] && [ "$version" != "$resolved_version" ]; then
    fail 'release checksums do not match the requested version'
fi
version=$resolved_version
printf 'Downloading ford %s...\n' "$version"
download "$repository/releases/download/$version/$asset" "$work_dir/archive.tar.gz" ||
    fail "cannot download $asset"
if [ "$checksum_tool" = sha256sum ]; then
    actual=$(sha256sum "$work_dir/archive.tar.gz")
else
    actual=$(shasum -a 256 "$work_dir/archive.tar.gz")
fi
[ "${actual%% *}" = "$expected" ] || fail 'SHA256 verification failed; existing installation was not changed'

# Extract only the executable's contents, without creating paths or links from the archive.
tar -xOzf "$work_dir/archive.tar.gz" ford > "$work_dir/ford"
chmod 755 "$work_dir/ford"
actual_version=$("$work_dir/ford" --version) ||
    fail 'the downloaded executable cannot run on this system; existing installation was not changed'
[ "$actual_version" = "ford ${version#v}" ] || fail 'downloaded executable version does not match the release'
mkdir -p "$bin_dir"
[ ! -d "$bin_dir/ford" ] || fail "installation path is a directory: $bin_dir/ford"
staged_file=$(mktemp "$bin_dir/.ford.XXXXXX")
cp "$work_dir/ford" "$staged_file"
chmod 755 "$staged_file"
mv -f "$staged_file" "$bin_dir/ford"
staged_file=

# Single-quote the literal directory so shell metacharacters in paths are never evaluated.
quoted_bin=$(printf '%s' "$bin_dir" | sed "s/'/'\\\\''/g")
env_dir=$HOME/.local/share/ford
mkdir -p "$env_dir"
[ ! -d "$env_dir/env" ] || fail "PATH loader path is a directory: $env_dir/env"
{
    printf '# Add ford to PATH. Generated by the ford installer.\n'
    printf 'case ":${PATH-}:" in\n'
    printf "    *:'%s':*) ;;\n" "$quoted_bin"
    printf "    *) export PATH='%s'\"\${PATH:+:\$PATH}\" ;;\n" "$quoted_bin"
    printf 'esac\n'
} > "$work_dir/env"
staged_file=$(mktemp "$env_dir/.env.XXXXXX")
cp "$work_dir/env" "$staged_file"
mv -f "$staged_file" "$env_dir/env"
staged_file=

profile_line='[ ! -f "$HOME/.local/share/ford/env" ] || . "$HOME/.local/share/ford/env"'
update_profile() {
    if [ -e "$1" ] && [ ! -f "$1" ]; then fail "shell startup path is not a file: $1"; fi
    if [ ! -f "$1" ] || ! grep -Fqx "$profile_line" "$1"; then
        printf '\n# ford\n%s\n' "$profile_line" >> "$1"
        printf 'Updated %s\n' "$1"
    fi
}
if [ "$modify_path" = true ]; then
    login_shell=${SHELL:-sh}
    case "${login_shell##*/}" in
        zsh)
            zsh_dir=${ZDOTDIR:-$HOME}
            mkdir -p "$zsh_dir"
            update_profile "$zsh_dir/.zshrc" ;;
        bash)
            update_profile "$HOME/.bashrc"
            if [ -f "$HOME/.bash_profile" ]; then
                update_profile "$HOME/.bash_profile"
            elif [ -f "$HOME/.bash_login" ]; then
                update_profile "$HOME/.bash_login"
            else
                update_profile "$HOME/.profile"
            fi ;;
        sh|dash|ksh) update_profile "$HOME/.profile" ;;
        *) printf 'Shell not configured automatically; add the installation directory to PATH manually.\n' ;;
    esac
fi
printf 'Installed ford %s to %s/ford\n' "$version" "$bin_dir"
printf 'To use ford in this terminal, run:\n  . "$HOME/.local/share/ford/env"\n'

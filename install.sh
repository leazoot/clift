#!/bin/sh
# Installs a released clift binary for macOS or Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh
#
# What it does, in order: work out the platform, find the release, download the
# archive and the SHA256SUMS file that was published next to it, refuse to go on
# unless the archive's digest matches, and only then copy the binary into place.
# Nothing is written outside a private temporary directory until the digest has
# been checked. It never asks for sudo.
#
# Options (flags win over environment variables):
#   --version <x.y.z>   CLIFT_VERSION         release to install (default: latest)
#   --dir <path>        CLIFT_INSTALL_DIR     where to put the binary (default: ~/.local/bin)
#   --with-relayd       CLIFT_WITH_RELAYD=1   also install clift-relayd
#                       CLIFT_DOWNLOAD_BASE   mirror of the releases area (default: GitHub)
#
# It is plain POSIX sh on purpose: the machine that runs it may have nothing but
# the system shell, curl or wget, tar, and one of sha256sum or shasum.

set -eu

REPO="leazoot/clift"
DOWNLOAD_BASE="${CLIFT_DOWNLOAD_BASE:-https://github.com/${REPO}/releases}"
VERSION="${CLIFT_VERSION:-}"
INSTALL_DIR="${CLIFT_INSTALL_DIR:-${HOME}/.local/bin}"
WITH_RELAYD="${CLIFT_WITH_RELAYD:-0}"
NO_SETUP="${CLIFT_NO_SETUP:-0}"

say() { printf '%s\n' "$*" >&2; }

fail() {
    say "install.sh: $*"
    exit 1
}

usage() {
    cat >&2 <<'USAGE'
usage: install.sh [--version <x.y.z>] [--dir <path>] [--with-relayd] [--no-setup]

  --version <x.y.z>   release to install (default: latest)     CLIFT_VERSION
  --dir <path>        where to put the binary (default: ~/.local/bin)  CLIFT_INSTALL_DIR
  --with-relayd       also install clift-relayd                CLIFT_WITH_RELAYD=1
  --no-setup          install only; do not start `clift setup`  CLIFT_NO_SETUP=1
                      mirror of the releases area              CLIFT_DOWNLOAD_BASE

After installing, `clift setup` asks a few questions on the terminal (only
when there is one) so that the first paste works.

Downloads are verified against the SHA256SUMS published with the release.
No sudo is used.
USAGE
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || fail "--version needs a value"; VERSION="$2"; shift 2 ;;
        --version=*) VERSION="${1#--version=}"; shift ;;
        --dir) [ $# -ge 2 ] || fail "--dir needs a value"; INSTALL_DIR="$2"; shift 2 ;;
        --dir=*) INSTALL_DIR="${1#--dir=}"; shift ;;
        --with-relayd) WITH_RELAYD=1; shift ;;
        --no-setup) NO_SETUP=1; shift ;;
        -h|--help) usage ;;
        *) fail "unknown option: $1 (try --help)" ;;
    esac
done

# --- platform ---------------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
    Darwin) os_parts="apple-darwin" ;;
    # Releases ship static musl binaries for Linux; a glibc archive is taken
    # only if a release lists no musl one for this architecture.
    Linux) os_parts="unknown-linux-musl unknown-linux-gnu" ;;
    MINGW*|MSYS*|CYGWIN*)
        fail "this is the macOS/Linux installer; on Windows run, in PowerShell:
  irm https://raw.githubusercontent.com/${REPO}/main/install.ps1 | iex" ;;
    *) fail "no prebuilt binary for $os; see https://github.com/${REPO}/releases" ;;
esac

case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *) fail "no prebuilt binary for $os on $arch; see https://github.com/${REPO}/releases" ;;
esac


# --- tools ------------------------------------------------------------------

if command -v curl >/dev/null 2>&1; then
    fetcher=curl
elif command -v wget >/dev/null 2>&1; then
    fetcher=wget
else
    fail "need curl or wget"
fi

command -v tar >/dev/null 2>&1 || fail "need tar"

if command -v sha256sum >/dev/null 2>&1; then
    digest() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    digest() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    fail "need sha256sum or shasum to verify the download"
fi

# fetch <url> <destination>
fetch() {
    case "$fetcher" in
        curl) curl -fsSL --proto '=https,http' -o "$2" "$1" ;;
        wget) wget -q -O "$2" "$1" ;;
    esac
}

# The latest release is found by following the redirect GitHub puts behind
# /releases/latest. That needs no API token and is not subject to the API's
# unauthenticated rate limit.
resolve_latest() {
    url="${DOWNLOAD_BASE}/latest"
    case "$fetcher" in
        curl) landed="$(curl -fsSL -o /dev/null -w '%{url_effective}' "$url")" ;;
        wget) landed="$(wget -q --max-redirect=0 -S -O /dev/null "$url" 2>&1 \
                  | sed -n 's/^ *[Ll]ocation: *//p' | head -n1 | tr -d '\r')" ;;
    esac
    case "$landed" in
        */tag/v*) printf '%s' "${landed##*/tag/v}" ;;
        *) fail "could not work out the latest release from $url (got: ${landed:-nothing})" ;;
    esac
}

case "$DOWNLOAD_BASE" in
    https://*) ;;
    *) say "install.sh: warning: CLIFT_DOWNLOAD_BASE is not https; the digest check still applies, but the SHA256SUMS file comes from the same place" ;;
esac

# --- release ----------------------------------------------------------------

if [ -z "$VERSION" ]; then
    VERSION="$(resolve_latest)"
fi
VERSION="${VERSION#v}"

release_url="${DOWNLOAD_BASE}/download/v${VERSION}"

workdir="$(mktemp -d "${TMPDIR:-/tmp}/clift-install.XXXXXX")"
chmod 0700 "$workdir"
trap 'rm -rf "$workdir"' EXIT INT TERM

# SHA256SUMS is fetched first: it is both the list of what the release
# contains and the only thing an archive is trusted against.
fetch "${release_url}/SHA256SUMS" "${workdir}/SHA256SUMS" \
    || fail "could not download ${release_url}/SHA256SUMS; refusing to install an unverified archive"

# listed_digest <archive name>: the digest SHA256SUMS gives for exactly that
# name, whether the line is "<hex>  <name>" or "<hex> *<name>" (binary mode).
listed_digest() {
    awk -v name="$1" '
        { line = $0; sub(/^[0-9a-fA-F]+[ \t]+\*?/, "", line); if (line == name) { print $1; exit } }
    ' "${workdir}/SHA256SUMS"
}

target=""
expected=""
for os_part in $os_parts; do
    candidate="${arch_part}-${os_part}"
    expected="$(listed_digest "clift-${VERSION}-${candidate}.tar.gz")"
    if [ -n "$expected" ]; then
        target="$candidate"
        break
    fi
done
[ -n "$target" ] || fail "release ${VERSION} has no archive for ${arch_part} ${os}; see https://github.com/${REPO}/releases/tag/v${VERSION}"

archive="clift-${VERSION}-${target}.tar.gz"

say "Downloading clift ${VERSION} for ${target}..."
fetch "${release_url}/${archive}" "${workdir}/${archive}" \
    || fail "could not download ${release_url}/${archive}"

# --- verify -----------------------------------------------------------------

actual="$(digest "${workdir}/${archive}")"
if [ "$actual" != "$expected" ]; then
    fail "digest mismatch for ${archive}
  expected: ${expected}
  actual:   ${actual}
Nothing was installed."
fi

# --- install ----------------------------------------------------------------

tar -xzf "${workdir}/${archive}" -C "$workdir"
unpacked="${workdir}/clift-${VERSION}-${target}"
[ -f "${unpacked}/clift" ] || fail "archive did not contain a clift binary"

mkdir -p "$INSTALL_DIR" || fail "could not create ${INSTALL_DIR}"
[ -w "$INSTALL_DIR" ] || fail "${INSTALL_DIR} is not writable; pick another with --dir (no sudo is used)"

install -m 0755 "${unpacked}/clift" "${INSTALL_DIR}/clift"
installed="clift"
if [ "$WITH_RELAYD" = "1" ]; then
    [ -f "${unpacked}/clift-relayd" ] || fail "archive did not contain clift-relayd"
    install -m 0755 "${unpacked}/clift-relayd" "${INSTALL_DIR}/clift-relayd"
    installed="clift and clift-relayd"
fi

# Run what was just installed. On Linux this is also the check that the glibc
# build works on this system at all.
reported="$("${INSTALL_DIR}/clift" --version 2>&1)" \
    || fail "${INSTALL_DIR}/clift was installed but does not run on this system:
  ${reported}"

say "Installed ${installed} ${VERSION} (${target}) to ${INSTALL_DIR}"
say "  verified: sha256 ${actual}"

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        say ""
        say "${INSTALL_DIR} is not on your PATH. Add it for your shell:"
        case "$(basename "${SHELL:-sh}")" in
            zsh)  say "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc" ;;
            fish) say "  fish_add_path ${INSTALL_DIR}" ;;
            *)    say "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc" ;;
        esac
        say "then open a new terminal."
        ;;
esac

say ""
# Piped through `sh`, stdin is this script, so the questions are asked on the
# terminal itself. Without one (CI, a redirected run) nothing waits: the
# command is named and the script ends.
if [ "${NO_SETUP}" = "1" ]; then
    say "Next: clift setup"
elif [ -t 2 ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then
    "${INSTALL_DIR}/clift" setup </dev/tty || say "Set up later with: clift setup"
else
    say "Next: clift setup"
fi

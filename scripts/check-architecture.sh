#!/usr/bin/env bash
#
# Executable form of the architecture and security boundaries. Ten stages of feature work will erode these
# rules unless something fails the build when they are crossed, so this script
# is wired into CI and must stay dependency-free (bash + cargo + grep only).
#
# Usage: scripts/check-architecture.sh

set -euo pipefail

cd "$(dirname "$0")/.."

ADAPTER_CRATES=(clift-cli clift-clipboard clift-transport clift-relay clift-relayd clift-inject)

# Options that silently disable man-in-the-middle protection. Clift must never
# emit them, and must never suggest them in user-facing text.
WEAKENING_PATTERNS=(
    'StrictHostKeyChecking'
    'UserKnownHostsFile'
    'CheckHostIP=no'
    'NoHostAuthenticationForLocalhost'
)

# Exemptions, written as 'pattern|path'. An entry exempts only the pattern it
# names, so allowing one string in a file does not quietly allow the others.
# Every entry needs a reason:
#   scripts/check-architecture.sh - this file defines the patterns it searches for.
#   tests/e2e/docker/sshd-fixture.sh - the fixture pins the container's real host
#       key into its own known_hosts file. That is stronger verification, not
#       weaker: remove the pin and the connection is refused, which the test
#       host_key_verification_is_enforced_not_bypassed asserts.
WEAKENING_ALLOWLIST=(
    'StrictHostKeyChecking|scripts/check-architecture.sh'
    'UserKnownHostsFile|scripts/check-architecture.sh'
    'CheckHostIP=no|scripts/check-architecture.sh'
    'NoHostAuthenticationForLocalhost|scripts/check-architecture.sh'
    'UserKnownHostsFile|tests/e2e/docker/sshd-fixture.sh'
)

FAILURES=0

pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }

# --- 1. clift-core must not depend on any adapter crate ----------------------

printf '[1/10] clift-core dependency direction\n'

# The manifest is checked directly rather than only through `cargo tree`,
# because a core-to-adapter edge is also a dependency cycle: cargo would refuse
# to resolve it and the tree check would pass vacuously on an empty result.
for crate in "${ADAPTER_CRATES[@]}"; do
    if grep -nE "(^|[^a-z-])${crate}([^a-z-]|$)" crates/clift-core/Cargo.toml \
        | grep -v '^[0-9]*:name = ' >/dev/null; then
        fail "crates/clift-core/Cargo.toml references ${crate}"
        grep -nE "(^|[^a-z-])${crate}([^a-z-]|$)" crates/clift-core/Cargo.toml | sed 's/^/        /'
    else
        pass "clift-core does not depend on ${crate}"
    fi
done

# Second line of defence: catch an adapter pulled in transitively through a
# third-party crate. A failure of cargo itself must not be mistaken for a pass.
if ! CORE_TREE="$(cargo tree -p clift-core --edges normal,build,dev --prefix none --no-dedupe 2>&1)"; then
    fail "cargo tree -p clift-core failed; dependency graph could not be verified"
    printf '%s\n' "$CORE_TREE" | sed 's/^/        /'
else
    tree_hits=""
    for crate in "${ADAPTER_CRATES[@]}"; do
        if printf '%s\n' "$CORE_TREE" | grep -qE "^${crate} "; then
            tree_hits="${tree_hits} ${crate}"
        fi
    done
    if [ -n "$tree_hits" ]; then
        fail "clift-core dependency graph contains:${tree_hits}"
    else
        pass "clift-core resolved dependency graph contains no adapter crate"
    fi
fi

# --- 2. clift-core must contain no IO and no platform API --------------------

printf '[2/10] clift-core purity\n'
for pattern in 'std::process' 'objc' 'StrictHostKeyChecking'; do
    if grep -rn --include='*.rs' -F "$pattern" crates/clift-core/src >/dev/null 2>&1; then
        fail "crates/clift-core/src contains '${pattern}'"
        grep -rn --include='*.rs' -F "$pattern" crates/clift-core/src | sed 's/^/        /'
    else
        pass "crates/clift-core/src is free of '${pattern}'"
    fi
done

# --- 3. No option that weakens SSH verification, anywhere in shipped code -----

printf '[3/10] SSH verification is never weakened\n'
for pattern in "${WEAKENING_PATTERNS[@]}"; do
    hits="$(grep -rn -F "$pattern" crates integrations scripts tests .github 2>/dev/null || true)"
    for entry in "${WEAKENING_ALLOWLIST[@]}"; do
        [ "${entry%%|*}" = "$pattern" ] || continue
        hits="$(printf '%s\n' "$hits" | grep -v "^${entry#*|}:" || true)"
    done
    hits="$(printf '%s' "$hits" | sed '/^$/d')"
    if [ -n "$hits" ]; then
        fail "'${pattern}' found outside the allowlist"
        printf '%s\n' "$hits" | sed 's/^/        /'
    else
        pass "no occurrence of '${pattern}'"
    fi
done

# --- 4. unsafe policy --------------------------------------------------------

printf '[4/10] unsafe policy\n'
for dir in crates/*/; do
    crate="$(basename "$dir")"
    root="${dir}src/lib.rs"
    [ -f "$root" ] || root="${dir}src/main.rs"
    if [ "$crate" = "clift-clipboard" ] || [ "$crate" = "clift-inject" ]; then
        # The two crates permitted to use unsafe, and only for the reason each
        # is named after: clift-clipboard reaches NSPasteboard through
        # Objective-C, clift-inject synthesises a keystroke through
        # CoreGraphics. Both must still deny unchecked unsafe inside unsafe fns.
        if grep -q '#!\[deny(unsafe_op_in_unsafe_fn)\]' "$root"; then
            pass "${crate} declares deny(unsafe_op_in_unsafe_fn)"
        else
            fail "${crate} must declare #![deny(unsafe_op_in_unsafe_fn)]"
        fi
    elif grep -q '#!\[forbid(unsafe_code)\]' "$root"; then
        pass "${crate} declares forbid(unsafe_code)"
    else
        fail "${crate} must declare #![forbid(unsafe_code)] in ${root}"
    fi
done

# --- 5. the error-to-exit-code mapping exists exactly once -------------------

printf '[5/10] exit code mapping is a single source of truth\n'
MAP_OWNER='crates/clift-core/src/exit.rs'
# Matching on `=> ExitCode::` would be wrong: the CLI legitimately returns the
# standard library's identically named ExitCode. What must exist only once is a
# table keyed on ErrorKind, and the function that consults it.
map_hits="$(grep -rln --include='*.rs' -E 'ErrorKind::[A-Za-z]+ *=> *ExitCode::' crates/*/src | grep -v "^${MAP_OWNER}$" || true)"
if [ -n "$map_hits" ]; then
    fail 'a second ErrorKind -> exit code table exists outside '"${MAP_OWNER}"
    printf '%s\n' "$map_hits" | sed 's/^/        /'
else
    pass "the ErrorKind mapping table lives only in ${MAP_OWNER}"
fi

definition_hits="$(grep -rln --include='*.rs' -E 'fn exit_code *\(' crates/*/src | grep -v "^${MAP_OWNER}$" || true)"
if [ -n "$definition_hits" ]; then
    fail "exit_code() is defined outside ${MAP_OWNER}"
    printf '%s\n' "$definition_hits" | sed 's/^/        /'
else
    pass "exit_code() is defined only in ${MAP_OWNER}"
fi

if grep -rn --include='*.rs' -E 'std::process::exit|process::exit\(' crates >/dev/null 2>&1; then
    fail 'process::exit bypasses the ExitCode contract'
    grep -rn --include='*.rs' -E 'std::process::exit|process::exit\(' crates | sed 's/^/        /'
else
    pass 'no call to process::exit'
fi

# --- 6. error causes are never discarded -------------------------------------

printf '[6/10] error cause chains are preserved\n'
# `map_err(|_| ...)` throws away the reason a call failed, which is what turns
# "SFTP subsystem missing" into "connection failed" for the user.
if grep -rn --include='*.rs' -E 'map_err\(\s*\|_' crates >/dev/null 2>&1; then
    fail 'map_err(|_| ...) discards the cause chain'
    grep -rn --include='*.rs' -E 'map_err\(\s*\|_' crates | sed 's/^/        /'
else
    pass 'no map_err(|_| ...) discarding a cause'
fi

if grep -rn --include='*.rs' -E 'ok\(\)\.ok_or|let _ = .*\?' crates >/dev/null 2>&1; then
    printf '  note  review these for discarded errors:\n'
    grep -rn --include='*.rs' -E 'ok\(\)\.ok_or|let _ = .*\?' crates | sed 's/^/        /'
fi

# --- 7. test fakes never reach shipped code ---------------------------------

printf '[7/10] test doubles stay out of the product\n'
# The specification forbids mock end-to-end tests. The fakes live behind clift-core's
# `testing` feature, so no crate that ships may switch it on.
feature_hits="$(grep -rn --include='Cargo.toml' -E 'clift-core.*features.*testing|features = \[.*"testing"' crates | grep -v '^crates/clift-core/Cargo.toml:' || true)"
if [ -n "$feature_hits" ]; then
    fail "a crate enables clift-core's testing feature"
    printf '%s\n' "$feature_hits" | sed 's/^/        /'
else
    pass "no crate enables clift-core's testing feature"
fi

if grep -rn --include='*.rs' -E 'clift_core::testing|use .*testing::(Fake|Recording)' crates/clift-cli/src crates/clift-clipboard/src crates/clift-transport/src >/dev/null 2>&1; then
    fail 'adapter or CLI source references the test fakes'
    grep -rn --include='*.rs' -E 'clift_core::testing' crates/clift-cli/src crates/clift-clipboard/src crates/clift-transport/src | sed 's/^/        /'
else
    pass 'no adapter or CLI source references the test fakes'
fi

# --- 8. stdout discipline ----------------------------------------------------

printf '[8/10] only machine results reach stdout\n'
# Whatever reaches stdout can be typed into an agent's prompt. A stray
# `println!` therefore does not look like a bug, it looks like Clift writing
# noise into someone's conversation. All output goes through clift-cli's
# Reporter, which is the only place allowed to touch stdout.
stdout_hits="$(grep -rn --include='*.rs' -E '(^|[^a-z_])(println!|print!)' crates/*/src || true)"
if [ -n "$stdout_hits" ]; then
    fail 'print!/println! writes to stdout outside the Reporter'
    printf '%s\n' "$stdout_hits" | sed 's/^/        /'
else
    pass 'no print!/println! in shipped source'
fi

writer_hits="$(grep -rln --include='*.rs' -E 'io::stdout\(\)' crates/*/src | grep -v '^crates/clift-cli/src/output.rs$' || true)"
if [ -n "$writer_hits" ]; then
    fail 'stdout is written outside crates/clift-cli/src/output.rs'
    printf '%s\n' "$writer_hits" | sed 's/^/        /'
else
    pass 'stdout is written only by crates/clift-cli/src/output.rs'
fi

# --- 9. the JSON contract is not the domain model -----------------------------

printf '[9/10] domain types are never serialized to the outside\n'
# A `Serialize` on a domain type turns every rename into a silent change to the
# JSON a third party depends on. The DTOs are hand written for exactly that
# reason, and the domain must have no way to bypass them.
serialize_hits="$(grep -rn --include='*.rs' -E 'derive\(.*Serialize|impl +Serialize' crates/clift-core/src/domain crates/clift-core/src/staging crates/clift-core/src/usecase 2>/dev/null || true)"
if [ -n "$serialize_hits" ]; then
    fail 'a domain, staging or use-case type derives Serialize'
    printf '%s\n' "$serialize_hits" | sed 's/^/        /'
else
    pass 'no domain, staging or use-case type is serializable'
fi

# --- 10. the relay client never touches key material -------------------------

printf '[10/11] the relay client cannot see a key\n'
# Universal Mode's entire security argument is that the relay holds ciphertext
# it cannot read. The port signatures are what enforce that, and this is the
# executable form: nothing in the relay client may name the key type, and no
# request-building code may interpolate one. The Cloudflare Worker is a second
# relay implementation in another language, so it is held to the same line.
key_hits="$(grep -rn --include='*.rs' --include='*.ts' -E 'SealKey|key\(\)|expose\(\)' crates/clift-relay/src crates/clift-relayd/src relay/cloudflare/src 2>/dev/null || true)"
if [ -n "$key_hits" ]; then
    fail 'the relay client, the daemon or the Worker references key material'
    printf '%s\n' "$key_hits" | sed 's/^/        /'
else
    pass 'neither the relay client, the daemon nor the Worker names a key'
fi

# A token is a key with a URL around it. It must not be logged, and the relay
# must never be sent one.
token_hits="$(grep -rn --include='*.rs' --include='*.ts' -E 'clift://' crates/clift-relay/src crates/clift-relayd/src relay/cloudflare/src 2>/dev/null || true)"
if [ -n "$token_hits" ]; then
    fail 'a token literal appears in the relay client or server'
    printf '%s\n' "$token_hits" | sed 's/^/        /'
else
    pass 'no token appears in the relay client or server'
fi

printf '[11/11] local paths and remote paths do not get mixed up\n'
# RemotePath is POSIX by definition: the far side is POSIX even when Clift runs
# on Windows, and its own documentation says local path semantics must never
# leak into it. The staging layer that writes on *this* machine used it anyway,
# which worked for as long as `clift fetch` only ever ran on Linux servers. The
# first Windows machine to redeem a token was told its `C:\Users\...` inbox
# "must be absolute". Nothing under staging/local may name the remote type.
# Comment lines are excluded: the file explains why it does not use the type,
# and an explanation must not be the thing that trips the check.
mixed="$(grep -n 'RemotePath' crates/clift-core/src/staging/local.rs 2>/dev/null | grep -vE '^[0-9]+:[[:space:]]*//' || true)"
if [ -n "$mixed" ]; then
    fail 'the local staging layer names RemotePath'
    printf '%s\n' "$mixed" | sed 's/^/        /'
else
    pass 'the local staging layer uses LocalPath, not RemotePath'
fi

printf '\n'
if [ "$FAILURES" -ne 0 ]; then
    printf 'architecture check FAILED (%d violation(s))\n' "$FAILURES"
    exit 1
fi
printf 'architecture check passed\n'

#!/usr/bin/env bash
#
# Starts and stops throwaway OpenSSH containers for Clift's integration tests.
#
# Everything about a fixture is disposable and self contained: a one-time client
# key, a one-time host key, a random loopback port and its own ssh_config. The
# developer's ~/.ssh is never read and never written.
#
# Host key verification stays on. The container's real public host key is read
# out of the running container and pinned into the fixture's own known_hosts,
# which is why no fixture ever needs to relax a verification setting.
#
# Usage:
#   sshd-fixture.sh available
#   sshd-fixture.sh build
#   sshd-fixture.sh start <topology> <workdir>
#   sshd-fixture.sh stop <workdir>
#
# Topologies: normal | no-sftp | readonly-home | small-quota | small-cache

set -euo pipefail

IMAGE=clift-sshd-fixture:3
DOCKER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
READY_TIMEOUT_SECONDS=60

die() { printf 'clift fixture: %s\n' "$1" >&2; exit 1; }

cmd_available() {
    command -v docker >/dev/null 2>&1 || die "docker executable not found"
    docker info >/dev/null 2>&1 || die "docker daemon is not reachable"
}

cmd_build() {
    cmd_available
    docker build --quiet --tag "$IMAGE" "$DOCKER_DIR" >/dev/null
}

ensure_image() {
    if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
        cmd_build
    fi
}

cmd_start() {
    local topology="$1" workdir="$2"
    case "$topology" in
        normal|no-sftp|readonly-home|small-quota|small-cache) ;;
        *) die "unknown topology '${topology}'" ;;
    esac

    cmd_available
    ensure_image

    mkdir -p "$workdir"
    chmod 0700 "$workdir"

    rm -f "$workdir/id_ed25519" "$workdir/id_ed25519.pub"
    ssh-keygen -q -t ed25519 -N '' -C clift-fixture -f "$workdir/id_ed25519"

    local -a run_args=(
        run --detach
        --publish 127.0.0.1::22
        --env "CLIFT_AUTHORIZED_KEY=$(cat "$workdir/id_ed25519.pub")"
        --env "CLIFT_TOPOLOGY=${topology}"
    )
    if [ "$topology" = "small-quota" ]; then
        # 1 MiB is small enough that a single test attachment overruns it, and
        # large enough that sshd itself is unaffected.
        run_args+=(--tmpfs /home/dev/quota:rw,size=1m,mode=0700)
    fi
    if [ "$topology" = "small-cache" ]; then
        # The same 1 MiB, but over the cache directory, so that Clift's own
        # inbox is the thing that runs out of room. Setup's self check writes a
        # few bytes and still succeeds.
        run_args+=(--tmpfs /home/dev/.cache:rw,size=1m,mode=0700)
    fi

    local container
    container="$(docker "${run_args[@]}" "$IMAGE")"
    printf '%s\n' "$container" >"$workdir/container"

    local port
    port="$(docker port "$container" 22 | head -n 1 | sed 's/.*://')"
    [ -n "$port" ] || die "container ${container} exposed no port"

    # The host key must exist before it can be pinned; ssh-keygen -A runs at the
    # very start of the entrypoint.
    local host_key='' deadline=$((SECONDS + READY_TIMEOUT_SECONDS))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if host_key="$(docker exec "$container" cat /etc/ssh/ssh_host_ed25519_key.pub 2>/dev/null)"; then
            [ -n "$host_key" ] && break
        fi
        host_key=''
        sleep 0.2
    done
    [ -n "$host_key" ] || die "container ${container} produced no host key in time"

    # known_hosts wants "type key", without the trailing comment.
    printf '[127.0.0.1]:%s %s %s\n' "$port" "$(echo "$host_key" | awk '{print $1}')" \
        "$(echo "$host_key" | awk '{print $2}')" >"$workdir/known_hosts"
    chmod 0600 "$workdir/known_hosts"

    cat >"$workdir/ssh_config" <<CONFIG
Host clift-fixture
    HostName 127.0.0.1
    Port ${port}
    User dev
    IdentityFile ${workdir}/id_ed25519
    IdentitiesOnly yes
    UserKnownHostsFile ${workdir}/known_hosts
    BatchMode yes
    ConnectTimeout 5
CONFIG
    chmod 0600 "$workdir/ssh_config"

    deadline=$((SECONDS + READY_TIMEOUT_SECONDS))
    local ready=0
    while [ "$SECONDS" -lt "$deadline" ]; do
        if ssh -F "$workdir/ssh_config" clift-fixture true >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.2
    done
    if [ "$ready" -ne 1 ]; then
        docker logs "$container" >&2 || true
        cmd_stop "$workdir" || true
        die "sshd in ${container} did not accept a connection in time"
    fi

    printf 'container=%s\n' "$container"
    printf 'port=%s\n' "$port"
    printf 'alias=%s\n' clift-fixture
    printf 'ssh_config=%s\n' "$workdir/ssh_config"
    printf 'identity=%s\n' "$workdir/id_ed25519"
    printf 'known_hosts=%s\n' "$workdir/known_hosts"
    printf 'remote_home=%s\n' /home/dev
}

cmd_stop() {
    local workdir="$1"
    [ -f "$workdir/container" ] || return 0
    docker rm --force "$(cat "$workdir/container")" >/dev/null 2>&1 || true
    rm -f "$workdir/container"
}

case "${1:-}" in
    available) cmd_available ;;
    build)     cmd_build ;;
    start)     [ $# -eq 3 ] || die "usage: sshd-fixture.sh start <topology> <workdir>"
               cmd_start "$2" "$3" ;;
    stop)      [ $# -eq 2 ] || die "usage: sshd-fixture.sh stop <workdir>"
               cmd_stop "$2" ;;
    *)         die "usage: sshd-fixture.sh {available|build|start <topology> <workdir>|stop <workdir>}" ;;
esac

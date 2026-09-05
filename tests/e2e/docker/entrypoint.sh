#!/bin/sh
#
# Prepares one throwaway sshd instance and hands the container over to it.
#
# CLIFT_AUTHORIZED_KEY - public half of the one-time key the test generated.
# CLIFT_TOPOLOGY       - which failure shape to build; see the case below.

set -eu

if [ -z "${CLIFT_AUTHORIZED_KEY:-}" ]; then
    echo "clift fixture: CLIFT_AUTHORIZED_KEY is required" >&2
    exit 1
fi

HOME_DIR=/home/dev

mkdir -p "$HOME_DIR/.ssh"
printf '%s\n' "$CLIFT_AUTHORIZED_KEY" >"$HOME_DIR/.ssh/authorized_keys"
chmod 0700 "$HOME_DIR/.ssh"
chmod 0600 "$HOME_DIR/.ssh/authorized_keys"
chown -R dev:dev "$HOME_DIR/.ssh"

# One-time host keys, generated after the image is built so that no two
# containers share an identity.
ssh-keygen -A >/dev/null

case "${CLIFT_TOPOLOGY:-normal}" in
    normal)
        ;;
    no-sftp)
        # Both halves are removed: a server can lack the subsystem declaration,
        # the binary, or both, and all three must surface as "SFTP unavailable"
        # rather than as a generic connection failure.
        rm -f /usr/lib/openssh/sftp-server
        sed -i '/^Subsystem/d' /etc/ssh/sshd_config.d/clift.conf
        ;;
    readonly-home)
        # Root-owned and not writable by dev. sshd's StrictModes accepts a
        # root-owned home, so the login still succeeds and the failure lands
        # where it belongs: on the first write.
        chown root:root "$HOME_DIR"
        chmod 0555 "$HOME_DIR"
        ;;
    small-cache)
        # The size limited filesystem is mounted over the cache directory, so
        # the inbox itself lands on it. Ownership is fixed up from in here.
        chown dev:dev "$HOME_DIR/.cache"
        chmod 0700 "$HOME_DIR/.cache"
        ;;
    small-quota)
        # The size limited filesystem is a tmpfs supplied by the runner; only
        # its ownership has to be fixed up from in here.
        chown dev:dev "$HOME_DIR/quota"
        chmod 0700 "$HOME_DIR/quota"
        ;;
    *)
        echo "clift fixture: unknown topology '${CLIFT_TOPOLOGY}'" >&2
        exit 1
        ;;
esac

exec /usr/sbin/sshd -D -e

# Real OpenSSH stderr

Every file here was captured from an actual `ssh` or `sftp` run against the
throwaway container in `tests/e2e/docker/`, on 2026-08-30, with
`OpenSSH_9.9p2, LibreSSL 3.3.6` on macOS 15.7.7 and `openssh-server` from
Ubuntu 24.04. None of it is written by hand: the specification requires these to be
collected from real execution, because a mapping tuned against invented text
only proves the invention was consistent.

| File | How it was produced |
| --- | --- |
| `auth-publickey.txt` | connect with a key the server does not authorise |
| `host-key-unknown.txt` | connect with an empty `known_hosts` |
| `host-key-changed.txt` | connect with another server's key pinned for this host |
| `connection-refused.txt` | connect to a port nothing listens on |
| `sftp-subsystem-missing.txt` | `sftp` against the `no-sftp` topology |
| `remote-permission-denied.txt` | `mkdir` inside a directory the account cannot write |
| `remote-no-such-file.txt` | `mkdir` under a parent that does not exist |
| `no-space-left.txt` | `put` a 3 MiB file onto the 1 MiB filesystem of the `small-quota` topology |
| `remote-mkdir-exists.txt` | `mkdir` a directory that is already there (captured 2026-09-02) |

## The one edit

`host-key-changed.txt` names the `known_hosts` file twice, and the capture ran
from a path under the developer's home directory. Both occurrences were
replaced with the literal `<known_hosts>`. Nothing else in any file was
touched.

## What `no-space-left.txt` shows

It says `write remote "...": Failure`, not "No space left on device". SFTP
protocol version 3 has no error code for a full disk, so the server reports
`SSH_FX_FAILURE` and the client can only print `Failure`. Clift therefore
cannot tell a full disk apart from other write failures over SFTP, and must not
claim otherwise. This is the evidence behind that decision.

## What is missing, and why

There is no fixture for `Could not resolve hostname`. This machine's DNS
resolves every unknown name to an address in `198.18.0.0/15`, inside the
container as well, so the message cannot be produced here. No matcher was
written for text that could not be observed.

# `ssh -G` fixtures

## Where this came from

`effective.txt` is the **real** output of

```sh
ssh -F <config> -G demo-core
```

run on macOS with `OpenSSH_9.9p2, LibreSSL 3.3.6` (2026-08-30), against a
throwaway config file containing only:

```sshconfig
Host demo-core
    HostName 192.0.2.10
    Port 2222
    User dev
```

`192.0.2.0/24` is the documentation range from RFC 5737, so nothing here points
at a real machine.

## The only edit

The developer's home directory was replaced with `<home>` on the
`userknownhostsfile` line. Nothing else was changed, added or removed: the 85
lines are what OpenSSH printed, including the seven `identityfile` lines.

**Those lines are the point.** `nothing_about_a_private_key_survives_the_parse`
asserts the fixture still contains them and that none of them reach any value
Clift can print. Trimming them to make the fixture tidier would delete the
evidence the test depends on.

## The multiplexed case

`multiplexed.txt` is the **real** output of the same command with the three
options Clift generates added:

```sh
ssh -F <config> -G -o ControlMaster=auto \
    -o 'ControlPath=/Users/x/.cache/clift/run/%C' -o ControlPersist=10m demo-core
```

macOS, `OpenSSH_9.9p2, LibreSSL 3.3.6`, 2026-09-01. Two facts in it are what
`connection_reuse.rs` is written against, and neither was guessed:

- **`controlpath` appears only when it is set.** `effective.txt` has no such
  line at all, which is how "the user does not multiplex this host" looks.
- **`%C` expands to a 40 character hex digest**, and `controlpersist` is
  printed in seconds (`600`), not as the `10m` that was passed in.

The same `<home>` substitution was made on the `userknownhostsfile` line, and
`/Users/x` in the control path was typed that way when the fixture was
collected rather than edited afterwards.

## What is not captured here

`ssh -G` output for the real `core` and `hk` hosts is **not** committed: it
contains the developer's own paths. Those two are covered by
`crates/clift-transport/tests/real_hosts.rs`, which reads them live and is
gated on `CLIFT_REAL_HOSTS`.

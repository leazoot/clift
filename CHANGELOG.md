# Changelog

All notable changes to this project are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Nothing yet.

## [0.1.0] - 2026-09-04

First public release.

### Added

**Universal Mode**: any terminal, any number of servers.

- `clift paste [--copy|--inject]` seals the attachment locally with
  XChaCha20-Poly1305, leaves the ciphertext with a relay that cannot read it,
  and hands back one short token. Whichever SSH session the token is pasted
  into is the host that gets the file, because only that host can redeem it.
  No target, no terminal adapter. `--inject` types the line into the focused
  window one character at a time, so whatever you had copied is still on the
  clipboard afterwards, byte for byte.
- `clift fetch '<token>'` redeems a token on the host that received it, writes
  the attachment into that host's inbox, and prints the absolute path. Single
  use: the relay deletes the object as it hands it over.
- `clift copy <file>...`, the return trip. On the machine an attachment is
  coming *from*, it seals the file, leaves the ciphertext with the relay and
  prints one bare token. Copy that line out of your terminal the way you copy
  anything, and press your Clift key at home. When you can reach the machine
  directly, `scp` is still the shorter answer and this says so.
- `clift fetch --copy` redeems a token onto this machine's clipboard, in every
  form the attachment has: the file itself, so a paste in Explorer or Finder
  makes a copy of it; the pixels, when it is a picture whose bytes really do
  begin like a PNG; and text, which is the attachment's own content for text
  files and its path for everything else. A batch of several offers the folder
  holding them, so one paste produces all of them rather than an arbitrary one.
- `clift hotkey [--install|--uninstall]`: one key combination registered with
  the operating system that does what `clift paste` does, in any application.
  A single press works in both directions, chosen by what is on the clipboard:
  an image goes out, a token that came back off a server's terminal comes in,
  and ordinary text does nothing at all. Only a token on its own counts, so
  pressing the key twice cannot spend the object the first press created.
  `--install` registers the helper to start hidden at login on macOS and
  Windows.
- `clift-relayd`: a self-hostable relay. Ciphertext in memory, single-use
  delivery, short TTL, per-source rate limiting, a total-size ceiling and a
  health endpoint. No disk, no accounts, no way to decrypt.
- The same relay as a **Cloudflare Worker** (`relay/cloudflare/`), for people
  with no machine to run the daemon on: one Durable Object, a free account, a
  "Deploy to Cloudflare" button. It passes the same contract tests as the
  daemon, against the real `workerd` runtime.
- Exit codes **27** (token unusable), **28** (relay unreachable) and **29**
  (integrity failure), alongside the Fast Mode codes and never renumbered.
- `mode`, `[relay]` and `[hotkey]` in the configuration, with
  `CLIFT_RELAY_URL`, `CLIFT_RELAY_MAX_BYTES` and `CLIFT_RELAY_TTL` overriding
  the relay section.

**Fast Mode**: one server, your own SSH.

- `clift setup <ssh-host>` verifies a host end to end (SSH, SFTP, a private
  inbox, a real upload that is then removed) and records it. Nothing is written
  to the configuration unless every check passed.
- `clift send [files...] [--clipboard] [--to] [--copy]` sends attachments over
  the user's own `ssh` and `sftp` and prints the text to paste. A batch arrives
  whole or not at all. The target comes from `--to` or from the default target,
  and a send refuses when neither says which: Clift never guesses a host, and
  never picks the one you used last.
- `clift target add|list|use|rename|remove` manages targets.
- One SSH connection per host and one SFTP session per run: connection reuse
  through OpenSSH `ControlMaster` in a private directory, and every SFTP
  operation of a run over a single `sftp` process. A user's own `ControlMaster`
  settings are never overridden.

**Both modes.**

- `clift setup` with no host is a first-time conversation: which mode, which
  relay (checked with a real sealed round trip before it is saved, with the
  refusal and a proxy hint when it cannot be reached), whether to register the
  key at login, which combination, and what to run on each server. Both
  installers start it when there is a terminal to talk to; `--no-setup` or
  `CLIFT_NO_SETUP=1` installs only. Without a terminal, or under `--json`, it
  refuses at once and names `clift config set` instead of waiting on a question
  nobody will answer.
- `clift doctor [target]`: thirteen checks; one failure does not stop the
  others, and every failure carries exactly one command to run.
- `clift status`, `clift clean [--all] [--older-than] [--dry-run]`,
  `clift uninstall [--purge] [--dry-run]`, `clift config path|get|set|validate`.
- A versioned JSON contract (`schema_version: 1`) on every machine-readable
  output, with a field-by-field compatibility test.
- The clipboard is read on macOS (screenshots, JPEG, and TIFF converted
  losslessly to PNG by the system framework, plus files copied as file URLs)
  and on Windows (a bitmap encoded as PNG with GDI+, which every Windows
  installation has; an application's own `PNG` bytes passed through untouched;
  copied files as a file list). Plain text is exit code 10 and nothing else.
- `install.sh` (macOS, Linux) and `install.ps1` (Windows): one-line installers
  that download a release archive together with its `SHA256SUMS`, install
  nothing unless the digest matches, and never ask for sudo. A release, a
  directory and `clift-relayd` can be chosen by flag or environment variable.
- `cargo binstall` metadata on `clift-cli` and `clift-relayd`, so
  `cargo binstall --git https://github.com/leazoot/clift clift-cli` fetches the
  release archive instead of compiling.
- `integrations/agents/clift.md`: a paragraph to append to an agent's
  instructions file so that it runs the pasted `clift fetch` once, reads the
  path, and hands the user Clift's own explanation when it fails.
- `integrations/claude-code/clift-hook.sh`: registered as a `UserPromptSubmit`
  hook, it redeems any Clift token in the prompt when Enter is pressed and
  tells Claude the file's path, so the attachment is read straight away instead
  of after a turn spent running the command. A failed fetch is handed to Claude
  as an explanation for the user; the prompt is never blocked and the token is
  never printed.
- A "Terminal report" issue form for terminals and SSH clients people try.
- Releases are signed with Sigstore, keyless: every published file gets a
  `.sigstore.json` bundle from the release workflow's own identity, and the
  workflow verifies them before publishing. `SECURITY.md` shows the check.
- Two CycloneDX 1.5 SBOMs per release, for `clift` and `clift-relayd`,
  generated from `Cargo.lock` across every target.
- Dependabot for Cargo, GitHub Actions and the Worker's npm toolchain, weekly,
  grouped.
- Static Linux binaries (`*-unknown-linux-musl`). A glibc build carries the
  glibc version of the machine it was built on (one made on Ubuntu 24.04 needs
  `GLIBC_2.39` and does not start on Debian 12); a static binary starts on any
  Linux of its architecture, Alpine included.

### Security

- Every attachment gets a fresh 256-bit key and a fresh 192-bit nonce from the
  OS CSPRNG, with no fallback of any kind. Every byte of the envelope is
  authenticated, header included.
- The relay receives the object id and nothing else. The port it is written
  against does not accept a key, and a build check asserts that neither relay
  crate nor the Cloudflare Worker can name the key type.
- A failed fetch writes nothing and prints nothing. An unreachable relay is an
  error, never a reason to fall back to a default host.
- No option is ever passed to `ssh` or `sftp` that weakens host verification;
  the only options Clift passes name a control socket, and a build check
  asserts the whole repository never mentions the weakening ones.
- Private keys are never read; no SSH library is linked; no credential store
  exists, and the configuration schema has nowhere to put one.
- Remote paths are never interpolated into a shell command. Uploads go to a
  random `.part`, are size-checked, and only then renamed. Directories are
  `0700`, files `0600`, batch names come from 128 bits of OS randomness.
- Cleanup refuses to leave the inbox, refuses to follow symbolic links, and
  refuses to delete anything whose age it cannot establish.
- Temporary files are `0600` in a private directory and are removed on normal
  return, on panic, and on `Ctrl+C`.

### Known limitations

- The clipboard is read on macOS and Windows only. On Linux the reader returns
  an explicit "not implemented" error; `clift fetch`, `clift copy` and the
  relay work anywhere the binary builds.
- Offering a redeemed attachment to a file manager is covered by tests on the
  bytes Clift builds for it, and the Windows calls that hand those bytes to the
  system have not themselves been run on Windows.
- Only the terminals on the maintainer's own machines have been tried.
- The local inbox that `clift fetch` writes into has no cleanup command of its
  own; `clift clean` cleans a remote target.
- Between `mkdir` and `chmod`, a directory Clift creates wears the remote
  umask for one round trip. The reachable case is the first creation of
  `~/.cache/clift`, which is empty at the time. `THREAT_MODEL.md` says so.
- On the Cloudflare Worker, a delivery that starts is a delivery: the object is
  consumed when handed over, where the daemon would restore it after a dropped
  connection. Neither ever delivers twice.

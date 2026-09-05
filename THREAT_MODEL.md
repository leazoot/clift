# Threat model

Clift introduces **no new authorisation layer**. It inherits the operating
system's permissions and OpenSSH's trust relationships exactly as they are. The
work of securing it is therefore not "implement authentication" but "do not
break what is already there, do not widen the attack surface, and do not leak
the user's data".

This document says what that means, and, just as importantly, what Clift does
**not** defend against.

---

## What Clift touches

| Boundary | What Clift does |
| --- | --- |
| Local clipboard | Reads it **once**, when the user asks it to send something. No watcher, no cache, no history |
| Local files | Reads with the current user's permissions. Unreadable means refused, never escalated |
| SSH identity | Entirely the system OpenSSH client's. Clift **never reads a private key**, stores a credential, or links an SSH library |
| Host identity | `known_hosts`, unchanged. A changed host key **fails** |
| Target authorisation | `--to` may only name a configured target |
| Remote storage | A private directory under the user's home, `0700` directories and `0600` files. No root, no sudo |
| Network | Fast Mode: between this machine and the SSH hosts the user configured. Universal Mode: additionally, between this machine and the relay the user configured, and between the remote host and the same relay. Nowhere else |
| Third-party services | Fast Mode: **none**. Universal Mode: **one relay**, which sees only ciphertext, and which the user may run themselves |
| Telemetry | **None** |
| Accounts | **None**, on either side |

---

## Universal Mode: what the relay can and cannot see

This is the one part of Clift where data passes through a machine the user may
not own, so it is worth being exact rather than reassuring.

**The relay can see:**

- how many bytes of ciphertext were stored, and when;
- when an object was fetched, and when it expired unfetched;
- the IP address of whoever uploaded and whoever downloaded;
- therefore: that somebody at address A sent roughly this much data to somebody
  at address B, at this time.

**The relay cannot see:**

- the attachment's contents, name, or media type: all of them are inside the
  sealed frame;
- which host will fetch it, or which host did, beyond an IP address;
- the key. There is no field in any request that carries one. The key lives in
  the URL fragment of the token, which by long convention is never sent to a
  server, and the port the client is written against does not accept a key
  argument at all; `scripts/check-architecture.sh` asserts the relay crates
  cannot even name the type.

**What that means in practice.** If the traffic analysis above is unacceptable,
there are two answers and both are supported: run the relay yourself (it is one
binary, no database, no state on disk), or use Fast Mode, which has no relay.

**A token is a bearer credential.** Anyone who obtains one before it is redeemed
can fetch the attachment. Three things bound that: the object is single use, so
whoever gets there first is the only one who gets anything; the default lifetime
is five minutes; and the token exists, in the clear, only in the user's own
terminal and shell history. Clift does not write it anywhere else: not to a
log, not under `--debug`, not into the configuration file. **The copy in the
user's shell history is the user's shell's decision, not Clift's**, and it is
worth knowing about: an old token in `~/.bash_history` is not usable after five
minutes, but for those five minutes it is.

---

## What Clift refuses to do

These are not defaults; there is no flag that changes them.

- It never passes `StrictHostKeyChecking=no`, `UserKnownHostsFile=/dev/null`, or
  any other option that weakens verification. In fact it passes **no `-o`
  option at all**, which is a stronger guarantee than a blacklist. A build-time
  check asserts this across the whole repository.
- It never suggests turning host key verification off. Not even
  `ssh-keygen -R`: that is a legitimate thing to do *after* verifying a key
  change, but a tool that volunteers it is teaching people how to make the
  warning go away.
- It never builds a remote shell command string containing a user-controlled
  path. Remote commands are `&'static str` in the type system, so a `format!`
  result cannot be passed to one; anything involving a path goes through SFTP
  with parameters.
- It never opens, executes, parses or unpacks an attachment.
- It never follows a symbolic link out of the inbox, and never deletes outside
  it.
- It never writes to the clipboard except on `--copy`, and then only after a
  successful send, and then only paths or a token, never an attachment's
  contents. `--inject` types the instruction into the focused window and leaves
  the clipboard holding whatever you copied.
- It never intercepts a keystroke. `--inject` **sends** keystrokes when asked,
  and hooks nothing; the user's own paste key is untouched.
- It never reports an injection that did not happen. Without the platform
  permission it says so, says how to grant it, and falls back to `--copy`.
- It never falls back to a different host. If the relay is unreachable,
  Universal Mode fails; it does not quietly upload to the default target
  instead.

---

## What Clift does **not** defend against

Stated in the same words in the README, because it matters more than any of the
above:

1. **A malicious process running under the same Unix account.** It can read the
   clipboard, the temporary files, the configuration and the attachments. Clift
   is not a sandbox and cannot become one.
2. **The remote system's administrator.** Files are `0600` and directories
   `0700`, which makes them private to the user's account. Root reads them
   anyway.
3. **A compromised remote host.** Clift makes attachments reachable there
   because that is the entire point.
4. **Anything on a shared account.** If several people use one login, they can
   all read each other's attachments. Do not use Clift there.
5. **The instant between `mkdir` and `chmod`.** The `sftp` client has no way to
   create a directory with a mode, so every directory Clift makes exists for
   one round trip with the remote umask, typically `0775`. Batch and date
   directories are inside an inbox that is already `0700`, which closes the
   window before it opens; the first creation of `~/.cache/clift` is the one
   reachable case, and the directory is empty then. Closing it would mean
   linking an SSH library or running a remote shell command, both of which
   this document refuses above.
6. **Bytes a server sends back.** `clift copy` on a remote machine and `clift
   fetch --copy` at home reverse the direction, and the picture that reaches
   the local clipboard came from the far end. The relay cannot substitute it:
   altering one byte of the ciphertext makes the whole object fail to
   authenticate, and the relay never holds the key. What is not defended
   against is a *server that is already hostile* sending something other than
   what the user expected, and the local clipboard then offering it to the next
   application as an image. Clift checks the PNG signature before making that
   claim, which stops an accident rather than an attacker. The trust boundary
   is unchanged: it is the same machine the user already runs an agent on.

Clift is a transport for files the user has already decided to put on a machine
they already trust enough to run an agent on, and, in the return direction, to
take back from it.

---

## Where the interesting failures live

The parts most likely to cause harm if they were wrong, and what holds them:

| Risk | What holds it |
| --- | --- |
| An attachment lands on the **wrong host**, Fast Mode | Target resolution refuses when it cannot decide. A test asserts the most recently used host is never chosen, with `last_success_at` present in the configuration to make the temptation real |
| An attachment lands on the **wrong host**, Universal Mode | Clift does not choose one. The ciphertext is fetched by whichever host received the token, which is a fact about where the user's keystrokes went. A test replaces `ssh`, `sftp` and `scp` on `PATH` with recording scripts and asserts none of them runs, so "no host was contacted" is proved by the absence of an execution rather than by Clift's own account of itself |
| An **object is fetched twice** | The relay removes it before writing the body out, so two simultaneous requests cannot both win. Eight threads race for one object in a test against a real relay process; exactly one gets it |
| A **tampered object** is written to disk | Every byte of the envelope is authenticated, header included. A test flips each byte of a sealed object in turn and asserts all of them fail. A failed decrypt writes nothing; the test asserts the inbox directory was never even created |
| A **half-written file** is read by the agent | Upload goes to a random `.part`, the size is verified, and only then is it renamed. The size check takes no path argument, so a failed upload cannot name a file that is about to be deleted |
| A **partial batch** is presented as complete | The success type has no partially staged variant. One failure means no paths at all |
| **Cleanup deletes too much** | Four refusals: outside the inbox, a symbolic link, not a directory, or no modification time. Each has a test, and the boundary is re-checked against paths the *host* produced |
| A **screenshot is left on disk** | Temporary files are `0600`, in a private directory (never `/tmp`), removed by RAII and by a signal handler, because `Ctrl+C` runs no destructors |
| **Terminal configuration is damaged** | A marked block, a backup, an atomic replacement with rollback, and an apply-then-revert round trip asserted byte for byte against fixtures |

---

## Supply chain

- `Cargo.lock` is committed.
- `cargo deny check` covers licences and security advisories.
- Dependencies are few, and each was added with a written justification.
- Releases are signed with Sigstore, keyless: the signer is the release
  workflow's own identity, so there is no signing key that could be stolen,
  and a signature proves "built by this repository's workflow at this tag"
  and nothing more. Each release ships a CycloneDX SBOM.
- Dependabot proposes dependency updates; nothing is merged without CI and a
  person.

## Reporting a problem

See `SECURITY.md`.

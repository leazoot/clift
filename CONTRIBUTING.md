# Contributing

Clift has a narrow scope on purpose, and the fastest way to have a change
accepted is to know which side of the line it falls on.

## What Clift is

A bridge that carries attachments from a local clipboard to a private directory
on a remote host, over the SSH the user already has, and hands the agent a path.

Five things are not negotiable:

1. **Attachment-first.** Images, files, several files at once, not a screenshot
   workaround.
2. **Agent-agnostic.** Clift produces a path. It does not model any agent's
   internals.
3. **No remote stack.** Nothing is installed on the remote host: no binary, no
   daemon, no port, no `xclip`, no `Xvfb`, no `sudo`.
4. **Paste-preserving.** Plain text pastes exactly as it would without Clift.
5. **Open integration surface.** A versioned JSON contract, a formatting
   profile, a terminal adapter.

A change that breaks one of these will not be accepted however well it is
written.

## What will not be accepted

- A "support" claim without a real verification path behind it. A pull
  request that says a platform or terminal works has to say what was run, and
  where.
- Anything that weakens SSH verification, including as an opt-in flag.
- A remote helper in the core transport path.
- Telemetry, version checks, crash reporting, or any outbound network call other
  than to the user's own SSH hosts.
- A dependency that duplicates something already there, or a large one pulled in
  for a small function.

### Why not the terminal's own clipboard protocol?

kitty's clipboard protocol (OSC 5522) lets a program on the far end of an SSH
session read the laptop's clipboard, images included, through the terminal
itself, with no relay and no key. It works: a PNG of several hundred kilobytes
arrives on the remote host byte for byte in well under a second on a nearby
link, and it even works inside tmux once `allow-passthrough` is on and the
request is wrapped for it. It is not Clift's path, for reasons that are about
reach rather than quality:

- **One terminal.** kitty implements it. Ghostty has an open pull request;
  WezTerm, iTerm2, Windows Terminal and MobaXterm answer clipboard reads with
  nothing. Universal Mode's whole point is that the terminal does not matter.
- **A dialog per read.** The terminal must ask the user before handing over the
  clipboard, every time, unless a password is stored for the program. That is
  correct and it is also a second interaction.
- **The far end needs kitty's terminfo.** With the default `TERM=xterm-kitty`,
  tmux on the remote host refused to start at all until `TERM` was changed.
- **Through tmux it needs cooperation.** Default tmux swallows both the
  request and the reply; it takes `allow-passthrough on` and a DCS-wrapped
  request.

A kitty-specific path could exist one day as an addition for people who
already have kitty. It could never be a requirement.

## Before you write code

Read, in this order:

1. `THREAT_MODEL.md`: what Clift refuses to do, and why. Most design
   questions are answered there.
2. `scripts/check-architecture.sh`: the boundaries the build enforces. Each
   check has a comment saying what it protects. `clift-core` never does IO and
   never depends on an adapter crate; adapters never depend on each other;
   nothing weakens SSH verification; the relay code cannot name a key.

A change that adds a platform or a terminal comes with what was run to check
it. A terminal you merely *tried* is worth reporting too: the "Terminal
report" issue form asks the right questions.

## Running the checks

```console
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace
$ ./scripts/check-architecture.sh
$ cargo deny check
```

Some tests need more than a compiler:

| Environment variable | What it enables | Why it is opt-in |
| --- | --- | --- |
| `CLIFT_E2E_REQUIRE_DOCKER=1` | Fails instead of skipping when Docker is missing | The container tests are the real ones; a silent skip is worse than a failure |
| `CLIFT_REAL_CLIPBOARD=1` | Tests that read and write the **real** clipboard | They overwrite whatever you had copied |
| `CLIFT_REAL_HOSTS=<alias,...>` | Tests against real SSH hosts from your `~/.ssh/config` | They write a few bytes into those hosts' inboxes |
| `CLIFT_E2E_REQUIRE_WRANGLER=1` | Fails instead of skipping when the Cloudflare Worker cannot start | Needs `npm install` in `relay/cloudflare/`; the Worker must pass the same contract as the daemon |

A skipped test says so, loudly, and says that it proved nothing.

## What tests are expected to look like

- **A failure path for every rule.** Testing only the happy path is not
  coverage.
- **Real objects for real claims.** A FIFO test makes a FIFO; a symlink test
  makes a symlink; an SFTP test talks to a real SFTP server. Mocking the
  transport and calling it end to end is explicitly forbidden.
- **Fixtures captured, not invented**, wherever the point is what some other
  program really does. `tests/fixtures/ssh-stderr/README.md` shows the standard;
  where a fixture is constructed, its README says so and why.
- **Reverse verification for guards.** A check that has never failed has not
  been checked.

## Commits

Short, imperative, `<type>: <what changed>`, 72 characters or fewer in the
subject. Types: `feat`, `fix`, `refactor`, `style`, `test`, `docs`, `build`,
`chore`.

Say what changed and why it had to. "update code" says neither.

## Cutting a release

1. Bump `version` in the workspace `Cargo.toml`, add the section to
   `CHANGELOG.md`, commit.
2. Tag it `v<version>` and push the tag. The release workflow refuses a tag
   that does not match `Cargo.toml`, builds every target natively, generates
   the two SBOMs, signs every file with Sigstore (keyless; no secret to set
   up), verifies its own signatures, and publishes the lot with one
   `SHA256SUMS`.
3. Once the release exists, run `./scripts/update-packaging.sh <version>` and
   commit the regenerated `packaging/homebrew/clift.rb` and
   `packaging/scoop/clift.json`. The formula is copied verbatim into the
   Homebrew tap.

`install.sh`, `install.ps1` and the `binstall` metadata in the two binary
crates all derive the archive names from the version and the target, so they
need no change per release. If the naming in `release.yml` ever changes, all
four must change with it.

## Licence

By contributing you agree your work is licensed under Apache-2.0.

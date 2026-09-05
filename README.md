<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
  <img src="assets/logo-light.png" alt="Clift" width="360">
</picture>

**Paste a screenshot into the coding agent running on your server.**

Copy on your laptop, paste in the SSH session, and the agent reads the file.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://github.com/leazoot/clift/actions/workflows/ci.yml/badge.svg)](https://github.com/leazoot/clift/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/leazoot/clift?include_prereleases)](https://github.com/leazoot/clift/releases)

English · [简体中文](README.zh-CN.md)

</div>

---

You run Claude Code, Codex or another CLI agent over SSH. You take a screenshot.
The clipboard is on your laptop, the agent is on a server, and `Cmd+V` pastes
nothing useful. Clift closes that gap without touching your SSH setup, without
a daemon on the server, and without caring which terminal you use.

```console
$ clift setup core              # your ~/.ssh/config alias; checks SSH and SFTP, then remembers it
$ clift hotkey --install        # one key combination, registered at login

$ # take a screenshot, press the key in the SSH session, and this appears:
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

The agent reads the path. Nothing was installed on the server, no relay is
involved, and the file went over the `ssh` and `sftp` you already have.

## Two ways to get there

| | **Fast Mode** | **Universal Mode** |
| --- | --- | --- |
| Good for | One server you have already set up | Any number of servers, including ones you have not configured |
| Which server? | The one you configured | Whichever session you paste into |
| Path | Your own SSH and SFTP, directly | Encrypted, through a relay that only sees ciphertext, then `clift fetch` |
| On the server | Nothing at all | The same `clift` binary, run once per paste |
| Before the first paste | `clift setup <ssh-host>` | The same, plus a relay you run or deploy |

Both are driven by the same key combination and both leave plain text alone.
Start with Fast Mode: it is two commands and touches nothing you do not already
have. Move to Universal Mode when one configured host stops being enough, which
is the moment you stop wanting to tell Clift which machine you mean.

## Quick start

### 1. Install on your laptop

One line on macOS or Linux:

```console
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh
```

or on Windows, in PowerShell:

```powershell
PS> irm https://raw.githubusercontent.com/leazoot/clift/main/install.ps1 | iex
```

Both download the release archive together with its `SHA256SUMS`, install
nothing unless the digest matches, and never ask for sudo. The installer then
starts `clift setup`, which asks which mode you want and takes it from there:
an SSH host alias for Fast Mode, or a relay address for Universal Mode, checked
with a real round trip before it is saved. Answering the questions does the
next two steps for you; the commands are here so that you can see what they
were. Run `clift setup` again any time.

Other ways in: `brew install leazoot/clift/clift`, `cargo binstall --git
https://github.com/leazoot/clift clift-cli`, the
[Releases](https://github.com/leazoot/clift/releases) page, or
`cargo build --release` with Rust 1.95 or newer. A Scoop manifest is in
[`packaging/`](packaging/). Add `--no-setup` (or set `CLIFT_NO_SETUP=1`) to
install without the questions.

### 2. Name one server

```console
$ clift setup core
```

`core` is an alias from your own `~/.ssh/config`. Clift shows the user, host and
port it resolved to, waits for you to agree, then checks SSH, checks SFTP,
creates a private inbox and uploads a file it deletes again. Nothing is written
to the configuration unless all of that passed, and the first host you set up
becomes the default.

### 3. Register the key

```console
$ clift hotkey --install
```

On macOS and Windows the helper starts at login and runs hidden; no terminal
has to stay open. macOS will ask for Accessibility permission the first time,
because typing into another application is what the permission is for.

### 4. Paste

Take a screenshot, then press the key in the terminal that is talking to your
server (`Cmd+Shift+V` on macOS, `Ctrl+Alt+V` elsewhere, unless you changed it).
One line is typed into the session:

```text
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

The agent reads it. Plain text on the clipboard is left alone: press the key
with text copied and your terminal pastes it as it always did.

That is the whole of Fast Mode. The rest of this page is what to do when one
server is not enough.

## Any number of servers: Universal Mode

Fast Mode sends to the host you configured. Universal Mode sends to whichever
SSH session you paste into, which means you never tell Clift which machine you
mean and you never configure the machines at all. The cost is a relay, and one
install per server.

### 1. Get a relay

A relay holds the encrypted attachment for a few minutes and cannot read it.
Run your own with `clift-relayd`, or deploy one to a free Cloudflare account
in one click:

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/leazoot/clift/tree/main/relay/cloudflare)

Give its address to `clift setup` on your laptop, or later with
`clift config set relay.url https://clift-relay.<you>.workers.dev`.

### 2. Set up each server

**Recommended: let the agent do it.** The agent that will receive your
screenshots can install and configure Clift itself. Paste this into it, with
your relay's address filled in:

```text
Set up Clift on this server so I can paste screenshots to you.
RELAY_URL: https://clift-relay.<you>.workers.dev
Follow https://raw.githubusercontent.com/leazoot/clift/main/install.md exactly:
fetch it, work through its TODO list in order, stop and show me the error if a
step fails, and report as its last step says.
```

[`install.md`](install.md) is the guide it follows: install without sudo,
point at the relay, run `clift doctor`, and add a short paragraph to its own
instructions file so that it knows what to do whenever a token arrives. It is
written to be read by a person too, and it is worth reading once, because it is
the list of commands your agent will run. You can also hand it over directly:

```console
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.md | claude
```

**By hand.** Three commands on the server:

```console
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh -s -- --no-setup
$ clift config set relay.url https://clift-relay.<you>.workers.dev
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> CLAUDE.md
```

The last line appends the paragraph that tells the agent how to handle a
token, a spent token, or a missing relay. Use `AGENTS.md`, `GEMINI.md`, or
whichever file your agent reads for standing instructions. A token carries the
object and the key but never the relay's address, which is why every server is
told the address once. Claude Code can go one step further: a
[hook](integrations/claude-code/README.md) fetches the attachment the moment
you press Enter, so Claude reads the file instead of first deciding to run the
command.

### 3. Paste

Take a screenshot, then in the terminal that is talking to your server press
the key you chose in setup (`Cmd+Shift+V` on macOS and `Ctrl+Alt+V` on Windows
unless you changed it), or run `clift paste --copy` and paste. One line lands
in the session:

```text
Attachment: clift fetch 'clift://v1/…'
```

The agent runs it and gets the file. No target to choose, no `ssh` config to
edit, no plugin to install.

### 4. Bring something back

The same key works the other way. On the server, name a file:

```console
$ clift copy build/report.png
clift://v1/…
```

Select that line and copy it the way you copy anything in your terminal, then
press the key at home. The picture is on your clipboard, ready to paste
anywhere.

This is for the case where you cannot reach the server from where you are
sitting. When you can, `scp` is shorter and Clift will tell you so.

## How Universal Mode works

```text
 laptop                        relay                         server
 ──────                        ─────                         ──────
 clipboard ──seal──▶ ciphertext ──▶ stored 5 min ──▶ ciphertext ──open──▶ inbox/
             key ─────────────────────────────────────────────▶ key
                     (inside the token you paste; never sent to the relay)
```

- Every attachment is sealed with a fresh **XChaCha20-Poly1305** key and nonce.
- The relay stores bytes it cannot read, returns them **exactly once**, and forgets them.
- The key travels in the token's fragment (`#…`), the part of a URL that is
  never sent to a server.
- `clift fetch` decrypts, checks every byte, and writes the file `0600` into a
  `0700` directory. If anything fails it writes nothing and says why.

Plain text is never touched: paste it and it pastes as it always did.

## How this compares

Several tools solve the same first problem. They differ in what they ask of you
and in how far they go.

| | Clift | [clipssh](https://github.com/samuellawrentz/clipssh) | [clipaste](https://github.com/hqhq1025/clipaste) | [claude-ssh-image-skill](https://github.com/AlexZeitler/claude-ssh-image-skill) |
| --- | --- | --- | --- | --- |
| Watches your clipboard | No, reads it once when you press the key | No | Yes, a background service | No |
| Servers per setup | Any number, unconfigured, in Universal Mode | The host you configure | The host you configure | The host you configure |
| On the server | Nothing in Fast Mode | Nothing | Nothing | A client binary |
| Network shape | Your SSH, or a relay that holds ciphertext | Your SSH | Your SSH | A reverse tunnel to a local daemon |
| Brings files back | Yes, `clift copy` | No | No | No |
| Tied to one agent | No | No | No | Claude Code |

Where Clift loses: if you have one Mac and one server and want the shortest
possible install, a tool that watches the clipboard and rewrites it is one
`brew install` and no configuration, and Clift is two commands. Universal Mode
also asks you to run or deploy a relay before the first paste, which is the
price of not having to name the server.

Where it wins: nothing sits in the background reading your clipboard, nothing
is installed on the server in Fast Mode, the attachment is encrypted end to end
when it does travel through a relay, files come back as well as go out, and no
part of it knows or cares which agent or terminal you use.

## Commands

| Command | What it does |
| --- | --- |
| `clift setup` | First-time questions; with `<ssh-host>`, verify a Fast Mode host and remember it |
| `clift paste [--copy\|--inject]` | Send the clipboard and hand you the text to paste |
| `clift fetch '<token>' [--copy]` | Redeem a token: print the file's path, or put the picture on your clipboard |
| `clift copy <file…>` | On the server: seal a file and print a token to paste at home |
| `clift hotkey [--install]` | One key combination, in any application |
| `clift send [files…] [--to <target>]` | Fast Mode: send files or `--clipboard` over SSH |
| `clift doctor` | Say exactly what would stop a send from working |
| `clift status` · `clift config` · `clift clean` | Inspect, edit, tidy up |

Every command has `--json` for machines and a stable exit code per failure.

## Configuration

`~/.config/clift/config.toml` on macOS and Linux, `%APPDATA%\Clift\config.toml`
on Windows. A few lines, no secrets anywhere in it:

```toml
mode = "universal"

[relay]
url = "https://clift-relay.<you>.workers.dev"
max_bytes = "8MiB"
ttl = "5m"

[hotkey]
combination = "cmd+shift+v"
```

## Security in one paragraph

Keys never leave the two machines at the ends. The relay sees ciphertext and an
unguessable id, nothing else. It cannot decrypt, and it is not asked to
authenticate anyone. Tokens are single use and expire. SSH is your own,
unmodified. There is no telemetry, no account, no public relay, and no address
compiled into the binary. What Clift does not defend against, such as a
malicious process running as you or the server's root, is written down in
[THREAT_MODEL.md](THREAT_MODEL.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports go through
[SECURITY.md](SECURITY.md).

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

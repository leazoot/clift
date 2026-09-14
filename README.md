<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
  <img src="assets/logo-light.png" alt="Clift" width="360">
</picture>

**Paste screenshots from your laptop straight into Claude Code, Codex and other coding agents on an SSH server.**

No image hosting, no SSH config changes, no daemon on the server.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://github.com/leazoot/clift/actions/workflows/ci.yml/badge.svg)](https://github.com/leazoot/clift/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/leazoot/clift?include_prereleases)](https://github.com/leazoot/clift/releases)

English · [简体中文](README.zh-CN.md)

</div>

---

## What does Clift do?

You use Claude Code, Codex, Gemini CLI or another command-line agent on a server over SSH.

Text pastes fine. Screenshots don't.

The image is on your laptop's clipboard, but the agent runs on another machine. Usually you have to save the image, upload it to the server, then send the path to the agent.

Clift turns those steps into one shortcut:

```text
Screenshot
  ↓
Cmd+Shift+V
  ↓
Sent over SSH / SFTP
  ↓
A file path appears in the terminal
  ↓
The agent reads it
```

For example:

```text
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

In Fast Mode the file goes over your existing SSH connection.

Nothing is installed on the server, and no relay is involved.

---

# Quick start

Start with **Fast Mode**.

If you already have a server you can `ssh` into, it takes a few minutes.

## 1. Install

The laptop side currently supports macOS and Windows.

### macOS

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh
```

### Windows

In PowerShell:

```powershell
irm https://raw.githubusercontent.com/leazoot/clift/main/install.ps1 | iex
```

The archive is downloaded together with its `SHA256SUMS`. If the check fails, nothing is installed. No `sudo` needed. When it finishes, `clift setup` starts.

---

## 2. Set up an SSH target

Say your `~/.ssh/config` already has:

```sshconfig
Host core
    HostName 10.0.0.8
    User dev
```

Run:

```bash
clift setup core
```

Clift checks, in order:

* SSH connects
* SFTP works
* The remote inbox can be created
* A test file can be uploaded and deleted

The configuration is saved only if all of these pass.

Clift does not modify your `~/.ssh/config` and does not read your SSH private keys. Connecting, authentication and `known_hosts` checks are still handled by the system's OpenSSH.

---

## 3. Register the shortcut

```bash
clift hotkey --install
```

Default shortcuts:

| Platform | Shortcut      |
| -------- | ------------- |
| macOS    | `Cmd+Shift+V` |
| Windows  | `Ctrl+Alt+V`  |

The shortcut helper is registered to start at login.

On macOS, the first time it runs it asks for Accessibility permission, because Clift needs to type the generated text into the current window.

---

## 4. Paste a screenshot

Take a screenshot, then put focus back on the terminal with the SSH session.

Press:

```text
Cmd+Shift+V
```

Clift uploads the image to the server, then types this into the current window:

```text
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

The agent reads the file.

### Plain text is not affected

If the clipboard holds plain text, Clift uploads nothing and the shortcut does nothing. Paste text with `Cmd+V` as usual.

---

# How does Fast Mode work?

Fast Mode has no extra service.

```text
┌──────────────┐         SSH / SFTP         ┌──────────────┐
│   Laptop     │ ─────────────────────────▶ │    Server    │
│              │                            │              │
│  Clipboard   │                            │ inbox/image  │
└──────────────┘                            └──────────────┘
```

Clift uses what you already have set up:

```text
ssh
~/.ssh/config
known_hosts
SSH agent / system authentication
```

The server does not need:

* Clift installed
* A plugin
* SSH config changes
* Open ports
* A daemon
* A relay

Files go into a private inbox under the user's home directory. Directories are `0700`, files are `0600`.

Clift reads the clipboard once, when you paste. It does not watch the clipboard and keeps no clipboard history.

---

# Fast Mode and Universal Mode

Clift has two ways to transfer files.

|                       | **Fast Mode**                        | **Universal Mode**                                              |
| --------------------- | ------------------------------------ | --------------------------------------------------------------- |
| Good for              | SSH servers you have already set up  | Switching servers often, temporary machines, many remotes       |
| How the target is set | Clift uses the configured SSH target | Whichever server the token is pasted into fetches the file      |
| Transfer              | SSH / SFTP, direct                   | Encrypted locally → relay → fetched by the server               |
| Relay                 | Not needed                           | Needed                                                          |
| Clift on the server   | Not needed                           | Needed                                                          |
| SSH config changes    | Not needed                           | Not needed                                                      |
| Daemon on the server  | Not needed                           | Not needed                                                      |
| When to use           | Default                              | When you want "the current session decides the target"          |

If your servers are already in `~/.ssh/config`, use Fast Mode first.

Universal Mode solves a different problem:

> I don't want Clift on my laptop to know in advance which server this is going to.

---

# Universal Mode

In Fast Mode, your laptop picks the target.

In Universal Mode, the **current terminal session** decides.

When you press the shortcut, your laptop produces a line like this:

```text
Attachment: clift fetch 'clift://v1/…'
```

Whichever server you paste it into can fetch the attachment.

Your laptop doesn't need to know about that server beforehand.

---

## 1. Get a relay

Universal Mode needs a relay.

The relay only holds the encrypted attachment for a while. You can run `clift-relayd` yourself, or deploy one to your own Cloudflare account:

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/leazoot/clift/tree/main/relay/cloudflare)

Once you have the address, for example:

```text
https://clift-relay.<you>.workers.dev
```

---

## 2. Set up the server

Universal Mode doesn't need you to register servers **on your laptop**, but a server that receives attachments needs `clift` installed and the relay address.

### Let the agent install and configure it (recommended)

Send this to the agent running on the server, with your relay address filled in:

```text
Set up Clift on this server so I can paste screenshots to you.

RELAY_URL: https://clift-relay.<you>.workers.dev

Follow https://raw.githubusercontent.com/leazoot/clift/main/install.md exactly:
fetch it, work through its TODO list in order, stop and show me the error if a
step fails, and report as its last step says.
```

### By hand

Linux / macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh -s -- --no-setup

clift config set relay.url https://clift-relay.<you>.workers.dev

clift doctor
```

If you want the agent to recognise Clift tokens, add the instructions to the agent's instructions file.

Claude Code:

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> CLAUDE.md
```

Codex:

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> AGENTS.md
```

For other agents, append it to whichever instructions file the agent actually reads, for example:

```text
GEMINI.md
AGENTS.md
CLAUDE.md
```

The relay address is not in the token, so every receiving server needs the relay address set once.

---

## 3. Paste

After taking a screenshot, press the shortcut in the SSH session.

Universal Mode doesn't upload to an SSH host. It types:

```text
Attachment: clift fetch 'clift://v1/…'
```

The server runs:

```bash
clift fetch 'clift://v1/…'
```

On success it prints the file's path on the server, and the agent can read it.

You never tell Clift on your laptop which machine you're connected to.

---

## Claude Code hook (optional)

If you use Claude Code, you can install a hook for it:

[Claude Code integration](integrations/claude-code/README.md)

With it installed, the token is fetched when you submit, so Claude doesn't have to decide to run `clift fetch` first.

---

# Bringing an image back from the server

Universal Mode also works the other way.

On the server:

```console
$ clift copy build/report.png
clift://v1/…
```

Copy the token it returns:

```text
clift://v1/…
```

Then press the Clift shortcut on your laptop.

The image goes onto your local clipboard, and you can paste it into a browser, a chat app or anything else.

If you can easily pull the file with `scp` / `sftp`, that is usually simpler. This is for when you're already working in the terminal and don't want to deal with a separate file transfer (honestly, it's not that useful).

---

# How Universal Mode works

```text
 Laptop                         Relay                         Server
─────────                     ─────────                     ─────────

Clipboard
    │
    │  XChaCha20-Poly1305
    ▼
Ciphertext ───────────────────▶ store
                                  │
                                  │ ciphertext
                                  ▼
                              clift fetch
                                  │
                                  ▼
                               decrypt
                                  │
                                  ▼
                                inbox/


Encryption key ─────── inside pasted Token ───────────────▶ Server

                    key is never sent to Relay
```

Every attachment uses a new **XChaCha20-Poly1305** key and nonce.

The relay only gets encrypted data. The attachment's contents, file name and media type are all inside it.

The key is in the token's URL fragment and is never sent to the relay.

Note: by default an attachment can be fetched successfully only once. Anything not fetched expires.

---

# Other ways to install

### Homebrew

```bash
brew install leazoot/clift/clift
```

### cargo-binstall

```bash
cargo binstall --git https://github.com/leazoot/clift clift-cli
```

### Releases

Download the archive for your platform from

[GitHub Releases](https://github.com/leazoot/clift/releases)

### Build from source

Needs Rust 1.95 or newer:

```bash
git clone https://github.com/leazoot/clift.git
cd clift
cargo build --release
```

The Scoop manifest for Windows is in

[`packaging/`](packaging/)

---

# Common commands

| Command                             | What it does                                    |
| ----------------------------------- | ----------------------------------------------- |
| `clift setup`                       | Interactive setup                               |
| `clift setup <ssh-host>`            | Verify and save a Fast Mode SSH target          |
| `clift paste`                       | Handle what's on the clipboard                  |
| `clift paste --copy`                | Put the generated text on the clipboard         |
| `clift paste --inject`              | Type the generated text into the current window |
| `clift send [files…]`               | Send files in Fast Mode                         |
| `clift send [files…] --to <target>` | Pick the Fast Mode target                       |
| `clift fetch '<token>'`             | Fetch an attachment in Universal Mode           |
| `clift fetch '<token>' --copy`      | Fetch an image onto the clipboard               |
| `clift copy <file…>`                | Wrap a server file in a token you can take home |
| `clift hotkey --install`            | Install the global shortcut helper              |
| `clift doctor`                      | Check the current config and connection         |
| `clift status`                      | Show the current status                         |
| `clift config`                      | Show or change the config                       |
| `clift clean`                       | Clean up Clift's files                          |

Commands support `--json` output and errors have fixed exit codes, so scripts and agents can call them.

---

# Configuration

Location:

### macOS / Linux

```text
~/.config/clift/config.toml
```

### Windows

```text
%APPDATA%\Clift\config.toml
```

A Universal Mode config might look like:

```toml
mode = "universal"

[relay]
url = "https://clift-relay.<you>.workers.dev"
max_bytes = "8MiB"
ttl = "5m"

[hotkey]
combination = "cmd+shift+v"
```

No attachment keys are stored in the config.

---

# Troubleshooting

Run this first:

```bash
clift doctor
```

It checks what the current mode needs and shows where it fails.

### SSH works, but Clift can't send

Verify the target again:

```bash
clift setup core
```

Clift needs both SSH and SFTP to work.

### The SSH host key changed

Clift won't bypass host key verification.

Check whether the change is legitimate first, then handle it the way you normally manage OpenSSH.

### On macOS you have to press Cmd+V after the shortcut

The shortcut helper doesn't have Accessibility permission, so it can only put the text on the clipboard.

The permission has to go to the `clift` program itself, not your terminal. The `program:` line printed by `clift hotkey --install` is its path, `~/.local/bin/clift` by default.

Add it here:

```text
System Settings
→ Privacy & Security
→ Accessibility
```

Then run again:

```bash
clift hotkey --install
```

### Universal Mode can't fetch an attachment

Check the relay config on both ends:

```bash
clift status
clift doctor
```

A token works only once, and it expires.

---

# Why not just use scp?

Clift is for a different situation:

> The image has just landed on your clipboard, and your hands are still in a Claude Code / Codex SSH session.

It saves you:

```text
Save the image
→ Find the file
→ Think of a path
→ scp
→ Work out the remote path
→ Send the path to the agent
```

and turns it into:

```text
Screenshot
→ Shortcut
```

---

# Why two modes?

Fast Mode and Universal Mode are not a "lite" and a "full" version.

They are two different ways of choosing the target.

### Fast Mode

Your laptop knows the target:

```text
This goes to core.
```

So it can go straight over SSH / SFTP.

### Universal Mode

Your laptop doesn't know the target, and doesn't need to:

```text
Whichever machine I paste the token into fetches it.
```

So a relay holds the ciphertext for a while.

Most of the time, **Fast Mode is enough**.

Use Universal Mode only when "the current session decides the target" is more convenient than "the laptop has the target configured".

---

# Privacy

Clift has:

* No account system
* No telemetry
* No clipboard watching
* No clipboard history
* No third-party service in Fast Mode
* No public relay hard-coded into the client

---

# Contributing

How to develop and contribute:

[CONTRIBUTING.md](CONTRIBUTING.md)

Found a security issue:

[SECURITY.md](SECURITY.md)

Security model:

[THREAT_MODEL.md](THREAT_MODEL.md)

## Links

- [LINUX DO](https://linux.do/)

---

# License

Apache-2.0

See [LICENSE](LICENSE) and [NOTICE](NOTICE).

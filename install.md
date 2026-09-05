# clift

> Clift lets a person paste a screenshot from their own machine into the
> coding agent running on this server. Their machine encrypts the image and
> leaves it with a relay that cannot read it; what lands in the terminal here
> is one line containing a single-use token. `clift fetch` on this server
> redeems the token, writes the file under `~/.cache/clift/inbox/`, and prints
> its absolute path. This document sets that up on the server side. It is meant
> to be given to a coding agent (Claude Code, Codex CLI, Gemini CLI, …) running
> on the server; a person can follow it too.

You are setting up Clift on the machine you are running on, so that pasted
attachments can be fetched here. Work through the TODO list below in order.
Do not use sudo, do not modify `~/.ssh/config`, and do not install anything
other than what is named here. If a step fails, stop, show the command's own
error message, and ask the user; do not improvise a different way.

## OBJECTIVE

`clift` is installed for this user, knows the relay's address, and passes its
own relay check. The agent's standing instructions say what to do when a token
arrives.

## DONE WHEN

Both of these hold:

- `clift doctor` prints a line starting with `relay` whose status is `pass`;
- the file this agent reads for standing instructions (`CLAUDE.md`,
  `AGENTS.md`, `GEMINI.md`, or whatever it is) contains a section headed
  `## Attachments arrive through Clift`.

## TODO

- [ ] Step 1: Get the relay address from the user, or from the environment
- [ ] Step 2: Install `clift` for this user, without sudo
- [ ] Step 3: Point it at the relay
- [ ] Step 4: Verify with `clift doctor`
- [ ] Step 5: Add the token-handling instructions to your own instructions file
- [ ] Step 6: If you are Claude Code, install the hook that fetches on paste
- [ ] Step 7: Report what was done

## Step 1: Get the relay address

The relay's address is the one the user's own machine uses. It is **not**
inside a token, by design, so it has to be told to this server once.

- If the text that brought you here contains a `RELAY_URL` (an `https://…`
  address), use it.
- Otherwise, if the environment variable `CLIFT_RELAY_URL` is set, use that.
- Otherwise **ask the user**: "What relay address does your own machine use?
  `clift status` there shows it." Do not guess one and do not use an example
  address from documentation.

## Step 2: Install

Run the one-line installer. `--no-setup` matters: without it the installer
starts an interactive first-time setup meant for the machine one pastes
*from*, and its questions do not apply to a server.

On Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh -s -- --no-setup
```

On Windows, in PowerShell:

```powershell
$env:CLIFT_NO_SETUP = "1"; irm https://raw.githubusercontent.com/leazoot/clift/main/install.ps1 | iex
```

It downloads the release archive together with its `SHA256SUMS`, installs
nothing unless the digest matches, puts `clift` in `~/.local/bin` (Windows:
`%LOCALAPPDATA%\Programs\clift`), and never asks for sudo. If `~/.local/bin`
is not on the `PATH`, the installer prints the line that adds it; for the rest
of this document you may call the binary by its full path.

Verify:

```sh
clift --version
```

Expected: one line like `clift 0.1.0 (…)`.

## Step 3: Point it at the relay

```sh
clift config set relay.url RELAY_URL
```

with the address from Step 1 in place of `RELAY_URL`. This writes
`~/.config/clift/config.toml` (Windows: `%APPDATA%\Clift\config.toml`) and
nothing else.

## Step 4: Verify

```sh
clift doctor
```

`doctor` does a real round trip through the relay: it seals a small test
object, stores it, retrieves it, and compares. The line starting with `relay`
must say `pass`. The lines about the clipboard and the SSH targets will say
`warn` / `not checked` on a server; that is expected: this machine only
receives.

If `relay` says `fail`, the message names the reason (unreachable, refused,
not JSON, …) and a `curl` command to check the relay by hand. Show it to the
user and stop. Two causes are common enough to name: the address is wrong, or
this server cannot reach it (`*.workers.dev` is often unreachable from
mainland China; a proxy is only seen through the `HTTPS_PROXY` environment
variable).

## Step 5: Add the instructions to your own instructions file

`clift fetch` is a plain shell command and you would run it anyway; this
paragraph tells you how to behave when it fails, when a token has already been
spent, and to never echo a token. Append it to the file you read for standing
instructions in this project:

| Agent | File |
| --- | --- |
| Claude Code | `CLAUDE.md` (or `~/.claude/CLAUDE.md` for every project) |
| Codex CLI | `AGENTS.md` (or `~/.codex/AGENTS.md`) |
| Gemini CLI | `GEMINI.md` (or `~/.gemini/GEMINI.md`) |
| OpenCode | `AGENTS.md` |
| Anything else | Whatever file it treats as standing instructions |

```sh
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> CLAUDE.md
```

Append, do not overwrite. If the file already contains the heading
`## Attachments arrive through Clift`, skip this step.

## Step 6: Claude Code only: the hook

Skip this step unless you are Claude Code. The hook makes the attachment
arrive before you read the message: Claude Code runs it when the user presses
Enter, it redeems any token in the prompt, and it tells you the file's path.
Without it you run the pasted command yourself, one turn later; both work.

```sh
mkdir -p ~/.claude/hooks
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/claude-code/clift-hook.sh -o ~/.claude/hooks/clift.sh
chmod +x ~/.claude/hooks/clift.sh
```

Then add this to `~/.claude/settings.json`. Create the file with exactly this
content if it does not exist; if it exists, merge: keep everything already in
it, and add the `UserPromptSubmit` entry under `hooks` (next to any other
events there). Do not remove or reorder anything else in the file.

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "\"$HOME/.claude/hooks/clift.sh\"" }
        ]
      }
    ]
  }
}
```

The hook takes effect the next time Claude Code starts.

## Step 7: Report

Tell the user, in a few lines: the `clift --version` output, the relay
address that was configured, the `relay` line from `clift doctor`, which file
received the instructions, and whether the hook was installed. From now on, when a message contains `Attachment: clift fetch '…'`, run that
command once, exactly as written, and read the file at the path it prints.

## EXECUTE NOW

Work through the TODO list above, in order, until every DONE WHEN condition
holds or a step has failed and you have shown the user its message.

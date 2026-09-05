# Telling an agent what a Clift token is

Two files, for two moments:

- [`../../install.md`](../../install.md) is for **setting the server up**: an
  agent-executable guide (install, point at the relay, verify, add the
  instructions below to its own file). Paste it to the agent on the server, or
  `curl … | claude`.
- `clift.md`, below, is for **every day after that**: the standing paragraph
  that says what to do when a token arrives.

The line Clift pastes into a session is already a runnable command, and an
agent that runs it gets the file. `clift.md` is the paragraph that tells the
agent to do exactly that and nothing else: run it once, read the path it
prints, and, when it fails, hand the user the explanation instead of improvising.

It names no agent, because the same text is right in front of any of them.

## Where to put it

Append the contents of `clift.md` to the file your agent reads for project
instructions, on the **host the agent runs on** (that is where `clift fetch`
runs):

| Agent | File it reads |
| --- | --- |
| Claude Code | `CLAUDE.md` in the project, or `~/.claude/CLAUDE.md` for every project |
| Codex CLI | `AGENTS.md` in the project, or `~/.codex/AGENTS.md` |
| Gemini CLI | `GEMINI.md` in the project, or `~/.gemini/GEMINI.md` |
| OpenCode | `AGENTS.md` in the project |
| Anything else | Whatever file it treats as standing instructions |

One way to do it, on the server:

```console
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> CLAUDE.md
```

For Claude Code there is one more, optional piece: a hook that redeems the
token the moment the message is submitted, so the file is on disk before
Claude reads a word. See [`../claude-code/`](../claude-code/README.md).

Nothing here is required. The instruction Clift pastes is a plain shell command
and an agent that runs shell commands will usually run it unprompted; the
snippet exists so that the second attempt, the failure and the spent token are
handled the way the user would want rather than the way the agent guesses.


# Claude Code: fetch the attachment the moment it is pasted

Optional, Claude Code only. Without it the line Clift pastes is still a
command Claude Code runs itself; with it, the attachment is already on disk
by the time Claude reads the message, and its first move is to look at the
file rather than to decide to run something.

`clift-hook.sh` is a `UserPromptSubmit` hook. Claude Code runs it when you
press Enter, with the prompt on its stdin. If the prompt contains a Clift
token, the hook runs `clift fetch` and tells Claude where the file landed; if
the fetch fails, it hands Claude the explanation to show you. Whatever the
hook prints is context for Claude, not for you, and it never prints the
token. It never blocks a prompt.

## Install, on the host Claude Code runs on

```sh
mkdir -p ~/.claude/hooks
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/claude-code/clift-hook.sh -o ~/.claude/hooks/clift.sh
chmod +x ~/.claude/hooks/clift.sh
```

Then register it in `~/.claude/settings.json` (create the file if it does
not exist; if it already has a `hooks` section, add the `UserPromptSubmit`
entry to it):

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

A project can carry the same block in `.claude/settings.json` instead, for
that project only. Claude Code picks up the change at its next start.

## What you will see

Nothing new: the pasted line looks the same and Claude answers about the
file straight away. `clift fetch` is not run a second time, because the hook
tells Claude the token is already spent.

The hook needs `clift` on the `PATH` Claude Code was started with, or at
`~/.local/bin/clift`, which is where the installer puts it.

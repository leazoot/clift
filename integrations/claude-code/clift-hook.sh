#!/bin/sh
# Claude Code hook, UserPromptSubmit: redeem Clift tokens before Claude reads
# the prompt.
#
# Claude Code runs this when the user presses Enter, with the prompt on stdin
# as JSON. If the prompt contains a Clift token, the attachment is fetched
# here and now, and Claude is told where it landed; the first thing it does is
# read the file, instead of spending a turn deciding to run a command. Without
# the hook the pasted line still works: Claude runs it itself.
#
# Everything printed on stdout becomes context for Claude and is not shown to
# the user. The token is never printed. The exit code is always 0: a fetch
# that fails must not block the prompt, it must explain itself to Claude,
# which then explains it to the user.
#
# Install: copy this file to ~/.claude/hooks/clift.sh, make it executable, and
# register it in ~/.claude/settings.json; the README next to this file shows
# the three lines.

set -u

input=$(cat)
tokens=$(printf '%s' "$input" | grep -oE "clift://v1/[A-Za-z0-9_-]+#[A-Za-z0-9_-]+" | sort -u) || exit 0
[ -n "$tokens" ] || exit 0

clift=$(command -v clift 2>/dev/null || true)
if [ -z "$clift" ] && [ -x "$HOME/.local/bin/clift" ]; then
    clift="$HOME/.local/bin/clift"
fi
if [ -z "$clift" ]; then
    echo "Clift: this message contains an attachment token, but clift is not installed on this host, so the attachment could not be fetched. Tell the user. The installer is: curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh -s -- --no-setup"
    exit 0
fi

errors=$(mktemp) || exit 0
trap 'rm -f "$errors"' EXIT

for token in $tokens; do
    # --print-path is the default; passed explicitly because the whole hook
    # depends on the path arriving on stdout.
    if paths=$("$clift" fetch "$token" --print-path 2>"$errors"); then
        echo "Clift already fetched the attachment in this message; the token is spent, so do not run the clift fetch command. Read the file at this path (one per line):"
        echo "$paths"
    else
        echo "Clift tried to fetch the attachment in this message and failed. Do not run the command yourself. Show the user this explanation as it is; the fix is on their side:"
        cat "$errors"
    fi
done

exit 0

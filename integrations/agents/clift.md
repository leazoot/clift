## Attachments arrive through Clift

When a message contains a line like

    Attachment: clift fetch 'clift://v1/…'

(older versions wrote it as a sentence ending in `--print-path`; treat it the
same way) the user has pasted an attachment, usually a screenshot, from their own
machine. The file is not on this host yet. To get it:

1. Run the command exactly as written, quotes included. The quoted `clift://…`
   is a single-use token; the part after `#` is the decryption key, and the
   quotes stop a shell from reading it as a comment. Do not shorten it, retype
   it, or split it.
2. It prints one absolute path per attachment on stdout and nothing else. Open
   the file at that path; that is the attachment.
3. Do not run it a second time. The token is spent by the first successful
   fetch, and the relay forgets the object after a few minutes.

If the command fails it prints nothing on stdout, exits non-zero, and explains
itself on stderr. Show the user that explanation as it is; the fix is always
on their side, and the message names it:

| Exit code | Meaning | What the user has to do |
| --- | --- | --- |
| 20 | No relay is configured on this host | Once, on this host: `clift config set relay.url <the relay their own machine uses>`; `clift status` there shows the address |
| 27 | The token is spent, expired or damaged | Paste the attachment again from their machine |
| 28 | The relay cannot be reached from this host | Check this host's network, or the relay |
| 29 | What arrived did not authenticate | Paste again; nothing was written |
| 25 | The inbox under `~/.cache/clift` cannot be written | Make the home directory writable |

A fetch that failed with 20 or 28 never reached the relay, so the same token
still works once this host is configured or the relay is reachable again, until
it expires. A fetch that failed with 27 or 29 needs a new token.

If `clift` is not installed on this host, the user installs it the same way as
on their own machine:

    curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh

Never echo the token back, and never write it into a file, a log or a commit.

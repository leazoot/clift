# Reporting a security problem

**Please do not open a public issue for a security problem.**

Report it privately through GitHub's advisory form:

<https://github.com/leazoot/clift/security/advisories/new>

If that is not available to you, open an issue containing only "security report,
please provide a private channel" and nothing else: no details, no reproduction.

## What to include

- What the problem is, in one or two sentences.
- The version (`clift --version`) and platform.
- How to reproduce it, or why you believe it is exploitable if you have not.
- What you think an attacker gets.

A working exploit is not required and not requested.

## What to expect

This is a small project with one maintainer. A first response within a week is
the realistic promise, not a guarantee of a fix in that time. You will be told
what is happening either way.

## Scope

Clift's threat model is in `THREAT_MODEL.md`, and it is short. Things that
are **outside** it, and so are not vulnerabilities in Clift:

- another process running as the same user reading the clipboard, the temporary
  files or the attachments;
- the remote system's administrator reading files Clift uploaded;
- attachments being readable by other users of a **shared** remote account.

Things that are firmly **inside** it, and are worth reporting:

- anything that weakens or bypasses SSH host key verification;
- an attachment reaching a host the user did not name;
- cleanup deleting anything outside the inbox;
- a path, permission or symlink handling flaw in the remote staging layer;
- a private key path, credential or attachment content appearing in any output,
  log or error;
- terminal configuration being damaged or made unrecoverable.

## Verifying a release

Every file on a release page, each archive, each SBOM and
`SHA256SUMS`, has a Sigstore bundle next to it, named `<file>.sigstore.json`.
The signer is not a person with a key; it is the release workflow in this
repository, running at a `v*` tag, with a certificate issued for that run and a
record in Sigstore's public log. So what you check is that identity:

```console
$ cosign verify-blob \
    --bundle clift-<version>-<target>.tar.gz.sigstore.json \
    --certificate-identity-regexp '^https://github\.com/leazoot/clift/\.github/workflows/release\.yml@refs/tags/v' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    clift-<version>-<target>.tar.gz
```

`cosign` is from <https://github.com/sigstore/cosign>. Verifying `SHA256SUMS`
the same way, and then the archive's digest against it, is equivalent and
covers every file at once.

The two `.cdx.json` files are CycloneDX 1.5 SBOMs for `clift` and
`clift-relayd`, generated from `Cargo.lock` with the dependencies of every
target listed, and signed like everything else.

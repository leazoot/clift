# Clift's relay on Cloudflare

English · [简体中文](README.zh-CN.md)

For people who have no machine to run `clift-relayd` on.

This is the same relay, as a Cloudflare Worker: it holds ciphertext for a few
minutes, hands it out once, and forgets it. It speaks the same four routes with
the same refusals and the same numbers as the daemon, and it is held to that by
the same tests: every scenario in `crates/clift-relay/tests/real_relay.rs` runs
against the daemon *and* against this Worker in the real `workerd` runtime.

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/leazoot/clift/tree/main/relay/cloudflare)

## What you get

One Worker and one Durable Object, on a free Cloudflare account, at an address
like `https://clift-relay.<your-subdomain>.workers.dev`. Then, on the machine
you paste from:

```console
$ clift config set relay.url https://clift-relay.<your-subdomain>.workers.dev
```

And once on each machine an agent runs on, because a token carries the object
and the key but never the relay's address:

```console
$ clift config set relay.url https://clift-relay.<your-subdomain>.workers.dev
$ # or, for one command:  CLIFT_RELAY_URL=https://… clift fetch '<token>'
```

That is all. `clift paste` now works into any SSH session with no target
configured, and `clift fetch` on the far side takes the attachment down.

## Why one Durable Object

A Durable Object runs one event at a time. That single thread is what makes
the relay's one hard guarantee, *an object is handed to exactly one of
however many fetches arrive together*, true by construction rather than by a
lock, and it is what lets the health endpoint say how much is held, because
there is one place it is held. Everything the daemon keeps in one process, the
Worker keeps in one object.

The object is named `relay`, always. Every request from the Worker in front
goes to the same one.

Ciphertext is stored in the object's SQLite in 1 MiB rows, because a row is
capped at 2 MB. An 8 MiB attachment is eight rows. Expiry is checked on every
request and, for the idle case, by an alarm set to the earliest expiry.

## Limits, and why the free plan is enough

From Cloudflare's own documentation:

| | Free plan | What the relay needs |
| --- | --- | --- |
| Durable Objects | Available, SQLite storage only | SQLite storage |
| Requests | 100,000 / day | Two per attachment |
| Stored data | 5 GB | Under 256 MiB, for minutes |
| Rows written | 100,000 / day | About nine per attachment |
| Request body | 100 MB | 8 MiB by default |
| CPU per request | 10 ms | Copying bytes; no cryptography happens here |

## Configuration

The same names and the same syntax as `clift-relayd`, set in `wrangler.jsonc`
or on the command line with `--var NAME:VALUE`:

| Variable | Default | Meaning |
| --- | --- | --- |
| `CLIFT_RELAY_MAX_BYTES` | `8MiB` | Largest single object |
| `CLIFT_RELAY_TTL` | `5m` | Longest an object may live (hard cap 1h) |
| `CLIFT_RELAY_MAX_TOTAL_BYTES` | `256MiB` | Most it will hold at once |
| `CLIFT_RELAY_RATE_LIMIT` | `60` | Requests per minute per source; `0` disables |

A misconfigured value does not stop a Worker from existing the way it stops a
daemon from starting, so it refuses every request instead, with the reason.

## What is different from the daemon

- **TLS is Cloudflare's.** The daemon speaks plain HTTP and expects a reverse
  proxy; here the proxy is the edge, with a certificate you never see.
- **A started delivery is a delivery.** The daemon puts an object back if the
  client dropped the connection before the bytes arrived. A Worker is not told
  whether the client read the response, so the Worker consumes the object once
  it hands it over. This errs on the side that never delivers twice.
- **Rate limiting counts `CF-Connecting-IP`,** which is the client's real
  address at the edge. The daemon counts the socket's peer.
- **The window is one place's memory.** Both keep their rate-limit windows in
  memory; the daemon's reset when it restarts, the Worker's when the object is
  evicted after idling. Neither persists them, on purpose.

## What is the same

Everything a client can observe. The routes, the status codes, the error
document, the `Cache-Control: no-store` on every response, the fixed messages
that never echo anything from the request, the 22-character ids from 128 bits
of the runtime's randomness, the refusal to say whether a `DELETE` found
anything. The relay sees ciphertext and ids and nothing else; no request has a
field that could carry a key, and the id is generated here so a client cannot
choose one.

## Running it yourself, without the button

```console
$ cd relay/cloudflare
$ npm install
$ npx wrangler login          # opens a browser, once
$ npx wrangler deploy
```

`wrangler deploy` prints the URL. To run the contract tests against the Worker
locally, `npm install` is the only prerequisite; the tests start `wrangler dev`
themselves and skip, loudly, if it is not installed:

```console
$ CLIFT_E2E_REQUIRE_WRANGLER=1 cargo test -p clift-relay --test real_relay
```

## What it does not do

Nothing the daemon does not do. No listing, no authentication, no accounts, no
metadata, no way to ask what an object contains. The URL is unauthenticated by
design: the relay cannot decrypt what it holds, so there is nothing an
account would protect. That means somebody who learns the address can spend
your daily request quota. That is the whole exposure, and the per-source rate
limit is what bounds it.

## One thing Cloudflare adds that the daemon does not

Cloudflare's edge applies its own bot protection in front of every Worker, and
it can answer some non-browser HTTP clients with `403` and `error code: 1010`
before the Worker sees the request. `clift` itself (which sends
`clift/<version>` as its user agent) and `curl` go through. If you script
against your relay with something else and get a 403 that is not JSON, that is
the edge, not the relay.

Node.js and `wrangler` are needed to deploy this and to run its tests. They
are never needed to run `clift`.

## If the server is in mainland China

`*.workers.dev` is often unreachable from networks in mainland China: the name
resolves to an address that is not Cloudflare's, and the connection never
completes. Nothing in Clift changes this: a host that cannot reach the relay
cannot redeem a token, so `clift fetch` there exits 28.

Two ways around it, neither of them Clift's to make:

- put the Worker behind a domain of your own on Cloudflare (a Workers route),
  which is a different name and may be reachable where `workers.dev` is not;
  or
- run `clift-relayd` on a machine both ends can reach, which is what the daemon
  is for.

Check before relying on either: `curl -sS <relay-url>/v1/health` from the
server itself is the whole test.


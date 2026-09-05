// Clift's relay as a Cloudflare Worker.
//
// This is the same four-route protocol as `clift-relayd`, with the same
// refusals and the same numbers, for people who have no machine to run the
// daemon on. It is deliberately not cleverer than the daemon: it stores
// ciphertext, returns it once, forgets it, and says how much it is holding.
//
//     POST   /v1/objects?ttl=<seconds>   store ciphertext, get an id back
//     GET    /v1/objects/<id>            take it, once
//     DELETE /v1/objects/<id>            withdraw it
//     GET    /v1/health                  say what is being held
//
// Everything lives in one Durable Object. A Durable Object runs one event at
// a time, so "hand this object to exactly one of eight simultaneous fetches"
// is true by construction rather than by a lock, and the health endpoint can
// count what is held because there is one place it is held. The Worker in
// front of it does nothing but forward.
//
// The relay never sees a key. Nothing in any request can carry one: the id is
// generated here, the body is opaque bytes, and no field of any document is
// interpreted. That is the whole security argument, and it is the same one
// the daemon makes.

import { DurableObject } from "cloudflare:workers";

export interface Env {
  RELAY: DurableObjectNamespace<CliftRelay>;
  CLIFT_RELAY_MAX_BYTES?: string;
  CLIFT_RELAY_TTL?: string;
  CLIFT_RELAY_MAX_TOTAL_BYTES?: string;
  CLIFT_RELAY_RATE_LIMIT?: string;
}

/** The relay's own protocol version, in every document it emits. */
const SCHEMA_VERSION = 1;

/** Base64url of a 128-bit id, without padding, is exactly this long. */
const ENCODED_ID_LEN = 22;
const OBJECT_PREFIX = "/v1/objects/";

/** One message for every way of being too large, whichever check caught it. */
const TOO_LARGE = "the object is larger than this relay accepts";

/**
 * SQLite-backed Durable Objects cap a row at 2 MB. Half of that per chunk
 * leaves room for the key columns and whatever overhead the row carries, and
 * an 8 MiB object is eight rows, which is nothing.
 */
const CHUNK_BYTES = 1024 * 1024;

/** The longest any relay may hold an object; the same cap as the daemon. */
const MAX_TTL_SECONDS = 60 * 60;

const RATE_WINDOW_MS = 60_000;

const DEFAULTS = {
  maxObjectBytes: "8MiB",
  ttl: "5m",
  maxTotalBytes: "256MiB",
  rateLimit: "60",
};

export default {
  fetch(request, env): Promise<Response> {
    // One relay, one object. The name is fixed on purpose: the point of a
    // single Durable Object is that every request meets the same state.
    const stub = env.RELAY.get(env.RELAY.idFromName("relay"));
    return stub.fetch(request);
  },
} satisfies ExportedHandler<Env>;

interface Limits {
  maxObjectBytes: number;
  maxTotalBytes: number;
  maxTtlSeconds: number;
  requestsPerMinute: number;
}

interface Window {
  started: number;
  count: number;
}

export class CliftRelay extends DurableObject<Env> {
  private readonly limits: Limits | Error;
  private readonly windows = new Map<string, Window>();

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.limits = readLimits(env);
    // The tables must exist before the first request is looked at. Nothing
    // else is done here: an idle relay has no work.
    void ctx.blockConcurrencyWhile(async () => {
      ctx.storage.sql.exec(`
        CREATE TABLE IF NOT EXISTS objects (
          id TEXT PRIMARY KEY,
          size INTEGER NOT NULL,
          expires_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS chunks (
          id TEXT NOT NULL,
          seq INTEGER NOT NULL,
          data BLOB NOT NULL,
          PRIMARY KEY (id, seq)
        );
      `);
    });
  }

  override async fetch(request: Request): Promise<Response> {
    if (this.limits instanceof Error) {
      // A daemon with this configuration refuses to start. A Worker cannot
      // refuse to exist, so it refuses every request instead, with the reason.
      return problem(500, `the relay is misconfigured: ${this.limits.message}`);
    }
    const limits = this.limits;

    // Cloudflare puts the client's address in this header at the edge; in
    // local development it may be absent, which reads as "one client".
    const source = request.headers.get("cf-connecting-ip") ?? "0.0.0.0";
    if (!this.allow(source, limits.requestsPerMinute)) {
      return problem(429, "too many requests");
    }

    // Every read and write sweeps, so an idle relay still forgets.
    this.dropExpired(Date.now());

    const url = new URL(request.url);
    const path = url.pathname;

    if (request.method === "GET" && path === "/v1/health") {
      return this.health(limits);
    }
    if (request.method === "POST" && path === "/v1/objects") {
      return this.publish(request, url.searchParams, limits);
    }
    if (path.startsWith(OBJECT_PREFIX)) {
      const id = path.slice(OBJECT_PREFIX.length);
      if (request.method === "GET") {
        return this.retrieve(id);
      }
      if (request.method === "DELETE") {
        this.remove(id);
        // Always 204, whether it was there or not. Reporting the difference
        // would turn this endpoint into an oracle for "does this id exist",
        // which is the one thing an unauthenticated relay must not answer.
        return new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } });
      }
    }
    return problem(404, "no such endpoint");
  }

  /** Expiry is enforced on every request; the alarm is for the idle case. */
  override async alarm(): Promise<void> {
    const now = Date.now();
    this.dropExpired(now);
    const next = this.nextExpiry();
    if (next !== null) {
      await this.ctx.storage.setAlarm(next);
    }
  }

  private health(limits: Limits): Response {
    const row = this.ctx.storage.sql
      .exec<{ objects: number; bytes: number }>(
        "SELECT COUNT(*) AS objects, COALESCE(SUM(size), 0) AS bytes FROM objects",
      )
      .one();
    return json(200, {
      schema_version: SCHEMA_VERSION,
      status: "ok",
      objects: row.objects,
      bytes: row.bytes,
      max_object_bytes: limits.maxObjectBytes,
      max_ttl_seconds: limits.maxTtlSeconds,
    });
  }

  private async publish(
    request: Request,
    query: URLSearchParams,
    limits: Limits,
  ): Promise<Response> {
    const ttl = requestedTtl(query, limits.maxTtlSeconds);
    if (typeof ttl === "string") {
      return discard(request, problem(400, ttl));
    }

    // The declared length is checked before a byte is read, so an oversized
    // upload costs the relay nothing but a status line.
    const declared = Number(request.headers.get("content-length") ?? "0");
    if (Number.isFinite(declared) && declared > limits.maxObjectBytes) {
      return discard(request, problem(413, TOO_LARGE));
    }

    // Read with a hard ceiling anyway: a chunked request has no declared
    // length to have checked, and a lying Content-Length is free to send.
    const body = await readCapped(request.body, limits.maxObjectBytes);
    if (body === null) {
      return problem(413, TOO_LARGE);
    }
    if (body.byteLength === 0) {
      return problem(400, "the request body is empty");
    }

    const id = newObjectId();
    if (id === null) {
      return problem(503, "the relay has no working random source");
    }

    // From here to the last INSERT nothing is awaited, so no other request is
    // let in between the capacity check and the write that consumes capacity.
    const now = Date.now();
    this.dropExpired(now);
    if (this.heldBytes() + body.byteLength > limits.maxTotalBytes) {
      return problem(503, "the relay is holding as much as it can");
    }
    const expiresAt = now + ttl * 1000;
    const sql = this.ctx.storage.sql;
    sql.exec(
      "INSERT INTO objects (id, size, expires_at) VALUES (?, ?, ?)",
      id,
      body.byteLength,
      expiresAt,
    );
    for (let seq = 0, offset = 0; offset < body.byteLength; seq += 1, offset += CHUNK_BYTES) {
      const end = Math.min(offset + CHUNK_BYTES, body.byteLength);
      // A fresh ArrayBuffer of exactly the chunk: binding a view over the
      // whole body would store the whole body every time.
      const chunk = body.buffer.slice(body.byteOffset + offset, body.byteOffset + end);
      sql.exec("INSERT INTO chunks (id, seq, data) VALUES (?, ?, ?)", id, seq, chunk);
    }
    await this.scheduleAlarm(expiresAt);

    return json(201, {
      schema_version: SCHEMA_VERSION,
      object_id: id,
      ttl_seconds: ttl,
    });
  }

  private retrieve(id: string): Response {
    if (id.length !== ENCODED_ID_LEN || !/^[A-Za-z0-9_-]+$/.test(id)) {
      return problem(404, "no such object");
    }
    const sql = this.ctx.storage.sql;
    const now = Date.now();
    const found = sql
      .exec<{ expires_at: number }>("SELECT expires_at FROM objects WHERE id = ?", id)
      .toArray()[0];
    if (found === undefined || found.expires_at <= now) {
      return problem(404, "no such object");
    }

    // Read, then delete, with nothing awaited in between: this is the single
    // use guarantee. Two fetches arriving together are run one after the
    // other, and the second finds nothing.
    const chunks = sql
      .exec<{ data: ArrayBuffer }>("SELECT data FROM chunks WHERE id = ? ORDER BY seq", id)
      .toArray();
    const bytes = concat(chunks.map((row) => new Uint8Array(row.data)));
    this.remove(id);

    // Unlike the daemon, a delivery that fails after this point is not put
    // back: a Worker is not told whether the client read the response. The
    // object is consumed once it is handed over, which errs on the side that
    // never delivers twice.
    return new Response(bytes, {
      status: 200,
      headers: {
        "Content-Type": "application/octet-stream",
        "Cache-Control": "no-store",
      },
    });
  }

  private remove(id: string): void {
    const sql = this.ctx.storage.sql;
    sql.exec("DELETE FROM chunks WHERE id = ?", id);
    sql.exec("DELETE FROM objects WHERE id = ?", id);
  }

  private dropExpired(now: number): void {
    const sql = this.ctx.storage.sql;
    sql.exec("DELETE FROM chunks WHERE id IN (SELECT id FROM objects WHERE expires_at <= ?)", now);
    sql.exec("DELETE FROM objects WHERE expires_at <= ?", now);
  }

  private heldBytes(): number {
    return this.ctx.storage.sql
      .exec<{ bytes: number }>("SELECT COALESCE(SUM(size), 0) AS bytes FROM objects")
      .one().bytes;
  }

  private nextExpiry(): number | null {
    const row = this.ctx.storage.sql
      .exec<{ next: number | null }>("SELECT MIN(expires_at) AS next FROM objects")
      .one();
    return row.next;
  }

  /** Moves the alarm earlier if this object expires before it. */
  private async scheduleAlarm(at: number): Promise<void> {
    const current = await this.ctx.storage.getAlarm();
    if (current === null || at < current) {
      await this.ctx.storage.setAlarm(at);
    }
  }

  /**
   * A fixed window per source, the same as the daemon's. Bounded by sweeping
   * the whole map when it grows: the alternative is a table an attacker can
   * grow one address at a time.
   */
  private allow(source: string, perMinute: number): boolean {
    if (perMinute === 0) {
      return true;
    }
    const now = Date.now();
    if (this.windows.size > 10_000) {
      for (const [key, window] of this.windows) {
        if (now - window.started >= RATE_WINDOW_MS) {
          this.windows.delete(key);
        }
      }
    }
    let window = this.windows.get(source);
    if (window === undefined || now - window.started >= RATE_WINDOW_MS) {
      window = { started: now, count: 0 };
      this.windows.set(source, window);
    }
    window.count += 1;
    return window.count <= perMinute;
  }
}

/**
 * The TTL the client asked for, clamped to what this relay will honour.
 *
 * A client asking for longer than the maximum gets the maximum rather than a
 * rejection; what is rejected is a value that is not a number at all, because
 * that means the two sides disagree about the protocol.
 */
function requestedTtl(query: URLSearchParams, maximum: number): number | string {
  const value = query.get("ttl");
  if (value === null) {
    return maximum;
  }
  if (!/^\+?[0-9]+$/.test(value)) {
    return "ttl must be a whole number of seconds";
  }
  const seconds = Number(value);
  if (seconds === 0) {
    return "ttl must be greater than zero";
  }
  return Math.min(seconds, maximum);
}

/**
 * Answers without reading the body, and says so.
 *
 * A request body that is never read is not simply ignored by the runtime:
 * under `wrangler dev` the unread bytes wedge the connection between the
 * local proxy and workerd, and every later request fails. Cancelling the
 * stream tells the runtime the bytes are not wanted, so it can drain or close.
 */
async function discard(request: Request, response: Response): Promise<Response> {
  if (request.body !== null) {
    // Read to the end rather than cancel: cancelling alone does not unwedge
    // the local proxy, and the runtime already caps a body at 100 MB, so
    // draining it is bounded by something other than the client's goodwill.
    const reader = request.body.getReader();
    for (;;) {
      const { done } = await reader.read();
      if (done) {
        break;
      }
    }
  }
  return response;
}

/** Reads a body up to `cap` bytes; `null` means it was larger than that. */
async function readCapped(
  body: ReadableStream<Uint8Array> | null,
  cap: number,
): Promise<Uint8Array | null> {
  if (body === null) {
    return new Uint8Array(0);
  }
  const reader = body.getReader();
  const parts: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      break;
    }
    total += value.byteLength;
    if (total > cap) {
      await reader.cancel();
      return null;
    }
    parts.push(value);
  }
  return concat(parts);
}

function concat(parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.byteLength;
  }
  return out;
}

/**
 * A fresh 128-bit identifier, base64url encoded.
 *
 * `null` when the runtime has no randomness to give, which must stop the
 * request: a predictable id would let somebody guess where another user's
 * ciphertext is, and the whole point of the id is that they cannot.
 */
function newObjectId(): string | null {
  const bytes = new Uint8Array(16);
  try {
    crypto.getRandomValues(bytes);
  } catch {
    return null;
  }
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function readLimits(env: Env): Limits | Error {
  try {
    const maxObjectBytes = parseSize(env.CLIFT_RELAY_MAX_BYTES ?? DEFAULTS.maxObjectBytes);
    const maxTotalBytes = parseSize(env.CLIFT_RELAY_MAX_TOTAL_BYTES ?? DEFAULTS.maxTotalBytes);
    if (maxObjectBytes > maxTotalBytes) {
      throw new Error(
        `CLIFT_RELAY_MAX_BYTES (${maxObjectBytes}) is larger than CLIFT_RELAY_MAX_TOTAL_BYTES (${maxTotalBytes}), so no object could ever be stored`,
      );
    }
    const maxTtlSeconds = parseDuration(env.CLIFT_RELAY_TTL ?? DEFAULTS.ttl);
    if (maxTtlSeconds === 0 || maxTtlSeconds > MAX_TTL_SECONDS) {
      throw new Error(`CLIFT_RELAY_TTL must be between 1 second and ${MAX_TTL_SECONDS} seconds`);
    }
    const rate = env.CLIFT_RELAY_RATE_LIMIT ?? DEFAULTS.rateLimit;
    if (!/^[0-9]+$/.test(rate)) {
      throw new Error(`CLIFT_RELAY_RATE_LIMIT: ${JSON.stringify(rate)} is not a whole number`);
    }
    return {
      maxObjectBytes,
      maxTotalBytes,
      maxTtlSeconds,
      requestsPerMinute: Number(rate),
    };
  } catch (error) {
    return error instanceof Error ? error : new Error(String(error));
  }
}

/** The daemon's size grammar: `8MiB`, `512KiB`, `1GiB`, or a bare byte count. */
function parseSize(value: string): number {
  const match = /^\s*([0-9]+)\s*([A-Za-z]*)\s*$/.exec(value);
  if (match === null) {
    throw new Error(`size ${JSON.stringify(value)} does not start with a number`);
  }
  const number = Number(match[1]);
  const unit = (match[2] ?? "").toLowerCase();
  const multiplier =
    unit === "" || unit === "b"
      ? 1
      : unit === "kib" || unit === "k"
        ? 1024
        : unit === "mib" || unit === "m"
          ? 1024 * 1024
          : unit === "gib" || unit === "g"
            ? 1024 * 1024 * 1024
            : null;
  if (multiplier === null) {
    throw new Error(`size: unknown unit ${JSON.stringify(match[2])}; use B, KiB, MiB or GiB`);
  }
  return number * multiplier;
}

/** The daemon's duration grammar: `30s`, `5m`, `1h`, `1d`. A unit is required. */
function parseDuration(value: string): number {
  const match = /^\s*([0-9]+)\s*([A-Za-z]*)\s*$/.exec(value);
  if (match === null) {
    throw new Error(`duration ${JSON.stringify(value)} does not start with a number`);
  }
  const number = Number(match[1]);
  const unit = (match[2] ?? "").toLowerCase();
  const seconds =
    unit === "s" ? 1 : unit === "m" ? 60 : unit === "h" ? 3600 : unit === "d" ? 86_400 : null;
  if (seconds === null) {
    throw new Error(
      unit === ""
        ? `duration ${JSON.stringify(value)} has no unit; use s, m, h or d`
        : `duration: unknown unit ${JSON.stringify(match[2])}; use s, m, h or d`,
    );
  }
  return number * seconds;
}

/**
 * Every error body has the same shape, so a client can read one thing.
 *
 * The message is a fixed string chosen by this file. Nothing from the request
 * is echoed back -- not the path, not the id, not a header -- because an error
 * page that quotes its input is how a relay ends up reflecting somebody's
 * token into somebody else's log.
 */
function problem(status: number, message: string): Response {
  return json(status, { schema_version: SCHEMA_VERSION, status: "error", message });
}

/**
 * `no-store` is not decoration. An object is single use, and an intermediate
 * cache holding a copy would quietly make it double use -- which is the one
 * property of this relay that a proxy must not be able to break.
 */
function json(status: number, document: unknown): Response {
  return new Response(JSON.stringify(document), {
    status,
    headers: {
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
    },
  });
}

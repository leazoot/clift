//! Four routes, and a deliberate refusal to grow a fifth.
//!
//! ```text
//! POST   /v1/objects?ttl=<seconds>   store ciphertext, get an id back
//! GET    /v1/objects/<id>            take it, once
//! DELETE /v1/objects/<id>            withdraw it
//! GET    /v1/health                  say what is being held
//! ```
//!
//! What is not here is the point of the design. There is no listing, no
//! authentication, no account, no metadata, and no way to ask what an object
//! contains -- the relay cannot answer that last one anyway, and the other four
//! are things the specification rules out because each of them turns a dumb pipe into a
//! service that knows things about its users.
//!
//! The object id is generated here rather than accepted from the client. That
//! way one client cannot claim an id another client's object would later have
//! been given, and cannot choose a memorable one.

use crate::ratelimit::RateLimiter;
use crate::store::{Refusal, Store};
use std::io::Read;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, StatusCode};

/// The relay's own protocol version, in every document it emits.
pub const SCHEMA_VERSION: u32 = 1;

/// Base64url with no padding produces exactly this many characters for a
/// 128-bit id. Checked before the store is consulted so a nonsense path never
/// becomes a map lookup.
const ENCODED_ID_LEN: usize = 22;

const OBJECT_PREFIX: &str = "/v1/objects/";

/// One message for every way of being too large, so a client sees the same
/// text whichever check caught it.
const TOO_LARGE: &str = "the object is larger than this relay accepts";

/// Everything a request handler needs.
pub struct Relay {
    store: Arc<Store>,
    limiter: RateLimiter,
    max_ttl: Duration,
}

impl Relay {
    #[must_use]
    pub fn new(store: Arc<Store>, requests_per_minute: u32, max_ttl: Duration) -> Self {
        Self {
            store,
            limiter: RateLimiter::new(requests_per_minute),
            max_ttl,
        }
    }

    /// Handles one request. Never panics on input; every failure is a status.
    pub fn handle(&self, request: Request) {
        let address = request
            .remote_addr()
            .map_or(IpAddr::from([0, 0, 0, 0]), std::net::SocketAddr::ip);
        if !self.limiter.allow(address) {
            respond(request, 429, "too many requests");
            return;
        }

        let (path, query) = split_query(request.url());
        let method = request.method().clone();

        match (method, path.as_str()) {
            (Method::Get, "/v1/health") => self.health(request),
            (Method::Post, "/v1/objects") => self.publish(request, &query),
            (Method::Get, path) if path.starts_with(OBJECT_PREFIX) => {
                let id = path[OBJECT_PREFIX.len()..].to_string();
                self.retrieve(request, &id);
            }
            (Method::Delete, path) if path.starts_with(OBJECT_PREFIX) => {
                let id = path[OBJECT_PREFIX.len()..].to_string();
                self.store.remove(&id);
                // Always 204, whether it was there or not. Reporting the
                // difference would turn this endpoint into an oracle for
                // "does this id exist", which is the one thing an
                // unauthenticated relay must not answer.
                respond_empty(request, 204);
            }
            _ => respond(request, 404, "no such endpoint"),
        }
    }

    fn health(&self, request: Request) {
        let usage = self.store.usage();
        let document = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "status": "ok",
            "objects": usage.objects,
            "bytes": usage.bytes,
            "max_object_bytes": self.store.max_object_bytes(),
            "max_ttl_seconds": self.max_ttl.as_secs(),
        });
        respond_json(request, 200, &document);
    }

    fn publish(&self, mut request: Request, query: &str) {
        let ttl = match requested_ttl(query, self.max_ttl) {
            Ok(ttl) => ttl,
            Err(message) => {
                respond(request, 400, message);
                return;
            }
        };

        let ceiling = self.store.max_object_bytes();
        // The declared length is checked before a byte is read, so an oversized
        // upload costs the relay nothing but a status line.
        let declared = u64::try_from(request.body_length().unwrap_or(0)).unwrap_or(u64::MAX);
        if declared > ceiling {
            respond(request, 413, TOO_LARGE);
            return;
        }

        // Read with a hard ceiling anyway: a chunked request has no declared
        // length to have checked, and a lying Content-Length is free to send.
        let mut body = Vec::new();
        let read = request
            .as_reader()
            .take(ceiling.saturating_add(1))
            .read_to_end(&mut body);
        if read.is_err() {
            respond(request, 400, "the request body could not be read");
            return;
        }
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > ceiling {
            respond(request, 413, TOO_LARGE);
            return;
        }
        if body.is_empty() {
            respond(request, 400, "the request body is empty");
            return;
        }

        let Some(id) = new_object_id() else {
            respond(request, 503, "the relay has no working random source");
            return;
        };

        match self.store.put(id.clone(), body, ttl) {
            Ok(()) => {}
            Err(Refusal::TooLarge) => {
                respond(request, 413, TOO_LARGE);
                return;
            }
            Err(Refusal::Full) => {
                respond(request, 503, "the relay is holding as much as it can");
                return;
            }
        }

        let document = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "object_id": id,
            "ttl_seconds": ttl.as_secs(),
        });
        respond_json(request, 201, &document);
    }

    fn retrieve(&self, request: Request, id: &str) {
        if id.len() != ENCODED_ID_LEN || !id.bytes().all(is_base64url) {
            respond(request, 404, "no such object");
            return;
        }
        // Read the expiry before taking, so a failed delivery can be restored
        // with the time it had left rather than a fresh window.
        let expires_at = self.store.expiry_of(id);
        let Some(bytes) = self.store.take(id) else {
            respond(request, 404, "no such object");
            return;
        };

        let mut response = Response::from_data(bytes.clone()).with_status_code(StatusCode(200));
        add_headers(&mut response, "application/octet-stream");

        if request.respond(response).is_err() {
            // The client did not get it, so it has not been used. Partial bytes
            // are worthless without the key, so putting it back cannot leak
            // anything a failed download did not already fail to deliver.
            if let Some(expires_at) = expires_at {
                self.store.restore(id.to_string(), bytes, expires_at);
            }
        }
    }
}

/// Splits `"/v1/objects?ttl=60"` into its two halves.
fn split_query(url: &str) -> (String, String) {
    url.split_once('?').map_or_else(
        || (url.to_string(), String::new()),
        |(path, query)| (path.to_string(), query.to_string()),
    )
}

/// The TTL the client asked for, clamped to what this relay will honour.
///
/// A client asking for longer than the relay's maximum gets the maximum rather
/// than a rejection: the client's own configuration is capped too, and failing
/// a paste over a policy difference helps nobody. What is rejected is a value
/// that is not a number at all, because that means the two sides disagree about
/// the protocol.
fn requested_ttl(query: &str, maximum: Duration) -> Result<Duration, &'static str> {
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key != "ttl" {
            continue;
        }
        let Ok(seconds) = value.parse::<u64>() else {
            return Err("ttl must be a whole number of seconds");
        };
        if seconds == 0 {
            return Err("ttl must be greater than zero");
        }
        return Ok(Duration::from_secs(seconds).min(maximum));
    }
    Ok(maximum)
}

fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// A fresh 128-bit identifier, base64url encoded.
///
/// `None` when the operating system has no randomness to give, which must stop
/// the request: a predictable id would let somebody guess where another user's
/// ciphertext is, and the whole point of the id is that they cannot.
fn new_object_id() -> Option<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(clift_core::universal::ObjectId::from_bytes(bytes).encoded())
}

/// Adds the two headers every response carries.
///
/// `no-store` is not decoration. An object is single use, and an intermediate
/// cache holding a copy would quietly make it double use -- which is the one
/// property of this relay that a proxy must not be able to break.
fn add_headers(response: &mut Response<std::io::Cursor<Vec<u8>>>, content_type: &str) {
    for (name, value) in [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
    ] {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(header);
        }
    }
}

fn respond(request: Request, status: u16, message: &str) {
    let _ = request.respond(problem(status, message));
}

fn respond_empty(request: Request, status: u16) {
    let _ = request.respond(Response::empty(StatusCode(status)));
}

fn respond_json(request: Request, status: u16, document: &serde_json::Value) {
    let _ = request.respond(json(status, document));
}

/// Every error body has the same shape, so a client can read one thing.
///
/// The message is a fixed string chosen by this file. Nothing from the request
/// is echoed back -- not the path, not the id, not a header -- because an error
/// page that quotes its input is how a relay ends up reflecting somebody's
/// token into somebody else's log.
fn problem(status: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json(
        status,
        &serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "status": "error",
            "message": message,
        }),
    )
}

fn json(status: u16, document: &serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response =
        Response::from_data(document.to_string().into_bytes()).with_status_code(StatusCode(status));
    add_headers(&mut response, "application/json");
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_is_split_off_the_path() {
        assert_eq!(
            split_query("/v1/objects?ttl=60"),
            ("/v1/objects".to_string(), "ttl=60".to_string())
        );
        assert_eq!(
            split_query("/v1/health"),
            ("/v1/health".to_string(), String::new())
        );
    }

    #[test]
    fn a_ttl_beyond_the_relays_maximum_is_clamped_rather_than_refused() {
        let maximum = Duration::from_secs(300);
        assert_eq!(
            requested_ttl("ttl=60", maximum),
            Ok(Duration::from_secs(60))
        );
        assert_eq!(requested_ttl("ttl=99999", maximum), Ok(maximum));
        assert_eq!(requested_ttl("", maximum), Ok(maximum));
        assert_eq!(requested_ttl("other=1", maximum), Ok(maximum));
    }

    #[test]
    fn a_ttl_that_is_not_a_number_is_a_protocol_error() {
        let maximum = Duration::from_secs(300);
        assert!(requested_ttl("ttl=soon", maximum).is_err());
        assert!(requested_ttl("ttl=0", maximum).is_err());
        assert!(requested_ttl("ttl=-5", maximum).is_err());
    }

    #[test]
    fn only_base64url_characters_can_name_an_object() {
        for byte in b"AZaz09-_" {
            assert!(is_base64url(*byte), "{}", *byte as char);
        }
        for byte in b"/.+= &?%" {
            assert!(!is_base64url(*byte), "{}", *byte as char);
        }
    }

    #[test]
    fn an_object_id_is_the_length_the_router_checks_for() {
        let id = new_object_id().unwrap_or_default();
        assert_eq!(id.len(), ENCODED_ID_LEN);
        assert!(id.bytes().all(is_base64url), "{id}");
    }

    #[test]
    fn two_object_ids_are_never_the_same() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            assert!(seen.insert(new_object_id().unwrap_or_default()));
        }
    }
}

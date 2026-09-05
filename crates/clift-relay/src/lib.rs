//! The relay client: the only thing in Clift that speaks to a third party.
//!
//! Its whole contract is a negative one, and the type signatures are what
//! enforce it. [`clift_core::ports::Relay`] takes bytes and an object id and
//! nothing else, so there is no argument this code could put in a URL, a
//! header or a log line that would leak the key -- not by mistake, and not
//! after somebody refactors it. If the key ever needs to reach this crate, the
//! design has gone wrong somewhere upstream and the compiler will say so.
//!
//! What this file does contain is the mapping from HTTP onto Clift's error
//! kinds, and getting that mapping right is most of the user experience:
//!
//! | Relay says | Clift says | Exit |
//! | --- | --- | ---: |
//! | 404, 410 | the token cannot be redeemed | 27 |
//! | 413 | the attachment is too large for this relay | 26 |
//! | 429 | the relay is rate limiting | 28 |
//! | 5xx, or no answer | the relay is unavailable | 28 |
//! | 400 | Clift sent something the relay did not understand | 30 |
//!
//! The distinction that matters is the first row against the fourth. "Your
//! token is spent" and "the relay is down" call for completely different things
//! from the user, and conflating them is how somebody spends ten minutes
//! restarting a service because their five-minute-old token expired.

#![forbid(unsafe_code)]

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{PublishedObject, Relay};
use clift_core::universal::{ObjectId, RelaySettings};
use std::time::Duration;

/// How long any single relay request may take.
///
/// One number for everything, because every request here is small: an upload of
/// at most a few megabytes, or a download of the same. A user waiting on a
/// paste will have given up long before this, and the timeout exists so that a
/// black-holed connection ends in an error rather than a hang.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A relay reached over HTTP.
pub struct HttpRelay {
    agent: ureq::Agent,
    base_url: String,
}

impl HttpRelay {
    /// # Errors
    /// Never fails today; the signature leaves room for a client that has to
    /// validate something at construction time.
    #[must_use]
    pub fn new(settings: &RelaySettings) -> Self {
        let config = ureq::Agent::config_builder()
            // Statuses are answers, not errors: the mapping above needs to see
            // 404 and 413 as themselves rather than as one opaque failure.
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            // A relay that redirects is a relay doing something Clift did not
            // ask for, and following it would send ciphertext somewhere the
            // user did not configure.
            .max_redirects(0)
            .user_agent(concat!("clift/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base_url: settings.url().to_string(),
        }
    }

    fn object_url(&self, id: &ObjectId) -> String {
        format!("{}/v1/objects/{}", self.base_url, id.encoded())
    }

    /// The relay's health document, for `clift doctor`.
    ///
    /// # Errors
    /// Fails when the relay cannot be reached or does not answer with a
    /// document this build understands.
    pub fn health(&self) -> Result<RelayHealth, CliftError> {
        let mut response = self
            .agent
            .get(format!("{}/v1/health", self.base_url))
            .call()
            .map_err(|error| unreachable(&self.base_url, &error))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(status_error(status, &self.base_url, "the health check"));
        }
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| unreachable(&self.base_url, &error))?;
        let document: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| malformed_answer("the health document is not JSON", error))?;
        Ok(RelayHealth {
            objects: document
                .get("objects")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
            max_object_bytes: document
                .get("max_object_bytes")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
            max_ttl_seconds: document
                .get("max_ttl_seconds")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        })
    }
}

/// What a relay says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayHealth {
    pub objects: u64,
    pub max_object_bytes: u64,
    pub max_ttl_seconds: u64,
}

impl Relay for HttpRelay {
    fn publish(&self, sealed: &[u8], ttl: Duration) -> Result<PublishedObject, CliftError> {
        let url = format!("{}/v1/objects?ttl={}", self.base_url, ttl.as_secs());
        let mut response = self
            .agent
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .send(sealed)
            .map_err(|error| unreachable(&self.base_url, &error))?;

        let status = response.status().as_u16();
        if status != 201 && status != 200 {
            return Err(status_error(status, &self.base_url, "the upload"));
        }

        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| unreachable(&self.base_url, &error))?;
        let document: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| malformed_answer("the relay's answer is not JSON", error))?;

        let Some(id) = document
            .get("object_id")
            .and_then(serde_json::Value::as_str)
        else {
            return Err(CliftError::new(
                Stage::Relay,
                ErrorKind::RelayUnavailable,
                "the relay accepted the object but did not say what it is called",
            ));
        };
        let granted = document
            .get("ttl_seconds")
            .and_then(serde_json::Value::as_u64)
            .map_or(ttl, Duration::from_secs);

        Ok(PublishedObject {
            id: ObjectId::parse(id)?,
            ttl: granted,
        })
    }

    fn retrieve(&self, id: &ObjectId) -> Result<Vec<u8>, CliftError> {
        let mut response = self
            .agent
            .get(self.object_url(id))
            .call()
            .map_err(|error| unreachable(&self.base_url, &error))?;

        let status = response.status().as_u16();
        if status != 200 {
            return Err(status_error(status, &self.base_url, "the download"));
        }

        // Bounded read. The relay is not trusted to be honest about how much it
        // is sending, and an unbounded `read_to_end` on a hostile server is how
        // a fetch turns into an out-of-memory kill on somebody's VPS.
        response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map_err(|error| {
                CliftError::new(
                    Stage::Relay,
                    ErrorKind::RelayUnavailable,
                    "the object could not be downloaded in full",
                )
                .with_source(error)
            })
    }

    fn revoke(&self, id: &ObjectId) -> Result<(), CliftError> {
        let response = self
            .agent
            .delete(self.object_url(id))
            .call()
            .map_err(|error| unreachable(&self.base_url, &error))?;
        let status = response.status().as_u16();
        // 404 counts as success: the caller wanted the object gone, and it is.
        if status == 204 || status == 200 || status == 404 {
            return Ok(());
        }
        Err(status_error(status, &self.base_url, "the withdrawal"))
    }
}

/// The most any relay may hand back.
///
/// Above the largest object Clift will publish, with room for an envelope, and
/// far below anything that would hurt a small VPS. A relay sending more than
/// this is either broken or hostile, and both are reasons to stop reading.
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

fn unreachable(base_url: &str, error: &(impl std::fmt::Display + ?Sized)) -> CliftError {
    CliftError::new(
        Stage::Relay,
        ErrorKind::RelayUnavailable,
        format!("the relay at {base_url} could not be reached: {error}"),
    )
    .with_remedy(Remedy::new(
        "Check the relay is up, or send over SSH instead:",
        format!("curl -sS {base_url}/v1/health"),
    ))
}

/// Turns one HTTP status into the failure a user can act on.
fn status_error(status: u16, base_url: &str, what: &str) -> CliftError {
    match status {
        404 | 410 => CliftError::new(
            Stage::Relay,
            ErrorKind::TokenUnusable,
            "the relay has no object for this token: it expired, or it was already fetched",
        )
        .with_remedy(Remedy::new(
            "Tokens are single use. Copy the attachment again and paste a new one:",
            "clift paste --copy",
        )),
        413 => CliftError::new(
            Stage::Relay,
            ErrorKind::LimitExceeded,
            "the relay refused the object because it is too large",
        )
        .with_remedy(Remedy::new(
            "Send it over your own SSH connection instead:",
            "clift send --clipboard --to <target>",
        )),
        429 => CliftError::new(
            Stage::Relay,
            ErrorKind::RelayUnavailable,
            "the relay is rate limiting this client",
        )
        .with_remedy(Remedy::new(
            "Wait a moment and retry:",
            "clift paste --copy",
        )),
        400 => CliftError::new(
            Stage::Relay,
            ErrorKind::Internal,
            format!("the relay did not understand {what}; the two sides may be different versions"),
        )
        .with_remedy(Remedy::new("Compare the versions:", "clift --version")),
        _ => CliftError::new(
            Stage::Relay,
            ErrorKind::RelayUnavailable,
            format!("{what} failed: the relay at {base_url} answered {status}"),
        )
        .with_remedy(Remedy::new(
            "Check the relay is healthy:",
            format!("curl -sS {base_url}/v1/health"),
        )),
    }
}

fn malformed_answer(message: &str, error: serde_json::Error) -> CliftError {
    CliftError::new(
        Stage::Relay,
        ErrorKind::RelayUnavailable,
        message.to_string(),
    )
    .with_source(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping is the contract, and it is the part a user feels. Each row
    /// asserts the kind, which is what becomes the exit code.
    #[test]
    fn every_status_maps_to_the_failure_the_user_should_act_on() {
        let cases = [
            (404, ErrorKind::TokenUnusable),
            (410, ErrorKind::TokenUnusable),
            (413, ErrorKind::LimitExceeded),
            (429, ErrorKind::RelayUnavailable),
            (400, ErrorKind::Internal),
            (500, ErrorKind::RelayUnavailable),
            (502, ErrorKind::RelayUnavailable),
            (403, ErrorKind::RelayUnavailable),
        ];
        for (status, expected) in cases {
            let error = status_error(status, "https://relay.example.com", "the download");
            assert_eq!(error.kind(), expected, "status {status}");
            assert_eq!(error.stage(), Stage::Relay);
        }
    }

    #[test]
    fn a_spent_token_is_not_reported_as_a_broken_relay() {
        let spent = status_error(404, "https://relay.example.com", "the download");
        let broken = status_error(503, "https://relay.example.com", "the download");
        assert_ne!(spent.kind(), broken.kind());
        assert_ne!(spent.exit_code().as_u8(), broken.exit_code().as_u8());
    }

    #[test]
    fn every_relay_failure_offers_one_command() {
        for status in [404, 413, 429, 400, 500] {
            let error = status_error(status, "https://relay.example.com", "the upload");
            assert!(error.remedy().is_some(), "status {status} has no remedy");
        }
        assert!(
            unreachable("https://relay.example.com", "refused")
                .remedy()
                .is_some()
        );
    }

    #[test]
    fn the_object_url_is_built_from_the_id_alone() {
        let settings =
            RelaySettings::new("https://relay.example.com", 1024, Duration::from_secs(60))
                .unwrap_or_else(|error| panic!("{error}"));
        let relay = HttpRelay::new(&settings);
        let id = ObjectId::from_bytes([7; 16]);
        let url = relay.object_url(&id);
        assert_eq!(
            url,
            format!("https://relay.example.com/v1/objects/{}", id.encoded())
        );
        assert!(!url.contains('#'), "{url}");
        assert!(!url.contains('?'), "{url}");
    }
}

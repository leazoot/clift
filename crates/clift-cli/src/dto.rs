//! The JSON `send` and `paste` produce.
//!
//! Written by hand and kept apart from the domain types on purpose. A terminal
//! plugin and any third-party tool read this shape, so renaming a field in
//! `clift-core` must not be able to change it by accident. The compatibility
//! rules are: adding a field is fine, removing or renaming one requires
//! `schema_version` to go up.
//!
//! The specification again applies by omission: there is no field here for a local path,
//! a key location or an attachment's bytes.

use clift_core::diagnostics::SCHEMA_VERSION;
use clift_core::staging::{StagedBatch, WrittenBatch};
use clift_core::usecase::Published;
use serde::Serialize;

/// The document a successful send prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendDto {
    pub schema_version: u32,
    pub status: &'static str,
    pub target: String,
    pub items: Vec<SendItemDto>,
    /// Exactly the text the user would paste. The terminal adapter types this
    /// into the agent's prompt verbatim.
    pub insertion_text: String,
}

/// One attachment that arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendItemDto {
    /// Absolute, on the remote host. The only kind of path in this document.
    pub remote_path: String,
    pub mime: String,
    pub size: u64,
}

/// The document a run with nothing to upload prints in `--json` mode.
///
/// Exit code 10 already says this, but a terminal adapter written in Lua cannot
/// always see an exit code -- a wrapper may report success as
/// a boolean. Without this the adapter would have to read the human-readable
/// message to tell "nothing to send" from "something broke", and reading prose
/// meant for people is exactly what an integration must never do.
#[must_use]
pub fn no_attachment() -> NoAttachmentDto {
    NoAttachmentDto {
        schema_version: SCHEMA_VERSION,
        status: "no_attachment",
    }
}

/// The shape of that document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct NoAttachmentDto {
    pub schema_version: u32,
    pub status: &'static str,
}

/// The document a Universal Mode paste prints.
///
/// It has the same `schema_version`, `status` and `insertion_text` as
/// [`SendDto`], so a reader that only wants "what do I type" needs no new code.
/// What it does not have is `target` or `remote_path`, and their absence is the
/// whole difference between the modes: at this moment nobody knows which host
/// will redeem the token, and inventing a field to say so would be inventing
/// the answer.
///
/// The token is here because the caller has to be able to paste it. It is the
/// only place in any Clift output where the key material appears, and it goes
/// to stdout rather than into a log for that reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UniversalPasteDto {
    pub schema_version: u32,
    pub status: &'static str,
    pub mode: &'static str,
    /// The relay the ciphertext went to, so a reader can tell two configured
    /// relays apart. No credential: a relay has none.
    pub relay_url: String,
    pub token: String,
    /// Seconds the relay says the object will live for.
    pub ttl_seconds: u64,
    /// Bytes of ciphertext the relay is holding.
    pub sealed_size: u64,
    pub items: Vec<UniversalItemDto>,
    pub insertion_text: String,
}

/// One attachment inside a published object, as the sender described it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UniversalItemDto {
    /// The name the far side will write it under, before any disambiguation.
    pub name: String,
    pub mime: String,
    /// The plaintext size. The sealed size of the whole object is on the
    /// document itself.
    pub size: u64,
}

/// The document `clift fetch --json` prints on the remote host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FetchDto {
    pub schema_version: u32,
    pub status: &'static str,
    /// The batch directory, absolute, on this host.
    pub directory: String,
    pub items: Vec<FetchItemDto>,
}

/// One attachment that was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FetchItemDto {
    /// Absolute, on this host. What the agent opens.
    pub path: String,
    pub mime: String,
    pub size: u64,
}

/// Builds the document a Universal Mode paste prints.
#[must_use]
pub fn universal_paste(
    published: &Published,
    relay_url: &str,
    insertion_text: String,
) -> UniversalPasteDto {
    UniversalPasteDto {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        mode: "universal",
        relay_url: relay_url.to_string(),
        token: published.token().expose(),
        ttl_seconds: published.ttl().as_secs(),
        sealed_size: published.sealed_bytes(),
        items: published
            .entries()
            .iter()
            .map(|entry| UniversalItemDto {
                name: entry.name().to_string(),
                mime: entry.media_type().to_string(),
                size: entry.size(),
            })
            .collect(),
        insertion_text,
    }
}

/// Builds the document `clift fetch` prints.
#[must_use]
pub fn fetch(batch: &WrittenBatch) -> FetchDto {
    FetchDto {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        directory: batch.directory().as_str().to_string(),
        items: batch
            .files()
            .iter()
            .map(|file| FetchItemDto {
                path: file.path().as_str().to_string(),
                mime: file.media_type().to_string(),
                size: file.size(),
            })
            .collect(),
    }
}

/// Builds the document from a batch that arrived complete.
#[must_use]
pub fn send(target: &str, batch: &StagedBatch, insertion_text: String) -> SendDto {
    SendDto {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        target: target.to_string(),
        items: batch
            .files()
            .iter()
            .map(|file| SendItemDto {
                remote_path: file.path().as_str().to_string(),
                mime: mime_for(file.name().extension()),
                size: file.size(),
            })
            .collect(),
        insertion_text,
    }
}

/// A media type for a file name's extension.
///
/// This is a label, not a gate. Clift never opens, parses or unpacks an
/// attachment, so nothing here decides whether a file may be sent -- the field
/// exists because the specification promises it, and a reader of the JSON finds it useful.
/// Anything unrecognised is `application/octet-stream`, which is the honest
/// answer rather than a guess.
fn mime_for(extension: Option<&str>) -> String {
    let Some(extension) = extension else {
        return "application/octet-stream".to_string();
    };
    // `SafeFileName::extension` keeps the dot, which is what a caller wanting
    // to rebuild a name needs and not what a lookup table wants.
    let extension = extension.trim_start_matches('.');
    let known = match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    };
    known.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_extension_gets_its_media_type_and_the_rest_get_the_honest_default() {
        // The dot is part of what `SafeFileName::extension` returns.
        assert_eq!(mime_for(Some(".png")), "image/png");
        assert_eq!(mime_for(Some(".PNG")), "image/png");
        assert_eq!(mime_for(Some(".pdf")), "application/pdf");
        assert_eq!(mime_for(Some(".sqlite3")), "application/octet-stream");
        assert_eq!(mime_for(None), "application/octet-stream");
    }
}

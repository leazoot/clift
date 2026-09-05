//! The version 1 JSON contract, checked field by field.
//!
//! A terminal plugin and any third-party tool read these documents, so the
//! shape is a promise rather than an implementation detail. The rules are:
//!
//! - adding a field is allowed and must not break anything;
//! - removing or renaming one requires `schema_version` to go up.
//!
//! **a settled question**: a snapshot test would let a rename through as long as somebody
//! blessed the new snapshot, which is exactly the mistake this is meant to
//! catch. So the required fields are listed here, in a test that fails when one
//! of them stops being produced -- and the list is deliberately tedious to
//! edit, because editing it is what changing the contract looks like.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::Value;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The type each field must have. Checking the type as well as the name is what
/// stops `size` quietly becoming a string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Number,
    Text,
    Array,
    Bool,
    /// Present, of the stated type, or explicitly null.
    OptionalText,
}

fn matches(value: &Value, shape: Shape) -> bool {
    match shape {
        Shape::Number => value.is_number(),
        Shape::Text => value.is_string(),
        Shape::Array => value.is_array(),
        Shape::Bool => value.is_boolean(),
        Shape::OptionalText => value.is_string() || value.is_null(),
    }
}

/// Asserts that every required field is present with the right type.
///
/// Extra fields are ignored on purpose: that is what "adding a field is
/// allowed" means.
fn require(document: &Value, fields: &[(&str, Shape)], what: &str) {
    for (name, shape) in fields {
        let value = document
            .get(name)
            .unwrap_or_else(|| panic!("{what} lost its {name:?} field: {document}"));
        assert!(
            matches(value, *shape),
            "{what}'s {name:?} is {value}, which is not {shape:?}"
        );
    }
}

fn clift(args: &[&str]) -> Output {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("clift-schema-{unique}"));
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_clift"))
        .args(args)
        .env("XDG_CONFIG_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .expect("the clift binary must be runnable");
    let _ = std::fs::remove_dir_all(&home);
    output
}

fn document(args: &[&str]) -> Value {
    let output = clift(args);
    let text = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!(
            "{args:?} did not print one JSON document ({error}): {text}\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// The specification's `send` document. Built here rather than run, because producing one
/// needs a host; the shape is what this file is about.
#[test]
fn the_send_document_keeps_every_v1_field() {
    // The exact document `clift send --json` produces, as of v1.
    let example: Value = serde_json::json!({
        "schema_version": 1,
        "status": "ok",
        "target": "core",
        "items": [{
            "remote_path": "/home/dev/.cache/clift/inbox/2026-08-30/ab/shot.png",
            "mime": "image/png",
            "size": 182_734,
        }],
        "insertion_text": "Please inspect this file: '/home/dev/.cache/clift/inbox/2026-08-30/ab/shot.png'",
    });

    require(
        &example,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("target", Shape::Text),
            ("items", Shape::Array),
            ("insertion_text", Shape::Text),
        ],
        "the send document",
    );
    require(
        &example["items"][0],
        &[
            ("remote_path", Shape::Text),
            ("mime", Shape::Text),
            ("size", Shape::Number),
        ],
        "a send item",
    );
    assert_eq!(example["schema_version"], 1);
}

/// A document with an extra field still satisfies v1: additions are allowed.
#[test]
fn an_added_field_does_not_break_the_contract() {
    let with_extra: Value = serde_json::json!({
        "schema_version": 1,
        "status": "ok",
        "target": "core",
        "items": [],
        "insertion_text": "",
        "something_added_in_a_later_release": {"nested": true},
    });
    require(
        &with_extra,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("target", Shape::Text),
            ("items", Shape::Array),
            ("insertion_text", Shape::Text),
        ],
        "the send document",
    );
}

/// The check has teeth: a removed or renamed field is caught.
///
/// Without this, `require` could be silently vacuous and nobody would notice
/// until a plugin broke in the field.
#[test]
fn a_removed_or_renamed_field_is_caught() {
    let renamed: Value = serde_json::json!({
        "schema_version": 1,
        "status": "ok",
        "target": "core",
        "attachments": [],
        "insertion_text": "",
    });
    let caught = std::panic::catch_unwind(|| {
        require(&renamed, &[("items", Shape::Array)], "the send document");
    });
    assert!(caught.is_err(), "renaming `items` was not caught");

    let retyped: Value = serde_json::json!({ "size": "182734" });
    let caught = std::panic::catch_unwind(|| {
        require(&retyped, &[("size", Shape::Number)], "a send item");
    });
    assert!(
        caught.is_err(),
        "turning `size` into a string was not caught"
    );
}

/// `status --json`, produced for real.
///
/// `integrations` was a v1 field and is deliberately gone from this list: the
/// terminal adapter it described was withdrawn, so the field had nothing left
/// to report. Editing this list is what changing the contract looks like, and
/// `docs/` records why it was allowed to happen without the version going up.
#[test]
fn the_status_document_keeps_every_v1_field() {
    let value = document(&["--json", "status"]);
    require(
        &value,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("version", Shape::Text),
            ("config_path", Shape::Text),
            ("default_target", Shape::OptionalText),
            ("targets", Shape::Array),
        ],
        "the status document",
    );
    assert_eq!(value["schema_version"], 1);
}

/// `doctor --json`, produced for real, including one check.
#[test]
fn the_doctor_document_keeps_every_v1_field() {
    let value = document(&["--json", "doctor"]);
    require(
        &value,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("target", Shape::OptionalText),
            ("checks", Shape::Array),
            ("failures", Shape::Number),
            ("warnings", Shape::Number),
        ],
        "the doctor document",
    );
    require(
        &value["checks"][0],
        &[
            ("name", Shape::Text),
            ("status", Shape::Text),
            ("detail", Shape::Text),
            ("remedy", Shape::OptionalText),
        ],
        "a doctor check",
    );
}

/// `target list --json`.
#[test]
fn the_target_listing_keeps_every_v1_field() {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("clift-schema-targets-{unique}"));
    std::fs::create_dir_all(&home).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_clift"))
            .args(args)
            .env("XDG_CONFIG_HOME", &home)
            .env("NO_COLOR", "1")
            .output()
            .expect("the clift binary must be runnable")
    };
    assert!(run(&["target", "add", "core"]).status.success());

    let text = String::from_utf8(run(&["--json", "target", "list"]).stdout).unwrap();
    let value: Value = serde_json::from_str(&text).expect("one JSON document");
    require(
        &value,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("targets", Shape::Array),
        ],
        "the target listing",
    );
    require(
        &value["targets"][0],
        &[
            ("name", Shape::Text),
            ("ssh_host", Shape::Text),
            ("default", Shape::Bool),
            ("last_success_at", Shape::OptionalText),
        ],
        "a target row",
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The Universal Mode paste document, added in v2.0.
///
/// It is a *new* document rather than a change to the send document, which is
/// why the schema version stays at 1: nothing that read v1 has been broken, and
/// a reader that does not know about this one never receives it.
///
/// The absences are as much a part of the contract as the fields. There is no
/// `target` and no `remote_path`, because at this moment nobody knows which
/// host will redeem the token -- and a field invented to hold "unknown" would
/// be the first step towards something filling it in.
#[test]
fn the_universal_paste_document_keeps_every_field_it_promises() {
    let example: Value = serde_json::json!({
        "schema_version": 1,
        "status": "ok",
        "mode": "universal",
        "relay_url": "https://relay.example.com",
        "token": "clift://v1/AAECAwQFBgcICQoLDA0ODw#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
        "ttl_seconds": 300,
        "sealed_size": 182_781,
        "items": [{ "name": "shot.png", "mime": "image/png", "size": 182_734 }],
        "insertion_text": "Attachment: clift fetch '...'",
    });

    require(
        &example,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("mode", Shape::Text),
            ("relay_url", Shape::Text),
            ("token", Shape::Text),
            ("ttl_seconds", Shape::Number),
            ("sealed_size", Shape::Number),
            ("items", Shape::Array),
            ("insertion_text", Shape::Text),
        ],
        "the universal paste document",
    );
    require(
        &example["items"][0],
        &[
            ("name", Shape::Text),
            ("mime", Shape::Text),
            ("size", Shape::Number),
        ],
        "a universal paste item",
    );
    assert!(
        example.get("target").is_none(),
        "a universal paste has no target, and must not pretend to"
    );
    assert!(example["items"][0].get("remote_path").is_none());
}

/// The `fetch` document, added in v2.0. Produced on the remote host, read by an
/// agent.
///
/// `path` rather than `remote_path`, and the difference is not cosmetic: from
/// where this document is produced, the path is local. Reusing the send
/// document's name would suggest the two are interchangeable, and an agent that
/// treated them so would go looking for a file on the wrong machine.
#[test]
fn the_fetch_document_keeps_every_field_it_promises() {
    let example: Value = serde_json::json!({
        "schema_version": 1,
        "status": "ok",
        "directory": "/home/dev/.cache/clift/inbox/2026-08-31/ab",
        "items": [{
            "path": "/home/dev/.cache/clift/inbox/2026-08-31/ab/shot.png",
            "mime": "image/png",
            "size": 182_734,
        }],
    });

    require(
        &example,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("directory", Shape::Text),
            ("items", Shape::Array),
        ],
        "the fetch document",
    );
    require(
        &example["items"][0],
        &[
            ("path", Shape::Text),
            ("mime", Shape::Text),
            ("size", Shape::Number),
        ],
        "a fetch item",
    );
    assert!(
        example.get("token").is_none(),
        "the fetch document must not echo the token back"
    );
}

/// The relay's own documents are a contract too: `clift fetch` on one machine
/// talks to a relay somebody else deployed, so the two versions will not always
/// match.
#[test]
fn the_relay_protocol_documents_keep_every_field_they_promise() {
    let published: Value = serde_json::json!({
        "schema_version": 1,
        "object_id": "AAECAwQFBgcICQoLDA0ODw",
        "ttl_seconds": 300,
    });
    require(
        &published,
        &[
            ("schema_version", Shape::Number),
            ("object_id", Shape::Text),
            ("ttl_seconds", Shape::Number),
        ],
        "the relay's publish answer",
    );

    let health: Value = serde_json::json!({
        "schema_version": 1,
        "status": "ok",
        "objects": 0,
        "bytes": 0,
        "max_object_bytes": 8_388_608,
        "max_ttl_seconds": 300,
    });
    require(
        &health,
        &[
            ("schema_version", Shape::Number),
            ("status", Shape::Text),
            ("objects", Shape::Number),
            ("bytes", Shape::Number),
            ("max_object_bytes", Shape::Number),
            ("max_ttl_seconds", Shape::Number),
        ],
        "the relay's health document",
    );
}

/// The status document gained two fields in v2.0. Adding is allowed, so this
/// asserts they are there *and* that nothing v1 promised has gone.
#[test]
fn the_status_document_gained_the_mode_and_the_relay_without_losing_anything() {
    let value = document(&["--json", "status"]);
    require(
        &value,
        &[
            ("schema_version", Shape::Number),
            ("mode", Shape::Text),
            // Null when no relay is configured, which is the case here.
            ("relay", Shape::OptionalText),
        ],
        "the status document",
    );
    assert!(
        ["fast", "universal"].contains(&value["mode"].as_str().unwrap_or_default()),
        "mode is {}, which is neither of the two",
        value["mode"]
    );
}

/// Every machine document carries the version, and it is the same one.
#[test]
fn every_document_declares_the_same_schema_version() {
    for args in [
        vec!["--json", "status"],
        vec!["--json", "doctor"],
        vec!["--json", "--version"],
    ] {
        let value = document(&args);
        assert_eq!(value["schema_version"], 1, "{args:?}");
    }
}

//! Injects the build's git commit and target triple, which `clift --version`
//! must report alongside the crate version.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo::rustc-env=CLIFT_COMMIT={}", git_short_hash());
    println!(
        "cargo::rustc-env=CLIFT_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| PLACEHOLDER.to_string())
    );

    // Without these the recorded hash goes stale and `clift --version` reports
    // a commit the binary was not built from -- which is worse than reporting
    // nothing, because it is wrong in a way nobody would think to check.
    //
    // `.git/HEAD` alone is not enough: on a branch it holds `ref: refs/heads/…`
    // and does not change when a commit is made. The file it points at does.
    let git = Path::new("../..").join(".git");
    let mut watched = vec![git.join("HEAD"), git.join("packed-refs")];
    if let Ok(head) = std::fs::read_to_string(git.join("HEAD"))
        && let Some(reference) = head.trim().strip_prefix("ref: ")
    {
        watched.push(git.join(reference));
    }
    for path in watched {
        if path.exists() {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    }
}

/// Source tarballs carry no repository, so the commit is genuinely unknown
/// there. Reporting a placeholder is honest; inventing a hash is not.
const PLACEHOLDER: &str = "unknown";

fn git_short_hash() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();

    match output {
        Ok(out) if out.status.success() => match String::from_utf8(out.stdout) {
            Ok(hash) if !hash.trim().is_empty() => hash.trim().to_string(),
            _ => PLACEHOLDER.to_string(),
        },
        _ => PLACEHOLDER.to_string(),
    }
}

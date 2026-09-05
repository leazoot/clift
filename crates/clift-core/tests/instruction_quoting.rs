//! The quoting in the `instruction` profile, checked by asking a shell.
//!
//! Reasoning about quoting rules is how quoting bugs are written. This asks
//! `/bin/sh` to read the rendered text back and compares it with what went in.
//!
//! It lives here rather than beside the code because `clift-core` may not
//! reference the process module at all -- `scripts/check-architecture.sh`
//! enforces that, and the rule is worth more than the convenience.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::domain::RemotePath;
use clift_core::format::render;

fn quoted_path(value: &str) -> String {
    let path = RemotePath::new(value).unwrap();
    let text = render(std::slice::from_ref(&path));
    text.strip_prefix("Please inspect this file: ")
        .expect("the single-file form")
        .to_string()
}

#[test]
fn a_shell_reads_every_awkward_path_back_unchanged() {
    for input in [
        "/home/dev/inbox/plain.png",
        "/home/dev/inbox/my screenshot.png",
        "/home/dev/inbox/it's here.png",
        "/home/dev/inbox/$HOME and `whoami`.png",
        "/home/dev/inbox/名字 with spaces.png",
        "/home/dev/inbox/semi;colon&and|pipe.png",
        "/home/dev/inbox/star*and?question.png",
        "/home/dev/inbox/quote\"double.png",
        "/home/dev/inbox/back\\slash.png",
    ] {
        let quoted = quoted_path(input);
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .expect("sh must be runnable");
        assert!(
            output.status.success(),
            "the shell could not even parse {quoted}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            input,
            "the shell did not read {quoted} back as {input}"
        );
    }
}

/// The multi-file form quotes every line the same way.
#[test]
fn every_line_of_the_multi_file_form_is_one_shell_word() {
    let paths = [
        RemotePath::new("/home/dev/inbox/one file.png").unwrap(),
        RemotePath::new("/home/dev/inbox/it's two.pdf").unwrap(),
    ];
    let text = render(&paths);

    let lines: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(lines.len(), 2, "{text}");
    for (line, expected) in lines.iter().zip([
        "/home/dev/inbox/one file.png",
        "/home/dev/inbox/it's two.pdf",
    ]) {
        let quoted = line.strip_prefix("- ").expect("each line is a bullet");
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .expect("sh must be runnable");
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }
}

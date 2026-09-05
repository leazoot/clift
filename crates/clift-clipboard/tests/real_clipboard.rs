//! The macOS clipboard, read for real.
//!
//! These tests put something on the **actual** system clipboard and read it
//! back, because that is the only way to know that Clift reads what macOS
//! really puts there. The specification forbids claiming support for a platform on the
//! strength of a constructed fixture.
//!
//! They are opt-in, because they overwrite whatever the person running them had
//! copied:
//!
//! ```text
//! CLIFT_REAL_CLIPBOARD=1 cargo test -p clift-clipboard --test real_clipboard
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_clipboard::MacClipboard;
use clift_core::ports::ClipboardSource;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

/// The pasteboard is one shared resource, and these tests both write to it and
/// read from it. Running two at once made the harness fault inside AppKit, so
/// they take turns.
static PASTEBOARD: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    PASTEBOARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Straight to the file descriptor, so a skip is visible without `--nocapture`
/// and cannot be mistaken for a pass.
fn announce(line: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{line}");
    let _ = stderr.flush();
}

fn skip(test: &str) -> bool {
    if std::env::var_os("CLIFT_REAL_CLIPBOARD").is_some() {
        return false;
    }
    announce(&format!(
        "SKIPPED {test}: CLIFT_REAL_CLIPBOARD is not set (it overwrites the \
         real clipboard); this test proved nothing"
    ));
    true
}

fn put_text(text: &str) {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .expect("pbcopy is part of macOS");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

/// A real screenshot, taken by macOS itself and placed on the clipboard exactly
/// as `Cmd+Shift+3` would.
fn put_screenshot() {
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-c"])
        .status()
        .expect("screencapture is part of macOS");
    assert!(status.success(), "screencapture failed");
}

/// Puts file references on the pasteboard the way Finder does.
///
/// AppKit's own `writeObjects:` with `NSURL`s, which produces `public.file-url`
/// -- byte for byte the representation a Finder copy leaves behind. It is run
/// through `osascript -l JavaScript` so that the writing is done by the system
/// frameworks rather than by the code under test.
///
/// A genuine `Cmd+C` in Finder cannot be driven from here: it needs
/// Accessibility permission this environment does not have. That check
/// belongs to a manual run.
fn put_files(paths: &[&str]) {
    // The array is built as an NSMutableArray rather than as a JavaScript array
    // handed to `$()`: the bridge's conversion of an array of Objective-C
    // objects dropped elements intermittently, which made the test lie about
    // how many files had been copied.
    const SCRIPT: &str = r"
        ObjC.import('AppKit');
        // With `-e`, osascript's own arguments occupy 0..4 and the caller's
        // begin at 5.
        const args = $.NSProcessInfo.processInfo.arguments;
        const urls = $.NSMutableArray.array;
        for (let i = 5; i < args.count; i++) {
            urls.addObject($.NSURL.fileURLWithPath(args.objectAtIndex(i)));
        }
        const pb = $.NSPasteboard.generalPasteboard;
        pb.clearContents;
        if (!pb.writeObjects(urls)) { throw new Error('writeObjects refused the list'); }
        // The items are read back before this process exits. Without that, the
        // last one was intermittently unreadable by the next process to look:
        // a writer that has not seen its own items on the pasteboard has not
        // finished writing them, and a test that returns there reports a fault
        // in the reader that is not there.
        const items = pb.pasteboardItems;
        let readable = 0;
        for (let i = 0; i < items.count; i++) {
            if (!items.objectAtIndex(i).stringForType('public.file-url').isNil()) {
                readable++;
            }
        }
        String(readable);
    ";
    let mut command = Command::new("/usr/bin/osascript");
    command.args(["-l", "JavaScript", "-e", SCRIPT]);
    for path in paths {
        command.arg(path);
    }
    let output = command.output().expect("osascript is part of macOS");
    assert!(
        output.status.success(),
        "osascript failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The script echoes how many URLs it actually wrote. Without this, a
    // silently dropped one looks like a bug in the code under test.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        paths.len().to_string(),
        "the pasteboard did not receive every file"
    );
}

#[test]
fn plain_text_is_read_as_text_and_nothing_else() {
    if skip("plain_text_is_read_as_text_and_nothing_else") {
        return;
    }
    let _turn = exclusive();
    put_text("a line of text, nothing more");

    let snapshot = MacClipboard::new().read_snapshot().unwrap();
    assert_eq!(
        snapshot.text.as_deref(),
        Some("a line of text, nothing more")
    );
    assert!(
        snapshot.files.is_empty() && snapshot.images.is_empty(),
        "plain text must not look like an attachment: {snapshot:?}"
    );
}

#[test]
fn a_real_screenshot_is_read_and_written_to_a_private_file() {
    if skip("a_real_screenshot_is_read_and_written_to_a_private_file") {
        return;
    }
    let _turn = exclusive();
    put_screenshot();

    let clipboard = MacClipboard::new();
    let snapshot = clipboard.read_snapshot().unwrap();
    assert_eq!(snapshot.images.len(), 1, "{snapshot:?}");

    let image = &snapshot.images[0];
    assert!(image.path.exists(), "{}", image.path.display());
    assert!(
        !image.path.starts_with("/tmp"),
        "a screenshot must not be written to a world-writable directory: {}",
        image.path.display()
    );

    let bytes = std::fs::read(&image.path).unwrap();
    assert!(bytes.len() > 1024, "a screenshot of a screen is not tiny");
    let recognised = Command::new("/usr/bin/file")
        .arg("--brief")
        .arg(&image.path)
        .output()
        .expect("file(1) is part of macOS");
    let description = String::from_utf8_lossy(&recognised.stdout).to_lowercase();
    assert!(
        description.contains("png") || description.contains("tiff"),
        "file(1) does not recognise what Clift wrote: {description}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&image.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a clipboard image must be private");
    }
}

/// The temporary file lives exactly as long as the clipboard that made it.
#[test]
fn a_clipboard_image_is_removed_when_the_clipboard_is_dropped() {
    if skip("a_clipboard_image_is_removed_when_the_clipboard_is_dropped") {
        return;
    }
    let _turn = exclusive();
    put_screenshot();

    let path = {
        let clipboard = MacClipboard::new();
        let snapshot = clipboard.read_snapshot().unwrap();
        let path = snapshot.images[0].path.clone();
        assert!(path.exists());
        assert_eq!(clipboard.retained_files(), 1);
        path
    };
    assert!(
        !path.exists(),
        "the clipboard image outlived its reader: {}",
        path.display()
    );
}

#[test]
fn files_copied_in_finder_are_read_as_paths() {
    if skip("files_copied_in_finder_are_read_as_paths") {
        return;
    }
    let _turn = exclusive();
    let directory = std::env::temp_dir().join(format!("clift-clip-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let first = directory.join("one.txt");
    let second = directory.join("名字 with spaces.txt");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();

    put_files(&[first.to_str().unwrap(), second.to_str().unwrap()]);

    let snapshot = MacClipboard::new().read_snapshot().unwrap();
    assert_eq!(snapshot.files.len(), 2, "{snapshot:?}");
    assert!(snapshot.files.contains(&first), "{snapshot:?}");
    assert!(
        snapshot.files.contains(&second),
        "a path with spaces and non-ASCII must survive: {snapshot:?}"
    );
    assert!(
        snapshot.images.is_empty(),
        "a file list is not an image: {snapshot:?}"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// Puts one raw representation on the pasteboard under a chosen UTI.
///
/// Used to reproduce a source that offers **only** TIFF, which is what makes
/// the conversion path reachable. The data itself is produced by macOS: a real
/// screen capture, re-encoded by `sips`.
fn put_raw(identifier: &str, path: &str) {
    const SCRIPT: &str = r"
        ObjC.import('AppKit');
        // With `-e`, osascript's own arguments occupy 0..4.
        const args = $.NSProcessInfo.processInfo.arguments;
        const uti = ObjC.unwrap(args.objectAtIndex(5));
        const file = ObjC.unwrap(args.objectAtIndex(6));
        const data = $.NSData.dataWithContentsOfFile(file);
        const pb = $.NSPasteboard.generalPasteboard;
        pb.clearContents;
        pb.setDataForType(data, uti);
    ";
    let output = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", SCRIPT, identifier, path])
        .output()
        .expect("osascript is part of macOS");
    assert!(
        output.status.success(),
        "osascript failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A real screen capture, saved to a file in the requested format by macOS.
fn capture_as(directory: &std::path::Path, format: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("shot.{format}"));
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", format])
        .arg(&path)
        .status()
        .expect("screencapture is part of macOS");
    assert!(status.success(), "screencapture failed");
    path
}

fn describe(path: &std::path::Path) -> String {
    let output = Command::new("/usr/bin/file")
        .arg("--brief")
        .arg(path)
        .output()
        .expect("file(1) is part of macOS");
    String::from_utf8_lossy(&output.stdout).to_lowercase()
}

/// `sips` reports the pixel dimensions macOS itself sees.
fn dimensions(path: &std::path::Path) -> (u32, u32) {
    let output = Command::new("/usr/bin/sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(path)
        .output()
        .expect("sips is part of macOS");
    let text = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| -> u32 {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(key))
            .and_then(|rest| rest.trim().strip_prefix(':'))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or_else(|| panic!("sips did not report {key}: {text}"))
    };
    (value("pixelWidth"), value("pixelHeight"))
}

/// A screenshot offers PNG and TIFF at once; Clift must take the PNG.
///
/// Not a preference: the TIFF of the same picture is several times the size,
/// and it is the agent's connection that pays for the difference.
#[test]
fn a_screenshot_offering_both_png_and_tiff_is_taken_as_png() {
    if skip("a_screenshot_offering_both_png_and_tiff_is_taken_as_png") {
        return;
    }
    let _turn = exclusive();
    put_screenshot();

    let clipboard = MacClipboard::new();
    let snapshot = clipboard.read_snapshot().unwrap();
    let image = &snapshot.images[0];
    assert_eq!(image.mime, "image/png");
    assert!(
        describe(&image.path).contains("png"),
        "{}",
        describe(&image.path)
    );
}

/// A source that offers only TIFF is converted, losslessly, to PNG.
#[test]
fn a_tiff_only_clipboard_is_converted_to_a_png_of_the_same_size() {
    if skip("a_tiff_only_clipboard_is_converted_to_a_png_of_the_same_size") {
        return;
    }
    let _turn = exclusive();
    let directory = std::env::temp_dir().join(format!("clift-tiff-{}", std::process::id()));
    let tiff = capture_as(&directory, "tiff");
    assert!(describe(&tiff).contains("tiff"), "{}", describe(&tiff));
    let original = dimensions(&tiff);

    put_raw("public.tiff", tiff.to_str().unwrap());

    let clipboard = MacClipboard::new();
    let snapshot = clipboard.read_snapshot().unwrap();
    assert_eq!(snapshot.images.len(), 1, "{snapshot:?}");
    let image = &snapshot.images[0];

    assert_eq!(image.mime, "image/png");
    assert!(
        describe(&image.path).contains("png"),
        "file(1) does not see a PNG: {}",
        describe(&image.path)
    );
    assert_eq!(
        dimensions(&image.path),
        original,
        "the conversion changed the picture's size"
    );
    assert!(
        image.path.extension().is_some_and(|ext| ext == "png"),
        "{}",
        image.path.display()
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// A JPEG is forwarded as it stands. Re-encoding it could only lose more.
#[test]
fn a_jpeg_is_passed_through_without_being_re_encoded() {
    if skip("a_jpeg_is_passed_through_without_being_re_encoded") {
        return;
    }
    let _turn = exclusive();
    let directory = std::env::temp_dir().join(format!("clift-jpeg-{}", std::process::id()));
    let jpeg = capture_as(&directory, "jpg");
    assert!(describe(&jpeg).contains("jpeg"), "{}", describe(&jpeg));
    let original = std::fs::read(&jpeg).unwrap();

    put_raw("public.jpeg", jpeg.to_str().unwrap());

    let clipboard = MacClipboard::new();
    let snapshot = clipboard.read_snapshot().unwrap();
    let image = &snapshot.images[0];
    assert_eq!(image.mime, "image/jpeg");
    assert_eq!(
        std::fs::read(&image.path).unwrap(),
        original,
        "the JPEG was altered on its way through"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

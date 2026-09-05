//! A process that holds a scratch file open and waits to be interrupted.
//!
//! Exists so that the `Ctrl+C` cleanup can be tested the only way it can
//! honestly be tested: by sending a real signal to a real process and looking
//! at the filesystem afterwards. A unit test cannot do that, because the whole
//! point is what happens when `Drop` does *not* run.
//!
//! It prints the path it created and then waits. The test sends SIGINT.

fn main() {
    clift_core::runtime::remove_scratch_files_on_signal()
        .unwrap_or_else(|error| panic!("could not install the signal handler: {error}"));

    let scratch = clift_core::runtime::ScratchFile::create("signal-test", "bin", b"payload")
        .unwrap_or_else(|error| panic!("could not create the scratch file: {error}"));

    println!("{}", scratch.path().display());
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    // Long enough that the test's signal always arrives first; if it somehow
    // does not, the process ends on its own rather than hanging a test run.
    std::thread::sleep(std::time::Duration::from_secs(30));
}

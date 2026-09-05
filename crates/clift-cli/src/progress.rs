//! A live line for the seconds a remote round trip takes.
//!
//! `setup` opens five or six SSH sessions, and on a distant host each one costs
//! three to five seconds. Printing nothing for half a minute is indistinguishable
//! from being hung, which is what this module exists to fix.
//!
//! Three rules shape it, and none of them are negotiable:
//!
//! - **stderr only.** Whatever reaches stdout can be typed into an agent's
//!   prompt, so a spinner frame there would land in somebody's conversation.
//! - **Nothing before half a second** (the output rules). A step that
//!   finishes quickly must leave no trace at all, so the animation starts late
//!   and erases itself when the step ends. What remains on screen afterwards is
//!   byte for byte what Clift printed before this module existed.
//! - **Only on a terminal.** Piped or redirected, there is no animation, so a
//!   script's stderr stays diffable and no escape sequence reaches a log file.
//!
//! The label always names the remote operation actually in flight, because it
//! is produced by the wrapper around the transport port rather than by a script
//! of what `setup` is expected to do. A step that hangs therefore names the
//! thing that is hanging.

use clift_core::domain::RemotePath;
use clift_core::error::CliftError;
use clift_core::ports::{ProbeReport, RemoteEntry, RemoteFs, RemoteUpload, TransportTarget};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Braille frames: one glyph wide in every terminal font, so the line never
/// reflows as it animates.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How often a new frame is drawn.
const TICK: Duration = Duration::from_millis(90);

/// Nothing is drawn before this. Anything faster is not worth a spinner, and
/// drawing one would make a quick command flicker.
const QUIET: Duration = Duration::from_millis(500);

/// Past this, the elapsed time joins the label: "still working" is reassuring
/// for two seconds and useless for twenty.
const SHOW_ELAPSED: Duration = Duration::from_secs(2);

/// Return to column zero and clear the line. Written before every frame, so a
/// short label cannot leave the tail of a longer one behind it.
const ERASE: &str = "\r\u{1b}[2K";

/// Draws one line, in place, for whatever is currently in flight.
pub struct Spinner {
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

struct State {
    activity: Option<(String, Instant)>,
    stop: bool,
    on_screen: bool,
    tick: usize,
    out: Box<dyn Write + Send>,
}

impl Spinner {
    /// A spinner that draws on stderr, or one that does nothing at all.
    ///
    /// `enabled` is the caller's decision, not this module's: it belongs with
    /// the rest of the output policy in [`crate::output::Reporter`].
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        if enabled {
            Self::writing_to(Box::new(std::io::stderr()))
        } else {
            Self {
                shared: None,
                worker: None,
            }
        }
    }

    fn writing_to(out: Box<dyn Write + Send>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                activity: None,
                stop: false,
                on_screen: false,
                tick: 0,
                out,
            }),
            wake: Condvar::new(),
        });
        let worker = std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || animate(&shared)
        });
        Self {
            shared: Some(shared),
            worker: Some(worker),
        }
    }

    /// Says what is happening now. Replaces whatever was being shown.
    pub fn begin(&self, label: String) {
        let Some(shared) = &self.shared else {
            return;
        };
        let mut state = lock(&shared.state);
        erase(&mut state);
        state.activity = Some((label, Instant::now()));
        // The clock restarts, so the new step gets its own quiet half second
        // rather than inheriting the previous one's.
        shared.wake.notify_all();
    }

    /// Says that nothing is happening, and takes the line back off the screen.
    pub fn end(&self) {
        let Some(shared) = &self.shared else {
            return;
        };
        let mut state = lock(&shared.state);
        erase(&mut state);
        state.activity = None;
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            let mut state = lock(&shared.state);
            state.stop = true;
            erase(&mut state);
            drop(state);
            shared.wake.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            // A panicking worker must not turn a successful send into a panic
            // of its own; the line it failed to erase is the whole damage.
            let _ = worker.join();
        }
    }
}

/// The drawing loop. Wakes on its own to advance the frame, and immediately
/// when the label changes or the spinner is dropped.
fn animate(shared: &Shared) {
    let mut state = lock(&shared.state);
    loop {
        if state.stop {
            return;
        }
        if let Some((label, started)) = state.activity.clone() {
            let elapsed = started.elapsed();
            if elapsed >= QUIET {
                let text = line(state.tick, &label, elapsed);
                state.tick = state.tick.wrapping_add(1);
                let _ = state.out.write_all(text.as_bytes());
                let _ = state.out.flush();
                state.on_screen = true;
            }
        }
        let (next, _) = shared
            .wake
            .wait_timeout(state, TICK)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = next;
    }
}

/// One frame, ready to write.
fn line(tick: usize, label: &str, elapsed: Duration) -> String {
    let frame = FRAMES[tick % FRAMES.len()];
    if elapsed >= SHOW_ELAPSED {
        format!("{ERASE}{frame} {label}… {}s", elapsed.as_secs())
    } else {
        format!("{ERASE}{frame} {label}…")
    }
}

/// Clears the line, but only if there is something on it: an unconditional
/// erase would wipe out the last thing `Reporter` printed.
fn erase(state: &mut State) {
    if !state.on_screen {
        return;
    }
    let _ = state.out.write_all(ERASE.as_bytes());
    let _ = state.out.flush();
    state.on_screen = false;
}

fn lock(mutex: &Mutex<State>) -> MutexGuard<'_, State> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A transport that says what it is doing before it does it.
///
/// It adds no behaviour of its own: every method hands straight through, and
/// the only thing it knows is how to describe one remote operation in a few
/// words. Progress is a presentation concern, which is why it lives here in the
/// composition root instead of as a parameter threaded through the use cases.
pub struct Narrating<'a, T> {
    inner: &'a T,
    spinner: &'a Spinner,
}

impl<'a, T> Narrating<'a, T> {
    pub const fn new(inner: &'a T, spinner: &'a Spinner) -> Self {
        Self { inner, spinner }
    }

    fn narrate<R>(&self, label: String, action: impl FnOnce(&T) -> R) -> R {
        self.spinner.begin(label);
        let outcome = action(self.inner);
        self.spinner.end();
        outcome
    }
}

impl<T: RemoteFs> RemoteFs for Narrating<'_, T> {
    fn probe(&self, target: &TransportTarget) -> Result<ProbeReport, CliftError> {
        self.narrate(format!("Reaching {}", target.ssh_host()), |inner| {
            inner.probe(target)
        })
    }

    fn resolve_home(&self, target: &TransportTarget) -> Result<RemotePath, CliftError> {
        self.narrate(
            format!("Asking {} where home is", target.ssh_host()),
            |inner| inner.resolve_home(target),
        )
    }

    fn resolve_cache_home(
        &self,
        target: &TransportTarget,
    ) -> Result<Option<RemotePath>, CliftError> {
        self.narrate(
            format!("Asking {} for its cache directory", target.ssh_host()),
            |inner| inner.resolve_cache_home(target),
        )
    }

    fn ensure_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
    ) -> Result<(), CliftError> {
        self.narrate(
            format!("Creating a directory on {}", target.ssh_host()),
            |inner| inner.ensure_dir(target, path, mode),
        )
    }

    fn stat(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Option<RemoteEntry>, CliftError> {
        self.narrate(
            format!("Checking {} on {}", leaf(path), target.ssh_host()),
            |inner| inner.stat(target, path),
        )
    }

    fn list_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Vec<RemoteEntry>, CliftError> {
        self.narrate(
            format!("Listing a directory on {}", target.ssh_host()),
            |inner| inner.list_dir(target, path),
        )
    }

    fn remove(&self, target: &TransportTarget, path: &RemotePath) -> Result<(), CliftError> {
        self.narrate(
            format!("Removing {} on {}", leaf(path), target.ssh_host()),
            |inner| inner.remove(target, path),
        )
    }
}

impl<T: RemoteUpload> RemoteUpload for Narrating<'_, T> {
    fn upload_atomic(
        &self,
        target: &TransportTarget,
        source: &Path,
        destination: &RemotePath,
    ) -> Result<u64, CliftError> {
        self.narrate(
            format!("Uploading {} to {}", leaf(destination), target.ssh_host()),
            |inner| inner.upload_atomic(target, source, destination),
        )
    }
}

/// The last component of a remote path.
///
/// The full path is often longer than the terminal is wide, and a line that
/// wraps cannot be erased with one escape sequence -- the tail would be left
/// behind on the row above.
///
/// Only files are named this way. A directory's last component is a batch
/// identifier as often as it is a word, and thirty-two characters of hex tell
/// the reader nothing, so `ensure_dir` and `list_dir` say what they are doing
/// without saying what to.
fn leaf(path: &RemotePath) -> &str {
    path.as_str().rsplit('/').next().unwrap_or(path.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer the tests can read back, shared with the drawing thread.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        fn contents(&self) -> String {
            let bytes = self.0.lock().unwrap_or_else(|p| p.into_inner());
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_disabled_spinner_writes_nothing_and_starts_no_thread() {
        let spinner = Spinner::new(false);
        assert!(spinner.worker.is_none());
        spinner.begin("Reaching core".to_string());
        spinner.end();
        // Nowhere to write to, and nothing that could have written.
        assert!(spinner.shared.is_none());
    }

    /// The rule from the output rules: under half a second, no
    /// animation at all.
    #[test]
    fn a_step_that_finishes_quickly_leaves_no_trace() {
        let buffer = Buffer::default();
        let spinner = Spinner::writing_to(Box::new(buffer.clone()));
        spinner.begin("Reaching core".to_string());
        std::thread::sleep(Duration::from_millis(200));
        spinner.end();
        drop(spinner);

        assert_eq!(
            buffer.contents(),
            "",
            "a fast step must not flicker a spinner"
        );
    }

    #[test]
    fn a_slow_step_is_drawn_and_then_erased_again() {
        let buffer = Buffer::default();
        let spinner = Spinner::writing_to(Box::new(buffer.clone()));
        spinner.begin("Reaching core".to_string());
        std::thread::sleep(QUIET + Duration::from_millis(250));
        let while_running = buffer.contents();
        spinner.end();
        drop(spinner);

        assert!(
            while_running.contains("Reaching core…"),
            "the label names what is happening: {while_running:?}"
        );
        assert!(
            FRAMES.iter().any(|frame| while_running.contains(frame)),
            "an animation frame should have been drawn: {while_running:?}"
        );
        assert!(
            buffer.contents().ends_with(ERASE),
            "the line must be taken back off the screen when the step ends"
        );
    }

    #[test]
    fn the_elapsed_time_appears_only_once_the_wait_is_worth_reporting() {
        assert_eq!(
            line(0, "Reaching core", Duration::from_millis(600)),
            format!("{ERASE}⠋ Reaching core…")
        );
        assert_eq!(
            line(0, "Reaching core", Duration::from_secs(4)),
            format!("{ERASE}⠋ Reaching core… 4s")
        );
    }

    #[test]
    fn frames_advance_and_wrap() {
        assert!(line(0, "x", QUIET).starts_with(&format!("{ERASE}⠋")));
        assert!(line(1, "x", QUIET).starts_with(&format!("{ERASE}⠙")));
        assert!(
            line(FRAMES.len(), "x", QUIET).starts_with(&format!("{ERASE}⠋")),
            "the frame index wraps rather than panicking"
        );
    }

    /// A wrapped line cannot be erased with one escape sequence, so the label
    /// carries the file name rather than the whole path.
    #[test]
    fn a_label_names_the_leaf_not_the_whole_path() {
        let path = RemotePath::new("/home/dev/.cache/clift/inbox/2026-08-30/abc/shot.png")
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(leaf(&path), "shot.png");
    }
}

//! Putting the instruction in front of the user without a terminal adapter.
//!
//! This is the piece that makes Universal Mode work in a terminal Clift has
//! never heard of. There is no plugin, no configuration file to patch and no
//! API to call: the operating system is asked to deliver the text itself, one
//! character at a time, to whatever window has focus. A terminal that accepts
//! typing can therefore receive it, which is the whole of the compatibility
//! claim v2.0 makes.
//!
//! **The clipboard is not involved, and that is the point.** The first version
//! took the shorter road: put the text on the clipboard, send the paste key,
//! wait a quarter of a second and put the user's screenshot back. That wait is
//! a race nothing here can win, because the three steps it waits out -- the
//! window server delivers the key, the application notices, the application
//! reads the clipboard -- are not observable from this process. Restore too
//! early and the user pastes their screenshot instead of the token; never
//! restore and the screenshot is gone. Typing the characters removes the race
//! by removing the borrowing: the clipboard still holds what the user copied,
//! before and after, byte for byte.
//!
//! Two things this deliberately does **not** do.
//!
//! - **It does not intercept anything.** Nothing is hooked, no key is captured
//!   and the user's own paste key keeps working exactly as it did. This code
//!   only ever *sends* keystrokes, and only when the user asked it to.
//! - **It does not pretend.** Synthesising input needs a permission the user
//!   grants explicitly, and on a machine where that has not been granted the
//!   honest answer is [`Availability::NeedsPermission`] and a fall back to
//!   `--copy`. Reporting a successful injection that did not happen would be
//!   the worst failure this crate could have: the user would go looking for a
//!   paste that never arrived.

// Synthesising keystrokes means calling into CoreGraphics, so this crate joins
// clift-clipboard as one of the two allowed to use `unsafe`. Every block must
// state the preconditions that make it sound.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod autostart;
pub mod hotkey;

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};

/// Whether this machine can be asked to deliver a paste keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// It can. This says the permission is granted, not that the keystroke will
    /// land where the user expects -- that depends on which window has focus,
    /// which nothing here can know.
    Ready,
    /// The mechanism exists but the operating system has not been told to allow
    /// it. Carries the exact steps, because "grant accessibility permission" is
    /// not findable advice on its own.
    NeedsPermission(String),
    /// Nothing here can do it on this platform. Carries why.
    Unsupported(String),
}

impl Availability {
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Availability::Ready)
    }

    /// The one line to show a user who asked for `--inject` and cannot have it.
    #[must_use]
    pub fn explanation(&self) -> Option<&str> {
        match self {
            Availability::Ready => None,
            Availability::NeedsPermission(text) | Availability::Unsupported(text) => Some(text),
        }
    }
}

/// The one command that opens the place where this permission is granted.
///
/// Platform knowledge, so it lives with the platform code. `doctor` needs to
/// hand the user something to run rather than a paragraph to follow, and only
/// this crate knows which pane of which settings application that is. `None`
/// where there is no such place, which is every platform that does not gate
/// this in the first place.
#[must_use]
pub fn permission_command() -> Option<&'static str> {
    platform::permission_command()
}

/// Whether [`type_into_focused_window`] would work right now.
#[must_use]
pub fn availability() -> Availability {
    platform::availability()
}

/// Like [`availability`], but where the platform gates keystrokes behind a
/// permission it can ask for, asks for it.
///
/// On macOS this is the system's own dialog ("clift would like to control this
/// computer using accessibility features"), shown by the operating system and
/// answered in System Settings; nothing here can grant anything. Asked once,
/// when a helper starts, rather than on every press: the answer is the user's
/// to give in their own time, and a dialog per keystroke would be a nag.
/// Everywhere else this is exactly [`availability`].
#[must_use]
pub fn request_permission() -> Availability {
    platform::request_permission()
}

/// Whether the user is holding a modifier key at this instant.
///
/// Exposed for diagnostics rather than for control flow: a caller that wants
/// the keystrokes to land should call [`type_into_focused_window`], which waits
/// on its own.
#[must_use]
pub fn modifiers_are_held() -> bool {
    platform::modifiers_are_held()
}

/// The longest a paste will wait for the user to release the keys they are
/// holding before refusing to send anything.
///
/// Two seconds is far longer than a key press lasts and short enough that a
/// user who has genuinely wedged a modifier down gets an answer rather than a
/// hang.
const RELEASE_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Waits until no modifier key is physically down.
///
/// This exists because of one specific way the whole feature fails. A hotkey
/// handler runs while the combination that triggered it is still held, and the
/// window server merges the physically-held modifiers into a synthesised
/// event: type `c` with Control still down and the application receives
/// `Ctrl+C`. Typing a whole line under a held modifier is a run of shortcuts
/// rather than a run of characters, and in a terminal one of them interrupts
/// whatever is running there.
///
/// Waiting is the fix rather than clearing the modifiers by force: posting
/// key-up events for keys the user is still holding would leave the machine
/// disagreeing with the keyboard about what is pressed.
///
/// # Errors
/// Refuses to send anything if the keys are still down after [`RELEASE_LIMIT`].
/// Sending regardless would deliver keystrokes that are not the ones asked for,
/// and the wrong keystrokes into an unknown window are worse than none.
fn wait_until_the_user_lets_go() -> Result<(), CliftError> {
    let deadline = std::time::Instant::now() + RELEASE_LIMIT;
    while platform::modifiers_are_held() {
        if std::time::Instant::now() >= deadline {
            return Err(CliftError::new(
                Stage::Injection,
                ErrorKind::Internal,
                "a modifier key is still held down, so the text would arrive as a run of \
                 keyboard shortcuts rather than as characters",
            )
            .with_remedy(Remedy::new(
                "Let go of the keys and try again, or take the keystrokes out of it:",
                "clift paste --copy",
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

/// The text [`type_into_focused_window`] will accept: printable ASCII, on one
/// line, and not empty.
///
/// The instruction Clift generates is exactly that, so this never fires in
/// normal use. It is here because a synthesised control character is not a
/// character but a command: a newline submits whatever prompt it lands in,
/// which is the one thing the specification says an integration must never do
/// on the user's behalf, and an escape starts a sequence the terminal will act
/// on. Refusing is safe; the user is pointed at `--copy`, where the terminal's
/// own paste decides what such bytes mean.
///
/// # Errors
/// Returns the text back to the caller as an error when it holds anything
/// outside that range.
fn typeable(text: &str) -> Result<(), CliftError> {
    let refused = |why: &str| {
        Err(CliftError::new(
            Stage::Injection,
            ErrorKind::Internal,
            format!("Clift will not type this text into a window: {why}"),
        )
        .with_remedy(Remedy::new(
            "Put it on the clipboard and paste it yourself:",
            "clift paste --copy",
        )))
    };
    if text.is_empty() {
        return refused("there is nothing to type");
    }
    match text.chars().find(|c| !c.is_ascii_graphic() && *c != ' ') {
        Some(character) => refused(&format!(
            "it contains {character:?}, which is not a printable character"
        )),
        None => Ok(()),
    }
}

/// Types `text` into whichever window has focus.
///
/// The clipboard is not read, written or borrowed; whatever the user copied is
/// still there when this returns.
///
/// # Errors
/// Fails when the permission is missing, when the platform has no
/// implementation, and when the text is not something [`typeable`] allows. It
/// does **not** fail when the characters were delivered somewhere unexpected:
/// nothing here can observe where focus was, and claiming otherwise would be a
/// lie the user could not check.
pub fn type_into_focused_window(text: &str) -> Result<(), CliftError> {
    typeable(text)?;

    // Before anything else, and for a reason that is not obvious: the caller
    // may be a hotkey handler, in which case the user's own fingers are still
    // on the modifier keys that triggered it. See `wait_until_the_user_lets_go`.
    wait_until_the_user_lets_go()?;

    match availability() {
        Availability::Ready => platform::send_text(text),
        Availability::NeedsPermission(text) => Err(CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            "Clift is not allowed to send keystrokes on this machine",
        )
        .with_remedy(Remedy::new(text, "clift paste --copy"))),
        Availability::Unsupported(text) => Err(CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            text,
        )
        .with_remedy(Remedy::new(
            "Put the text on the clipboard and paste it yourself:",
            "clift paste --copy",
        ))),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! CoreGraphics event synthesis.
    //!
    //! Three calls do the work: one builds a keyboard event, one attaches the
    //! character that event delivers, and one posts it at the HID tap so that
    //! it is indistinguishable from a real key press by the time an
    //! application sees it. Posting at the HID tap is what makes this work
    //! with terminals that read the keyboard directly.
    //!
    //! The character is carried by `CGEventKeyboardSetUnicodeString` rather
    //! than by a virtual key code, which is what makes the keyboard layout
    //! irrelevant: a virtual key code is a physical position, and the letter
    //! that position produces is different on a French or a Dvorak keyboard.
    //! The token would arrive scrambled on any layout but the one it was
    //! written on.
    //!
    //! macOS gates this behind Accessibility, which is correct -- a program
    //! that can synthesise keystrokes can drive any application the user can --
    //! and `AXIsProcessTrusted` is how a process asks whether it has been let
    //! through. It is asked *before* posting rather than after, because a post
    //! without permission fails silently: the call returns, nothing happens,
    //! and there is no error to report.

    use super::{Availability, CliftError, ErrorKind, Remedy, Stage};
    use std::ffi::c_void;

    type CGEventSourceRef = *mut c_void;
    type CGEventRef = *mut c_void;

    /// `kCGEventSourceStateHIDSystemState`: the event is attributed to the
    /// hardware event stream, which is what an application expects a key press
    /// to come from.
    const HID_SYSTEM_STATE: i32 = 1;
    /// The pause between one synthesised event and the next.
    ///
    /// Not a guess and not politeness: posted flat out, the events are dropped.
    /// Measured against a real application, typing a 60 character line, three
    /// runs each: with no pause 2 of the 60 characters arrived, at 100 us one
    /// run in three lost five characters, and at 250 us and above every run
    /// arrived complete. A millisecond is four times the point where the
    /// losses stop, which costs a 60 character line 120 ms -- less than the
    /// quarter second the clipboard version spent waiting before it could give
    /// the user their screenshot back.
    ///
    /// Windows needs no equivalent: `SendInput` hands the whole line to the
    /// input queue in one call rather than posting events one by one.
    const KEY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);
    /// `kCGHIDEventTap`: posted at the lowest point, before any application.
    const HID_EVENT_TAP: u32 = 0;

    /// Every modifier `CGEventSourceFlagsState` can report as down. Caps Lock
    /// is deliberately absent: it is a latch rather than a held key, and
    /// waiting for a user to turn it off would hang forever.
    const FLAG_MASK_MODIFIERS: u64 = 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
    /// `kCGEventSourceStateCombinedSessionState`: what the session as a whole
    /// currently sees, which is the state a synthesised event would be merged
    /// with.
    const COMBINED_SESSION_STATE: i32 = 0;

    type CFTypeRef = *const c_void;
    type CFDictionaryRef = *const c_void;

    /// `CFDictionaryKeyCallBacks` and `CFDictionaryValueCallBacks`: a version
    /// word followed by five function pointers. Only their addresses are
    /// needed, to hand the standard callbacks back to CoreFoundation.
    #[repr(C)]
    struct CFDictionaryCallBacks {
        _fields: [*const c_void; 6],
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        static kAXTrustedCheckOptionPrompt: CFTypeRef;
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
        fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
        fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            virtual_key: u16,
            key_down: bool,
        ) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: usize, string: *const u16);
        fn CGEventPost(tap: u32, event: CGEventRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const CFDictionaryCallBacks,
            value_callbacks: *const CFDictionaryCallBacks,
        ) -> CFDictionaryRef;
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: CFDictionaryCallBacks;
        static kCFTypeDictionaryValueCallBacks: CFDictionaryCallBacks;
    }

    /// Asks macOS to show its Accessibility prompt for this process if it is
    /// not yet trusted. The prompt is the system's; the answer lands in
    /// System Settings, and the return value only says whether it has landed
    /// already.
    pub fn request_permission() -> Availability {
        // SAFETY: no arguments, no preconditions.
        if unsafe { AXIsProcessTrusted() } {
            return Availability::Ready;
        }
        // SAFETY: the option dictionary is built from CoreFoundation's own
        // constants with its standard callbacks, holds exactly the one pair
        // its `count` says, is owned here, and is released after the single
        // call that reads it. `kAXTrustedCheckOptionPrompt = true` is the
        // documented way to ask for the dialog; the call has no other effect.
        let trusted = unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            );
            if options.is_null() {
                return availability();
            }
            let trusted = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
            trusted
        };
        if trusted {
            Availability::Ready
        } else {
            availability()
        }
    }

    pub fn modifiers_are_held() -> bool {
        // SAFETY: takes a documented state id and returns a bit field; it reads
        // process-external state and has no preconditions.
        let flags = unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) };
        flags & FLAG_MASK_MODIFIERS != 0
    }

    /// Opens Accessibility directly. The pane is several levels down in
    /// System Settings and the search field does not find it by the word
    /// "keystroke", which is why this is a command rather than directions.
    pub const fn permission_command() -> Option<&'static str> {
        Some(
            "open \"x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility\"",
        )
    }

    pub fn availability() -> Availability {
        // SAFETY: takes no arguments, returns a Boolean, and has no
        // preconditions beyond being called from a normal process.
        if unsafe { AXIsProcessTrusted() } {
            return Availability::Ready;
        }
        Availability::NeedsPermission(
            "Allow your terminal to control the computer, then try again: System Settings > \
             Privacy & Security > Accessibility. Until then, use:"
                .to_string(),
        )
    }

    pub fn send_text(text: &str) -> Result<(), CliftError> {
        // SAFETY: `HID_SYSTEM_STATE` is a documented state id. The returned
        // reference is owned here and released below; a null return is checked
        // rather than passed on.
        let source = unsafe { CGEventSourceCreate(HID_SYSTEM_STATE) };
        if source.is_null() {
            return Err(failed("the window server would not create an event source"));
        }

        let outcome = post_characters(source, text);

        // SAFETY: `source` is non-null and was created by
        // `CGEventSourceCreate`, so this process owns exactly one reference to
        // it and this is the matching release.
        unsafe { CFRelease(source.cast_const()) };
        outcome
    }

    /// One character per event, down then up.
    ///
    /// The whole string could go on a single event, and some applications read
    /// it that way; others take only its first character, and a token that
    /// arrives as `A` is worse than one that arrives slowly.
    ///
    /// The flags are cleared on every event. Left alone they inherit whatever
    /// the source last saw, and a Command flag riding along under a run of
    /// letters is a run of menu shortcuts.
    ///
    /// The string is attached to the key-up as well as the key-down for the
    /// same reason it was on both halves of the old `Cmd+V`: an application
    /// that tracks the pair sees a release it cannot match otherwise.
    ///
    /// Every event is followed by [`KEY_INTERVAL`], which is what makes the
    /// line arrive whole. See the constant for the measurement.
    fn post_characters(source: CGEventSourceRef, text: &str) -> Result<(), CliftError> {
        let mut utf16 = [0u16; 2];
        for character in text.chars() {
            let units = character.encode_utf16(&mut utf16);
            for key_down in [true, false] {
                // SAFETY: `source` is a live event source. Virtual key 0 with
                // a Unicode string attached is the documented way to deliver a
                // character rather than a key position; the returned event is
                // owned here.
                let event = unsafe { CGEventCreateKeyboardEvent(source, 0, key_down) };
                if event.is_null() {
                    return Err(failed("the window server would not create a key event"));
                }
                // SAFETY: `event` is a live keyboard event this function owns,
                // and `units` is a live buffer of `units.len()` UTF-16 units
                // that outlives the call, which copies them.
                unsafe {
                    CGEventSetFlags(event, 0);
                    CGEventKeyboardSetUnicodeString(event, units.len(), units.as_ptr());
                    CGEventPost(HID_EVENT_TAP, event);
                    CFRelease(event.cast_const());
                }
                std::thread::sleep(KEY_INTERVAL);
            }
        }
        Ok(())
    }

    fn failed(message: &str) -> CliftError {
        CliftError::new(Stage::Injection, ErrorKind::Internal, message.to_string()).with_remedy(
            Remedy::new(
                "Put the text on the clipboard and paste it yourself:",
                "clift paste --copy",
            ),
        )
    }
}

#[cfg(windows)]
mod platform {
    //! `SendInput`, the Win32 equivalent.
    //!
    //! Every character travels as a `KEYEVENTF_UNICODE` event, which carries
    //! the character itself instead of a key position and therefore does not
    //! care what keyboard layout is installed.
    //!
    //! There is a second reason this crate types rather than pastes, and it is
    //! specific to Windows: there is no paste key every window agrees on. The
    //! first version sent `Ctrl+V`, and on a real Windows 10 machine it pasted
    //! into Notepad and into nothing that mattered, because in a terminal
    //! `Ctrl+V` has meant "literal next character" for forty years. The second
    //! sent `Shift+Insert`, which the terminals do accept but some editors and
    //! remote desktop clients do not. A character needs no such agreement.
    //!
    //! Windows needs no permission for this: any process in the interactive
    //! session may synthesise input into it. So `availability` is unconditional,
    //! which is a real difference from macOS rather than an oversight.

    use super::{Availability, CliftError, ErrorKind, Remedy, Stage};

    const INPUT_KEYBOARD: u32 = 1;
    /// The event carries a UTF-16 code unit in `scan_code`; `virtual_key` must
    /// be zero.
    const KEYEVENTF_UNICODE: u32 = 0x0004;
    const KEYEVENTF_KEYUP: u32 = 0x0002;
    const VK_CONTROL: u16 = 0x11;
    const VK_SHIFT: u16 = 0x10;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeyboardInput {
        virtual_key: u16,
        scan_code: u16,
        flags: u32,
        time: u32,
        extra_info: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Input {
        kind: u32,
        // The union in the Win32 header is as wide as its largest member, the
        // mouse variant. Padding it explicitly keeps the layout right without
        // needing a union type.
        keyboard: KeyboardInput,
        padding: [u8; 8],
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
        fn GetAsyncKeyState(virtual_key: i32) -> i16;
    }

    /// Nowhere to grant what is not gated.
    pub const fn permission_command() -> Option<&'static str> {
        None
    }

    pub fn availability() -> Availability {
        Availability::Ready
    }

    pub fn request_permission() -> Availability {
        availability()
    }

    /// `GetAsyncKeyState`'s high bit means "down right now".
    pub fn modifiers_are_held() -> bool {
        const VK_MENU: i32 = 0x12;
        const VK_LWIN: i32 = 0x5B;
        const VK_RWIN: i32 = 0x5C;
        [
            VK_SHIFT as i32,
            VK_CONTROL as i32,
            VK_MENU,
            VK_LWIN,
            VK_RWIN,
        ]
        .into_iter()
        // SAFETY: takes a virtual key code and returns a bit field; it
        // reads process-external state and has no preconditions.
        .any(|key| unsafe { GetAsyncKeyState(key) } as u16 & 0x8000 != 0)
    }

    /// Types `text`, in one call.
    ///
    /// One call rather than one per character, because a single `SendInput` is
    /// inserted into the input stream as a block: nothing the user or another
    /// program does can land in the middle of the token and split it.
    pub fn send_text(text: &str) -> Result<(), CliftError> {
        let key = |unit: u16, up: bool| Input {
            kind: INPUT_KEYBOARD,
            keyboard: KeyboardInput {
                virtual_key: 0,
                scan_code: unit,
                flags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                extra_info: 0,
            },
            padding: [0; 8],
        };
        // A character outside the basic plane becomes two units, and Windows
        // documents that as the way to send one: the pair is delivered in
        // order and reassembled by the receiving window.
        let events: Vec<Input> = text
            .encode_utf16()
            .flat_map(|unit| [key(unit, false), key(unit, true)])
            .collect();
        let Ok(count) = u32::try_from(events.len()) else {
            return Err(refused("the text is too long to type in one go"));
        };

        let size = i32::try_from(std::mem::size_of::<Input>()).unwrap_or(0);
        // SAFETY: `events` is a live slice of `count` correctly laid out
        // `Input` values, and `size` is the size of one of them, which is the
        // contract `SendInput` documents.
        let sent = unsafe { SendInput(count, events.as_ptr(), size) };
        if usize::try_from(sent).unwrap_or(0) == events.len() {
            return Ok(());
        }
        // A partial send is the failure that matters most here: half a token
        // is in the window already, and saying so is the only way the user
        // knows to clear the line before trying again.
        Err(refused(
            "Windows accepted only part of the text, so what reached the window is incomplete",
        ))
    }

    fn refused(message: &str) -> CliftError {
        CliftError::new(Stage::Injection, ErrorKind::Internal, message.to_string()).with_remedy(
            Remedy::new(
                "Put the text on the clipboard and paste it yourself:",
                "clift paste --copy",
            ),
        )
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    //! No implementation, and it says so.
    //!
    //! On Linux this is not one job but two incompatible ones: X11 has
    //! `XTEST`, Wayland has no equivalent at all by design, and a portal-based
    //! path exists on some compositors and not others. Shipping something that
    //! works on one desktop and silently does nothing on another would be worse
    //! than this message, which at least tells the user what to do instead.

    use super::{Availability, CliftError};

    /// Nothing here can send a keystroke, so nothing here needs to know
    /// whether one would be mangled.
    pub const fn modifiers_are_held() -> bool {
        false
    }

    /// There is no permission to grant, because there is nothing to permit.
    pub const fn permission_command() -> Option<&'static str> {
        None
    }

    pub fn availability() -> Availability {
        Availability::Unsupported(
            "Clift cannot type into a window on this platform yet; use --copy and paste it \
             yourself"
                .to_string(),
        )
    }

    pub fn request_permission() -> Availability {
        availability()
    }

    pub fn send_text(_text: &str) -> Result<(), CliftError> {
        // Unreachable through `type_into_focused_window`, which checks
        // availability first. Present so the module has the same shape on every
        // platform.
        Err(super::CliftError::new(
            super::Stage::Injection,
            super::ErrorKind::Config,
            "keystroke injection is not implemented on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instruction Clift actually generates must be typeable, and the
    /// things that would turn a line of text into a command must not be.
    #[test]
    fn only_printable_single_line_text_is_typed() {
        assert!(typeable("Attachment: clift fetch 'clift://v1/aB3-_x#k9Zq'").is_ok());
        assert!(typeable("a b").is_ok());

        for wrong in [
            "",
            "two\nlines",
            "trailing\n",
            "a\tb",
            "\u{1b}[31m",
            "caf\u{e9}",
            "emoji \u{1f600}",
            "\u{7f}",
        ] {
            let error = typeable(wrong).unwrap_err();
            assert_eq!(error.stage(), Stage::Injection, "{wrong:?}");
            assert!(
                error
                    .remedy()
                    .is_some_and(|remedy| remedy.command().contains("--copy")),
                "{wrong:?} was refused without offering the fallback"
            );
        }
    }

    /// A refusal must happen before any key is synthesised. Asserted through
    /// the public entry point, because the ordering inside it is the thing
    /// that matters: text is checked, then the keyboard is waited on, then
    /// events are posted.
    #[test]
    fn text_that_cannot_be_typed_is_refused_before_anything_is_sent() {
        let error = type_into_focused_window("two\nlines").unwrap_err();
        assert_eq!(error.stage(), Stage::Injection);
        assert!(error.to_string().contains("not a printable character"));
    }

    /// Whatever this machine answers, the answer must be usable: either it is
    /// ready, or it explains itself well enough to act on.
    #[test]
    fn an_unavailable_platform_says_what_to_do_instead() {
        let availability = availability();
        match &availability {
            Availability::Ready => assert!(availability.explanation().is_none()),
            other => {
                let text = other.explanation().unwrap_or("");
                assert!(!text.is_empty(), "no explanation given");
                assert!(text.len() > 20, "the explanation is not actionable: {text}");
            }
        }
    }

    /// With no key held, the wait must return at once rather than sitting out
    /// the two-second limit. A test run holds nothing, so this also asserts
    /// that the modifier check is not stuck reporting `true`.
    #[test]
    fn an_idle_keyboard_is_not_waited_on() {
        if modifiers_are_held() {
            // Something is genuinely holding a key; nothing to assert.
            return;
        }
        let started = std::time::Instant::now();
        assert!(wait_until_the_user_lets_go().is_ok());
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "waited {:?} with nothing held",
            started.elapsed()
        );
    }

    /// Asking for an injection that cannot happen must fail, and must point at
    /// `--copy`. A user who is told nothing will assume it worked.
    #[test]
    fn a_refused_injection_points_at_the_fallback() {
        if availability().is_ready() {
            // Nothing to assert on a machine that has the permission: this test
            // is about the refusal path, and typing into whatever window has
            // focus is not something a test may do.
            return;
        }
        let error = type_into_focused_window("Attachment: clift fetch 'x'").unwrap_err();
        assert_eq!(error.stage(), Stage::Injection);
        assert!(
            error
                .remedy()
                .is_some_and(|remedy| remedy.command().contains("--copy")),
            "the fallback was not offered"
        );
    }
}

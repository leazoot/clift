//! Registering one key combination with the operating system, and waiting.
//!
//! The mirror image of the rest of this crate: everything else here sends a
//! keystroke, this receives one. It is still not interception. A registered
//! hotkey is a request the window server grants or refuses, scoped to exactly
//! the combination asked for; nothing is hooked, no event stream is tapped, and
//! every other key on the keyboard reaches the focused application untouched.
//! The distinction matters because a keyboard tap would see the user's
//! passwords and this cannot.
//!
//! What it does cost is honest to state: while the helper runs, the registered
//! combination stops reaching whatever application had it. That is what
//! registering means, it is why the combination is configurable, and it is why
//! `clift-core` refuses to let anyone register the plain paste key.

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::hotkey::Hotkey;

/// Registers `hotkey` and calls `on_press` every time it is pressed.
///
/// Blocks until the process is interrupted, so it never returns `Ok`: an
/// ordinary end to this function is `Ctrl+C`, which the operating system
/// delivers as a signal rather than as a return.
///
/// `on_press` is called on the same thread that received the key. It must not
/// panic and it must return: nothing else is listening while it runs, so a
/// press during a slow paste is a press the user has to repeat.
///
/// # Errors
/// Fails when the combination is already registered by something else, when
/// the platform has no implementation, and when the event queue stops
/// answering.
pub fn listen(hotkey: &Hotkey, on_press: &mut dyn FnMut()) -> Result<(), CliftError> {
    platform::listen(hotkey, on_press)
}

/// Whether this build can register a global hotkey at all.
///
/// Separate from [`crate::availability`], which answers a different question:
/// registering needs no permission on either platform, while *sending* the
/// paste keystroke needs Accessibility on macOS. A machine can therefore be
/// able to hear the key and unable to act on it, which is exactly the case
/// The specification requires to degrade to `--copy` rather than to fail.
#[must_use]
pub fn is_supported() -> bool {
    platform::IS_SUPPORTED
}

/// Whether a helper started by this user is running right now.
///
/// Answered where the platform gives a helper no other way to be found: on
/// Windows the helper owns a named event for as long as it runs. On macOS
/// launchd is the authority and this always says `false`; ask it instead.
#[must_use]
pub fn helper_is_running() -> bool {
    platform::helper_is_running()
}

/// Asks the running helper, if there is one, to stop. Returns whether there
/// was one to ask.
///
/// Windows only, for the same reason as [`helper_is_running`]: a helper
/// started hidden at login has no terminal to press `Ctrl+C` in, so
/// `--uninstall` and a reinstall with a new combination both need this. On
/// macOS launchd stops the helper and this does nothing.
pub fn stop_running_helper() -> bool {
    platform::stop_running_helper()
}

#[cfg(target_os = "macos")]
mod platform {
    //! `RegisterEventHotKey`, and a pull loop over the event queue.
    //!
    //! This is the Carbon Event Manager, which is old and still the only
    //! documented way to ask macOS for a single combination. The alternative is
    //! a `CGEventTap`, which sees every keystroke on the machine and needs
    //! Accessibility to do it -- a far larger thing to ask for and a far larger
    //! thing to be trusted with, in exchange for a feature nobody wants.
    //!
    //! The loop pulls every event rather than filtering for the hot key one, so
    //! that anything else the queue receives is released rather than left to
    //! accumulate for as long as this process lives.

    use super::{CliftError, ErrorKind, Hotkey, Remedy, Stage};
    use clift_core::hotkey::Key;
    use std::ffi::c_void;

    pub const IS_SUPPORTED: bool = true;

    /// launchd knows; this module does not keep a second record of it.
    pub fn helper_is_running() -> bool {
        false
    }

    pub fn stop_running_helper() -> bool {
        false
    }

    type EventTargetRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;
    type EventRef = *mut c_void;
    type OsStatus = i32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    /// `'clft'`, the four-character code identifying our registration. Only
    /// ever compared against our own, so any distinct value would do.
    const SIGNATURE: u32 = u32::from_be_bytes(*b"clft");

    /// `kEventClassKeyboard` (`'keyb'`) and `kEventHotKeyPressed`.
    const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
    const EVENT_HOT_KEY_PRESSED: u32 = 5;
    /// `kEventDurationForever`.
    const FOREVER: f64 = -1.0;
    /// `eventLoopTimedOutErr`, which cannot happen with an infinite timeout but
    /// is checked anyway so a surprising one is not read as a key press.
    const TIMED_OUT: OsStatus = -9875;

    // Carbon modifier masks, which are not the CoreGraphics ones used to send a
    // keystroke: the two APIs are from different eras and disagree.
    const CMD_KEY: u32 = 0x0100;
    const SHIFT_KEY: u32 = 0x0200;
    const OPTION_KEY: u32 = 0x0800;
    const CONTROL_KEY: u32 = 0x1000;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetEventDispatcherTarget() -> EventTargetRef;
        fn RegisterEventHotKey(
            code: u32,
            modifiers: u32,
            id: EventHotKeyId,
            target: EventTargetRef,
            options: u32,
            out: *mut EventHotKeyRef,
        ) -> OsStatus;
        fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
        fn ReceiveNextEvent(
            num_types: u32,
            list: *const c_void,
            timeout: f64,
            pull: bool,
            out: *mut EventRef,
        ) -> OsStatus;
        fn GetEventClass(event: EventRef) -> u32;
        fn GetEventKind(event: EventRef) -> u32;
        fn ReleaseEvent(event: EventRef);
    }

    /// A registration that unregisters itself, so an error on the way out of
    /// the loop cannot leave the combination captured by a dead process.
    struct Registered(EventHotKeyRef);

    impl Drop for Registered {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the non-null reference `RegisterEventHotKey`
            // produced, released exactly once because this type is not `Clone`.
            unsafe { UnregisterEventHotKey(self.0) };
        }
    }

    pub fn listen(hotkey: &Hotkey, on_press: &mut dyn FnMut()) -> Result<(), CliftError> {
        let Some(code) = virtual_key(hotkey.key()) else {
            return Err(no_such_key(hotkey));
        };

        let mut reference: EventHotKeyRef = std::ptr::null_mut();
        // SAFETY: `GetEventDispatcherTarget` takes nothing and returns the
        // process's own event target. `reference` is a live, correctly typed
        // out parameter. The call does not retain anything of ours.
        let status = unsafe {
            RegisterEventHotKey(
                code,
                carbon_modifiers(hotkey),
                EventHotKeyId {
                    signature: SIGNATURE,
                    id: 1,
                },
                GetEventDispatcherTarget(),
                0,
                &raw mut reference,
            )
        };
        if status != 0 || reference.is_null() {
            return Err(already_taken(hotkey, status));
        }
        let _registration = Registered(reference);

        loop {
            let mut event: EventRef = std::ptr::null_mut();
            // SAFETY: no type filter, so `list` is null and `num_types` zero,
            // which is the documented way to receive everything. `event` is a
            // live out parameter, and ownership of what comes back is ours.
            let status =
                unsafe { ReceiveNextEvent(0, std::ptr::null(), FOREVER, true, &raw mut event) };
            if status == TIMED_OUT {
                continue;
            }
            if status != 0 {
                return Err(queue_stopped(status));
            }
            if event.is_null() {
                continue;
            }

            // SAFETY: `event` is the non-null event just received.
            let (class, kind) = unsafe { (GetEventClass(event), GetEventKind(event)) };
            // SAFETY: released exactly once, and not used afterwards.
            unsafe { ReleaseEvent(event) };

            if class == EVENT_CLASS_KEYBOARD && kind == EVENT_HOT_KEY_PRESSED {
                on_press();
            }
        }
    }

    fn carbon_modifiers(hotkey: &Hotkey) -> u32 {
        let modifiers = hotkey.modifiers();
        let mut mask = 0;
        if modifiers.command {
            mask |= CMD_KEY;
        }
        if modifiers.control {
            mask |= CONTROL_KEY;
        }
        if modifiers.option {
            mask |= OPTION_KEY;
        }
        if modifiers.shift {
            mask |= SHIFT_KEY;
        }
        mask
    }

    /// `kVK_ANSI_*`, which are physical positions rather than letters.
    ///
    /// The table is written out because there is no arithmetic in it: the codes
    /// follow the ANSI keyboard's physical layout, so `a` is 0 and `b` is 11.
    fn virtual_key(key: Key) -> Option<u32> {
        let code: u16 = match key {
            Key::Letter(letter) => match letter {
                'a' => 0,
                's' => 1,
                'd' => 2,
                'f' => 3,
                'h' => 4,
                'g' => 5,
                'z' => 6,
                'x' => 7,
                'c' => 8,
                'v' => 9,
                'b' => 11,
                'q' => 12,
                'w' => 13,
                'e' => 14,
                'r' => 15,
                'y' => 16,
                't' => 17,
                'o' => 31,
                'u' => 32,
                'i' => 34,
                'p' => 35,
                'l' => 37,
                'j' => 38,
                'k' => 40,
                'n' => 45,
                'm' => 46,
                _ => return None,
            },
            Key::Digit(digit) => match digit {
                1 => 18,
                2 => 19,
                3 => 20,
                4 => 21,
                5 => 23,
                6 => 22,
                7 => 26,
                8 => 28,
                9 => 25,
                0 => 29,
                _ => return None,
            },
            Key::Function(index) => match index {
                1 => 122,
                2 => 120,
                3 => 99,
                4 => 118,
                5 => 96,
                6 => 97,
                7 => 98,
                8 => 100,
                9 => 101,
                10 => 109,
                11 => 103,
                12 => 111,
                _ => return None,
            },
        };
        Some(u32::from(code))
    }

    fn no_such_key(hotkey: &Hotkey) -> CliftError {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            format!(
                "macOS has no key code for {:?} in this build",
                hotkey.render()
            ),
        )
        .with_remedy(Remedy::new(
            "Choose another combination:",
            "clift config set hotkey.combination cmd+shift+v",
        ))
    }

    fn already_taken(hotkey: &Hotkey, status: OsStatus) -> CliftError {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            format!(
                "macOS would not give Clift {} (error {status}); something else has it",
                hotkey.render()
            ),
        )
        .with_remedy(Remedy::new(
            "Choose a combination nothing else uses:",
            "clift config set hotkey.combination cmd+ctrl+v",
        ))
    }

    fn queue_stopped(status: OsStatus) -> CliftError {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Internal,
            format!("the macOS event queue stopped answering (error {status})"),
        )
        .with_remedy(Remedy::new("Start the helper again:", "clift hotkey"))
    }
}

#[cfg(windows)]
mod platform {
    //! `RegisterHotKey`, a message loop, and a named event to stop it.
    //!
    //! This module has been compiled for Windows by the release pipeline and
    //! the binary that contains it runs there; nobody has yet watched a
    //! registered key fire. Do not describe Windows hotkeys as working until
    //! somebody has.
    //!
    //! `RegisterHotKey` with a null window delivers `WM_HOTKEY` to the calling
    //! thread's message queue, which is why there is no window here and no need
    //! for one: the specification forbids a window and this needs none.
    //!
    //! The named event exists because Windows has no launchd. A helper started
    //! hidden at login has no terminal to press `Ctrl+C` in and no service
    //! manager to unload it, so `clift hotkey --uninstall` needs a way to reach
    //! it: it opens the event by name and sets it, and the loop below, which
    //! waits on the event and the message queue together, returns. The same
    //! event doubles as the "already running" check, so a second helper says
    //! so instead of failing on the registration with a less useful message.

    use super::{CliftError, ErrorKind, Hotkey, Remedy, Stage};
    use clift_core::hotkey::Key;
    use std::ffi::c_void;

    pub const IS_SUPPORTED: bool = true;

    const MOD_ALT: u32 = 0x0001;
    const MOD_CONTROL: u32 = 0x0002;
    const MOD_SHIFT: u32 = 0x0004;
    const MOD_WIN: u32 = 0x0008;
    /// Without this a held-down combination repeats as fast as the keyboard
    /// does, and every repeat would publish another attachment.
    const MOD_NOREPEAT: u32 = 0x4000;
    const WM_QUIT: u32 = 0x0012;
    const WM_HOTKEY: u32 = 0x0312;
    const HOTKEY_ID: i32 = 1;
    const PM_REMOVE: u32 = 0x0001;
    const QS_ALLINPUT: u32 = 0x04FF;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_FAILED: u32 = 0xFFFF_FFFF;
    const EVENT_MODIFY_STATE: u32 = 0x0002;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_ALREADY_EXISTS: i32 = 183;

    /// The event's name. `Local\` scopes it to this logon session, which is
    /// the scope a hotkey has anyway: another user's helper is not this one.
    const STOP_EVENT: &str = "Local\\dev.clift.hotkey.stop";

    type Handle = *mut c_void;

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Message {
        window: *mut c_void,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        point: Point,
        private: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterHotKey(window: *mut c_void, id: i32, modifiers: u32, virtual_key: u32) -> i32;
        fn UnregisterHotKey(window: *mut c_void, id: i32) -> i32;
        fn PeekMessageW(
            message: *mut Message,
            window: *mut c_void,
            filter_min: u32,
            filter_max: u32,
            remove: u32,
        ) -> i32;
        fn MsgWaitForMultipleObjects(
            count: u32,
            handles: *const Handle,
            wait_all: i32,
            milliseconds: u32,
            wake_mask: u32,
        ) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(
            attributes: *const c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> Handle;
        fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Handle;
        fn SetEvent(event: Handle) -> i32;
        fn CloseHandle(object: Handle) -> i32;
    }

    struct Registered;

    impl Drop for Registered {
        fn drop(&mut self) {
            // SAFETY: unregisters the id this thread registered; harmless if
            // the registration is already gone.
            unsafe { UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID) };
        }
    }

    /// A kernel handle closed when dropped, so no early return leaks one.
    struct Owned(Handle);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the handle was returned by Create/OpenEventW to this
                // process and is closed exactly once, here.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn listen(hotkey: &Hotkey, on_press: &mut dyn FnMut()) -> Result<(), CliftError> {
        let Some(code) = virtual_key(hotkey.key()) else {
            return Err(no_such_key(hotkey));
        };

        let name = wide(STOP_EVENT);
        // SAFETY: default security, a manual-reset event that starts unset,
        // and a NUL-terminated name that outlives the call.
        let stop = Owned(unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) });
        // Read before anything else can overwrite it: the "already exists"
        // condition comes back as a success with this error code set.
        let last_error = std::io::Error::last_os_error().raw_os_error();
        if stop.0.is_null() {
            return Err(queue_stopped("could not create the stop event"));
        }
        if last_error == Some(ERROR_ALREADY_EXISTS) {
            return Err(already_running());
        }

        // SAFETY: a null window means "deliver to this thread's queue", which
        // is what the documentation specifies and what this loop reads from.
        let registered =
            unsafe { RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, modifiers(hotkey), code) };
        if registered == 0 {
            return Err(already_taken(hotkey));
        }
        let _registration = Registered;

        loop {
            // SAFETY: one live handle in an array of one; a wait on it or on
            // any input arriving for this thread, with no timeout.
            let woke = unsafe {
                MsgWaitForMultipleObjects(1, &raw const stop.0, 0, INFINITE, QS_ALLINPUT)
            };
            if woke == WAIT_OBJECT_0 {
                // Somebody set the stop event: an uninstall, or a reinstall
                // about to start a helper with a new combination.
                return Ok(());
            }
            if woke == WAIT_FAILED {
                return Err(queue_stopped("the wait on the message queue failed"));
            }
            // Drain everything that arrived: the wait only wakes for input
            // that is new since the last time the queue was looked at, so a
            // message left behind here would be waited on forever.
            loop {
                let mut message = Message {
                    window: std::ptr::null_mut(),
                    message: 0,
                    w_param: 0,
                    l_param: 0,
                    time: 0,
                    point: Point { x: 0, y: 0 },
                    private: 0,
                };
                // SAFETY: `message` is a live, correctly laid out `MSG`. A
                // null window and a zero filter range mean "every message for
                // this thread"; PM_REMOVE takes it off the queue.
                let pending = unsafe {
                    PeekMessageW(&raw mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE)
                };
                if pending == 0 {
                    break;
                }
                if message.message == WM_QUIT {
                    return Ok(());
                }
                if message.message == WM_HOTKEY {
                    on_press();
                }
            }
        }
    }

    /// Opens the running helper's stop event, if there is one.
    fn open_stop_event(access: u32) -> Option<Owned> {
        let name = wide(STOP_EVENT);
        // SAFETY: a NUL-terminated name that outlives the call; a null result
        // means "no such event" and is handled.
        let handle = unsafe { OpenEventW(access, 0, name.as_ptr()) };
        (!handle.is_null()).then_some(Owned(handle))
    }

    pub fn helper_is_running() -> bool {
        open_stop_event(SYNCHRONIZE).is_some()
    }

    pub fn stop_running_helper() -> bool {
        let Some(event) = open_stop_event(EVENT_MODIFY_STATE) else {
            return false;
        };
        // SAFETY: a live event handle opened with the right to set it.
        unsafe { SetEvent(event.0) };
        // The handle is dropped here, on purpose: while this process holds
        // one the kernel keeps the object alive, and a helper started next
        // would see "already running" for a helper that has already gone.
        true
    }

    fn modifiers(hotkey: &Hotkey) -> u32 {
        let modifiers = hotkey.modifiers();
        let mut mask = MOD_NOREPEAT;
        if modifiers.command {
            mask |= MOD_WIN;
        }
        if modifiers.control {
            mask |= MOD_CONTROL;
        }
        if modifiers.option {
            mask |= MOD_ALT;
        }
        if modifiers.shift {
            mask |= MOD_SHIFT;
        }
        mask
    }

    /// Windows virtual key codes, which unlike the macOS ones are arithmetic:
    /// letters and digits are their uppercase ASCII values.
    fn virtual_key(key: Key) -> Option<u32> {
        match key {
            Key::Letter(letter) if letter.is_ascii_lowercase() => {
                Some(u32::from(letter.to_ascii_uppercase() as u8))
            }
            Key::Letter(_) => None,
            Key::Digit(digit) if digit <= 9 => Some(u32::from(b'0' + digit)),
            Key::Digit(_) => None,
            // VK_F1 is 0x70 and the twelve are consecutive.
            Key::Function(index) if (1..=12).contains(&index) => Some(0x70 + u32::from(index) - 1),
            Key::Function(_) => None,
        }
    }

    fn no_such_key(hotkey: &Hotkey) -> CliftError {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            format!(
                "Windows has no key code for {:?} in this build",
                hotkey.render()
            ),
        )
        .with_remedy(Remedy::new(
            "Choose another combination:",
            "clift config set hotkey.combination ctrl+alt+v",
        ))
    }

    fn already_taken(hotkey: &Hotkey) -> CliftError {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            format!(
                "Windows would not give Clift {}; something else has it",
                hotkey.render()
            ),
        )
        .with_remedy(Remedy::new(
            "Choose a combination nothing else uses:",
            "clift config set hotkey.combination ctrl+alt+f9",
        ))
    }

    fn already_running() -> CliftError {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            "a Clift hotkey helper is already running in this session",
        )
        .with_remedy(Remedy::new(
            "Stop it (and its login entry) first:",
            "clift hotkey --uninstall",
        ))
    }

    fn queue_stopped(detail: &str) -> CliftError {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Internal,
            format!("the Windows message loop stopped: {detail}"),
        )
        .with_source(std::io::Error::last_os_error())
        .with_remedy(Remedy::new("Start the helper again:", "clift hotkey"))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    //! No implementation, and it says so.
    //!
    //! X11 has `XGrabKey` and Wayland has, by design, nothing a client can call
    //! -- a global hotkey there belongs to the compositor's own configuration.
    //! One binary that works on one desktop and silently does nothing on
    //! another would be worse than this refusal, which at least names the
    //! alternative.

    use super::{CliftError, ErrorKind, Hotkey, Remedy, Stage};

    pub const IS_SUPPORTED: bool = false;

    pub fn helper_is_running() -> bool {
        false
    }

    pub fn stop_running_helper() -> bool {
        false
    }

    pub fn listen(_hotkey: &Hotkey, _on_press: &mut dyn FnMut()) -> Result<(), CliftError> {
        Err(CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            "Clift cannot register a global hotkey on this platform yet",
        )
        .with_remedy(Remedy::new(
            "Bind a key in your own terminal or window manager to:",
            "clift paste --copy",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clift_core::hotkey;

    /// Whatever this platform answers, an unsupported one must say what to do
    /// instead rather than failing blankly.
    #[test]
    fn an_unsupported_platform_points_at_the_alternative() {
        if is_supported() {
            return;
        }
        let error = listen(&hotkey::default_combination(), &mut || {}).unwrap_err();
        assert_eq!(error.stage(), Stage::Injection);
        assert!(
            error
                .remedy()
                .is_some_and(|remedy| remedy.command().contains("--copy"))
        );
    }
}

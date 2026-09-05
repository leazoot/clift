//! The key combination that triggers a Clift paste, and the rules about which
//! combinations may be taken.
//!
//! A global hotkey is not like other configuration. Registering one takes a key
//! away from every application on the machine for as long as the helper runs,
//! so the interesting part of this module is not the parsing but the two
//! refusals below: a combination with no modifier would swallow a letter
//! everywhere, and the platform's plain paste key is the one thing the specification says
//! Clift must never take.
//!
//! Nothing here knows how to register anything. Virtual key codes are a
//! platform's business and live in the adapter; this is the vocabulary the two
//! sides agree on.
//!
//! [`token_to_redeem`] is the other half of what the key means: one key, and
//! what it does is decided by what is on the clipboard when it is pressed.

use crate::domain::DomainError;
use crate::ports::ClipboardSnapshot;
use crate::universal::Token;

/// The four modifiers every desktop platform has, under one set of names.
///
/// `command` is the Command key on macOS and the Windows key elsewhere. One
/// vocabulary rather than two means a configuration file written on one machine
/// says the same thing on another, which matters because `config.toml` is the
/// sort of file people copy between machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub command: bool,
    pub control: bool,
    pub option: bool,
    pub shift: bool,
}

impl Modifiers {
    #[must_use]
    pub const fn any(self) -> bool {
        self.command || self.control || self.option || self.shift
    }

    #[must_use]
    const fn only_command(self) -> bool {
        self.command && !self.control && !self.option && !self.shift
    }

    #[must_use]
    const fn only_control(self) -> bool {
        self.control && !self.command && !self.option && !self.shift
    }

    #[must_use]
    const fn only_control_shift(self) -> bool {
        self.control && self.shift && !self.command && !self.option
    }
}

/// The non-modifier half of a combination.
///
/// Deliberately small. Punctuation keys differ by keyboard layout, and a
/// combination that means one thing on a US keyboard and another on a German
/// one is worse than not offering it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// An ASCII letter, always stored lowercase.
    Letter(char),
    /// A digit row key, `0` to `9`.
    Digit(u8),
    /// A function key, `F1` to `F12`.
    Function(u8),
}

impl Key {
    fn parse(token: &str) -> Result<Self, DomainError> {
        if let Some(number) = token.strip_prefix('f')
            && let Ok(index) = number.parse::<u8>()
        {
            if (1..=12).contains(&index) {
                return Ok(Key::Function(index));
            }
            return Err(invalid(format!(
                "there is no F{index}; the function keys are F1 to F12"
            )));
        }

        let mut characters = token.chars();
        match (characters.next(), characters.next()) {
            (Some(character), None) if character.is_ascii_lowercase() => Ok(Key::Letter(character)),
            (Some(character), None) if character.is_ascii_digit() => Ok(Key::Digit(
                u8::try_from(character.to_digit(10).unwrap_or(0)).unwrap_or(0),
            )),
            _ => Err(invalid(format!(
                "{token:?} is not a key Clift can register; use a letter, a digit or F1 to F12"
            ))),
        }
    }

    #[must_use]
    pub fn render(self) -> String {
        match self {
            Key::Letter(character) => character.to_string(),
            Key::Digit(digit) => digit.to_string(),
            Key::Function(index) => format!("f{index}"),
        }
    }
}

/// A modifier combination plus one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    modifiers: Modifiers,
    key: Key,
}

impl Hotkey {
    /// Reads a combination such as `cmd+shift+v`.
    ///
    /// Order does not matter and case does not matter; the canonical form comes
    /// back from [`Hotkey::render`]. Aliases are accepted for every modifier
    /// because the same key has three names depending on which platform's
    /// documentation somebody read last.
    ///
    /// # Errors
    /// Refuses an empty or malformed combination, an unknown modifier, a
    /// combination with no modifier at all, and the platform paste key.
    pub fn parse(spec: &str) -> Result<Self, DomainError> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(invalid(
                "a hotkey needs at least one modifier and one key, for example \"cmd+shift+v\"",
            ));
        }

        let tokens: Vec<String> = spec
            .split('+')
            .map(|token| token.trim().to_ascii_lowercase())
            .collect();
        if tokens.iter().any(String::is_empty) {
            return Err(invalid(format!(
                "{spec:?} has an empty part; write it as \"cmd+shift+v\""
            )));
        }

        let (last, rest) = tokens.split_last().unwrap_or((&tokens[0], &[]));
        let key = Key::parse(last)?;

        let mut modifiers = Modifiers::default();
        for token in rest {
            let slot = match token.as_str() {
                "cmd" | "command" | "super" | "win" | "meta" => &mut modifiers.command,
                "ctrl" | "control" => &mut modifiers.control,
                "alt" | "option" | "opt" => &mut modifiers.option,
                "shift" => &mut modifiers.shift,
                other => {
                    return Err(invalid(format!(
                        "{other:?} is not a modifier; use cmd, ctrl, alt or shift"
                    )));
                }
            };
            if *slot {
                return Err(invalid(format!("{token:?} appears twice in {spec:?}")));
            }
            *slot = true;
        }

        Self::new(modifiers, key)
    }

    /// # Errors
    /// Refuses a combination with no modifier, and the plain paste key.
    pub fn new(modifiers: Modifiers, key: Key) -> Result<Self, DomainError> {
        if !modifiers.any() {
            return Err(invalid(
                "a hotkey without a modifier would take that key away from every application; \
                 add cmd, ctrl, alt or shift",
            ));
        }

        // The specification's one prohibition, and the reason it is checked here rather
        // than left to the adapter: registering the plain paste key globally
        // would break ordinary text pasting on the whole machine, which is the
        // failure v2.0 exists to avoid rather than to cause. Both spellings are
        // refused on every platform -- the combination is wrong to take even on
        // a platform where it is not the local paste key.
        if key == Key::Letter('v') && (modifiers.only_command() || modifiers.only_control()) {
            return Err(invalid(
                "Clift will not take the ordinary paste key; choose a combination that adds \
                 shift or alt, for example \"cmd+shift+v\"",
            ));
        }

        Ok(Self { modifiers, key })
    }

    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    #[must_use]
    pub const fn key(&self) -> Key {
        self.key
    }

    /// The canonical spelling, which is what gets written back to the file.
    #[must_use]
    pub fn render(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.modifiers.command {
            parts.push("cmd".to_string());
        }
        if self.modifiers.control {
            parts.push("ctrl".to_string());
        }
        if self.modifiers.option {
            parts.push("alt".to_string());
        }
        if self.modifiers.shift {
            parts.push("shift".to_string());
        }
        parts.push(self.key.render());
        parts.join("+")
    }

    /// Something true about this combination that the user should hear once,
    /// without it being a reason to refuse.
    ///
    /// A registered hotkey stops reaching the focused application, so a
    /// combination an application already uses is a collision the user needs to
    /// be told about rather than protected from -- they may well mean it.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        if self.key == Key::Letter('v') && self.modifiers.only_control_shift() {
            return Some(
                "ctrl+shift+v is the paste key in most terminals on Windows and Linux; while \
                 this helper runs it will paste a Clift attachment there instead"
                    .to_string(),
            );
        }
        // A registration that looks like it worked and then never fires. macOS
        // sends the media key rather than the function key unless Fn is held,
        // so Clift waits for an F9 the keyboard is not sending -- which reads,
        // from the outside, exactly like a broken hotkey.
        if cfg!(target_os = "macos") && matches!(self.key, Key::Function(_)) {
            return Some(format!(
                "on macOS a function key sends its media action instead unless Fn is held or \
                 \"Use F1, F2, etc. keys as standard function keys\" is on in System Settings > \
                 Keyboard; press Fn+{} or choose a letter instead",
                self.render()
            ));
        }
        None
    }
}

/// The token this key press should redeem, if the clipboard holds one.
///
/// One key does both directions, and this is the whole of the decision. Out:
/// there is an image on the clipboard, so publish it. Back: the clipboard holds
/// a token that came off a server's terminal, so redeem it and put the picture
/// here. Neither: the key does nothing, which is what an ordinary paste needs
/// it to do.
///
/// **Only a bare token counts, and that is a safety rule rather than a matter
/// of taste.** When Clift cannot type for the user it falls back to leaving
/// `clift fetch '<token>'` on their clipboard for them to paste. If that line
/// were accepted here, a user who pressed the key a second time -- out of
/// habit, or because they were not sure the first press worked -- would redeem
/// the object they had just published, and objects are single use. The
/// screenshot would be gone, spent on nothing. So the instruction line falls
/// through to "do nothing", and `clift copy` prints a bare token precisely so
/// that the gesture on the other end is unambiguous.
///
/// An image or a file list on the clipboard also means no: those are the
/// outward direction, and a clipboard holding both is not a case worth
/// guessing at.
#[must_use]
pub fn token_to_redeem(snapshot: &ClipboardSnapshot) -> Option<Token> {
    if !snapshot.images.is_empty() || !snapshot.files.is_empty() {
        return None;
    }
    Token::parse(snapshot.text.as_deref()?).ok()
}

/// The combination used when the configuration does not name one.
///
/// Different per platform because the modifier that is comfortable is
/// different, not because the feature is. `cfg!` is a compile-time constant, so
/// this stays a pure function and the crate still touches no platform API.
///
/// On macOS this shadows Terminal.app's "Paste Escaped Text". That is a real
/// cost and the reason the combination is configurable; it is accepted because
/// `cmd+shift+v` is the one combination a macOS user will guess.
#[must_use]
pub fn default_combination() -> Hotkey {
    let modifiers = if cfg!(target_os = "macos") {
        Modifiers {
            command: true,
            shift: true,
            ..Modifiers::default()
        }
    } else {
        Modifiers {
            control: true,
            option: true,
            ..Modifiers::default()
        }
    };
    // Both combinations are valid by construction; the fallback keeps this
    // infallible rather than making every caller handle an impossible error.
    Hotkey::new(modifiers, Key::Letter('v')).unwrap_or(Hotkey {
        modifiers,
        key: Key::Letter('v'),
    })
}

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::new("hotkey", message.into())
}

#[cfg(test)]
mod tests {
    use crate::ports::ClipboardImage;

    /// The four things a clipboard can hold when the key is pressed, and what
    /// each one means. Row three is the one worth reading twice: the
    /// instruction line is what `--copy` leaves behind, and accepting it would
    /// let a second press spend the object the first press created.
    #[test]
    fn one_key_decides_what_to_do_from_what_is_on_the_clipboard() {
        // Built with the product's own encoder rather than typed out, so the
        // test cannot pass by agreeing with a token shape nothing produces.
        let token = Token::new(
            crate::universal::ObjectId::from_bytes([0x11; 16]),
            crate::universal::SealKey::from_bytes([0x22; 32]),
        )
        .expose();
        let token = token.as_str();

        // A bare token, alone: the return trip.
        assert!(super::token_to_redeem(&text(token)).is_some());
        // Whatever the terminal put around it when it was selected.
        assert!(super::token_to_redeem(&text(&format!("  {token}\n"))).is_some());

        // The instruction `paste --copy` writes. Must not redeem.
        assert!(
            super::token_to_redeem(&text(&format!("clift fetch '{token}'"))).is_none(),
            "the instruction line would let a second press spend the object"
        );
        // Ordinary text, and an empty clipboard.
        assert!(super::token_to_redeem(&text("just some notes")).is_none());
        assert!(super::token_to_redeem(&ClipboardSnapshot::default()).is_none());

        // An image is the outward direction, even with a token alongside it.
        let mut both = text(token);
        both.images.push(ClipboardImage {
            mime: "image/png".to_string(),
            path: std::path::PathBuf::from("/tmp/shot.png"),
        });
        assert!(super::token_to_redeem(&both).is_none());
    }

    /// A token from a version this build does not understand is not a token to
    /// redeem here. It falls through to "the key did nothing" rather than
    /// failing loudly, because the clipboard belongs to the user and Clift was
    /// not asked about it.
    #[test]
    fn a_token_from_another_version_is_not_redeemed() {
        assert!(super::token_to_redeem(&text("clift://v2/abc#def")).is_none());
        assert!(super::token_to_redeem(&text("clift://v1/short#short")).is_none());
    }

    fn text(value: &str) -> ClipboardSnapshot {
        ClipboardSnapshot {
            text: Some(value.to_string()),
            ..ClipboardSnapshot::default()
        }
    }

    use super::*;

    fn parse(spec: &str) -> Hotkey {
        Hotkey::parse(spec).unwrap_or_else(|error| panic!("{spec:?}: {error}"))
    }

    #[test]
    fn order_case_and_aliases_all_reach_the_same_combination() {
        let canonical = parse("cmd+shift+v");
        for spec in [
            "cmd+shift+v",
            "shift+cmd+v",
            "CMD+SHIFT+V",
            "  command + shift + v ",
            "super+shift+v",
            "win+shift+v",
        ] {
            assert_eq!(parse(spec), canonical, "{spec:?}");
        }
        assert_eq!(canonical.render(), "cmd+shift+v");
    }

    #[test]
    fn every_key_shape_survives_a_round_trip() {
        for spec in [
            "ctrl+alt+a",
            "cmd+7",
            "cmd+ctrl+alt+shift+f12",
            "alt+f1",
            "shift+0",
        ] {
            assert_eq!(parse(spec).render(), spec, "{spec:?}");
        }
    }

    /// The red line: the plain paste key is not available, whichever spelling
    /// is used and whichever platform this is built for.
    #[test]
    fn the_ordinary_paste_key_is_refused() {
        for spec in ["cmd+v", "ctrl+v", "command+v", "CTRL+V"] {
            let error = Hotkey::parse(spec).unwrap_err();
            assert!(
                error.to_string().contains("ordinary paste key"),
                "{spec:?} was not refused for the right reason: {error}"
            );
        }
        // The neighbouring combinations are fine: only the bare one is taken.
        assert!(Hotkey::parse("cmd+shift+v").is_ok());
        assert!(Hotkey::parse("ctrl+alt+v").is_ok());
        assert!(Hotkey::parse("cmd+ctrl+v").is_ok());
    }

    #[test]
    fn a_combination_without_a_modifier_is_refused() {
        for spec in ["v", "f5", "a"] {
            let error = Hotkey::parse(spec).unwrap_err();
            assert!(error.to_string().contains("modifier"), "{spec:?}: {error}");
        }
    }

    #[test]
    fn malformed_combinations_say_what_is_wrong() {
        for spec in [
            "",
            "   ",
            "cmd+",
            "+v",
            "cmd++v",
            "hyper+v",
            "cmd+shift+vv",
            "cmd+f13",
            "cmd+f0",
            "cmd+;",
        ] {
            let error = Hotkey::parse(spec).unwrap_err();
            assert!(
                error.to_string().len() > 20,
                "{spec:?} was refused without an explanation: {error}"
            );
        }
    }

    #[test]
    fn a_modifier_written_twice_is_refused() {
        let error = Hotkey::parse("cmd+cmd+v").unwrap_err();
        assert!(error.to_string().contains("twice"), "{error}");
    }

    /// A collision the user may well mean is a warning, not a refusal.
    #[test]
    fn the_terminal_paste_key_is_allowed_but_reported() {
        let hotkey = parse("ctrl+shift+v");
        assert!(hotkey.warning().is_some());
        assert!(parse("cmd+shift+v").warning().is_none());
        assert!(parse("ctrl+shift+a").warning().is_none());
    }

    /// Found by using it: `ctrl+alt+f9` registered successfully and then never
    /// fired, because the keyboard was sending a media key.
    #[test]
    fn a_function_key_on_macos_is_allowed_but_reported() {
        let warning = parse("ctrl+alt+f9").warning();
        if cfg!(target_os = "macos") {
            assert!(
                warning.as_deref().is_some_and(|text| text.contains("Fn")),
                "{warning:?}"
            );
        }
        assert!(parse("ctrl+alt+a").warning().is_none());
    }

    #[test]
    fn the_default_is_a_combination_this_module_would_accept() {
        let default = default_combination();
        assert_eq!(parse(&default.render()), default);
        assert!(default.modifiers().any());
    }
}

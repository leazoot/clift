//! Starting the hotkey helper when the user logs in.
//!
//! Part of this crate rather than the CLI because it is the same job as the
//! rest of it: getting the operating system to cooperate with a helper that has
//! no window. The CLI stays a composition root that decides nothing.
//!
//! What this is not is an installer. It writes one file the user can read,
//! delete or edit, registers it, and can take it all back out again -- there is
//! no receipt, no privileged step and nothing left behind after `--uninstall`
//! except the log, which is kept on purpose because it is the only record of
//! why the helper stopped.

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use std::path::{Path, PathBuf};

/// The reverse-DNS name macOS knows the agent by, and the file's stem.
pub const LABEL: &str = "dev.clift.hotkey";

/// Where an installation put things, so the user can be told rather than have
/// to guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The binary that was registered. Reported because it is whichever one
    /// the user happened to run, and because it is the path macOS wants in its
    /// Accessibility list -- not the one they may assume is on their PATH.
    pub program: PathBuf,
    pub definition: PathBuf,
    pub log: PathBuf,
}

/// Registers the helper to start at login.
///
/// # Errors
/// Fails when the home directory cannot be found, when the file cannot be
/// written, when the operating system refuses the registration, and on every
/// platform where this is not implemented.
pub fn install(program: &Path, arguments: &[String]) -> Result<Installed, CliftError> {
    platform::install(program, arguments)
}

/// Removes the registration if there is one.
///
/// # Errors
/// Fails when the file exists and cannot be removed. Finding nothing installed
/// is not a failure: it returns `Ok(None)`, because asking for a state that is
/// already the state is not an error.
pub fn uninstall() -> Result<Option<PathBuf>, CliftError> {
    platform::uninstall()
}

/// The definition file, if one is installed.
#[must_use]
pub fn installed() -> Option<PathBuf> {
    platform::definition_path()
        .ok()
        .filter(|path| path.is_file())
}

/// The binary the installed definition would start, if there is one.
///
/// Read back out of the file rather than remembered, because the reason for
/// asking is to catch the case where it is *not* the binary the user has in
/// mind: macOS attaches the permission to send keystrokes to one binary, and a
/// helper registered against a different copy of `clift` will be refused while
/// looking, from the outside, entirely correct. An answer this crate remembered
/// would only ever agree with itself.
#[must_use]
pub fn registered_program() -> Option<PathBuf> {
    let document = std::fs::read_to_string(installed()?).ok()?;
    platform::program_in(&document).map(PathBuf::from)
}

/// Whether this build can register the helper to start at login.
#[must_use]
pub fn is_supported() -> bool {
    platform::IS_SUPPORTED
}

/// The first entry of `ProgramArguments`, which is the executable.
#[cfg(any(target_os = "macos", test))]
fn first_program_argument(document: &str) -> Option<String> {
    let array = document.split_once("<key>ProgramArguments</key>")?.1;
    let opened = array.split_once("<string>")?.1;
    let (value, _) = opened.split_once("</string>")?;
    Some(unescape(value))
}

/// The reverse of `escape` in the platform module below. Only the three
/// entities that module writes are recognised, because those are the only ones
/// that can be there.
#[cfg(any(target_os = "macos", test))]
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn failed(message: impl Into<String>, remedy: Remedy) -> CliftError {
    CliftError::new(Stage::Injection, ErrorKind::Config, message.into()).with_remedy(remedy)
}

/// The Startup-folder script for Windows, as text.
///
/// Platform-neutral on purpose: the file is a string, and building and
/// reading back the string is the part worth testing on every machine,
/// including the ones that will never run it.
#[cfg(any(windows, test))]
mod windows_startup {
    /// The script `wscript.exe` runs at login.
    ///
    /// `shell.Run` with window style `0` starts the command hidden, and the
    /// command is `cmd.exe /c` only so that the helper's output can be sent
    /// to a log file; `cmd` keeps the outer pair of quotes for itself and
    /// hands the rest on as written. Every `"` inside a VBScript string is
    /// doubled, which is where the runs of quotes below come from. `logFile`
    /// rather than `log` because `Log` is a VBScript function.
    pub fn document(program: &str, arguments: &[String], log: &str) -> String {
        let mut script = String::new();
        script.push_str(
            "' Starts the Clift hotkey helper when you log in, without a console window.\r\n",
        );
        script.push_str("' Managed by clift. Remove it with: clift hotkey --uninstall\r\n");
        script.push_str("Option Explicit\r\n");
        script.push_str("Dim program, logFile, shell\r\n");
        script.push_str(&format!("program = \"{}\"\r\n", escape(program)));
        script.push_str(&format!("logFile = \"{}\"\r\n", escape(log)));
        script.push_str("Set shell = CreateObject(\"WScript.Shell\")\r\n");
        let mut tail = String::new();
        for argument in arguments {
            tail.push(' ');
            tail.push_str(&escape(argument));
        }
        script.push_str(&format!(
            "shell.Run \"cmd.exe /c \"\"\"\"\" & program & \"\"\"{tail} >> \"\"\" & logFile & \"\"\" 2>&1\"\"\", 0, False\r\n"
        ));
        script
    }

    /// The value of the `program = "..."` line, which is the executable.
    pub fn program_in(document: &str) -> Option<String> {
        let line = document
            .lines()
            .find_map(|line| line.trim_end_matches('\r').strip_prefix("program = \""))?;
        let value = line.strip_suffix('"')?;
        Some(unescape(value))
    }

    fn escape(value: &str) -> String {
        value.replace('"', "\"\"")
    }

    fn unescape(value: &str) -> String {
        value.replace("\"\"", "\"")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_script_starts_the_program_hidden_and_logs_to_the_file() {
            let script = document(
                r"C:\Users\x\AppData\Local\Programs\clift\clift.exe",
                &["hotkey".to_string()],
                r"C:\Users\x\AppData\Local\Clift\hotkey.log",
            );
            assert!(script.contains(
                "program = \"C:\\Users\\x\\AppData\\Local\\Programs\\clift\\clift.exe\"\r\n"
            ));
            assert!(
                script
                    .contains("logFile = \"C:\\Users\\x\\AppData\\Local\\Clift\\hotkey.log\"\r\n")
            );
            // The exact line, because the quoting is the whole difficulty:
            // once VBScript has un-doubled the quotes, cmd.exe receives
            //   cmd.exe /c ""<program>" hotkey >> "<log>" 2>&1"
            // and strips the outer pair.
            assert!(script.contains(
                "shell.Run \"cmd.exe /c \"\"\"\"\" & program & \"\"\" hotkey >> \"\"\" & logFile & \"\"\" 2>&1\"\"\", 0, False\r\n"
            ), "{script}");
            assert!(script.contains("clift hotkey --uninstall"));
        }

        #[test]
        fn the_program_can_be_read_back_out_of_the_script() {
            let script = document(r"C:\a b\clift.exe", &["hotkey".to_string()], r"C:\l.log");
            assert_eq!(program_in(&script).as_deref(), Some(r"C:\a b\clift.exe"));
            assert_eq!(program_in("' nothing here"), None);
        }

        /// What VBScript will see once it has parsed the literal, spelled out
        /// so a change to the quoting cannot pass by looking plausible.
        #[test]
        fn the_command_line_cmd_receives_is_the_documented_one() {
            let script = document(r"C:\p\clift.exe", &["hotkey".to_string()], r"C:\l\h.log");
            let line = script
                .lines()
                .find(|line| line.starts_with("shell.Run "))
                .unwrap();
            let literal = line
                .strip_prefix("shell.Run ")
                .and_then(|rest| rest.strip_suffix(", 0, False"))
                .unwrap();
            // Evaluate the VBScript expression: string literals with doubled
            // quotes, joined with `&`, and two variables.
            let mut command = String::new();
            for part in literal.split(" & ") {
                match part {
                    "program" => command.push_str(r"C:\p\clift.exe"),
                    "logFile" => command.push_str(r"C:\l\h.log"),
                    quoted => {
                        let inner = quoted
                            .strip_prefix('"')
                            .and_then(|q| q.strip_suffix('"'))
                            .unwrap();
                        command.push_str(&inner.replace("\"\"", "\""));
                    }
                }
            }
            assert_eq!(
                command,
                r#"cmd.exe /c ""C:\p\clift.exe" hotkey >> "C:\l\h.log" 2>&1""#
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! A launchd user agent.
    //!
    //! `KeepAlive` is on. A hotkey helper that quietly died in November and was
    //! not noticed until January is worse than one that restarts, and the only
    //! way out is the uninstall below -- which is why it exists as a command
    //! rather than as an instruction to go and delete a file.

    use super::{Installed, failed};
    use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub const IS_SUPPORTED: bool = true;

    pub fn program_in(document: &str) -> Option<String> {
        super::first_program_argument(document)
    }

    pub fn definition_path() -> Result<PathBuf, CliftError> {
        Ok(home()?
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", super::LABEL)))
    }

    fn log_path() -> Result<PathBuf, CliftError> {
        Ok(home()?.join("Library/Logs/clift-hotkey.log"))
    }

    fn home() -> Result<PathBuf, CliftError> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                failed(
                    "HOME is not set to an absolute path, so there is nowhere to install to",
                    Remedy::new("Run the helper in the foreground instead:", "clift hotkey"),
                )
            })
    }

    pub fn install(program: &Path, arguments: &[String]) -> Result<Installed, CliftError> {
        let definition = definition_path()?;
        let log = log_path()?;

        if let Some(parent) = definition.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                io_failed(format!("cannot create {}", parent.display()), &error)
            })?;
        }

        // Written whole and moved into place, so an interrupted write cannot
        // leave launchd a half-parsed file to choke on at the next login.
        let temporary = definition.with_extension("plist.partial");
        std::fs::write(&temporary, document(program, arguments, &log))
            .map_err(|error| io_failed(format!("cannot write {}", temporary.display()), &error))?;
        std::fs::rename(&temporary, &definition).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            io_failed(format!("cannot replace {}", definition.display()), &error)
        })?;

        let domain = format!("gui/{}", user_id()?);
        // An existing registration must go first: `bootstrap` refuses a label
        // that is already loaded, and a reinstall after changing the
        // combination is the ordinary case rather than the rare one.
        let _ = launchctl(&["bootout".to_string(), format!("{domain}/{}", super::LABEL)]);
        launchctl(&[
            "bootstrap".to_string(),
            domain,
            definition.display().to_string(),
        ])?;

        Ok(Installed {
            program: program.to_path_buf(),
            definition,
            log,
        })
    }

    pub fn uninstall() -> Result<Option<PathBuf>, CliftError> {
        let definition = definition_path()?;
        if !definition.is_file() {
            return Ok(None);
        }
        // Unloaded before the file goes, so launchd is not left holding a
        // registration whose definition has disappeared.
        if let Ok(uid) = user_id() {
            let _ = launchctl(&["bootout".to_string(), format!("gui/{uid}/{}", super::LABEL)]);
        }
        std::fs::remove_file(&definition).map_err(|error| {
            io_failed(format!("cannot remove {}", definition.display()), &error)
        })?;
        Ok(Some(definition))
    }

    /// launchd needs the numeric uid to name the per-user domain, and asking
    /// the system for it is more reliable than an environment variable a shell
    /// may or may not have exported.
    fn user_id() -> Result<String, CliftError> {
        let output = Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .map_err(|error| io_failed("cannot run /usr/bin/id".to_string(), &error))?;
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
            return Err(failed(
                "could not determine this user's numeric id",
                Remedy::new("Run the helper in the foreground instead:", "clift hotkey"),
            ));
        }
        Ok(uid)
    }

    fn launchctl(arguments: &[String]) -> Result<(), CliftError> {
        let output = Command::new("/bin/launchctl")
            .args(arguments)
            .output()
            .map_err(|error| io_failed("cannot run /bin/launchctl".to_string(), &error))?;
        if output.status.success() {
            return Ok(());
        }
        // launchd's own words, kept rather than summarised: they name the
        // actual obstacle far better than any sentence written here could.
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(failed(
            format!(
                "launchctl {} failed: {}",
                arguments.first().map_or("", String::as_str),
                if detail.is_empty() {
                    "no reason given"
                } else {
                    &detail
                }
            ),
            Remedy::new("Run the helper in the foreground instead:", "clift hotkey"),
        ))
    }

    fn io_failed(message: String, error: &std::io::Error) -> CliftError {
        CliftError::new(Stage::Injection, ErrorKind::Config, message)
            .with_source(std::io::Error::new(error.kind(), error.to_string()))
            .with_remedy(Remedy::new(
                "Run the helper in the foreground instead:",
                "clift hotkey",
            ))
    }

    fn document(program: &Path, arguments: &[String], log: &Path) -> String {
        let mut argv = String::new();
        argv.push_str(&format!(
            "\t\t<string>{}</string>\n",
            escape(&program.display().to_string())
        ));
        for argument in arguments {
            argv.push_str(&format!("\t\t<string>{}</string>\n", escape(argument)));
        }

        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
{argv}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
            label = super::LABEL,
            log = escape(&log.display().to_string()),
        )
    }

    /// A home directory can contain `&` and `<`, and an unescaped one would
    /// produce a plist launchd silently refuses to parse.
    fn escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_document_names_the_program_its_arguments_and_the_log() {
            let plist = document(
                Path::new("/usr/local/bin/clift"),
                &["hotkey".to_string()],
                Path::new("/Users/x/Library/Logs/clift-hotkey.log"),
            );
            assert!(plist.starts_with("<?xml"));
            assert!(plist.contains("<string>/usr/local/bin/clift</string>"));
            assert!(plist.contains("<string>hotkey</string>"));
            assert!(plist.contains("clift-hotkey.log"));
            assert!(plist.contains(super::super::LABEL));
        }

        /// A path with XML metacharacters must not produce a file launchd
        /// cannot parse. Rare, and silent when it happens, which is what makes
        /// it worth a test.
        #[test]
        fn metacharacters_in_a_path_are_escaped() {
            let plist = document(
                Path::new("/Users/a&b/<clift>"),
                &[],
                Path::new("/tmp/l&g.log"),
            );
            assert!(plist.contains("/Users/a&amp;b/&lt;clift&gt;"));
            assert!(plist.contains("/tmp/l&amp;g.log"));
            assert!(!plist.contains("a&b"));
        }

        /// What is written must be what is read back, escaping and all. The
        /// two halves are what `doctor` compares against the running binary,
        /// so a disagreement between them would produce a warning about a
        /// mismatch that does not exist.
        #[test]
        fn the_program_can_be_read_back_out_of_the_document() {
            let plist = document(
                Path::new("/Users/a&b/clift"),
                &["hotkey".to_string()],
                Path::new("/tmp/log"),
            );
            assert_eq!(
                super::super::first_program_argument(&plist).as_deref(),
                Some("/Users/a&b/clift")
            );
        }
    }
}

#[cfg(windows)]
mod platform {
    //! A script in the user's Startup folder.
    //!
    //! `clift.exe` is a console program, and a console program started from a
    //! Startup shortcut or a `Run` registry value gets a console window that
    //! sits on the taskbar for as long as the helper runs; "no window" rules
    //! that out. The script is run by `wscript.exe`, which has no console of
    //! its own, and starts the helper with the window style "hidden", so
    //! nothing appears at login and nothing has to be kept open.
    //!
    //! Windows has no launchd to stop the helper, so the helper listens for a
    //! named event and both `--uninstall` and a reinstall set it; see the
    //! hotkey module. That is also how "is running now" is known here rather
    //! than assumed: the installer waits to see the event appear.
    //!
    //! Windows Script Host is old, and Microsoft has said it will one day be
    //! optional. When that happens this module changes and nothing else does.

    use super::{Installed, failed, windows_startup};
    use crate::hotkey::{helper_is_running, stop_running_helper};
    use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    pub const IS_SUPPORTED: bool = true;

    pub fn program_in(document: &str) -> Option<String> {
        windows_startup::program_in(document)
    }

    pub fn definition_path() -> Result<PathBuf, CliftError> {
        Ok(known_folder("APPDATA")?
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join("clift-hotkey.vbs"))
    }

    fn log_path() -> Result<PathBuf, CliftError> {
        Ok(known_folder("LOCALAPPDATA")?
            .join("Clift")
            .join("hotkey.log"))
    }

    /// One of the two per-user folders Windows always sets; the same two the
    /// rest of Clift keeps its files under.
    fn known_folder(name: &str) -> Result<PathBuf, CliftError> {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                failed(
                    format!(
                        "{name} is not set to an absolute path, so there is nowhere to install to"
                    ),
                    Remedy::new("Run the helper in the foreground instead:", "clift hotkey"),
                )
            })
    }

    pub fn install(program: &Path, arguments: &[String]) -> Result<Installed, CliftError> {
        let definition = definition_path()?;
        let log = log_path()?;

        for parent in [definition.parent(), log.parent()].into_iter().flatten() {
            std::fs::create_dir_all(parent).map_err(|error| {
                io_failed(format!("cannot create {}", parent.display()), &error)
            })?;
        }

        // Written whole and moved into place, so an interrupted write cannot
        // leave a half script for the next login to run.
        let temporary = definition.with_extension("vbs.partial");
        let document = windows_startup::document(
            &program.display().to_string(),
            arguments,
            &log.display().to_string(),
        );
        std::fs::write(&temporary, document)
            .map_err(|error| io_failed(format!("cannot write {}", temporary.display()), &error))?;
        std::fs::rename(&temporary, &definition).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            io_failed(format!("cannot replace {}", definition.display()), &error)
        })?;

        // A helper already running holds the old combination. It is stopped
        // and waited for, so the one started next is the one the file
        // describes and the two never fight over the registration.
        if stop_running_helper() && !settles(|| !helper_is_running(), Duration::from_secs(3)) {
            return Err(failed(
                "the hotkey helper that was already running did not stop",
                Remedy::new("Log out and in again, then check:", "clift doctor"),
            ));
        }

        let mut command = Command::new("wscript.exe");
        command
            .arg("//B")
            .arg("//Nologo")
            .arg(&definition)
            // Nothing of this console is handed down: the helper is meant to
            // outlive it.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .spawn()
            .map_err(|error| io_failed("cannot run wscript.exe".to_string(), &error))?;

        // Seen, not assumed: the helper owns a named event once it is
        // listening, and only its appearance counts as "running now".
        if !settles(helper_is_running, Duration::from_secs(5)) {
            return Err(failed(
                format!(
                    "the login entry was written but the helper did not start; its log is {}",
                    log.display()
                ),
                Remedy::new("Run it in the foreground to see why:", "clift hotkey"),
            ));
        }

        Ok(Installed {
            program: program.to_path_buf(),
            definition,
            log,
        })
    }

    pub fn uninstall() -> Result<Option<PathBuf>, CliftError> {
        // The running helper goes first, whether or not the file is still
        // there: a helper with no login entry is the one case where the user
        // has nothing else to stop it with.
        stop_running_helper();
        let definition = definition_path()?;
        if !definition.is_file() {
            return Ok(None);
        }
        std::fs::remove_file(&definition).map_err(|error| {
            io_failed(format!("cannot remove {}", definition.display()), &error)
        })?;
        Ok(Some(definition))
    }

    /// Polls `condition` until it holds or `patience` runs out.
    fn settles(condition: impl Fn() -> bool, patience: Duration) -> bool {
        let started = Instant::now();
        loop {
            if condition() {
                return true;
            }
            if started.elapsed() > patience {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn io_failed(message: String, error: &std::io::Error) -> CliftError {
        CliftError::new(Stage::Injection, ErrorKind::Config, message)
            .with_source(std::io::Error::new(error.kind(), error.to_string()))
            .with_remedy(Remedy::new(
                "Run the helper in the foreground instead:",
                "clift hotkey",
            ))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    //! Not implemented, and it says so rather than pretending.
    //!
    //! On Linux this would be a systemd user unit or an XDG autostart entry
    //! depending on the desktop, and the hotkey itself is not implemented
    //! there yet.

    use super::{Installed, failed};
    use clift_core::error::{CliftError, Remedy};
    use std::path::{Path, PathBuf};

    pub const IS_SUPPORTED: bool = false;

    pub fn program_in(_document: &str) -> Option<String> {
        None
    }

    pub fn definition_path() -> Result<PathBuf, CliftError> {
        Err(not_implemented())
    }

    pub fn install(_program: &Path, _arguments: &[String]) -> Result<Installed, CliftError> {
        Err(not_implemented())
    }

    pub fn uninstall() -> Result<Option<PathBuf>, CliftError> {
        Err(not_implemented())
    }

    fn not_implemented() -> CliftError {
        failed(
            "Clift cannot register itself to start at login on this platform yet",
            Remedy::new(
                "Start the helper yourself, from your desktop's own startup settings:",
                "clift hotkey",
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::first_program_argument;

    #[test]
    fn a_document_without_program_arguments_yields_nothing() {
        assert_eq!(first_program_argument("<plist><dict></dict></plist>"), None);
        assert_eq!(
            first_program_argument("<key>ProgramArguments</key><array></array>"),
            None
        );
    }

    /// The `Label` string comes before `ProgramArguments` in what Clift writes,
    /// and taking the first `<string>` in the file rather than the first one
    /// after the key would return the label instead of the binary.
    #[test]
    fn the_label_is_not_mistaken_for_the_program() {
        let document = "\
<key>Label</key>
<string>dev.clift.hotkey</string>
<key>ProgramArguments</key>
<array>
\t<string>/usr/local/bin/clift</string>
\t<string>hotkey</string>
</array>";
        assert_eq!(
            first_program_argument(document).as_deref(),
            Some("/usr/local/bin/clift")
        );
    }
}

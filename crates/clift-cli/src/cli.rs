//! The command tree from the specification.
//!
//! One command from that tree is absent on purpose: `clift update` is not
//! built. Listing it in `--help` while it does nothing would advertise support
//! that does not exist, so an unknown subcommand error is the honest outcome
//! until it is built.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "clift",
    about = "Universal attachment bridge for remote terminal agents",
    // Version is handled here rather than by clap, because the specification requires the
    // commit and target triple alongside the crate version.
    disable_version_flag = true
)]
pub struct Cli {
    /// Print the version, commit and target platform
    #[arg(short = 'V', long, global = true)]
    pub version: bool,

    /// Emit machine readable output on stdout
    #[arg(long, global = true)]
    pub json: bool,

    /// Report each stage and how long it took
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Report diagnostic detail, including the full cause chain
    #[arg(long, global = true)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Prepare a host, or with no host, walk through the first-time setup
    Setup {
        /// SSH host alias from ~/.ssh/config; leave it out to be asked what to set up
        ssh_host: Option<String>,

        /// Answer the confirmation in advance, for non-interactive runs
        #[arg(long)]
        yes: bool,
    },

    /// Send attachments and print the text to paste into the agent
    Send {
        /// Files to send
        files: Vec<String>,

        /// Take the attachments from the clipboard
        #[arg(long)]
        clipboard: bool,

        /// Target to send to
        #[arg(long)]
        to: Option<String>,

        /// Replace the local clipboard with the resulting text
        #[arg(long)]
        copy: bool,

        /// Insertion text profile
        #[arg(long)]
        format: Option<String>,
    },

    /// Paste an attachment into whatever terminal you are in
    ///
    /// Two modes, and they answer "which host?" in opposite ways.
    ///
    /// Universal Mode seals the attachment locally, leaves the ciphertext with
    /// a relay that cannot read it, and gives you one short token to paste.
    /// Whichever host your terminal is talking to is the host that redeems it,
    /// so there is no target to choose and no terminal plugin to install.
    ///
    /// Fast Mode uploads over your own SSH to a target resolved before
    /// anything is sent -- `--to`, or your default target. Use it for anything
    /// large, or anywhere a relay is not wanted.
    ///
    /// Without `--mode`, Clift uses Universal if a relay is configured and
    /// Fast if one is not.
    Paste {
        /// Which mode to use: universal or fast
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,

        /// Target to send to (Fast Mode only)
        #[arg(long)]
        to: Option<String>,

        /// Put the text on the clipboard instead of typing it
        #[arg(long)]
        copy: bool,

        /// Type the instruction into the focused window
        #[arg(long, conflicts_with = "copy")]
        inject: bool,
    },

    /// Redeem a Universal Mode token and print where the attachment landed
    ///
    /// Run on the host the attachment is for, usually by the agent. The token
    /// is single use: once this succeeds the relay has nothing left to give,
    /// and a second attempt fails with exit code 27.
    ///
    ///   clift fetch 'clift://v1/<id>#<key>'
    ///
    /// It prints one absolute path per attachment on stdout and nothing else;
    /// everything else goes to stderr. Quote the token. It contains a '#',
    /// which every POSIX shell reads as the start of a comment.
    Fetch {
        /// The token that was pasted into this session
        token: String,

        /// Accepted for lines pasted by earlier versions; printing the path
        /// is the default now
        #[arg(long, hide = true)]
        print_path: bool,

        /// Put the attachment on this machine's clipboard instead of printing
        /// its path; for tokens that came back from a server
        #[arg(long)]
        copy: bool,
    },

    /// Seal a file on this machine and print a token to paste on your own
    ///
    /// The return trip. Run it on the server, on a file you can already see;
    /// it prints one bare token and nothing else. Select that line, copy it the
    /// way you copy anything in your terminal, and press your Clift key at
    /// home: the file arrives on your clipboard.
    ///
    ///   clift copy build/report.png
    ///
    /// This host needs the same relay as the machine you paste on. The token is
    /// single use and expires; a file too large for the relay is a job for scp.
    Copy {
        /// Files to seal and publish
        #[arg(required = true)]
        files: Vec<String>,
    },

    /// Run one key combination that pastes an attachment, in any application
    ///
    /// The terminal-independent half of Clift: instead of a plugin per
    /// terminal, one combination registered with the operating system, which
    /// does what `clift paste` does wherever the cursor happens to be.
    ///
    /// This is the only part of Clift that keeps running between commands. It
    /// reads nothing until the key is pressed -- there is no clipboard watching
    /// here -- and it stops when you stop it.
    ///
    ///   clift hotkey                    run it here, Ctrl+C to stop
    ///   clift hotkey --install          run it from login onwards
    ///   clift hotkey --uninstall        stop doing that
    ///
    /// The combination comes from `hotkey.combination` in the configuration,
    /// and defaults to cmd+shift+v on macOS and ctrl+alt+v elsewhere. Clift
    /// will not register the plain paste key.
    Hotkey {
        /// Combination for this run only, such as cmd+shift+v
        #[arg(long, value_name = "COMBINATION")]
        key: Option<String>,

        /// Register the helper to start at login, and start it now
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,

        /// Remove the login registration
        #[arg(long)]
        uninstall: bool,
    },

    /// Manage configured targets
    Target {
        #[command(subcommand)]
        command: TargetCommand,
    },

    /// Check everything needed for a send to succeed
    Doctor {
        /// Target to check; defaults to the default target
        target: Option<String>,
    },

    /// Show configured targets, the default target and the current version
    ///
    /// A binding is listed only while it is in use: one whose session ended,
    /// whose lease lapsed, or whose target has been removed is left out rather
    /// than shown as something still in effect.
    Status,

    /// Remove expired batches from a remote inbox
    Clean {
        /// Target to clean; defaults to the default target
        target: Option<String>,

        /// Remove every batch, not only the expired ones
        #[arg(long)]
        all: bool,

        /// Only remove batches older than this, for example 7d
        #[arg(long)]
        older_than: Option<String>,

        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,

        /// Show what would be removed without removing it
        #[arg(long)]
        dry_run: bool,
    },

    /// Inspect and edit the configuration file
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Remove what Clift registered on this machine and, optionally, its configuration
    Uninstall {
        /// Also delete the local configuration
        #[arg(long)]
        purge: bool,

        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,

        /// List what would change without changing anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TargetCommand {
    /// Add a target for an SSH host alias
    Add {
        name: String,
        #[arg(long)]
        ssh_host: Option<String>,
    },
    /// List configured targets
    List,
    /// Make a target the default
    Use { name: String },
    /// Rename a target
    Rename { from: String, to: String },
    /// Remove a target, leaving its remote inbox untouched
    Remove { name: String },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved path of the configuration file
    Path,
    /// Print one configuration value
    Get { key: String },
    /// Set one configuration value
    Set { key: String, value: String },
    /// Parse the configuration and report every problem found
    Validate,
}

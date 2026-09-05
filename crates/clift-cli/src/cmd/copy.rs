//! `clift copy <file>...`: hand a file on this machine to whoever is looking at
//! this terminal.
//!
//! The return trip, and it is not the mirror image of `clift paste`. Going out,
//! the attachment has no path at all -- it is a screenshot living in a
//! clipboard, and the whole job is to give it one on the far side. Coming back,
//! the file already has a path; you can see it, and `scp` would do. What this
//! adds is the case where the machine you are sitting at cannot reach this one
//! directly, and the only channel between them is the text of the terminal you
//! are already looking at.
//!
//! So the entire output is one bare token on stdout. Bare is load-bearing: the
//! key press on the other end has to tell a token apart from the instruction
//! `paste --copy` leaves on a clipboard, and it does that by accepting only a
//! token on its own. Wrap this one in an instruction and pressing the key twice
//! after a `--copy` would redeem the object the user had just published.
//!
//! Nothing here is new machinery. The sealing, the frame, the relay and the
//! token are the same code the outward direction runs, called with a different
//! source and a different way of showing the result.

use crate::cmd::universal::{self, Delivery};
use crate::output::Reporter;
use crate::relay;
use clift_core::config;
use clift_core::error::{CliftError, Remedy};
use clift_core::universal::unconfigured_for_return;
use clift_core::usecase;
use std::path::PathBuf;

/// # Errors
/// Fails when a named file is not something Clift can send, when this host has
/// no relay, when the attachment will not fit through it, and when the relay
/// refuses or cannot be reached.
pub fn run(files: &[String], reporter: &Reporter) -> Result<(), CliftError> {
    let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    // No clipboard argument, and not because this machine might not have one:
    // `clift copy` is about files that were named. A server that did have a
    // clipboard would still not be asked for it.
    let resolved = usecase::resolve(&paths, None)?;

    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }

    // Asked here rather than left to the relay resolution inside `run`, because
    // the two "no relay" situations need different advice and only this level
    // knows which one it is in. On the machine a user pastes from, the fix is
    // to pick a relay; here it is to use the one the other machine already has.
    if !relay::is_configured(&loaded.config) {
        return Err(unconfigured_for_return());
    }

    universal::run(
        &loaded.config,
        resolved.attachments(),
        Delivery::Token,
        &copy_it_directly_instead(),
        reporter,
    )
}

/// The way out when the file will not fit through the relay.
///
/// Deliberately not `clift send --to <target>`, which is what the outward
/// direction suggests. A server has no target pointing back at the laptop, and
/// telling its user to configure one would send them to set up a mode that
/// cannot reach them anyway. The file already has a path here, so the honest
/// advice is the one that does not involve Clift at all.
fn copy_it_directly_instead() -> Remedy {
    Remedy::new(
        "Copy it straight from your own machine instead:",
        "scp <this-host>:<path> .",
    )
}

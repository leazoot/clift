//! What Clift knows about the machine it is running on before it connects.
//!
//! Two things live here: what the user's own SSH configuration says about a
//! host, and whether Clift is allowed to ask the user a question at all.

pub mod confirm;
pub mod sshconfig;

pub use confirm::{Confirmation, confirmation_for};
pub use sshconfig::{
    SshHostSettings, effective_setting, multiplexes_already, parse_effective_config,
};

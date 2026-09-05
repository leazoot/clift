//! Use cases: the order in which things happen.
//!
//! A use case takes its dependencies as ports and performs no IO of its own. It
//! decides flow -- what runs first, what aborts, what is swallowed -- and never
//! rewrites the meaning of an error it is handed.

mod fetch;
mod publish;
mod send;
mod setup;
mod source;
mod target;
mod uninstall;

pub use fetch::{Fetched, fetch};
pub use publish::{Published, PublishedEntry, publish};
pub use send::{SendOutcome, SendPolicy, perform, stage_attachments};
pub use setup::{SetupReport, SetupStep, prepare_target};
pub use source::{Origin, Resolved, resolve};
pub use target::{
    Removal, TargetSummary, add, list, remove, rename, resolve_send_target, use_default,
};
pub use uninstall::{RemoteLeftovers, UninstallPlan, cleanup_command, plan_uninstall};

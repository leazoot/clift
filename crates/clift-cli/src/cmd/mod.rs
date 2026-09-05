//! Command implementations.
//!
//! Each module here parses nothing and decides nothing: it calls into
//! `clift-core` and renders the result onto the right channel.

pub mod clean;
pub mod config;
pub mod copy;
pub mod doctor;
pub mod fetch;
pub mod first_run;
pub mod hotkey;
pub mod paste;
pub mod send;
pub mod setup;
pub mod status;
pub mod target;
pub mod uninstall;
pub mod universal;

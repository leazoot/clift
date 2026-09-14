//! OpenSSH/SFTP transport adapter for Clift.
//!
//! Drives the system `ssh` executable with parameterised arguments, and speaks
//! SFTP to the server's subsystem over it. It never links an SSH protocol
//! library, never reads private key material and never weakens the user's host
//! key verification.

#![forbid(unsafe_code)]

pub mod errmap;
pub mod fsops;
pub mod probe;
pub mod proc;
pub mod reuse;
pub mod session;
pub mod sshconfig;
pub mod upload;
pub mod wire;

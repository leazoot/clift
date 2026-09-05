//! OpenSSH/SFTP transport adapter for Clift.
//!
//! Drives the system `ssh` and `sftp` executables with parameterised arguments.
//! It never links an SSH protocol library, never reads private key material and
//! never weakens the user's host key verification.

#![forbid(unsafe_code)]

pub mod errmap;
pub mod fsops;
pub mod probe;
pub mod proc;
pub mod reuse;
pub mod session;
pub mod sshconfig;
pub mod upload;

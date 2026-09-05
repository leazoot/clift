//! Domain model, use cases and ports for Clift.
//!
//! This crate owns every business rule: source priority, target resolution,
//! batch limits, cleanup boundaries and the error-to-exit-code mapping. It must
//! never perform IO or touch a platform API: adapters do that behind the ports
//! defined here, so the rules stay testable and platform independent.

#![forbid(unsafe_code)]

pub mod attachments;
pub mod calendar;
pub mod config;
pub mod context;
pub mod diagnostics;
pub mod domain;
pub mod error;
pub mod exit;
pub mod format;
pub mod hotkey;
pub mod places;
pub mod ports;
pub mod runtime;
pub mod staging;
pub mod universal;
pub mod usecase;

// Fakes are compiled only for tests and for consumers that explicitly opt in.
// Nothing that ships enables this feature, which is what keeps the specification's ban
// on mock end-to-end tests enforceable rather than aspirational.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

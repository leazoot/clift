//! Turning remote paths, and tokens, into the text that goes into an agent's
//! prompt.
//!
//! Two profiles, because the two modes hand the agent different things. Fast
//! Mode gives it a path to a file that is already on the host; Universal Mode
//! gives it one command to run, because the file is not there yet.

pub mod instruction;
pub mod token;

pub use instruction::render;
pub use token::render as render_token;

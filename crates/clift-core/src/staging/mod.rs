//! Where attachments live on the remote host, and the rules that keep that
//! place private.

mod atomic;
mod batch;
mod clean;
mod inbox;
mod local;
mod selfcheck;

pub use atomic::{StagedBatch, StagedFile, stage_batch};
pub use batch::{BatchPlan, create_batch, plan_batch};
pub use clean::{Action, CleanReport, Retention, clean};
pub use inbox::{INBOX_MODE, InboxLocation, InboxRootSource, ensure_inbox, locate_inbox};
pub use local::{WrittenBatch, WrittenFile, inbox_root as local_inbox_root, write_batch};
pub use selfcheck::{SELF_CHECK_NAME, verify_round_trip};

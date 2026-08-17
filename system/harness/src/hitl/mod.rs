//! `hex hitl` — the pending-human-action queue.
//!
//! Agents file an item whenever they are blocked on something only a human can
//! do (sign a doc, pass KYC, pay an invoice, grant a permission). The queue is
//! plain files under `$HEX_DIR/.hex/hitl/` so it survives restarts, is
//! inspectable by hand, and needs no daemon:
//!
//! ```text
//! $HEX_DIR/.hex/hitl/
//!   items/<id>.toml   one file per item (ids are sequential, 1-based)
//!   log.jsonl         append-only: every state transition, every ping sent
//!   config.toml       mode / imessage_handle / digest_hour / quiet hours / cap
//!   state/            small stamp files (digest-sent marker, ping counters)
//! ```
//!
//! Layering: `store` owns the on-disk shape (this task). `policy` (pure ping
//! decisions) and `transport` (iMessage send) land alongside it and are
//! declared here once they exist.

pub mod policy;
pub mod store;
pub mod transport;

pub use policy::{compose_digest, pings_due, Digest, PingAction, PingReason};
pub use store::{Config, Item, Mode, NewItem, Priority, Status};
pub use transport::{Outcome, Sender};

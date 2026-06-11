//! Live escalation path (Phase A2, SPEC-A2 §2): a hand-rolled stdio LSP
//! client (`lsp`) and the per-worktree rust-analyzer instance lifecycle
//! (`instance`).
//!
//! One instance == one worktree — no sharing, no overlays, no chimera
//! answers. Instances are Warming until rust-analyzer reports quiescent via
//! `experimental/serverStatus`; warming instances answer nothing but a
//! structured `LiveError::Warming`. The pool (Task 3) manages instances
//! through the `LiveBackend` trait so its policy tests never pay a real
//! rust-analyzer prime.
//!
//! Std threads + blocking IO throughout; deliberately NO `lsp-types`,
//! `tower-lsp`, or tokio (SPEC-A2 §7).

pub mod instance;
pub mod lsp;
pub mod pool;
pub mod translate;

pub use instance::{InstanceState, LiveBackend, LiveError, LiveInstance, LiveResult};
pub use pool::Pool;

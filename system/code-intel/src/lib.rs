//! scipd_core — index-backed code intelligence for agent fleets (Phase A1).
//!
//! See `docs/code-intel/SPEC-A1.md` for the contract. This crate hosts:
//! - the error taxonomy mapping to CLI exit codes (spec §5),
//! - the JSON response envelope every query verb emits (spec §5),
//! - (later tasks) workspace identity, generation store, SCIP ingest,
//!   the query engine, freshness, and the indexer.

pub mod envelope;
pub mod error;
pub mod ingest;
pub mod schema;
pub mod store;

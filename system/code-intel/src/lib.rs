//! scipd_core — index-backed code intelligence for agent fleets (Phase A1).
//!
//! See `docs/code-intel/SPEC-A1.md` for the contract. This crate hosts:
//! - the error taxonomy mapping to CLI exit codes (spec §5),
//! - the JSON response envelope every query verb emits (spec §5),
//! - workspace identity, registry, and worktree resolution (spec §3),
//! - (later tasks) generation store, SCIP ingest, the query engine,
//!   freshness, and the indexer.

pub mod config;
pub mod daemon;
pub mod doctor;
pub mod envelope;
pub mod error;
pub mod freshness;
pub mod indexer;
pub mod ingest;
pub mod proto;
pub mod query;
pub mod respond;
pub mod schema;
pub mod store;
pub mod workspace;

// Self-alias so module files compiled into this crate via `#[path] mod` (the
// `*.worker.rs` overlay) can `use hex::…` uniformly, whether they live in-crate
// (core modules) or out-of-crate (personal modules).
extern crate self as hex;

pub mod act_evidence;
pub mod harness;
pub mod capability_exec;
pub mod audit;
pub mod capability_guard;
pub mod memory;
pub mod messages;
pub mod ops;
pub mod registry;
pub mod telemetry;
pub mod types;
pub mod worker;
pub mod workers;

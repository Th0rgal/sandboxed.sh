//! Node-side runtime for the `sandboxed-node` runner binary.
//!
//! Lives in the library crate (rather than the binary) so the durable job
//! store and the job runner are unit-testable with `cargo test --lib`.

pub mod job_store;
pub mod lean;
pub mod runner;

pub use job_store::{JobRecord, JobState, JobStore};
pub use lean::{cached_toolchains, spawn_cache_gc};
pub use runner::{
    maybe_exec_cleared_scope_payload, read_log_tail, JobRunner, NodeQueueFull,
    DEFAULT_MAX_JOB_SECS, LOG_TAIL_MAX_BYTES,
};

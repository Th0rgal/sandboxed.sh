//! Paloma core primitives.
//!
//! Telegram remains the delivery adapter; this module holds the small,
//! deterministic building blocks that can be shared by future channels.

pub mod brain;
pub mod capability;
pub mod channel;
pub mod commands;
pub mod decision_log;
pub mod digest;
pub mod event;
pub mod memory;
pub mod planner;
pub mod policy;
pub mod queue;
pub mod satellite;
pub mod scheduler;

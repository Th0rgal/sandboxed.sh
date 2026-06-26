//! Paloma core primitives.
//!
//! Telegram remains the delivery adapter; this module holds the small,
//! deterministic building blocks that can be shared by future channels.

pub mod commands;
pub mod cooldown;
pub mod decision_log;
pub mod digest;
pub mod event;
pub mod mission_card;
pub mod planner;
pub mod policy;
pub mod preferences;
pub mod queue;
pub mod scheduler;

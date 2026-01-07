//! Task module - defines tasks and deliverable tracking.

pub mod deliverables;
pub mod task;

pub use deliverables::{extract_deliverables, Deliverable, DeliverableSet};
pub use task::{Task, TaskAnalysis, TaskCost, TaskError, TaskId, TaskStatus};

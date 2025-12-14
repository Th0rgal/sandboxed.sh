//! Leaf agents - specialized agents that do actual work.
//!
//! # Leaf Agent Types
//! - `ComplexityEstimator`: Estimates task complexity (0-1 score)
//! - `ModelSelector`: Selects optimal model for task/budget
//! - `TaskExecutor`: Executes tasks using tools (main worker)
//! - `Verifier`: Validates task completion

mod complexity;
mod model_select;
mod executor;
mod verifier;

pub use complexity::{ComplexityEstimator, ComplexityPromptVariant};
pub use model_select::ModelSelector;
pub use executor::TaskExecutor;
pub use verifier::Verifier;


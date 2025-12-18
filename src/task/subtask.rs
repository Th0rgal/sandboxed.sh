//! Subtask definitions and splitting logic.
//!
//! When a task is too complex, it can be split into subtasks.
//! Each subtask has:
//! - A description of what to do
//! - Verification criteria (how to check it's done)
//! - A budget allocation

use serde::{Deserialize, Serialize};

use super::{Task, TaskId, VerificationCriteria};
use crate::budget::Budget;

/// A planned subtask before it becomes a full Task.
/// 
/// # Purpose
/// Represents the output of task splitting before budget allocation.
/// Once budgets are assigned, these become full `Task` objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    /// Description of what this subtask should accomplish
    pub description: String,
    
    /// How to verify this subtask is complete
    pub verification: VerificationCriteria,
    
    /// Relative weight for budget allocation (higher = more budget)
    pub weight: f64,
    
    /// Dependencies: IDs of subtasks that must complete first
    pub dependencies: Vec<usize>,
}

impl Subtask {
    /// Create a new subtask with no dependencies.
    /// 
    /// # Preconditions
    /// - `description` is non-empty
    /// - `weight > 0.0`
    pub fn new(
        description: impl Into<String>,
        verification: VerificationCriteria,
        weight: f64,
    ) -> Self {
        Self {
            description: description.into(),
            verification,
            weight: weight.max(0.01), // Ensure positive weight
            dependencies: Vec::new(),
        }
    }

    /// Add a dependency on another subtask (by index).
    pub fn with_dependency(mut self, index: usize) -> Self {
        self.dependencies.push(index);
        self
    }

    /// Add multiple dependencies.
    pub fn with_dependencies(mut self, indices: Vec<usize>) -> Self {
        self.dependencies.extend(indices);
        self
    }
}

/// A plan for splitting a task into subtasks.
/// 
/// # Invariants
/// - `subtasks` is non-empty
/// - All dependency indices are valid (< subtasks.len())
/// - No circular dependencies
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskPlan {
    /// The parent task ID
    parent_id: TaskId,
    
    /// The subtasks to create
    subtasks: Vec<Subtask>,
    
    /// Reasoning for the split
    reasoning: String,
}

impl SubtaskPlan {
    /// Create a new subtask plan.
    /// 
    /// # Preconditions
    /// - `subtasks` is non-empty
    /// - All dependency indices are valid
    /// 
    /// # Errors
    /// Returns `Err` if preconditions are violated.
    pub fn new(
        parent_id: TaskId,
        subtasks: Vec<Subtask>,
        reasoning: impl Into<String>,
    ) -> Result<Self, SubtaskPlanError> {
        if subtasks.is_empty() {
            return Err(SubtaskPlanError::EmptySubtasks);
        }

        // Validate dependency indices
        for (i, subtask) in subtasks.iter().enumerate() {
            for &dep in &subtask.dependencies {
                if dep >= subtasks.len() {
                    return Err(SubtaskPlanError::InvalidDependency { 
                        subtask_index: i, 
                        dependency_index: dep 
                    });
                }
                if dep == i {
                    return Err(SubtaskPlanError::SelfDependency { subtask_index: i });
                }
            }
        }

        // TODO: Check for circular dependencies (would need topological sort)

        Ok(Self {
            parent_id,
            subtasks,
            reasoning: reasoning.into(),
        })
    }

    /// Get the parent task ID.
    pub fn parent_id(&self) -> TaskId {
        self.parent_id
    }

    /// Get the subtasks.
    pub fn subtasks(&self) -> &[Subtask] {
        &self.subtasks
    }

    /// Get the reasoning for the split.
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    /// Convert this plan into actual Task objects with allocated budgets.
    /// 
    /// # Preconditions
    /// - `total_budget.remaining() > 0`
    /// 
    /// # Postconditions
    /// - Sum of subtask budgets <= total_budget
    /// - Each subtask has parent_id set to self.parent_id
    /// 
    /// # Budget Allocation
    /// Budget is allocated proportionally based on subtask weights.
    pub fn into_tasks(self, total_budget: &Budget) -> Result<Vec<Task>, SubtaskPlanError> {
        let total_weight: f64 = self.subtasks.iter().map(|s| s.weight).sum();
        
        if total_weight <= 0.0 {
            return Err(SubtaskPlanError::ZeroTotalWeight);
        }

        let available = total_budget.remaining_cents();
        
        self.subtasks
            .into_iter()
            .map(|subtask| {
                // Allocate budget proportionally
                let proportion = subtask.weight / total_weight;
                let allocated = ((available as f64) * proportion) as u64;
                
                let budget = Budget::new(allocated);
                
                Task::new_subtask(
                    subtask.description,
                    subtask.verification,
                    budget,
                    self.parent_id,
                ).map_err(|e| SubtaskPlanError::TaskCreation(e.to_string()))
            })
            .collect()
    }

    /// Get execution order respecting dependencies (topological sort).
    /// 
    /// # Returns
    /// Vector of subtask indices in valid execution order.
    /// 
    /// # Errors
    /// Returns `Err` if there are circular dependencies.
    pub fn execution_order(&self) -> Result<Vec<usize>, SubtaskPlanError> {
        let n = self.subtasks.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

        // Build adjacency list and compute in-degrees
        for (i, subtask) in self.subtasks.iter().enumerate() {
            for &dep in &subtask.dependencies {
                adj[dep].push(i);
                in_degree[i] += 1;
            }
        }

        // Kahn's algorithm for topological sort
        let mut queue: Vec<usize> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();
        
        let mut order = Vec::with_capacity(n);

        while let Some(node) = queue.pop() {
            order.push(node);
            for &next in &adj[node] {
                in_degree[next] -= 1;
                if in_degree[next] == 0 {
                    queue.push(next);
                }
            }
        }

        if order.len() != n {
            Err(SubtaskPlanError::CircularDependency)
        } else {
            Ok(order)
        }
    }

    /// Get execution waves for parallel processing.
    /// 
    /// Each wave contains indices of tasks that can be executed in parallel
    /// (all their dependencies are in previous waves).
    /// 
    /// # Returns
    /// Vector of waves, where each wave is a vector of subtask indices.
    /// 
    /// # Errors
    /// Returns `Err` if there are circular dependencies.
    pub fn execution_waves(&self) -> Result<Vec<Vec<usize>>, SubtaskPlanError> {
        let n = self.subtasks.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

        // Build adjacency list and compute in-degrees
        for (i, subtask) in self.subtasks.iter().enumerate() {
            for &dep in &subtask.dependencies {
                adj[dep].push(i);
                in_degree[i] += 1;
            }
        }

        let mut waves = Vec::new();
        let mut processed = 0;

        // Initial wave: all tasks with no dependencies
        let mut current_wave: Vec<usize> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();

        while !current_wave.is_empty() {
            processed += current_wave.len();
            waves.push(current_wave.clone());

            let mut next_wave = Vec::new();
            for &node in &current_wave {
                for &next in &adj[node] {
                    in_degree[next] -= 1;
                    if in_degree[next] == 0 {
                        next_wave.push(next);
                    }
                }
            }
            current_wave = next_wave;
        }

        if processed != n {
            Err(SubtaskPlanError::CircularDependency)
        } else {
            Ok(waves)
        }
    }
}

/// Errors in subtask plan creation or execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SubtaskPlanError {
    #[error("Subtask list cannot be empty")]
    EmptySubtasks,
    
    #[error("Subtask {subtask_index} has invalid dependency index {dependency_index}")]
    InvalidDependency { subtask_index: usize, dependency_index: usize },
    
    #[error("Subtask {subtask_index} depends on itself")]
    SelfDependency { subtask_index: usize },
    
    #[error("Circular dependency detected in subtask plan")]
    CircularDependency,
    
    #[error("Total weight of subtasks is zero")]
    ZeroTotalWeight,
    
    #[error("Failed to create task: {0}")]
    TaskCreation(String),
}


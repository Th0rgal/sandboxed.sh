//! Budget allocation strategies for subtasks.
//!
//! # Strategies
//! - Proportional: allocate based on estimated complexity
//! - Equal: split evenly among subtasks
//! - Priority: allocate more to critical subtasks

use crate::task::Subtask;

/// Strategy for allocating budget across subtasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationStrategy {
    /// Allocate proportionally based on subtask weights
    Proportional,
    
    /// Allocate equally among all subtasks
    Equal,
    
    /// Allocate based on priority (first subtasks get more)
    PriorityFirst,
}

/// Allocate budget across subtasks.
/// 
/// # Preconditions
/// - `subtasks` is non-empty
/// - `total_budget > 0`
/// 
/// # Postconditions
/// - `result.len() == subtasks.len()`
/// - `result.iter().sum() <= total_budget`
/// 
/// # Pure Function
/// This is a pure function with no side effects.
pub fn allocate_budget(
    subtasks: &[Subtask],
    total_budget: u64,
    strategy: AllocationStrategy,
) -> Vec<u64> {
    if subtasks.is_empty() {
        return Vec::new();
    }

    match strategy {
        AllocationStrategy::Proportional => allocate_proportional(subtasks, total_budget),
        AllocationStrategy::Equal => allocate_equal(subtasks, total_budget),
        AllocationStrategy::PriorityFirst => allocate_priority(subtasks, total_budget),
    }
}

/// Allocate proportionally based on weights.
/// 
/// # Invariant
/// Sum of allocations == total_budget (minus rounding)
fn allocate_proportional(subtasks: &[Subtask], total_budget: u64) -> Vec<u64> {
    let total_weight: f64 = subtasks.iter().map(|s| s.weight).sum();
    
    if total_weight <= 0.0 {
        return allocate_equal(subtasks, total_budget);
    }

    let mut allocations: Vec<u64> = subtasks
        .iter()
        .map(|s| {
            let proportion = s.weight / total_weight;
            ((total_budget as f64) * proportion).floor() as u64
        })
        .collect();

    // Distribute remainder to maintain total
    let allocated: u64 = allocations.iter().sum();
    let remainder = total_budget.saturating_sub(allocated);
    
    // Give remainder to the first subtask (or distribute evenly)
    if remainder > 0 && !allocations.is_empty() {
        allocations[0] += remainder;
    }

    allocations
}

/// Allocate equally among all subtasks.
fn allocate_equal(subtasks: &[Subtask], total_budget: u64) -> Vec<u64> {
    let n = subtasks.len() as u64;
    if n == 0 {
        return Vec::new();
    }

    let base = total_budget / n;
    let remainder = total_budget % n;

    let mut allocations = vec![base; subtasks.len()];
    
    // Distribute remainder
    for i in 0..(remainder as usize) {
        allocations[i] += 1;
    }

    allocations
}

/// Allocate with priority to earlier subtasks.
/// 
/// Earlier subtasks (lower index) get proportionally more.
/// Uses exponential decay: weight[i] = 2^(n-i)
fn allocate_priority(subtasks: &[Subtask], total_budget: u64) -> Vec<u64> {
    let n = subtasks.len();
    if n == 0 {
        return Vec::new();
    }

    // Compute exponential weights
    let weights: Vec<f64> = (0..n)
        .map(|i| 2.0_f64.powi((n - i - 1) as i32))
        .collect();
    
    let total_weight: f64 = weights.iter().sum();

    let mut allocations: Vec<u64> = weights
        .iter()
        .map(|w| ((total_budget as f64) * (w / total_weight)).floor() as u64)
        .collect();

    // Distribute remainder
    let allocated: u64 = allocations.iter().sum();
    let remainder = total_budget.saturating_sub(allocated);
    
    if remainder > 0 && !allocations.is_empty() {
        allocations[0] += remainder;
    }

    allocations
}

/// Estimate reasonable budget for a task based on complexity.
/// 
/// # Formula
/// Uses a heuristic based on complexity score:
/// - 0.0-0.2: ~10 cents (simple task)
/// - 0.2-0.5: ~50 cents (moderate task)
/// - 0.5-0.8: ~200 cents (complex task)
/// - 0.8-1.0: ~500 cents (very complex task)
/// 
/// # Pure Function
#[allow(dead_code)]
pub fn estimate_budget_for_complexity(complexity_score: f64) -> u64 {
    let clamped = complexity_score.clamp(0.0, 1.0);
    
    // Exponential scaling
    let base: f64 = 10.0; // Minimum 10 cents
    let max: f64 = 500.0; // Maximum 500 cents ($5)
    
    let budget = base * (max / base).powf(clamped);
    budget.ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::VerificationCriteria;

    fn make_subtasks(weights: &[f64]) -> Vec<Subtask> {
        weights
            .iter()
            .map(|&w| Subtask::new("test", VerificationCriteria::None, w))
            .collect()
    }

    #[test]
    fn test_proportional_allocation() {
        let subtasks = make_subtasks(&[1.0, 2.0, 1.0]);
        let allocs = allocate_budget(&subtasks, 100, AllocationStrategy::Proportional);
        
        assert_eq!(allocs.len(), 3);
        // Should be roughly 25, 50, 25
        assert!(allocs[1] > allocs[0]);
        assert_eq!(allocs.iter().sum::<u64>(), 100);
    }

    #[test]
    fn test_equal_allocation() {
        let subtasks = make_subtasks(&[1.0, 1.0, 1.0]);
        let allocs = allocate_budget(&subtasks, 99, AllocationStrategy::Equal);
        
        assert_eq!(allocs.len(), 3);
        assert_eq!(allocs.iter().sum::<u64>(), 99);
    }

    #[test]
    fn test_priority_allocation() {
        let subtasks = make_subtasks(&[1.0, 1.0, 1.0]);
        let allocs = allocate_budget(&subtasks, 100, AllocationStrategy::PriorityFirst);
        
        assert_eq!(allocs.len(), 3);
        assert!(allocs[0] > allocs[2]); // First gets more
        assert_eq!(allocs.iter().sum::<u64>(), 100);
    }

    #[test]
    fn test_complexity_budget_estimation() {
        assert!(estimate_budget_for_complexity(0.0) <= 15);
        assert!(estimate_budget_for_complexity(0.5) > 50);
        assert!(estimate_budget_for_complexity(1.0) >= 450);
    }
}


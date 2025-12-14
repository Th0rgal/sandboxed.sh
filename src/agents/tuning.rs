//! Empirical tuning parameters for agent heuristics.
//!
//! This module exists to support **trial-and-error calibration**:
//! we run tasks, compare predicted vs actual usage/cost, and update parameters.
//!
//! The core agent logic should remain correct even if tuning values are absent
//! (defaults apply).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::agents::leaf::ComplexityPromptVariant;

/// Top-level tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningParams {
    pub complexity: ComplexityTuning,
    pub model_selector: ModelSelectorTuning,
}

impl Default for TuningParams {
    fn default() -> Self {
        Self {
            complexity: ComplexityTuning::default(),
            model_selector: ModelSelectorTuning::default(),
        }
    }
}

impl TuningParams {
    /// Load tuning parameters from the workspace, if present.
    ///
    /// # Path
    /// `{workspace}/.open_agent/tuning.json`
    pub async fn load_from_workspace(workspace: &Path) -> Self {
        let path = workspace.join(".open_agent").join("tuning.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => serde_json::from_str::<TuningParams>(&s).unwrap_or_default(),
            Err(_) => TuningParams::default(),
        }
    }

    /// Save tuning parameters to the workspace.
    ///
    /// # Postcondition
    /// If successful, subsequent `load_from_workspace` returns an equivalent value.
    pub async fn save_to_workspace(&self, workspace: &Path) -> anyhow::Result<PathBuf> {
        let dir = workspace.join(".open_agent");
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join("tuning.json");
        let content = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&path, content).await?;
        Ok(path)
    }
}

/// Tuning parameters for ComplexityEstimator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityTuning {
    pub prompt_variant: ComplexityPromptVariant,
    pub split_threshold: f64,
    pub token_multiplier: f64,
}

impl Default for ComplexityTuning {
    fn default() -> Self {
        Self {
            prompt_variant: ComplexityPromptVariant::CalibratedV2,
            split_threshold: 0.60,
            token_multiplier: 1.00,
        }
    }
}

/// Tuning parameters for ModelSelector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSelectorTuning {
    /// Retry multiplier cost penalty for failures.
    pub retry_multiplier: f64,
    /// Token inefficiency scaling for weaker models.
    pub inefficiency_scale: f64,
    /// Cap for failure probability.
    pub max_failure_probability: f64,
}

impl Default for ModelSelectorTuning {
    fn default() -> Self {
        Self {
            retry_multiplier: 1.5,
            inefficiency_scale: 0.5,
            max_failure_probability: 0.9,
        }
    }
}



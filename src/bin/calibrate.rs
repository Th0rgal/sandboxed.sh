//! Calibration harness for Open Agent estimators.
//!
//! This binary runs trial tasks in a temporary workspace and measures:
//! - ComplexityEstimator: predicted tokens vs actual tokens used by TaskExecutor
//! - Split decision quality (against a small labeled set)
//!
//! The goal is *empirical tuning* by trial-and-error, while keeping the core
//! agent code maintainable and (eventually) provable.
//!
//! ## Usage
//!
//! ```bash
//! export OPENROUTER_API_KEY="..."
//! cargo run --release --bin calibrate -- --workspace /tmp/open_agent_calibration --model openai/gpt-4.1-mini
//! ```
//!
//! Notes:
//! - This will create and delete files under the given workspace.
//! - Costs real money. Keep the task set small.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use open_agent::agents::leaf::{ComplexityEstimator, ComplexityPromptVariant, TaskExecutor};
use open_agent::agents::{Agent, AgentContext};
use open_agent::budget::ModelPricing;
use open_agent::config::Config;
use open_agent::llm::OpenRouterClient;
use open_agent::task::{Task, VerificationCriteria};
use open_agent::tools::ToolRegistry;
use open_agent::agents::tuning::{TuningParams, ComplexityTuning};

#[derive(Debug, Clone)]
struct CalibTask {
    name: &'static str,
    prompt: &'static str,
    expected_should_split: bool,
}

fn parse_args() -> (PathBuf, String, bool) {
    let mut workspace = None::<PathBuf>;
    let mut model = None::<String>;
    let mut write_tuning = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--workspace" => workspace = args.next().map(PathBuf::from),
            "--model" => model = args.next(),
            "--write-tuning" => write_tuning = true,
            _ => {}
        }
    }

    let workspace = workspace.unwrap_or_else(|| PathBuf::from("./.open_agent_calibration"));
    let model = model.unwrap_or_else(|| "openai/gpt-4.1-mini".to_string());
    (workspace, model, write_tuning)
}

fn task_set() -> Vec<CalibTask> {
    vec![
        CalibTask {
            name: "hello_world",
            prompt: "Create a Python script called hello.py that prints 'Hello World'.",
            expected_should_split: false,
        },
        CalibTask {
            name: "calculator",
            prompt: "Create a Python script called calculator.py with add/subtract/multiply/divide functions and a small CLI menu.",
            expected_should_split: false,
        },
        CalibTask {
            name: "mini_project",
            prompt: "Create a tiny Python project with: (1) src/app.py that reads a name from argv and prints a greeting, (2) tests/test_app.py using pytest, (3) a pyproject.toml. Ensure 'python -m pytest' passes.",
            expected_should_split: true,
        },
    ]
}

#[derive(Debug, Clone)]
struct Score {
    mean_token_rel_error: f64,
    split_accuracy: f64,
}

impl Score {
    fn objective(&self) -> f64 {
        // Lower is better. Penalize wrong split decisions.
        self.mean_token_rel_error + (1.0 - self.split_accuracy) * 0.50
    }
}

async fn ensure_clean_dir(dir: &Path) -> anyhow::Result<()> {
    if dir.exists() {
        tokio::fs::remove_dir_all(dir).await?;
    }
    tokio::fs::create_dir_all(dir).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (workspace_root, exec_model, write_tuning) = parse_args();

    let api_key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| anyhow::anyhow!("OPENROUTER_API_KEY must be set for calibration"))?;

    let tasks = task_set();

    // Grid to try.
    let variants = [
        ComplexityPromptVariant::RubricV1,
        ComplexityPromptVariant::CalibratedV2,
    ];
    let split_thresholds = [0.55, 0.60, 0.65];
    let token_multipliers = [0.9, 1.0, 1.1, 1.2, 1.3];

    let llm: Arc<dyn open_agent::llm::LlmClient> = Arc::new(OpenRouterClient::new(api_key));
    let pricing = Arc::new(ModelPricing::new());

    let mut best = None::<(ComplexityPromptVariant, f64, f64, Score)>;

    for &variant in &variants {
        for &split_threshold in &split_thresholds {
            for &token_mult in &token_multipliers {
                let mut rel_errors = Vec::new();
                let mut correct_split = 0usize;

                for t in &tasks {
                    let ws = workspace_root.join(format!(
                        "{}_st{}_tm{}",
                        t.name,
                        (split_threshold * 100.0) as u64,
                        (token_mult * 100.0) as u64
                    ));
                    ensure_clean_dir(&ws).await?;

                    // Minimal config for context.
                    let cfg = Config::new("<redacted>".to_string(), exec_model.clone(), ws.clone());

                    let ctx = AgentContext::new(
                        cfg,
                        Arc::clone(&llm),
                        ToolRegistry::new(),
                        Arc::clone(&pricing),
                        ws.clone(),
                    );

                    // Build task with generous budget.
                    let budget = open_agent::budget::Budget::new(10_000); // $100 in cents
                    let mut task = Task::new(
                        t.prompt.to_string(),
                        VerificationCriteria::None,
                        budget,
                    )?;

                    // Run estimator (with candidate params).
                    let estimator = ComplexityEstimator::with_params(variant, split_threshold, token_mult);
                    let _ = estimator.execute(&mut task, &ctx).await;

                    let predicted_tokens = task.analysis().estimated_total_tokens.unwrap_or(2000);
                    let predicted_split = task.analysis().should_split.unwrap_or(false);
                    if predicted_split == t.expected_should_split {
                        correct_split += 1;
                    }

                    // Force execution model for comparability.
                    task.analysis_mut().selected_model = Some(exec_model.clone());

                    let executor = TaskExecutor::new();
                    let _exec_res = executor.execute(&mut task, &ctx).await;

                    let actual_tokens = task
                        .analysis()
                        .actual_usage
                        .as_ref()
                        .map(|u| u.total_tokens)
                        .unwrap_or(predicted_tokens);

                    let denom = (actual_tokens as f64).max(1.0);
                    let rel = ((predicted_tokens as f64) - (actual_tokens as f64)).abs() / denom;
                    rel_errors.push(rel);
                }

                let mean_token_rel_error = if rel_errors.is_empty() {
                    1.0
                } else {
                    rel_errors.iter().sum::<f64>() / (rel_errors.len() as f64)
                };

                let split_accuracy = (correct_split as f64) / (tasks.len() as f64);
                let score = Score {
                    mean_token_rel_error,
                    split_accuracy,
                };

                let candidate = (variant, split_threshold, token_mult, score.clone());
                let better = best
                    .as_ref()
                    .map(|(_, _, _, s)| score.objective() < s.objective())
                    .unwrap_or(true);

                if better {
                    best = Some(candidate);
                    eprintln!(
                        "New best: variant={:?} split={:.2} mult={:.2} token_err={:.3} split_acc={:.2} obj={:.3}",
                        variant,
                        split_threshold,
                        token_mult,
                        score.mean_token_rel_error,
                        score.split_accuracy,
                        score.objective()
                    );
                }
            }
        }
    }

    if let Some((variant, split_threshold, token_mult, score)) = best {
        println!("=== Recommended ComplexityEstimator Settings ===");
        println!("prompt_variant: {:?}", variant);
        println!("split_threshold: {:.2}", split_threshold);
        println!("token_multiplier: {:.2}", token_mult);
        println!("mean_token_rel_error: {:.3}", score.mean_token_rel_error);
        println!("split_accuracy: {:.2}", score.split_accuracy);

        if write_tuning {
            let mut tuning = TuningParams::default();
            tuning.complexity = ComplexityTuning {
                prompt_variant: variant,
                split_threshold,
                token_multiplier: token_mult,
            };
            let path = tuning.save_to_workspace(&workspace_root).await?;
            println!("Wrote tuning file to {}", path.to_string_lossy());
        }
    } else {
        println!("No calibration result produced.");
    }

    Ok(())
}



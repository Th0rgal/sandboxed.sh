//! Verification agent - validates task completion.
//!
//! # Verification Strategy (Hybrid)
//! 1. Try programmatic verification first (fast, deterministic)
//! 2. Fall back to LLM verification if needed
//!
//! # Programmatic Checks
//! - File exists
//! - Command succeeds
//! - Output matches pattern

use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentResult, AgentType, LeafAgent, LeafCapability,
};
use crate::llm::{ChatMessage, Role};
use crate::task::{ProgrammaticCheck, Task, VerificationCriteria, VerificationMethod, VerificationResult};

/// Agent that verifies task completion.
/// 
/// # Hybrid Verification
/// - Programmatic: Fast, deterministic, no cost
/// - LLM: Flexible, for subjective criteria
pub struct Verifier {
    id: AgentId,
}

impl Verifier {
    /// Create a new verifier.
    pub fn new() -> Self {
        Self { id: AgentId::new() }
    }

    /// Execute a programmatic check.
    /// 
    /// # Returns
    /// `Ok(true)` if check passes, `Ok(false)` if fails, `Err` on error.
    /// 
    /// # Note
    /// Paths in checks can be absolute or relative to working_dir.
    async fn run_programmatic_check(
        &self,
        check: &ProgrammaticCheck,
        working_dir: &Path,
    ) -> Result<bool, String> {
        match check {
            ProgrammaticCheck::FileExists { path } => {
                let full_path = Self::resolve_path(path, working_dir);
                Ok(full_path.exists())
            }

            ProgrammaticCheck::FileContains { path, content } => {
                let full_path = Self::resolve_path(path, working_dir);
                match tokio::fs::read_to_string(&full_path).await {
                    Ok(file_content) => Ok(file_content.contains(content)),
                    Err(_) => Ok(false),
                }
            }

            ProgrammaticCheck::CommandSucceeds { command } => {
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(working_dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .map_err(|e| e.to_string())?;
                
                Ok(output.success())
            }

            ProgrammaticCheck::CommandOutputMatches { command, pattern } => {
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(working_dir)
                    .output()
                    .await
                    .map_err(|e| e.to_string())?;
                
                let stdout = String::from_utf8_lossy(&output.stdout);
                let regex = regex::Regex::new(pattern).map_err(|e| e.to_string())?;
                Ok(regex.is_match(&stdout))
            }

            ProgrammaticCheck::DirectoryExists { path } => {
                let full_path = Self::resolve_path(path, working_dir);
                Ok(full_path.is_dir())
            }

            ProgrammaticCheck::FileMatchesRegex { path, pattern } => {
                let full_path = Self::resolve_path(path, working_dir);
                match tokio::fs::read_to_string(&full_path).await {
                    Ok(content) => {
                        let regex = regex::Regex::new(pattern).map_err(|e| e.to_string())?;
                        Ok(regex.is_match(&content))
                    }
                    Err(_) => Ok(false),
                }
            }

            ProgrammaticCheck::All(checks) => {
                for c in checks {
                    if !Box::pin(self.run_programmatic_check(c, working_dir)).await? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }

            ProgrammaticCheck::Any(checks) => {
                for c in checks {
                    if Box::pin(self.run_programmatic_check(c, working_dir)).await? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    /// Resolve a path - if absolute, use as-is; if relative, join with working_dir.
    fn resolve_path(path_str: &str, working_dir: &Path) -> std::path::PathBuf {
        let path = Path::new(path_str);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            working_dir.join(path)
        }
    }

    /// Verify using LLM.
    /// 
    /// # Parameters
    /// - `task`: The task that was executed
    /// - `success_criteria`: What success looks like
    /// - `ctx`: Agent context
    /// 
    /// # Returns
    /// VerificationResult with LLM's assessment
    async fn verify_with_llm(
        &self,
        task: &Task,
        success_criteria: &str,
        ctx: &AgentContext,
    ) -> VerificationResult {
        let prompt = format!(
            r#"You are verifying if a task was completed correctly.

Task: {}

Success Criteria: {}

Based on your assessment, respond with a JSON object:
{{
    "passed": true/false,
    "reasoning": "explanation of why the task passed or failed"
}}

Respond ONLY with the JSON object."#,
            task.description(),
            success_criteria
        );

        let messages = vec![
            ChatMessage {
                role: Role::System,
                content: Some("You are a precise task verifier. Respond only with JSON.".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::User,
                content: Some(prompt),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let model = "openai/gpt-4.1-mini";
        
        match ctx.llm.chat_completion(model, &messages, None).await {
            Ok(response) => {
                let content = response.content.unwrap_or_default();
                self.parse_llm_verification(&content, model)
            }
            Err(e) => {
                VerificationResult::fail(
                    format!("LLM verification failed: {}", e),
                    VerificationMethod::Llm { model: model.to_string() },
                    0,
                )
            }
        }
    }

    /// Parse LLM verification response.
    fn parse_llm_verification(&self, response: &str, model: &str) -> VerificationResult {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
            let passed = json["passed"].as_bool().unwrap_or(false);
            let reasoning = json["reasoning"]
                .as_str()
                .unwrap_or("No reasoning provided")
                .to_string();
            
            if passed {
                VerificationResult::pass(
                    reasoning,
                    VerificationMethod::Llm { model: model.to_string() },
                    1, // Minimal cost
                )
            } else {
                VerificationResult::fail(
                    reasoning,
                    VerificationMethod::Llm { model: model.to_string() },
                    1,
                )
            }
        } else {
            // Try to infer from text
            let passed = response.to_lowercase().contains("pass")
                || response.to_lowercase().contains("success")
                || response.to_lowercase().contains("completed");
            
            if passed {
                VerificationResult::pass(
                    response.to_string(),
                    VerificationMethod::Llm { model: model.to_string() },
                    1,
                )
            } else {
                VerificationResult::fail(
                    response.to_string(),
                    VerificationMethod::Llm { model: model.to_string() },
                    1,
                )
            }
        }
    }

    /// Run verification according to criteria.
    async fn verify(
        &self,
        task: &Task,
        ctx: &AgentContext,
    ) -> VerificationResult {
        match task.verification() {
            VerificationCriteria::None => {
                VerificationResult::pass(
                    "No verification required",
                    VerificationMethod::None,
                    0,
                )
            }

            VerificationCriteria::Programmatic(check) => {
                match self.run_programmatic_check(check, &ctx.working_dir).await {
                    Ok(true) => VerificationResult::pass(
                        "Programmatic check passed",
                        VerificationMethod::Programmatic,
                        0,
                    ),
                    Ok(false) => VerificationResult::fail(
                        "Programmatic check failed",
                        VerificationMethod::Programmatic,
                        0,
                    ),
                    Err(e) => VerificationResult::fail(
                        format!("Programmatic check error: {}", e),
                        VerificationMethod::Programmatic,
                        0,
                    ),
                }
            }

            VerificationCriteria::LlmBased { success_criteria } => {
                self.verify_with_llm(task, success_criteria, ctx).await
            }

            VerificationCriteria::Hybrid { programmatic, llm_fallback } => {
                // Try programmatic first
                match self.run_programmatic_check(programmatic, &ctx.working_dir).await {
                    Ok(true) => VerificationResult::pass(
                        "Programmatic check passed",
                        VerificationMethod::Programmatic,
                        0,
                    ),
                    Ok(false) => {
                        // Fall back to LLM
                        self.verify_with_llm(task, llm_fallback, ctx).await
                    }
                    Err(_) => {
                        // Error in programmatic, fall back to LLM
                        self.verify_with_llm(task, llm_fallback, ctx).await
                    }
                }
            }
        }
    }
}

impl Default for Verifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for Verifier {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Verifier
    }

    fn description(&self) -> &str {
        "Verifies task completion using programmatic checks and LLM fallback"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        let result = self.verify(task, ctx).await;
        
        if result.passed() {
            AgentResult::success(
                result.reasoning(),
                result.cost_cents(),
            )
            .with_data(serde_json::json!({
                "passed": true,
                "method": format!("{:?}", result.method()),
                "reasoning": result.reasoning(),
            }))
        } else {
            AgentResult::failure(
                result.reasoning(),
                result.cost_cents(),
            )
            .with_data(serde_json::json!({
                "passed": false,
                "method": format!("{:?}", result.method()),
                "reasoning": result.reasoning(),
            }))
        }
    }
}

impl LeafAgent for Verifier {
    fn capability(&self) -> LeafCapability {
        LeafCapability::Verification
    }
}


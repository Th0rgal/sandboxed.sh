//! Agent improvements module.
//!
//! This module contains improvements to make the agent more capable:
//! - Smart tool result handling with summarization
//! - Enhanced pivot prompts for stuck states
//! - Tool failure fallback suggestions
//! - Better error context
//! - Configurable thresholds

use std::collections::HashMap;

/// Thresholds for agent execution behavior.
/// These can be configured via environment variables.
#[derive(Debug, Clone)]
pub struct ExecutionThresholds {
    /// Number of times same tool call triggers warning (default: 2)
    pub loop_warning_threshold: u32,
    /// Number of times same tool call forces completion (default: 4)
    pub loop_force_complete_threshold: u32,
    /// Number of empty responses before warning (default: 2)
    pub empty_response_warning_threshold: u32,
    /// Number of empty responses before force completion (default: 4)
    pub empty_response_force_complete_threshold: u32,
    /// Number of failures in a tool category before suggesting pivot (default: 3)
    pub tool_failure_threshold: u32,
    /// Maximum characters for tool result before truncation (default: 15000)
    pub max_tool_result_chars: usize,
    /// Whether to summarize large tool results instead of just truncating
    pub summarize_large_results: bool,
}

impl Default for ExecutionThresholds {
    fn default() -> Self {
        Self {
            loop_warning_threshold: 2,
            loop_force_complete_threshold: 4,
            empty_response_warning_threshold: 2,
            empty_response_force_complete_threshold: 4,
            tool_failure_threshold: 3,
            max_tool_result_chars: 15000,
            summarize_large_results: true,
        }
    }
}

impl ExecutionThresholds {
    /// Load thresholds from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        let mut thresholds = Self::default();

        if let Ok(v) = std::env::var("LOOP_WARNING_THRESHOLD") {
            if let Ok(n) = v.parse() {
                thresholds.loop_warning_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("LOOP_FORCE_COMPLETE_THRESHOLD") {
            if let Ok(n) = v.parse() {
                thresholds.loop_force_complete_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("EMPTY_RESPONSE_WARNING_THRESHOLD") {
            if let Ok(n) = v.parse() {
                thresholds.empty_response_warning_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("EMPTY_RESPONSE_FORCE_COMPLETE_THRESHOLD") {
            if let Ok(n) = v.parse() {
                thresholds.empty_response_force_complete_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("TOOL_FAILURE_THRESHOLD") {
            if let Ok(n) = v.parse() {
                thresholds.tool_failure_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("MAX_TOOL_RESULT_CHARS") {
            if let Ok(n) = v.parse() {
                thresholds.max_tool_result_chars = n;
            }
        }
        if let Ok(v) = std::env::var("SUMMARIZE_LARGE_RESULTS") {
            thresholds.summarize_large_results = v.to_lowercase() == "true" || v == "1";
        }

        thresholds
    }
}

/// Tool categories for grouping related tools.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    StaticAnalysis,
    ShellCommand,
    Compilation,
    FileOps,
    Network,
    Git,
    Search,
    Browser,
    Other(String),
}

impl ToolCategory {
    /// Categorize a tool name into a broader approach category.
    pub fn from_tool_name(tool_name: &str) -> Self {
        match tool_name {
            // Browser automation (check first since "browser_" is more specific)
            name if name.starts_with("browser_") => Self::Browser,

            // Static analysis tools
            name if name.contains("slither")
                || name.contains("mythril")
                || name.contains("solhint")
                || name.contains("echidna") =>
            {
                Self::StaticAnalysis
            }

            // Code execution/compilation
            "run_command" => Self::ShellCommand,
            name if name.contains("compile") || name.contains("build") => Self::Compilation,

            // File operations
            "read_file" | "write_file" | "delete_file" | "list_directory" | "search_files" => {
                Self::FileOps
            }

            // Network/API calls
            name if name.contains("http")
                || name.contains("fetch")
                || name.contains("curl") =>
            {
                Self::Network
            }

            // Git operations
            name if name.contains("git") || name.contains("clone") => Self::Git,

            // Search operations
            name if name.contains("search") || name.contains("grep") => Self::Search,

            // Default: use the tool name itself
            _ => Self::Other(tool_name.to_string()),
        }
    }

    /// Get a string representation for display.
    pub fn as_str(&self) -> &str {
        match self {
            Self::StaticAnalysis => "static_analysis",
            Self::ShellCommand => "shell_command",
            Self::Compilation => "compilation",
            Self::FileOps => "file_ops",
            Self::Network => "network",
            Self::Git => "git",
            Self::Search => "search",
            Self::Browser => "browser",
            Self::Other(s) => s,
        }
    }

    /// Get suggested fallback tools for this category.
    pub fn fallback_suggestions(&self) -> Vec<&'static str> {
        match self {
            Self::Network => vec![
                "If fetch_url fails, try browser_navigate + browser_get_content",
                "If curl fails due to rate limiting, wait and retry or use a different approach",
                "For JavaScript-heavy sites, browser tools work better than fetch_url",
            ],
            Self::FileOps => vec![
                "If file not found, use list_directory to explore available files",
                "For binary files, use run_command with file/hexdump instead",
                "Check file permissions with run_command 'ls -la'",
            ],
            Self::ShellCommand => vec![
                "If command not found, try installing it with apt/pip first",
                "For long-running commands, consider breaking into smaller steps",
                "Check for syntax errors in complex commands",
            ],
            Self::Git => vec![
                "If clone fails, check if the repository URL is correct",
                "For authentication issues, the repo might be private",
                "Try using HTTPS instead of SSH or vice versa",
            ],
            Self::Search => vec![
                "If grep finds nothing, try broader patterns or different file types",
                "Use list_directory first to understand the directory structure",
                "Consider case-insensitive search (-i flag)",
            ],
            Self::Browser => vec![
                "If page doesn't load, check if the URL is correct",
                "For dynamic content, use browser_wait before browser_get_content",
                "Some sites block automation - try adding delays between actions",
            ],
            _ => vec![
                "Try a different approach to accomplish the same goal",
                "Check if there's a simpler way to get the information you need",
            ],
        }
    }
}

/// Generate a pivot prompt when the agent is stuck in a loop.
pub fn generate_pivot_prompt(
    tool_name: &str,
    repetition_count: u32,
    last_result: Option<&str>,
    remaining_attempts: u32,
) -> String {
    let result_hint = last_result
        .map(|r| {
            let preview: String = r.chars().take(200).collect();
            format!("\n\nLast result was: {}", preview)
        })
        .unwrap_or_default();

    let termination_warning = if remaining_attempts <= 1 {
        "The next repeated call WILL TERMINATE this task.".to_string()
    } else {
        format!(
            "You have {} more attempts before this task is terminated.",
            remaining_attempts
        )
    };

    let category = ToolCategory::from_tool_name(tool_name);
    let suggestions = category.fallback_suggestions();
    let suggestions_text = suggestions
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"[CRITICAL SYSTEM WARNING] You have repeated the EXACT SAME tool call {} times.
This is an infinite loop and you MUST stop.{}

DO NOT call the same command again. Instead:

**Category-specific suggestions for '{}' failures:**
{}

**General recovery options:**
1. If the path doesn't exist, use `list_directory` or `find` to locate the correct path
2. If you've gathered enough info, call complete_mission with your findings
3. Try a completely different approach to accomplish your goal
4. If you're fundamentally blocked, call complete_mission(blocked, reason)

{}
"#,
        repetition_count,
        result_hint,
        category.as_str(),
        suggestions_text,
        termination_warning
    )
}

/// Generate a prompt when a tool category has failed multiple times.
pub fn generate_tool_failure_prompt(category: &ToolCategory, failure_count: u32) -> String {
    let suggestions = category.fallback_suggestions();
    let suggestions_text = suggestions
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"[SYSTEM NOTE] The '{}' approach has failed {} times.

**Suggestions:**
{}

**Recovery options:**
1. Try a completely different tool/approach
2. Analyze what you DO have and produce partial results
3. Call complete_mission(blocked, reason) if fundamentally stuck

Don't keep trying the same failing approach."#,
        category.as_str(),
        failure_count,
        suggestions_text
    )
}

/// Smart truncation for tool results that preserves useful information.
pub fn smart_truncate_result(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }

    // Find a safe UTF-8 boundary
    let safe_end = content
        .char_indices()
        .take_while(|(i, _)| *i < max_chars)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);

    // Try to find a good break point (newline or space)
    let break_point = content[..safe_end]
        .rfind('\n')
        .or_else(|| content[..safe_end].rfind(' '))
        .unwrap_or(safe_end);

    let truncated = &content[..break_point];

    // Count what we're truncating
    let remaining_lines = content[break_point..].lines().count();
    let remaining_chars = content.len() - break_point;

    format!(
        "{}...\n\n[TRUNCATED: {} more characters, ~{} more lines. For large data, consider:\n\
         - Writing to a file and reading specific sections\n\
         - Using grep_search to find specific patterns\n\
         - Asking for a specific portion of the output]",
        truncated, remaining_chars, remaining_lines
    )
}

/// Detect potential blockers from tool output.
#[derive(Debug, Clone)]
pub struct BlockerDetection {
    pub is_blocker: bool,
    pub blocker_type: Option<BlockerType>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub enum BlockerType {
    /// Wrong project type (e.g., asked for Solidity but found C++)
    TypeMismatch { expected: String, found: String },
    /// Cannot access resource (e.g., repo doesn't exist)
    AccessDenied { resource: String, reason: String },
    /// Required tool not available
    MissingTool { tool: String },
    /// Source code not available
    NoSourceCode,
    /// Rate limited by external service
    RateLimited { service: String },
}

impl BlockerDetection {
    pub fn check_output(output: &str, task_description: &str) -> Self {
        let output_lower = output.to_lowercase();
        let task_lower = task_description.to_lowercase();

        // Check for type mismatches
        if let Some(mismatch) = Self::detect_type_mismatch(&output_lower, &task_lower) {
            return Self {
                is_blocker: true,
                blocker_type: Some(mismatch),
                message: Some(
                    "Project type doesn't match the requested analysis type".to_string(),
                ),
            };
        }

        // Check for access issues
        if output_lower.contains("404")
            || output_lower.contains("not found")
            || output_lower.contains("does not exist")
            || output_lower.contains("no such file")
        {
            return Self {
                is_blocker: false, // Not a hard blocker, might be wrong path
                blocker_type: Some(BlockerType::AccessDenied {
                    resource: "file/repository".to_string(),
                    reason: "not found".to_string(),
                }),
                message: Some("Resource not found - verify the path/URL".to_string()),
            };
        }

        // Check for rate limiting
        if output_lower.contains("rate limit")
            || output_lower.contains("too many requests")
            || output_lower.contains("429")
        {
            return Self {
                is_blocker: false, // Can retry later
                blocker_type: Some(BlockerType::RateLimited {
                    service: "external API".to_string(),
                }),
                message: Some("Rate limited - wait and retry or try alternative approach".to_string()),
            };
        }

        // Check for missing tools
        if output_lower.contains("command not found")
            || output_lower.contains("not recognized")
            || output_lower.contains("no such command")
        {
            return Self {
                is_blocker: false, // Can install the tool
                blocker_type: Some(BlockerType::MissingTool {
                    tool: "unknown".to_string(),
                }),
                message: Some("Required tool not installed - try installing it".to_string()),
            };
        }

        Self {
            is_blocker: false,
            blocker_type: None,
            message: None,
        }
    }

    fn detect_type_mismatch(output: &str, task: &str) -> Option<BlockerType> {
        // Solidity task but found different project type
        let is_solidity_task = task.contains("solidity")
            || task.contains("smart contract")
            || task.contains("ethereum")
            || task.contains("audit");

        if is_solidity_task {
            // Check if we found C++ project indicators
            if output.contains("configure.ac")
                || output.contains("makefile.am")
                || output.contains(".cpp")
                || output.contains(".hpp")
            {
                return Some(BlockerType::TypeMismatch {
                    expected: "Solidity/Smart Contract".to_string(),
                    found: "C++ project".to_string(),
                });
            }

            // Check if we found Rust project
            if output.contains("cargo.toml") {
                return Some(BlockerType::TypeMismatch {
                    expected: "Solidity/Smart Contract".to_string(),
                    found: "Rust project".to_string(),
                });
            }

            // Check if we found Go project
            if output.contains("go.mod") {
                return Some(BlockerType::TypeMismatch {
                    expected: "Solidity/Smart Contract".to_string(),
                    found: "Go project".to_string(),
                });
            }
        }

        None
    }
}

/// Track failed tool attempts by category for smarter pivot suggestions.
#[derive(Debug, Default)]
pub struct ToolFailureTracker {
    failures: HashMap<ToolCategory, Vec<FailedAttempt>>,
}

#[derive(Debug, Clone)]
pub struct FailedAttempt {
    pub tool_name: String,
    pub error: String,
    pub timestamp: std::time::Instant,
}

impl ToolFailureTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a tool failure.
    pub fn record_failure(&mut self, tool_name: &str, error: &str) {
        let category = ToolCategory::from_tool_name(tool_name);
        let attempt = FailedAttempt {
            tool_name: tool_name.to_string(),
            error: error.to_string(),
            timestamp: std::time::Instant::now(),
        };
        self.failures.entry(category).or_default().push(attempt);
    }

    /// Get failure count for a category.
    pub fn failure_count(&self, category: &ToolCategory) -> u32 {
        self.failures.get(category).map(|v| v.len() as u32).unwrap_or(0)
    }

    /// Check if a category has exceeded the failure threshold.
    pub fn should_pivot(&self, category: &ToolCategory, threshold: u32) -> bool {
        self.failure_count(category) >= threshold
    }

    /// Get suggested alternatives based on failure patterns.
    pub fn get_alternatives(&self, category: &ToolCategory) -> Vec<String> {
        let mut suggestions = Vec::new();

        // Add category-specific fallbacks
        for s in category.fallback_suggestions() {
            suggestions.push(s.to_string());
        }

        // Add cross-category suggestions based on what hasn't been tried
        if !self.failures.contains_key(&ToolCategory::Browser) && matches!(category, ToolCategory::Network) {
            suggestions.push("Try browser automation tools (browser_navigate, browser_get_content) for dynamic content".to_string());
        }

        if !self.failures.contains_key(&ToolCategory::Search) && matches!(category, ToolCategory::FileOps) {
            suggestions.push("Try grep_search to find the file you're looking for".to_string());
        }

        suggestions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_category_from_name() {
        assert_eq!(
            ToolCategory::from_tool_name("run_command"),
            ToolCategory::ShellCommand
        );
        assert_eq!(
            ToolCategory::from_tool_name("read_file"),
            ToolCategory::FileOps
        );
        assert_eq!(
            ToolCategory::from_tool_name("browser_navigate"),
            ToolCategory::Browser
        );
        assert_eq!(
            ToolCategory::from_tool_name("git_status"),
            ToolCategory::Git
        );
    }

    #[test]
    fn test_smart_truncate() {
        let long_content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n".repeat(100);
        let truncated = smart_truncate_result(&long_content, 100);
        assert!(truncated.contains("TRUNCATED"));
        assert!(truncated.len() < long_content.len());
    }

    #[test]
    fn test_blocker_detection_rate_limit() {
        let output = "Error 429: Too many requests";
        let detection = BlockerDetection::check_output(output, "search for files");
        assert!(!detection.is_blocker); // Rate limiting is recoverable
        assert!(matches!(
            detection.blocker_type,
            Some(BlockerType::RateLimited { .. })
        ));
    }

    #[test]
    fn test_type_mismatch_detection() {
        let output = "Found files: Cargo.toml, src/main.rs, README.md";
        let task = "Audit the Solidity smart contracts";
        let detection = BlockerDetection::check_output(output, task);
        assert!(detection.is_blocker);
        assert!(matches!(
            detection.blocker_type,
            Some(BlockerType::TypeMismatch { .. })
        ));
    }
}

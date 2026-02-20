//! Composite tools that combine multiple primitive tools.
//!
//! These higher-level operations capture common workflow patterns
//! and reduce the number of iterations needed for complex tasks.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

use super::Tool;

/// Analyze a codebase by listing structure and searching for key patterns.
pub struct AnalyzeCodebase;

#[async_trait]
impl Tool for AnalyzeCodebase {
    fn name(&self) -> &str {
        "analyze_codebase"
    }

    fn description(&self) -> &str {
        "Analyze a codebase: list directory structure, find key files (README, configs), and identify programming languages. Returns a structured overview."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the codebase directory (default: current directory)"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth to explore (default: 3)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let max_depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

        let target_path = if Path::new(path_str).is_absolute() {
            Path::new(path_str).to_path_buf()
        } else {
            working_dir.join(path_str)
        };

        if !target_path.exists() {
            return Ok(format!(
                "Error: Path does not exist: {}",
                target_path.display()
            ));
        }

        let mut result = String::new();
        result.push_str(&format!(
            "# Codebase Analysis: {}\n\n",
            target_path.display()
        ));

        // 1. Directory structure
        result.push_str("## Directory Structure\n```\n");
        if let Ok(output) = tokio::process::Command::new("find")
            .arg(&target_path)
            .arg("-maxdepth")
            .arg(max_depth.to_string())
            .arg("-type")
            .arg("d")
            .output()
            .await
        {
            let dirs = String::from_utf8_lossy(&output.stdout);
            for line in dirs.lines().take(50) {
                result.push_str(line);
                result.push('\n');
            }
        }
        result.push_str("```\n\n");

        // 2. Key files detection
        result.push_str("## Key Files Detected\n");

        let key_files = [
            ("README.md", "Documentation"),
            ("README", "Documentation"),
            ("package.json", "Node.js project"),
            ("Cargo.toml", "Rust project"),
            ("go.mod", "Go project"),
            ("requirements.txt", "Python project"),
            ("pyproject.toml", "Python project"),
            ("pom.xml", "Java/Maven project"),
            ("build.gradle", "Java/Gradle project"),
            ("Makefile", "Make-based build"),
            ("CMakeLists.txt", "CMake project"),
            ("docker-compose.yml", "Docker Compose"),
            ("Dockerfile", "Docker container"),
            (".gitignore", "Git repository"),
            ("hardhat.config.js", "Solidity/Hardhat"),
            ("foundry.toml", "Solidity/Foundry"),
            ("truffle-config.js", "Solidity/Truffle"),
        ];

        for (file, description) in key_files {
            let file_path = target_path.join(file);
            if file_path.exists() {
                result.push_str(&format!("- **{}**: {}\n", file, description));
            }
        }
        result.push('\n');

        // 3. Language detection by file extensions
        result.push_str("## Languages Detected\n");
        let extensions = [
            ("rs", "Rust"),
            ("py", "Python"),
            ("js", "JavaScript"),
            ("ts", "TypeScript"),
            ("jsx", "React JSX"),
            ("tsx", "React TSX"),
            ("go", "Go"),
            ("java", "Java"),
            ("sol", "Solidity"),
            ("c", "C"),
            ("cpp", "C++"),
            ("h", "C/C++ headers"),
            ("rb", "Ruby"),
            ("php", "PHP"),
            ("swift", "Swift"),
            ("kt", "Kotlin"),
        ];

        for (ext, lang) in extensions {
            let count_output = tokio::process::Command::new("find")
                .arg(&target_path)
                .arg("-name")
                .arg(format!("*.{}", ext))
                .arg("-type")
                .arg("f")
                .output()
                .await;

            if let Ok(output) = count_output {
                let files = String::from_utf8_lossy(&output.stdout);
                let count = files.lines().count();
                if count > 0 {
                    result.push_str(&format!("- **{}**: {} files\n", lang, count));
                }
            }
        }
        result.push('\n');

        // 4. README content preview
        let readme_path = target_path.join("README.md");
        if readme_path.exists() {
            result.push_str("## README Preview\n");
            if let Ok(content) = tokio::fs::read_to_string(&readme_path).await {
                let preview: String = content.chars().take(1000).collect();
                result.push_str("```markdown\n");
                result.push_str(&preview);
                if content.len() > 1000 {
                    result.push_str("\n... [truncated]");
                }
                result.push_str("\n```\n");
            }
        }

        Ok(result)
    }
}

/// Search for a pattern across multiple file types and contexts.
pub struct DeepSearch;

#[async_trait]
impl Tool for DeepSearch {
    fn name(&self) -> &str {
        "deep_search"
    }

    fn description(&self) -> &str {
        "Search for a pattern across the codebase with context. Searches file contents, file names, and provides surrounding context for matches."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The pattern to search for (regex supported)"
                },
                "path": {
                    "type": "string",
                    "description": "Path to search in (default: current directory)"
                },
                "file_types": {
                    "type": "string",
                    "description": "Comma-separated file extensions to search (e.g., 'rs,py,js'). Default: all files"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of context lines before and after match (default: 2)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))?;

        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let file_types = args.get("file_types").and_then(|v| v.as_str());

        let context_lines = args
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(2);

        let target_path = if Path::new(path_str).is_absolute() {
            Path::new(path_str).to_path_buf()
        } else {
            working_dir.join(path_str)
        };

        let mut result = String::new();
        result.push_str(&format!("# Search Results for: `{}`\n\n", pattern));

        // 1. Search file names
        result.push_str("## Matching File Names\n");
        let find_output = tokio::process::Command::new("find")
            .arg(&target_path)
            .arg("-name")
            .arg(format!("*{}*", pattern))
            .arg("-type")
            .arg("f")
            .output()
            .await;

        if let Ok(output) = find_output {
            let files = String::from_utf8_lossy(&output.stdout);
            if files.trim().is_empty() {
                result.push_str("_No matching file names found_\n\n");
            } else {
                for line in files.lines().take(20) {
                    result.push_str(&format!("- `{}`\n", line));
                }
                if files.lines().count() > 20 {
                    result.push_str(&format!("... and {} more\n", files.lines().count() - 20));
                }
                result.push('\n');
            }
        }

        // 2. Search file contents with grep
        result.push_str("## Matching Content\n");

        let mut grep_cmd = tokio::process::Command::new("grep");
        grep_cmd
            .arg("-rn")
            .arg(format!("-C{}", context_lines))
            .arg("--color=never");

        // Add file type filters
        if let Some(types) = file_types {
            for ext in types.split(',') {
                grep_cmd.arg("--include").arg(format!("*.{}", ext.trim()));
            }
        }

        grep_cmd.arg(pattern).arg(&target_path);

        match grep_cmd.output().await {
            Ok(output) => {
                let matches = String::from_utf8_lossy(&output.stdout);
                if matches.trim().is_empty() {
                    result.push_str("_No content matches found_\n");
                } else {
                    result.push_str("```\n");
                    // Limit output size by character count (not byte count)
                    let char_count = matches.chars().count();
                    let truncated: String = matches.chars().take(10000).collect();
                    result.push_str(&truncated);
                    if char_count > 10000 {
                        result.push_str("\n... [truncated, showing first 10000 chars]");
                    }
                    result.push_str("\n```\n");
                }
            }
            Err(e) => {
                result.push_str(&format!("Error running grep: {}\n", e));
            }
        }

        Ok(result)
    }
}

/// Prepare a project for development by checking dependencies and setup.
pub struct PrepareProject;

#[async_trait]
impl Tool for PrepareProject {
    fn name(&self) -> &str {
        "prepare_project"
    }

    fn description(&self) -> &str {
        "Prepare a project for development: detect project type, check for missing dependencies, and suggest setup steps. Returns a checklist."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the project directory (default: current directory)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let target_path = if Path::new(path_str).is_absolute() {
            Path::new(path_str).to_path_buf()
        } else {
            working_dir.join(path_str)
        };

        let mut result = String::new();
        result.push_str(&format!(
            "# Project Preparation: {}\n\n",
            target_path.display()
        ));

        // Detect project type and provide setup instructions
        let project_types = vec![
            (
                "Cargo.toml",
                "Rust",
                vec![
                    "Run `cargo build` to compile",
                    "Run `cargo test` to run tests",
                    "Run `cargo fmt` to format code",
                ],
            ),
            (
                "package.json",
                "Node.js",
                vec![
                    "Run `npm install` or `bun install` to install dependencies",
                    "Check `scripts` section for available commands",
                    "Run `npm test` or `bun test` to run tests",
                ],
            ),
            (
                "requirements.txt",
                "Python",
                vec![
                    "Run `pip install -r requirements.txt` to install dependencies",
                    "Consider using a virtual environment: `python -m venv venv`",
                    "Activate with `source venv/bin/activate` (Linux/Mac)",
                ],
            ),
            (
                "go.mod",
                "Go",
                vec![
                    "Run `go mod download` to fetch dependencies",
                    "Run `go build ./...` to compile",
                    "Run `go test ./...` to run tests",
                ],
            ),
            (
                "hardhat.config.js",
                "Solidity (Hardhat)",
                vec![
                    "Run `bun install` or `npm install` to install dependencies",
                    "Run `bunx hardhat compile` or `npx hardhat compile` to compile contracts",
                    "Run `bunx hardhat test` or `npx hardhat test` to run tests",
                ],
            ),
            (
                "foundry.toml",
                "Solidity (Foundry)",
                vec![
                    "Run `forge install` to install dependencies",
                    "Run `forge build` to compile contracts",
                    "Run `forge test` to run tests",
                ],
            ),
        ];

        let mut detected = false;
        for (file, project_type, steps) in project_types {
            if target_path.join(file).exists() {
                detected = true;
                result.push_str(&format!("## {} Project Detected\n\n", project_type));
                result.push_str("### Setup Steps\n");
                for (i, step) in steps.iter().enumerate() {
                    result.push_str(&format!("{}. {}\n", i + 1, step));
                }
                result.push('\n');
            }
        }

        if !detected {
            result.push_str("## Unknown Project Type\n\n");
            result.push_str("Could not detect a known project type. Look for:\n");
            result.push_str("- README.md for setup instructions\n");
            result.push_str("- Makefile for build commands\n");
            result.push_str("- Any configuration files\n");
        }

        // Check for common tools
        result.push_str("## Environment Check\n\n");
        let tools_to_check = [
            "git", "node", "bun", "npm", "cargo", "python3", "go", "forge",
        ];

        for tool in tools_to_check {
            let output = tokio::process::Command::new("which")
                .arg(tool)
                .output()
                .await;

            if let Ok(out) = output {
                if out.status.success() {
                    result.push_str(&format!("- ✓ `{}` is installed\n", tool));
                } else {
                    result.push_str(&format!("- ✗ `{}` not found\n", tool));
                }
            }
        }

        Ok(result)
    }
}

/// Debug an error by analyzing error message, searching for similar issues.
pub struct DebugError;

#[async_trait]
impl Tool for DebugError {
    fn name(&self) -> &str {
        "debug_error"
    }

    fn description(&self) -> &str {
        "Debug an error: parse the error message, search for the error pattern in the codebase, and suggest potential fixes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "error_message": {
                    "type": "string",
                    "description": "The full error message or stack trace"
                },
                "path": {
                    "type": "string",
                    "description": "Path to the codebase to search for related code (default: current directory)"
                }
            },
            "required": ["error_message"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let error_message = args
            .get("error_message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("error_message is required"))?;

        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let target_path = if Path::new(path_str).is_absolute() {
            Path::new(path_str).to_path_buf()
        } else {
            working_dir.join(path_str)
        };

        let mut result = String::new();
        result.push_str("# Error Analysis\n\n");

        // 1. Parse the error
        result.push_str("## Error Message\n```\n");
        result.push_str(error_message);
        result.push_str("\n```\n\n");

        // 2. Extract key identifiers from the error
        result.push_str("## Key Identifiers Extracted\n");

        // Look for file:line patterns
        let file_line_pattern = regex::Regex::new(r"([a-zA-Z0-9_/\.\-]+)\:(\d+)").ok();
        if let Some(re) = file_line_pattern {
            for cap in re.captures_iter(error_message).take(5) {
                result.push_str(&format!(
                    "- File reference: `{}` at line `{}`\n",
                    &cap[1], &cap[2]
                ));
            }
        }

        // Look for function/method names
        let func_pattern =
            regex::Regex::new(r"(?:fn|function|def|func)\s+([a-zA-Z_][a-zA-Z0-9_]*)").ok();
        if let Some(re) = func_pattern {
            for cap in re.captures_iter(error_message).take(5) {
                result.push_str(&format!("- Function: `{}`\n", &cap[1]));
            }
        }

        // Look for common error types
        let error_types = [
            ("undefined", "Undefined variable or function"),
            ("not found", "Missing file, module, or dependency"),
            ("permission denied", "File permission issue"),
            ("timeout", "Operation took too long"),
            ("connection refused", "Network/service connection issue"),
            ("out of memory", "Memory exhaustion"),
            ("null pointer", "Null reference error"),
            ("type mismatch", "Type compatibility issue"),
            ("syntax error", "Code syntax problem"),
        ];

        result.push_str("\n## Error Type Analysis\n");
        let error_lower = error_message.to_lowercase();
        let mut found_type = false;
        for (pattern, description) in error_types {
            if error_lower.contains(pattern) {
                result.push_str(&format!("- Likely issue: **{}**\n", description));
                found_type = true;
            }
        }
        if !found_type {
            result.push_str("- Could not determine specific error type\n");
        }

        // 3. Search for related code in the codebase
        result.push_str("\n## Related Code Search\n");

        // Extract likely search terms (words that look like identifiers)
        let identifier_pattern = regex::Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]{2,}").ok();
        if let Some(re) = identifier_pattern {
            let identifiers: Vec<_> = re
                .find_iter(error_message)
                .map(|m| m.as_str())
                .filter(|s| {
                    ![
                        "the", "and", "error", "warning", "info", "debug", "trace", "at", "in",
                        "from", "to",
                    ]
                    .contains(s)
                })
                .take(3)
                .collect();

            for identifier in identifiers {
                let grep_output = tokio::process::Command::new("grep")
                    .arg("-rn")
                    .arg("-l")
                    .arg(identifier)
                    .arg(&target_path)
                    .output()
                    .await;

                if let Ok(output) = grep_output {
                    let files = String::from_utf8_lossy(&output.stdout);
                    if !files.trim().is_empty() {
                        result.push_str(&format!("\n### Files containing `{}`\n", identifier));
                        for line in files.lines().take(5) {
                            result.push_str(&format!("- `{}`\n", line));
                        }
                    }
                }
            }
        }

        // 4. Suggest potential fixes
        result.push_str("\n## Suggested Actions\n");
        result.push_str("1. Check the specific file and line number mentioned in the error\n");
        result.push_str("2. Verify all dependencies are installed correctly\n");
        result.push_str("3. Check for recent changes that might have introduced the error\n");
        result.push_str("4. Search for similar errors in the project's issue tracker\n");

        Ok(result)
    }
}

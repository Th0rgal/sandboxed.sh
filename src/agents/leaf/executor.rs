//! Task executor agent - the main worker that uses tools.
//!
//! This is a refactored version of the original agent loop,
//! now as a leaf agent in the hierarchical tree.
//!
//! ## Tool Reuse
//! The executor automatically discovers and lists reusable tools in `/root/tools/`
//! at the start of each execution, injecting their documentation into the system prompt.

use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentResult, AgentType, LeafAgent, LeafCapability,
};
use crate::api::control::{AgentEvent, ControlRunState};
use crate::budget::ExecutionSignals;
use crate::llm::{ChatMessage, MessageContent, Role, ToolCall};
use crate::task::{Task, TokenUsageSummary};
use crate::tools::ToolRegistry;

/// Result from running the agent loop with detailed signals for failure analysis.
#[derive(Debug)]
pub struct ExecutionLoopResult {
    /// Final output text
    pub output: String,
    /// Total cost in cents
    pub cost_cents: u64,
    /// Log of tool calls made
    pub tool_log: Vec<String>,
    /// Token usage summary
    pub usage: Option<TokenUsageSummary>,
    /// Detailed signals for failure analysis
    pub signals: ExecutionSignals,
    /// Whether execution succeeded
    pub success: bool,
}

/// Agent that executes tasks using tools.
/// 
/// # Algorithm
/// 1. Build system prompt with available tools
/// 2. Call LLM with task description
/// 3. If LLM requests tool call: execute, feed back result
/// 4. Repeat until LLM produces final response or max iterations
/// 
/// # Budget Management
/// - Tracks token usage and costs
/// - Stops if budget is exhausted
pub struct TaskExecutor {
    id: AgentId,
}

impl TaskExecutor {
    /// Create a new task executor.
    pub fn new() -> Self {
        Self { id: AgentId::new() }
    }

    /// Discover reusable tools in /root/tools/.
    /// 
    /// Scans the directory for README.md files and tool scripts,
    /// building an inventory of available reusable tools.
    async fn discover_reusable_tools(&self, working_dir: &str) -> String {
        let tools_dir = if working_dir.starts_with('/') {
            // Find the root (e.g., /root or the working directory's ancestor)
            if working_dir.contains("/root") {
                "/root/tools".to_string()
            } else {
                format!("{}/tools", working_dir)
            }
        } else {
            "/root/tools".to_string()
        };

        let tools_path = Path::new(&tools_dir);
        if !tools_path.exists() {
            return String::new();
        }

        let mut tool_inventory = Vec::new();

        // Try to read the directory
        if let Ok(entries) = std::fs::read_dir(&tools_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                
                // Skip hidden files
                if name.starts_with('.') {
                    continue;
                }

                if path.is_dir() {
                    // Check for README.md in the tool folder
                    let readme_path = path.join("README.md");
                    if readme_path.exists() {
                        if let Ok(content) = std::fs::read_to_string(&readme_path) {
                            // Extract first paragraph or first 500 chars as description
                            let description = content
                                .lines()
                                .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
                                .take(3)
                                .collect::<Vec<_>>()
                                .join(" ")
                                .chars()
                                .take(300)
                                .collect::<String>();
                            
                            tool_inventory.push(format!("- **{}**: {}", name, description));
                        } else {
                            tool_inventory.push(format!("- **{}**: (tool folder, check README.md for details)", name));
                        }
                    } else {
                        // List scripts in the folder
                        let scripts: Vec<_> = std::fs::read_dir(&path)
                            .ok()
                            .map(|entries| {
                                entries
                                    .flatten()
                                    .filter(|e| {
                                        let name = e.file_name().to_string_lossy().to_string();
                                        name.ends_with(".sh") || name.ends_with(".py") || name.ends_with(".rs")
                                    })
                                    .map(|e| e.file_name().to_string_lossy().to_string())
                                    .collect()
                            })
                            .unwrap_or_default();
                        
                        if !scripts.is_empty() {
                            tool_inventory.push(format!("- **{}**: scripts: {}", name, scripts.join(", ")));
                        }
                    }
                } else if name.ends_with(".sh") || name.ends_with(".py") || name.ends_with(".rs") {
                    // Standalone script
                    tool_inventory.push(format!("- **{}**: standalone script", name));
                }
            }
        }

        // Also check for top-level README
        let main_readme = Path::new(&tools_dir).join("README.md");
        if main_readme.exists() {
            if let Ok(content) = std::fs::read_to_string(&main_readme) {
                // Return the entire tools README as inventory
                return format!(
                    "\n## Available Reusable Tools (from /root/tools/)\n\n{}\n\n### Tool Inventory\n{}",
                    content.chars().take(1000).collect::<String>(),
                    tool_inventory.join("\n")
                );
            }
        }

        if tool_inventory.is_empty() {
            String::new()
        } else {
            format!(
                "\n## Available Reusable Tools (from /root/tools/)\n\nThese tools have been created in previous runs. Check their documentation before recreating them!\n\n{}\n",
                tool_inventory.join("\n")
            )
        }
    }

    /// Build the system prompt for task execution.
    fn build_system_prompt(&self, working_dir: &str, tools: &ToolRegistry, reusable_tools: &str) -> String {
        let tool_descriptions = tools
            .list_tools()
            .iter()
            .map(|t| format!("- **{}**: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"You are an autonomous task executor with **full system access** on a Linux server.
You can read/write any file, execute any command, install any software, and search anywhere.
Your default working directory is: {working_dir}

## FIRST THING TO DO
**Always start by checking `/root/context/`** — Users deposit files, samples, or context there for you to analyze.
Run `ls -la /root/context/` to see what's available before doing anything else.

## Directory Structure (KEEP IT ORGANIZED!)
```
/root/
├── context/           # 📥 INPUT: User-provided files (READ-ONLY, don't write here)
│   └── [task files]   #    Check this FIRST for any files to analyze
├── work/              # 🔨 WORKSPACE: Your main working area
│   └── [task-name]/   #    Create a subfolder for each distinct task/analysis
│       ├── output/    #    Final outputs, reports, results
│       ├── temp/      #    Intermediate/temporary files
│       └── notes.md   #    Task notes, findings, decisions
└── tools/             # 🛠️ TOOLBOX: Reusable scripts & utilities
    ├── README.md      #    Document each tool's purpose & usage
    └── [tool-name]/   #    One folder per tool/script
```

**Keep it clean:**
- Create a descriptive subfolder in `/root/work/` for each task (e.g., `/root/work/analyze-myapp/`)
- Don't dump files directly in `/root/` or `/root/work/`
- Clean up temp files when done
- Document your tools with README files
{reusable_tools}
## Available Tools (API)
{tool_descriptions}

## Philosophy: BE PROACTIVE
You are encouraged to **experiment and try things**:
- Install ANY software you need without asking (decompilers, debuggers, analyzers, language runtimes)
- **IMPORTANT: Before creating new helper scripts, check /root/tools/ for existing reusable tools!**
- Create helper scripts and save them in /root/tools/ for reuse
- Write documentation for your tools so future runs can use them
- If a tool doesn't exist, build it or find an alternative
- Don't give up easily - try multiple approaches before declaring failure
- **You have unlimited freedom** to install packages, create files, run experiments

## Workflow for Unknown Files
When encountering files you need to analyze:
1. **IDENTIFY** — Run `file <filename>` to detect the file type
2. **CHECK EXISTING TOOLS** — Look in `/root/tools/` for reusable scripts for this file type
3. **INSTALL TOOLS** — Install appropriate tools for that file type:
   - **Java/JAR/Class**: `apt install -y default-jdk jadx` (jadx is a Java decompiler)
   - **Android APK**: `apt install -y jadx apktool`
   - **Native binaries**: `apt install -y ghidra radare2 binutils`
   - **Python .pyc**: `pip install uncompyle6 decompyle3`
   - **.NET**: `apt install -y mono-complete; pip install dnfile`
   - **Archives**: `apt install -y p7zip-full unzip`
4. **ANALYZE** — Use the installed tools to examine/decompile the file
5. **Handle obfuscation** — If code is obfuscated:
   - Java: Try `java-deobfuscator` or `cfr` with string decryption
   - Look for string encryption patterns, rename variables to understand flow
   - Run the code dynamically if static analysis fails
6. **Document findings** — Save analysis notes to your task folder in `/root/work/`

## Java Reverse Engineering (Common)
For .jar or .class files:
```bash
# Install tools
apt install -y default-jdk
pip install jadx || apt install -y jadx

# Create organized workspace
mkdir -p /root/work/java-analysis/{{output,temp}}

# Decompile
jadx -d /root/work/java-analysis/output/decompiled <jar_file>

# If obfuscated, try:
# 1. CFR decompiler (handles some obfuscation)
wget -O /root/tools/cfr.jar https://github.com/leibnitz27/cfr/releases/download/0.152/cfr-0.152.jar
java -jar /root/tools/cfr.jar <jar_file> --outputdir /root/work/java-analysis/output/cfr

# 2. For string encryption, analyze decryption routines
# 3. Dynamic analysis: add debug logging and run
```

## Rules
1. **Act, don't just describe** — Use tools to accomplish tasks, don't just explain what to do
2. **Check /root/context/ first** — This is where users put files for you
3. **Check /root/tools/ for existing tools** — Reuse scripts before creating new ones
4. **Stay organized** — Create task-specific folders in /root/work/, keep /root/context/ read-only
5. **Identify before analyzing** — Always run `file` on unknown files
6. **Install what you need** — Don't ask permission, just `apt install` or `pip install`
7. **Handle obfuscation** — If decompiled code looks obfuscated, install deobfuscators and try them
8. **Create reusable tools** — Save useful scripts to /root/tools/ with README
9. **Verify your work** — Test, run, check outputs when possible
10. **Iterate** — If first approach fails, try alternatives before giving up

## Response
When task is complete, provide a clear summary of:
- What you did (approach taken)
- Files created/modified (with full paths, organized in /root/work/[task]/)
- Tools installed (for future reference)
- Tools reused from /root/tools/ (if any)
- How to verify the result
- Any NEW reusable scripts saved to /root/tools/"#,
            working_dir = working_dir,
            tool_descriptions = tool_descriptions,
            reusable_tools = reusable_tools
        )
    }

    /// Execute a single tool call.
    async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
        ctx: &AgentContext,
    ) -> anyhow::Result<String> {
        let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
            .unwrap_or(serde_json::Value::Null);

        ctx.tools
            .execute(&tool_call.function.name, args, &ctx.working_dir)
            .await
    }

    /// Run the agent loop for a task.
    async fn run_loop(
        &self,
        task: &Task,
        model: &str,
        ctx: &AgentContext,
    ) -> ExecutionLoopResult {
        let mut total_cost_cents = 0u64;
        let mut tool_log = Vec::new();
        let mut usage: Option<TokenUsageSummary> = None;
        
        // Track execution signals for failure analysis
        let mut successful_tool_calls = 0u32;
        let mut failed_tool_calls = 0u32;
        let mut files_modified = false;
        let mut last_tool_calls: Vec<String> = Vec::new();
        let mut repetitive_actions = false;
        let mut has_error_messages = false;
        let mut iterations_completed = 0u32;

        // If we can fetch pricing, compute real costs from token usage.
        let pricing = ctx.pricing.get_pricing(model).await;

        // Discover reusable tools from /root/tools/ (or working_dir/tools)
        let reusable_tools = self.discover_reusable_tools(&ctx.working_dir_str()).await;
        if !reusable_tools.is_empty() {
            tracing::info!("Discovered reusable tools inventory");
        }

        // Build initial messages with reusable tools info
        let system_prompt = self.build_system_prompt(&ctx.working_dir_str(), &ctx.tools, &reusable_tools);
        let mut messages = vec![
            ChatMessage::new(Role::System, system_prompt),
            ChatMessage::new(Role::User, task.description().to_string()),
        ];

        // Get tool schemas
        let tool_schemas = ctx.tools.get_tool_schemas();

        // Agent loop
        for iteration in 0..ctx.max_iterations {
            iterations_completed = iteration as u32 + 1;
            tracing::debug!("TaskExecutor iteration {}", iteration + 1);

            // Cooperative cancellation (control session).
            if let Some(token) = &ctx.cancel_token {
                if token.is_cancelled() {
                    has_error_messages = true;
                    let signals = ExecutionSignals {
                        iterations: iterations_completed,
                        max_iterations: ctx.max_iterations as u32,
                        successful_tool_calls,
                        failed_tool_calls,
                        files_modified,
                        repetitive_actions,
                        has_error_messages,
                        partial_progress: files_modified || successful_tool_calls > 0,
                        cost_spent_cents: total_cost_cents,
                        budget_total_cents: task.budget().total_cents(),
                        final_output: "Cancelled".to_string(),
                        model_used: model.to_string(),
                    };
                    return ExecutionLoopResult {
                        output: "Cancelled".to_string(),
                        cost_cents: total_cost_cents,
                        tool_log,
                        usage,
                        signals,
                        success: false,
                    };
                }
            }

            // Check budget
            let remaining = task.budget().remaining_cents();
            if remaining == 0 && total_cost_cents > 0 {
                let signals = ExecutionSignals {
                    iterations: iterations_completed,
                    max_iterations: ctx.max_iterations as u32,
                    successful_tool_calls,
                    failed_tool_calls,
                    files_modified,
                    repetitive_actions,
                    has_error_messages,
                    partial_progress: files_modified || successful_tool_calls > 0,
                    cost_spent_cents: total_cost_cents,
                    budget_total_cents: task.budget().total_cents(),
                    final_output: "Budget exhausted before task completion".to_string(),
                    model_used: model.to_string(),
                };
                return ExecutionLoopResult {
                    output: "Budget exhausted before task completion".to_string(),
                    cost_cents: total_cost_cents,
                    tool_log,
                    usage,
                    signals,
                    success: false,
                };
            }

            // Call LLM
            let response = match ctx.llm.chat_completion(model, &messages, Some(&tool_schemas)).await {
                Ok(r) => r,
                Err(e) => {
                    has_error_messages = true;
                    let error_msg = format!("LLM error: {}", e);
                    let signals = ExecutionSignals {
                        iterations: iterations_completed,
                        max_iterations: ctx.max_iterations as u32,
                        successful_tool_calls,
                        failed_tool_calls,
                        files_modified,
                        repetitive_actions,
                        has_error_messages,
                        partial_progress: files_modified || successful_tool_calls > 0,
                        cost_spent_cents: total_cost_cents,
                        budget_total_cents: task.budget().total_cents(),
                        final_output: error_msg.clone(),
                        model_used: model.to_string(),
                    };
                    return ExecutionLoopResult {
                        output: error_msg,
                        cost_cents: total_cost_cents,
                        tool_log,
                        usage,
                        signals,
                        success: false,
                    };
                }
            };

            // Emit thinking event if there's content (agent reasoning)
            if let Some(ref content) = response.content {
                if !content.is_empty() {
                    if let Some(events) = &ctx.control_events {
                        let _ = events.send(AgentEvent::Thinking {
                            content: content.clone(),
                            done: response.tool_calls.is_none(),
                        });
                    }
                }
            }

            // Cost + usage accounting.
            if let Some(u) = &response.usage {
                let u_sum = TokenUsageSummary::new(u.prompt_tokens, u.completion_tokens);
                usage = Some(match &usage {
                    Some(acc) => acc.add(&u_sum),
                    None => u_sum,
                });

                if let Some(p) = &pricing {
                    total_cost_cents = total_cost_cents.saturating_add(
                        p.calculate_cost_cents(u.prompt_tokens, u.completion_tokens),
                    );
                } else {
                    // Fallback heuristic when usage exists but pricing doesn't.
                    total_cost_cents = total_cost_cents.saturating_add(2);
                }
            } else {
                // Legacy heuristic if upstream doesn't return usage.
                total_cost_cents = total_cost_cents.saturating_add(2);
            }

            // Check for tool calls
            if let Some(tool_calls) = &response.tool_calls {
                if !tool_calls.is_empty() {
                    // Add assistant message with tool calls
                    messages.push(ChatMessage {
                        role: Role::Assistant,
                        content: response.content.clone().map(MessageContent::text),
                        tool_calls: Some(tool_calls.clone()),
                        tool_call_id: None,
                    });

                    // Check for repetitive actions
                    let current_calls: Vec<String> = tool_calls
                        .iter()
                        .map(|tc| format!("{}:{}", tc.function.name, tc.function.arguments))
                        .collect();
                    if current_calls == last_tool_calls && !current_calls.is_empty() {
                        repetitive_actions = true;
                    }
                    last_tool_calls = current_calls;

                    // Execute each tool call
                    for tool_call in tool_calls {
                        let tool_name = tool_call.function.name.clone();
                        let args_json: serde_json::Value =
                            serde_json::from_str(&tool_call.function.arguments)
                                .unwrap_or(serde_json::Value::Null);

                        // For interactive frontend tools, register the tool_call_id before notifying the UI,
                        // so a fast tool_result POST can't race ahead of registration.
                        let mut pending_frontend_rx: Option<tokio::sync::oneshot::Receiver<serde_json::Value>> = None;
                        if tool_name == "ui_optionList" {
                            if let Some(hub) = &ctx.frontend_tool_hub {
                                pending_frontend_rx = Some(hub.register(tool_call.id.clone()).await);
                            }
                        }

                        if let Some(events) = &ctx.control_events {
                            let _ = events.send(AgentEvent::ToolCall {
                                tool_call_id: tool_call.id.clone(),
                                name: tool_name.clone(),
                                args: args_json.clone(),
                            });
                        }

                        tool_log.push(format!(
                            "Tool: {} Args: {}",
                            tool_call.function.name,
                            tool_call.function.arguments
                        ));

                        // Track file modifications
                        if tool_name == "write_file" || tool_name == "delete_file" {
                            files_modified = true;
                        }

                        // UI tools are handled by the frontend. We emit events and (optionally) wait for a user result.
                        let (tool_message_content, tool_result_json): (String, serde_json::Value) =
                            if tool_name.starts_with("ui_") {
                                // Interactive tool: wait for frontend to POST result.
                                if tool_name == "ui_optionList" {
                                    if let Some(rx) = pending_frontend_rx {
                                        if let (Some(status), Some(events)) = (&ctx.control_status, &ctx.control_events) {
                                            let mut s = status.write().await;
                                            s.state = ControlRunState::WaitingForTool;
                                            let q = s.queue_len;
                                            drop(s);
                                            let _ = events.send(AgentEvent::Status { state: ControlRunState::WaitingForTool, queue_len: q });
                                        }

                                        let recv = if let Some(token) = &ctx.cancel_token {
                                            tokio::select! {
                                                v = rx => v,
                                                _ = token.cancelled() => {
                                                    has_error_messages = true;
                                                    let signals = ExecutionSignals {
                                                        iterations: iterations_completed,
                                                        max_iterations: ctx.max_iterations as u32,
                                                        successful_tool_calls,
                                                        failed_tool_calls,
                                                        files_modified,
                                                        repetitive_actions,
                                                        has_error_messages,
                                                        partial_progress: files_modified || successful_tool_calls > 0,
                                                        cost_spent_cents: total_cost_cents,
                                                        budget_total_cents: task.budget().total_cents(),
                                                        final_output: "Cancelled".to_string(),
                                                        model_used: model.to_string(),
                                                    };
                                                    return ExecutionLoopResult {
                                                        output: "Cancelled".to_string(),
                                                        cost_cents: total_cost_cents,
                                                        tool_log,
                                                        usage,
                                                        signals,
                                                        success: false,
                                                    };
                                                }
                                            }
                                        } else {
                                            rx.await
                                        };

                                        match recv {
                                            Ok(v) => {
                                                successful_tool_calls += 1;
                                                let msg = serde_json::to_string(&v)
                                                    .unwrap_or_else(|_| v.to_string());
                                                if let (Some(status), Some(events)) = (&ctx.control_status, &ctx.control_events) {
                                                    let mut s = status.write().await;
                                                    s.state = ControlRunState::Running;
                                                    let q = s.queue_len;
                                                    drop(s);
                                                    let _ = events.send(AgentEvent::Status { state: ControlRunState::Running, queue_len: q });
                                                }
                                                (msg, v)
                                            }
                                            Err(_) => {
                                                has_error_messages = true;
                                                failed_tool_calls += 1;
                                                if let (Some(status), Some(events)) = (&ctx.control_status, &ctx.control_events) {
                                                    let mut s = status.write().await;
                                                    s.state = ControlRunState::Running;
                                                    let q = s.queue_len;
                                                    drop(s);
                                                    let _ = events.send(AgentEvent::Status { state: ControlRunState::Running, queue_len: q });
                                                }
                                                ("Error: tool result channel closed".to_string(), serde_json::Value::Null)
                                            }
                                        }
                                    } else {
                                        has_error_messages = true;
                                        failed_tool_calls += 1;
                                        ("Error: frontend tool hub not configured".to_string(), serde_json::Value::Null)
                                    }
                                } else {
                                    // Non-interactive UI render: echo args as the tool result payload.
                                    let msg = serde_json::to_string(&args_json)
                                        .unwrap_or_else(|_| args_json.to_string());
                                    successful_tool_calls += 1;
                                    (msg, args_json.clone())
                                }
                            } else {
                                // Regular server tool.
                                match self.execute_tool_call(tool_call, ctx).await {
                                    Ok(output) => {
                                        successful_tool_calls += 1;
                                        (output.clone(), serde_json::Value::String(output))
                                    }
                                    Err(e) => {
                                        failed_tool_calls += 1;
                                        has_error_messages = true;
                                        let s = format!("Error: {}", e);
                                        (s.clone(), serde_json::Value::String(s))
                                    }
                                }
                            };

                        if let Some(events) = &ctx.control_events {
                            let _ = events.send(AgentEvent::ToolResult {
                                tool_call_id: tool_call.id.clone(),
                                name: tool_name.clone(),
                                result: tool_result_json.clone(),
                            });
                        }

                        // Truncate tool result if too large to prevent context overflow
                        const MAX_TOOL_RESULT_CHARS: usize = 15000;
                        let truncated_content = if tool_message_content.len() > MAX_TOOL_RESULT_CHARS {
                            format!(
                                "{}... [truncated, {} chars total. For large data, consider writing to a file and reading specific sections]",
                                &tool_message_content[..MAX_TOOL_RESULT_CHARS],
                                tool_message_content.len()
                            )
                        } else {
                            tool_message_content
                        };

                        // Check for vision image marker [VISION_IMAGE:url] and create multimodal content
                        let message_content = if let Some(captures) = extract_vision_image_url(&truncated_content) {
                            // Create multimodal content with text and image
                            let text_without_marker = truncated_content.replace(&format!("[VISION_IMAGE:{}]", captures), "").trim().to_string();
                            tracing::info!("Including vision image in context: {}", captures);
                            MessageContent::multimodal(vec![
                                crate::llm::ContentPart::text(text_without_marker),
                                crate::llm::ContentPart::image_url(captures),
                            ])
                        } else {
                            MessageContent::text(truncated_content)
                        };

                        // Add tool result
                        messages.push(ChatMessage {
                            role: Role::Tool,
                            content: Some(message_content),
                            tool_calls: None,
                            tool_call_id: Some(tool_call.id.clone()),
                        });
                    }

                    // Special case: if complete_mission was called AND the LLM provided content,
                    // use that content as the final response instead of continuing the loop.
                    // This prevents the common pattern where Claude says "I'll complete the mission"
                    // with actual content, but then returns empty on the next iteration.
                    let called_complete_mission = tool_calls.iter().any(|tc| tc.function.name == "complete_mission");
                    if called_complete_mission {
                        if let Some(content) = response.content.as_ref().filter(|c| !c.trim().is_empty()) {
                            tracing::debug!("complete_mission called with content, returning early");
                            let signals = ExecutionSignals {
                                iterations: iterations_completed,
                                max_iterations: ctx.max_iterations as u32,
                                successful_tool_calls,
                                failed_tool_calls,
                                files_modified,
                                repetitive_actions,
                                has_error_messages,
                                partial_progress: true,
                                cost_spent_cents: total_cost_cents,
                                budget_total_cents: task.budget().total_cents(),
                                final_output: content.clone(),
                                model_used: model.to_string(),
                            };
                            return ExecutionLoopResult {
                                output: content.clone(),
                                cost_cents: total_cost_cents,
                                tool_log,
                                usage,
                                signals,
                                success: true,
                            };
                        }
                    }

                    continue;
                }
            }

            // No tool calls - final response
            if let Some(content) = response.content.filter(|c| !c.trim().is_empty()) {
                let signals = ExecutionSignals {
                    iterations: iterations_completed,
                    max_iterations: ctx.max_iterations as u32,
                    successful_tool_calls,
                    failed_tool_calls,
                    files_modified,
                    repetitive_actions,
                    has_error_messages,
                    partial_progress: true, // Completed successfully
                    cost_spent_cents: total_cost_cents,
                    budget_total_cents: task.budget().total_cents(),
                    final_output: content.clone(),
                    model_used: model.to_string(),
                };
                return ExecutionLoopResult {
                    output: content,
                    cost_cents: total_cost_cents,
                    tool_log,
                    usage,
                    signals,
                    success: true,
                };
            }

            // Empty response
            has_error_messages = true;
            let signals = ExecutionSignals {
                iterations: iterations_completed,
                max_iterations: ctx.max_iterations as u32,
                successful_tool_calls,
                failed_tool_calls,
                files_modified,
                repetitive_actions,
                has_error_messages,
                partial_progress: files_modified || successful_tool_calls > 0,
                cost_spent_cents: total_cost_cents,
                budget_total_cents: task.budget().total_cents(),
                final_output: "LLM returned empty response".to_string(),
                model_used: model.to_string(),
            };
            return ExecutionLoopResult {
                output: "LLM returned empty response".to_string(),
                cost_cents: total_cost_cents,
                tool_log,
                usage,
                signals,
                success: false,
            };
        }

        // Max iterations reached
        let signals = ExecutionSignals {
            iterations: iterations_completed,
            max_iterations: ctx.max_iterations as u32,
            successful_tool_calls,
            failed_tool_calls,
            files_modified,
            repetitive_actions,
            has_error_messages,
            partial_progress: files_modified || successful_tool_calls > 0,
            cost_spent_cents: total_cost_cents,
            budget_total_cents: task.budget().total_cents(),
            final_output: format!("Max iterations ({}) reached", ctx.max_iterations),
            model_used: model.to_string(),
        };
        ExecutionLoopResult {
            output: format!("Max iterations ({}) reached", ctx.max_iterations),
            cost_cents: total_cost_cents,
            tool_log,
            usage,
            signals,
            success: false,
        }
    }
}

impl Default for TaskExecutor {
    fn default() -> Self {
        Self::new()
    }
}


#[async_trait]
impl Agent for TaskExecutor {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::TaskExecutor
    }

    fn description(&self) -> &str {
        "Executes tasks using tools (file ops, terminal, search, etc.)"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        // Use model selected during planning, otherwise fall back to default.
        let selected = task
            .analysis()
            .selected_model
            .clone()
            .unwrap_or_else(|| ctx.config.default_model.clone());
        let model = selected.as_str();

        let result = self.run_loop(task, model, ctx).await;

        // Record telemetry
        task.analysis_mut().selected_model = Some(model.to_string());
        task.analysis_mut().actual_usage = result.usage.clone();

        // Update task budget
        let _ = task.budget_mut().try_spend(result.cost_cents);

        let mut agent_result = if result.success {
            AgentResult::success(&result.output, result.cost_cents)
        } else {
            AgentResult::failure(&result.output, result.cost_cents)
        };

        agent_result = agent_result
            .with_model(model)
            .with_data(json!({
                "tool_calls": result.tool_log.len(),
                "tools_used": result.tool_log,
                "usage": result.usage.as_ref().map(|u| json!({
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "total_tokens": u.total_tokens
                })),
                "execution_signals": {
                    "iterations": result.signals.iterations,
                    "max_iterations": result.signals.max_iterations,
                    "successful_tool_calls": result.signals.successful_tool_calls,
                    "failed_tool_calls": result.signals.failed_tool_calls,
                    "files_modified": result.signals.files_modified,
                    "repetitive_actions": result.signals.repetitive_actions,
                    "partial_progress": result.signals.partial_progress,
                }
            }));

        agent_result
    }
}

impl TaskExecutor {
    /// Execute a task and return detailed execution result for retry analysis.
    pub async fn execute_with_signals(&self, task: &mut Task, ctx: &AgentContext) -> (AgentResult, ExecutionSignals) {
        let selected = task
            .analysis()
            .selected_model
            .clone()
            .unwrap_or_else(|| ctx.config.default_model.clone());
        let model = selected.as_str();

        let result = self.run_loop(task, model, ctx).await;

        // Record telemetry
        task.analysis_mut().selected_model = Some(model.to_string());
        task.analysis_mut().actual_usage = result.usage.clone();

        // Update task budget
        let _ = task.budget_mut().try_spend(result.cost_cents);

        let mut agent_result = if result.success {
            AgentResult::success(&result.output, result.cost_cents)
        } else {
            AgentResult::failure(&result.output, result.cost_cents)
        };

        agent_result = agent_result
            .with_model(model)
            .with_data(json!({
                "tool_calls": result.tool_log.len(),
                "tools_used": result.tool_log,
                "usage": result.usage.as_ref().map(|u| json!({
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "total_tokens": u.total_tokens
                })),
            }));

        (agent_result, result.signals)
    }
}

impl LeafAgent for TaskExecutor {
    fn capability(&self) -> LeafCapability {
        LeafCapability::TaskExecution
    }
}

/// Extract vision image URL from tool result content.
/// Looks for the marker format: [VISION_IMAGE:https://...]
fn extract_vision_image_url(content: &str) -> Option<String> {
    let marker_start = "[VISION_IMAGE:";
    if let Some(start_idx) = content.find(marker_start) {
        let url_start = start_idx + marker_start.len();
        if let Some(end_idx) = content[url_start..].find(']') {
            let url = &content[url_start..url_start + end_idx];
            if url.starts_with("http") {
                return Some(url.to_string());
            }
        }
    }
    None
}


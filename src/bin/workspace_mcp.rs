//! MCP Server for core host tools (filesystem + library updates).
//!
//! Exposes a minimal set of Open Agent tools to OpenCode via MCP.
//! Communicates over stdio using JSON-RPC 2.0.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use sandboxed_sh::tools;
use sandboxed_sh::tools::Tool;

// =============================================================================
// JSON-RPC Types
// =============================================================================

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RuntimeWorkspace {
    workspace_root: Option<String>,
    workspace_type: Option<String>,
    working_dir: Option<String>,
    workspace_name: Option<String>,
    mission_id: Option<String>,
    context_root: Option<String>,
    mission_context: Option<String>,
    context_dir_name: Option<String>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// =============================================================================
// MCP Types
// =============================================================================

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct ToolResult {
    content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
}

// =============================================================================
// Tool Registry
// =============================================================================

fn container_root_from_path(path: &Path) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();
    let mut components = path.components();
    while let Some(component) = components.next() {
        prefix.push(component.as_os_str());
        if component.as_os_str() == std::ffi::OsStr::new("containers") {
            if let Some(next) = components.next() {
                prefix.push(next.as_os_str());
                return Some(prefix);
            }
            break;
        }
    }
    None
}

/// Check if we're running inside a container by detecting the presence of
/// container-relative paths and absence of HOST container paths.
fn is_inside_container() -> bool {
    // If /workspaces exists but the typical HOST container path doesn't,
    // we're likely inside a container
    Path::new("/workspaces").exists() && !Path::new("/root/.sandboxed-sh/containers").exists()
}

/// Translate a HOST path to a container-relative path.
/// HOST path: /root/.sandboxed-sh/containers/<name>/workspaces/mission-xxx
/// Container path: /workspaces/mission-xxx
fn translate_host_path_for_container(host_path: &str, workspace_root: Option<&str>) -> String {
    // If we have the workspace_root (container root on host), strip it from the path
    if let Some(root) = workspace_root {
        if host_path.starts_with(root) {
            let relative = &host_path[root.len()..];
            // Ensure it starts with /
            if relative.starts_with('/') {
                return relative.to_string();
            } else {
                return format!("/{}", relative);
            }
        }
    }

    // Fallback: try to detect and strip container path patterns
    // Pattern: /root/.sandboxed-sh/containers/<name>/...
    if let Some(idx) = host_path.find("/containers/") {
        let after_containers = &host_path[idx + "/containers/".len()..];
        if let Some(slash_idx) = after_containers.find('/') {
            return after_containers[slash_idx..].to_string();
        }
    }

    host_path.to_string()
}

fn hydrate_workspace_env(override_path: Option<PathBuf>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace = override_path.unwrap_or_else(|| {
        std::env::var("SANDBOXED_SH_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| cwd.clone())
    });

    if std::env::var("SANDBOXED_SH_WORKSPACE").is_err() {
        std::env::set_var(
            "SANDBOXED_SH_WORKSPACE",
            workspace.to_string_lossy().to_string(),
        );
    }

    if std::env::var("SANDBOXED_SH_WORKSPACE_TYPE").is_err() {
        if let Some(root) = container_root_from_path(&workspace) {
            std::env::set_var("SANDBOXED_SH_WORKSPACE_TYPE", "container");
            if std::env::var("SANDBOXED_SH_WORKSPACE_ROOT").is_err() {
                std::env::set_var(
                    "SANDBOXED_SH_WORKSPACE_ROOT",
                    root.to_string_lossy().to_string(),
                );
            }
        } else {
            std::env::set_var("SANDBOXED_SH_WORKSPACE_TYPE", "host");
        }
    }

    workspace
}

fn extract_workspace_from_initialize(params: &Value) -> Option<PathBuf> {
    if let Some(path) = params.get("rootPath").and_then(|v| v.as_str()) {
        return Some(PathBuf::from(path));
    }

    if let Some(uri) = params.get("rootUri").and_then(|v| v.as_str()) {
        if let Some(path) = uri.strip_prefix("file://") {
            return Some(PathBuf::from(path));
        }
    }

    if let Some(folders) = params.get("workspaceFolders").and_then(|v| v.as_array()) {
        for folder in folders {
            if let Some(path) = folder.get("path").and_then(|v| v.as_str()) {
                return Some(PathBuf::from(path));
            }
            if let Some(uri) = folder.get("uri").and_then(|v| v.as_str()) {
                if let Some(path) = uri.strip_prefix("file://") {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }

    None
}

fn runtime_workspace_path() -> PathBuf {
    if let Ok(path) = std::env::var("SANDBOXED_SH_RUNTIME_WORKSPACE_FILE") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".sandboxed-sh")
        .join("runtime")
        .join("current_workspace.json")
}

fn load_runtime_workspace() -> Option<RuntimeWorkspace> {
    // First, try to load from the global runtime file to get workspace_root
    let global_path = runtime_workspace_path();
    let global_state: Option<RuntimeWorkspace> = std::fs::read_to_string(&global_path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok());

    // Check for a local context file in the workspace root
    // This is more specific and avoids race conditions with parallel missions
    // We use workspace_root (host path) instead of working_dir (which may be container-relative)
    if let Some(ref state) = global_state {
        if let Some(ref workspace_root) = state.workspace_root {
            let local_context = PathBuf::from(workspace_root).join(".sandboxed-sh_context.json");
            if local_context.exists() {
                if let Ok(contents) = std::fs::read_to_string(&local_context) {
                    if let Ok(local_state) = serde_json::from_str::<RuntimeWorkspace>(&contents) {
                        // Use the local context which is specific to this workspace
                        return Some(local_state);
                    }
                }
            }
        }
    }

    global_state
}

fn apply_runtime_workspace(working_dir: &Arc<RwLock<PathBuf>>) {
    // CRITICAL: Do NOT overwrite SANDBOXED_SH_WORKSPACE_ROOT and SANDBOXED_SH_WORKSPACE_TYPE
    // These are set correctly at MCP spawn time from opencode.json and determine which
    // container commands run in. Overwriting them from a shared file causes race conditions
    // when multiple missions run in parallel.
    //
    // We only load from the file to get auxiliary context (mission_id, context paths) that
    // may be useful for some tools, but we preserve the core workspace identity.

    let Some(state) = load_runtime_workspace() else {
        debug_log("runtime_workspace", &json!({"status": "missing"}));
        return;
    };

    // Check if we're running inside a container and need to translate paths
    let inside_container = is_inside_container();

    // Use workspace_root from spawn env, NOT from the (potentially stale) file
    let workspace_root = std::env::var("SANDBOXED_SH_WORKSPACE_ROOT").ok();
    let workspace_root_ref = workspace_root.as_deref();

    debug_log(
        "runtime_workspace",
        &json!({
            "working_dir": state.working_dir,
            "workspace_root_from_env": workspace_root,
            "workspace_root_from_file": state.workspace_root,
            "workspace_type": state.workspace_type,
            "inside_container": inside_container,
        }),
    );

    // Only update working_dir if not already set from spawn env
    if std::env::var("SANDBOXED_SH_WORKSPACE").is_err() {
        if let Some(dir) = state.working_dir.as_ref() {
            let effective_dir = if inside_container {
                translate_host_path_for_container(dir, workspace_root_ref)
            } else {
                dir.clone()
            };
            std::env::set_var("SANDBOXED_SH_WORKSPACE", &effective_dir);
            if let Ok(mut guard) = working_dir.write() {
                *guard = PathBuf::from(&effective_dir);
            }
        }
    }

    // Only update workspace name if not already set
    if std::env::var("SANDBOXED_SH_WORKSPACE_NAME").is_err() {
        if let Some(name) = state.workspace_name.as_ref() {
            std::env::set_var("SANDBOXED_SH_WORKSPACE_NAME", name);
        }
    }

    // IMPORTANT: Do NOT modify SANDBOXED_SH_WORKSPACE_ROOT or SANDBOXED_SH_WORKSPACE_TYPE here!
    // These are set at spawn time and must remain stable for the lifetime of the MCP process.
    // The code below handles the special case of running INSIDE a container.
    if inside_container {
        // When running inside a container, clear these variables so RunCommand
        // executes directly (we're already in the container, no need to nspawn again)
        std::env::remove_var("SANDBOXED_SH_WORKSPACE_ROOT");
        std::env::set_var("SANDBOXED_SH_WORKSPACE_TYPE", "host");
    }
    // NOTE: We intentionally do NOT set SANDBOXED_SH_WORKSPACE_ROOT or SANDBOXED_SH_WORKSPACE_TYPE
    // from the file when not inside a container. These must come from spawn env only.

    if let Some(context_root) = state.context_root.as_ref() {
        // Also translate context_root for container environments
        let effective_context = if inside_container {
            translate_host_path_for_container(context_root, workspace_root_ref)
        } else {
            context_root.clone()
        };
        std::env::set_var("SANDBOXED_SH_CONTEXT_ROOT", &effective_context);
    }

    if let Some(mission_id) = state.mission_id.as_ref() {
        std::env::set_var("SANDBOXED_SH_MISSION_ID", mission_id);
    }

    if let Some(mission_context) = state.mission_context.as_ref() {
        // Also translate mission_context for container environments
        let effective_mission_context = if inside_container {
            translate_host_path_for_container(mission_context, workspace_root_ref)
        } else {
            mission_context.clone()
        };
        std::env::set_var("SANDBOXED_SH_MISSION_CONTEXT", &effective_mission_context);
    }

    if let Some(context_dir_name) = state.context_dir_name.as_ref() {
        std::env::set_var("SANDBOXED_SH_CONTEXT_DIR_NAME", context_dir_name);
    }
}

fn debug_log(tag: &str, payload: &Value) {
    if std::env::var("SANDBOXED_SH_MCP_DEBUG").ok().as_deref() != Some("1") {
        return;
    }
    let line = format!("[workspace-mcp] {} {}\n", tag, payload);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/workspace-mcp-debug.log")
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Tool for updating skill content in the library.
///
/// Updates the skill file directly in the library directory and triggers
/// workspace syncing via the backend API.
struct UpdateSkillTool;

#[async_trait]
impl Tool for UpdateSkillTool {
    fn name(&self) -> &str {
        "update_skill"
    }

    fn description(&self) -> &str {
        "Update skill content in the library. Writes to SKILL.md or additional reference files. \
         Changes are synced to all workspaces that use this skill. Use this to keep skill \
         data (like reference materials, examples, or instructions) up to date."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "Name of the skill to update (folder name in library/skill/)"
                },
                "content": {
                    "type": "string",
                    "description": "The new content for SKILL.md (the main skill file)"
                },
                "file_path": {
                    "type": "string",
                    "description": "Optional: path to a reference file within the skill (e.g., 'references/examples.md'). If provided, updates this file instead of SKILL.md"
                }
            },
            "required": ["skill_name", "content"]
        })
    }

    async fn execute(&self, args: Value, _working_dir: &Path) -> anyhow::Result<String> {
        let skill_name = args["skill_name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'skill_name' argument"))?;

        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' argument"))?;

        let file_path = args["file_path"].as_str();

        // Validate skill name (prevent path traversal)
        if skill_name.contains("..") || skill_name.contains('/') || skill_name.contains('\\') {
            return Err(anyhow::anyhow!(
                "Invalid skill name: contains path separators or '..'"
            ));
        }

        // Get backend API URL (defaults to localhost in dev)
        let api_base = std::env::var("SANDBOXED_SH_API_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());

        // Get auth token if set
        let auth_token = std::env::var("SANDBOXED_SH_API_TOKEN").ok();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        // Build the request URL
        let url = if let Some(ref_path) = file_path {
            // Validate reference path
            if ref_path.contains("..") {
                return Err(anyhow::anyhow!("Invalid file_path: contains '..'"));
            }
            format!(
                "{}/api/library/skill/{}/files/{}",
                api_base, skill_name, ref_path
            )
        } else {
            format!("{}/api/library/skill/{}", api_base, skill_name)
        };

        // Build request with optional auth
        let mut request = client
            .put(&url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "content": content }));

        if let Some(token) = auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        if status.is_success() {
            let target = file_path.unwrap_or("SKILL.md");
            Ok(format!(
                "Successfully updated skill '{}' (file: {}). Changes will sync to workspaces.",
                skill_name, target
            ))
        } else {
            let error_text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to update skill: {} - {}",
                status,
                error_text
            ))
        }
    }
}

/// Tool: update_init_script
///
/// Updates an init script fragment in the library directory and triggers
/// workspace syncing via the backend API.
struct UpdateInitScriptTool;

#[async_trait]
impl Tool for UpdateInitScriptTool {
    fn name(&self) -> &str {
        "update_init_script"
    }

    fn description(&self) -> &str {
        "Update an init script fragment in the library. Init scripts are reusable shell scripts \
         that run during workspace initialization. Use this to create or update setup scripts \
         that can be shared across workspaces."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script_name": {
                    "type": "string",
                    "description": "Name of the init script fragment (folder name in library/init-script/)"
                },
                "content": {
                    "type": "string",
                    "description": "The shell script content for SCRIPT.sh"
                }
            },
            "required": ["script_name", "content"]
        })
    }

    async fn execute(&self, args: Value, _working_dir: &Path) -> anyhow::Result<String> {
        let script_name = args["script_name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'script_name' argument"))?;

        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' argument"))?;

        // Validate script name (prevent path traversal)
        if script_name.contains("..") || script_name.contains('/') || script_name.contains('\\') {
            return Err(anyhow::anyhow!(
                "Invalid script name: contains path separators or '..'"
            ));
        }

        // Get backend API URL (defaults to localhost in dev)
        let api_base = std::env::var("SANDBOXED_SH_API_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());

        // Get auth token if set
        let auth_token = std::env::var("SANDBOXED_SH_API_TOKEN").ok();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let url = format!("{}/api/library/init-script/{}", api_base, script_name);

        // Build request with optional auth
        let mut request = client
            .put(&url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "content": content }));

        if let Some(token) = auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        if status.is_success() {
            Ok(format!(
                "Successfully updated init script '{}'. Changes will sync to workspaces.",
                script_name
            ))
        } else {
            let error_text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to update init script: {} - {}",
                status,
                error_text
            ))
        }
    }
}

fn tool_set() -> HashMap<String, Arc<dyn Tool>> {
    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();

    tools.insert("read_file".to_string(), Arc::new(tools::ReadFile));
    tools.insert("write_file".to_string(), Arc::new(tools::WriteFile));
    tools.insert("delete_file".to_string(), Arc::new(tools::DeleteFile));
    tools.insert("list_directory".to_string(), Arc::new(tools::ListDirectory));
    tools.insert("search_files".to_string(), Arc::new(tools::SearchFiles));
    tools.insert("grep_search".to_string(), Arc::new(tools::GrepSearch));
    tools.insert("fetch_url".to_string(), Arc::new(tools::FetchUrl));
    tools.insert("update_skill".to_string(), Arc::new(UpdateSkillTool));
    tools.insert(
        "update_init_script".to_string(),
        Arc::new(UpdateInitScriptTool),
    );

    tools
}

fn tool_definitions(tools: &HashMap<String, Arc<dyn Tool>>) -> Vec<ToolDefinition> {
    let mut defs = Vec::new();
    for tool in tools.values() {
        defs.push(ToolDefinition {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.parameters_schema(),
        });
    }
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    defs
}

fn execute_tool(
    runtime: &tokio::runtime::Runtime,
    tools: &HashMap<String, Arc<dyn Tool>>,
    name: &str,
    args: &Value,
    working_dir: &Path,
) -> ToolResult {
    let Some(tool) = tools.get(name) else {
        return ToolResult {
            content: vec![ToolContent::Text {
                text: format!("Unknown tool: {}", name),
            }],
            is_error: true,
        };
    };

    let result = runtime.block_on(tool.execute(args.clone(), working_dir));
    match result {
        Ok(text) => ToolResult {
            content: vec![ToolContent::Text { text }],
            is_error: false,
        },
        Err(e) => ToolResult {
            content: vec![ToolContent::Text {
                text: format!("Tool error: {}", e),
            }],
            is_error: true,
        },
    }
}

fn handle_request(
    request: &JsonRpcRequest,
    runtime: &tokio::runtime::Runtime,
    tools: &HashMap<String, Arc<dyn Tool>>,
    working_dir: &Arc<RwLock<PathBuf>>,
) -> Option<JsonRpcResponse> {
    match request.method.as_str() {
        "initialize" => {
            debug_log("initialize", &request.params);
            if let Some(path) = extract_workspace_from_initialize(&request.params) {
                let resolved = hydrate_workspace_env(Some(path));
                if let Ok(mut guard) = working_dir.write() {
                    *guard = resolved;
                }
            }
            apply_runtime_workspace(working_dir);
            Some(JsonRpcResponse::success(
                request.id.clone(),
                json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "workspace-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    }
                }),
            ))
        }
        "notifications/initialized" | "initialized" => None,
        "tools/list" => {
            let defs = tool_definitions(tools);
            Some(JsonRpcResponse::success(
                request.id.clone(),
                json!({ "tools": defs }),
            ))
        }
        "tools/call" => {
            debug_log("tools/call", &request.params);
            apply_runtime_workspace(working_dir);
            let name = request
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(json!({}));
            let cwd = working_dir
                .read()
                .map(|guard| guard.clone())
                .unwrap_or_else(|_| PathBuf::from("."));
            let result = execute_tool(runtime, tools, name, &args, &cwd);
            Some(JsonRpcResponse::success(request.id.clone(), json!(result)))
        }
        _ => Some(JsonRpcResponse::error(
            request.id.clone(),
            -32601,
            format!("Method not found: {}", request.method),
        )),
    }
}

fn main() {
    eprintln!("[workspace-mcp] Starting MCP server for workspace tools...");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to start tokio runtime");

    let tools = tool_set();
    let workspace = Arc::new(RwLock::new(hydrate_workspace_env(None)));

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let response = JsonRpcResponse::error(Value::Null, -32700, e.to_string());
                let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
                let _ = stdout.flush();
                continue;
            }
        };

        if let Some(response) = handle_request(&request, &runtime, &tools, &workspace) {
            if let Ok(resp) = serde_json::to_string(&response) {
                let _ = writeln!(stdout, "{}", resp);
                let _ = stdout.flush();
            }
        }
    }
}

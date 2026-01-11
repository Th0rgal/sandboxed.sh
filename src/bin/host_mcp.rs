//! MCP Server for core host tools (filesystem + command execution).
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

use open_agent::tools;
use open_agent::tools::Tool;

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

fn hydrate_workspace_env(override_path: Option<PathBuf>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace = override_path.unwrap_or_else(|| {
        std::env::var("OPEN_AGENT_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| cwd.clone())
    });

    if std::env::var("OPEN_AGENT_WORKSPACE").is_err() {
        std::env::set_var(
            "OPEN_AGENT_WORKSPACE",
            workspace.to_string_lossy().to_string(),
        );
    }

    if std::env::var("OPEN_AGENT_WORKSPACE_TYPE").is_err() {
        if let Some(root) = container_root_from_path(&workspace) {
            std::env::set_var("OPEN_AGENT_WORKSPACE_TYPE", "chroot");
            if std::env::var("OPEN_AGENT_WORKSPACE_ROOT").is_err() {
                std::env::set_var(
                    "OPEN_AGENT_WORKSPACE_ROOT",
                    root.to_string_lossy().to_string(),
                );
            }
        } else {
            std::env::set_var("OPEN_AGENT_WORKSPACE_TYPE", "host");
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
    if let Ok(path) = std::env::var("OPEN_AGENT_RUNTIME_WORKSPACE_FILE") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".openagent")
        .join("runtime")
        .join("current_workspace.json")
}

fn load_runtime_workspace() -> Option<RuntimeWorkspace> {
    let path = runtime_workspace_path();
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn apply_runtime_workspace(working_dir: &Arc<RwLock<PathBuf>>) {
    let Some(state) = load_runtime_workspace() else {
        debug_log("runtime_workspace", &json!({"status": "missing"}));
        return;
    };
    debug_log(
        "runtime_workspace",
        &json!({
            "working_dir": state.working_dir,
            "workspace_root": state.workspace_root,
            "workspace_type": state.workspace_type,
        }),
    );

    if let Some(dir) = state.working_dir.as_ref() {
        std::env::set_var("OPEN_AGENT_WORKSPACE", dir);
        if let Ok(mut guard) = working_dir.write() {
            *guard = PathBuf::from(dir);
        }
    }

    if let Some(name) = state.workspace_name.as_ref() {
        std::env::set_var("OPEN_AGENT_WORKSPACE_NAME", name);
    }

    if let Some(root) = state.workspace_root.as_ref() {
        std::env::set_var("OPEN_AGENT_WORKSPACE_ROOT", root);
    }

    if let Some(kind) = state.workspace_type.as_ref() {
        std::env::set_var("OPEN_AGENT_WORKSPACE_TYPE", kind);
    }

    if let Some(context_root) = state.context_root.as_ref() {
        std::env::set_var("OPEN_AGENT_CONTEXT_ROOT", context_root);
    }

    if let Some(mission_id) = state.mission_id.as_ref() {
        std::env::set_var("OPEN_AGENT_MISSION_ID", mission_id);
    }

    if let Some(mission_context) = state.mission_context.as_ref() {
        std::env::set_var("OPEN_AGENT_MISSION_CONTEXT", mission_context);
    }

    if let Some(context_dir_name) = state.context_dir_name.as_ref() {
        std::env::set_var("OPEN_AGENT_CONTEXT_DIR_NAME", context_dir_name);
    }
}

fn debug_log(tag: &str, payload: &Value) {
    if std::env::var("OPEN_AGENT_MCP_DEBUG").ok().as_deref() != Some("1") {
        return;
    }
    let line = format!("[host-mcp] {} {}\n", tag, payload);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/host-mcp-debug.log")
    {
        let _ = file.write_all(line.as_bytes());
    }
}

struct BashTool {
    delegate: tools::RunCommand,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command. Runs in the active workspace or isolated environment."
    }

    fn parameters_schema(&self) -> Value {
        self.delegate.parameters_schema()
    }

    async fn execute(&self, mut args: Value, working_dir: &Path) -> anyhow::Result<String> {
        if let Some(obj) = args.as_object_mut() {
            obj.entry("shell".to_string())
                .or_insert_with(|| Value::String("/bin/bash".to_string()));
        }
        self.delegate.execute(args, working_dir).await
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
    tools.insert(
        "bash".to_string(),
        Arc::new(BashTool {
            delegate: tools::RunCommand,
        }),
    );
    tools.insert("fetch_url".to_string(), Arc::new(tools::FetchUrl));

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
                        "name": "host-mcp",
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
    eprintln!("[host-mcp] Starting MCP server for host tools...");

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

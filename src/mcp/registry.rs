//! MCP runtime registry - manages connections and tool execution.
//!
//! Supports both HTTP and stdio transports:
//! - HTTP: JSON-RPC over HTTP POST requests
//! - Stdio: JSON-RPC over stdin/stdout with spawned child processes

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use super::config::McpConfigStore;
use super::types::*;

/// MCP protocol version we support
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Sanitize MCP server name to create a valid function name prefix.
///
/// Converts names like "filesystem" or "My Server" to lowercase alphanumeric.
fn sanitize_mcp_prefix(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_lowercase()
        .replace('-', "_")
}

fn command_exists(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }

    let path = Path::new(command);
    if path.is_absolute() || command.contains('/') {
        return path.exists();
    }

    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                if candidate.with_extension(ext).is_file() {
                    return true;
                }
            }
        }
    }

    false
}

/// Handle for a stdio MCP process
struct StdioProcess {
    child: Child,
    stdin: tokio::process::ChildStdin,
    stdout_lines: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
}

/// Runtime registry for MCP servers.
pub struct McpRegistry {
    /// Persistent configuration store
    config_store: Arc<McpConfigStore>,
    /// Runtime state for each MCP (keyed by ID)
    states: RwLock<HashMap<Uuid, McpServerState>>,
    /// HTTP client for HTTP MCP requests
    http_client: reqwest::Client,
    /// Stdio processes for stdio MCPs (keyed by ID)
    stdio_processes: RwLock<HashMap<Uuid, Arc<Mutex<StdioProcess>>>>,
    /// Disabled tools (by name)
    disabled_tools: RwLock<std::collections::HashSet<String>>,
    /// Request ID counter for JSON-RPC
    request_id: AtomicU64,
}

const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

impl McpRegistry {
    /// Create a new MCP registry.
    pub async fn new(working_dir: &Path) -> Self {
        let config_store = Arc::new(McpConfigStore::new(working_dir).await);

        // Initialize states from configs
        let mut configs = config_store.list().await;
        configs = Self::ensure_defaults(&config_store, configs, working_dir).await;
        let mut states = HashMap::new();
        for config in configs {
            states.insert(config.id, McpServerState::from_config(config));
        }

        // Use generous timeouts for long-running MCP tools (e.g., Minecraft launches)
        let http_client = reqwest::Client::builder()
            .timeout(MCP_REQUEST_TIMEOUT)
            .connect_timeout(MCP_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();

        Self {
            config_store,
            states: RwLock::new(states),
            http_client,
            stdio_processes: RwLock::new(HashMap::new()),
            disabled_tools: RwLock::new(std::collections::HashSet::new()),
            request_id: AtomicU64::new(1),
        }
    }

    /// Return the raw MCP configs (for workspace opencode.json generation).
    pub async fn list_configs(&self) -> Vec<McpServerConfig> {
        self.config_store.list().await
    }

    fn default_configs(working_dir: &Path) -> Vec<McpServerConfig> {
        let mut desktop_env = HashMap::new();
        if let Ok(res) = std::env::var("DESKTOP_RESOLUTION") {
            if !res.trim().is_empty() {
                desktop_env.insert("DESKTOP_RESOLUTION".to_string(), res);
            }
        } else {
            desktop_env.insert("DESKTOP_RESOLUTION".to_string(), "1920x1080".to_string());
        }

        let desktop_command = {
            let release = working_dir
                .join("target")
                .join("release")
                .join("desktop-mcp");
            let debug = working_dir.join("target").join("debug").join("desktop-mcp");
            if release.exists() {
                release.to_string_lossy().to_string()
            } else if debug.exists() {
                debug.to_string_lossy().to_string()
            } else {
                "desktop-mcp".to_string()
            }
        };
        let mut desktop = McpServerConfig::new_stdio(
            "desktop".to_string(),
            desktop_command,
            Vec::new(),
            desktop_env,
        );
        desktop.scope = McpScope::Workspace;

        let host_command = {
            let release = working_dir.join("target").join("release").join("host-mcp");
            let debug = working_dir.join("target").join("debug").join("host-mcp");
            if release.exists() {
                release.to_string_lossy().to_string()
            } else if debug.exists() {
                debug.to_string_lossy().to_string()
            } else {
                "host-mcp".to_string()
            }
        };
        let mut host = McpServerConfig::new_stdio(
            "host".to_string(),
            host_command,
            Vec::new(),
            HashMap::new(),
        );
        host.scope = McpScope::Workspace;
        // Prefer bunx (Bun) when present, but fall back to npx for compatibility.
        let js_runner = if command_exists("bunx") {
            "bunx"
        } else {
            "npx"
        };
        let mut playwright = McpServerConfig::new_stdio(
            "playwright".to_string(),
            js_runner.to_string(),
            vec![
                "@playwright/mcp@latest".to_string(),
                "--isolated".to_string(),
                "--no-sandbox".to_string(),
            ],
            HashMap::new(),
        );
        playwright.scope = McpScope::Workspace;

        vec![host, desktop, playwright]
    }

    async fn ensure_defaults(
        config_store: &McpConfigStore,
        mut configs: Vec<McpServerConfig>,
        working_dir: &Path,
    ) -> Vec<McpServerConfig> {
        fn resolve_local_binary(working_dir: &Path, name: &str) -> Option<String> {
            let release = working_dir.join("target").join("release").join(name);
            if release.exists() {
                return Some(release.to_string_lossy().to_string());
            }
            let debug = working_dir.join("target").join("debug").join(name);
            if debug.exists() {
                return Some(debug.to_string_lossy().to_string());
            }
            None
        }

        let defaults = Self::default_configs(working_dir);
        for config in defaults {
            if configs.iter().any(|c| c.name == config.name) {
                continue;
            }
            match config_store.add(config.clone()).await {
                Ok(saved) => configs.push(saved),
                Err(e) => tracing::warn!("Failed to add default MCP {}: {}", config.name, e),
            }
        }
        // Ensure Playwright MCP runs in isolated mode and without sandboxing
        // so it can launch browsers under root.
        for config in configs.iter_mut() {
            if config.name != "playwright" {
                continue;
            }

            if config.scope != McpScope::Workspace {
                config.scope = McpScope::Workspace;
                let id = config.id;
                let _ = config_store
                    .update(id, |c| {
                        c.scope = McpScope::Workspace;
                    })
                    .await;
            }

            let missing_flags: Vec<&str> = match &config.transport {
                McpTransport::Stdio { args, .. } => ["--isolated", "--no-sandbox"]
                    .iter()
                    .copied()
                    .filter(|flag| !args.iter().any(|arg| arg == *flag))
                    .collect(),
                McpTransport::Http { .. } => Vec::new(),
            };

            if missing_flags.is_empty() {
                continue;
            }

            if let McpTransport::Stdio { args, .. } = &mut config.transport {
                for flag in &missing_flags {
                    args.push((*flag).to_string());
                }
            }

            let id = config.id;
            let _ = config_store
                .update(id, |c| {
                    if let McpTransport::Stdio { args, .. } = &mut c.transport {
                        for flag in ["--isolated", "--no-sandbox"] {
                            if !args.iter().any(|arg| arg == flag) {
                                args.push(flag.to_string());
                            }
                        }
                    }
                })
                .await;
        }

        // Prefer repo-local MCP binaries for host/desktop (debug or release),
        // so default configs work without installing to PATH.
        for config in configs.iter_mut() {
            let binary_name = match config.name.as_str() {
                "host" => Some("host-mcp"),
                "desktop" => Some("desktop-mcp"),
                _ => None,
            };

            let Some(binary_name) = binary_name else {
                continue;
            };
            let Some(resolved) = resolve_local_binary(working_dir, binary_name) else {
                continue;
            };

            if let McpTransport::Stdio { command, .. } = &mut config.transport {
                if command != &resolved {
                    *command = resolved.clone();
                    let id = config.id;
                    let _ = config_store
                        .update(id, |c| {
                            if let McpTransport::Stdio { command, .. } = &mut c.transport {
                                *command = resolved.clone();
                            }
                        })
                        .await;
                }
            }

            if config.scope != McpScope::Workspace {
                config.scope = McpScope::Workspace;
                let id = config.id;
                let _ = config_store
                    .update(id, |c| {
                        c.scope = McpScope::Workspace;
                    })
                    .await;
            }
        }

        configs
    }

    /// Get the next request ID for JSON-RPC
    fn next_request_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a JSON-RPC request via HTTP
    async fn send_jsonrpc_http(
        &self,
        endpoint: &str,
        method: &str,
        params: Option<serde_json::Value>,
        headers: &HashMap<String, String>,
    ) -> anyhow::Result<serde_json::Value> {
        let request = JsonRpcRequest::new(self.next_request_id(), method, params);

        let mut req_builder = self
            .http_client
            .post(endpoint)
            .header("Content-Type", "application/json");

        // Add custom headers
        for (key, value) in headers {
            req_builder = req_builder.header(key.as_str(), value.as_str());
        }

        let response = req_builder.json(&request).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status());
        }

        let json_response: JsonRpcResponse = response.json().await?;

        if let Some(error) = json_response.error {
            anyhow::bail!("JSON-RPC error {}: {}", error.code, error.message);
        }

        json_response
            .result
            .ok_or_else(|| anyhow::anyhow!("No result in response"))
    }

    /// Send a JSON-RPC request via stdio
    async fn send_jsonrpc_stdio(
        &self,
        process: &Arc<Mutex<StdioProcess>>,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let request = JsonRpcRequest::new(self.next_request_id(), method, params);
        let request_json = serde_json::to_string(&request)?;

        let mut proc = process.lock().await;

        // Write request to stdin
        proc.stdin.write_all(request_json.as_bytes()).await?;
        proc.stdin.write_all(b"\n").await?;
        proc.stdin.flush().await?;

        // Read response from stdout
        let mut stdout = proc.stdout_lines.lock().await;
        let mut line = String::new();

        // Read with timeout
        let read_result =
            tokio::time::timeout(MCP_REQUEST_TIMEOUT, stdout.read_line(&mut line)).await;

        match read_result {
            Ok(Ok(0)) => anyhow::bail!("MCP process closed stdout"),
            Ok(Ok(_)) => {
                let json_response: JsonRpcResponse = serde_json::from_str(&line)?;

                if let Some(error) = json_response.error {
                    anyhow::bail!("JSON-RPC error {}: {}", error.code, error.message);
                }

                json_response
                    .result
                    .ok_or_else(|| anyhow::anyhow!("No result in response"))
            }
            Ok(Err(e)) => anyhow::bail!("Read error: {}", e),
            Err(_) => anyhow::bail!("Timeout waiting for MCP response"),
        }
    }

    /// Spawn a stdio MCP process
    async fn spawn_stdio_process(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> anyhow::Result<StdioProcess> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Add environment variables
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;

        let stdout_lines = Arc::new(Mutex::new(BufReader::new(stdout)));

        Ok(StdioProcess {
            child,
            stdin,
            stdout_lines,
        })
    }

    /// Initialize connection with an MCP server (HTTP)
    async fn initialize_mcp_http(
        &self,
        endpoint: &str,
        headers: &HashMap<String, String>,
    ) -> anyhow::Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "open-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let result = self
            .send_jsonrpc_http(
                endpoint,
                "initialize",
                Some(serde_json::to_value(params)?),
                headers,
            )
            .await?;

        let init_result: InitializeResult = serde_json::from_value(result)?;

        // Send initialized notification (no response expected, but some servers require it)
        let mut req_builder = self
            .http_client
            .post(endpoint)
            .header("Content-Type", "application/json");

        for (key, value) in headers {
            req_builder = req_builder.header(key.as_str(), value.as_str());
        }

        let _ = req_builder
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .send()
            .await;

        Ok(init_result)
    }

    /// Initialize connection with an MCP server (stdio)
    async fn initialize_mcp_stdio(
        &self,
        process: &Arc<Mutex<StdioProcess>>,
    ) -> anyhow::Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "open-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let result = self
            .send_jsonrpc_stdio(process, "initialize", Some(serde_json::to_value(params)?))
            .await?;

        let init_result: InitializeResult = serde_json::from_value(result)?;

        // Send initialized notification
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let notification_json = serde_json::to_string(&notification)?;

        let mut proc = process.lock().await;
        let _ = proc.stdin.write_all(notification_json.as_bytes()).await;
        let _ = proc.stdin.write_all(b"\n").await;
        let _ = proc.stdin.flush().await;

        Ok(init_result)
    }

    /// List all MCP servers with their current state.
    pub async fn list(&self) -> Vec<McpServerState> {
        self.states.read().await.values().cloned().collect()
    }

    /// Get a specific MCP server state.
    pub async fn get(&self, id: Uuid) -> Option<McpServerState> {
        self.states.read().await.get(&id).cloned()
    }

    /// Add a new MCP server.
    /// Note: This does NOT automatically attempt to connect. Use refresh() after adding.
    pub async fn add(&self, req: AddMcpRequest) -> anyhow::Result<McpServerState> {
        let mut config = match &req.transport {
            McpTransport::Http { endpoint, .. } => {
                McpServerConfig::new(req.name.clone(), endpoint.clone())
            }
            McpTransport::Stdio { command, args, env } => McpServerConfig::new_stdio(
                req.name.clone(),
                command.clone(),
                args.clone(),
                env.clone(),
            ),
        };
        config.description = req.description;
        if let Some(scope) = req.scope {
            config.scope = scope;
        }

        // Save to persistent store
        let config = self.config_store.add(config).await?;
        let id = config.id;

        // Create runtime state
        let state = McpServerState::from_config(config.clone());

        // Add to states
        {
            let mut states = self.states.write().await;
            states.insert(id, state.clone());
        }

        // Return immediately - user should call refresh() to connect
        Ok(state)
    }

    /// Remove an MCP server.
    pub async fn remove(&self, id: Uuid) -> anyhow::Result<()> {
        // Kill stdio process if running
        {
            let mut processes = self.stdio_processes.write().await;
            if let Some(process) = processes.remove(&id) {
                let mut proc = process.lock().await;
                let _ = proc.child.kill().await;
            }
        }

        // Remove from persistent store
        self.config_store.remove(id).await?;

        // Remove from states
        self.states.write().await.remove(&id);

        Ok(())
    }

    /// Enable an MCP server.
    /// Note: This does NOT automatically attempt to connect. Use refresh() after enabling.
    pub async fn enable(&self, id: Uuid) -> anyhow::Result<McpServerState> {
        // Update persistent config
        let config = self.config_store.enable(id).await?;

        // Update runtime state
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&id) {
                state.config = config;
                state.status = McpStatus::Disconnected;
                state.error = None;
            }
        }

        self.get(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Disable an MCP server.
    pub async fn disable(&self, id: Uuid) -> anyhow::Result<McpServerState> {
        // Kill stdio process if running
        {
            let mut processes = self.stdio_processes.write().await;
            if let Some(process) = processes.remove(&id) {
                let mut proc = process.lock().await;
                let _ = proc.child.kill().await;
            }
        }

        // Update persistent config
        let config = self.config_store.disable(id).await?;

        // Update runtime state
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&id) {
                state.config = config;
                state.status = McpStatus::Disabled;
                state.error = None;
            }
        }

        self.get(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Update an MCP server configuration (name, description, transport/env).
    /// Note: If transport changes, a refresh is recommended.
    pub async fn update(
        &self,
        id: Uuid,
        req: super::types::UpdateMcpRequest,
    ) -> anyhow::Result<McpServerState> {
        // Kill existing stdio process if transport might change
        if req.transport.is_some() {
            let mut processes = self.stdio_processes.write().await;
            if let Some(process) = processes.remove(&id) {
                let mut proc = process.lock().await;
                let _ = proc.child.kill().await;
            }
        }

        // Update persistent config
        let config = self
            .config_store
            .update(id, |c| {
                if let Some(name) = &req.name {
                    c.name = name.clone();
                }
                if let Some(description) = &req.description {
                    c.description = Some(description.clone());
                }
                if let Some(enabled) = req.enabled {
                    c.enabled = enabled;
                }
                if let Some(scope) = req.scope {
                    c.scope = scope;
                }
                if let Some(transport) = &req.transport {
                    c.transport = transport.clone();
                }
            })
            .await?;

        // Update runtime state
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&id) {
                state.config = config;
                // Reset status if transport changed
                if req.transport.is_some() {
                    state.status = if state.config.enabled {
                        McpStatus::Disconnected
                    } else {
                        McpStatus::Disabled
                    };
                    state.error = None;
                }
            }
        }

        self.get(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Helper to update state with error - retries a few times to handle lock contention
    async fn update_state_error(&self, id: Uuid, error_msg: String) {
        // Try up to 5 times with small delays to handle temporary lock contention
        for attempt in 0..5 {
            if let Ok(mut states) = self.states.try_write() {
                if let Some(state) = states.get_mut(&id) {
                    state.status = McpStatus::Error;
                    state.error = Some(error_msg);
                }
                return;
            }
            // Small delay before retry (10ms, 20ms, 40ms, 80ms, 160ms)
            if attempt < 4 {
                tokio::time::sleep(Duration::from_millis(10 << attempt)).await;
            }
        }
        // If still can't get lock after retries, log warning
        tracing::warn!("Failed to update MCP {} error state after retries", id);
    }

    /// Helper to update state with success - retries a few times to handle lock contention
    async fn update_state_success(
        &self,
        id: Uuid,
        tool_descriptors: Vec<McpToolDescriptor>,
        server_version: Option<String>,
    ) {
        let tool_names: Vec<String> = tool_descriptors.iter().map(|t| t.name.clone()).collect();

        // Try up to 5 times with small delays to handle temporary lock contention
        for attempt in 0..5 {
            if let Ok(mut states) = self.states.try_write() {
                if let Some(state) = states.get_mut(&id) {
                    state.config.tools = tool_names.clone();
                    state.config.tool_descriptors = tool_descriptors.clone();
                    state.config.version = server_version.clone();
                    state.config.last_connected_at = Some(chrono::Utc::now());
                    state.status = McpStatus::Connected;
                    state.error = None;
                }
                return;
            }
            // Small delay before retry
            if attempt < 4 {
                tokio::time::sleep(Duration::from_millis(10 << attempt)).await;
            }
        }
        // If still can't get lock after retries, log warning
        tracing::warn!("Failed to update MCP {} success state after retries", id);
    }

    /// Refresh an MCP server - reconnect and discover tools.
    pub async fn refresh(&self, id: Uuid) -> anyhow::Result<McpServerState> {
        let state = self
            .get(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP not found"))?;

        if !state.config.enabled {
            return Ok(state);
        }

        match &state.config.transport {
            McpTransport::Http { endpoint, headers } => {
                self.refresh_http(id, endpoint.clone(), headers.clone())
                    .await
            }
            McpTransport::Stdio { command, args, env } => {
                self.refresh_stdio(id, command.clone(), args.clone(), env.clone())
                    .await
            }
        }
    }

    /// Refresh an HTTP MCP server
    async fn refresh_http(
        &self,
        id: Uuid,
        endpoint: String,
        headers: HashMap<String, String>,
    ) -> anyhow::Result<McpServerState> {
        let endpoint = endpoint.trim_end_matches('/').to_string();

        // Step 1: Initialize the MCP connection with JSON-RPC
        let init_result = match self.initialize_mcp_http(&endpoint, &headers).await {
            Ok(result) => result,
            Err(e) => {
                self.update_state_error(id, format!("Initialize failed: {}", e))
                    .await;
                return self
                    .get(id)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("MCP not found"));
            }
        };

        // Extract server version if available
        let server_version = init_result
            .server_info
            .as_ref()
            .and_then(|s| s.version.clone());

        // Step 2: List tools using JSON-RPC
        match self
            .send_jsonrpc_http(&endpoint, "tools/list", None, &headers)
            .await
        {
            Ok(result) => {
                match serde_json::from_value::<McpToolsResponse>(result) {
                    Ok(tools_response) => {
                        let tool_descriptors = tools_response.tools;
                        let tool_names: Vec<String> =
                            tool_descriptors.iter().map(|t| t.name.clone()).collect();

                        // Update config with discovered tools
                        let _ = self
                            .config_store
                            .update(id, |c| {
                                c.tools = tool_names.clone();
                                c.tool_descriptors = tool_descriptors.clone();
                                c.version = server_version.clone();
                                c.last_connected_at = Some(chrono::Utc::now());
                            })
                            .await;

                        // Update runtime state
                        self.update_state_success(id, tool_descriptors, server_version)
                            .await;
                    }
                    Err(e) => {
                        self.update_state_error(id, format!("Failed to parse tools: {}", e))
                            .await;
                    }
                }
            }
            Err(e) => {
                self.update_state_error(id, format!("tools/list failed: {}", e))
                    .await;
            }
        }

        self.get(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Refresh a stdio MCP server
    async fn refresh_stdio(
        &self,
        id: Uuid,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> anyhow::Result<McpServerState> {
        // Kill existing process if any
        {
            let mut processes = self.stdio_processes.write().await;
            if let Some(process) = processes.remove(&id) {
                let mut proc = process.lock().await;
                let _ = proc.child.kill().await;
            }
        }

        // Spawn new process
        let process = match self.spawn_stdio_process(&command, &args, &env).await {
            Ok(p) => Arc::new(Mutex::new(p)),
            Err(e) => {
                self.update_state_error(id, format!("Failed to spawn process: {}", e))
                    .await;
                return self
                    .get(id)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("MCP not found"));
            }
        };

        // Store process handle
        {
            let mut processes = self.stdio_processes.write().await;
            processes.insert(id, Arc::clone(&process));
        }

        // Step 1: Initialize the MCP connection
        let init_result = match self.initialize_mcp_stdio(&process).await {
            Ok(result) => result,
            Err(e) => {
                self.update_state_error(id, format!("Initialize failed: {}", e))
                    .await;
                // Clean up process
                let mut processes = self.stdio_processes.write().await;
                if let Some(process) = processes.remove(&id) {
                    let mut proc = process.lock().await;
                    let _ = proc.child.kill().await;
                }
                return self
                    .get(id)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("MCP not found"));
            }
        };

        // Extract server version if available
        let server_version = init_result
            .server_info
            .as_ref()
            .and_then(|s| s.version.clone());

        // Step 2: List tools
        match self.send_jsonrpc_stdio(&process, "tools/list", None).await {
            Ok(result) => {
                match serde_json::from_value::<McpToolsResponse>(result) {
                    Ok(tools_response) => {
                        let tool_descriptors = tools_response.tools;
                        let tool_names: Vec<String> =
                            tool_descriptors.iter().map(|t| t.name.clone()).collect();

                        // Update config with discovered tools
                        let _ = self
                            .config_store
                            .update(id, |c| {
                                c.tools = tool_names.clone();
                                c.tool_descriptors = tool_descriptors.clone();
                                c.version = server_version.clone();
                                c.last_connected_at = Some(chrono::Utc::now());
                            })
                            .await;

                        // Update runtime state
                        self.update_state_success(id, tool_descriptors, server_version)
                            .await;
                    }
                    Err(e) => {
                        self.update_state_error(id, format!("Failed to parse tools: {}", e))
                            .await;
                    }
                }
            }
            Err(e) => {
                self.update_state_error(id, format!("tools/list failed: {}", e))
                    .await;
            }
        }

        self.get(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Refresh all MCP servers concurrently.
    pub async fn refresh_all(&self) {
        let ids: Vec<Uuid> = self.states.read().await.keys().cloned().collect();

        // Refresh all MCPs concurrently using join_all
        let futures: Vec<_> = ids.iter().map(|id| self.refresh(*id)).collect();
        futures::future::join_all(futures).await;
    }

    /// Call a tool on an MCP server.
    pub async fn call_tool(
        &self,
        mcp_id: Uuid,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<String> {
        // Check if tool is disabled
        if self.disabled_tools.read().await.contains(tool_name) {
            anyhow::bail!("Tool {} is disabled", tool_name);
        }

        let state = self
            .get(mcp_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP not found"))?;

        if !state.config.enabled {
            anyhow::bail!("MCP {} is disabled", state.config.name);
        }

        if state.status != McpStatus::Connected {
            anyhow::bail!("MCP {} is not connected", state.config.name);
        }

        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments
        });

        let result = match &state.config.transport {
            McpTransport::Http { endpoint, headers } => {
                let endpoint = endpoint.trim_end_matches('/');
                self.send_jsonrpc_http(endpoint, "tools/call", Some(params), headers)
                    .await
            }
            McpTransport::Stdio { .. } => {
                let processes = self.stdio_processes.read().await;
                let process = processes
                    .get(&mcp_id)
                    .ok_or_else(|| anyhow::anyhow!("No stdio process for MCP {}", mcp_id))?;
                self.send_jsonrpc_stdio(process, "tools/call", Some(params))
                    .await
            }
        };

        match result {
            Ok(result) => {
                let response: McpCallToolResponse = serde_json::from_value(result)?;

                // Increment counters
                {
                    let mut states = self.states.write().await;
                    if let Some(state) = states.get_mut(&mcp_id) {
                        if response.is_error {
                            state.tool_errors += 1;
                        } else {
                            state.tool_calls += 1;
                        }
                    }
                }

                if response.is_error {
                    let error_text = response
                        .content
                        .iter()
                        .filter_map(|c| c.text.as_deref())
                        .collect::<Vec<_>>()
                        .join("\n");
                    anyhow::bail!("Tool error: {}", error_text);
                }

                // Combine text content
                let output = response
                    .content
                    .iter()
                    .filter_map(|c| c.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");

                Ok(output)
            }
            Err(e) => {
                // Increment error counter
                let mut states = self.states.write().await;
                if let Some(state) = states.get_mut(&mcp_id) {
                    state.tool_errors += 1;
                }
                anyhow::bail!("Tool call failed: {}", e);
            }
        }
    }

    /// List all tools from all connected MCPs.
    ///
    /// Tool names are prefixed with the MCP server name to avoid conflicts
    /// with built-in tools (e.g., `filesystem_read_file` instead of `read_file`).
    pub async fn list_tools(&self) -> Vec<McpTool> {
        let states = self.states.read().await;
        let disabled = self.disabled_tools.read().await;

        let mut tools = Vec::new();
        for state in states.values() {
            if state.config.enabled && state.status == McpStatus::Connected {
                // Derive prefix from MCP server name (sanitized for function names)
                let prefix = sanitize_mcp_prefix(&state.config.name);

                for descriptor in &state.config.tool_descriptors {
                    // Prefix tool name with MCP server name to avoid conflicts
                    let prefixed_name = format!("{}_{}", prefix, descriptor.name);
                    let prefixed_description =
                        format!("[{}] {}", state.config.name, descriptor.description);

                    tools.push(McpTool {
                        name: prefixed_name.clone(),
                        description: prefixed_description,
                        parameters_schema: descriptor.input_schema.clone(),
                        mcp_id: state.config.id,
                        enabled: !disabled.contains(&descriptor.name)
                            && !disabled.contains(&prefixed_name),
                    });
                }
            }
        }
        tools
    }

    /// Enable a tool.
    pub async fn enable_tool(&self, name: &str) {
        self.disabled_tools.write().await.remove(name);
    }

    /// Disable a tool.
    pub async fn disable_tool(&self, name: &str) {
        self.disabled_tools.write().await.insert(name.to_string());
    }

    /// Check if a tool is enabled.
    pub async fn is_tool_enabled(&self, name: &str) -> bool {
        !self.disabled_tools.read().await.contains(name)
    }

    /// Find a tool by name (prefixed) and return its MCP ID if found.
    ///
    /// Tool names should be in prefixed format (e.g., `filesystem_read_file`).
    pub async fn find_tool(&self, name: &str) -> Option<McpTool> {
        let states = self.states.read().await;
        let disabled = self.disabled_tools.read().await;

        for state in states.values() {
            if state.config.enabled && state.status == McpStatus::Connected {
                let prefix = sanitize_mcp_prefix(&state.config.name);

                for descriptor in &state.config.tool_descriptors {
                    let prefixed_name = format!("{}_{}", prefix, descriptor.name);

                    if prefixed_name == name
                        && !disabled.contains(&descriptor.name)
                        && !disabled.contains(&prefixed_name)
                    {
                        return Some(McpTool {
                            name: prefixed_name,
                            description: format!(
                                "[{}] {}",
                                state.config.name, descriptor.description
                            ),
                            parameters_schema: descriptor.input_schema.clone(),
                            mcp_id: state.config.id,
                            enabled: true,
                        });
                    }
                }
            }
        }
        None
    }

    /// Get the original (unprefixed) tool name for an MCP call.
    ///
    /// When calling the MCP server, we need to use the original tool name.
    pub fn strip_prefix(prefixed_name: &str) -> String {
        // Find the first underscore and return everything after it
        if let Some(idx) = prefixed_name.find('_') {
            prefixed_name[idx + 1..].to_string()
        } else {
            prefixed_name.to_string()
        }
    }
}

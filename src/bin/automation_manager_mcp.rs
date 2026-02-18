//! MCP Server for automation management.
//!
//! Allows agents to create, update, list, and delete automations for their mission.
//! Communicates over stdio using JSON-RPC 2.0.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use sandboxed_sh::api::mission_store::{
    Automation, AutomationExecution, CommandSource, RetryConfig, TriggerType,
};

// =============================================================================
// JSON-RPC Types
// =============================================================================

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
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
struct ServerInfo {
    name: String,
    version: String,
}

// =============================================================================
// Tool Params
// =============================================================================

#[derive(Debug, Deserialize)]
struct ListAutomationsParams {
    #[serde(default)]
    active_only: bool,
}

#[derive(Debug, Deserialize)]
struct CreateAutomationParams {
    command_source: CommandSource,
    trigger: TriggerType,
    #[serde(default)]
    variables: HashMap<String, String>,
    #[serde(default)]
    retry_config: Option<RetryConfig>,
}

#[derive(Debug, Deserialize)]
struct UpdateAutomationParams {
    id: String,
    #[serde(default)]
    command_source: Option<CommandSource>,
    #[serde(default)]
    trigger: Option<TriggerType>,
    #[serde(default)]
    variables: Option<HashMap<String, String>>,
    #[serde(default)]
    retry_config: Option<RetryConfig>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct DeleteAutomationParams {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GetExecutionHistoryParams {
    automation_id: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

// =============================================================================
// MCP Server Implementation
// =============================================================================

struct AutomationManagerMcp {
    mission_id: Uuid,
    api_url: String,
    api_token: Option<String>,
}

impl AutomationManagerMcp {
    fn new(mission_id: Uuid, api_url: String, api_token: Option<String>) -> Self {
        Self {
            mission_id,
            api_url,
            api_token,
        }
    }

    fn get_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "list_automations".to_string(),
                description: "List all automations for the current mission".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "active_only": {
                            "type": "boolean",
                            "description": "Only return active automations (default: false)"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "create_automation".to_string(),
                description: "Create a new automation for the current mission".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["command_source", "trigger"],
                    "properties": {
                        "command_source": {
                            "type": "object",
                            "description": "Source of the command to execute",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": {"const": "library"},
                                        "name": {"type": "string", "description": "Command name from library"}
                                    },
                                    "required": ["type", "name"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": {"const": "local_file"},
                                        "path": {"type": "string", "description": "Path to command file (relative to workspace)"}
                                    },
                                    "required": ["type", "path"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": {"const": "inline"},
                                        "content": {"type": "string", "description": "Inline command content"}
                                    },
                                    "required": ["type", "content"]
                                }
                            ]
                        },
                        "trigger": {
                            "type": "object",
                            "description": "When to trigger this automation",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": {"const": "interval"},
                                        "seconds": {"type": "number", "description": "Interval in seconds"}
                                    },
                                    "required": ["type", "seconds"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": {"const": "agent_finished"}
                                    },
                                    "required": ["type"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": {"const": "webhook"},
                                        "config": {
                                            "type": "object",
                                            "properties": {
                                                "webhook_id": {"type": "string", "description": "Unique webhook ID (optional, generated if not provided)"},
                                                "secret": {"type": "string", "description": "Optional HMAC secret for webhook validation"},
                                                "variable_mappings": {
                                                    "type": "object",
                                                    "description": "Map webhook payload fields to variables"
                                                }
                                            }
                                        }
                                    },
                                    "required": ["type", "config"]
                                }
                            ]
                        },
                        "variables": {
                            "type": "object",
                            "description": "Variable substitutions (e.g., {'env': 'production'})",
                            "additionalProperties": {"type": "string"}
                        },
                        "retry_config": {
                            "type": "object",
                            "properties": {
                                "max_retries": {"type": "number", "description": "Maximum retry attempts"},
                                "retry_delay_seconds": {"type": "number", "description": "Initial delay between retries"},
                                "backoff_multiplier": {"type": "number", "description": "Exponential backoff multiplier"}
                            }
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "update_automation".to_string(),
                description: "Update an existing automation".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": {"type": "string", "description": "Automation ID to update"},
                        "command_source": {
                            "type": "object",
                            "description": "New command source (optional)"
                        },
                        "trigger": {
                            "type": "object",
                            "description": "New trigger configuration (optional)"
                        },
                        "variables": {
                            "type": "object",
                            "description": "New variables (optional)",
                            "additionalProperties": {"type": "string"}
                        },
                        "retry_config": {
                            "type": "object",
                            "description": "New retry configuration (optional)"
                        },
                        "active": {"type": "boolean", "description": "Enable or disable automation"}
                    }
                }),
            },
            ToolDefinition {
                name: "delete_automation".to_string(),
                description: "Delete an automation".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": {"type": "string", "description": "Automation ID to delete"}
                    }
                }),
            },
            ToolDefinition {
                name: "get_execution_history".to_string(),
                description: "Get execution history for automations in this mission".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "automation_id": {"type": "string", "description": "Filter by specific automation ID (optional)"},
                        "limit": {"type": "number", "description": "Maximum number of executions to return (default: 20)"}
                    }
                }),
            },
        ]
    }

    async fn list_automations(&self, params: ListAutomationsParams) -> Result<Value, String> {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/api/control/missions/{}/automations",
            self.api_url, self.mission_id
        );

        let mut request = client.get(&url);
        if let Some(ref token) = self.api_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("API returned error: {}", response.status()));
        }

        let mut automations: Vec<Automation> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        if params.active_only {
            automations.retain(|a| a.active);
        }

        Ok(serde_json::to_value(automations).unwrap())
    }

    async fn create_automation(&self, params: CreateAutomationParams) -> Result<Value, String> {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/api/control/missions/{}/automations",
            self.api_url, self.mission_id
        );

        let mut request = client.post(&url).json(&json!({
            "command_source": params.command_source,
            "trigger": params.trigger,
            "variables": params.variables,
            "retry_config": params.retry_config,
        }));

        if let Some(ref token) = self.api_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("API returned error: {}", error_text));
        }

        let automation: Automation = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        Ok(serde_json::to_value(automation).unwrap())
    }

    async fn update_automation(&self, params: UpdateAutomationParams) -> Result<Value, String> {
        let automation_id =
            Uuid::parse_str(&params.id).map_err(|_| "Invalid automation ID format".to_string())?;

        let client = reqwest::Client::new();
        let url = format!("{}/api/control/automations/{}", self.api_url, automation_id);

        let mut request = client.patch(&url).json(&json!({
            "command_source": params.command_source,
            "trigger": params.trigger,
            "variables": params.variables,
            "retry_config": params.retry_config,
            "active": params.active,
        }));

        if let Some(ref token) = self.api_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("API returned error: {}", error_text));
        }

        Ok(json!({"success": true}))
    }

    async fn delete_automation(&self, params: DeleteAutomationParams) -> Result<Value, String> {
        let automation_id =
            Uuid::parse_str(&params.id).map_err(|_| "Invalid automation ID format".to_string())?;

        let client = reqwest::Client::new();
        let url = format!("{}/api/control/automations/{}", self.api_url, automation_id);

        let mut request = client.delete(&url);
        if let Some(ref token) = self.api_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("API returned error: {}", error_text));
        }

        Ok(json!({"success": true}))
    }

    async fn get_execution_history(
        &self,
        params: GetExecutionHistoryParams,
    ) -> Result<Value, String> {
        let client = reqwest::Client::new();

        let url = if let Some(automation_id) = params.automation_id {
            let id = Uuid::parse_str(&automation_id)
                .map_err(|_| "Invalid automation ID format".to_string())?;
            format!(
                "{}/api/control/automations/{}/executions?limit={}",
                self.api_url, id, params.limit
            )
        } else {
            format!(
                "{}/api/control/missions/{}/automation-executions?limit={}",
                self.api_url, self.mission_id, params.limit
            )
        };

        let mut request = client.get(&url);
        if let Some(ref token) = self.api_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("API returned error: {}", response.status()));
        }

        let executions: Vec<AutomationExecution> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        Ok(serde_json::to_value(executions).unwrap())
    }

    async fn handle_call(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "list_automations" => {
                let params: ListAutomationsParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.list_automations(params).await
            }
            "create_automation" => {
                let params: CreateAutomationParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.create_automation(params).await
            }
            "update_automation" => {
                let params: UpdateAutomationParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.update_automation(params).await
            }
            "delete_automation" => {
                let params: DeleteAutomationParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.delete_automation(params).await
            }
            "get_execution_history" => {
                let params: GetExecutionHistoryParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.get_execution_history(params).await
            }
            _ => Err(format!("Unknown method: {}", method)),
        }
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => {
                let info = ServerInfo {
                    name: "automation-manager".to_string(),
                    version: "0.1.0".to_string(),
                };
                JsonRpcResponse::success(
                    req.id,
                    json!({
                        "protocolVersion": "2024-11-05",
                        "serverInfo": info,
                        "capabilities": {
                            "tools": {}
                        }
                    }),
                )
            }
            "tools/list" => {
                let tools = Self::get_tools();
                JsonRpcResponse::success(req.id, json!({ "tools": tools }))
            }
            "tools/call" => {
                let params = match req.params.as_object() {
                    Some(p) => p,
                    None => {
                        return JsonRpcResponse::error(
                            req.id,
                            -32602,
                            "Invalid params".to_string(),
                        );
                    }
                };
                let method = match params.get("name").and_then(|n| n.as_str()) {
                    Some(m) => m,
                    None => {
                        return JsonRpcResponse::error(
                            req.id,
                            -32602,
                            "Missing tool name".to_string(),
                        );
                    }
                };
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

                match self.handle_call(method, arguments).await {
                    Ok(result) => JsonRpcResponse::success(
                        req.id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string_pretty(&result).unwrap()
                            }]
                        }),
                    ),
                    Err(e) => JsonRpcResponse::error(req.id, -32000, e),
                }
            }
            _ => JsonRpcResponse::error(req.id, -32601, format!("Unknown method: {}", req.method)),
        }
    }
}

// =============================================================================
// Main
// =============================================================================

#[tokio::main]
async fn main() {
    // Read mission context from environment
    let mission_id = std::env::var("MISSION_ID")
        .ok()
        .and_then(|id| Uuid::parse_str(&id).ok())
        .expect("MISSION_ID environment variable not set or invalid");

    let api_url = std::env::var("API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    let api_token = std::env::var("API_TOKEN").ok();

    let server = Arc::new(AutomationManagerMcp::new(mission_id, api_url, api_token));

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin);

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
                let error_resp =
                    JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {}", e));
                if let Ok(json) = serde_json::to_string(&error_resp) {
                    writeln!(stdout, "{}", json).ok();
                }
                stdout.flush().ok();
                continue;
            }
        };

        let response = server.handle_request(request).await;
        if let Ok(json) = serde_json::to_string(&response) {
            writeln!(stdout, "{}", json).ok();
        } else {
            eprintln!("[automation-mcp] Failed to serialize response");
        }
        stdout.flush().ok();
    }
}

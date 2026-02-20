//! Variable substitution engine for automation commands.
//!
//! Supports placeholders like <timestamp/>, <mission_id/>, <webhook.data/>

use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

/// Built-in variable types that can be used in automation commands.
#[derive(Debug, Clone)]
pub enum BuiltinVariable {
    /// Current timestamp (RFC3339 format)
    Timestamp,
    /// Current date (YYYY-MM-DD)
    Date,
    /// Unix timestamp (seconds since epoch)
    UnixTime,
    /// Mission ID
    MissionId,
    /// Mission name (if available)
    MissionName,
    /// Working directory of the mission
    WorkingDirectory,
}

/// Context for variable substitution.
#[derive(Debug, Clone)]
pub struct SubstitutionContext {
    /// Mission ID for this execution
    pub mission_id: Uuid,
    /// Mission name (optional)
    pub mission_name: Option<String>,
    /// Working directory (optional)
    pub working_directory: Option<String>,
    /// Webhook payload data (optional)
    pub webhook_payload: Option<Value>,
    /// Custom variables provided by the automation
    pub custom_variables: HashMap<String, String>,
}

impl SubstitutionContext {
    /// Create a new context for a mission.
    pub fn new(mission_id: Uuid) -> Self {
        Self {
            mission_id,
            mission_name: None,
            working_directory: None,
            webhook_payload: None,
            custom_variables: HashMap::new(),
        }
    }

    /// Add webhook payload to the context.
    pub fn with_webhook_payload(mut self, payload: Value) -> Self {
        self.webhook_payload = Some(payload);
        self
    }

    /// Add mission name to the context.
    pub fn with_mission_name(mut self, name: String) -> Self {
        self.mission_name = Some(name);
        self
    }

    /// Add working directory to the context.
    pub fn with_working_directory(mut self, dir: String) -> Self {
        self.working_directory = Some(dir);
        self
    }

    /// Add custom variables to the context.
    pub fn with_custom_variables(mut self, vars: HashMap<String, String>) -> Self {
        self.custom_variables = vars;
        self
    }
}

/// Substitute variables in a command string.
///
/// Supports these placeholder formats:
/// - `<timestamp/>` - Current timestamp (RFC3339)
/// - `<date/>` - Current date (YYYY-MM-DD)
/// - `<unix_time/>` - Unix timestamp
/// - `<mission_id/>` - Mission UUID
/// - `<mission_name/>` - Mission name
/// - `<cwd/>` - Working directory
/// - `<webhook.path.to.field/>` - Access webhook payload fields
/// - `<custom_var_name/>` - Custom variables from automation config
pub fn substitute_variables(command: &str, context: &SubstitutionContext) -> String {
    let mut result = command.to_string();

    // Built-in time variables
    let now = Utc::now();
    result = result.replace("<timestamp/>", &now.to_rfc3339());
    result = result.replace("<date/>", &now.format("%Y-%m-%d").to_string());
    result = result.replace("<unix_time/>", &now.timestamp().to_string());

    // Mission variables
    result = result.replace("<mission_id/>", &context.mission_id.to_string());
    if let Some(ref name) = context.mission_name {
        result = result.replace("<mission_name/>", name);
    }
    if let Some(ref dir) = context.working_directory {
        result = result.replace("<cwd/>", dir);
    }

    // Webhook payload variables
    if let Some(ref payload) = context.webhook_payload {
        // Find all <webhook.xxx/> patterns
        let webhook_pattern = regex::Regex::new(r"<webhook\.([^/>]+)/>").unwrap();
        for cap in webhook_pattern.captures_iter(command) {
            let full_match = &cap[0];
            let path = &cap[1];

            // Try to access the field in the webhook payload
            if let Some(value) = access_json_path(payload, path) {
                result = result.replace(full_match, &value);
            }
        }
    }

    substitute_custom_variables(&result, &context.custom_variables)
}

/// Substitute only custom variables in a command string.
///
/// This is shared by mission command execution and automations so both flows
/// use identical `<var/>` replacement semantics.
pub fn substitute_custom_variables(command: &str, variables: &HashMap<String, String>) -> String {
    let mut result = command.to_string();
    for (key, value) in variables {
        let placeholder = format!("<{}/>", key);
        result = result.replace(&placeholder, value);
    }
    result
}

/// Apply variable mappings to webhook payload and return substitution context.
///
/// Variable mappings define how to extract values from webhook payload:
/// ```json
/// {
///   "repo": "repository.name",
///   "commit": "head_commit.id"
/// }
/// ```
pub fn apply_webhook_mappings(
    payload: &Value,
    mappings: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();

    for (var_name, json_path) in mappings {
        if let Some(value) = access_json_path(payload, json_path) {
            result.insert(var_name.clone(), value);
        }
    }

    result
}

/// Access a nested field in a JSON value using dot notation.
/// Example: "repository.name" or "head_commit.id"
fn access_json_path(value: &Value, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = value;

    for part in parts {
        current = current.get(part)?;
    }

    // Convert the final value to a string
    match current {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some("null".to_string()),
        Value::Array(_) | Value::Object(_) => Some(current.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_substitute_builtin_variables() {
        let context = SubstitutionContext::new(Uuid::new_v4())
            .with_mission_name("test-mission".to_string())
            .with_working_directory("/workspace/test".to_string());

        let command = "Check status at <timestamp/> in <cwd/> for mission <mission_name/>";
        let result = substitute_variables(command, &context);

        assert!(result.contains("test-mission"));
        assert!(result.contains("/workspace/test"));
        assert!(!result.contains("<timestamp/>"));
    }

    #[test]
    fn test_substitute_webhook_variables() {
        let payload = json!({
            "repository": {
                "name": "sandboxed.sh"
            },
            "head_commit": {
                "id": "abc123"
            }
        });

        let context = SubstitutionContext::new(Uuid::new_v4()).with_webhook_payload(payload);

        let command = "Analyze commit <webhook.head_commit.id/> in <webhook.repository.name/>";
        let result = substitute_variables(command, &context);

        assert_eq!(result, "Analyze commit abc123 in sandboxed.sh");
    }

    #[test]
    fn test_webhook_mappings() {
        let payload = json!({
            "repository": {
                "name": "test-repo"
            },
            "sender": {
                "login": "testuser"
            }
        });

        let mut mappings = HashMap::new();
        mappings.insert("repo".to_string(), "repository.name".to_string());
        mappings.insert("user".to_string(), "sender.login".to_string());

        let vars = apply_webhook_mappings(&payload, &mappings);

        assert_eq!(vars.get("repo"), Some(&"test-repo".to_string()));
        assert_eq!(vars.get("user"), Some(&"testuser".to_string()));
    }

    #[test]
    fn test_custom_variables() {
        let mut custom_vars = HashMap::new();
        custom_vars.insert("target_env".to_string(), "production".to_string());
        custom_vars.insert("version".to_string(), "v1.2.3".to_string());

        let context = SubstitutionContext::new(Uuid::new_v4()).with_custom_variables(custom_vars);

        let command = "Deploy <version/> to <target_env/>";
        let result = substitute_variables(command, &context);

        assert_eq!(result, "Deploy v1.2.3 to production");
    }

    #[test]
    fn test_substitute_custom_variables_only() {
        let mut custom_vars = HashMap::new();
        custom_vars.insert("service".to_string(), "api".to_string());
        custom_vars.insert("env".to_string(), "staging".to_string());
        let command = "Deploy <service/> to <env/> now";
        let result = substitute_custom_variables(command, &custom_vars);
        assert_eq!(result, "Deploy api to staging now");
    }
}

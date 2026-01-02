//! OpenCode API client (minimal adapter).
//!
//! Provides the minimal subset of OpenCode HTTP API needed to run tasks
//! via an external OpenCode server.

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;

#[derive(Clone)]
pub struct OpenCodeClient {
    base_url: String,
    client: reqwest::Client,
    default_agent: Option<String>,
    permissive: bool,
}

impl OpenCodeClient {
    pub fn new(base_url: impl Into<String>, default_agent: Option<String>, permissive: bool) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            base_url,
            client: reqwest::Client::new(),
            default_agent,
            permissive,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn create_session(&self, directory: &str, title: Option<&str>) -> anyhow::Result<OpenCodeSession> {
        let mut url = format!("{}/session", self.base_url);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }

        let mut body = serde_json::Map::new();
        if let Some(t) = title {
            body.insert("title".to_string(), json!(t));
        }
        if self.permissive {
            body.insert(
                "permission".to_string(),
                json!([{
                    "permission": "*",
                    "pattern": "*",
                    "action": "allow"
                }]),
            );
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to call OpenCode /session")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("OpenCode /session failed: {} - {}", status, text);
        }

        let session: OpenCodeSession = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse OpenCode session response: {}", text))?;
        Ok(session)
    }

    pub async fn send_message(
        &self,
        session_id: &str,
        directory: &str,
        content: &str,
        model: Option<&str>,
        agent: Option<&str>,
    ) -> anyhow::Result<OpenCodeMessageResponse> {
        let mut url = format!("{}/session/{}/message", self.base_url, session_id);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }

        let mut body = serde_json::Map::new();
        body.insert(
            "parts".to_string(),
            json!([{
                "type": "text",
                "text": content
            }]),
        );

        let agent_value = agent
            .map(|s| s.to_string())
            .or_else(|| self.default_agent.clone());
        if let Some(agent_name) = agent_value {
            body.insert("agent".to_string(), json!(agent_name));
        }

        if let Some(model_str) = model {
            if let Some((provider_id, model_id)) = split_model(model_str) {
                body.insert(
                    "model".to_string(),
                    json!({
                        "providerID": provider_id,
                        "modelID": model_id
                    }),
                );
            }
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to call OpenCode /session/{id}/message")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("OpenCode message failed: {} - {}", status, text);
        }

        let message: OpenCodeMessageResponse = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse OpenCode message response: {}", text))?;
        Ok(message)
    }

    pub async fn abort_session(&self, session_id: &str, directory: &str) -> anyhow::Result<()> {
        let mut url = format!("{}/session/{}/abort", self.base_url, session_id);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to call OpenCode /session/{id}/abort")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode abort failed: {} - {}", status, text);
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeSession {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeMessageResponse {
    pub info: OpenCodeAssistantInfo,
    pub parts: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeAssistantInfo {
    #[serde(default)]
    #[serde(rename = "providerID")]
    pub provider_id: Option<String>,
    #[serde(default)]
    #[serde(rename = "modelID")]
    pub model_id: Option<String>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

pub fn extract_text(parts: &[serde_json::Value]) -> String {
    let mut out = Vec::new();
    for part in parts {
        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                out.push(text.to_string());
            }
        }
    }
    out.join("\n")
}

fn split_model(model: &str) -> Option<(String, String)> {
    let trimmed = model.trim();
    let mut parts = trimmed.splitn(2, '/');
    let provider = parts.next()?.trim();
    let model_id = parts.next()?.trim();
    if provider.is_empty() || model_id.is_empty() {
        None
    } else {
        Some((provider.to_string(), model_id.to_string()))
    }
}

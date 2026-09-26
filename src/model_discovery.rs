//! Bounded, metadata-only provider discovery. No inference requests.
use crate::ai_providers::{AIProvider, ProviderType};
use crate::api::providers::ProviderModel;
use crate::model_catalog::{self as catalog, Completeness, DiscoveryState, Observation, Success};
use chrono::Utc;
use futures::{stream, StreamExt};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug)]
pub struct Route {
    pub base: String,
    pub profile: String,
    pub exportable: bool,
    pub adapter: &'static str,
}

pub fn route(p: &AIProvider) -> Route {
    let oauth = p.api_key.as_ref().is_none_or(|s| s.is_empty()) && p.oauth.is_some();
    let default =
        crate::api::proxy::default_base_url(p.provider_type).unwrap_or(match p.provider_type {
            ProviderType::Anthropic => "https://api.anthropic.com/v1",
            ProviderType::Google => "https://generativelanguage.googleapis.com/v1beta",
            _ => "",
        });
    let base = p
        .base_url
        .as_deref()
        .unwrap_or(default)
        .trim_end_matches('/')
        .to_string();
    let mut profile = if oauth { "oauth" } else { "api" }.to_string();
    if p.provider_type == ProviderType::Kimi {
        profile = "coding".into();
    }
    if p.provider_type == ProviderType::Zai && base == "https://api.z.ai/api/coding/paas/v4" {
        profile = "coding".into();
    }
    let standard = base == default
        || (p.provider_type == ProviderType::Zai && base == "https://api.z.ai/api/paas/v4");
    let exportable = standard && p.provider_type != ProviderType::Custom;
    if !standard {
        profile = "custom".into();
    }
    let adapter = if oauth && p.provider_type != ProviderType::Kimi {
        "unsupported_oauth"
    } else {
        match p.provider_type {
            ProviderType::Anthropic => "anthropic",
            ProviderType::Google => "google",
            ProviderType::AmazonBedrock
            | ProviderType::Azure
            | ProviderType::Cohere
            | ProviderType::GithubCopilot => "unsupported",
            _ if base.is_empty() => "unsupported",
            _ => "openai",
        }
    };
    Route {
        base,
        profile,
        exportable,
        adapter,
    }
}

/// A successful empty response is not allowed to wipe the last successful
/// observation. A partial response supplements, rather than replaces, fallback.
pub fn record_result(
    o: &mut Observation,
    result: Result<(Vec<ProviderModel>, Completeness), String>,
) {
    o.checked_at = Utc::now();
    match result {
        Ok((models, completeness)) if !models.is_empty() => {
            o.last_success = Some(Success {
                observed_at: o.checked_at,
                completeness,
                models,
            });
            o.status = "discovered".into();
            o.diagnostic = None;
        }
        Ok(_) => {
            o.status = "error".into();
            o.diagnostic = Some("empty_catalog".into());
        }
        Err(code) => {
            o.status = if code.starts_with("unsupported") {
                "unsupported"
            } else {
                "error"
            }
            .into();
            o.diagnostic = Some(code);
        }
    }
}

pub async fn refresh(root: &Path, providers: Vec<AIProvider>) -> Result<DiscoveryState, String> {
    let _guard = REFRESH_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let old = catalog::read_state(root);
    let observations = stream::iter(providers.into_iter().filter(|p| p.enabled).map(|p| {
        let r = route(&p);
        let connection = catalog::route_key(&p.id.to_string(), &r.base, &r.profile);
        let previous = old
            .connections
            .iter()
            .find(|o| o.connection == connection)
            .cloned();
        async move {
            let provider_id = if p.provider_type == ProviderType::Custom {
                crate::api::providers::sanitize_custom_provider_id(&p.name)
            } else {
                p.provider_type.id().into()
            };
            let fallback_models =
                if r.profile == "custom" || p.provider_type == ProviderType::Custom {
                    p.custom_models
                        .as_ref()
                        .map(|ms| {
                            ms.iter()
                                .map(|m| ProviderModel {
                                    id: m.id.clone(),
                                    name: m.name.clone().unwrap_or_else(|| m.id.clone()),
                                    description: None,
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    catalog::bundled_models(&provider_id, &r.profile)
                };
            let mut o = previous.unwrap_or(Observation {
                connection,
                provider_id,
                access_profile: r.profile.clone(),
                exportable: r.exportable,
                checked_at: Utc::now(),
                status: "unsupported".into(),
                diagnostic: None,
                last_success: None,
                fallback_models: vec![],
            });
            o.fallback_models = fallback_models;
            record_result(&mut o, discover(&p, &r).await);
            o
        }
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    let mut state = DiscoveryState {
        schema_version: 1,
        connections: observations,
    };
    state
        .connections
        .sort_by(|a, b| a.connection.cmp(&b.connection));
    catalog::write_state(root, &state)?;
    Ok(state)
}

fn is_text_model(provider: ProviderType, id: &str, entry: &Value) -> bool {
    if let Some(outputs) = entry
        .pointer("/architecture/output_modalities")
        .and_then(Value::as_array)
    {
        if !outputs.iter().any(|v| v.as_str() == Some("text")) {
            return false;
        }
    }
    match provider {
        ProviderType::OpenAI => {
            ["gpt-", "chatgpt-", "o1", "o3", "o4"]
                .iter()
                .any(|prefix| id.starts_with(prefix))
                && !["audio", "realtime", "image", "transcribe", "tts"]
                    .iter()
                    .any(|part| id.contains(part))
        }
        ProviderType::Xai => {
            id.starts_with("grok-") && !id.contains("image") && !id.contains("video")
        }
        _ => true,
    }
}

async fn discover(p: &AIProvider, r: &Route) -> Result<(Vec<ProviderModel>, Completeness), String> {
    if r.adapter.starts_with("unsupported") {
        return Err(r.adapter.into());
    }
    let key = p
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .or_else(|| p.oauth.as_ref().map(|o| o.access_token.as_str()))
        .unwrap_or("");
    if key.is_empty()
        && p.provider_type != ProviderType::Custom
        && p.provider_type != ProviderType::OpenRouter
    {
        return Err("missing_credentials".into());
    }
    let mut url =
        reqwest::Url::parse(&format!("{}/models", r.base)).map_err(|_| "invalid_endpoint")?;
    if url.username() != ""
        || url.password().is_some()
        || !["http", "https"].contains(&url.scheme())
    {
        return Err("invalid_endpoint".into());
    }
    if r.adapter == "anthropic" {
        url.query_pairs_mut().append_pair("limit", "100");
    }
    if r.adapter == "google" {
        url.query_pairs_mut().append_pair("pageSize", "1000");
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "client_error")?;
    let mut models = BTreeMap::new();
    let mut cursors = BTreeSet::new();
    for page in 0..10 {
        let mut request = client.get(url.clone());
        if r.adapter == "anthropic" {
            request = request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01");
        } else if r.adapter == "google" {
            request = request.header("x-goog-api-key", key);
        } else if !key.is_empty() {
            request = request.bearer_auth(key);
        }
        if p.provider_type == ProviderType::Kimi {
            request = request.header("User-Agent", crate::api::ai_providers::KIMI_USER_AGENT);
        }
        let mut response = request.send().await.map_err(|_| "transport_error")?;
        if !response.status().is_success() {
            return Err(format!("http_{}", response.status().as_u16()));
        }
        if response.content_length().is_some_and(|n| n > 8_000_000) {
            return Err("catalog_too_large".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "transport_error")? {
            if bytes.len() + chunk.len() > 8_000_000 {
                return Err("catalog_too_large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_json")?;
        let entries = body
            .get(if r.adapter == "google" {
                "models"
            } else {
                "data"
            })
            .and_then(Value::as_array)
            .ok_or("invalid_catalog")?;
        let mut malformed = false;
        for entry in entries {
            if r.adapter == "google"
                && !entry
                    .get("supportedGenerationMethods")
                    .and_then(Value::as_array)
                    .is_some_and(|methods| {
                        methods
                            .iter()
                            .any(|m| m.as_str() == Some("generateContent"))
                    })
            {
                continue;
            }
            let id = entry
                .get(if r.adapter == "google" { "name" } else { "id" })
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_start_matches("models/");
            if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                malformed = true;
                continue;
            }
            if !is_text_model(p.provider_type, id, entry) {
                continue;
            }
            let name = entry
                .get("display_name")
                .or_else(|| entry.get("displayName"))
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(id);
            models.insert(
                id.to_string(),
                ProviderModel {
                    id: id.into(),
                    name: name.chars().take(256).collect(),
                    description: None,
                },
            );
        }
        if malformed {
            return Err("invalid_model_entry".into());
        }
        let cursor = if r.adapter == "google" {
            body.get("nextPageToken").and_then(Value::as_str)
        } else if body.get("has_more").and_then(Value::as_bool) == Some(true) {
            body.get("last_id").and_then(Value::as_str).or_else(|| {
                entries
                    .last()
                    .and_then(|e| e.get("id"))
                    .and_then(Value::as_str)
            })
        } else {
            None
        };
        if body.get("has_more").and_then(Value::as_bool) == Some(true) && cursor.is_none() {
            return Ok((models.into_values().collect(), Completeness::Partial));
        }
        if let Some(cursor) = cursor {
            if page == 9 || !cursors.insert(cursor.to_string()) {
                return Ok((models.into_values().collect(), Completeness::Partial));
            }
            let key = if r.adapter == "google" {
                "pageToken"
            } else {
                "after_id"
            };
            let pairs: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(k, _)| k != key)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            url.set_query(None);
            url.query_pairs_mut()
                .extend_pairs(pairs)
                .append_pair(key, cursor);
            continue;
        }
        // OpenAI-compatible schemas lack a completeness contract in general.
        // Kimi and custom routers have an established authoritative list.
        let complete = r.adapter == "anthropic"
            || r.adapter == "google"
            || matches!(
                p.provider_type,
                ProviderType::OpenAI
                    | ProviderType::Kimi
                    | ProviderType::Custom
                    | ProviderType::OpenRouter
            );
        return Ok((
            models.into_values().collect(),
            if complete {
                Completeness::Complete
            } else {
                Completeness::Partial
            },
        ));
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::Query, http::HeaderMap, routing::get, Json, Router};
    async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, task)
    }
    #[test]
    fn non_text_models_are_not_chat_suggestions() {
        let empty = serde_json::json!({});
        assert!(is_text_model(ProviderType::OpenAI, "gpt-6-astra", &empty));
        for id in [
            "text-embedding-3-large",
            "gpt-image-1",
            "gpt-4o-realtime-preview",
        ] {
            assert!(!is_text_model(ProviderType::OpenAI, id, &empty));
        }
        assert!(!is_text_model(
            ProviderType::Xai,
            "grok-imagine-video",
            &empty
        ));
        assert!(!is_text_model(
            ProviderType::OpenRouter,
            "image-only",
            &serde_json::json!({"architecture":{"output_modalities":["image"]}})
        ));
    }
    #[test]
    fn zai_uses_inference_endpoint() {
        let mut p = AIProvider::new(ProviderType::Zai, "test".into());
        assert_eq!(route(&p).base, "https://api.z.ai/api/coding/paas/v4");
        assert_eq!(route(&p).profile, "coding");
        p.base_url = Some("https://api.z.ai/api/paas/v4".into());
        assert_eq!(route(&p).profile, "api");
        assert!(route(&p).exportable);
        p.base_url = Some("https://private.example/v1".into());
        assert!(!route(&p).exportable);
        assert_eq!(route(&p).profile, "custom");
    }
    #[tokio::test]
    async fn paginates_and_isolates_accounts() {
        let (url,task)=server(Router::new().route("/v1/models",get(|headers:HeaderMap,Query(query):Query<BTreeMap<String,String>>|async move {
            let key=headers.get("authorization").unwrap().to_str().unwrap();
            let model=if key=="Bearer account-a" {"a"} else {"b"};
            let more=!query.contains_key("after_id");
            Json(serde_json::json!({"data":[{"id":format!("{model}-{}",if more {1}else{2})}],"has_more":more,"last_id":"page-1"}))
        }))).await;
        let root = tempfile::tempdir().unwrap();
        let mut a = AIProvider::new(ProviderType::Custom, "Router".into());
        a.base_url = Some(format!("{url}/v1"));
        a.api_key = Some("account-a".into());
        let mut b = a.clone();
        b.id = uuid::Uuid::new_v4();
        b.api_key = Some("account-b".into());
        let state = refresh(root.path(), vec![a, b]).await.unwrap();
        assert_eq!(state.connections.len(), 2);
        for o in state.connections {
            let models = o.last_success.unwrap().models;
            assert_eq!(models.len(), 2);
            assert_eq!(models[0].id.chars().next(), models[1].id.chars().next());
        }
        task.abort();
    }
    #[tokio::test]
    async fn failed_refresh_preserves_cache_and_removed_accounts_disappear() {
        let (url, task) = server(Router::new().route(
            "/models",
            get(|| async { Json(serde_json::json!({"data":[{"id":"live"}]})) }),
        ))
        .await;
        let root = tempfile::tempdir().unwrap();
        let mut p = AIProvider::new(ProviderType::Custom, "Router".into());
        p.base_url = Some(url);
        assert_eq!(
            refresh(root.path(), vec![p.clone()])
                .await
                .unwrap()
                .connections[0]
                .status,
            "discovered"
        );
        task.abort();
        let _ = task.await;
        let second = refresh(root.path(), vec![p]).await.unwrap();
        assert_eq!(second.connections[0].status, "error");
        assert_eq!(second.connections[0].effective_models()[0].id, "live");
        assert_eq!(second.connections[0].effective_source(), "stale_discovery");
        assert!(refresh(root.path(), vec![])
            .await
            .unwrap()
            .connections
            .is_empty());
    }
    #[tokio::test]
    async fn no_credential_forwarding_on_redirect() {
        let (url, task) = server(Router::new().route(
            "/models",
            get(|| async {
                axum::response::Redirect::temporary("https://untrusted.invalid/models")
            }),
        ))
        .await;
        let mut p = AIProvider::new(ProviderType::Custom, "Router".into());
        p.base_url = Some(url);
        p.api_key = Some("secret".into());
        let error = discover(&p, &route(&p)).await.unwrap_err();
        assert_eq!(error, "http_307");
        assert!(!error.contains("secret"));
        task.abort();
    }
}

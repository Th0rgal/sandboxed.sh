//! Project-scoped replication grants, signed with a key distinct from login JWTs.
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
struct Grant {
    project: String,
    node: String,
    exp: usize,
}
fn key(config: &crate::config::Config) -> Result<String, String> {
    let secret = config
        .auth
        .jwt_secret
        .as_deref()
        .ok_or("Context sync requires JWT_SECRET")?;
    Ok(crate::project_context::digest(
        format!("sandboxed-context-grant-v1:{secret}").as_bytes(),
    ))
}
pub fn issue(config: &crate::config::Config, project: &str, node: &str) -> Result<String, String> {
    let claims = Grant {
        project: project.into(),
        node: node.into(),
        exp: (chrono::Utc::now() + chrono::Duration::days(30)).timestamp() as usize,
    };
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(key(config)?.as_bytes()),
    )
    .map(|token| format!("ctx1.{token}"))
    .map_err(|e| e.to_string())
}
pub fn permits(
    config: &crate::config::Config,
    token: &str,
    path: &str,
    method: &axum::http::Method,
) -> bool {
    let Some(token) = token.strip_prefix("ctx1.") else {
        return false;
    };
    let Ok(key) = key(config) else {
        return false;
    };
    let Ok(data) = jsonwebtoken::decode::<Grant>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(key.as_bytes()),
        &jsonwebtoken::Validation::default(),
    ) else {
        return false;
    };
    if !config
        .remote_nodes
        .nodes
        .iter()
        .any(|node| node.id == data.claims.node)
    {
        return false;
    }
    let prefix = format!("/api/projects/{}/context/", data.claims.project);
    let Some(route) = path.strip_prefix(&prefix) else {
        return false;
    };
    match *method {
        axum::http::Method::GET => {
            matches!(route, "manifest" | "conflicts")
                || route.strip_prefix("blobs/").is_some_and(|hash| {
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                })
        }
        axum::http::Method::POST => matches!(route, "blobs" | "operations"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthConfig, Config, ContextConfig};
    use std::path::PathBuf;
    fn config() -> Config {
        Config {
            default_model: None,
            working_dir: PathBuf::from("/tmp"),
            host: "127.0.0.1".to_string(),
            port: 3000,
            max_iterations: 50,
            stale_mission_hours: 0,
            max_parallel_missions: 1,
            dev_mode: false,
            auth: AuthConfig::default(),
            context: ContextConfig::default(),
            opencode_base_url: "http://127.0.0.1:4096".to_string(),
            opencode_agent: None,
            opencode_permissive: false,
            library_path: PathBuf::from("/tmp/library"),
            default_backend: None,
            automations_enabled: true,
            paloma_webhook_forward_url: None,
            paloma_webhook_secret: None,
            max_concurrent_tasks: 5,
            spark_arbiter_url: None,
            spark_arbiter_token: None,
            spark_ssh_target: None,
            remote_nodes: crate::remote_node::RemoteNodeSettings::default(),
        }
    }

    #[test]
    fn context_grants_are_project_scoped_and_revoked_with_node() {
        let mut config = config();
        config.auth.jwt_secret = Some("test-context-secret".into());
        config
            .remote_nodes
            .nodes
            .push(crate::remote_node::RemoteNodeConfig {
                id: "node".into(),
                base_url: "http://node".into(),
                token_env: "NOT_READ".into(),
                labels: None,
            });
        let token = issue(&config, "notes", "node").unwrap();
        use axum::http::Method;
        assert!(permits(
            &config,
            &token,
            "/api/projects/notes/context/manifest",
            &Method::GET
        ));
        assert!(permits(
            &config,
            &token,
            "/api/projects/notes/context/operations",
            &Method::POST
        ));
        for path in [
            "/api/projects/other/context/manifest",
            "/api/control/local-origins",
            "/api/projects/notes/context/history",
            "/api/projects/notes/context/../file",
        ] {
            assert!(!permits(&config, &token, path, &Method::GET));
        }
        assert!(!permits(
            &config,
            &token,
            "/api/projects/notes/context/manifest",
            &Method::DELETE
        ));
        config.remote_nodes.nodes.clear();
        assert!(!permits(
            &config,
            &token,
            "/api/projects/notes/context/manifest",
            &Method::GET
        ));
    }
}

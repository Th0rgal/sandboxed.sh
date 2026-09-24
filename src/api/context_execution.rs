use crate::project_context::Manifest;
use std::path::Path;
pub fn has_mentions(text: &str) -> bool {
    regex::Regex::new(r#"(^|[\s(])@(?:\")?context(?:[/\s\"),.;!?]|$)"#)
        .unwrap()
        .is_match(text)
}
pub fn resolve(text: &str, root: &Path, manifest: &Manifest) -> Result<String, String> {
    let pattern =
        regex::Regex::new(r#"(^|[\s(])@(?:\"(context(?:/[^\"]*)?)\"|(context(?:/[^\s)\]},;]*)?))"#)
            .unwrap();
    let mut result = String::new();
    let mut last = 0;
    for captures in pattern.captures_iter(text) {
        let whole = captures.get(0).unwrap();
        if whole.end() < text.len()
            && text[whole.end()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let raw = captures
            .get(2)
            .or_else(|| captures.get(3))
            .unwrap()
            .as_str();
        let value = if captures.get(2).is_some() {
            raw
        } else {
            raw.trim_end_matches(['.', ',', ';', ':', '!', '?'])
        };
        let relative = value
            .strip_prefix("context/")
            .unwrap_or("")
            .trim_end_matches('/');
        if value != "context" && !value.starts_with("context/") {
            continue;
        }
        if !relative.is_empty() {
            crate::project_context::valid_path(relative)?;
            if !manifest.entries.contains_key(relative) {
                return Err(format!("Context path not found: {relative}"));
            }
        }
        result.push_str(&text[last..whole.start()]);
        result.push_str(&captures[1]);
        result.push_str(
            &serde_json::to_string(&root.join(relative).to_string_lossy())
                .map_err(|e| e.to_string())?,
        );
        result.push_str(&raw[value.len()..]);
        last = whole.end();
    }
    result.push_str(&text[last..]);
    Ok(result)
}
pub async fn remote(
    state: &super::routes::AppState,
    project: &str,
    node: &crate::remote_node::RemoteNodeConfig,
    text: &str,
) -> Result<String, String> {
    if !has_mentions(text) {
        return Ok(text.into());
    }
    if !super::projects_overview::is_plain_key(project) {
        return Err("Invalid context project".into());
    }
    let root = super::mission_payload::project_files_root(&state.config.working_dir, project);
    let metadata = state
        .config
        .working_dir
        .join(".sandboxed-sh/project-context-state")
        .join(project);
    crate::project_context::Store::new(root, metadata).manifest()?;
    let endpoint = super::mission_runner::public_api_base_url_from_env()
        .ok_or("Remote context requires SANDBOXED_PUBLIC_URL")?;
    let token = super::context_auth::issue(&state.config, project, &node.id)?;
    let node_token = std::env::var(&node.token_env).map_err(|_| "Node credentials unavailable")?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(format!(
            "{}/project-context/prepare",
            node.base_url.trim_end_matches('/')
        ))
        .bearer_auth(node_token)
        .json(&crate::node::project_context::Request {
            endpoint,
            token,
            project: project.into(),
        })
        .send()
        .await
        .map_err(|_| "Context preparation could not reach the node")?;
    if !response.status().is_success() {
        return Err(format!("Node context preparation failed (HTTP {}); update the node if this capability is missing",response.status().as_u16()));
    }
    #[derive(serde::Deserialize)]
    struct Prepared {
        root: std::path::PathBuf,
        manifest: Manifest,
    }
    let prepared: Prepared = response.json().await.map_err(|e| e.to_string())?;
    resolve(text, &prepared.root, &prepared.manifest)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolution_is_bound_to_context_tokens() {
        let m = Manifest::default();
        assert_eq!(
            resolve("Read @context.", Path::new("/srv/context"), &m).unwrap(),
            "Read \"/srv/context/\"."
        );
        assert_eq!(
            resolve("a@context.test", Path::new("/x"), &m).unwrap(),
            "a@context.test"
        );
        assert!(resolve("@context/missing", Path::new("/x"), &m).is_err());
        assert!(resolve("@context/../escape", Path::new("/x"), &m).is_err());
    }
}

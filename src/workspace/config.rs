//! Per-backend workspace config generation.
//!
//! Moved verbatim from `workspace.rs` (Phase 4 of the decomposition):
//! `write_backend_config` and the OpenCode/Claude Code/Codex writers it
//! dispatches to, plus their JSON/TOML entry builders.
//!
//! File writes go through [`write_file_atomic`] so a crash or concurrent
//! reader never observes a half-written config.

use super::*;

/// Write `contents` to `path` atomically: write to a sibling `.tmp` file,
/// then rename over the target. Renames within one directory are atomic on
/// POSIX, so readers (harness CLIs, nspawn binds) see either the old or the
/// new config — never a torn one.
pub(crate) fn write_file_atomic(
    path: &std::path::Path,
    contents: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    let tmp = path.with_extension(
        path.extension()
            .map(|e| format!("{}.tmp", e.to_string_lossy()))
            .unwrap_or_else(|| "tmp".to_string()),
    );
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

fn claude_entry_from_mcp(
    config: &McpServerConfig,
    workspace_dir: &Path,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    shared_network: Option<bool>,
) -> serde_json::Value {
    match &config.transport {
        McpTransport::Http { endpoint, headers } => {
            let mut entry = serde_json::Map::new();
            entry.insert("type".to_string(), json!("http"));
            entry.insert("url".to_string(), json!(endpoint));
            if !headers.is_empty() {
                entry.insert("headers".to_string(), json!(headers));
            }
            serde_json::Value::Object(entry)
        }
        McpTransport::Stdio { .. } => {
            let opencode_entry = opencode_entry_from_mcp(
                config,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
            );

            let command_vec = opencode_entry
                .get("command")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let command = command_vec
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let args: Vec<String> = command_vec
                .iter()
                .skip(1)
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();

            let mut entry = serde_json::Map::new();
            entry.insert("command".to_string(), json!(command));
            entry.insert("args".to_string(), json!(args));

            if let Some(env) = opencode_entry
                .get("environment")
                .and_then(|v| v.as_object())
            {
                entry.insert("env".to_string(), serde_json::Value::Object(env.clone()));
            }

            serde_json::Value::Object(entry)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_opencode_config(
    workspace_dir: &Path,
    mcp_configs: Vec<McpServerConfig>,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    skill_allowlist: Option<&[String]>,
    command_contents: Option<&[CommandContent]>,
    shared_network: Option<bool>,
    custom_providers: Option<&[AIProvider]>,
) -> anyhow::Result<()> {
    let mut mcp_map = serde_json::Map::new();
    let mut used = std::collections::HashSet::new();
    let has_desktop_mcp = mcp_configs
        .iter()
        .any(|config| config.enabled && config.name == "desktop");

    let filtered_configs = mcp_configs.into_iter().filter(|c| {
        if !c.enabled {
            return false;
        }
        true
    });

    for config in filtered_configs {
        let base = sanitize_key(&config.name);
        let key = unique_key(&base, &mut used);
        mcp_map.insert(
            key,
            opencode_entry_from_mcp(
                &config,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
            ),
        );
    }

    let mut permission = serde_json::Map::new();
    permission.insert("read".to_string(), json!("allow"));
    permission.insert("edit".to_string(), json!("allow"));
    permission.insert("glob".to_string(), json!("allow"));
    permission.insert("grep".to_string(), json!("allow"));
    permission.insert("list".to_string(), json!("allow"));
    permission.insert("bash".to_string(), json!("allow"));
    permission.insert("task".to_string(), json!("allow"));
    permission.insert("external_directory".to_string(), json!("allow"));
    permission.insert("todowrite".to_string(), json!("allow"));
    permission.insert("todoread".to_string(), json!("allow"));
    permission.insert("question".to_string(), json!("allow"));
    permission.insert("webfetch".to_string(), json!("allow"));
    permission.insert("websearch".to_string(), json!("allow"));
    permission.insert("codesearch".to_string(), json!("allow"));
    permission.insert("lsp".to_string(), json!("allow"));
    permission.insert("doom_loop".to_string(), json!("allow"));

    if let Some(skills) = skill_allowlist {
        if !skills.is_empty() {
            let mut skill_permissions = serde_json::Map::new();
            skill_permissions.insert("*".to_string(), json!("deny"));
            for skill in skills {
                skill_permissions.insert(skill.clone(), json!("allow"));
            }
            permission.insert(
                "skill".to_string(),
                serde_json::Value::Object(skill_permissions),
            );
        }
    }
    let workspace_desktop_flag = workspace_env
        .get("SANDBOXED_SH_ENABLE_DESKTOP_TOOLS")
        .or_else(|| workspace_env.get("DESKTOP_ENABLED"))
        .map(|value| {
            matches!(
                value.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "y" | "on"
            )
        })
        .unwrap_or(false);
    let workspace_has_display = workspace_env.contains_key("DISPLAY");

    // Tool policy:
    // - We want shell/file effects scoped to the workspace by running the agent process
    //   inside the workspace execution context (host/container).
    // - Therefore, OpenCode built-in bash MUST be enabled for all workspace types.
    // - The legacy workspace-mcp/desktop-mcp proxy tools are no longer required for core flows.
    // - Enable desktop tools automatically when a desktop MCP exists or the workspace advertises
    //   a display (browser/X11 templates), even if global env flags are unset.
    let enable_desktop_tools = env_var_bool("SANDBOXED_SH_ENABLE_DESKTOP_TOOLS", false)
        || env_var_bool("DESKTOP_ENABLED", false)
        || workspace_desktop_flag
        || workspace_has_display
        || has_desktop_mcp;
    let container_fallback = super::container_fallback_from_env(workspace_env);
    let per_workspace_runner = env_var_bool("SANDBOXED_SH_PER_WORKSPACE_RUNNER", true);
    let mut tools = serde_json::Map::new();
    match workspace_type {
        WorkspaceType::Container => {
            // Container workspace: OpenCode runs inside the container, so built-in bash is safe.
            tools.insert("Bash".to_string(), json!(true));
            tools.insert("bash".to_string(), json!(true));
            // Disable legacy MCP tool namespaces by default.
            tools.insert("workspace_*".to_string(), json!(false));
            tools.insert(
                "desktop_*".to_string(),
                json!(enable_desktop_tools && (container_fallback || per_workspace_runner)),
            );
            tools.insert("playwright_*".to_string(), json!(true));
            tools.insert("browser_*".to_string(), json!(true));
        }
        WorkspaceType::Host => {
            tools.insert("Bash".to_string(), json!(true));
            tools.insert("bash".to_string(), json!(true));
            tools.insert("workspace_*".to_string(), json!(false));
            tools.insert("desktop_*".to_string(), json!(enable_desktop_tools));
            tools.insert("playwright_*".to_string(), json!(false));
            tools.insert("browser_*".to_string(), json!(false));
        }
    }
    let mut base_config = serde_json::json!({});
    let base_dir = resolve_opencode_config_dir();
    let base_path = base_dir.join("opencode.json");
    let base_jsonc = base_dir.join("opencode.jsonc");
    let base_contents = if base_path.exists() {
        tokio::fs::read_to_string(&base_path).await.ok()
    } else if base_jsonc.exists() {
        tokio::fs::read_to_string(&base_jsonc).await.ok()
    } else {
        None
    };

    if let Some(contents) = base_contents {
        match serde_json::from_str::<serde_json::Value>(&contents) {
            Ok(value) => base_config = value,
            Err(_) => {
                let stripped = strip_jsonc_comments(&contents);
                match serde_json::from_str::<serde_json::Value>(&stripped) {
                    Ok(value) => base_config = value,
                    Err(e) => {
                        tracing::warn!("Failed to parse OpenCode base config: {}", e);
                    }
                }
            }
        }
    }

    if !base_config.is_object() {
        base_config = serde_json::json!({});
    }

    {
        let base_obj = base_config.as_object_mut().expect("opencode base config");
        base_obj.insert(
            "$schema".to_string(),
            json!("https://opencode.ai/config.json"),
        );
        base_obj.insert("mcp".to_string(), serde_json::Value::Object(mcp_map));
        base_obj.insert(
            "permission".to_string(),
            serde_json::Value::Object(permission),
        );
        base_obj.insert("tools".to_string(), serde_json::Value::Object(tools));

        // Add custom providers if any. Kimi is handled here too: like Custom
        // providers it is not an OpenCode built-in, so it needs an explicit
        // provider block (OpenAI-compatible npm package + base URL + creds).
        if let Some(providers) = custom_providers {
            let provider_blocks: Vec<_> = providers
                .iter()
                .filter(|p| {
                    matches!(p.provider_type, ProviderType::Custom | ProviderType::Kimi)
                        && p.enabled
                })
                .collect();

            if !provider_blocks.is_empty() {
                let mut provider_map = serde_json::Map::new();

                for provider in provider_blocks {
                    // Kimi: OpenAI-compatible block with the OAuth access token as
                    // the bearer key and the Kimi CLI User-Agent (the coding
                    // endpoint 403s without a known coding-agent UA).
                    if provider.provider_type == ProviderType::Kimi {
                        let mut provider_config = serde_json::Map::new();
                        provider_config
                            .insert("npm".to_string(), json!("@ai-sdk/openai-compatible"));
                        provider_config.insert("name".to_string(), json!("Kimi"));

                        let mut options = serde_json::Map::new();
                        options.insert(
                            "baseURL".to_string(),
                            json!(crate::api::ai_providers::KIMI_API_BASE_URL),
                        );
                        if let Some(oauth) = &provider.oauth {
                            if !oauth.access_token.trim().is_empty() {
                                options.insert("apiKey".to_string(), json!(oauth.access_token));
                            }
                        }
                        let mut headers = serde_json::Map::new();
                        headers.insert("User-Agent".to_string(), json!("KimiCLI/1.5"));
                        options.insert("headers".to_string(), serde_json::Value::Object(headers));
                        provider_config
                            .insert("options".to_string(), serde_json::Value::Object(options));

                        let mut models_map = serde_json::Map::new();
                        for (model_id, model_name) in [
                            ("kimi-for-coding", "Kimi for Coding"),
                            ("kimi-k2.6", "Kimi K2.6"),
                            ("kimi-k2-thinking", "Kimi K2 Thinking"),
                        ] {
                            let mut model_config = serde_json::Map::new();
                            model_config.insert("name".to_string(), json!(model_name));
                            models_map.insert(
                                model_id.to_string(),
                                serde_json::Value::Object(model_config),
                            );
                        }
                        provider_config
                            .insert("models".to_string(), serde_json::Value::Object(models_map));

                        provider_map.insert(
                            "kimi".to_string(),
                            serde_json::Value::Object(provider_config),
                        );
                        continue;
                    }

                    let provider_id = sanitize_key(&provider.name);
                    let mut provider_config = serde_json::Map::new();

                    // Set npm package (default to openai-compatible)
                    let npm = provider
                        .npm_package
                        .as_deref()
                        .unwrap_or("@ai-sdk/openai-compatible");
                    provider_config.insert("npm".to_string(), json!(npm));

                    // Set provider name
                    provider_config.insert("name".to_string(), json!(&provider.name));

                    // Build options
                    let mut options = serde_json::Map::new();
                    if let Some(base_url) = &provider.base_url {
                        options.insert("baseURL".to_string(), json!(base_url));
                    }

                    // API key: either direct value or env var reference
                    if let Some(api_key) = &provider.api_key {
                        options.insert("apiKey".to_string(), json!(api_key));
                    } else if let Some(env_var) = &provider.custom_env_var {
                        options.insert("apiKey".to_string(), json!(format!("{{env:{}}}", env_var)));
                    }
                    // API key is optional - some providers may not need it

                    if !options.is_empty() {
                        provider_config
                            .insert("options".to_string(), serde_json::Value::Object(options));
                    }

                    // Build models config
                    if let Some(models) = &provider.custom_models {
                        let mut models_map = serde_json::Map::new();
                        for model in models {
                            let mut model_config = serde_json::Map::new();

                            if let Some(name) = &model.name {
                                model_config.insert("name".to_string(), json!(name));
                            }

                            // Build limit config if either limit is set
                            if model.context_limit.is_some() || model.output_limit.is_some() {
                                let mut limit = serde_json::Map::new();
                                if let Some(context) = model.context_limit {
                                    limit.insert("context".to_string(), json!(context));
                                }
                                if let Some(output) = model.output_limit {
                                    limit.insert("output".to_string(), json!(output));
                                }
                                model_config
                                    .insert("limit".to_string(), serde_json::Value::Object(limit));
                            }

                            models_map
                                .insert(model.id.clone(), serde_json::Value::Object(model_config));
                        }
                        if !models_map.is_empty() {
                            provider_config.insert(
                                "models".to_string(),
                                serde_json::Value::Object(models_map),
                            );
                        }
                    }

                    provider_map.insert(provider_id, serde_json::Value::Object(provider_config));
                }

                if !provider_map.is_empty() {
                    base_obj.insert(
                        "provider".to_string(),
                        serde_json::Value::Object(provider_map),
                    );
                }
            }
        }
    }

    let config_value = base_config;
    let config_payload = serde_json::to_string_pretty(&config_value)?;

    // Write to workspace root
    let config_path = workspace_dir.join("opencode.json");
    write_file_atomic(&config_path, &config_payload)?;

    // Also write to .opencode/ for OpenCode config discovery
    let opencode_dir = workspace_dir.join(".opencode");
    tokio::fs::create_dir_all(&opencode_dir).await?;
    let opencode_config_path = opencode_dir.join("opencode.json");
    write_file_atomic(&opencode_config_path, config_payload)?;

    // Write commands as skills for OpenCode (since OpenCode doesn't have a separate command system)
    if let Some(commands) = command_contents {
        write_commands_as_opencode_skills(workspace_dir, commands).await?;
    }

    // Write Claude PreToolUse hooks for Claude-compatible execution.
    // These fix gh CLI hanging in PTY and block oversized image Reads before
    // provider submission.
    if let Some(hooks) =
        write_claude_pretool_hooks(workspace_dir, workspace_root, workspace_type).await?
    {
        let claude_dir = workspace_dir.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await?;
        let settings = json!({ "hooks": hooks });
        let settings_content = serde_json::to_string_pretty(&settings)?;
        let settings_path = claude_dir.join("settings.local.json");
        write_file_atomic(&settings_path, &settings_content)?;
        tracing::info!("Claude hooks written to .claude/settings.local.json for OpenCode backend");
    }

    Ok(())
}

/// Write Claude Code `PreToolUse` hooks for workspace execution.
///
/// The Bash hook always exists because it fixes `gh` hanging in PTY contexts.
///
/// The Read hook blocks oversized image reads before Claude Code serializes the
/// image into the next model request. Anthropic applies a 2000px per-dimension
/// limit when a request contains many images; one oversized screenshot can poison
/// the session context and make the next model call fail before the agent gets a
/// chance to recover.
/// Returns the `hooks` JSON value to embed in `.claude/settings.local.json`,
/// or `None` when no hooks were written.
///
/// For container workspaces, paths in the hook config are translated to
/// container-relative paths.
///
/// For the OpenCode backend this also keeps Claude-compatible tool hooks available.
async fn write_claude_pretool_hooks(
    workspace_dir: &Path,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
) -> anyhow::Result<Option<serde_json::Value>> {
    let is_container = workspace_type == WorkspaceType::Container && nspawn::nspawn_available();

    // Write the Bash hook script to .claude/hooks/bash-pretool.sh.
    // See `render_bash_pretool_script` for the script body.
    let hooks_dir = workspace_dir.join(".claude").join("hooks");
    tokio::fs::create_dir_all(&hooks_dir).await?;
    let hook_path = hooks_dir.join("bash-pretool.sh");
    let hook_script = render_bash_pretool_script();
    write_file_atomic(&hook_path, &hook_script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    // For container workspaces, translate the hook path from host to container-relative
    let hook_command = if is_container {
        if let Ok(rel) = hook_path.strip_prefix(workspace_root) {
            format!("/{}", rel.to_string_lossy())
        } else {
            hook_path.to_string_lossy().to_string()
        }
    } else {
        hook_path.to_string_lossy().to_string()
    };
    tracing::info!(
        hook_path = %hook_command,
        is_container = is_container,
        "Bash PreToolUse hook written"
    );

    let image_hook_path = hooks_dir.join("image-read-pretool.sh");
    let image_hook_script = r#"#!/bin/bash
# PreToolUse hook for Read. Blocks oversized PNG/JPEG images before Claude Code
# embeds them in a provider request that may contain many images.
set -euo pipefail

INPUT=$(cat)

if ! command -v python3 >/dev/null 2>&1; then
  exit 0
fi

export CLAUDE_HOOK_INPUT="$INPUT"
python3 <<'PY'
import json
import os
import struct
import sys

MAX_DIMENSION = 2000

def png_dimensions(data):
    if len(data) >= 24 and data.startswith(b"\x89PNG\r\n\x1a\n"):
        return struct.unpack(">II", data[16:24])
    return None

def jpeg_dimensions(path):
    with open(path, "rb") as f:
        if f.read(2) != b"\xff\xd8":
            return None
        while True:
            marker_prefix = f.read(1)
            if not marker_prefix:
                return None
            if marker_prefix != b"\xff":
                continue
            marker = f.read(1)
            while marker == b"\xff":
                marker = f.read(1)
            if not marker:
                return None
            code = marker[0]
            if code in (0xD8, 0xD9):
                continue
            length_bytes = f.read(2)
            if len(length_bytes) != 2:
                return None
            length = struct.unpack(">H", length_bytes)[0]
            if length < 2:
                return None
            if code in {
                0xC0, 0xC1, 0xC2, 0xC3,
                0xC5, 0xC6, 0xC7,
                0xC9, 0xCA, 0xCB,
                0xCD, 0xCE, 0xCF,
            }:
                segment = f.read(length - 2)
                if len(segment) >= 5:
                    height, width = struct.unpack(">HH", segment[1:5])
                    return width, height
                return None
            f.seek(length - 2, os.SEEK_CUR)

def image_dimensions(path):
    try:
        with open(path, "rb") as f:
            head = f.read(32)
        dims = png_dimensions(head)
        if dims:
            return dims
        return jpeg_dimensions(path)
    except Exception:
        return None

def main():
    try:
        payload = json.loads(os.environ.get("CLAUDE_HOOK_INPUT", "{}"))
    except Exception:
        return 0

    tool_input = payload.get("tool_input") or {}
    path = tool_input.get("file_path") or tool_input.get("path")
    if not isinstance(path, str) or not path:
        return 0
    if not os.path.isfile(path):
        return 0

    dims = image_dimensions(path)
    if not dims:
        return 0
    width, height = dims
    if width <= MAX_DIMENSION and height <= MAX_DIMENSION:
        return 0

    reason = (
        f"Refusing to Read oversized image {path} ({width}x{height}). "
        "Claude provider requests with many images allow at most 2000 pixels per dimension. "
        "Downscale or rerender this image first, then Read the smaller file. "
        "For PDF screenshots, use a lower pdftoppm DPI such as -r 120, or use pdftotext when text is sufficient."
    )
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
    return 0

sys.exit(main())
PY
"#;
    write_file_atomic(&image_hook_path, image_hook_script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&image_hook_path, perms)?;
    }

    let image_hook_command = if is_container {
        if let Ok(rel) = image_hook_path.strip_prefix(workspace_root) {
            format!("/{}", rel.to_string_lossy())
        } else {
            image_hook_path.to_string_lossy().to_string()
        }
    } else {
        image_hook_path.to_string_lossy().to_string()
    };
    tracing::info!(
        hook_path = %image_hook_command,
        is_container = is_container,
        max_dimension = 2000,
        "Image Read PreToolUse hook written"
    );

    Ok(Some(json!({
        "PreToolUse": [
            {
                "matcher": "Bash",
                "hooks": [{
                    "type": "command",
                    "command": hook_command
                }]
            },
            {
                "matcher": "Read",
                "hooks": [{
                    "type": "command",
                    "command": image_hook_command
                }]
            }
        ]
    })))
}

/// Render the Claude Code Bash `PreToolUse` hook script.
///
/// Its sole responsibility is the **gh terminal fix**: it wraps `gh` commands
/// with `env TERM=dumb` so lipgloss/glamour stops issuing terminal capability
/// queries (OSC 11, DSR, …) that never get a response in our PTY and hang
/// forever. Compound commands are left untouched.
pub(crate) fn render_bash_pretool_script() -> String {
    r#"#!/bin/bash
# PreToolUse hook for Bash commands.
# Fixes gh CLI hanging in PTY by setting TERM=dumb (prevents lipgloss/glamour
# terminal capability queries that never get a response in our PTY).
set -euo pipefail

INPUT=$(cat)
COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty')

if [ -z "$COMMAND" ]; then exit 0; fi

# Skip compound commands (pipes, chains, heredocs, subshells, semicolons).
case "$COMMAND" in
  *"&&"*|*"||"*|*"|"*|*"<<"*|*"("*|*";"*|*'`'*|*'$('*) exit 0 ;;
esac

# Extract the base command (first word, ignoring path prefix).
FIRST_WORD=$(echo "$COMMAND" | awk '{print $1}')
BASE_CMD=$(basename "$FIRST_WORD")

emit_rewrite() {
  jq -n --arg cmd "$1" '{
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "allow",
      updatedInput: { command: $cmd }
    }
  }'
}

# Fix gh commands that hang in PTY environments. The gh CLI (via
# lipgloss/glamour) sends terminal capability queries like OSC 11 (background
# color) and DSR (cursor position) when TERM != dumb; our PTY has no terminal
# emulator to respond, causing indefinite hangs.
case "$BASE_CMD" in
  gh)
    emit_rewrite "env TERM=dumb $COMMAND"
    exit 0
    ;;
esac

exit 0
"#
    .to_string()
}

/// Deep-merge `overlay` into `base`.
/// - Objects: recurse; overlay scalar wins on conflict
/// - Arrays: concatenate (base first, then overlay)
/// - Scalars / type mismatch: overlay replaces base
pub(crate) fn merge_json(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            for (k, v) in o {
                merge_json(b.entry(k.clone()).or_insert(serde_json::Value::Null), v);
            }
        }
        (serde_json::Value::Array(b), serde_json::Value::Array(o)) => {
            b.extend(o.iter().cloned());
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

/// Write Claude Code configuration to the workspace.
/// Generates `.claude/settings.local.json` and `CLAUDE.md` files.
#[allow(clippy::too_many_arguments)]
async fn write_claudecode_config(
    workspace_dir: &Path,
    mcp_configs: Vec<McpServerConfig>,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    skill_contents: Option<&[SkillContent]>,
    command_contents: Option<&[CommandContent]>,
    shared_network: Option<bool>,
    profile_overlay: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    // Create .claude directory
    let claude_dir = workspace_dir.join(".claude");
    tokio::fs::create_dir_all(&claude_dir).await?;

    // Build MCP servers config in Claude Code format
    let mut mcp_servers = serde_json::Map::new();
    let mut used = std::collections::HashSet::new();

    let filtered_configs = mcp_configs.into_iter().filter(|c| c.enabled);

    for config in filtered_configs {
        let base = sanitize_key(&config.name);
        let key = unique_key(&base, &mut used);
        mcp_servers.insert(
            key,
            claude_entry_from_mcp(
                &config,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
            ),
        );
    }

    // Write settings.local.json
    // Add permissive settings to avoid permission prompts.
    //
    // IMPORTANT: Claude Code permission syntax:
    // - "Bash" (no parentheses) allows ALL bash commands
    // - "Bash(*)" does NOT work as a wildcard - it's a literal pattern
    // - "mcp__*" works for MCP tools as a wildcard
    //
    // Tool policy:
    // - Claude Code CLI is executed inside the workspace execution context.
    // - Therefore, built-in Bash is safe to allow for both host + container workspaces.
    // - Legacy MCP tools are still allowed as a wildcard for compatibility.
    let permissions: Vec<&str> = match workspace_type {
        WorkspaceType::Container => vec!["Bash", "Edit", "Write", "Read", "mcp__*"],
        WorkspaceType::Host => vec!["Bash", "Edit", "Write", "Read", "mcp__*"],
    };
    let mut settings = json!({
        "mcpServers": mcp_servers,
        "permissions": {
            "allow": permissions
        }
    });

    // Add Claude PreToolUse hooks: Bash gh-PTY fix plus image Read guard.
    if let Some(hooks) =
        write_claude_pretool_hooks(workspace_dir, workspace_root, workspace_type).await?
    {
        settings
            .as_object_mut()
            .unwrap()
            .insert("hooks".to_string(), hooks);
    }

    // Apply config profile settings: profile is the base, generated settings win on top.
    // Arrays (e.g. hooks) are concatenated — profile hooks + generated hooks both survive.
    if let Some(profile) = profile_overlay {
        let mut merged = profile.clone();
        merge_json(&mut merged, &settings);
        settings = merged;
    }

    let settings_path = claude_dir.join("settings.local.json");
    let settings_content = serde_json::to_string_pretty(&settings)?;
    write_file_atomic(&settings_path, &settings_content)?;
    let settings_json_path = claude_dir.join("settings.json");
    write_file_atomic(&settings_json_path, &settings_content)?;

    // Write a dedicated MCP config for CLI flags like --mcp-config.
    // Use mcpServers from the merged settings (includes profile overlay MCPs)
    // rather than only the generated MCPs.
    let final_mcp_servers = settings
        .get("mcpServers")
        .cloned()
        .unwrap_or_else(|| json!(mcp_servers));
    let mcp_only = json!({ "mcpServers": final_mcp_servers });
    let mcp_content = serde_json::to_string_pretty(&mcp_only)?;
    let mcp_config_path = claude_dir.join("mcp.json");
    write_file_atomic(&mcp_config_path, &mcp_content)?;
    // Also write settings under XDG_CONFIG_HOME/claude for Claude CLI XDG lookups.
    let xdg_claude_dir = workspace_dir.join(".config").join("claude");
    tokio::fs::create_dir_all(&xdg_claude_dir).await?;
    let xdg_settings_path = xdg_claude_dir.join("settings.json");
    write_file_atomic(&xdg_settings_path, &settings_content)?;
    let xdg_settings_local = xdg_claude_dir.join("settings.local.json");
    write_file_atomic(&xdg_settings_local, &settings_content)?;
    let xdg_mcp_path = xdg_claude_dir.join("mcp.json");
    write_file_atomic(&xdg_mcp_path, &mcp_content)?;

    // Also write settings to ~/.claude so `claude mcp list` sees workspace MCPs.
    let claude_home = resolve_claudecode_dir(workspace_root, workspace_type, workspace_env);
    if claude_home != claude_dir {
        tokio::fs::create_dir_all(&claude_home).await?;
        let home_settings = claude_home.join("settings.local.json");
        write_file_atomic(&home_settings, &settings_content)?;
        let home_settings_json = claude_home.join("settings.json");
        write_file_atomic(&home_settings_json, &settings_content)?;
        let home_mcp = claude_home.join("mcp.json");
        write_file_atomic(&home_mcp, &mcp_content)?;
    }

    // Write skills to .claude/skills/ using Claude Code's native format
    // This allows Claude to discover and list skills properly
    if let Some(skills) = skill_contents {
        write_claudecode_skills_to_workspace(workspace_dir, skills).await?;

        // Generate minimal CLAUDE.md with workspace context only
        // Skills are now in .claude/skills/ and Claude will discover them automatically
        let claude_md_path = workspace_dir.join("CLAUDE.md");
        let mut claude_md = String::new();
        claude_md.push_str("# sandboxed.sh Workspace\n\n");

        match workspace_type {
            WorkspaceType::Container => {
                claude_md.push_str(
                    "This is an **isolated container workspace** managed by sandboxed.sh.\n\n",
                );
                claude_md.push_str("- Shell commands execute inside the container\n");
                claude_md.push_str("- Use the built-in `Bash` tool for shell commands\n");
                claude_md.push_str(
                    "- Skills are available in `.claude/skills/` - use `/help` to list them\n",
                );
            }
            WorkspaceType::Host => {
                claude_md.push_str("This is a **host workspace** managed by sandboxed.sh.\n\n");
                claude_md
                    .push_str("- Use the built-in `Bash` tool to run shell commands directly\n");
                claude_md.push_str(
                    "- Skills are available in `.claude/skills/` - use `/help` to list them\n",
                );
            }
        }

        write_file_atomic(&claude_md_path, claude_md)?;
    }

    // Write commands to .claude/commands/ using Claude Code's native custom slash command format
    if let Some(commands) = command_contents {
        write_claudecode_commands_to_workspace(workspace_dir, commands).await?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_codex_config(
    workspace_dir: &Path,
    mcp_configs: Vec<McpServerConfig>,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    skill_contents: Option<&[SkillContent]>,
    shared_network: Option<bool>,
    profile_base: Option<&str>,
) -> anyhow::Result<()> {
    let codex_dir = resolve_codex_dir(workspace_dir, workspace_root, workspace_type, workspace_env);
    tokio::fs::create_dir_all(&codex_dir).await?;

    tracing::debug!("Ensuring Codex config directory at {}", codex_dir.display());

    // Write MCP config for Codex so tools are available.
    let config_path = codex_dir.join("config.toml");
    let file_existing = tokio::fs::read_to_string(&config_path)
        .await
        .unwrap_or_default();
    // Profile is authoritative for non-MCP sections like [otel].
    // When a profile is selected (Some), use its content even if empty —
    // this clears stale config from previous missions/profiles.
    // Only fall back to existing file when no profile system is active (None).
    let existing = match profile_base {
        Some(toml) => toml.to_string(),
        None => file_existing,
    };

    let mut entries = Vec::new();
    let mut existing_names = std::collections::HashSet::new();
    for config in mcp_configs.iter().filter(|c| c.enabled) {
        existing_names.insert(config.name.clone());
        if let Some(entry) = codex_entry_from_mcp(
            config,
            workspace_dir,
            workspace_root,
            workspace_type,
            workspace_env,
            shared_network,
            None,
        ) {
            entries.push(entry);
        }
    }

    // Provide a filesystem alias for Codex (many prompts/toolchains expect it).
    if existing_names.contains("workspace") && !existing_names.contains("filesystem") {
        if let Some(workspace_cfg) = mcp_configs.iter().find(|c| c.name == "workspace") {
            if let Some(entry) = codex_entry_from_mcp(
                workspace_cfg,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
                Some("filesystem".to_string()),
            ) {
                entries.push(entry);
            }
        }
    }

    let config_payload = update_codex_mcp_config(&existing, &entries);
    // Codex only streams reasoning summaries when the request asks for them.
    // The CLI default (`model_reasoning_summary = "auto"`) yields zero
    // summaries on ChatGPT-OAuth gpt-5.5 turns (verified 2026-06-04: 2770
    // reasoning items across one day of rollouts, all `summary: []`), so no
    // `item/reasoning/*` notifications stream, no Thinking events are
    // emitted, and the Thoughts panel has nothing to persist or replay.
    // Pin "detailed" unless the profile/operator already set a value.
    let config_payload = ensure_codex_reasoning_summary(&config_payload);
    write_file_atomic(&config_path, config_payload)?;

    // Write skills to ~/.codex/skills using Codex's native skills format
    if let Some(skills) = skill_contents {
        write_codex_skills_to_workspace(&codex_dir, skills).await?;
    }

    Ok(())
}

pub(crate) struct CodexMcpEntry {
    pub(crate) name: String,
    pub(crate) command: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) env: HashMap<String, String>,
    pub(crate) url: Option<String>,
    pub(crate) headers: HashMap<String, String>,
}

fn resolve_codex_dir(
    workspace_dir: &Path,
    _workspace_root: &Path,
    _workspace_type: WorkspaceType,
    _workspace_env: &HashMap<String, String>,
) -> PathBuf {
    // Codex runs with a per-mission HOME rooted at `workspace_dir`. Writing
    // generated MCP configuration into the shared workspace/container home
    // leaves the live mission with only Codex's trust stanza and no MCP tools,
    // while concurrent mission preparation can overwrite another mission's
    // MISSION_ID. Keep config and native skills beside the mission HOME that
    // the runner actually exports.
    workspace_dir.join(".codex")
}

fn resolve_claudecode_dir(
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
) -> PathBuf {
    super::resolve_workspace_home_root(workspace_root, workspace_type, workspace_env)
        .join(".claude")
}

fn codex_entry_from_mcp(
    config: &McpServerConfig,
    workspace_dir: &Path,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    shared_network: Option<bool>,
    override_name: Option<String>,
) -> Option<CodexMcpEntry> {
    let raw_name = override_name.unwrap_or_else(|| config.name.clone());
    let sanitized = sanitize_key(&raw_name);
    let name = if sanitized.is_empty() {
        "mcp".to_string()
    } else {
        sanitized
    };
    match &config.transport {
        McpTransport::Http { endpoint, headers } => Some(CodexMcpEntry {
            name,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(endpoint.clone()),
            headers: headers.clone(),
        }),
        McpTransport::Stdio { .. } => {
            let opencode_entry = opencode_entry_from_mcp(
                config,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
            );
            let command_vec = opencode_entry
                .get("command")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let command = command_vec
                .first()
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let args: Vec<String> = command_vec
                .iter()
                .skip(1)
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();

            let env = opencode_entry
                .get("environment")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect::<HashMap<String, String>>()
                })
                .unwrap_or_default();

            command.map(|cmd| CodexMcpEntry {
                name,
                command: Some(cmd),
                args,
                env,
                url: None,
                headers: HashMap::new(),
            })
        }
    }
}

/// Ensure the Codex config requests reasoning summaries. TOML top-level keys
/// must precede the first `[section]`, so the key is prepended. A
/// `model_reasoning_summary` already present in the top-level prelude (from a
/// config profile or operator edit) wins — we only add the default when the
/// key is absent.
pub(crate) fn ensure_codex_reasoning_summary(config: &str) -> String {
    let has_key = config
        .lines()
        .take_while(|line| !line.trim_start().starts_with('['))
        .any(|line| {
            line.split('=')
                .next()
                .map(|key| key.trim() == "model_reasoning_summary")
                .unwrap_or(false)
        });
    if has_key {
        return config.to_string();
    }
    format!("model_reasoning_summary = \"detailed\"\n\n{}", config)
}

pub(crate) fn update_codex_mcp_config(existing: &str, entries: &[CodexMcpEntry]) -> String {
    let mut names = std::collections::HashSet::new();
    for entry in entries {
        names.insert(entry.name.clone());
    }

    let mut filtered: Vec<String> = Vec::new();
    let mut skip = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if let Some(section_name) = parse_mcp_section_name(line) {
                if names.contains(&section_name) {
                    skip = true;
                    continue;
                }
                skip = false;
                filtered.push(line.to_string());
                continue;
            }
            // Non-MCP section: stop skipping and keep section header.
            skip = false;
            filtered.push(line.to_string());
            continue;
        }
        if skip {
            continue;
        }
        filtered.push(line.to_string());
    }

    let mut output = filtered.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    if !output.is_empty() && !output.ends_with("\n\n") {
        output.push('\n');
    }

    for entry in entries {
        output.push_str(&render_codex_mcp_entry(entry));
        output.push('\n');
    }

    output
}

fn parse_mcp_section_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    let inner = trimmed.trim_start_matches('[').trim_end_matches(']');
    let prefix = "mcp_servers.";
    if !inner.starts_with(prefix) {
        return None;
    }
    let rest = &inner[prefix.len()..];
    let base = rest.split('.').next()?;
    Some(sanitize_key(base))
}

fn render_codex_mcp_entry(entry: &CodexMcpEntry) -> String {
    let mut out = String::new();
    out.push_str(&format!("[mcp_servers.{}]\n", entry.name));

    if let Some(url) = &entry.url {
        out.push_str(&format!("url = {}\n", toml_string(url)));
        if !entry.headers.is_empty() {
            out.push('\n');
            out.push_str(&format!("[mcp_servers.{}.headers]\n", entry.name));
            let mut headers = entry.headers.iter().collect::<Vec<_>>();
            headers.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in headers {
                out.push_str(&format!("{} = {}\n", toml_key(key), toml_string(value)));
            }
        }
        return out;
    }

    if let Some(command) = &entry.command {
        out.push_str(&format!("command = {}\n", toml_string(command)));
        if !entry.args.is_empty() {
            let args = entry
                .args
                .iter()
                .map(|arg| toml_string(arg))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("args = [{}]\n", args));
        }
        if !entry.env.is_empty() {
            out.push('\n');
            out.push_str(&format!("[mcp_servers.{}.env]\n", entry.name));
            let mut envs = entry.env.iter().collect::<Vec<_>>();
            envs.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in envs {
                out.push_str(&format!("{} = {}\n", toml_key(key), toml_string(value)));
            }
        }
    }

    out
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn toml_key(key: &str) -> String {
    if key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return key.to_string();
    }
    toml_string(key)
}

/// Write backend-specific configuration to the workspace.
/// This is the main entry point for config generation.
#[allow(clippy::too_many_arguments)]
pub async fn write_backend_config(
    workspace_dir: &Path,
    backend_id: &str,
    mcp_configs: Vec<McpServerConfig>,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    skill_allowlist: Option<&[String]>,
    skill_contents: Option<&[SkillContent]>,
    command_contents: Option<&[CommandContent]>,
    shared_network: Option<bool>,
    custom_providers: Option<&[AIProvider]>,
    claudecode_profile_overlay: Option<&serde_json::Value>,
    codex_profile_base: Option<&str>,
) -> anyhow::Result<()> {
    match backend_id {
        "opencode" => {
            write_opencode_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_allowlist,
                command_contents,
                shared_network,
                custom_providers,
            )
            .await
        }
        "claudecode" => {
            // Keep OpenCode config in sync for compatibility with existing execution pipeline.
            write_opencode_config(
                workspace_dir,
                mcp_configs.clone(),
                workspace_root,
                workspace_type,
                workspace_env,
                skill_allowlist,
                command_contents,
                shared_network,
                custom_providers,
            )
            .await?;
            write_claudecode_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_contents,
                command_contents,
                shared_network,
                claudecode_profile_overlay,
            )
            .await
        }
        "codex" => {
            write_codex_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_contents,
                shared_network,
                codex_profile_base,
            )
            .await
        }
        "gemini" | "grok" => {
            // These CLIs don't need a Sandboxed.sh-specific config format; use
            // OpenCode config for workspace setup (skills, commands, etc.).
            write_opencode_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_allowlist,
                command_contents,
                shared_network,
                custom_providers,
            )
            .await
        }
        _ => {
            // Unknown backend - write OpenCode config as fallback
            tracing::warn!(
                backend = backend_id,
                "Unknown backend, falling back to OpenCode config"
            );
            write_opencode_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_allowlist,
                command_contents,
                shared_network,
                custom_providers,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_generated_state_is_mission_scoped() {
        let workspace_root = Path::new("/srv/workspace-root");
        let mission_dir = workspace_root.join("workspaces/mission-deadbeef");

        assert_eq!(
            resolve_codex_dir(
                &mission_dir,
                workspace_root,
                WorkspaceType::Container,
                &HashMap::new(),
            ),
            mission_dir.join(".codex")
        );
    }

    #[test]
    fn third_party_mcp_only_receives_allowlisted_workspace_env() {
        let workspace_root = Path::new("/srv/workspace-root");
        let mission_dir = workspace_root.join("workspaces/mission-deadbeef");
        let mut mcp = McpServerConfig::new_stdio(
            "lean_lsp".to_string(),
            "/workspace/tools/lean-lsp-mcp".to_string(),
            Vec::new(),
            HashMap::new(),
        );
        mcp.workspace_env_allowlist = vec!["PATH".to_string(), "ELAN_HOME".to_string()];

        let workspace_env = HashMap::from([
            ("PATH".to_string(), "/root/.elan/bin:/usr/bin".to_string()),
            ("ELAN_HOME".to_string(), "/root/.elan".to_string()),
            ("GH_TOKEN".to_string(), "github-secret".to_string()),
            ("SSH_PRIVATE_KEY_B64".to_string(), "ssh-secret".to_string()),
            ("GPG_PRIVATE_KEY_B64".to_string(), "gpg-secret".to_string()),
        ]);

        let entry = codex_entry_from_mcp(
            &mcp,
            &mission_dir,
            workspace_root,
            WorkspaceType::Host,
            &workspace_env,
            None,
            None,
        )
        .expect("stdio MCP entry");
        let rendered = update_codex_mcp_config("", &[entry]);

        assert!(rendered.contains("-i"));
        assert!(rendered.contains("PATH=/root/.elan/bin:/usr/bin"));
        assert!(rendered.contains("ELAN_HOME=/root/.elan"));
        assert!(!rendered.contains("GH_TOKEN"));
        assert!(!rendered.contains("github-secret"));
        assert!(!rendered.contains("SSH_PRIVATE_KEY_B64"));
        assert!(!rendered.contains("ssh-secret"));
        assert!(!rendered.contains("GPG_PRIVATE_KEY_B64"));
        assert!(!rendered.contains("gpg-secret"));
        assert!(!rendered.contains("SANDBOXED_SH_WORKSPACE_ENV_VARS_FILE"));
    }

    #[test]
    fn third_party_mcp_isolated_process_does_not_inherit_harness_env() {
        let workspace_root = Path::new("/srv/workspace-root");
        let mission_dir = workspace_root.join("workspaces/mission-deadbeef");
        let mcp = McpServerConfig::new_stdio(
            "third_party".to_string(),
            "env".to_string(),
            Vec::new(),
            HashMap::new(),
        );
        let workspace_env = HashMap::from([(
            "GH_TOKEN".to_string(),
            "must-not-cross-boundary".to_string(),
        )]);

        let entry = opencode_entry_from_mcp(
            &mcp,
            &mission_dir,
            workspace_root,
            WorkspaceType::Host,
            &workspace_env,
            None,
        );
        let command = entry["command"].as_array().expect("command array");

        assert_eq!(command[1], "-i");
        assert!(command
            .iter()
            .any(|part| part.as_str().is_some_and(|part| part.starts_with("PATH="))));
        assert!(!entry.to_string().contains("GH_TOKEN"));
        assert!(!entry.to_string().contains("must-not-cross-boundary"));

        let command_parts: Vec<&str> = command
            .iter()
            .map(|part| part.as_str().expect("string command part"))
            .collect();
        let output = std::process::Command::new(command_parts[0])
            .args(&command_parts[1..])
            .env("GH_TOKEN", "inherited-harness-secret")
            .output()
            .expect("run isolated MCP command");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.contains("GH_TOKEN"));
        assert!(!stdout.contains("inherited-harness-secret"));
    }
}

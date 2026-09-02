//! Canonical Hermes assistant tool allowlist.
//!
//! Single source of truth for the `assistant-mcp` tools a Hermes gateway is
//! configured to see. Three places used to carry their own copy — the
//! `assistant-mcp` binary's tool table, the generated Hermes `config.yaml`
//! (`hermes_config_yaml` in `src/api/system.rs`), and
//! `scripts/configure_hermes_reliability.py` — and they drifted apart:
//! `get_chatgpt_ui_pool_status` is mandated by the mission-control skill's
//! ChatGPT-UI pool policy yet was absent from both generated allowlists, and
//! `acknowledge_mission` / the durable workspace-job tools were only in one.
//!
//! The `assistant-mcp` binary is itself the curated Hermes surface (it
//! deliberately omits deployment, board, and host-durable-job tools), so the
//! allowlist is simply "every tool the binary exposes". Tests here and in
//! `src/bin/assistant_mcp.rs` pin the other copies to this list; add or
//! remove tools HERE first.

pub const HERMES_ASSISTANT_TOOL_ALLOWLIST: &[&str] = &[
    "list_active_missions",
    "list_missions",
    "get_mission",
    "get_mission_digest",
    "get_mission_events",
    "get_chatgpt_ui_pool_status",
    "list_mission_shared_files",
    "download_shared_file",
    "start_mission",
    "send_message_to_mission",
    "ask_mission",
    "answer_mission_question",
    "cancel_mission",
    "acknowledge_mission",
    "adopt_mission",
    "get_compute_fleet",
    "list_projects",
    "get_project",
    "get_situation",
    "update_project_status",
    "set_project_track",
    "accept_project_track_evidence",
    "reopen_project_track",
    "accept_project_track",
    "invalidate_project_track_evidence",
    "get_project_grant",
    "set_project_grant",
    "record_project_decision",
    "answer_project_decision",
    "get_project_tasks",
    "plan_project_tasks",
    "update_project_task",
    "cancel_project_task",
    "link_mission_to_project",
    "list_workspaces",
    "get_workspace",
    "create_workspace",
    "update_workspace",
    "delete_workspace",
    "list_workspace_templates",
    "get_workspace_template",
    "save_workspace_template",
    "delete_workspace_template",
    "rebuild_workspace_from_template",
    "workspace_bash",
    "start_workspace_job",
    "get_workspace_job",
    "cancel_workspace_job",
    "get_mission_health",
    "get_mission_diagnostics",
    "update_mission_settings",
    "resume_mission",
];

/// Render the allowlist as the YAML items of a `tools.include` block, one
/// `{indent}- {tool}` line per tool.
pub fn yaml_include_items(indent: &str) -> String {
    HERMES_ASSISTANT_TOOL_ALLOWLIST
        .iter()
        .map(|tool| format!("{indent}- {tool}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for tool in HERMES_ASSISTANT_TOOL_ALLOWLIST {
            assert!(seen.insert(*tool), "duplicate tool in allowlist: {tool}");
        }
    }

    /// `scripts/configure_hermes_reliability.py` runs on Hermes hosts without
    /// the Rust source, so it carries its own copy of the allowlist. Pin that
    /// copy to the canonical one so the two can never drift again.
    #[test]
    fn reliability_script_allowlist_matches_canonical() {
        let script_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/configure_hermes_reliability.py"
        );
        let script =
            std::fs::read_to_string(script_path).expect("read configure_hermes_reliability.py");
        let body = script
            .split_once("ASSISTANT_TOOLS = [")
            .expect("ASSISTANT_TOOLS list in script")
            .1
            .split_once(']')
            .expect("ASSISTANT_TOOLS list terminator")
            .0;
        let script_tools: Vec<&str> = body
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.trim_matches(|c| c == '"' || c == '\''))
            .collect();
        assert_eq!(
            script_tools, HERMES_ASSISTANT_TOOL_ALLOWLIST,
            "scripts/configure_hermes_reliability.py ASSISTANT_TOOLS diverged from \
             src/hermes_tools.rs::HERMES_ASSISTANT_TOOL_ALLOWLIST — update both together"
        );
    }
}

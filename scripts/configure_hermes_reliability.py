#!/usr/bin/env python3
"""Apply the Hermes-side reliability boundary without rewriting secrets."""

from __future__ import annotations

import argparse
import os
import shutil
import tempfile
from pathlib import Path

from ruamel.yaml import YAML


# Canonical assistant-mcp allowlist. Mirrors
# src/hermes_tools.rs::HERMES_ASSISTANT_TOOL_ALLOWLIST — a Rust test pins the
# two lists together, so update both in the same change.
ASSISTANT_TOOLS = [
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
]


def configure(config: dict, assistant_mcp: str) -> None:
    config.setdefault("kanban", {}).update(
        {
            "dispatch_in_gateway": False,
            "auto_decompose": False,
            "auto_decompose_per_tick": 0,
        }
    )
    server = config.setdefault("mcp_servers", {}).setdefault("sandboxed_assistant", {})
    server["command"] = assistant_mcp
    # Ask turns (ask_mission) can make multiple sequential LLM/tool calls;
    # match the 600s timeout contract of the generated config
    # (hermes_config_yaml in src/api/system.rs) so they aren't cut off.
    server["timeout"] = 600
    tools = server.setdefault("tools", {})
    tools["include"] = list(ASSISTANT_TOOLS)
    tools["prompts"] = False
    tools["resources"] = False

    # Proton currently fails its production startup self-test because its
    # optional Python dependencies are not installed. Keep it explicitly
    # disabled until that probe passes instead of paying the failure cost on
    # every gateway start.
    plugins = config.setdefault("plugins", {})
    enabled = plugins.setdefault("enabled", [])
    disabled = plugins.setdefault("disabled", [])
    plugins["enabled"] = [name for name in enabled if name != "proton-platform"]
    if "proton-platform" not in disabled:
        disabled.append("proton-platform")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--assistant-mcp", required=True)
    args = parser.parse_args()

    path = args.home / "config.yaml"
    if not path.is_file():
        raise SystemExit(f"Hermes config not found: {path}")
    backup = path.with_name(f"{path.name}.pre-reliability")
    if not backup.exists():
        shutil.copy2(path, backup)

    yaml = YAML()
    yaml.preserve_quotes = True
    with path.open() as stream:
        config = yaml.load(stream) or {}
    configure(config, args.assistant_mcp)

    mode = path.stat().st_mode
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as stream:
        temp = Path(stream.name)
        yaml.dump(config, stream)
    os.chmod(temp, mode)
    os.replace(temp, path)

    print(
        f"Configured {path}: native Kanban disabled, "
        f"assistant-MCP timeout=600s, tools={len(ASSISTANT_TOOLS)}, "
        "Proton disabled"
    )


if __name__ == "__main__":
    main()

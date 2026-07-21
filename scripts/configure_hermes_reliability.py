#!/usr/bin/env python3
"""Apply the Hermes-side reliability boundary without rewriting secrets."""

from __future__ import annotations

import argparse
import os
import shutil
import tempfile
from pathlib import Path

from ruamel.yaml import YAML


ASSISTANT_TOOLS = [
    "list_active_missions",
    "list_missions",
    "get_mission",
    "get_mission_events",
    "start_mission",
    "send_message_to_mission",
    "cancel_mission",
    "list_workspaces",
    "get_compute_fleet",
    "get_mission_health",
    "get_mission_diagnostics",
    "update_mission_settings",
    "resume_mission",
    "acknowledge_mission",
    "start_workspace_job",
    "get_workspace_job",
    "cancel_workspace_job",
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
    server["timeout"] = 120
    tools = server.setdefault("tools", {})
    tools["include"] = list(ASSISTANT_TOOLS)
    tools["prompts"] = False
    tools["resources"] = False


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
        f"assistant-MCP timeout=120s, tools={len(ASSISTANT_TOOLS)}"
    )


if __name__ == "__main__":
    main()

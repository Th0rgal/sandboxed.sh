#!/usr/bin/env python3
"""Lint the versioned ChatGPT UI pool policy.

Validates docs/policy/chatgpt_ui_pool_policy.json against its JSON Schema,
enforces the non-negotiable pool invariants, and cross-checks every numeric
limit and signal against the runtime source, the driver, the operator doc,
and the hermes-mission-control skill so the policy cannot silently go stale.

Stdlib only. Exit code 0 on success, 1 with one error per line on failure.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

POLICY_JSON = Path("docs/policy/chatgpt_ui_pool_policy.json")
POLICY_SCHEMA = Path("docs/policy/chatgpt_ui_pool_policy.schema.json")
POLICY_DOC = Path("docs/policy/CHATGPT_UI_POOL_POLICY.md")
HARNESS_DOC = Path("docs/CHATGPT_UI_HARNESS.md")
RUNTIME_SOURCE = Path("src/api/runners/chatgpt_ui.rs")
DRIVER_SOURCE = Path("scripts/chatgpt_ui_driver.py")
SKILL_DOC = Path("skills/hermes-mission-control/SKILL.md")

JSON_TYPES = {
    "object": dict,
    "string": str,
    "integer": int,
    "null": type(None),
    "boolean": bool,
}


def validate_schema(instance, schema, path, errors):
    """Validate the JSON Schema subset used by the policy schema."""
    if "const" in schema:
        if instance != schema["const"]:
            errors.append(f"{path}: expected const {schema['const']!r}, got {instance!r}")
        return
    expected = schema.get("type")
    if expected is not None:
        python_type = JSON_TYPES[expected]
        if not isinstance(instance, python_type) or (
            python_type is int and isinstance(instance, bool)
        ):
            errors.append(f"{path}: expected {expected}, got {type(instance).__name__}")
            return
    if "pattern" in schema and isinstance(instance, str):
        if not re.fullmatch(schema["pattern"], instance):
            errors.append(f"{path}: {instance!r} does not match {schema['pattern']!r}")
    if "minimum" in schema and isinstance(instance, int):
        if instance < schema["minimum"]:
            errors.append(f"{path}: {instance} is below minimum {schema['minimum']}")
    if isinstance(instance, dict):
        for key in schema.get("required", []):
            if key not in instance:
                errors.append(f"{path}: missing required key {key!r}")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            for key in instance:
                if key not in properties:
                    errors.append(f"{path}: unexpected key {key!r}")
        for key, subschema in properties.items():
            if key in instance:
                validate_schema(instance[key], subschema, f"{path}.{key}", errors)


def check_invariants(policy, errors):
    """Non-negotiable pool rules, pinned in code so the schema alone
    cannot be loosened to weaken them."""
    rules = [
        ("capacity.source", "profile_dirs"),
        ("capacity.static_limit", None),
        ("lanes.read_only_pro.allowed", True),
        ("lanes.read_only_pro.writer", False),
        ("lanes.read_only_pro.concurrent", True),
        ("lanes.read_only_pro.requires_disjoint_slots", True),
        ("retry.compatibility_failure.max_automatic_retries", 1),
        ("retry.compatibility_failure.require_different_slot", True),
        ("retry.compatibility_failure.require_healthy_slot", True),
        ("retry.auth_failure.max_automatic_retries", 0),
        ("retry.auth_failure.operator_action_required", True),
        ("retry.rate_limited.max_automatic_retries", 0),
        ("writers.max_concurrent_per_workspace", 1),
        ("writers.chatgpt_ui_may_write", False),
        ("lean.independent_validation_required", True),
        ("lean.validator_must_differ_from_writer", True),
        ("lean.validation_before_write", True),
    ]
    for dotted, expected in rules:
        node = policy
        for part in dotted.split("."):
            if not isinstance(node, dict) or part not in node:
                errors.append(f"invariant {dotted}: missing")
                break
            node = node[part]
        else:
            if node != expected:
                errors.append(f"invariant {dotted}: expected {expected!r}, got {node!r}")


def rust_int(text):
    return int(text.replace("_", ""))


def check_runtime_constants(repo_root, policy, errors):
    source = (repo_root / RUNTIME_SOURCE).read_text(encoding="utf-8")
    limits = policy.get("runtime_limits", {})
    timeout = limits.get("timeout_secs", {})
    artifacts = limits.get("artifacts_per_turn", {})

    default_match = re.search(
        r'get_backend_u64_setting\("chatgpt_ui",\s*"timeout_secs"\)\.unwrap_or\(([0-9_]+)\)',
        source,
    )
    if not default_match:
        errors.append(f"{RUNTIME_SOURCE}: cannot locate timeout_secs default")
    elif rust_int(default_match.group(1)) != timeout.get("default"):
        errors.append(
            f"runtime_limits.timeout_secs.default {timeout.get('default')} != "
            f"runtime {rust_int(default_match.group(1))}"
        )

    clamp_match = re.search(r"\(([0-9_]+)\.\.=([0-9_]+)\)\.contains\(&timeout_secs\)", source)
    if not clamp_match:
        errors.append(f"{RUNTIME_SOURCE}: cannot locate timeout_secs clamp")
    else:
        low, high = rust_int(clamp_match.group(1)), rust_int(clamp_match.group(2))
        if (timeout.get("min"), timeout.get("max")) != (low, high):
            errors.append(
                f"runtime_limits.timeout_secs {timeout.get('min')}-{timeout.get('max')} != "
                f"runtime clamp {low}-{high}"
            )

    files_match = re.search(r"MAX_ARTIFACT_FILES:\s*usize\s*=\s*([0-9_]+)", source)
    if not files_match:
        errors.append(f"{RUNTIME_SOURCE}: cannot locate MAX_ARTIFACT_FILES")
    elif rust_int(files_match.group(1)) != artifacts.get("max_files"):
        errors.append(
            f"runtime_limits.artifacts_per_turn.max_files {artifacts.get('max_files')} != "
            f"runtime {rust_int(files_match.group(1))}"
        )

    bytes_match = re.search(r"MAX_ARTIFACT_BYTES:\s*u64\s*=\s*([0-9_]+)\s*\*\s*1024\s*\*\s*1024", source)
    if not bytes_match:
        errors.append(f"{RUNTIME_SOURCE}: cannot locate MAX_ARTIFACT_BYTES")
    elif rust_int(bytes_match.group(1)) * 1024 * 1024 != artifacts.get("max_total_bytes"):
        errors.append(
            f"runtime_limits.artifacts_per_turn.max_total_bytes {artifacts.get('max_total_bytes')} != "
            f"runtime {rust_int(bytes_match.group(1)) * 1024 * 1024}"
        )


def check_driver_signal(repo_root, policy, errors):
    driver = (repo_root / DRIVER_SOURCE).read_text(encoding="utf-8")
    match = re.search(r'COMPAT_VERSION\s*=\s*"([^"]+)"', driver)
    if not match:
        errors.append(f"{DRIVER_SOURCE}: cannot locate COMPAT_VERSION")
        return
    signal = (
        policy.get("retry", {}).get("compatibility_failure", {}).get("signal", "")
    )
    if signal != f"compatibility={match.group(1)}":
        errors.append(
            f"retry.compatibility_failure.signal {signal!r} != driver "
            f"compatibility={match.group(1)!r}"
        )


def check_harness_doc(repo_root, policy, errors):
    doc = (repo_root / HARNESS_DOC).read_text(encoding="utf-8")
    timeout = policy.get("runtime_limits", {}).get("timeout_secs", {})
    match = re.search(r"clamped to ([0-9]+)[–-]([0-9]+) seconds", doc)
    if not match:
        errors.append(f"{HARNESS_DOC}: cannot locate timeout clamp statement")
    else:
        low, high = int(match.group(1)), int(match.group(2))
        if (timeout.get("min"), timeout.get("max")) != (low, high):
            errors.append(
                f"{HARNESS_DOC}: states clamp {low}-{high}, policy says "
                f"{timeout.get('min')}-{timeout.get('max')}"
            )


def doc_version(text):
    match = re.search(r"^Version:\s*([0-9]+\.[0-9]+\.[0-9]+)\s*$", text, re.MULTILINE)
    return match.group(1) if match else None


def skill_versions(text):
    frontmatter_match = re.match(r"\A---\n(.*?)\n---\n", text, re.DOTALL)
    if not frontmatter_match:
        return None, None
    frontmatter = frontmatter_match.group(1)
    version_match = re.search(
        r"^version:\s*([0-9]+\.[0-9]+\.[0-9]+)\s*$", frontmatter, re.MULTILINE
    )
    policy_match = re.search(
        r"^\s+policy_version:\s*([0-9]+\.[0-9]+\.[0-9]+)\s*$", frontmatter, re.MULTILINE
    )
    return (
        version_match.group(1) if version_match else None,
        policy_match.group(1) if policy_match else None,
    )


def check_versions(repo_root, policy, errors):
    version = policy.get("version")
    md_version = doc_version((repo_root / POLICY_DOC).read_text(encoding="utf-8"))
    if md_version is None:
        errors.append(f"{POLICY_DOC}: missing 'Version: X.Y.Z' line")
    elif md_version != version:
        errors.append(f"{POLICY_DOC}: version {md_version} != policy {version}")

    skill_version, skill_policy_version = skill_versions(
        (repo_root / SKILL_DOC).read_text(encoding="utf-8")
    )
    if skill_version is None:
        errors.append(f"{SKILL_DOC}: missing frontmatter 'version:' field")
    if skill_policy_version is None:
        errors.append(f"{SKILL_DOC}: missing frontmatter 'policy_version:' field")
    elif skill_policy_version != version:
        errors.append(
            f"{SKILL_DOC}: policy_version {skill_policy_version} != policy {version}"
        )


def lint(repo_root: Path) -> list[str]:
    errors: list[str] = []
    try:
        policy = json.loads((repo_root / POLICY_JSON).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return [f"{POLICY_JSON}: {error}"]
    try:
        schema = json.loads((repo_root / POLICY_SCHEMA).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return [f"{POLICY_SCHEMA}: {error}"]

    validate_schema(policy, schema, "$", errors)
    check_invariants(policy, errors)
    for check in (
        check_runtime_constants,
        check_driver_signal,
        check_harness_doc,
        check_versions,
    ):
        try:
            check(repo_root, policy, errors)
        except OSError as error:
            errors.append(str(error))
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (defaults to the checkout containing this script)",
    )
    args = parser.parse_args()
    errors = lint(args.repo_root)
    if errors:
        for error in errors:
            print(f"policy-lint: {error}", file=sys.stderr)
        return 1
    print("policy-lint: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

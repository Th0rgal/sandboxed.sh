#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "== Harness contract tests: shared CLI invariants =="
cargo test --workspace --lib text_delta_does_not_contain_thinking_content
cargo test --workspace --lib thinking_produces_thinking_event
cargo test --workspace --lib tool_call_stored_for_result_correlation
cargo test --workspace --lib error_result_preserves_message

echo "== Harness contract tests: OpenCode SSE parser invariants =="
cargo test --workspace --lib opencode_sse_

echo "== Harness contract tests: harness event parser baselines =="
cargo test --workspace --lib test_parse_stream_event_delta
cargo test --workspace --lib test_parse_assistant_event
cargo test --workspace --lib test_parse_result_event

echo "Harness contract tests passed."

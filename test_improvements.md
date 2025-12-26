# Testing Agent Improvements

This document describes how to test the agent improvements implemented in this branch.

## Prerequisites

1. Set up environment variables:
```bash
export OPENROUTER_API_KEY="sk-or-v1-..."
export DEV_MODE=true
```

2. Build the project:
```bash
cargo build --release
```

3. Start the server:
```bash
./target/release/open_agent
```

## New Features to Test

### 1. Smart Tool Result Handling

**What changed**: Large tool results are now truncated more intelligently with:
- UTF-8 safe boundaries
- Newline-aware breaks
- Helpful message about what was truncated

**How to test**:
```bash
# Submit a task that produces large output
curl -X POST http://localhost:3000/api/task \
  -H "Content-Type: application/json" \
  -d '{"description": "List all files recursively in /usr", "budget_cents": 20}'
```

### 2. Enhanced Pivot Prompts (Loop Detection)

**What changed**: When the agent gets stuck in a loop, it now receives:
- Category-specific suggestions (e.g., file ops vs network)
- Alternative tool recommendations
- Clear warnings about remaining attempts

**How to test**:
Create a task that might cause looping:
```bash
curl -X POST http://localhost:3000/api/task \
  -H "Content-Type: application/json" \
  -d '{"description": "Find a file that does not exist anywhere", "budget_cents": 20}'
```

### 3. Configurable Thresholds

**What changed**: Execution thresholds can now be configured via environment variables.

**Environment variables**:
- `LOOP_WARNING_THRESHOLD` (default: 2)
- `LOOP_FORCE_COMPLETE_THRESHOLD` (default: 4)
- `EMPTY_RESPONSE_WARNING_THRESHOLD` (default: 2)
- `EMPTY_RESPONSE_FORCE_COMPLETE_THRESHOLD` (default: 4)
- `TOOL_FAILURE_THRESHOLD` (default: 3)
- `MAX_TOOL_RESULT_CHARS` (default: 15000)

**How to test**:
```bash
LOOP_WARNING_THRESHOLD=1 LOOP_FORCE_COMPLETE_THRESHOLD=2 ./target/release/open_agent
```

### 4. Tool Failure Tracking

**What changed**: When tools in the same category fail repeatedly, the agent gets:
- Category-specific fallback suggestions
- Cross-category alternatives that haven't been tried

**How to test**:
```bash
curl -X POST http://localhost:3000/api/task \
  -H "Content-Type: application/json" \
  -d '{"description": "Clone a private repository without authentication", "budget_cents": 20}'
```

### 5. Benchmark-Based Model Routing

**What changed**: When `USE_BENCHMARK_ROUTING=true`, the agent selects models based on:
- Task type inference from description (code, math, reasoning, etc.)
- Benchmark scores for each model on that task type
- Preference for cost-effective models (Gemini, Qwen, DeepSeek)

**How to test**:
```bash
USE_BENCHMARK_ROUTING=true ./target/release/open_agent
```

Then submit different task types:
```bash
# Math task - should prefer models with high math scores
curl -X POST http://localhost:3000/api/task \
  -H "Content-Type: application/json" \
  -d '{"description": "Calculate the derivative of x^3 + 2x^2 - 5x + 3", "budget_cents": 10}'

# Code task - should prefer models with high code scores
curl -X POST http://localhost:3000/api/task \
  -H "Content-Type: application/json" \
  -d '{"description": "Write a Python function to sort a list using quicksort", "budget_cents": 20}'
```

### 6. Composite Tools

**New tools added**:
- `analyze_codebase` - Get a structured overview of a codebase
- `deep_search` - Search for patterns with context
- `prepare_project` - Check project setup and dependencies
- `debug_error` - Parse error messages and suggest fixes

**How to test**:
```bash
curl -X POST http://localhost:3000/api/task \
  -H "Content-Type: application/json" \
  -d '{"description": "Use analyze_codebase to understand this project structure", "budget_cents": 15}'
```

## Expected Improvements

1. **Fewer stuck loops**: Agent should break out of loops faster with better suggestions
2. **Smarter model selection**: Right model for the right task
3. **Better error recovery**: Category-aware fallback suggestions
4. **More efficient workflows**: Composite tools reduce iteration count
5. **Configurable behavior**: Tune thresholds without code changes

## Verification Checklist

- [ ] Build succeeds: `cargo build --release`
- [ ] Tests pass: `cargo test agents::improvements`
- [ ] Server starts without errors
- [ ] Tasks complete without unexpected panics
- [ ] Loop detection fires and provides helpful suggestions
- [ ] Tool failures show category-specific alternatives
- [ ] Composite tools work as expected

# Open Agent Blockers

This document tracks critical blockers preventing full completion of Open Agent development.

## Critical Blockers

### 1. OpenCode Authentication Required (Severity: BLOCKER)

**Status**: ❌ Unresolved

**Description**:
Open Agent's backend is architected to exclusively use OpenCode as its execution engine. OpenCode requires authentication via Anthropic OAuth, which has expired and needs interactive browser-based re-authentication.

**Impact**:
- Cannot execute any missions
- Cannot test 10+ mission scenarios
- Blocks all automated testing of core functionality
- Prevents validation of agent/workspace system

**Evidence**:
```rust
// src/api/routes.rs:69-70
// Always use OpenCode backend
let root_agent: AgentRef = Arc::new(OpenCodeAgent::new(config.clone()));
```

**Error Message**:
```
OpenCode error: {"data":{"message":"Error: Token refresh failed: 400"},"name":"UnknownError"}
```

**Root Cause**:
1. OpenCode uses OAuth tokens for Anthropic API access
2. OAuth token has expired (likely after ~1 hour)
3. Token refresh fails with HTTP 400
4. Re-authentication requires interactive browser flow
5. Cannot automate OAuth flow in headless environment

**Resolution Options**:

**Option A: User Re-authenticates OpenCode** (Quick, temporary)
- User runs: `opencode auth login`
- Completes OAuth flow in browser
- Pros: Immediate fix, uses existing architecture
- Cons: Temporary (token expires again), requires user action

**Option B: Implement Direct Anthropic API Backend** (Medium effort)
- Create new agent type: `AnthropicAgent`
- Use API key instead of OAuth
- Modify `src/api/routes.rs` to conditionally create agent based on config
- Add `ANTHROPIC_API_KEY` environment variable
- Pros: No expiration, automatable
- Cons: Requires code changes, testing

**Option C: Implement OpenRouter Backend** (Medium effort)
- Create new agent type: `OpenRouterAgent`
- Use existing `OpenRouterClient` from `src/llm/`
- Already has `OPENROUTER_API_KEY` in `.env.example`
- Pros: Already partially implemented, API key based
- Cons: Requires code changes, different model access

**Option D: Hybrid Approach** (Best long-term)
- Support multiple backends: OpenCode, Anthropic, OpenRouter
- Let user choose via `AGENT_BACKEND` env var (already in .env.example)
- Default to OpenCode for Claude Max users
- Fallback to direct API for automation/CI
- Pros: Maximum flexibility, no lock-in
- Cons: Most code changes

**Recommended**: Option A for immediate unblocking + Option D for production

### 2. Playwright Tests Not Executing (Severity: HIGH)

**Status**: ⚠️ In Progress

**Description**:
Playwright tests created but hanging during execution. Tests appear to wait indefinitely without producing output or completing.

**Impact**:
- Cannot verify dashboard functionality
- No automated regression testing
- Web features untested

**Investigation**:
- Playwright browsers installed (Firefox, Webkit)
- Test files created with 13 tests total
- Dev server running on port 3001
- Tests hang when executed via `bunx playwright test`

**Possible Causes**:
1. Playwright waiting for dev server that's not responding correctly
2. Test configuration issue with webServer startup
3. Port conflicts or network issues
4. Tests waiting for elements that don't appear (due to backend issues)

**Next Steps**:
1. Run single test file in headed mode to see what's happening
2. Check Playwright webServer configuration
3. Verify dev server health manually
4. Simplify tests to minimal assertions

## Medium Priority Issues

### 3. iOS Dashboard Not Tested

**Status**: ⏳ Pending

**Description**:
iOS dashboard created with Agent and Workspace views but not tested in iOS Simulator.

**Impact**:
- Unknown if iOS app compiles
- Cross-platform sync untested
- Picture-in-picture feature untested

**Next Steps**:
- Build iOS app in Xcode
- Test in iOS Simulator
- Verify mission sync between iOS and web

### 4. Library Sync Not Implemented

**Status**: ⏳ Pending

**Description**:
Library system (skills, commands, MCPs) has CRUD operations but lacks:
- Git sync functionality completion
- Conflict resolution
- Version management across devices

**Impact**:
- Manual library management only
- No true sync between local agents and remote

### 5. Overview Page Not Showing Real Metrics

**Status**: ⏳ Pending

**Description**:
Overview page exists but shows placeholder/static data instead of real metrics (CPU, RAM, network, cost).

**Impact**:
- User cannot monitor system resources
- Cost tracking not visible
- Less useful dashboard

## Architectural Findings

### OpenCode-Only Architecture

The current implementation has hardcoded OpenCode as the only backend:

```rust
// src/agents/mod.rs:1-4
//! Agents module - task execution via OpenCode.
//!
//! # Agent Types
//! - **OpenCodeAgent**: Delegates task execution to an OpenCode server (Claude Max)
```

Despite `.env.example` mentioning `AGENT_BACKEND` can be "opencode" or "local", only OpenCode is implemented. The "local" backend does not exist in the codebase.

### Implications

1. **Dependency**: Hard dependency on external OpenCode service
2. **Authentication**: Requires OAuth flow, cannot use simple API keys
3. **Availability**: Service must be running and authenticated
4. **Testing**: Cannot run automated tests without OpenCode
5. **Deployment**: Production requires OpenCode server

### Suggested Architecture Improvements

1. **Abstract Agent Interface**: Already exists (`Agent` trait)
2. **Multiple Implementations**: Add `DirectAgent` using Anthropic/OpenRouter APIs
3. **Factory Pattern**: Choose agent based on configuration
4. **Graceful Degradation**: Fall back to direct API if OpenCode unavailable

## Summary

**Total Blockers**: 2 critical, 3 medium

**Can Complete Without Resolution**: No - mission testing is core requirement

**Estimated Effort to Unblock**:
- Option A (Re-auth): 5 minutes (user action)
- Option B/C (New backend): 4-8 hours (development)
- Option D (Hybrid): 8-16 hours (development + testing)

**Recommended Path**:
1. User re-authenticates OpenCode (immediate)
2. Complete mission testing (4-8 hours)
3. Fix Playwright tests (2-4 hours)
4. Test iOS app (2 hours)
5. Implement hybrid backend for future (defer to v2)

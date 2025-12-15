# Open Agent - Cursor Rules & Project Philosophy

## Project Overview

Open Agent is a minimal autonomous coding agent implemented in Rust. It is designed to be:
- **AI-maintainable**: Rust's strong type system and compiler provide immediate feedback
- **Self-contained**: No external dependencies beyond OpenRouter for LLM access
- **Full-access**: Has complete access to the local machine (filesystem, terminal, network)
- **Provable**: Code structured for future formal verification in Lean

## Architecture (v2: Hierarchical Agent Tree)

### Agent Hierarchy
```
                    ┌─────────────┐
                    │  RootAgent  │
                    └──────┬──────┘
         ┌─────────────────┼─────────────────┐
         ▼                 ▼                 ▼
 ┌───────────────┐ ┌─────────────┐ ┌─────────────┐ ┌──────────┐
 │ Complexity    │ │   Model     │ │    Task     │ │ Verifier │
 │ Estimator     │ │  Selector   │ │  Executor   │ │          │
 └───────────────┘ └─────────────┘ └─────────────┘ └──────────┘
```

### Agent Types

| Type | Role | Children |
|------|------|----------|
| **RootAgent** | Top-level orchestrator, receives API tasks | All leaf types |
| **NodeAgent** | Intermediate orchestrator for subtasks | Executor, Verifier |
| **ComplexityEstimator** | Estimates task difficulty (0-1 score) | None (leaf) |
| **ModelSelector** | Picks optimal model (U-curve optimization) | None (leaf) |
| **TaskExecutor** | Executes tasks using tools | None (leaf) |
| **Verifier** | Validates completion (hybrid) | None (leaf) |

### Task Flow
1. Receive task via HTTP API
2. **Estimate Complexity** (ComplexityEstimator)
3. If complex: **Split into subtasks** with budget allocation
4. **Select Model** for each (sub)task (U-curve cost optimization)
5. **Execute** using tools (TaskExecutor)
6. **Verify** completion (Verifier: programmatic → LLM fallback)
7. Aggregate results and return

### U-Curve Model Selection
```
Cost
  ^
  |    *                         *
  |     *                       *
  |        *       *         *
  |           * *     * *
  |            *       *
  +-------------------------> Model Capability
      (cheap/weak)    (optimal)    (expensive/strong)
```
- Cheap models: low per-token cost, high failure rate, more retries
- Expensive models: high per-token cost, low failure rate
- **Optimal**: minimizes expected total cost

## Module Structure

```
src/
├── agents/                # Hierarchical agent system
│   ├── mod.rs             # Agent traits (Agent, OrchestratorAgent, LeafAgent)
│   ├── types.rs           # AgentId, AgentType, AgentResult, Complexity
│   ├── context.rs         # Shared context for agent tree
│   ├── tree.rs            # Tree structure management
│   ├── orchestrator/      # Orchestrator agents
│   │   ├── root.rs        # RootAgent (top-level)
│   │   └── node.rs        # NodeAgent (intermediate)
│   └── leaf/              # Leaf agents (specialized workers)
│       ├── complexity.rs  # ComplexityEstimator
│       ├── model_select.rs # ModelSelector with U-curve
│       ├── executor.rs    # TaskExecutor (tools in a loop)
│       └── verifier.rs    # Hybrid verification
├── task/                  # Task types with invariants
│   ├── task.rs            # Task, TaskId, TaskStatus
│   ├── subtask.rs         # Subtask, SubtaskPlan
│   └── verification.rs    # VerificationCriteria, ProgrammaticCheck
├── budget/                # Cost tracking and pricing
│   ├── budget.rs          # Budget with spend/allocate invariants
│   ├── pricing.rs         # OpenRouter pricing client
│   └── allocation.rs      # Budget allocation strategies
├── memory/                # Persistent memory & retrieval
│   ├── mod.rs             # Memory subsystem exports
│   ├── supabase.rs        # PostgREST + Storage client
│   ├── embed.rs           # OpenRouter embeddings (Qwen3 8B)
│   ├── rerank.rs          # Reranker for precision retrieval
│   ├── writer.rs          # Event recording + chunking
│   ├── retriever.rs       # Semantic search + context packing
│   └── types.rs           # DbRun, DbTask, DbEvent, DbChunk
├── api/                   # HTTP interface
├── llm/                   # LLM client (OpenRouter)
├── tools/                 # Tool implementations
└── config.rs              # Configuration
```

## Memory System

### Purpose
- **Long tasks beyond context**: persist step-by-step execution so the agent can retrieve relevant context later
- **Fast query + browsing**: structured metadata in Postgres, heavy blobs in Storage
- **Embedding + rerank**: Qwen3 Embedding 8B for vectors, Qwen reranker for precision
- **Learning from execution**: store predictions vs actuals to improve estimates over time

### Data Flow
1. Agents emit events via `EventRecorder`
2. `MemoryWriter` persists to Supabase Postgres + Storage
3. Before LLM calls, `MemoryRetriever` fetches relevant context
4. On completion, run is archived with summary embedding
5. **Task outcomes recorded for learning** (complexity, cost, tokens, success)

### Storage Strategy
- **Postgres (pgvector)**: runs, tasks (hierarchical), events (preview), chunks (embeddings), **task_outcomes**
- **Supabase Storage**: full event streams (jsonl), large artifacts

## Learning System (v3)

### Purpose
Enable data-driven optimization of:
- **Complexity estimation**: learn actual token usage vs predicted
- **Model selection**: learn actual success rates per model/complexity
- **Budget allocation**: learn actual costs vs estimated

### Architecture
```
┌──────────────────────────────────────────────────────────────────────┐
│                    Memory-Enhanced Agent Flow                         │
├──────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  ┌──────────┐    Query similar     ┌─────────────────────────────┐   │
│  │ New Task │ ───────────────────▶│ MemoryRetriever             │   │
│  └────┬─────┘      past tasks      │  - find_similar_tasks()     │   │
│       │                            │  - get_historical_context() │   │
│       ▼                            │  - get_model_stats()        │   │
│  ┌────────────────┐                └───────────────┬─────────────┘   │
│  │ Complexity     │◀── historical context ─────────┘                 │
│  │ Estimator      │    (avg token ratio, avg cost ratio)             │
│  │ (enhanced)     │                                                  │
│  └────────┬───────┘                                                  │
│           │                                                          │
│           ▼                                                          │
│  ┌────────────────┐   Query: "models at complexity ~0.6"             │
│  │ Model Selector │   Returns: actual success rates, cost ratios     │
│  │ (enhanced)     │                                                  │
│  └────────┬───────┘                                                  │
│           │                                                          │
│           ▼                                                          │
│  ┌────────────────┐                                                  │
│  │ TaskExecutor   │──▶ record_task_outcome() ──▶ task_outcomes      │
│  └────────────────┘                                                  │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘
```

### Database Schema: `task_outcomes`
```sql
CREATE TABLE task_outcomes (
    id uuid PRIMARY KEY,
    run_id uuid REFERENCES runs(id),
    task_id uuid REFERENCES tasks(id),
    
    -- Predictions
    predicted_complexity float,
    predicted_tokens bigint,
    predicted_cost_cents bigint,
    selected_model text,
    
    -- Actuals
    actual_tokens bigint,
    actual_cost_cents bigint,
    success boolean,
    iterations int,
    tool_calls_count int,
    
    -- Computed ratios (actual/predicted)
    cost_error_ratio float,
    token_error_ratio float,
    
    -- Similarity search
    task_description text,
    task_embedding vector(1536)
);
```

### RPC Functions
- `get_model_stats(complexity_min, complexity_max)` - Model performance by complexity tier
- `search_similar_outcomes(embedding, threshold, limit)` - Find similar past tasks
- `get_global_learning_stats()` - Overall system metrics

### Learning Integration Points
1. **ComplexityEstimator**: Query similar tasks → adjust token estimate by `avg_token_ratio`
2. **ModelSelector**: Query model stats → use actual success rates instead of heuristics
3. **TaskExecutor**: After execution → call `record_task_outcome()` with all metrics
4. **Budget**: Use historical cost ratios to add appropriate safety margins

## Design for Provability

### Conventions for Future Lean Proofs
1. **Pre/Postconditions**: Document as `/// Precondition:` and `/// Postcondition:` comments
2. **Invariants**: Document struct invariants, enforce in constructors
3. **Algebraic Types**: Use enums with exhaustive matching, no `_` catch-all
4. **Pure Functions**: Separate pure logic from IO where possible
5. **Result Types**: Never panic, always return `Result`

### Example
```rust
/// Allocate budget for a subtask.
/// 
/// # Precondition
/// `amount <= self.remaining_cents()`
/// 
/// # Postcondition
/// `self.allocated_cents` increases by exactly `amount`
pub fn allocate(&mut self, amount: u64) -> Result<(), BudgetError>
```

## Adding a New Leaf Agent

1. Create `src/agents/leaf/your_agent.rs`
2. Implement `Agent` trait:
   - `id()`, `agent_type()`, `execute()`
3. Implement `LeafAgent` trait:
   - `capability()` → add variant to `LeafCapability` enum
4. Register in `RootAgent::new()` or relevant orchestrator
5. Document pre/postconditions for provability

## API Contract

```
POST /api/task              - Submit task (uses hierarchical agent)
GET  /api/task/{id}         - Get task status and result
GET  /api/task/{id}/stream  - Stream progress via SSE
GET  /api/health            - Health check
GET  /api/runs              - List archived runs
GET  /api/runs/{id}         - Run detail + task tree
GET  /api/runs/{id}/events  - Event timeline
GET  /api/memory/search     - Semantic search across memory
```

## Environment Variables

```
OPENROUTER_API_KEY       - Required. Your OpenRouter API key
DEFAULT_MODEL            - Optional. Default: anthropic/claude-sonnet-4.5
WORKSPACE_PATH           - Optional. Default: current directory
HOST                     - Optional. Default: 127.0.0.1
PORT                     - Optional. Default: 3000
MAX_ITERATIONS           - Optional. Default: 50
SUPABASE_URL             - Required for memory. Supabase project URL
SUPABASE_SERVICE_ROLE_KEY - Required for memory. Service role key
MEMORY_EMBED_MODEL       - Optional. Default: qwen/qwen3-embedding-8b
MEMORY_RERANK_MODEL      - Optional. Default: qwen/qwen3-reranker-8b
```

### Recommended Models
- **Default (tools)**: `anthropic/claude-sonnet-4.5` - Best coding, 1M context, $3/$15 per 1M tokens
- **Budget fallback**: `anthropic/claude-3.5-haiku` - Fast, cheap, good for simple tasks
- **Complex tasks**: `anthropic/claude-opus-4.5` - Highest capability when needed

## Security Considerations

This agent has **full machine access**. It can:
- Read/write any file the process can access
- Execute any shell command
- Make network requests

When deploying:
- Run as a limited user
- Use workspace isolation
- Consider a sandbox for terminal commands
- Never expose the API publicly without authentication
- Keep `.env` out of version control

## Future Work

- [ ] Formal verification in Lean (extract pure logic)
- [ ] WebSocket for bidirectional streaming
- [ ] Budget overflow strategies (fallback to cheaper model, request extension)
- [ ] Enhanced ComplexityEstimator with historical context injection
- [ ] Enhanced ModelSelector with data-driven success rates
- [x] Semantic code search (embeddings-based)
- [x] Multi-model support (U-curve optimization)
- [x] Cost tracking (Budget system)
- [x] Persistent memory (Supabase + pgvector)
- [x] Learning system (task_outcomes table, historical queries)


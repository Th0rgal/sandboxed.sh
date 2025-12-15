-- Migration: Task Outcomes for Learning
-- This table stores predictions vs actuals for each task execution,
-- enabling the agent to learn optimal model selection and cost estimation.

-- ============================================================================
-- Table: task_outcomes
-- ============================================================================

CREATE TABLE IF NOT EXISTS task_outcomes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id uuid REFERENCES runs(id) ON DELETE CASCADE,
    task_id uuid REFERENCES tasks(id) ON DELETE CASCADE,
    
    -- Predictions (what we estimated before execution)
    predicted_complexity float,
    predicted_tokens bigint,
    predicted_cost_cents bigint,
    selected_model text,
    
    -- Actuals (what happened during execution)
    actual_tokens bigint,
    actual_cost_cents bigint,
    success boolean NOT NULL DEFAULT false,
    iterations int,
    tool_calls_count int,
    
    -- Metadata for similarity search
    task_description text NOT NULL,
    task_type text,  -- 'file_create', 'refactor', 'debug', etc.
    task_embedding vector(1536),  -- For finding similar tasks
    
    -- Computed ratios for quick stats
    cost_error_ratio float,  -- actual/predicted (1.0 = accurate)
    token_error_ratio float,
    
    created_at timestamptz DEFAULT now()
);

-- Indexes for efficient queries
CREATE INDEX IF NOT EXISTS idx_outcomes_run_id ON task_outcomes(run_id);
CREATE INDEX IF NOT EXISTS idx_outcomes_task_id ON task_outcomes(task_id);
CREATE INDEX IF NOT EXISTS idx_outcomes_model ON task_outcomes(selected_model, success);
CREATE INDEX IF NOT EXISTS idx_outcomes_complexity ON task_outcomes(predicted_complexity);
CREATE INDEX IF NOT EXISTS idx_outcomes_created ON task_outcomes(created_at DESC);

-- Vector index for similarity search (HNSW)
CREATE INDEX IF NOT EXISTS idx_outcomes_embedding ON task_outcomes 
    USING hnsw (task_embedding vector_cosine_ops);

-- ============================================================================
-- RPC: get_model_stats
-- Returns aggregated statistics per model for a given complexity range.
-- ============================================================================

CREATE OR REPLACE FUNCTION get_model_stats(
    complexity_min float DEFAULT 0.0,
    complexity_max float DEFAULT 1.0
)
RETURNS TABLE (
    model_id text,
    success_rate float,
    avg_cost_ratio float,
    avg_token_ratio float,
    avg_iterations float,
    sample_count bigint
)
LANGUAGE sql STABLE
AS $$
    SELECT 
        selected_model as model_id,
        AVG(CASE WHEN success THEN 1.0 ELSE 0.0 END) as success_rate,
        COALESCE(AVG(cost_error_ratio), 1.0) as avg_cost_ratio,
        COALESCE(AVG(token_error_ratio), 1.0) as avg_token_ratio,
        COALESCE(AVG(iterations), 1.0) as avg_iterations,
        COUNT(*) as sample_count
    FROM task_outcomes
    WHERE 
        selected_model IS NOT NULL
        AND predicted_complexity >= complexity_min
        AND predicted_complexity <= complexity_max
    GROUP BY selected_model
    HAVING COUNT(*) >= 3  -- Minimum samples for reliability
    ORDER BY success_rate DESC, avg_cost_ratio ASC;
$$;

-- ============================================================================
-- RPC: search_similar_outcomes
-- Find similar past task outcomes by embedding similarity.
-- ============================================================================

CREATE OR REPLACE FUNCTION search_similar_outcomes(
    query_embedding vector(1536),
    match_threshold float DEFAULT 0.7,
    match_count int DEFAULT 5
)
RETURNS TABLE (
    id uuid,
    run_id uuid,
    task_id uuid,
    predicted_complexity float,
    predicted_tokens bigint,
    predicted_cost_cents bigint,
    selected_model text,
    actual_tokens bigint,
    actual_cost_cents bigint,
    success boolean,
    iterations int,
    tool_calls_count int,
    task_description text,
    task_type text,
    cost_error_ratio float,
    token_error_ratio float,
    created_at timestamptz,
    similarity float
)
LANGUAGE sql STABLE
AS $$
    SELECT 
        o.id,
        o.run_id,
        o.task_id,
        o.predicted_complexity,
        o.predicted_tokens,
        o.predicted_cost_cents,
        o.selected_model,
        o.actual_tokens,
        o.actual_cost_cents,
        o.success,
        o.iterations,
        o.tool_calls_count,
        o.task_description,
        o.task_type,
        o.cost_error_ratio,
        o.token_error_ratio,
        o.created_at,
        1 - (o.task_embedding <=> query_embedding) as similarity
    FROM task_outcomes o
    WHERE 
        o.task_embedding IS NOT NULL
        AND 1 - (o.task_embedding <=> query_embedding) > match_threshold
    ORDER BY o.task_embedding <=> query_embedding
    LIMIT match_count;
$$;

-- ============================================================================
-- RPC: get_global_learning_stats
-- Returns overall system learning statistics for monitoring/tuning.
-- ============================================================================

CREATE OR REPLACE FUNCTION get_global_learning_stats()
RETURNS json
LANGUAGE sql STABLE
AS $$
    SELECT json_build_object(
        'total_outcomes', (SELECT COUNT(*) FROM task_outcomes),
        'success_rate', (SELECT AVG(CASE WHEN success THEN 1.0 ELSE 0.0 END) FROM task_outcomes),
        'avg_cost_error', (SELECT AVG(cost_error_ratio) FROM task_outcomes WHERE cost_error_ratio IS NOT NULL),
        'avg_token_error', (SELECT AVG(token_error_ratio) FROM task_outcomes WHERE token_error_ratio IS NOT NULL),
        'models_used', (SELECT COUNT(DISTINCT selected_model) FROM task_outcomes),
        'top_models', (
            SELECT json_agg(row_to_json(t))
            FROM (
                SELECT 
                    selected_model,
                    COUNT(*) as uses,
                    AVG(CASE WHEN success THEN 1.0 ELSE 0.0 END) as success_rate
                FROM task_outcomes
                WHERE selected_model IS NOT NULL
                GROUP BY selected_model
                ORDER BY uses DESC
                LIMIT 5
            ) t
        ),
        'complexity_distribution', (
            SELECT json_agg(row_to_json(t))
            FROM (
                SELECT 
                    CASE 
                        WHEN predicted_complexity < 0.2 THEN 'trivial'
                        WHEN predicted_complexity < 0.4 THEN 'simple'
                        WHEN predicted_complexity < 0.6 THEN 'moderate'
                        WHEN predicted_complexity < 0.8 THEN 'complex'
                        ELSE 'very_complex'
                    END as tier,
                    COUNT(*) as count,
                    AVG(CASE WHEN success THEN 1.0 ELSE 0.0 END) as success_rate
                FROM task_outcomes
                WHERE predicted_complexity IS NOT NULL
                GROUP BY tier
            ) t
        )
    );
$$;

-- ============================================================================
-- RPC: get_optimal_model_for_complexity
-- Returns the best model for a given complexity based on historical data.
-- ============================================================================

CREATE OR REPLACE FUNCTION get_optimal_model_for_complexity(
    target_complexity float,
    budget_cents bigint DEFAULT 1000
)
RETURNS TABLE (
    model_id text,
    expected_success_rate float,
    expected_cost_cents float,
    confidence float
)
LANGUAGE sql STABLE
AS $$
    WITH model_perf AS (
        SELECT 
            selected_model,
            AVG(CASE WHEN success THEN 1.0 ELSE 0.0 END) as success_rate,
            AVG(actual_cost_cents) as avg_cost,
            COUNT(*) as samples,
            -- Confidence based on sample size and recency
            LEAST(1.0, COUNT(*) / 10.0) as sample_confidence
        FROM task_outcomes
        WHERE 
            selected_model IS NOT NULL
            AND ABS(predicted_complexity - target_complexity) < 0.2
            AND created_at > now() - interval '30 days'
        GROUP BY selected_model
    )
    SELECT 
        selected_model as model_id,
        success_rate as expected_success_rate,
        avg_cost as expected_cost_cents,
        sample_confidence as confidence
    FROM model_perf
    WHERE avg_cost <= budget_cents OR samples < 3  -- Allow trying new models
    ORDER BY 
        -- Balance success rate and cost
        (success_rate * 0.7 + (1.0 - LEAST(avg_cost / budget_cents, 1.0)) * 0.3) DESC
    LIMIT 3;
$$;


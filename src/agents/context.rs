//! Agent execution context - shared state across the agent tree.

use std::path::PathBuf;
use std::sync::Arc;

use crate::budget::ModelPricing;
use crate::config::Config;
use crate::llm::LlmClient;
use crate::memory::MemorySystem;
use crate::tools::ToolRegistry;
use tokio::sync::broadcast;

/// Shared context passed to all agents during execution.
/// 
/// # Thread Safety
/// Context is wrapped in Arc for sharing across async tasks.
/// Individual components use interior mutability where needed.
/// 
/// # System Access
/// The agent has **full system access** - it can read/write any file, execute any command,
/// and search anywhere on the machine. The `working_dir` is just the default for relative paths.
pub struct AgentContext {
    /// Application configuration
    pub config: Config,
    
    /// LLM client for model calls
    pub llm: Arc<dyn LlmClient>,
    
    /// Tool registry for task execution
    pub tools: ToolRegistry,
    
    /// Model pricing information
    pub pricing: Arc<ModelPricing>,
    
    /// Default working directory for relative paths.
    /// The agent can still access any path on the system using absolute paths.
    pub working_dir: PathBuf,
    
    /// Maximum depth for recursive task splitting
    pub max_split_depth: usize,
    
    /// Maximum iterations per agent
    pub max_iterations: usize,
    
    /// Memory system for persistent storage (optional)
    pub memory: Option<MemorySystem>,

    /// Optional event sink for streaming agent events (e.g. control session SSE).
    pub control_events: Option<broadcast::Sender<crate::api::control::AgentEvent>>,

    /// Optional hub for awaiting frontend (interactive) tool results.
    pub frontend_tool_hub: Option<Arc<crate::api::control::FrontendToolHub>>,

    /// Optional shared control-session status (so the executor can switch to WaitingForTool).
    pub control_status: Option<Arc<tokio::sync::RwLock<crate::api::control::ControlStatus>>>,

    /// Optional cancellation token for cooperative cancellation.
    pub cancel_token: Option<tokio_util::sync::CancellationToken>,
}

impl AgentContext {
    /// Create a new agent context.
    pub fn new(
        config: Config,
        llm: Arc<dyn LlmClient>,
        tools: ToolRegistry,
        pricing: Arc<ModelPricing>,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            max_iterations: config.max_iterations,
            config,
            llm,
            tools,
            pricing,
            working_dir,
            max_split_depth: 3, // Default max recursion for splitting
            memory: None,
            control_events: None,
            frontend_tool_hub: None,
            control_status: None,
            cancel_token: None,
        }
    }
    
    /// Create a new agent context with memory system.
    pub fn with_memory(
        config: Config,
        llm: Arc<dyn LlmClient>,
        tools: ToolRegistry,
        pricing: Arc<ModelPricing>,
        working_dir: PathBuf,
        memory: Option<MemorySystem>,
    ) -> Self {
        Self {
            max_iterations: config.max_iterations,
            config,
            llm,
            tools,
            pricing,
            working_dir,
            max_split_depth: 3,
            memory,
            control_events: None,
            frontend_tool_hub: None,
            control_status: None,
            cancel_token: None,
        }
    }

    /// Create a child context with reduced split depth.
    /// 
    /// # Postcondition
    /// `child.max_split_depth == self.max_split_depth - 1`
    pub fn child_context(&self) -> Self {
        Self {
            config: self.config.clone(),
            llm: Arc::clone(&self.llm),
            tools: ToolRegistry::new(), // Fresh tools for isolation
            pricing: Arc::clone(&self.pricing),
            working_dir: self.working_dir.clone(),
            max_split_depth: self.max_split_depth.saturating_sub(1),
            max_iterations: self.max_iterations,
            memory: self.memory.clone(),
            control_events: self.control_events.clone(),
            frontend_tool_hub: self.frontend_tool_hub.clone(),
            control_status: self.control_status.clone(),
            cancel_token: self.cancel_token.clone(),
        }
    }

    /// Check if further task splitting is allowed.
    pub fn can_split(&self) -> bool {
        self.max_split_depth > 0
    }

    /// Get the working directory path as a string.
    pub fn working_dir_str(&self) -> String {
        self.working_dir.to_string_lossy().to_string()
    }
    
    /// Check if memory is available.
    pub fn has_memory(&self) -> bool {
        self.memory.is_some()
    }

    /// Check if cooperative cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token
            .as_ref()
            .map(|t| t.is_cancelled())
            .unwrap_or(false)
    }
}


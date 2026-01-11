//! Open Agent - HTTP Server Entry Point
//!
//! Starts the HTTP server that exposes the agent API.

use open_agent::{api, config::Config};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "open_agent=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration
    let config = Config::from_env()?;
    info!(
        "Loaded configuration: model={}",
        config.default_model.as_deref().unwrap_or("(opencode default)")
    );
    let context_root = config
        .context
        .context_dir(&config.working_dir.to_string_lossy());
    std::env::set_var("OPEN_AGENT_CONTEXT_ROOT", &context_root);
    std::env::set_var("OPEN_AGENT_CONTEXT_DIR_NAME", &config.context.context_dir_name);
    let runtime_workspace_file = config
        .working_dir
        .join(".openagent")
        .join("runtime")
        .join("current_workspace.json");
    std::env::set_var(
        "OPEN_AGENT_RUNTIME_WORKSPACE_FILE",
        runtime_workspace_file.to_string_lossy().to_string(),
    );

    // Start HTTP server
    let addr = format!("{}:{}", config.host, config.port);
    info!("Starting server on {}", addr);

    api::serve(config).await?;

    Ok(())
}

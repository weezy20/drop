use color_eyre::eyre::{Context, Result};
use drop::{AppState, Config, create_app, initialize_memory_pool};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    info!("Starting drop...💧");

    // Load configuration from environment
    let config = Config::from_env();
    info!(
        "Loaded configuration: bind_address={}, max_file_size={}, temp_directory={:?}",
        config.bind_address, config.max_file_size_limit, config.temp_directory
    );

    // Initialize memory pool based on system memory
    initialize_memory_pool();

    // Create shared state
    let app_state = AppState {
        config: config.clone(),
        file_storage: Arc::new(Mutex::new(HashMap::new())),
        short_url_storage: Arc::new(Mutex::new(HashMap::new())),
        rate_limit_storage: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = create_app(app_state);

    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .with_context(|| format!("Failed to bind to address {}", config.bind_address))?;

    info!("Server running on http://{}", config.bind_address);

    axum::serve(listener, app)
        .await
        .context("Server failed to start")?;

    Ok(())
}

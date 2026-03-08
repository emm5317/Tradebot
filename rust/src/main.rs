mod config;
mod execution;
mod kalshi;
mod logging;
mod orderbook_feed;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fred::interfaces::ClientLike;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file (ignore if missing — production uses real env vars)
    let _ = dotenvy::dotenv();

    let config = config::Config::from_env().context(
        "Failed to load configuration from environment variables. \
         Copy config/.env.example to .env and fill in required values.",
    )?;

    config.validate().map_err(|e| anyhow::anyhow!("Invalid configuration: {e}"))?;

    logging::init(&config.log_level, &config.log_format);
    config.log_startup();

    // Connect to PostgreSQL with configurable pool size
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(config.database_pool_size)
        .connect(&config.database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    let row: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .context("PostgreSQL health check failed")?;
    tracing::info!(result = row.0, "postgresql connected");

    // Connect to Redis with automatic reconnection
    let redis_config = fred::types::config::Config::from_url(&config.redis_url)
        .context("Invalid REDIS_URL")?;
    let reconnect_policy = fred::types::config::ReconnectPolicy::new_exponential(
        0,    // unlimited retries
        1,    // min delay ms
        5000, // max delay ms
        2,    // multiplier
    );
    let redis = fred::clients::Client::new(redis_config, None, None, Some(reconnect_policy));
    redis.connect();
    redis
        .wait_for_connect()
        .await
        .context("Failed to connect to Redis")?;
    let pong: String = fred::interfaces::ClientLike::ping(&redis, None).await
        .context("Redis PING failed")?;
    tracing::info!(response = %pong, "redis connected");

    // Connect to NATS
    let nats = async_nats::connect(&config.nats_url)
        .await
        .context("Failed to connect to NATS")?;
    tracing::info!(server = %config.nats_url, "nats connected");

    // Initialize Kalshi client
    let kalshi_auth = kalshi::auth::KalshiAuth::new(
        config.kalshi_api_key.clone(),
        &config.kalshi_private_key_path,
    ).context("Failed to initialize Kalshi auth")?;
    let kalshi = kalshi::client::KalshiClient::new(
        kalshi_auth,
        config.kalshi_base_url.clone(),
    )?;

    // Shared cancellation token for graceful shutdown
    let cancel = CancellationToken::new();

    // Start Kalshi WebSocket feed → OrderbookManager → Redis pipeline
    let orderbooks = Arc::new(kalshi::orderbook::OrderbookManager::new());
    let (ws_tx, ws_rx) = tokio::sync::mpsc::channel(1024);

    let ws_auth = kalshi::auth::KalshiAuth::new(
        config.kalshi_api_key.clone(),
        &config.kalshi_private_key_path,
    ).context("Failed to create WS auth")?;

    let ws_feed = kalshi::websocket::KalshiWsFeed::new(
        config.kalshi_ws_url.clone(),
        ws_auth,
        cancel.clone(),
    );

    let ws_handle = tokio::spawn(async move {
        ws_feed.run(ws_tx).await;
    });

    let orderbook_handle = tokio::spawn({
        let orderbooks = Arc::clone(&orderbooks);
        let redis = redis.clone();
        let cancel = cancel.clone();
        async move {
            orderbook_feed::run(ws_rx, orderbooks, redis.clone(), cancel).await;
        }
    });

    tracing::info!(
        paper_mode = config.paper_mode,
        max_trade_size = config.max_trade_size_cents,
        max_daily_loss = config.max_daily_loss_cents,
        "all systems operational — tradebot ready"
    );

    // Run execution engine
    let execution_handle = tokio::spawn({
        let config = config.clone();
        let pool = pool.clone();
        async move {
            if let Err(e) = execution::run(&config, nats, pool, kalshi).await {
                tracing::error!(error = %e, "execution engine failed");
            }
        }
    });

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");

    cancel.cancel();
    execution_handle.abort();

    // Graceful shutdown with 10s timeout
    let shutdown = async {
        let _ = ws_handle.await;
        let _ = orderbook_handle.await;
        pool.close().await;
        let _ = redis.quit().await;
    };

    if tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .is_err()
    {
        tracing::warn!("shutdown timed out after 10s, forcing exit");
    }

    Ok(())
}

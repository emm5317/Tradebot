mod config;
mod kalshi;
mod logging;

use std::time::Duration;

use anyhow::{Context, Result};
use fred::interfaces::ClientLike;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file (ignore if missing — production uses real env vars)
    let _ = dotenvy::dotenv();

    let config = config::Config::from_env().context(
        "Failed to load configuration from environment variables. \
         Copy config/.env.example to .env and fill in required values.",
    )?;

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

    // Connect to NATS (held for use by subsystems)
    let _nats = async_nats::connect(&config.nats_url)
        .await
        .context("Failed to connect to NATS")?;
    tracing::info!(server = %config.nats_url, "nats connected");

    tracing::info!("all systems operational — tradebot ready");

    // Keep running until interrupted
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");

    // Graceful shutdown with 10s timeout
    let shutdown = async {
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

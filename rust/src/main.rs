mod config;
mod kalshi;
mod logging;

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

    // Verify database connectivity
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    let row: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .context("PostgreSQL health check failed")?;
    tracing::info!(result = row.0, "postgresql connected");

    // Verify Redis connectivity
    let redis_config = fred::types::config::Config::from_url(&config.redis_url)
        .context("Invalid REDIS_URL")?;
    let redis = fred::clients::Client::new(redis_config, None, None, None);
    redis.connect();
    redis
        .wait_for_connect()
        .await
        .context("Failed to connect to Redis")?;
    let pong: String = fred::interfaces::ClientLike::ping(&redis, None).await
        .context("Redis PING failed")?;
    tracing::info!(response = %pong, "redis connected");

    // Verify NATS connectivity
    let nats = async_nats::connect(&config.nats_url)
        .await
        .context("Failed to connect to NATS")?;
    tracing::info!(server = %config.nats_url, "nats connected");
    drop(nats);

    tracing::info!("all systems operational — tradebot ready");

    // Keep running until interrupted
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");

    pool.close().await;
    redis.quit().await?;

    Ok(())
}

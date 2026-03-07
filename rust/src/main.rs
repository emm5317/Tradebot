mod config;
mod db;
mod execution;
mod kalshi;
mod risk;
mod signal;
mod ui;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    tracing::info!("Tradebot starting…");

    // TODO: load config, connect DB, start subsystems
    Ok(())
}

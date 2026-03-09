mod clock;
mod config;
mod contract_discovery;
mod crypto_evaluator;
mod crypto_fv;
mod crypto_state;
mod dashboard;
mod dead_letter;
mod decision_log;
mod execution;
mod feed_health;
#[cfg(test)]
mod integration_tests;
mod feeds;
mod kalshi;
mod kill_switch;
mod logging;
mod order_manager;
mod orderbook_feed;
mod types;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fred::interfaces::ClientLike;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    // Install rustls crypto provider before any TLS connections
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    // Load .env file (ignore if missing — production uses real env vars)
    let _ = dotenvy::dotenv();

    let config = config::Config::from_env().context(
        "Failed to load configuration from environment variables. \
         Copy config/.env.example to .env and fill in required values.",
    )?;

    logging::init(&config.log_level, &config.log_format);
    config.log_startup();

    // Phase 5.5: Clock discipline — check system clock before startup
    match clock::enforce_clock_discipline(config.paper_mode).await {
        Ok(offset_ms) => {
            tracing::info!(offset_ms = offset_ms, "clock discipline check passed");
        }
        Err(e) => {
            tracing::error!(error = %e, "clock discipline check failed");
            return Err(e);
        }
    }

    // Paper mode startup guard (Phase 0.3)
    if !config.paper_mode {
        tracing::warn!("LIVE TRADING MODE — PAPER_MODE=false");
        tracing::warn!(
            kalshi_base_url = %config.kalshi_base_url,
            "Orders will be submitted with real money"
        );
    } else {
        tracing::info!("Paper trading mode active — no real orders will be submitted");
    }

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

    // Initialize Kalshi client (Arc-shared between execution and crypto evaluator)
    let kalshi_auth = kalshi::auth::KalshiAuth::new(
        config.kalshi_api_key.clone(),
        &config.kalshi_private_key_path,
    ).context("Failed to initialize Kalshi auth")?;
    let kalshi = Arc::new(kalshi::client::KalshiClient::new(
        kalshi_auth,
        config.kalshi_base_url.clone(),
    )?);

    // Shared cancellation token for graceful shutdown
    let cancel = CancellationToken::new();

    // Start Kalshi WebSocket feed → OrderbookManager → Redis pipeline
    let orderbooks = Arc::new(kalshi::orderbook::OrderbookManager::new());
    let (ws_tx, ws_rx) = tokio::sync::mpsc::channel(1024);

    let ws_auth = kalshi::auth::KalshiAuth::new(
        config.kalshi_api_key.clone(),
        &config.kalshi_private_key_path,
    ).context("Failed to create WS auth")?;

    let (ws_feed, ws_sub_handle) = kalshi::websocket::KalshiWsFeed::new(
        config.kalshi_ws_url.clone(),
        ws_auth,
        cancel.clone(),
    );

    let ws_handle = tokio::spawn(async move {
        ws_feed.run(ws_tx).await;
    });

    // Initialize feed health tracker (before feeds so they can report health)
    let feed_health = Arc::new(feed_health::FeedHealth::new());

    // Shared trade tape for orderbook feed + crypto evaluator (Phase 4.3)
    let trade_tape = Arc::new(std::sync::RwLock::new(
        kalshi::trade_tape::TradeTape::new(10_000),
    ));

    let orderbook_handle = tokio::spawn({
        let orderbooks = Arc::clone(&orderbooks);
        let trade_tape = Arc::clone(&trade_tape);
        let fh = Arc::clone(&feed_health);
        let redis = redis.clone();
        let cancel = cancel.clone();
        async move {
            orderbook_feed::run(ws_rx, orderbooks, trade_tape, fh, redis.clone(), cancel).await;
        }
    });

    // Canonical crypto state — shared across all feeds and execution
    let rti_config = crypto_state::RtiConfig {
        stale_threshold_secs: config.rti_stale_threshold_secs,
        outlier_threshold_pct: config.rti_outlier_threshold_pct,
        min_venues: config.rti_min_venues,
    };
    let crypto_state = Arc::new(crypto_state::CryptoState::with_config(rti_config));

    // Spawn crypto exchange feeds (gated by config)
    if config.enable_coinbase {
        let feed = feeds::coinbase::CoinbaseFeed::new(
            config.coinbase_ws_url.clone(),
            cancel.clone(),
        );
        let redis_clone = redis.clone();
        let cs = Arc::clone(&crypto_state);
        let fh = Arc::clone(&feed_health);
        tokio::spawn(async move { feed.run(redis_clone, cs, fh).await });
        tracing::info!("coinbase feed enabled");
    }

    if config.enable_binance_spot {
        let feed = feeds::binance_spot::BinanceSpotFeed::new(
            config.binance_spot_ws_url.clone(),
            cancel.clone(),
        );
        let redis_clone = redis.clone();
        let cs = Arc::clone(&crypto_state);
        let fh = Arc::clone(&feed_health);
        tokio::spawn(async move { feed.run(redis_clone, cs, fh).await });
        tracing::info!("binance spot feed enabled");
    }

    if config.enable_binance_futures {
        let feed = feeds::binance_futures::BinanceFuturesFeed::new(
            config.binance_futures_ws_url.clone(),
            cancel.clone(),
        );
        let redis_clone = redis.clone();
        let cs = Arc::clone(&crypto_state);
        let fh = Arc::clone(&feed_health);
        tokio::spawn(async move { feed.run(redis_clone, cs, fh).await });
        tracing::info!("binance futures feed enabled");
    }

    if config.enable_deribit {
        let feed = feeds::deribit::DeribitFeed::new(
            config.deribit_ws_url.clone(),
            cancel.clone(),
        );
        let redis_clone = redis.clone();
        let cs = Arc::clone(&crypto_state);
        let fh = Arc::clone(&feed_health);
        tokio::spawn(async move { feed.run(redis_clone, cs, fh).await });
        tracing::info!("deribit dvol feed enabled");
    }

    // Initialize kill switch state
    let kill_switch = Arc::new(kill_switch::KillSwitchState::new(
        config.kill_switch_all,
        config.kill_switch_crypto,
        config.kill_switch_weather,
    ));

    // Phase 3: Contract discovery for crypto evaluator (with WS subscription wiring)
    let contract_discovery = Arc::new(contract_discovery::ContractDiscovery::with_ws_handle(ws_sub_handle));
    let discovery_handle = tokio::spawn({
        let cd = Arc::clone(&contract_discovery);
        let pool = pool.clone();
        let cancel = cancel.clone();
        async move {
            cd.run(pool, cancel).await;
        }
    });

    // Phase 3: Shared OrderManager (used by both execution engine and crypto evaluator)
    let order_mgr = Arc::new(tokio::sync::Mutex::new(order_manager::OrderManager::new()));
    {
        let mut mgr = order_mgr.lock().await;
        if let Err(e) = mgr.reconcile_on_startup(&kalshi, &pool).await {
            tracing::warn!(error = %e, "startup reconciliation failed, continuing with empty state");
        }
    }

    // Phase 3: Spawn event-driven crypto evaluator (with Phase 4.3 trade tape)
    let crypto_eval_handle = tokio::spawn({
        let config = Arc::new(config.clone());
        let cs = Arc::clone(&crypto_state);
        let cd = Arc::clone(&contract_discovery);
        let ob = Arc::clone(&orderbooks);
        let tt = Arc::clone(&trade_tape);
        let om = Arc::clone(&order_mgr);
        let k = Arc::clone(&kalshi);
        let ks = Arc::clone(&kill_switch);
        let fh = Arc::clone(&feed_health);
        let pool = pool.clone();
        let redis = redis.clone();
        let nats = nats.clone();
        let cancel = cancel.clone();
        async move {
            crypto_evaluator::run(config, cs, cd, ob, tt, om, k, ks, fh, pool, redis, nats, cancel)
                .await;
        }
    });

    // Start Axum HTTP server (kill switch + health + dashboard)
    let dashboard_state = dashboard::DashboardState {
        config: Arc::new(config.clone()),
        crypto_state: Arc::clone(&crypto_state),
        order_mgr: Arc::clone(&order_mgr),
        kill_switch: Arc::clone(&kill_switch),
        feed_health: Arc::clone(&feed_health),
        contract_discovery: Arc::clone(&contract_discovery),
        pool: pool.clone(),
    };
    let http_app = kill_switch::router(Arc::clone(&kill_switch))
        .merge(dashboard::routes(dashboard_state));
    let http_port = config.http_port;
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(("0.0.0.0", http_port)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, port = http_port, "failed to bind HTTP server");
                return;
            }
        };
        tracing::info!(port = http_port, "http server listening");
        if let Err(e) = axum::serve(listener, http_app).await {
            tracing::error!(error = %e, "http server error");
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
        let ks = Arc::clone(&kill_switch);
        let fh = Arc::clone(&feed_health);
        let cs = Arc::clone(&crypto_state);
        let kalshi = Arc::clone(&kalshi);
        let om = Arc::clone(&order_mgr);
        let nats = nats.clone();
        async move {
            if let Err(e) = execution::run(&config, nats, pool, kalshi, ks, fh, cs, om).await {
                tracing::error!(error = %e, "execution engine failed");
            }
        }
    });

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");

    cancel.cancel();
    execution_handle.abort();
    crypto_eval_handle.abort();

    // Graceful shutdown with 10s timeout
    let shutdown = async {
        let _ = ws_handle.await;
        let _ = orderbook_handle.await;
        let _ = discovery_handle.await;
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

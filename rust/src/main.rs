// Allow dead_code: many pub APIs are used by downstream consumers or future phases
#![allow(dead_code)]
// Allow collapsible_if: nested if-let guards are often more readable
#![allow(clippy::collapsible_if)]

mod clock;
mod config;
mod contract_discovery;
mod crypto_asset;
mod crypto_evaluator;
mod crypto_fv;
mod crypto_state;
mod crypto_state_registry;
mod dashboard;
mod dead_letter;
mod decision_log;
mod discord;
mod execution;
mod feed_health;
mod feeds;
mod health;
#[cfg(test)]
mod integration_tests;
mod kalshi;
mod kill_switch;
mod lock_ext;
mod logging;
mod metrics_registry;
mod order_manager;
mod orderbook_feed;
mod supervisor;
mod types;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fred::interfaces::ClientLike;
use tokio_util::sync::CancellationToken;

use supervisor::TaskCriticality;

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

    // Validate configuration bounds and cross-field invariants
    if let Err(errors) = config.validate() {
        for err in &errors {
            eprintln!("config error: {err}");
        }
        anyhow::bail!(
            "Configuration validation failed with {} error(s). Fix your .env and retry.",
            errors.len()
        );
    }

    logging::init(&config.log_level, &config.log_format);
    config.log_startup();

    // Phase 12.1: Initialize Prometheus metrics recorder
    let prometheus_handle = metrics_registry::init();

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

    // Paper mode startup guard (Phase 0.3 + Phase 16: ceremony gate)
    if !config.paper_mode {
        tracing::warn!("========================================");
        tracing::warn!("  LIVE TRADING MODE — PAPER_MODE=false  ");
        tracing::warn!("========================================");
        tracing::warn!(
            kalshi_base_url = %config.kalshi_base_url,
            "Orders will be submitted with real money"
        );
        // Send Discord alert for live mode activation (if configured)
        if let Some(ref webhook_url) = config.discord_webhook_url {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .ok();
            if let Some(client) = client {
                let msg = serde_json::json!({
                    "content": "🚨 **LIVE TRADING MODE ACTIVATED** — PAPER_MODE=false, real money orders enabled"
                });
                let _ = client.post(webhook_url).json(&msg).send().await;
            }
        }
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
    let redis_config =
        fred::types::config::Config::from_url(&config.redis_url).context("Invalid REDIS_URL")?;
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
    let pong: String = fred::interfaces::ClientLike::ping(&redis, None)
        .await
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
    )
    .context("Failed to initialize Kalshi auth")?;
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
    )
    .context("Failed to create WS auth")?;

    let (ws_feed, ws_sub_handle) =
        kalshi::websocket::KalshiWsFeed::new(config.kalshi_ws_url.clone(), ws_auth, cancel.clone());

    // Initialize kill switch state (moved up for supervisor)
    let kill_switch = Arc::new(kill_switch::KillSwitchState::new(
        config.kill_switch_all,
        config.kill_switch_crypto,
        config.kill_switch_weather,
    ));

    // Phase 12.0c: Task supervisor for critical/non-critical task management
    let discord_config = config.discord_webhook_url.clone().map(|url| {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build discord http client");
        (client, url)
    });
    let mut supervisor =
        supervisor::TaskSupervisor::new(cancel.clone(), Arc::clone(&kill_switch), discord_config);

    supervisor.spawn("kalshi_ws", TaskCriticality::Critical, async move {
        ws_feed.run(ws_tx).await;
    });

    // Initialize feed health tracker (before feeds so they can report health)
    let feed_health = Arc::new(feed_health::FeedHealth::new());

    // Shared trade tape for orderbook feed + crypto evaluator (Phase 4.3)
    let trade_tape = Arc::new(std::sync::RwLock::new(kalshi::trade_tape::TradeTape::new(
        10_000,
    )));

    supervisor.spawn("orderbook_feed", TaskCriticality::Critical, {
        let orderbooks = Arc::clone(&orderbooks);
        let trade_tape = Arc::clone(&trade_tape);
        let fh = Arc::clone(&feed_health);
        let redis = redis.clone();
        let cancel = cancel.clone();
        async move {
            orderbook_feed::run(ws_rx, orderbooks, trade_tape, fh, redis.clone(), cancel).await;
        }
    });

    // Phase 13: Per-asset crypto state registry
    let rti_config = crypto_state::RtiConfig {
        stale_threshold_secs: config.rti_stale_threshold_secs,
        outlier_threshold_pct: config.rti_outlier_threshold_pct,
        min_venues: config.rti_min_venues,
    };
    let enabled_assets = config.enabled_crypto_assets();
    tracing::info!(assets = ?enabled_assets, "enabled crypto assets");
    let registry = Arc::new(crypto_state_registry::CryptoStateRegistry::new(
        &enabled_assets,
        rti_config,
    ));

    // Spawn crypto exchange feeds (gated by config)
    if config.enable_coinbase {
        let feed = feeds::coinbase::CoinbaseFeed::new(
            config.coinbase_ws_url.clone(),
            enabled_assets.clone(),
            cancel.clone(),
        );
        let redis_clone = redis.clone();
        let reg = Arc::clone(&registry);
        let fh = Arc::clone(&feed_health);
        tokio::spawn(async move { feed.run(redis_clone, reg, fh).await });
        tracing::info!("coinbase feed enabled (multi-asset)");
    }

    if config.enable_binance_spot {
        let feed = feeds::binance_spot::BinanceSpotFeed::new(
            config.binance_spot_ws_url.clone(),
            enabled_assets.clone(),
            cancel.clone(),
        );
        let redis_clone = redis.clone();
        let reg = Arc::clone(&registry);
        let fh = Arc::clone(&feed_health);
        tokio::spawn(async move { feed.run(redis_clone, reg, fh).await });
        tracing::info!("binance spot feed enabled (multi-asset)");
    }

    if config.enable_binance_futures {
        // BTC-only — only BTC has perps on Binance.us
        if let Some(btc_state) = registry.get(crypto_asset::CryptoAsset::BTC) {
            let feed = feeds::binance_futures::BinanceFuturesFeed::new(
                config.binance_futures_ws_url.clone(),
                cancel.clone(),
            );
            let redis_clone = redis.clone();
            let cs = Arc::clone(btc_state);
            let fh = Arc::clone(&feed_health);
            tokio::spawn(async move { feed.run(redis_clone, cs, fh).await });
            tracing::info!("binance futures feed enabled (BTC-only)");
        }
    }

    if config.enable_deribit {
        // BTC-only — only BTC has DVOL
        if let Some(btc_state) = registry.get(crypto_asset::CryptoAsset::BTC) {
            let feed =
                feeds::deribit::DeribitFeed::new(config.deribit_ws_url.clone(), cancel.clone());
            let redis_clone = redis.clone();
            let cs = Arc::clone(btc_state);
            let fh = Arc::clone(&feed_health);
            tokio::spawn(async move { feed.run(redis_clone, cs, fh).await });
            tracing::info!("deribit dvol feed enabled (BTC-only)");
        }
    }

    // Phase 3: Contract discovery for crypto evaluator (with WS subscription wiring)
    let contract_discovery = Arc::new(contract_discovery::ContractDiscovery::with_ws_handle(
        ws_sub_handle,
    ));
    supervisor.spawn("contract_discovery", TaskCriticality::Critical, {
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

    // Phase 12.0d: Batched decision log writer (replaces per-eval tokio::spawn)
    let decision_writer = decision_log::DecisionLogWriter::spawn(pool.clone(), cancel.clone());

    // Phase 3: Spawn event-driven crypto evaluator (with Phase 4.3 trade tape)
    supervisor.spawn("crypto_evaluator", TaskCriticality::Critical, {
        let config = Arc::new(config.clone());
        let reg = Arc::clone(&registry);
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
        let dw = decision_writer.clone();
        async move {
            crypto_evaluator::run(
                config, reg, cd, ob, tt, om, k, ks, fh, pool, redis, nats, cancel, dw,
            )
            .await;
        }
    });

    // Start Axum HTTP server (kill switch + health + dashboard)
    let dashboard_state = dashboard::DashboardState {
        config: Arc::new(config.clone()),
        crypto_registry: Arc::clone(&registry),
        order_mgr: Arc::clone(&order_mgr),
        kill_switch: Arc::clone(&kill_switch),
        feed_health: Arc::clone(&feed_health),
        contract_discovery: Arc::clone(&contract_discovery),
        pool: pool.clone(),
    };
    let health_state = health::HealthState {
        pool: pool.clone(),
        redis: redis.clone(),
        nats: nats.clone(),
        feed_health: Arc::clone(&feed_health),
        prometheus_handle,
    };
    let http_app = kill_switch::router(Arc::clone(&kill_switch))
        .merge(dashboard::routes(dashboard_state))
        .merge(health::routes(health_state));
    let http_port = config.http_port;
    supervisor.spawn("http_server", TaskCriticality::NonCritical, async move {
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

    // Run execution engine (uses BTC state for advisory pricing)
    supervisor.spawn("execution", TaskCriticality::Critical, {
        let config = config.clone();
        let pool = pool.clone();
        let ks = Arc::clone(&kill_switch);
        let fh = Arc::clone(&feed_health);
        // Execution engine uses BTC CryptoState for advisory snapshot;
        // fall back to a fresh empty state if BTC is not enabled
        let cs = registry
            .get(crypto_asset::CryptoAsset::BTC)
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::new(crypto_state::CryptoState::new()));
        let kalshi = Arc::clone(&kalshi);
        let om = Arc::clone(&order_mgr);
        let nats = nats.clone();
        let cancel = cancel.clone();
        async move {
            if let Err(e) =
                execution::run(&config, nats, pool, kalshi, ks, fh, cs, om, cancel).await
            {
                tracing::error!(error = %e, "execution engine failed");
            }
        }
    });

    // Wait for shutdown: either ctrl+c or supervisor detects critical task death
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown signal received — beginning graceful shutdown");
        }
        _ = supervisor.run() => {
            tracing::warn!("supervisor triggered shutdown (critical task died)");
        }
    }

    // ── Stage 1: Activate kill switch (prevents new orders) ──────────
    kill_switch
        .kill_all
        .store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::warn!("kill switch activated — no new orders will be placed");

    // ── Stage 2: Signal all loops to stop + drain supervised tasks ───
    cancel.cancel();

    // Give supervisor time to drain all remaining tasks
    if tokio::time::timeout(Duration::from_secs(5), supervisor.run())
        .await
        .is_err()
    {
        tracing::warn!("supervised tasks did not exit within 5s");
    }

    // ── Stage 3: Cancel all in-flight orders via Kalshi API ──────────
    tracing::info!("draining in-flight orders...");
    {
        let mut mgr = order_mgr.lock().await;
        let (attempted, succeeded) = mgr.drain_all_orders(&kalshi).await;
        if attempted > 0 {
            tracing::info!(attempted, succeeded, "order drain complete");
        } else {
            tracing::info!("no in-flight orders to drain");
        }
    }

    // ── Stage 4: Wait for order confirmations ────────────────────────
    let confirm_start = std::time::Instant::now();
    let confirm_deadline = Duration::from_secs(5);
    loop {
        {
            let mgr = order_mgr.lock().await;
            if !mgr.has_in_flight_orders() {
                tracing::info!("all orders confirmed terminal");
                break;
            }
            let remaining = mgr.in_flight_count();
            tracing::info!(remaining, "waiting for order confirmations...");
        }
        if confirm_start.elapsed() > confirm_deadline {
            tracing::warn!("order confirmation timed out after 5s");
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── Stage 5: Clean up infrastructure ─────────────────────────────
    let cleanup = async {
        pool.close().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), redis.quit()).await;
    };
    if tokio::time::timeout(Duration::from_secs(5), cleanup)
        .await
        .is_err()
    {
        tracing::warn!("infrastructure cleanup timed out after 5s");
    }

    tracing::info!("shutdown complete");
    Ok(())
}

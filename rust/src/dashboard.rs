//! Dashboard API endpoints — serves JSON data and the terminal UI.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::header;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use serde::Serialize;

use crate::config::Config;
use crate::contract_discovery::ContractDiscovery;
use crate::crypto_state::CryptoState;
use crate::feed_health::FeedHealth;
use crate::kill_switch::KillSwitchState;
use crate::order_manager::OrderManager;

/// Shared state for all dashboard endpoints.
#[derive(Clone)]
pub struct DashboardState {
    pub config: Arc<Config>,
    pub crypto_state: Arc<CryptoState>,
    pub order_mgr: Arc<tokio::sync::Mutex<OrderManager>>,
    pub kill_switch: Arc<KillSwitchState>,
    pub feed_health: Arc<FeedHealth>,
    pub contract_discovery: Arc<ContractDiscovery>,
    pub pool: sqlx::PgPool,
}

/// Build the dashboard router (merged into the main Axum app).
pub fn routes(state: DashboardState) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/api/state", get(api_state))
        .route("/api/signals", get(api_signals))
        .route("/api/orders", get(api_orders))
        .route("/health/detail", get(api_health_detail))
        .with_state(state)
}

// ── Index HTML ──────────────────────────────────────────────────────────────

async fn index_html() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Html(include_str!("dashboard.html")),
    )
}

// ── /api/state ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ApiState {
    btc_price: f64,
    coinbase_spot: f64,
    binance_spot: f64,
    perp_price: f64,
    mark_price: f64,
    funding_rate: f64,
    basis: f64,
    dvol: f64,
    best_vol: Option<f64>,
    shadow_rti: f64,

    feeds: FeedStatus,
    kill_switches: KillSwitches,
    risk: RiskSnapshot,
    contracts: Vec<ContractInfo>,
    paper_mode: bool,
}

#[derive(Serialize)]
struct FeedStatus {
    coinbase: FeedInfo,
    binance_spot: FeedInfo,
    binance_futures: FeedInfo,
    deribit: FeedInfo,
    kalshi_ws: FeedInfo,
}

#[derive(Serialize)]
struct FeedInfo {
    connected: bool,
    age_ms: Option<u64>,
}

#[derive(Serialize)]
struct KillSwitches {
    all: bool,
    crypto: bool,
    weather: bool,
}

#[derive(Serialize)]
struct RiskSnapshot {
    position_count: usize,
    max_positions: usize,
    daily_pnl_cents: i64,
    daily_loss_cents: i64,
    max_daily_loss_cents: i64,
    max_exposure_cents: i64,
    max_trade_size_cents: i64,
    positions: Vec<PositionInfo>,
}

#[derive(Serialize)]
struct PositionInfo {
    ticker: String,
    direction: String,
    size: i64,
    state: String,
    model_prob: f64,
    market_price: f64,
    entry_price: Option<f64>,
}

#[derive(Serialize)]
struct ContractInfo {
    ticker: String,
    strike: f64,
    settlement_time: String,
    minutes_remaining: f64,
}

async fn api_state(State(st): State<DashboardState>) -> Json<ApiState> {
    let snap = st.crypto_state.snapshot();

    // Feed health with age
    let feed_age = |updated: &Option<std::time::Instant>| -> Option<u64> {
        updated.map(|t| t.elapsed().as_millis() as u64)
    };

    let feeds = FeedStatus {
        coinbase: FeedInfo {
            connected: snap.coinbase_spot > 0.0,
            age_ms: feed_age(&snap.coinbase_updated),
        },
        binance_spot: FeedInfo {
            connected: snap.binance_spot > 0.0,
            age_ms: feed_age(&snap.binance_spot_updated),
        },
        binance_futures: FeedInfo {
            connected: snap.perp_price > 0.0,
            age_ms: feed_age(&snap.futures_updated),
        },
        deribit: FeedInfo {
            connected: snap.dvol > 0.0,
            age_ms: feed_age(&snap.dvol_updated),
        },
        kalshi_ws: FeedInfo {
            connected: st.feed_health.is_healthy("kalshi_ws"),
            age_ms: None,
        },
    };

    let kill_switches = KillSwitches {
        all: st.kill_switch.kill_all.load(Ordering::Relaxed),
        crypto: st.kill_switch.kill_crypto.load(Ordering::Relaxed),
        weather: st.kill_switch.kill_weather.load(Ordering::Relaxed),
    };

    // Order manager snapshot
    let mgr = st.order_mgr.lock().await;
    let positions: Vec<PositionInfo> = mgr
        .all_orders()
        .iter()
        .filter(|o| !o.state.is_terminal() || o.state.has_fill())
        .map(|o| PositionInfo {
            ticker: o.ticker.clone(),
            direction: o.direction.clone(),
            size: o.requested_qty,
            state: o.state.to_string(),
            model_prob: o.model_prob,
            market_price: o.market_price,
            entry_price: o.entry_price,
        })
        .collect();

    let risk = RiskSnapshot {
        position_count: mgr.position_count(),
        max_positions: st.config.max_positions,
        daily_pnl_cents: mgr.daily_pnl_cents(),
        daily_loss_cents: mgr.daily_loss_cents(),
        max_daily_loss_cents: st.config.max_daily_loss_cents,
        max_exposure_cents: st.config.max_exposure_cents,
        max_trade_size_cents: st.config.max_trade_size_cents,
        positions,
    };
    drop(mgr);

    // Active contracts
    let now = chrono::Utc::now();
    let contracts: Vec<ContractInfo> = st
        .contract_discovery
        .active_contracts()
        .into_iter()
        .map(|c| {
            let mins = (c.settlement_time - now).num_seconds() as f64 / 60.0;
            ContractInfo {
                ticker: c.ticker,
                strike: c.strike,
                settlement_time: c.settlement_time.to_rfc3339(),
                minutes_remaining: mins,
            }
        })
        .collect();

    Json(ApiState {
        btc_price: snap.shadow_rti,
        coinbase_spot: snap.coinbase_spot,
        binance_spot: snap.binance_spot,
        perp_price: snap.perp_price,
        mark_price: snap.mark_price,
        funding_rate: snap.funding_rate,
        basis: snap.basis,
        dvol: snap.dvol,
        best_vol: snap.best_vol,
        shadow_rti: snap.shadow_rti,
        feeds,
        kill_switches,
        risk,
        contracts,
        paper_mode: st.config.paper_mode,
    })
}

// ── /health/detail ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct HealthDetail {
    feeds: Vec<crate::feed_health::FeedHealthDetail>,
    crypto_health: f64,
    weather_health: f64,
    system_health: f64,
}

async fn api_health_detail(State(st): State<DashboardState>) -> Json<HealthDetail> {
    let feeds = st.feed_health.health_detail();
    Json(HealthDetail {
        feeds,
        crypto_health: st.feed_health.strategy_health("crypto"),
        weather_health: st.feed_health.strategy_health("weather"),
        system_health: st.feed_health.system_health(),
    })
}

// ── /api/signals ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SignalRow {
    id: i64,
    created_at: String,
    ticker: String,
    signal_type: String,
    direction: String,
    model_prob: f32,
    market_price: f32,
    edge: f32,
    kelly_fraction: f32,
    minutes_remaining: f32,
    acted_on: bool,
    rejection_reason: Option<String>,
    source: Option<String>,
}

async fn api_signals(State(st): State<DashboardState>) -> Json<Vec<SignalRow>> {
    let rows = sqlx::query_as::<_, (
        i64, chrono::DateTime<chrono::Utc>, String, String, String,
        f32, f32, f32, f32, f32, bool, Option<String>, Option<String>,
    )>(
        r#"
        SELECT id, created_at, ticker, signal_type, direction,
               model_prob, market_price, edge, kelly_fraction,
               minutes_remaining, acted_on, rejection_reason,
               observation_data->>'source' AS source
        FROM signals
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    Json(
        rows.into_iter()
            .map(|r| SignalRow {
                id: r.0,
                created_at: r.1.to_rfc3339(),
                ticker: r.2,
                signal_type: r.3,
                direction: r.4,
                model_prob: r.5,
                market_price: r.6,
                edge: r.7,
                kelly_fraction: r.8,
                minutes_remaining: r.9,
                acted_on: r.10,
                rejection_reason: r.11,
                source: r.12,
            })
            .collect(),
    )
}

// ── /api/orders ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OrderRow {
    id: i64,
    created_at: String,
    ticker: String,
    direction: String,
    size_cents: i32,
    fill_price: Option<f32>,
    status: String,
    pnl_cents: Option<i32>,
}

async fn api_orders(State(st): State<DashboardState>) -> Json<Vec<OrderRow>> {
    let rows = sqlx::query_as::<_, (
        i64, chrono::DateTime<chrono::Utc>, String, String,
        i32, Option<f32>, String, Option<i32>,
    )>(
        r#"
        SELECT id, created_at, ticker, direction,
               size_cents, fill_price, status, pnl_cents
        FROM orders
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    Json(
        rows.into_iter()
            .map(|r| OrderRow {
                id: r.0,
                created_at: r.1.to_rfc3339(),
                ticker: r.2,
                direction: r.3,
                size_cents: r.4,
                fill_price: r.5,
                status: r.6,
                pnl_cents: r.7,
            })
            .collect(),
    )
}

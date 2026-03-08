//! NATS signal consumer and order execution engine.
//!
//! Subscribes to `tradebot.signals`, deserializes incoming signals,
//! applies risk checks via OrderManager, and places orders via Kalshi.
//! Phase 2: uses OrderManager state machine instead of fire-and-forget.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::crypto_fv;
use crate::crypto_state::CryptoState;
use crate::feed_health::FeedHealth;
use crate::kalshi::client::KalshiClient;
use crate::kill_switch::KillSwitchState;
use crate::order_manager::OrderManager;

/// Signal schema matching the Python SignalSchema.
#[derive(Debug, Deserialize)]
pub struct Signal {
    pub ticker: String,
    pub signal_type: String,
    pub action: String,
    pub direction: String,
    pub model_prob: f64,
    pub market_price: f64,
    pub edge: f64,
    pub kelly_fraction: f64,
    pub minutes_remaining: f64,
    pub spread: f64,
    pub order_imbalance: f64,
}

/// Run the execution loop: subscribe to NATS and process signals.
pub async fn run(
    config: &Config,
    nats: async_nats::Client,
    pool: sqlx::PgPool,
    kalshi: KalshiClient,
    kill_switch: Arc<KillSwitchState>,
    feed_health: Arc<FeedHealth>,
    crypto_state: Arc<CryptoState>,
) -> Result<()> {
    let mut subscriber = nats
        .subscribe("tradebot.signals")
        .await
        .context("Failed to subscribe to tradebot.signals")?;

    // Subscribe to advisory crypto signals for logging/comparison (Phase 1.3)
    let mut advisory_sub = nats
        .subscribe("tradebot.advisory.crypto")
        .await
        .context("Failed to subscribe to tradebot.advisory.crypto")?;

    info!("execution engine listening on tradebot.signals + tradebot.advisory.crypto");

    // Spawn advisory logger — these signals are informational only
    let advisory_crypto_state = Arc::clone(&crypto_state);
    tokio::spawn(async move {
        while let Some(msg) = advisory_sub.next().await {
            if let Ok(signal) = serde_json::from_slice::<Signal>(&msg.payload) {
                let snap = advisory_crypto_state.snapshot();
                if snap.shadow_rti > 0.0 {
                    let fv = crypto_fv::compute_crypto_fair_value(
                        &snap,
                        signal.market_price * 100_000.0,
                        signal.minutes_remaining,
                    );
                    info!(
                        ticker = %signal.ticker,
                        python_prob = %format!("{:.4}", signal.model_prob),
                        rust_prob = %format!("{:.4}", fv.probability),
                        python_edge = %format!("{:.4}", signal.edge),
                        confidence = %format!("{:.2}", fv.confidence),
                        "advisory crypto signal (python → rust comparison)"
                    );
                } else {
                    info!(
                        ticker = %signal.ticker,
                        python_prob = %format!("{:.4}", signal.model_prob),
                        "advisory crypto signal (no rust state yet)"
                    );
                }
            }
        }
    });

    // Initialize order manager with startup reconciliation (Phase 2.5)
    let mut order_mgr = OrderManager::new();
    if let Err(e) = order_mgr.reconcile_on_startup(&kalshi, &pool).await {
        warn!(error = %e, "startup reconciliation failed, continuing with empty state");
    }

    // Periodic GC and kill switch check interval
    let mut gc_interval = tokio::time::interval(std::time::Duration::from_secs(300));
    let mut kill_check_interval = tokio::time::interval(std::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            Some(msg) = subscriber.next() => {
                let start = Instant::now();

                let signal: Signal = match serde_json::from_slice(&msg.payload) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "failed to deserialize signal");
                        continue;
                    }
                };

                info!(
                    ticker = %signal.ticker,
                    action = %signal.action,
                    direction = %signal.direction,
                    edge = %format!("{:.4}", signal.edge),
                    kelly = %format!("{:.4}", signal.kelly_fraction),
                    "signal received"
                );

                // Handle exit signals
                if signal.action == "exit" {
                    if order_mgr.has_position(&signal.ticker) {
                        match order_mgr.submit_exit(config, &kalshi, &pool, &signal, &crypto_state).await {
                            Ok(()) => {
                                info!(
                                    ticker = %signal.ticker,
                                    latency_ms = %start.elapsed().as_millis(),
                                    "exit order processed"
                                );
                            }
                            Err(e) => error!(ticker = %signal.ticker, error = %e, "exit order failed"),
                        }
                    }
                    continue;
                }

                // Entry signal — apply risk checks (Phase 2.6)
                if let Err(reason) = order_mgr.check_risk(config, &signal, &kill_switch, &feed_health) {
                    warn!(ticker = %signal.ticker, reason = %reason, "signal rejected");
                    continue;
                }

                // Submit entry via order manager (Phase 2.1)
                match order_mgr.submit_entry(config, &kalshi, &pool, &signal, &crypto_state).await {
                    Ok(()) => {
                        info!(
                            ticker = %signal.ticker,
                            latency_ms = %start.elapsed().as_millis(),
                            positions = order_mgr.position_count(),
                            "entry order processed"
                        );
                    }
                    Err(e) => error!(ticker = %signal.ticker, error = %e, "entry order failed"),
                }
            }

            // Periodic kill switch check — cancel in-flight orders (Phase 2.7)
            _ = kill_check_interval.tick() => {
                if kill_switch.is_blocked("crypto") {
                    order_mgr.cancel_in_flight(&kalshi, Some("crypto")).await;
                }
                if kill_switch.is_blocked("weather") {
                    order_mgr.cancel_in_flight(&kalshi, Some("weather")).await;
                }
            }

            // Periodic GC of old terminal orders
            _ = gc_interval.tick() => {
                order_mgr.gc_old_orders();
            }

            else => break,
        }
    }

    Ok(())
}

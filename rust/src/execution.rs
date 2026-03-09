//! NATS signal consumer and order execution engine.
//!
//! Subscribes to `tradebot.signals`, deserializes incoming signals,
//! applies risk checks via OrderManager, and places orders via Kalshi.
//! Phase 2: uses OrderManager state machine instead of fire-and-forget.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::crypto_fv;
use crate::crypto_state::CryptoState;
use crate::dead_letter::{self, DeadLetterReason};
use crate::feed_health::FeedHealth;
use crate::kalshi::client::KalshiClient;
use crate::kill_switch::KillSwitchState;
use crate::order_manager::OrderManager;
use crate::types::Signal;

/// Run the execution loop: subscribe to NATS and process signals.
///
/// Phase 3: accepts shared OrderManager (Arc<Mutex>) so crypto evaluator
/// can also submit orders through the same manager.
pub async fn run(
    config: &Config,
    nats: async_nats::Client,
    pool: sqlx::PgPool,
    kalshi: Arc<KalshiClient>,
    kill_switch: Arc<KillSwitchState>,
    feed_health: Arc<FeedHealth>,
    crypto_state: Arc<CryptoState>,
    order_mgr: Arc<tokio::sync::Mutex<OrderManager>>,
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

    // Periodic GC, kill switch check, and reconciliation intervals
    let mut gc_interval = tokio::time::interval(std::time::Duration::from_secs(300));
    let mut kill_check_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    let mut reconciliation_interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 5 min

    loop {
        tokio::select! {
            Some(msg) = subscriber.next() => {
                let start = Instant::now();

                let signal: Signal = match serde_json::from_slice(&msg.payload) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "failed to deserialize signal");
                        dead_letter::send_dead_letter(
                            &nats,
                            &pool,
                            DeadLetterReason::DeserializationFailure(e.to_string()),
                            Some(&msg.payload),
                            "execution",
                        ).await;
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

                let mut mgr = order_mgr.lock().await;

                // Handle exit signals
                if signal.action == "exit" {
                    if mgr.has_position(&signal.ticker) {
                        match mgr.submit_exit(config, &kalshi, &pool, &signal, None, &crypto_state).await {
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
                if let Err(reason) = mgr.check_risk(config, &signal, &kill_switch, &feed_health) {
                    warn!(ticker = %signal.ticker, reason = %reason, "signal rejected");
                    continue;
                }

                // Submit entry via order manager (Phase 2.1)
                match mgr.submit_entry(config, &kalshi, &pool, &signal, None, &crypto_state).await {
                    Ok(()) => {
                        info!(
                            ticker = %signal.ticker,
                            latency_ms = %start.elapsed().as_millis(),
                            positions = mgr.position_count(),
                            "entry order processed"
                        );
                    }
                    Err(e) => error!(ticker = %signal.ticker, error = %e, "entry order failed"),
                }
            }

            // Periodic kill switch check — cancel in-flight orders (Phase 2.7)
            _ = kill_check_interval.tick() => {
                let mut mgr = order_mgr.lock().await;
                if kill_switch.is_blocked("crypto") {
                    mgr.cancel_in_flight(&kalshi, Some("crypto")).await;
                }
                if kill_switch.is_blocked("weather") {
                    mgr.cancel_in_flight(&kalshi, Some("weather")).await;
                }
            }

            // Periodic GC of old terminal orders
            _ = gc_interval.tick() => {
                let mut mgr = order_mgr.lock().await;
                mgr.gc_old_orders();
            }

            // Phase 5.4: Periodic reconciliation against exchange
            _ = reconciliation_interval.tick() => {
                let mut mgr = order_mgr.lock().await;
                if let Err(e) = mgr.reconcile_positions(&kalshi, &pool).await {
                    warn!(error = %e, "periodic reconciliation failed");
                }
            }

            else => break,
        }
    }

    Ok(())
}

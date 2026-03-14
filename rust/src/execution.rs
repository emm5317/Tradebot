//! NATS signal consumer and order execution engine.
//!
//! Subscribes to `tradebot.signals`, deserializes incoming signals,
//! applies risk checks via OrderManager, and places orders via Kalshi.
//! Phase 2: uses OrderManager state machine instead of fire-and-forget.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::crypto_fv;
use crate::crypto_state::CryptoState;
use crate::dead_letter::{self, DeadLetterReason};
use crate::feed_health::FeedHealth;
use crate::kalshi::client::KalshiClient;
use crate::kalshi::error::KalshiError;
use crate::kalshi::types::OrderRequest;
use crate::kill_switch::KillSwitchState;
use crate::order_manager::{
    compute_order_size, persist_order, record_paper_trade, ManagedOrder, OrderManager, OrderState,
};
use crate::types::Signal;

/// Data extracted from OrderManager under lock for entry orders,
/// allowing network I/O to happen without holding the mutex.
struct EntryPrep {
    client_order_id: String,
    order: ManagedOrder,
    size_cents: i64,
    paper_mode: bool,
}

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
    cancel: CancellationToken,
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
    let advisory_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = advisory_cancel.cancelled() => break,
                Some(msg) = advisory_sub.next() => {
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
            }
        }
    });

    // Periodic GC, kill switch check, and reconciliation intervals
    let mut gc_interval = tokio::time::interval(std::time::Duration::from_secs(300));
    let mut kill_check_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    let mut reconciliation_interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 5 min

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("execution engine shutting down");
                break;
            }
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

                // Handle exit signals
                if signal.action == "exit" {
                    let mut mgr = order_mgr.lock().await;
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

                // ── Entry signal: split into prep (under lock) → network I/O (no lock) → commit (under lock) ──

                // Phase 1: Risk check + prepare order (hold lock briefly, no I/O)
                let prep = {
                    let mut mgr = order_mgr.lock().await;

                    // Risk checks (Phase 2.6)
                    if let Err(reason) = mgr.check_risk(config, &signal, &kill_switch, &feed_health) {
                        warn!(ticker = %signal.ticker, reason = %reason, "signal rejected");
                        continue;
                    }

                    let size_cents = compute_order_size(config, &signal);
                    let client_order_id = mgr.generate_client_order_id(&signal);

                    let snapshot = if signal.signal_type == "crypto" {
                        Some(crypto_state.snapshot())
                    } else {
                        None
                    };

                    let mut order = ManagedOrder::new(
                        client_order_id.clone(),
                        signal.ticker.clone(),
                        signal.signal_type.clone(),
                        signal.direction.clone(),
                        size_cents,
                        signal.model_prob,
                        signal.market_price,
                        snapshot,
                    );
                    order.transition(OrderState::Submitting);

                    // Record cooldowns immediately so concurrent signals are rejected
                    mgr.record_signal_cooldown_pub(&signal.ticker);
                    mgr.record_order_submission_pub(&signal.ticker);

                    EntryPrep {
                        client_order_id,
                        order,
                        size_cents,
                        paper_mode: config.paper_mode,
                    }
                };
                // ── Lock dropped here ──

                // Phase 2: Execute (paper or live) — NO mutex held during network I/O
                if prep.paper_mode {
                    let paper_fill = if signal.direction == "no" {
                        1.0 - signal.market_price
                    } else {
                        signal.market_price
                    };
                    let mut order = prep.order;
                    order.record_fill(prep.size_cents, Some(paper_fill));

                    info!(
                        ticker = %signal.ticker,
                        direction = %signal.direction,
                        size_cents = prep.size_cents,
                        edge = %format!("{:.4}", signal.edge),
                        client_order_id = %prep.client_order_id,
                        "[PAPER] order filled"
                    );

                    // DB writes (no mutex needed)
                    if let Err(e) = record_paper_trade(&pool, &signal, prep.size_cents).await {
                        error!(error = %e, "failed to record paper trade");
                    }
                    if let Err(e) = persist_order(&pool, &order, &signal).await {
                        error!(error = %e, "failed to persist order");
                    }

                    // Phase 3: Commit state back under lock
                    let mut mgr = order_mgr.lock().await;
                    mgr.positions_mut().insert(signal.ticker.clone(), prep.client_order_id.clone());
                    mgr.orders_mut().insert(prep.client_order_id, order);

                    let latency = start.elapsed();
                    metrics::histogram!(
                        crate::metrics_registry::ORDER_LATENCY,
                        "signal_type" => "nats"
                    ).record(latency.as_secs_f64());
                    metrics::counter!(
                        crate::metrics_registry::ORDERS_TOTAL,
                        "signal_type" => "nats",
                        "action" => "entry",
                        "result" => "ok"
                    ).increment(1);
                    info!(
                        ticker = %signal.ticker,
                        latency_ms = %latency.as_millis(),
                        positions = mgr.position_count(),
                        "entry order processed"
                    );
                } else {
                    // Live mode: network call to Kalshi WITHOUT holding the mutex
                    let order_req = OrderRequest {
                        ticker: signal.ticker.clone(),
                        action: "buy".to_string(),
                        side: signal.direction.clone(),
                        r#type: "market".to_string(),
                        count: prep.size_cents,
                        yes_price: None,
                        no_price: None,
                        client_order_id: Some(prep.client_order_id.clone()),
                    };

                    let submit_start = Instant::now();
                    let api_result = kalshi.place_order(order_req).await;

                    // Phase 3: Commit result back under lock
                    let mut mgr = order_mgr.lock().await;
                    let mut order = prep.order;

                    match api_result {
                        Ok(resp) => {
                            let latency_ms = submit_start.elapsed().as_millis();
                            order.latency_ms = Some(latency_ms as i64);
                            order.kalshi_order_id = Some(resp.order.order_id.clone());

                            let filled_qty = resp.order.count.unwrap_or(0)
                                .saturating_sub(resp.order.remaining_count.unwrap_or(0));
                            let fill_price = resp.order.yes_price
                                .or(resp.order.no_price)
                                .map(|p| p as f64 / 100.0);

                            if filled_qty > 0 {
                                order.record_fill(filled_qty, fill_price);
                                if order.state.has_fill() {
                                    mgr.positions_mut()
                                        .insert(signal.ticker.clone(), prep.client_order_id.clone());
                                }
                            } else {
                                order.transition(OrderState::Acknowledged);
                            }

                            info!(
                                ticker = %signal.ticker,
                                client_order_id = %prep.client_order_id,
                                kalshi_order_id = %resp.order.order_id,
                                filled_qty,
                                requested_qty = prep.size_cents,
                                state = %order.state,
                                latency_ms = %latency_ms,
                                "entry order submitted"
                            );

                            if let Err(e) = persist_order(&pool, &order, &signal).await {
                                error!(error = %e, "failed to persist order");
                            }
                            mgr.orders_mut().insert(prep.client_order_id, order);

                            let latency = start.elapsed();
                            metrics::histogram!(
                                crate::metrics_registry::ORDER_LATENCY,
                                "signal_type" => "nats"
                            ).record(latency.as_secs_f64());
                            metrics::counter!(
                                crate::metrics_registry::ORDERS_TOTAL,
                                "signal_type" => "nats",
                                "action" => "entry",
                                "result" => "ok"
                            ).increment(1);
                            info!(
                                ticker = %signal.ticker,
                                latency_ms = %latency.as_millis(),
                                positions = mgr.position_count(),
                                "entry order processed"
                            );
                        }
                        Err(ref e) => {
                            match e {
                                KalshiError::InsufficientFunds => {
                                    order.transition(OrderState::Rejected);
                                    warn!(ticker = %signal.ticker, "order rejected: insufficient funds");
                                }
                                KalshiError::MarketClosed => {
                                    order.transition(OrderState::Rejected);
                                    warn!(ticker = %signal.ticker, "order rejected: market closed");
                                }
                                KalshiError::RateLimit { retry_after } => {
                                    order.transition(OrderState::Rejected);
                                    warn!(
                                        ticker = %signal.ticker,
                                        retry_after_ms = %retry_after.as_millis(),
                                        "order rejected: rate limited"
                                    );
                                }
                                _ => {
                                    order.transition(OrderState::Unknown);
                                }
                            }

                            if let Err(pe) = persist_order(&pool, &order, &signal).await {
                                error!(error = %pe, "failed to persist order");
                            }
                            mgr.orders_mut().insert(prep.client_order_id, order);

                            metrics::counter!(
                                crate::metrics_registry::ORDERS_TOTAL,
                                "signal_type" => "nats",
                                "action" => "entry",
                                "result" => "error"
                            ).increment(1);
                            // Log non-rejection errors
                            if !matches!(e,
                                KalshiError::InsufficientFunds |
                                KalshiError::MarketClosed |
                                KalshiError::RateLimit { .. }
                            ) {
                                error!(ticker = %signal.ticker, error = %e, "entry order failed");
                            }
                        }
                    }
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

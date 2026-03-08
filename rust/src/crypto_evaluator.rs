//! Event-driven crypto evaluation — triggered by every exchange price update.
//!
//! Phase 3: Core of the event-driven architecture. Subscribes to CryptoState
//! watch channel, evaluates all active crypto contracts on every update,
//! and submits signals through the shared OrderManager.
//!
//! Replaces the Python 10-second polling loop for crypto contracts,
//! targeting sub-500ms signal generation from price update to order submission.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use rust_decimal::prelude::ToPrimitive;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::contract_discovery::ContractDiscovery;
use crate::crypto_fv;
use crate::crypto_state::CryptoState;
use crate::feed_health::FeedHealth;
use crate::kalshi::client::KalshiClient;
use crate::kalshi::orderbook::OrderbookManager;
use crate::kill_switch::KillSwitchState;
use crate::order_manager::OrderManager;
use crate::types::{Signal, SignalPriority};

/// Minimum confidence from the fair-value model to generate a signal.
const MIN_CONFIDENCE: f64 = 0.5;

/// Minimum effective edge (spread-adjusted) to generate an entry signal.
const MIN_EDGE: f64 = 0.06;

/// Minimum Kelly fraction to generate an entry signal.
const MIN_KELLY: f64 = 0.04;

/// Exit edge threshold — if edge flips beyond this, exit.
const EXIT_EDGE_THRESHOLD: f64 = -0.03;

/// Per-ticker debounce interval to avoid evaluating the same ticker too rapidly.
const DEBOUNCE_MS: u128 = 500;

/// Contract lifecycle phase.
enum ContractPhase {
    /// 15–30 min to settlement: too early, skip.
    Eligible,
    /// 5–15 min to settlement: generate entry signals.
    InWindow,
    /// <5 min to settlement: exit signals only.
    ExitOnly,
}

fn contract_phase(minutes_remaining: f64) -> ContractPhase {
    if minutes_remaining > 15.0 {
        ContractPhase::Eligible
    } else if minutes_remaining >= 5.0 {
        ContractPhase::InWindow
    } else {
        ContractPhase::ExitOnly
    }
}

/// Run the event-driven crypto evaluator.
///
/// Subscribes to CryptoState changes and evaluates all active crypto contracts
/// on every exchange price update.
pub async fn run(
    config: Arc<Config>,
    crypto_state: Arc<CryptoState>,
    contract_discovery: Arc<ContractDiscovery>,
    orderbooks: Arc<OrderbookManager>,
    order_mgr: Arc<tokio::sync::Mutex<OrderManager>>,
    kalshi: Arc<KalshiClient>,
    kill_switch: Arc<KillSwitchState>,
    feed_health: Arc<FeedHealth>,
    pool: sqlx::PgPool,
    redis: fred::clients::Client,
    nats: async_nats::Client,
    cancel: CancellationToken,
) {
    let mut rx = crypto_state.subscribe();
    let mut last_eval: HashMap<String, Instant> = HashMap::new();

    info!("crypto evaluator started (event-driven)");

    loop {
        tokio::select! {
            result = rx.changed() => {
                if result.is_err() {
                    // Sender dropped — CryptoState is gone
                    break;
                }

                let eval_start = Instant::now();
                let snap = crypto_state.snapshot();

                // Skip if we don't have meaningful state yet
                if snap.shadow_rti <= 0.0 {
                    continue;
                }

                let contracts = contract_discovery.active_contracts();
                if contracts.is_empty() {
                    continue;
                }

                for contract in &contracts {
                    // Per-ticker debounce
                    if let Some(last) = last_eval.get(&contract.ticker) {
                        if last.elapsed().as_millis() < DEBOUNCE_MS {
                            continue;
                        }
                    }
                    last_eval.insert(contract.ticker.clone(), Instant::now());

                    let now = Utc::now();
                    let minutes_remaining =
                        (contract.settlement_time - now).num_seconds() as f64 / 60.0;

                    if minutes_remaining <= 0.0 {
                        continue;
                    }

                    match contract_phase(minutes_remaining) {
                        ContractPhase::Eligible => {
                            // Too early — skip
                            continue;
                        }
                        ContractPhase::InWindow => {
                            evaluate_entry(
                                &config,
                                &snap,
                                contract,
                                minutes_remaining,
                                &orderbooks,
                                &order_mgr,
                                &kalshi,
                                &kill_switch,
                                &feed_health,
                                &pool,
                                &redis,
                                &nats,
                                &crypto_state,
                            )
                            .await;
                        }
                        ContractPhase::ExitOnly => {
                            evaluate_exit(
                                &config,
                                &snap,
                                contract,
                                minutes_remaining,
                                &orderbooks,
                                &order_mgr,
                                &kalshi,
                                &pool,
                                &crypto_state,
                            )
                            .await;
                        }
                    }
                }

                let elapsed = eval_start.elapsed();
                if elapsed.as_millis() > 500 {
                    warn!(
                        elapsed_ms = %elapsed.as_millis(),
                        contracts = contracts.len(),
                        "crypto evaluation exceeded 500ms target"
                    );
                }
            }
            _ = cancel.cancelled() => {
                info!("crypto evaluator shutting down");
                break;
            }
        }
    }
}

/// Evaluate a contract for entry signal (InWindow phase).
/// Mirrors the Python CryptoSignalEvaluator 11-step pipeline.
#[allow(clippy::too_many_arguments)]
async fn evaluate_entry(
    config: &Config,
    snap: &crate::crypto_state::CryptoStateInner,
    contract: &crate::contract_discovery::CryptoContract,
    minutes_remaining: f64,
    orderbooks: &OrderbookManager,
    order_mgr: &Arc<tokio::sync::Mutex<OrderManager>>,
    kalshi: &Arc<KalshiClient>,
    kill_switch: &KillSwitchState,
    feed_health: &FeedHealth,
    pool: &sqlx::PgPool,
    redis: &fred::clients::Client,
    nats: &async_nats::Client,
    crypto_state: &CryptoState,
) {
    // 1. Read orderbook
    let mid_price = match orderbooks.mid_price(&contract.ticker) {
        Some(d) => d.to_f64().unwrap_or(0.5),
        None => return, // no orderbook data
    };
    let spread = orderbooks
        .spread(&contract.ticker)
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);
    let order_imbalance = orderbooks
        .order_imbalance(&contract.ticker)
        .unwrap_or(0.5);

    // 2. Compute fair value
    let fv = crypto_fv::compute_crypto_fair_value(snap, contract.strike, minutes_remaining);

    // 3. Check confidence
    if fv.confidence < MIN_CONFIDENCE {
        return;
    }

    // 4. Direction and raw edge
    let (direction, raw_edge) = crypto_fv::determine_direction(fv.probability, mid_price);

    // 5. Effective edge (spread-adjusted)
    let effective_edge = crypto_fv::compute_effective_edge(raw_edge, spread);

    // 6. Check minimum edge
    if effective_edge < MIN_EDGE {
        return;
    }

    // 7. Fill price estimate
    let fill_price = crypto_fv::estimate_fill_price(direction, mid_price, spread);

    // 8. Kelly criterion
    let kelly = crypto_fv::compute_kelly(fv.probability, fill_price, direction);

    // 9. Check minimum Kelly
    if kelly < MIN_KELLY {
        return;
    }

    // 10. Build signal
    let signal = Signal {
        ticker: contract.ticker.clone(),
        signal_type: "crypto".to_string(),
        action: "entry".to_string(),
        direction: direction.to_string(),
        model_prob: fv.probability,
        market_price: mid_price,
        edge: effective_edge,
        kelly_fraction: kelly,
        minutes_remaining,
        spread,
        order_imbalance,
        priority: SignalPriority::NewData,
    };

    info!(
        ticker = %signal.ticker,
        direction = %signal.direction,
        edge = %format!("{:.4}", signal.edge),
        kelly = %format!("{:.4}", kelly),
        model_prob = %format!("{:.4}", fv.probability),
        confidence = %format!("{:.2}", fv.confidence),
        shadow_rti = %format!("{:.0}", snap.shadow_rti),
        strike = %format!("{:.0}", contract.strike),
        "crypto eval: entry signal"
    );

    // 11. Risk check + submit
    {
        let mut mgr = order_mgr.lock().await;

        if let Err(reason) = mgr.check_risk(config, &signal, kill_switch, feed_health) {
            warn!(
                ticker = %signal.ticker,
                reason = %reason,
                "crypto eval: signal rejected"
            );
            return;
        }

        if let Err(e) = mgr
            .submit_entry(config, kalshi, pool, &signal, crypto_state)
            .await
        {
            warn!(
                ticker = %signal.ticker,
                error = %e,
                "crypto eval: entry submission failed"
            );
            return;
        }
    }

    // Persist signal and publish advisory (fire-and-forget)
    persist_signal(pool, &signal).await;
    write_model_state(redis, &contract.ticker, &fv).await;
    publish_advisory(nats, &signal).await;
}

/// Evaluate a held position for exit (ExitOnly phase).
#[allow(clippy::too_many_arguments)]
async fn evaluate_exit(
    config: &Config,
    snap: &crate::crypto_state::CryptoStateInner,
    contract: &crate::contract_discovery::CryptoContract,
    minutes_remaining: f64,
    orderbooks: &OrderbookManager,
    order_mgr: &Arc<tokio::sync::Mutex<OrderManager>>,
    kalshi: &Arc<KalshiClient>,
    pool: &sqlx::PgPool,
    crypto_state: &CryptoState,
) {
    // Check if we hold a position on this ticker
    let (has_pos, held_direction) = {
        let mgr = order_mgr.lock().await;
        if !mgr.has_position(&contract.ticker) {
            return;
        }
        // Infer held direction from the entry order
        let dir = mgr.held_direction(&contract.ticker).unwrap_or("yes");
        (true, dir.to_string())
    };

    if !has_pos {
        return;
    }

    // Skip if too close to settlement for orderly exit
    if minutes_remaining < 2.0 {
        return;
    }

    let mid_price = match orderbooks.mid_price(&contract.ticker) {
        Some(d) => d.to_f64().unwrap_or(0.5),
        None => return,
    };
    let spread = orderbooks
        .spread(&contract.ticker)
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);

    let fv = crypto_fv::compute_crypto_fair_value(snap, contract.strike, minutes_remaining);

    // Check if edge has flipped
    let current_edge = if held_direction == "yes" {
        fv.probability - mid_price
    } else {
        mid_price - fv.probability
    };

    if current_edge >= EXIT_EDGE_THRESHOLD {
        return; // Edge hasn't flipped enough to exit
    }

    let exit_direction = if held_direction == "yes" {
        "no"
    } else {
        "yes"
    };

    info!(
        ticker = %contract.ticker,
        held = %held_direction,
        edge = %format!("{:.4}", current_edge),
        "crypto eval: exit signal"
    );

    let signal = Signal {
        ticker: contract.ticker.clone(),
        signal_type: "crypto".to_string(),
        action: "exit".to_string(),
        direction: exit_direction.to_string(),
        model_prob: fv.probability,
        market_price: mid_price,
        edge: current_edge.abs(),
        kelly_fraction: 0.0,
        minutes_remaining,
        spread,
        order_imbalance: 0.5,
        priority: SignalPriority::NewData,
    };

    let mut mgr = order_mgr.lock().await;
    if let Err(e) = mgr
        .submit_exit(config, kalshi, pool, &signal, crypto_state)
        .await
    {
        warn!(
            ticker = %contract.ticker,
            error = %e,
            "crypto eval: exit submission failed"
        );
    }
}

/// Persist signal to the signals table.
async fn persist_signal(pool: &sqlx::PgPool, signal: &Signal) {
    let result = sqlx::query(
        r#"
        INSERT INTO signals (
            ticker, signal_type, action, direction,
            model_prob, market_price, edge, kelly_fraction,
            minutes_remaining, spread, order_imbalance, source
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'rust')
        "#,
    )
    .bind(&signal.ticker)
    .bind(&signal.signal_type)
    .bind(&signal.action)
    .bind(&signal.direction)
    .bind(signal.model_prob as f32)
    .bind(signal.market_price as f32)
    .bind(signal.edge as f32)
    .bind(signal.kelly_fraction as f32)
    .bind(signal.minutes_remaining as f32)
    .bind(signal.spread as f32)
    .bind(signal.order_imbalance as f32)
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(error = %e, "failed to persist signal");
    }
}

/// Write model state to Redis for observability.
async fn write_model_state(
    redis: &fred::clients::Client,
    ticker: &str,
    fv: &crypto_fv::CryptoFairValue,
) {
    use fred::interfaces::KeysInterface;

    let key = format!("model_state:{ticker}");
    let value = serde_json::json!({
        "probability": fv.probability,
        "shadow_rti": fv.shadow_rti,
        "vol_used": fv.vol_used,
        "basis": fv.basis,
        "basis_signal": fv.basis_signal,
        "funding_signal": fv.funding_signal,
        "confidence": fv.confidence,
        "source": "rust",
        "updated_at": Utc::now().to_rfc3339(),
    })
    .to_string();

    let result: Result<(), _> = redis.set(&key, value, None, None, false).await;
    if let Err(e) = result {
        warn!(error = %e, key = %key, "failed to write model state to Redis");
    }
    // Set 120s TTL
    let _: Result<(), _> = redis.expire(&key, 120, None).await;
}

/// Publish advisory signal to NATS for logging/comparison.
async fn publish_advisory(nats: &async_nats::Client, signal: &Signal) {
    let payload = match serde_json::to_vec(signal) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "failed to serialize advisory signal");
            return;
        }
    };

    if let Err(e) = nats
        .publish("tradebot.advisory.crypto", payload.into())
        .await
    {
        warn!(error = %e, "failed to publish advisory signal");
    }
}

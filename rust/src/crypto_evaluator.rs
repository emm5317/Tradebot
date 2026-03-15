//! Event-driven crypto evaluation — triggered by every exchange price update.
//!
//! Phase 3: Core of the event-driven architecture. Subscribes to CryptoState
//! watch channel, evaluates all active crypto contracts on every update,
//! and submits signals through the shared OrderManager.
//!
//! Replaces the Python 10-second polling loop for crypto contracts,
//! targeting sub-500ms signal generation from price update to order submission.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use rust_decimal::prelude::ToPrimitive;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use crate::lock_ext::RwLockExt;

use crate::config::Config;
use crate::contract_discovery::ContractDiscovery;
use crate::crypto_asset::CryptoAsset;
use crate::crypto_fv::{self, AssetConfig};
use crate::crypto_state::CryptoState;
use crate::crypto_state_registry::CryptoStateRegistry;
use crate::decision_log::{self, DecisionEntry, DecisionLogWriter};
use crate::feed_health::FeedHealth;
use crate::kalshi::client::KalshiClient;
use crate::kalshi::orderbook::OrderbookManager;
use crate::kalshi::trade_tape::TradeTape;
use crate::kill_switch::KillSwitchState;
use crate::order_manager::OrderManager;
use crate::types::{Signal, SignalPriority};

/// Exit edge threshold — if edge flips beyond this, exit.
const EXIT_EDGE_THRESHOLD: f64 = -0.03;

/// Per-ticker debounce interval to avoid evaluating the same ticker too rapidly.
const DEBOUNCE_MS: u128 = 250;

/// Phase 4.3: Microstructure adjustment from trade tape and orderbook.
#[derive(Debug)]
struct MicrostructureAdj {
    /// Trade tape aggressiveness * scaling factor.
    trade_imbalance: f64,
    /// Spread regime bonus/penalty.
    spread_regime: f64,
    /// Order book depth imbalance aligned with direction.
    depth_imbalance: f64,
    /// VWAP-vs-mid signal (Phase 7.3b).
    vwap_signal: f64,
    /// Price momentum signal (Phase 7.3a).
    momentum_signal: f64,
    /// Volume surge confidence boost (Phase 7.3c).
    volume_surge_signal: f64,
    /// Sum of components, clamped to [-0.06, +0.06].
    total: f64,
}

/// Phase 7.5: Edge trajectory tracker — determines whether edge is growing or shrinking.
struct EdgeTracker {
    /// Rolling window of (timestamp, adjusted_edge) per ticker.
    history: HashMap<String, VecDeque<(Instant, f64)>>,
}

impl EdgeTracker {
    fn new() -> Self {
        Self {
            history: HashMap::new(),
        }
    }

    /// Record an edge measurement for a ticker.
    fn record(&mut self, ticker: &str, edge: f64) {
        let entries = self.history.entry(ticker.to_string()).or_default();
        let now = Instant::now();
        entries.push_back((now, edge));
        // Prune entries older than 60 seconds
        let cutoff = now - Duration::from_secs(60);
        while let Some((ts, _)) = entries.front() {
            if *ts < cutoff {
                entries.pop_front();
            } else {
                break;
            }
        }
    }

    /// Compute linear regression slope of edge over the last `window_secs`.
    /// Returns cents-of-edge per second. Positive = edge growing.
    fn trend(&self, ticker: &str, window_secs: u64) -> Option<f64> {
        let entries = self.history.get(ticker)?;
        if entries.len() < 3 {
            return None;
        }
        let cutoff = Instant::now() - Duration::from_secs(window_secs);
        let recent: Vec<_> = entries.iter().filter(|(ts, _)| *ts >= cutoff).collect();
        if recent.len() < 3 {
            return None;
        }
        let anchor = recent[0].0;
        let n = recent.len() as f64;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_xy = 0.0;
        let mut sum_x2 = 0.0;

        for (ts, edge) in &recent {
            let x = ts.duration_since(anchor).as_secs_f64();
            let y = *edge;
            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_x2 += x * x;
        }
        let denom = n * sum_x2 - sum_x * sum_x;
        if denom.abs() < 1e-12 {
            return None;
        }
        Some((n * sum_xy - sum_x * sum_y) / denom)
    }

    /// Returns true if the edge is growing fast enough that we should wait for a better entry.
    fn should_wait(&self, ticker: &str) -> bool {
        if let Some(slope) = self.trend(ticker, 30) {
            // Edge growing > 0.1% per second — wait for better terms
            slope > 0.001
        } else {
            false
        }
    }
}

/// Compute microstructure adjustment from trade tape and orderbook state.
///
/// `price_momentum` is cents/second from linear regression on last_price history (7.3a).
/// `volume_surge` indicates 60s volume > 3x the 5-minute average rate (7.3c).
fn compute_microstructure_adj(
    _ticker: &str,
    direction: &str,
    order_imbalance: f64,
    mid_price: f64,
    spread: f64,
    tape: &TradeTape,
    price_momentum: f64,
    volume_surge: bool,
) -> MicrostructureAdj {
    // Trade tape aggressiveness over last 30 seconds
    let aggr = tape.aggressiveness(Duration::from_secs(30));
    let dir_sign = if direction == "yes" { 1.0 } else { -1.0 };
    let trade_imbalance = aggr * dir_sign * 0.02;

    // Spread regime
    let spread_regime = if spread < 0.04 {
        0.01 // tight spread bonus
    } else if spread > 0.10 {
        -0.02 // wide spread penalty
    } else {
        0.0
    };

    // Depth imbalance: order_imbalance is bid_depth/(bid_depth+ask_depth), ~0.5 balanced
    // Align with trade direction
    let depth_imbalance = (order_imbalance - 0.5) * dir_sign * 0.02;

    // Phase 7.3b: VWAP-vs-mid signal
    // VWAP above mid = buying pressure (bullish for YES), below = selling pressure
    let vwap_signal = if spread > 0.0 {
        tape.vwap(Duration::from_secs(60))
            .map(|vwap| ((vwap - mid_price) / spread).clamp(-1.0, 1.0) * dir_sign * 0.02)
            .unwrap_or(0.0)
    } else {
        0.0
    };

    // Phase 7.3a: Price momentum signal
    // Positive momentum aligned with direction = market converging toward our view = bonus
    let momentum_signal = (price_momentum * dir_sign).clamp(-1.0, 1.0) * 0.02;

    // Phase 7.3c: Volume surge confidence boost
    // Unusual volume suggests informed trading — small bonus when aligned with direction
    let volume_surge_signal = if volume_surge {
        // Use trade aggressiveness to determine alignment
        let aligned = (aggr * dir_sign) > 0.0;
        if aligned { 0.01 } else { -0.01 }
    } else {
        0.0
    };

    let total = (trade_imbalance
        + spread_regime
        + depth_imbalance
        + vwap_signal
        + momentum_signal
        + volume_surge_signal)
        .clamp(-0.06, 0.06);

    MicrostructureAdj {
        trade_imbalance,
        spread_regime,
        depth_imbalance,
        vwap_signal,
        momentum_signal,
        volume_surge_signal,
        total,
    }
}

/// Contract lifecycle phase.
enum ContractPhase {
    /// Too early, skip.
    Eligible,
    /// In entry window: generate entry signals.
    InWindow,
    /// Near settlement: exit signals only.
    ExitOnly,
}

fn contract_phase(minutes_remaining: f64, min_minutes: f64, max_minutes: f64) -> ContractPhase {
    if minutes_remaining > max_minutes {
        ContractPhase::Eligible
    } else if minutes_remaining >= min_minutes {
        ContractPhase::InWindow
    } else {
        ContractPhase::ExitOnly
    }
}

/// Run the event-driven crypto evaluator.
///
/// Subscribes to CryptoState changes and evaluates all active crypto contracts
/// on every exchange price update.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Arc<Config>,
    registry: Arc<CryptoStateRegistry>,
    contract_discovery: Arc<ContractDiscovery>,
    orderbooks: Arc<OrderbookManager>,
    trade_tape: Arc<RwLock<TradeTape>>,
    order_mgr: Arc<tokio::sync::Mutex<OrderManager>>,
    kalshi: Arc<KalshiClient>,
    kill_switch: Arc<KillSwitchState>,
    feed_health: Arc<FeedHealth>,
    pool: sqlx::PgPool,
    redis: fred::clients::Client,
    nats: async_nats::Client,
    cancel: CancellationToken,
    decision_writer: DecisionLogWriter,
) {
    let mut rx = registry.subscribe();
    let mut last_eval: HashMap<String, Instant> = HashMap::new();
    let mut last_summary = Instant::now();
    let mut edge_tracker = EdgeTracker::new();
    // Phase 14: Per-ticker signal cooldown and global rate limiter
    let mut last_signal_fired: HashMap<String, Instant> = HashMap::new();
    let mut signal_timestamps: VecDeque<Instant> = VecDeque::new();

    info!("crypto evaluator started (event-driven, multi-asset)");

    loop {
        tokio::select! {
            result = rx.changed() => {
                if result.is_err() {
                    // Sender dropped — registry is gone
                    break;
                }

                let eval_start = Instant::now();

                // Periodic 60s summary
                if last_summary.elapsed() >= Duration::from_secs(60) {
                    let contracts = contract_discovery.active_contracts();

                    // Log per-asset contract counts
                    let mut asset_counts: HashMap<CryptoAsset, usize> = HashMap::new();
                    for c in &contracts {
                        *asset_counts.entry(c.asset).or_default() += 1;
                    }
                    let asset_summary: String = asset_counts
                        .iter()
                        .map(|(a, n)| format!("{}={}", a.short_name(), n))
                        .collect::<Vec<_>>()
                        .join(", ");

                    info!(
                        contract_count = contracts.len(),
                        assets = %asset_summary,
                        enabled_assets = ?registry.enabled_assets(),
                        "crypto eval: 60s summary"
                    );
                    last_summary = Instant::now();

                    // Publish feed health as Prometheus gauges
                    feed_health.publish_metrics();

                    // Write feed health snapshot to DB for Grafana
                    let details = feed_health.health_detail();
                    let pool_snap = pool.clone();
                    tokio::spawn(async move {
                        for d in &details {
                            decision_log::write_feed_health(
                                &pool_snap,
                                &d.feed,
                                d.score,
                                d.age_ms.map(|ms| ms as f64),
                                !d.healthy,
                            ).await;
                        }
                    });
                }

                let contracts = contract_discovery.active_contracts();
                if contracts.is_empty() {
                    trace!("crypto eval: skipping, no active contracts");
                    continue;
                }

                for contract in &contracts {
                    // Get the per-asset CryptoState
                    let asset_state = match registry.get(contract.asset) {
                        Some(s) => s,
                        None => continue, // asset not enabled
                    };
                    let snap = asset_state.snapshot();
                    if snap.shadow_rti <= 0.0 {
                        continue; // no price data yet for this asset
                    }

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

                    let asset_config = AssetConfig::for_asset_with_full_overrides(
                        contract.asset,
                        config.crypto_vol_multiplier,
                        config.crypto_prob_ceiling,
                        config.crypto_compress_factor,
                    ).with_trading_overrides(
                        config.crypto_min_edge,
                        config.crypto_min_kelly,
                        config.crypto_max_edge,
                        config.crypto_max_market_disagreement,
                        config.crypto_cooldown_secs,
                    );

                    match contract_phase(minutes_remaining, config.crypto_entry_min_minutes, config.crypto_entry_max_minutes) {
                        ContractPhase::Eligible => {
                            trace!(ticker = %contract.ticker, minutes = %format!("{:.1}", minutes_remaining), "crypto eval: eligible phase, too early");
                            continue;
                        }
                        ContractPhase::InWindow => {
                            evaluate_entry(
                                &config,
                                &snap,
                                contract,
                                minutes_remaining,
                                &orderbooks,
                                &trade_tape,
                                &order_mgr,
                                &kalshi,
                                &kill_switch,
                                &feed_health,
                                &pool,
                                &redis,
                                &nats,
                                asset_state,
                                &mut edge_tracker,
                                &decision_writer,
                                &asset_config,
                                &mut last_signal_fired,
                                &mut signal_timestamps,
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
                                &trade_tape,
                                &order_mgr,
                                &kalshi,
                                &pool,
                                &redis,
                                asset_state,
                                &asset_config,
                            )
                            .await;
                        }
                    }
                }

                let elapsed = eval_start.elapsed();
                metrics::histogram!(
                    crate::metrics_registry::EVAL_DURATION,
                    "signal_type" => "crypto"
                ).record(elapsed.as_secs_f64());
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
    trade_tape: &Arc<RwLock<TradeTape>>,
    order_mgr: &Arc<tokio::sync::Mutex<OrderManager>>,
    kalshi: &Arc<KalshiClient>,
    kill_switch: &KillSwitchState,
    feed_health: &FeedHealth,
    pool: &sqlx::PgPool,
    redis: &fred::clients::Client,
    nats: &async_nats::Client,
    crypto_state: &CryptoState,
    edge_tracker: &mut EdgeTracker,
    decision_writer: &DecisionLogWriter,
    asset_config: &AssetConfig,
    last_signal_fired: &mut HashMap<String, Instant>,
    signal_timestamps: &mut VecDeque<Instant>,
) {
    let eval_start = Instant::now();

    // 1. Read orderbook (with ticker fallback)
    let (mid_price, mid_source) = match orderbooks.mid_price_with_fallback(&contract.ticker) {
        Some((d, src)) => (d.to_f64().unwrap_or(0.5), src),
        None => {
            let status = orderbooks.book_status(&contract.ticker);
            debug!(
                ticker = %contract.ticker,
                has_entry = status.has_entry,
                has_bids = status.has_bids,
                has_asks = status.has_asks,
                has_tob = status.has_tob,
                "crypto eval: no price data"
            );
            let reason = format!(
                "no_price_data(book={},bids={},asks={},tob={})",
                status.has_entry, status.has_bids, status.has_asks, status.has_tob
            );
            decision_writer.send(DecisionEntry {
                ticker: contract.ticker.clone(),
                signal_type: "crypto".into(),
                outcome: "skipped".into(),
                rejection_reason: Some(reason),
                minutes_remaining: Some(minutes_remaining),
                ..Default::default()
            });
            return;
        }
    };
    if mid_source != crate::kalshi::orderbook::MidPriceSource::Orderbook {
        debug!(
            ticker = %contract.ticker,
            source = ?mid_source,
            mid = %format!("{:.4}", mid_price),
            "crypto eval: using fallback mid price"
        );
    }
    let raw_spread = orderbooks
        .spread(&contract.ticker)
        .and_then(|d| d.to_f64())
        .unwrap_or_else(|| {
            // Fallback: compute from ticker ToB, or use conservative default
            orderbooks
                .ticker_tob(&contract.ticker)
                .and_then(|tob| {
                    let bid = tob.yes_bid? as f64 / 100.0;
                    let ask = tob.yes_ask? as f64 / 100.0;
                    Some(ask - bid)
                })
                .unwrap_or(0.10)
        });
    // Negative spread = crossed/invalid book — treat as no reliable spread data
    let spread = if raw_spread < 0.0 {
        debug!(
            ticker = %contract.ticker,
            raw_spread = %format!("{:.4}", raw_spread),
            "crypto eval: negative spread, using conservative default"
        );
        0.10
    } else {
        raw_spread
    };

    // 1b. Early reject extreme-price contracts (market ≤2¢ or ≥98¢)
    // These will always fail the kelly fill_price guard [0.03, 0.97],
    // so skip the expensive FV/microstructure computation entirely.
    if mid_price <= 0.02 || mid_price >= 0.98 {
        debug!(
            ticker = %contract.ticker,
            mid = %format!("{:.4}", mid_price),
            "crypto eval: extreme market price, skipping"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "skipped".into(),
            rejection_reason: Some("extreme_market_price".into()),
            market_price: Some(mid_price),
            minutes_remaining: Some(minutes_remaining),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // Phase 14: Per-ticker signal cooldown (Phase 15: per-asset thresholds)
    if let Some(last_fired) = last_signal_fired.get(&contract.ticker) {
        let elapsed = last_fired.elapsed().as_secs();
        if elapsed < asset_config.cooldown_secs {
            trace!(
                ticker = %contract.ticker,
                elapsed_secs = elapsed,
                cooldown_secs = asset_config.cooldown_secs,
                "crypto eval: signal cooldown"
            );
            decision_writer.send(DecisionEntry {
                ticker: contract.ticker.clone(),
                signal_type: "crypto".into(),
                outcome: "rejected".into(),
                rejection_reason: Some(format!("signal_cooldown ({}s/{}s)", elapsed, asset_config.cooldown_secs)),
                minutes_remaining: Some(minutes_remaining),
                eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
                ..Default::default()
            });
            return;
        }
    }

    // Phase 14: Global hourly signal rate limiter
    let now_instant = Instant::now();
    let one_hour = Duration::from_secs(3600);
    while let Some(front) = signal_timestamps.front() {
        if now_instant.duration_since(*front) > one_hour {
            signal_timestamps.pop_front();
        } else {
            break;
        }
    }
    if signal_timestamps.len() >= config.crypto_max_signals_per_hour as usize {
        debug!(
            ticker = %contract.ticker,
            signals_this_hour = signal_timestamps.len(),
            max = config.crypto_max_signals_per_hour,
            "crypto eval: hourly rate limit"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("hourly_rate_limit".into()),
            minutes_remaining: Some(minutes_remaining),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    let order_imbalance = orderbooks.order_imbalance(&contract.ticker).unwrap_or(0.5);

    // 2. Compute fair value
    // Read microstructure signals early — directional model needs them for FV
    let (price_momentum, volume_surge) = read_ticker_signals(redis, &contract.ticker).await;

    let fv = if contract.directional {
        // Directional "Will BTC go up?" — momentum/order-flow model
        let tape_data = {
            let tape = trade_tape.read_or_recover();
            let aggr = tape.aggressiveness(Duration::from_secs(30));
            let vwap_dev = if spread > 0.0 {
                tape.vwap(Duration::from_secs(60))
                    .map(|vwap| ((vwap - mid_price) / spread).clamp(-1.0, 1.0))
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            (aggr, vwap_dev)
        };
        // Volume surge is aligned if trade aggression and momentum agree
        let dir_sign = if price_momentum >= 0.0 { 1.0 } else { -1.0 };
        let surge_aligned = volume_surge && (tape_data.0 * dir_sign) > 0.0;

        crypto_fv::compute_directional_fair_value_with_config(
            snap,
            minutes_remaining,
            price_momentum,
            tape_data.0,     // trade_imbalance (aggressiveness)
            tape_data.1,     // vwap_deviation
            order_imbalance, // depth_imbalance
            surge_aligned,
            asset_config,
        )
    } else {
        // Strike-based contracts — standard N(d2) model
        crypto_fv::compute_crypto_fair_value_with_config(snap, contract.strike, minutes_remaining, asset_config)
    };

    // 3. Check confidence
    if fv.confidence < config.crypto_min_confidence {
        debug!(ticker = %contract.ticker, confidence = %format!("{:.2}", fv.confidence), "crypto eval: confidence below minimum");
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("confidence_below_minimum".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            confidence: Some(fv.confidence),
            minutes_remaining: Some(minutes_remaining),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 4. Direction and raw edge
    let (direction, raw_edge) = crypto_fv::determine_direction(fv.probability, mid_price);

    // 4a. Guard: ATM directional no-opinion — N(d2) at ATM is ~0.50 by necessity, not signal
    if contract.directional
        && (fv.probability - 0.50).abs() < config.crypto_directional_min_conviction
    {
        debug!(
            ticker = %contract.ticker,
            model_prob = %format!("{:.4}", fv.probability),
            "crypto eval: directional ATM flatness — no opinion"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("directional_atm_no_opinion".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(raw_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 4b. Guard: market price band — only trade contracts priced 30-70c (near-ATM)
    //     Deep OTM (<30c) and deep ITM (>70c) have ~0% model accuracy.
    if mid_price < config.crypto_market_price_floor || mid_price > config.crypto_market_price_ceiling {
        debug!(
            ticker = %contract.ticker,
            market_price = %format!("{:.4}", mid_price),
            floor = %format!("{:.2}", config.crypto_market_price_floor),
            ceiling = %format!("{:.2}", config.crypto_market_price_ceiling),
            "crypto eval: market price outside tradeable band"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("market_price_out_of_band".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(raw_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 4c. Guard: market disagreement — model shouldn't disagree with market by >N% (per-asset)
    if raw_edge > asset_config.max_market_disagreement {
        debug!(
            ticker = %contract.ticker,
            raw_edge = %format!("{:.4}", raw_edge),
            model_prob = %format!("{:.4}", fv.probability),
            market_price = %format!("{:.4}", mid_price),
            "crypto eval: market disagreement too large"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("market_disagreement".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(raw_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 5. Effective edge (spread-adjusted)
    let effective_edge = crypto_fv::compute_effective_edge(raw_edge, spread);

    // Phase 4.3: Microstructure adjustment
    // For directional contracts, momentum/VWAP/depth/volume are already model inputs
    // — only apply spread_regime to avoid double-counting.
    let micro = if contract.directional {
        let spread_regime = if spread < 0.04 {
            0.01
        } else if spread > 0.10 {
            -0.02
        } else {
            0.0
        };
        MicrostructureAdj {
            trade_imbalance: 0.0,
            spread_regime,
            depth_imbalance: 0.0,
            vwap_signal: 0.0,
            momentum_signal: 0.0,
            volume_surge_signal: 0.0,
            total: spread_regime,
        }
    } else {
        let tape = trade_tape.read_or_recover();
        compute_microstructure_adj(
            &contract.ticker,
            direction,
            order_imbalance,
            mid_price,
            spread,
            &tape,
            price_momentum,
            volume_surge,
        )
    };
    let adjusted_edge = effective_edge + micro.total;

    // 7.5: Track edge trajectory
    edge_tracker.record(&contract.ticker, adjusted_edge);

    // 6a. Check maximum edge (model miscalibration filter)
    if adjusted_edge > asset_config.max_edge {
        debug!(ticker = %contract.ticker, edge = %format!("{:.4}", adjusted_edge), "crypto eval: edge too large (likely miscalibration)");
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("edge_too_large".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(effective_edge),
            adjusted_edge: Some(adjusted_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            micro_total: Some(micro.total),
            micro_trade: Some(micro.trade_imbalance),
            micro_spread: Some(micro.spread_regime),
            micro_depth: Some(micro.depth_imbalance),
            micro_vwap: Some(micro.vwap_signal),
            micro_momentum: Some(micro.momentum_signal),
            micro_vol_surge: Some(micro.volume_surge_signal),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 6b. Check minimum edge (using microstructure-adjusted edge)
    if adjusted_edge < asset_config.min_edge {
        debug!(ticker = %contract.ticker, edge = %format!("{:.4}", adjusted_edge), "crypto eval: edge below minimum");
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("edge_below_minimum".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(effective_edge),
            adjusted_edge: Some(adjusted_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            micro_total: Some(micro.total),
            micro_trade: Some(micro.trade_imbalance),
            micro_spread: Some(micro.spread_regime),
            micro_depth: Some(micro.depth_imbalance),
            micro_vwap: Some(micro.vwap_signal),
            micro_momentum: Some(micro.momentum_signal),
            micro_vol_surge: Some(micro.volume_surge_signal),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 7.5: If edge is growing rapidly, defer firing to capture better entry
    if edge_tracker.should_wait(&contract.ticker) {
        debug!(
            ticker = %contract.ticker,
            edge = %format!("{:.4}", adjusted_edge),
            "crypto eval: edge growing, deferring entry"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("edge_growing_defer".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(effective_edge),
            adjusted_edge: Some(adjusted_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 7. Fill price estimate — use book-walking VWAP when available
    let book_side = if direction == "yes" {
        crate::kalshi::orderbook::Side::Bid
    } else {
        crate::kalshi::orderbook::Side::Ask
    };
    // Estimate fill for a typical order size (use max_trade_size as upper bound)
    let est_size = config.max_trade_size_cents.min(100);
    let fill_price = orderbooks
        .estimated_fill_price(&contract.ticker, book_side, est_size)
        .and_then(|d| d.to_f64())
        .unwrap_or_else(|| crypto_fv::estimate_fill_price(direction, mid_price, spread));

    // 8. Kelly criterion (with configurable fill price bounds)
    let fill_max = 1.0 - config.crypto_kelly_fill_min;
    let kelly = crypto_fv::compute_kelly_with_bounds(fv.probability, fill_price, direction, config.crypto_kelly_fill_min, fill_max);

    // 8b. Reject trades with terrible risk/reward ratio
    let (win_payout, lose_payout) = if direction == "yes" {
        (1.0 - fill_price, fill_price)
    } else {
        (fill_price, 1.0 - fill_price)
    };
    if lose_payout > config.crypto_risk_reward_max_ratio * win_payout {
        debug!(
            ticker = %contract.ticker,
            fill_price = %format!("{:.4}", fill_price),
            win_payout = %format!("{:.4}", win_payout),
            lose_payout = %format!("{:.4}", lose_payout),
            ratio = config.crypto_risk_reward_max_ratio,
            "crypto eval: bad risk/reward ratio"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("bad_risk_reward".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(effective_edge),
            adjusted_edge: Some(adjusted_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 9. Check minimum Kelly
    if kelly < asset_config.min_kelly {
        debug!(ticker = %contract.ticker, kelly = %format!("{:.4}", kelly), "crypto eval: kelly below minimum");
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some("kelly_below_minimum".into()),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(effective_edge),
            adjusted_edge: Some(adjusted_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            micro_total: Some(micro.total),
            micro_trade: Some(micro.trade_imbalance),
            micro_spread: Some(micro.spread_regime),
            micro_depth: Some(micro.depth_imbalance),
            micro_vwap: Some(micro.vwap_signal),
            micro_momentum: Some(micro.momentum_signal),
            micro_vol_surge: Some(micro.volume_surge_signal),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
        return;
    }

    // 9b. Per-asset feed health gate (Phase 15 fix #3)
    if let Err(stale) = feed_health.required_feeds_healthy_for_asset(contract.asset) {
        warn!(
            ticker = %contract.ticker,
            asset = %contract.asset.short_name(),
            stale_feeds = ?stale,
            "crypto eval: asset feeds stale"
        );
        decision_writer.send(DecisionEntry {
            ticker: contract.ticker.clone(),
            signal_type: "crypto".into(),
            outcome: "rejected".into(),
            rejection_reason: Some(format!("asset_feeds_stale:{}", contract.asset.short_name())),
            model_prob: Some(fv.probability),
            market_price: Some(mid_price),
            edge: Some(effective_edge),
            adjusted_edge: Some(adjusted_edge),
            direction: Some(direction.to_string()),
            minutes_remaining: Some(minutes_remaining),
            confidence: Some(fv.confidence),
            eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
            ..Default::default()
        });
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
        edge: adjusted_edge,
        kelly_fraction: kelly,
        minutes_remaining,
        spread,
        order_imbalance,
        priority: SignalPriority::NewData,
        confidence: fv.confidence,
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
        micro_total = %format!("{:.4}", micro.total),
        micro_trade = %format!("{:.4}", micro.trade_imbalance),
        micro_spread = %format!("{:.4}", micro.spread_regime),
        micro_depth = %format!("{:.4}", micro.depth_imbalance),
        micro_vwap = %format!("{:.4}", micro.vwap_signal),
        micro_momentum = %format!("{:.4}", micro.momentum_signal),
        micro_vol_surge = %format!("{:.4}", micro.volume_surge_signal),
        "crypto eval: entry signal"
    );

    // Persist signal first to get DB id for order linkage
    let signal_id = persist_signal(pool, &signal, contract.settlement_time).await;
    if signal_id.is_none() {
        warn!(ticker = %signal.ticker, "crypto eval: signal persist failed, skipping order");
        return;
    }

    // Phase 14: Record signal fire time for cooldown and rate limiting
    last_signal_fired.insert(contract.ticker.clone(), Instant::now());
    signal_timestamps.push_back(Instant::now());

    // 11. Risk check + submit
    {
        let mut mgr = order_mgr.lock().await;

        if let Err(reason) = mgr.check_risk(config, &signal, kill_switch, feed_health) {
            warn!(
                ticker = %signal.ticker,
                reason = %reason,
                "crypto eval: signal rejected"
            );
            decision_writer.send(DecisionEntry {
                ticker: signal.ticker.clone(),
                signal_type: "crypto".into(),
                outcome: "rejected".into(),
                rejection_reason: Some(reason.clone()),
                model_prob: Some(fv.probability),
                market_price: Some(mid_price),
                edge: Some(effective_edge),
                adjusted_edge: Some(adjusted_edge),
                direction: Some(direction.to_string()),
                minutes_remaining: Some(minutes_remaining),
                confidence: Some(fv.confidence),
                signal_id,
                eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
                ..Default::default()
            });
            return;
        }

        if let Err(e) = mgr
            .submit_entry(config, kalshi, pool, &signal, signal_id, crypto_state)
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

    // Mark signal as acted on
    if let Some(sid) = signal_id {
        let pool_c = pool.clone();
        tokio::spawn(async move {
            let _ = sqlx::query("UPDATE signals SET acted_on = true WHERE id = $1")
                .bind(sid)
                .execute(&pool_c)
                .await;
        });
    }

    // Metrics: successful signal
    metrics::counter!(
        crate::metrics_registry::EVAL_TOTAL,
        "signal_type" => "crypto",
        "outcome" => "signal"
    )
    .increment(1);
    metrics::histogram!(
        crate::metrics_registry::ORDER_LATENCY,
        "signal_type" => "crypto"
    )
    .record(eval_start.elapsed().as_secs_f64());

    // Decision log: successful signal
    decision_writer.send(DecisionEntry {
        ticker: signal.ticker.clone(),
        signal_type: "crypto".into(),
        outcome: "signal".into(),
        model_prob: Some(fv.probability),
        market_price: Some(mid_price),
        edge: Some(effective_edge),
        adjusted_edge: Some(adjusted_edge),
        direction: Some(direction.to_string()),
        minutes_remaining: Some(minutes_remaining),
        confidence: Some(fv.confidence),
        micro_total: Some(micro.total),
        micro_trade: Some(micro.trade_imbalance),
        micro_spread: Some(micro.spread_regime),
        micro_depth: Some(micro.depth_imbalance),
        micro_vwap: Some(micro.vwap_signal),
        micro_momentum: Some(micro.momentum_signal),
        micro_vol_surge: Some(micro.volume_surge_signal),
        signal_id,
        eval_latency_ms: Some(eval_start.elapsed().as_secs_f64() * 1000.0),
        ..Default::default()
    });

    // Publish advisory and model state (fire-and-forget)
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
    trade_tape: &Arc<RwLock<TradeTape>>,
    order_mgr: &Arc<tokio::sync::Mutex<OrderManager>>,
    kalshi: &Arc<KalshiClient>,
    pool: &sqlx::PgPool,
    redis: &fred::clients::Client,
    crypto_state: &CryptoState,
    asset_config: &AssetConfig,
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

    let mid_price = match orderbooks.mid_price_with_fallback(&contract.ticker) {
        Some((d, _src)) => d.to_f64().unwrap_or(0.5),
        None => return,
    };
    let raw_spread = orderbooks
        .spread(&contract.ticker)
        .and_then(|d| d.to_f64())
        .unwrap_or_else(|| {
            orderbooks
                .ticker_tob(&contract.ticker)
                .and_then(|tob| {
                    let bid = tob.yes_bid? as f64 / 100.0;
                    let ask = tob.yes_ask? as f64 / 100.0;
                    Some(ask - bid)
                })
                .unwrap_or(0.10)
        });
    let spread = if raw_spread < 0.0 { 0.10 } else { raw_spread };

    let fv = if contract.directional {
        let (price_momentum, volume_surge) = read_ticker_signals(redis, &contract.ticker).await;
        let order_imbalance = orderbooks.order_imbalance(&contract.ticker).unwrap_or(0.5);
        let tape_data = {
            let tape = trade_tape.read_or_recover();
            let aggr = tape.aggressiveness(Duration::from_secs(30));
            let vwap_dev = if spread > 0.0 {
                tape.vwap(Duration::from_secs(60))
                    .map(|vwap| ((vwap - mid_price) / spread).clamp(-1.0, 1.0))
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            (aggr, vwap_dev)
        };
        let dir_sign = if price_momentum >= 0.0 { 1.0 } else { -1.0 };
        let surge_aligned = volume_surge && (tape_data.0 * dir_sign) > 0.0;
        crypto_fv::compute_directional_fair_value_with_config(
            snap,
            minutes_remaining,
            price_momentum,
            tape_data.0,
            tape_data.1,
            order_imbalance,
            surge_aligned,
            asset_config,
        )
    } else {
        crypto_fv::compute_crypto_fair_value_with_config(snap, contract.strike, minutes_remaining, asset_config)
    };

    // Check if edge has flipped
    let current_edge = if held_direction == "yes" {
        fv.probability - mid_price
    } else {
        mid_price - fv.probability
    };

    if current_edge >= EXIT_EDGE_THRESHOLD {
        return; // Edge hasn't flipped enough to exit
    }

    let exit_direction = if held_direction == "yes" { "no" } else { "yes" };

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
        confidence: fv.confidence,
    };

    let mut mgr = order_mgr.lock().await;
    if let Err(e) = mgr
        .submit_exit(config, kalshi, pool, &signal, None, crypto_state)
        .await
    {
        warn!(
            ticker = %contract.ticker,
            error = %e,
            "crypto eval: exit submission failed"
        );
    }
}

/// Read per-ticker momentum and volume surge from Redis orderbook summary.
///
/// Returns (price_momentum, volume_surge). Falls back to (0.0, false) on error.
async fn read_ticker_signals(redis: &fred::clients::Client, ticker: &str) -> (f64, bool) {
    use fred::interfaces::KeysInterface;

    let key = format!("orderbook:{ticker}");
    let result: Result<Option<String>, _> = redis.get(&key).await;
    match result {
        Ok(Some(json_str)) => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json_str) {
                let momentum = v
                    .get("price_momentum")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let surge = v
                    .get("volume_surge")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                (momentum, surge)
            } else {
                (0.0, false)
            }
        }
        _ => (0.0, false),
    }
}

/// Persist signal to the signals table. Returns the DB-assigned signal id.
///
/// Ensures the contract row exists first (lightweight upsert) to avoid FK violation
/// when a new contract ticker hasn't been synced yet.
async fn persist_signal(
    pool: &sqlx::PgPool,
    signal: &Signal,
    settlement_time: chrono::DateTime<Utc>,
) -> Option<i64> {
    // Ensure contract row exists to satisfy FK constraint
    let _ = sqlx::query(
        r#"INSERT INTO contracts (ticker, title, category, settlement_time, status)
           VALUES ($1, '', 'crypto', $2, 'active')
           ON CONFLICT (ticker) DO NOTHING"#,
    )
    .bind(&signal.ticker)
    .bind(settlement_time)
    .execute(pool)
    .await;

    let result = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO signals (
            ticker, signal_type, direction,
            model_prob, market_price, edge, kelly_fraction,
            minutes_remaining, observation_data
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id
        "#,
    )
    .bind(&signal.ticker)
    .bind(&signal.signal_type)
    .bind(&signal.direction)
    .bind(signal.model_prob as f32)
    .bind(signal.market_price as f32)
    .bind(signal.edge as f32)
    .bind(signal.kelly_fraction as f32)
    .bind(signal.minutes_remaining as f32)
    .bind(serde_json::json!({
        "action": signal.action,
        "spread": signal.spread,
        "order_imbalance": signal.order_imbalance,
        "source": "rust"
    }))
    .fetch_one(pool)
    .await;

    match result {
        Ok(id) => Some(id),
        Err(e) => {
            warn!(error = %e, ticker = %signal.ticker, "failed to persist signal");
            None
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kalshi::trade_tape::TradeRecord;

    fn empty_tape() -> TradeTape {
        TradeTape::new(100)
    }

    fn tape_with_trades(yes_count: i64, no_count: i64) -> TradeTape {
        let mut tape = TradeTape::new(100);
        if yes_count > 0 {
            tape.record(TradeRecord {
                ticker: "TEST".to_string(),
                price_cents: 50,
                count: yes_count,
                taker_side: "yes".to_string(),
                timestamp: Instant::now(),
            });
        }
        if no_count > 0 {
            tape.record(TradeRecord {
                ticker: "TEST".to_string(),
                price_cents: 50,
                count: no_count,
                taker_side: "no".to_string(),
                timestamp: Instant::now(),
            });
        }
        tape
    }

    #[test]
    fn test_microstructure_momentum_aligned_yes() {
        let tape = empty_tape();
        // Positive momentum + yes direction = positive signal
        let adj = compute_microstructure_adj("T", "yes", 0.5, 0.5, 0.05, &tape, 0.5, false);
        assert!(
            adj.momentum_signal > 0.0,
            "momentum should be positive when aligned"
        );
    }

    #[test]
    fn test_microstructure_momentum_opposed_yes() {
        let tape = empty_tape();
        // Negative momentum + yes direction = negative signal
        let adj = compute_microstructure_adj("T", "yes", 0.5, 0.5, 0.05, &tape, -0.5, false);
        assert!(
            adj.momentum_signal < 0.0,
            "momentum should be negative when opposed"
        );
    }

    #[test]
    fn test_microstructure_volume_surge_aligned() {
        // Bullish aggression (yes trades) + yes direction + surge
        let tape = tape_with_trades(30, 10);
        let adj = compute_microstructure_adj("T", "yes", 0.5, 0.5, 0.05, &tape, 0.0, true);
        assert_eq!(adj.volume_surge_signal, 0.01);
    }

    #[test]
    fn test_microstructure_volume_surge_opposed() {
        // Bearish aggression (no trades) + yes direction + surge
        let tape = tape_with_trades(10, 30);
        let adj = compute_microstructure_adj("T", "yes", 0.5, 0.5, 0.05, &tape, 0.0, true);
        assert_eq!(adj.volume_surge_signal, -0.01);
    }

    #[test]
    fn test_microstructure_no_volume_surge() {
        let tape = empty_tape();
        let adj = compute_microstructure_adj("T", "yes", 0.5, 0.5, 0.05, &tape, 0.0, false);
        assert_eq!(adj.volume_surge_signal, 0.0);
    }

    #[test]
    fn test_microstructure_total_clamped() {
        let tape = empty_tape();
        // Extreme momentum should still be clamped to [-0.06, 0.06]
        let adj = compute_microstructure_adj("T", "yes", 0.5, 0.5, 0.05, &tape, 100.0, true);
        assert!(adj.total <= 0.06);
        assert!(adj.total >= -0.06);
    }

    #[test]
    fn test_edge_tracker_no_data() {
        let tracker = EdgeTracker::new();
        assert!(tracker.trend("TICKER", 30).is_none());
        assert!(!tracker.should_wait("TICKER"));
    }

    #[test]
    fn test_edge_tracker_growing_edge() {
        let mut tracker = EdgeTracker::new();
        let base = Instant::now() - Duration::from_secs(30);

        // Simulate edge growing over 30 seconds
        let entries = tracker.history.entry("TICKER".to_string()).or_default();
        for i in 0..20 {
            entries.push_back((base + Duration::from_secs(i), 0.01 + i as f64 * 0.005));
        }

        let slope = tracker.trend("TICKER", 30).unwrap();
        assert!(
            slope > 0.001,
            "growing edge should have positive slope > 0.001, got {slope}"
        );
        assert!(
            tracker.should_wait("TICKER"),
            "should wait when edge is growing fast"
        );
    }

    #[test]
    fn test_edge_tracker_shrinking_edge() {
        let mut tracker = EdgeTracker::new();
        let base = Instant::now() - Duration::from_secs(30);

        let entries = tracker.history.entry("TICKER".to_string()).or_default();
        for i in 0..20 {
            entries.push_back((base + Duration::from_secs(i), 0.10 - i as f64 * 0.005));
        }

        let slope = tracker.trend("TICKER", 30).unwrap();
        assert!(
            slope < 0.0,
            "shrinking edge should have negative slope, got {slope}"
        );
        assert!(
            !tracker.should_wait("TICKER"),
            "should NOT wait when edge is shrinking"
        );
    }

    #[test]
    fn test_edge_tracker_stable_edge() {
        let mut tracker = EdgeTracker::new();
        let base = Instant::now() - Duration::from_secs(30);

        let entries = tracker.history.entry("TICKER".to_string()).or_default();
        for i in 0..20 {
            entries.push_back((base + Duration::from_secs(i), 0.05));
        }

        let slope = tracker.trend("TICKER", 30).unwrap();
        assert!(
            slope.abs() < 0.001,
            "stable edge should have ~0 slope, got {slope}"
        );
        assert!(
            !tracker.should_wait("TICKER"),
            "should NOT wait when edge is stable"
        );
    }

    #[test]
    fn test_market_disagreement_guard_logic() {
        // When raw_edge > threshold, trade should be rejected
        let threshold = 0.25;

        // Model says 0.50, market says 0.99 → raw_edge = 0.49 > 0.25 → REJECT
        let (_, raw_edge) = crypto_fv::determine_direction(0.50, 0.99);
        assert!(
            raw_edge > threshold,
            "0.49 edge should exceed 0.25 threshold"
        );

        // Model says 0.60, market says 0.55 → raw_edge = 0.05 < 0.25 → PASS
        let (_, raw_edge) = crypto_fv::determine_direction(0.60, 0.55);
        assert!(
            raw_edge < threshold,
            "0.05 edge should be below 0.25 threshold"
        );

        // Model says 0.30, market says 0.55 → raw_edge = 0.25, at boundary → PASS (not >)
        let (_, raw_edge) = crypto_fv::determine_direction(0.30, 0.55);
        assert!((raw_edge - 0.25).abs() < 1e-10, "boundary case");
    }

    #[test]
    fn test_atm_directional_flatness_guard_logic() {
        // ATM directional: model_prob near 0.50 → no opinion → REJECT
        let conviction_threshold: f64 = 0.05;

        // model_prob = 0.50 → |0.50 - 0.50| = 0.0 < 0.05 → REJECT
        let model_prob: f64 = 0.50;
        assert!((model_prob - 0.50).abs() < conviction_threshold);

        // model_prob = 0.52 → |0.52 - 0.50| = 0.02 < 0.05 → REJECT
        let model_prob: f64 = 0.52;
        assert!((model_prob - 0.50).abs() < conviction_threshold);

        // model_prob = 0.56 → |0.56 - 0.50| = 0.06 > 0.05 → PASS
        let model_prob: f64 = 0.56;
        assert!((model_prob - 0.50).abs() >= conviction_threshold);
    }

    #[test]
    fn test_directional_microstructure_skip() {
        // For directional contracts, only spread_regime should apply
        // (other components are already in the directional FV model)
        let spread = 0.03; // tight → bonus 0.01
        let spread_regime = if spread < 0.04 {
            0.01
        } else if spread > 0.10 {
            -0.02
        } else {
            0.0
        };
        assert_eq!(spread_regime, 0.01);

        let spread = 0.15; // wide → penalty -0.02
        let spread_regime = if spread < 0.04 {
            0.01
        } else if spread > 0.10 {
            -0.02
        } else {
            0.0
        };
        assert_eq!(spread_regime, -0.02);

        let spread = 0.06; // normal → 0
        let spread_regime = if spread < 0.04 {
            0.01
        } else if spread > 0.10 {
            -0.02
        } else {
            0.0
        };
        assert_eq!(spread_regime, 0.0);
    }

    #[test]
    fn test_edge_tracker_pruning() {
        let mut tracker = EdgeTracker::new();
        // Record old edge (will be pruned)
        let entries = tracker.history.entry("T".to_string()).or_default();
        entries.push_back((Instant::now() - Duration::from_secs(120), 0.05));

        // Record via public method — should prune old entry
        tracker.record("T", 0.06);
        assert_eq!(tracker.history["T"].len(), 1);
    }

    #[test]
    fn test_negative_spread_uses_default() {
        // Verify the negative spread guard logic: negative values should be clamped to 0.10
        let raw_spread = -0.36;
        let spread = if raw_spread < 0.0 { 0.10 } else { raw_spread };
        assert_eq!(
            spread, 0.10,
            "negative spread should be replaced with 0.10 default"
        );

        // Zero and positive should pass through
        let raw_spread = 0.05;
        let spread = if raw_spread < 0.0 { 0.10 } else { raw_spread };
        assert_eq!(spread, 0.05, "positive spread should pass through");

        let raw_spread = 0.0;
        let spread = if raw_spread < 0.0 { 0.10 } else { raw_spread };
        assert_eq!(spread, 0.0, "zero spread should pass through");
    }

    #[test]
    fn test_risk_reward_guard_logic() {
        let max_ratio = 5.0; // default config value

        // YES direction: fill_price=0.85 → win=0.15, lose=0.85 → ratio=5.67 > 5.0 → REJECT
        let fill_price = 0.85;
        let (win, lose) = (1.0 - fill_price, fill_price);
        assert!(
            lose > max_ratio * win,
            "0.85 YES fill should fail risk/reward: win={win}, lose={lose}"
        );

        // YES direction: fill_price=0.82 → win=0.18, lose=0.82 → ratio=4.56 < 5.0 → PASS
        let fill_price = 0.82;
        let (win, lose) = (1.0 - fill_price, fill_price);
        assert!(
            !(lose > max_ratio * win),
            "0.82 YES fill should pass risk/reward with 5.0 ratio: win={win}, lose={lose}"
        );

        // NO direction: fill_price=0.15 → win=0.15, lose=0.85 → ratio=5.67 > 5.0 → REJECT
        let fill_price = 0.15;
        let (win, lose) = (fill_price, 1.0 - fill_price);
        assert!(
            lose > max_ratio * win,
            "0.15 NO fill should fail risk/reward: win={win}, lose={lose}"
        );

        // YES direction: fill_price=0.50 → win=0.50, lose=0.50 → ratio=1.0 → PASS
        let fill_price = 0.50;
        let (win, lose) = (1.0 - fill_price, fill_price);
        assert!(!(lose > max_ratio * win), "0.50 YES fill should pass risk/reward");
    }
}

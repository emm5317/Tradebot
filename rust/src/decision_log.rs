//! Decision audit log — fire-and-forget DB writes for every evaluation.
//!
//! Called from crypto_evaluator at each rejection/signal point.
//! Uses the same sqlx pattern as persist_signal: non-blocking, logs warn on error.

use sqlx::PgPool;
use tracing::warn;

#[derive(Debug, Default)]
pub struct DecisionEntry {
    pub ticker: String,
    pub signal_type: String,
    pub outcome: String,
    pub rejection_reason: Option<String>,
    pub model_prob: Option<f64>,
    pub market_price: Option<f64>,
    pub edge: Option<f64>,
    pub adjusted_edge: Option<f64>,
    pub direction: Option<String>,
    pub minutes_remaining: Option<f64>,
    pub confidence: Option<f64>,
    pub micro_total: Option<f64>,
    pub micro_trade: Option<f64>,
    pub micro_spread: Option<f64>,
    pub micro_depth: Option<f64>,
    pub micro_vwap: Option<f64>,
    pub micro_momentum: Option<f64>,
    pub micro_vol_surge: Option<f64>,
    pub signal_id: Option<i64>,
    pub eval_latency_ms: Option<f64>,
}

pub async fn write(pool: &PgPool, entry: DecisionEntry) {
    let result = sqlx::query(
        r#"
        INSERT INTO decision_log (
            ticker, signal_type, source, outcome, rejection_reason,
            model_prob, market_price, edge, adjusted_edge, direction,
            minutes_remaining, confidence,
            micro_total, micro_trade, micro_spread, micro_depth,
            micro_vwap, micro_momentum, micro_vol_surge,
            signal_id, eval_latency_ms
        ) VALUES (
            $1, $2, 'rust', $3, $4,
            $5, $6, $7, $8, $9,
            $10, $11,
            $12, $13, $14, $15,
            $16, $17, $18,
            $19, $20
        )
        "#,
    )
    .bind(&entry.ticker)
    .bind(&entry.signal_type)
    .bind(&entry.outcome)
    .bind(&entry.rejection_reason)
    .bind(entry.model_prob.map(|v| v as f32))
    .bind(entry.market_price.map(|v| v as f32))
    .bind(entry.edge.map(|v| v as f32))
    .bind(entry.adjusted_edge.map(|v| v as f32))
    .bind(&entry.direction)
    .bind(entry.minutes_remaining.map(|v| v as f32))
    .bind(entry.confidence.map(|v| v as f32))
    .bind(entry.micro_total.map(|v| v as f32))
    .bind(entry.micro_trade.map(|v| v as f32))
    .bind(entry.micro_spread.map(|v| v as f32))
    .bind(entry.micro_depth.map(|v| v as f32))
    .bind(entry.micro_vwap.map(|v| v as f32))
    .bind(entry.micro_momentum.map(|v| v as f32))
    .bind(entry.micro_vol_surge.map(|v| v as f32))
    .bind(entry.signal_id)
    .bind(entry.eval_latency_ms.map(|v| v as f32))
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(error = %e, ticker = %entry.ticker, "decision_log write failed");
    }
}

/// Write a feed health snapshot to feed_health_log.
pub async fn write_feed_health(
    pool: &PgPool,
    feed_name: &str,
    score: f64,
    last_msg_age_ms: Option<f64>,
    is_stale: bool,
) {
    let result = sqlx::query(
        "INSERT INTO feed_health_log (feed_name, score, last_msg_age_ms, is_stale) VALUES ($1, $2, $3, $4)",
    )
    .bind(feed_name)
    .bind(score as f32)
    .bind(last_msg_age_ms.map(|v| v as f32))
    .bind(is_stale)
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(error = %e, feed = feed_name, "feed_health_log write failed");
    }
}

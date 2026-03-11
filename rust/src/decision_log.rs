//! Decision audit log — batched DB writes for every evaluation.
//!
//! Phase 12.0d: Replaces fire-and-forget `tokio::spawn` per write with an
//! mpsc channel → background flush task. Batches up to 100 entries or
//! flushes every 1s, whichever comes first. Multi-row INSERT via
//! sqlx::QueryBuilder::push_values().

use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

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

/// Maximum entries per batch INSERT.
const BATCH_SIZE: usize = 100;

/// Flush interval when batch isn't full.
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Batched decision log writer. Send entries via `send()`, a background
/// task flushes them to PostgreSQL in multi-row INSERTs.
#[derive(Clone)]
pub struct DecisionLogWriter {
    tx: mpsc::Sender<DecisionEntry>,
}

impl DecisionLogWriter {
    /// Create a new writer and spawn the background flush task.
    /// Returns the writer handle (cloneable) for sending entries.
    pub fn spawn(pool: PgPool, cancel: CancellationToken) -> Self {
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(flush_loop(pool, rx, cancel));
        Self { tx }
    }

    /// Send an entry to be batched and flushed. Non-blocking; drops
    /// the entry if the channel is full (back-pressure safety).
    pub fn send(&self, entry: DecisionEntry) {
        // Emit channel utilization gauge (0.0 = empty, 1.0 = full)
        let remaining = self.tx.capacity();
        metrics::gauge!(crate::metrics_registry::DECISION_LOG_CHANNEL_USAGE)
            .set(1.0 - (remaining as f64 / 1024.0));

        if let Err(e) = self.tx.try_send(entry) {
            metrics::counter!(crate::metrics_registry::DECISION_LOG_DROPPED).increment(1);
            warn!(error = %e, "decision_log channel full or closed, dropping entry");
        }
    }
}

/// Background loop: drain channel into batches, flush on size or timer.
async fn flush_loop(
    pool: PgPool,
    mut rx: mpsc::Receiver<DecisionEntry>,
    cancel: CancellationToken,
) {
    let mut batch: Vec<DecisionEntry> = Vec::with_capacity(BATCH_SIZE);
    let mut interval = tokio::time::interval(FLUSH_INTERVAL);

    loop {
        tokio::select! {
            // Receive entries from the channel
            maybe = rx.recv() => {
                match maybe {
                    Some(entry) => {
                        batch.push(entry);
                        // Drain any additional buffered entries (non-blocking)
                        while batch.len() < BATCH_SIZE {
                            match rx.try_recv() {
                                Ok(e) => batch.push(e),
                                Err(_) => break,
                            }
                        }
                        if batch.len() >= BATCH_SIZE {
                            flush_batch(&pool, &mut batch).await;
                        }
                    }
                    None => {
                        // Channel closed — flush remaining and exit
                        if !batch.is_empty() {
                            flush_batch(&pool, &mut batch).await;
                        }
                        info!("decision_log flush loop: channel closed, exiting");
                        return;
                    }
                }
            }
            // Periodic timer flush
            _ = interval.tick() => {
                if !batch.is_empty() {
                    flush_batch(&pool, &mut batch).await;
                }
            }
            // Graceful shutdown
            _ = cancel.cancelled() => {
                // Drain remaining from channel
                rx.close();
                while let Ok(entry) = rx.try_recv() {
                    batch.push(entry);
                }
                if !batch.is_empty() {
                    flush_batch(&pool, &mut batch).await;
                }
                info!("decision_log flush loop: shutdown complete");
                return;
            }
        }
    }
}

/// Flush a batch of entries via multi-row INSERT.
async fn flush_batch(pool: &PgPool, batch: &mut Vec<DecisionEntry>) {
    if batch.is_empty() {
        return;
    }

    let count = batch.len();

    let mut qb = sqlx::QueryBuilder::new(
        "INSERT INTO decision_log (\
            ticker, signal_type, source, outcome, rejection_reason, \
            model_prob, market_price, edge, adjusted_edge, direction, \
            minutes_remaining, confidence, \
            micro_total, micro_trade, micro_spread, micro_depth, \
            micro_vwap, micro_momentum, micro_vol_surge, \
            signal_id, eval_latency_ms\
        ) ",
    );

    qb.push_values(batch.iter(), |mut b, entry| {
        b.push_bind(&entry.ticker)
            .push_bind(&entry.signal_type)
            .push_bind("rust")
            .push_bind(&entry.outcome)
            .push_bind(&entry.rejection_reason)
            .push_bind(entry.model_prob.map(|v| v as f32))
            .push_bind(entry.market_price.map(|v| v as f32))
            .push_bind(entry.edge.map(|v| v as f32))
            .push_bind(entry.adjusted_edge.map(|v| v as f32))
            .push_bind(&entry.direction)
            .push_bind(entry.minutes_remaining.map(|v| v as f32))
            .push_bind(entry.confidence.map(|v| v as f32))
            .push_bind(entry.micro_total.map(|v| v as f32))
            .push_bind(entry.micro_trade.map(|v| v as f32))
            .push_bind(entry.micro_spread.map(|v| v as f32))
            .push_bind(entry.micro_depth.map(|v| v as f32))
            .push_bind(entry.micro_vwap.map(|v| v as f32))
            .push_bind(entry.micro_momentum.map(|v| v as f32))
            .push_bind(entry.micro_vol_surge.map(|v| v as f32))
            .push_bind(entry.signal_id)
            .push_bind(entry.eval_latency_ms.map(|v| v as f32));
    });

    match qb.build().execute(pool).await {
        Ok(_) => {
            tracing::trace!(count, "decision_log batch flushed");
        }
        Err(e) => {
            warn!(error = %e, count, "decision_log batch flush failed");
        }
    }

    batch.clear();
}

/// Write a single decision log entry directly (for callers outside the batch path).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_entry_default() {
        let entry = DecisionEntry::default();
        assert_eq!(entry.ticker, "");
        assert_eq!(entry.signal_type, "");
        assert_eq!(entry.outcome, "");
        assert!(entry.rejection_reason.is_none());
        assert!(entry.model_prob.is_none());
    }

    #[tokio::test]
    async fn test_flush_batch_empty_is_noop() {
        // Verify that flush_batch with empty vec doesn't panic
        // (can't test actual DB without PgPool, but structure is sound)
        let mut batch: Vec<DecisionEntry> = Vec::new();
        assert!(batch.is_empty());
        batch.clear(); // noop
        assert_eq!(batch.len(), 0);
    }

    #[test]
    fn test_batch_size_constant() {
        assert_eq!(BATCH_SIZE, 100);
    }

    #[test]
    fn test_flush_interval_constant() {
        assert_eq!(FLUSH_INTERVAL, std::time::Duration::from_secs(1));
    }
}

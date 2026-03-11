//! Prometheus metrics registry — global recorder + metric name constants.
//!
//! Phase 12.1: Initializes the metrics-exporter-prometheus recorder and
//! exposes a PrometheusHandle for the /metrics HTTP endpoint.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

// ── Counters ────────────────────────────────────────────────

/// Total evaluations (labels: signal_type, outcome)
pub const EVAL_TOTAL: &str = "tradebot_eval_total";

/// Total orders submitted (labels: signal_type, action, result)
pub const ORDERS_TOTAL: &str = "tradebot_orders_total";

/// Decision log entries dropped due to channel backpressure
pub const DECISION_LOG_DROPPED: &str = "tradebot_decision_log_dropped_total";

// ── Histograms ──────────────────────────────────────────────

/// Evaluation duration in seconds (labels: signal_type)
pub const EVAL_DURATION: &str = "tradebot_eval_duration_seconds";

/// Order submission latency in seconds (labels: signal_type)
pub const ORDER_LATENCY: &str = "tradebot_order_latency_seconds";

// ── Gauges ──────────────────────────────────────────────────

/// Per-feed health score 0.0–1.0 (labels: feed)
pub const FEED_HEALTH_SCORE: &str = "tradebot_feed_health_score";

/// Decision log channel utilization 0.0–1.0
pub const DECISION_LOG_CHANNEL_USAGE: &str = "tradebot_decision_log_channel_usage";

/// Number of supervised tasks currently active
pub const SUPERVISOR_TASKS_ACTIVE: &str = "tradebot_supervisor_tasks_active";

/// Open position count
pub const POSITIONS_OPEN: &str = "tradebot_positions_open";

/// Kill switch state (labels: switch)
pub const KILL_SWITCH_ACTIVE: &str = "tradebot_kill_switch_active";

/// Install the global Prometheus recorder and return the handle for /metrics.
pub fn init() -> PrometheusHandle {
    let builder = PrometheusBuilder::new();
    builder
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_names_are_valid() {
        // Prometheus metric names must match [a-zA-Z_:][a-zA-Z0-9_:]*
        let names = [
            EVAL_TOTAL,
            ORDERS_TOTAL,
            DECISION_LOG_DROPPED,
            EVAL_DURATION,
            ORDER_LATENCY,
            FEED_HEALTH_SCORE,
            DECISION_LOG_CHANNEL_USAGE,
            SUPERVISOR_TASKS_ACTIVE,
            POSITIONS_OPEN,
            KILL_SWITCH_ACTIVE,
        ];
        for name in &names {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':'),
                "invalid metric name: {name}"
            );
            assert!(
                name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == ':'),
                "metric name must start with letter/underscore: {name}"
            );
        }
    }
}

//! Health and metrics endpoints — /health/live, /health/ready, /metrics.
//!
//! Phase 12.1: Structured health checks for container orchestration
//! and Prometheus metrics export.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::Serialize;

use crate::feed_health::FeedHealth;

/// Shared state for health endpoints.
#[derive(Clone)]
pub struct HealthState {
    pub pool: sqlx::PgPool,
    pub redis: fred::clients::Client,
    pub nats: async_nats::Client,
    pub feed_health: Arc<FeedHealth>,
    pub prometheus_handle: PrometheusHandle,
}

/// Build the health + metrics router.
pub fn routes(state: HealthState) -> Router {
    Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/metrics", get(metrics_endpoint))
        .with_state(state)
}

/// Liveness probe — if this responds, the process is alive.
async fn health_live() -> StatusCode {
    StatusCode::OK
}

#[derive(Serialize)]
struct ReadinessResult {
    ready: bool,
    db: bool,
    redis: bool,
    nats: bool,
    feeds: bool,
}

/// Readiness probe — checks all critical dependencies.
async fn health_ready(State(st): State<HealthState>) -> (StatusCode, Json<ReadinessResult>) {
    let db_ok = sqlx::query("SELECT 1")
        .fetch_one(&st.pool)
        .await
        .is_ok();

    let redis_ok = fred::interfaces::ClientLike::ping::<String>(&st.redis, None)
        .await
        .is_ok();

    let nats_ok = st.nats.connection_state() == async_nats::connection::State::Connected;

    let feeds_ok = st.feed_health.system_health() > 0.0;

    let ready = db_ok && redis_ok && nats_ok && feeds_ok;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(ReadinessResult {
            ready,
            db: db_ok,
            redis: redis_ok,
            nats: nats_ok,
            feeds: feeds_ok,
        }),
    )
}

/// Prometheus metrics scrape endpoint.
async fn metrics_endpoint(State(st): State<HealthState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        st.prometheus_handle.render(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_readiness_result_serializes() {
        let result = ReadinessResult {
            ready: true,
            db: true,
            redis: true,
            nats: true,
            feeds: false,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"ready\":true"));
        assert!(json.contains("\"feeds\":false"));
    }
}

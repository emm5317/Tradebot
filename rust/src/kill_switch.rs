//! Kill switch system for halting trading without stopping the process.
//!
//! Provides global and per-strategy kill switches accessible via HTTP endpoints.
//! State is backed by `AtomicBool` for lock-free reads from the execution hot path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Shared kill switch state. Lock-free reads via AtomicBool.
pub struct KillSwitchState {
    pub kill_all: AtomicBool,
    pub kill_crypto: AtomicBool,
    pub kill_weather: AtomicBool,
}

impl KillSwitchState {
    /// Create kill switch state from config (env var defaults).
    pub fn new(kill_all: bool, kill_crypto: bool, kill_weather: bool) -> Self {
        Self {
            kill_all: AtomicBool::new(kill_all),
            kill_crypto: AtomicBool::new(kill_crypto),
            kill_weather: AtomicBool::new(kill_weather),
        }
    }

    /// Check if trading is blocked for a given signal type.
    pub fn is_blocked(&self, signal_type: &str) -> bool {
        if self.kill_all.load(Ordering::Relaxed) {
            return true;
        }
        match signal_type {
            "crypto" => self.kill_crypto.load(Ordering::Relaxed),
            "weather" => self.kill_weather.load(Ordering::Relaxed),
            _ => false,
        }
    }

    /// Get current state as a serializable snapshot.
    fn snapshot(&self) -> KillSwitchSnapshot {
        KillSwitchSnapshot {
            kill_all: self.kill_all.load(Ordering::Relaxed),
            kill_crypto: self.kill_crypto.load(Ordering::Relaxed),
            kill_weather: self.kill_weather.load(Ordering::Relaxed),
        }
    }
}

#[derive(Serialize)]
struct KillSwitchSnapshot {
    kill_all: bool,
    kill_crypto: bool,
    kill_weather: bool,
}

#[derive(Deserialize)]
struct KillSwitchToggle {
    switch: String,
    active: bool,
}

/// Build the Axum router for kill switch and health endpoints.
pub fn router(state: Arc<KillSwitchState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/kill-switch", get(get_kill_switch))
        .route("/kill-switch", post(post_kill_switch))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn get_kill_switch(State(state): State<Arc<KillSwitchState>>) -> Json<KillSwitchSnapshot> {
    Json(state.snapshot())
}

async fn post_kill_switch(
    State(state): State<Arc<KillSwitchState>>,
    Json(toggle): Json<KillSwitchToggle>,
) -> Result<Json<KillSwitchSnapshot>, StatusCode> {
    let switch_ref = match toggle.switch.as_str() {
        "all" => &state.kill_all,
        "crypto" => &state.kill_crypto,
        "weather" => &state.kill_weather,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let previous = switch_ref.swap(toggle.active, Ordering::Relaxed);
    if previous != toggle.active {
        if toggle.active {
            warn!(switch = %toggle.switch, "kill switch ACTIVATED — trading halted");
        } else {
            info!(switch = %toggle.switch, "kill switch deactivated — trading resumed");
        }
    }

    Ok(Json(state.snapshot()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kill_all_blocks_everything() {
        let state = KillSwitchState::new(true, false, false);
        assert!(state.is_blocked("crypto"));
        assert!(state.is_blocked("weather"));
        assert!(state.is_blocked("unknown"));
    }

    #[test]
    fn test_kill_crypto_only() {
        let state = KillSwitchState::new(false, true, false);
        assert!(state.is_blocked("crypto"));
        assert!(!state.is_blocked("weather"));
    }

    #[test]
    fn test_kill_weather_only() {
        let state = KillSwitchState::new(false, false, true);
        assert!(!state.is_blocked("crypto"));
        assert!(state.is_blocked("weather"));
    }

    #[test]
    fn test_default_state_allows_all() {
        let state = KillSwitchState::new(false, false, false);
        assert!(!state.is_blocked("crypto"));
        assert!(!state.is_blocked("weather"));
    }

    #[test]
    fn test_toggle_switch() {
        let state = KillSwitchState::new(false, false, false);
        assert!(!state.is_blocked("crypto"));

        state.kill_crypto.store(true, Ordering::Relaxed);
        assert!(state.is_blocked("crypto"));

        state.kill_crypto.store(false, Ordering::Relaxed);
        assert!(!state.is_blocked("crypto"));
    }
}

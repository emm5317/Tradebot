//! Shared types used across multiple modules.
//!
//! Phase 3: Signal struct + SignalPriority for event-driven evaluation.

use serde::{Deserialize, Serialize};

/// Signal priority levels for cooldown bypass logic.
/// Higher priority signals can bypass cooldown in certain conditions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalPriority {
    /// Timer-based evaluation (Python polling loop).
    #[default]
    Timer = 0,
    /// Re-evaluation of existing position.
    Reeval = 1,
    /// New exchange data triggered evaluation.
    NewData = 2,
    /// Lock detection (highest priority).
    LockDetection = 3,
}

/// Signal schema matching the Python SignalSchema.
/// Shared between execution engine, crypto evaluator, and order manager.
#[derive(Debug, Serialize, Deserialize)]
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
    #[serde(default)]
    pub priority: SignalPriority,
    /// Model confidence (0.0-1.0). Defaults to 0.5 for backward compat.
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

fn default_confidence() -> f64 {
    0.5
}

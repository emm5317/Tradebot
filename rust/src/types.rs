//! Shared types used across multiple modules.
//!
//! Phase 3: Signal struct + SignalPriority for event-driven evaluation.

use serde::{Deserialize, Serialize};

/// Signal priority levels for cooldown bypass logic.
/// Higher priority signals can bypass cooldown in certain conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalPriority {
    /// Timer-based evaluation (Python polling loop).
    Timer = 0,
    /// Re-evaluation of existing position.
    Reeval = 1,
    /// New exchange data triggered evaluation.
    NewData = 2,
    /// Lock detection (highest priority).
    LockDetection = 3,
}

impl Default for SignalPriority {
    fn default() -> Self {
        Self::Timer
    }
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
}

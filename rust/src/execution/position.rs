use std::collections::HashMap;

/// Tracks current positions per market.
pub struct PositionTracker {
    positions: HashMap<String, i64>, // ticker -> net contracts (positive = yes, negative = no)
}

impl PositionTracker {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
        }
    }

    pub fn update(&mut self, ticker: &str, delta: i64) {
        *self.positions.entry(ticker.to_string()).or_insert(0) += delta;
    }

    pub fn get(&self, ticker: &str) -> i64 {
        self.positions.get(ticker).copied().unwrap_or(0)
    }

    pub fn all(&self) -> &HashMap<String, i64> {
        &self.positions
    }
}

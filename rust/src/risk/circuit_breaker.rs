use std::time::Instant;

/// Halts trading when error rate or drawdown exceeds thresholds.
pub struct CircuitBreaker {
    tripped: bool,
    tripped_at: Option<Instant>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self {
            tripped: false,
            tripped_at: None,
        }
    }

    pub fn trip(&mut self) {
        self.tripped = true;
        self.tripped_at = Some(Instant::now());
    }

    pub fn reset(&mut self) {
        self.tripped = false;
        self.tripped_at = None;
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped
    }
}

use super::circuit_breaker::CircuitBreaker;

/// Central risk manager – gates every order.
pub struct RiskManager {
    pub max_position: u32,
    pub daily_loss_limit: f64,
    pub circuit_breaker: CircuitBreaker,
    daily_pnl: f64,
}

impl RiskManager {
    pub fn new(max_position: u32, daily_loss_limit: f64) -> Self {
        Self {
            max_position,
            daily_loss_limit,
            circuit_breaker: CircuitBreaker::new(),
            daily_pnl: 0.0,
        }
    }

    pub fn allow_order(&self, _ticker: &str, _quantity: u32) -> bool {
        if self.circuit_breaker.is_tripped() {
            return false;
        }
        if self.daily_pnl <= -self.daily_loss_limit {
            return false;
        }
        // TODO: position-level checks
        true
    }

    pub fn record_pnl(&mut self, pnl: f64) {
        self.daily_pnl += pnl;
    }
}

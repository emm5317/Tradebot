use crate::kalshi::types::{Order, Side};

/// Builds and submits orders to Kalshi.
pub struct OrderManager {
    // TODO: reference to KalshiClient, pending orders
}

impl OrderManager {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn submit_order(
        &self,
        _ticker: &str,
        _side: Side,
        _price: f64,
        _quantity: u32,
    ) -> Result<Order, Box<dyn std::error::Error>> {
        // TODO: validate via risk manager, then send to Kalshi
        todo!()
    }

    pub async fn cancel_order(&self, _order_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: cancel via API
        todo!()
    }
}

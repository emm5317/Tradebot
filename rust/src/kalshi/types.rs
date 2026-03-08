use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Market metadata from Kalshi API.
#[derive(Debug, Clone, Deserialize)]
pub struct Market {
    pub ticker: String,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub status: String,
    pub category: Option<String>,
    pub yes_ask: Option<f64>,
    pub yes_bid: Option<f64>,
    pub no_ask: Option<f64>,
    pub no_bid: Option<f64>,
    pub last_price: Option<f64>,
    pub volume: Option<i64>,
    pub open_interest: Option<i64>,
    pub close_time: Option<DateTime<Utc>>,
    pub expiration_time: Option<DateTime<Utc>>,
    pub result: Option<String>,
}

/// Account balance.
#[derive(Debug, Clone, Deserialize)]
pub struct Balance {
    pub balance: i64,
}

/// Request to place an order.
#[derive(Debug, Clone, Serialize)]
pub struct OrderRequest {
    pub ticker: String,
    pub action: String,       // "buy" or "sell"
    pub side: String,         // "yes" or "no"
    pub r#type: String,       // "market" or "limit"
    pub count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yes_price: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_price: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration_time: Option<String>,
}

/// Request to place multiple orders in a batch.
#[derive(Debug, Clone, Serialize)]
pub struct BatchOrderRequest {
    pub orders: Vec<OrderRequest>,
}

/// Response from batch order placement.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchOrderResponse {
    pub orders: Vec<Order>,
}

/// Fill event from WebSocket.
#[derive(Debug, Clone, Deserialize)]
pub struct FillEvent {
    pub trade_id: Option<String>,
    pub order_id: Option<String>,
    pub market_ticker: String,
    pub side: String,
    pub yes_price: Option<i64>,
    pub no_price: Option<i64>,
    pub count: Option<i64>,
    pub action: Option<String>,
    pub is_taker: Option<bool>,
}

/// Response from placing an order.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    pub order: Order,
}

/// An order record.
#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub ticker: String,
    pub status: String,
    pub action: String,
    pub side: String,
    pub r#type: String,
    pub yes_price: Option<i64>,
    pub no_price: Option<i64>,
    pub count: Option<i64>,
    pub remaining_count: Option<i64>,
    pub created_time: Option<DateTime<Utc>>,
}

/// Cancel response.
#[derive(Debug, Clone, Deserialize)]
pub struct CancelResponse {
    pub order: Order,
}

/// A position held.
#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    pub ticker: String,
    pub market_exposure: Option<i64>,
    pub resting_orders_count: Option<i64>,
    pub total_traded: Option<i64>,
    pub realized_pnl: Option<i64>,
}

/// Settlement record.
#[derive(Debug, Clone, Deserialize)]
pub struct Settlement {
    pub ticker: String,
    pub result: Option<String>,
    pub settled_time: Option<DateTime<Utc>>,
}

/// Wrapper types for paginated API responses.
#[derive(Debug, Deserialize)]
pub struct MarketsResponse {
    pub markets: Vec<Market>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MarketResponse {
    pub market: Market,
}

#[derive(Debug, Deserialize)]
pub struct BalanceResponse {
    pub balance: i64,
}

#[derive(Debug, Deserialize)]
pub struct PositionsResponse {
    pub market_positions: Vec<Position>,
}

#[derive(Debug, Deserialize)]
pub struct OrdersResponse {
    pub orders: Vec<Order>,
}

#[derive(Debug, Deserialize)]
pub struct SettlementsResponse {
    pub settlements: Vec<Settlement>,
}

/// Query params for fetching orders.
#[derive(Debug, Default)]
pub struct OrderQueryParams {
    pub ticker: Option<String>,
    pub status: Option<String>,
}

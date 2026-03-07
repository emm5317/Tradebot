use axum::{extract::State, response::Json, routing::get, Router};
use super::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/positions", get(get_positions))
        .route("/api/orders", get(get_orders))
        .route("/api/pnl", get(get_pnl))
        .with_state(state)
}

async fn get_positions(State(_state): State<AppState>) -> Json<serde_json::Value> {
    // TODO: return current positions
    Json(serde_json::json!({ "positions": [] }))
}

async fn get_orders(State(_state): State<AppState>) -> Json<serde_json::Value> {
    // TODO: return open orders
    Json(serde_json::json!({ "orders": [] }))
}

async fn get_pnl(State(_state): State<AppState>) -> Json<serde_json::Value> {
    // TODO: return P&L summary
    Json(serde_json::json!({ "daily_pnl": 0.0 }))
}

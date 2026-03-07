use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use tracing::{info, warn};

use crate::kalshi::auth::KalshiAuth;
use crate::kalshi::error::KalshiError;
use crate::kalshi::types::*;

/// Kalshi REST API client with per-request RSA signing and automatic retry.
pub struct KalshiClient {
    http: reqwest::Client,
    auth: KalshiAuth,
    base_url: String,
}

impl KalshiClient {
    pub fn new(auth: KalshiAuth, base_url: String) -> Result<Self, KalshiError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(4)
            .build()
            .map_err(KalshiError::NetworkError)?;

        Ok(Self {
            http,
            auth,
            base_url,
        })
    }

    /// Build auth headers for a request.
    fn auth_headers(&self, method: &str, path: &str) -> Result<HeaderMap, KalshiError> {
        let ah = self.auth.sign_request(method, path)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "KALSHI-ACCESS-KEY",
            HeaderValue::from_str(&ah.api_key).unwrap(),
        );
        headers.insert(
            "KALSHI-ACCESS-SIGNATURE",
            HeaderValue::from_str(&ah.signature).unwrap(),
        );
        headers.insert(
            "KALSHI-ACCESS-TIMESTAMP",
            HeaderValue::from_str(&ah.timestamp).unwrap(),
        );
        Ok(headers)
    }

    /// Execute a GET request with auth, retry on 5xx up to 3 times.
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, KalshiError> {
        let url = format!("{}{}", self.base_url, path);
        let mut last_err = None;

        for attempt in 0..3 {
            let headers = self.auth_headers("GET", path)?;
            let start = std::time::Instant::now();

            match self.http.get(&url).headers(headers).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let latency = start.elapsed();
                    info!(path, status = status.as_u16(), latency_ms = %latency.as_millis(), "kalshi GET");

                    if status.is_success() {
                        return resp.json::<T>().await.map_err(KalshiError::NetworkError);
                    }

                    let body = resp.text().await.unwrap_or_else(|e| format!("<read error: {e}>"));
                    let err = parse_error_response(status.as_u16(), &body);
                    if status.is_server_error() && attempt < 2 {
                        let delay = Duration::from_secs(1 << attempt);
                        warn!(attempt, ?delay, "kalshi 5xx, retrying");
                        tokio::time::sleep(delay).await;
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
                Err(e) => {
                    if attempt < 2 {
                        let delay = Duration::from_secs(1 << attempt);
                        warn!(attempt, ?delay, error = %e, "kalshi request failed, retrying");
                        tokio::time::sleep(delay).await;
                        last_err = Some(KalshiError::NetworkError(e));
                        continue;
                    }
                    return Err(KalshiError::NetworkError(e));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| KalshiError::Other("max retries exceeded".into())))
    }

    /// Execute a POST request with auth and JSON body.
    async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, KalshiError> {
        let url = format!("{}{}", self.base_url, path);
        let mut last_err = None;

        for attempt in 0..3 {
            let headers = self.auth_headers("POST", path)?;
            let start = std::time::Instant::now();

            match self.http.post(&url).headers(headers).json(body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let latency = start.elapsed();
                    info!(path, status = status.as_u16(), latency_ms = %latency.as_millis(), "kalshi POST");

                    if status.is_success() {
                        return resp.json::<T>().await.map_err(KalshiError::NetworkError);
                    }

                    let body = resp.text().await.unwrap_or_else(|e| format!("<read error: {e}>"));
                    let err = parse_error_response(status.as_u16(), &body);
                    if status.is_server_error() && attempt < 2 {
                        let delay = Duration::from_secs(1 << attempt);
                        warn!(attempt, ?delay, "kalshi 5xx, retrying");
                        tokio::time::sleep(delay).await;
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
                Err(e) => {
                    if attempt < 2 {
                        let delay = Duration::from_secs(1 << attempt);
                        warn!(attempt, ?delay, error = %e, "kalshi POST failed, retrying");
                        tokio::time::sleep(delay).await;
                        last_err = Some(KalshiError::NetworkError(e));
                        continue;
                    }
                    return Err(KalshiError::NetworkError(e));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| KalshiError::Other("max retries exceeded".into())))
    }

    /// Execute a DELETE request with auth.
    async fn delete<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, KalshiError> {
        let url = format!("{}{}", self.base_url, path);
        let headers = self.auth_headers("DELETE", path)?;
        let start = std::time::Instant::now();

        let resp = self.http.delete(&url).headers(headers).send().await
            .map_err(KalshiError::NetworkError)?;

        let status = resp.status();
        let latency = start.elapsed();
        info!(path, status = status.as_u16(), latency_ms = %latency.as_millis(), "kalshi DELETE");

        if status.is_success() {
            return resp.json::<T>().await.map_err(KalshiError::NetworkError);
        }

        let body = resp.text().await.unwrap_or_else(|e| format!("<read error: {e}>"));
        Err(parse_error_response(status.as_u16(), &body))
    }

    // --- Public API methods ---

    /// Fetch markets with optional status and category filters.
    pub async fn get_markets(
        &self,
        status: &str,
        category: Option<&str>,
    ) -> Result<Vec<Market>, KalshiError> {
        let mut path = format!(
            "/trade-api/v2/markets?status={}",
            urlencoding::encode(status)
        );
        if let Some(cat) = category {
            path.push_str(&format!("&category={}", urlencoding::encode(cat)));
        }
        let resp: MarketsResponse = self.get(&path).await?;
        Ok(resp.markets)
    }

    /// Fetch a single market by ticker.
    pub async fn get_market(&self, ticker: &str) -> Result<Market, KalshiError> {
        let path = format!(
            "/trade-api/v2/markets/{}",
            urlencoding::encode(ticker)
        );
        let resp: MarketResponse = self.get(&path).await?;
        Ok(resp.market)
    }

    /// Get account balance.
    pub async fn get_balance(&self) -> Result<Balance, KalshiError> {
        let resp: BalanceResponse = self.get("/trade-api/v2/portfolio/balance").await?;
        Ok(Balance {
            balance: resp.balance,
        })
    }

    /// Place an order.
    pub async fn place_order(&self, req: OrderRequest) -> Result<OrderResponse, KalshiError> {
        self.post("/trade-api/v2/portfolio/orders", &req).await
    }

    /// Cancel an order by ID.
    pub async fn cancel_order(&self, order_id: &str) -> Result<CancelResponse, KalshiError> {
        let path = format!(
            "/trade-api/v2/portfolio/orders/{}",
            urlencoding::encode(order_id)
        );
        self.delete(&path).await
    }

    /// Get all open positions.
    pub async fn get_positions(&self) -> Result<Vec<Position>, KalshiError> {
        let resp: PositionsResponse =
            self.get("/trade-api/v2/portfolio/positions").await?;
        Ok(resp.market_positions)
    }

    /// Get orders with optional filters.
    pub async fn get_orders(&self, params: OrderQueryParams) -> Result<Vec<Order>, KalshiError> {
        let mut path = "/trade-api/v2/portfolio/orders?".to_string();
        if let Some(ticker) = &params.ticker {
            path.push_str(&format!("ticker={}&", urlencoding::encode(ticker)));
        }
        if let Some(status) = &params.status {
            path.push_str(&format!("status={}&", urlencoding::encode(status)));
        }
        let resp: OrdersResponse = self.get(&path).await?;
        Ok(resp.orders)
    }

    /// Get settlements since a given time.
    pub async fn get_settlements(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Settlement>, KalshiError> {
        let path = format!(
            "/trade-api/v2/markets/settlements?min_close_ts={}",
            since.timestamp()
        );
        let resp: SettlementsResponse = self.get(&path).await?;
        Ok(resp.settlements)
    }
}

/// Parse Kalshi error response body into typed error.
fn parse_error_response(status: u16, body: &str) -> KalshiError {
    match status {
        401 | 403 => KalshiError::AuthFailure,
        429 => {
            // Try to parse retry-after from body
            let retry_after = Duration::from_secs(1);
            KalshiError::RateLimit { retry_after }
        }
        400 => {
            if body.contains("insufficient") || body.contains("balance") {
                KalshiError::InsufficientFunds
            } else if body.contains("closed") {
                KalshiError::MarketClosed
            } else {
                KalshiError::InvalidOrder {
                    reason: body.to_string(),
                }
            }
        }
        500..=599 => KalshiError::ServerError(status),
        _ => KalshiError::Other(format!("HTTP {status}: {body}")),
    }
}

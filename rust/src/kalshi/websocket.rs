use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{Instant, interval};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::kalshi::auth::KalshiAuth;

/// Messages emitted by the WebSocket feed to downstream consumers.
#[derive(Debug, Clone)]
pub enum WsFeedMessage {
    /// Orderbook snapshot for a ticker.
    OrderbookSnapshot {
        ticker: String,
        yes_bids: Vec<(i64, i64)>, // (price_cents, size)
        yes_asks: Vec<(i64, i64)>,
    },
    /// Incremental orderbook delta.
    OrderbookDelta {
        ticker: String,
        side: String,      // "yes" or "no"
        price_cents: i64,
        delta: i64,        // positive = add, negative = remove
    },
    /// Trade occurred.
    Trade {
        ticker: String,
        price_cents: i64,
        count: i64,
        taker_side: String,
    },
    /// Feed disconnected (reconnecting).
    Disconnected,
    /// Feed reconnected.
    Reconnected,
}

/// Kalshi WebSocket subscription command.
#[derive(Debug, Serialize)]
struct SubscribeCmd {
    id: u64,
    cmd: String,
    params: SubscribeParams,
}

#[derive(Debug, Serialize)]
struct SubscribeParams {
    channels: Vec<String>,
    market_tickers: Vec<String>,
}

/// Raw WebSocket message from Kalshi (loosely typed for initial parsing).
#[derive(Debug, Deserialize)]
struct WsRawMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    channel: Option<String>,
    msg: Option<serde_json::Value>,
    sid: Option<u64>,
}

/// Manages the persistent WebSocket connection to Kalshi.
pub struct KalshiWsFeed {
    ws_url: String,
    auth: KalshiAuth,
    subscriptions: Arc<tokio::sync::Mutex<HashSet<String>>>,
}

impl KalshiWsFeed {
    pub fn new(ws_url: String, auth: KalshiAuth) -> Self {
        Self {
            ws_url,
            auth,
            subscriptions: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
        }
    }

    /// Add tickers to subscribe to. If already connected, sends subscribe command.
    pub async fn subscribe(&self, tickers: Vec<String>) {
        let mut subs = self.subscriptions.lock().await;
        for t in &tickers {
            subs.insert(t.clone());
        }
    }

    /// Remove tickers from subscriptions.
    pub async fn unsubscribe(&self, tickers: &[String]) {
        let mut subs = self.subscriptions.lock().await;
        for t in tickers {
            subs.remove(t);
        }
    }

    /// Run the WebSocket feed loop with auto-reconnect.
    /// Sends parsed messages to `tx`. Runs until the channel is closed.
    pub async fn run(&self, tx: mpsc::Sender<WsFeedMessage>) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            match self.connect_and_stream(&tx).await {
                Ok(()) => {
                    info!("kalshi ws closed cleanly");
                    return;
                }
                Err(e) => {
                    error!(error = %e, "kalshi ws disconnected");
                    let _ = tx.send(WsFeedMessage::Disconnected).await;

                    let delay = Duration::from_secs(backoff_secs);
                    warn!(?delay, "reconnecting to kalshi ws");
                    tokio::time::sleep(delay).await;
                    backoff_secs = (backoff_secs * 2).min(max_backoff);
                }
            }
        }
    }

    /// Connect, authenticate, subscribe, and stream until error.
    async fn connect_and_stream(
        &self,
        tx: &mpsc::Sender<WsFeedMessage>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let auth_headers = self.auth.sign_request("GET", "/trade-api/ws/v2")?;

        let url = format!(
            "{}?api_key={}&signature={}&timestamp={}",
            self.ws_url, auth_headers.api_key, auth_headers.signature, auth_headers.timestamp
        );

        let (ws_stream, _resp) = connect_async(&url).await?;
        info!("kalshi ws connected");
        let _ = tx.send(WsFeedMessage::Reconnected).await;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to current ticker set
        let subs: Vec<String> = self.subscriptions.lock().await.iter().cloned().collect();
        if !subs.is_empty() {
            let cmd = SubscribeCmd {
                id: 1,
                cmd: "subscribe".into(),
                params: SubscribeParams {
                    channels: vec!["orderbook_delta".into(), "trade".into()],
                    market_tickers: subs,
                },
            };
            let msg = serde_json::to_string(&cmd)?;
            write.send(Message::Text(msg.into())).await?;
            info!("kalshi ws subscribed");
        }

        let mut ping_interval = interval(Duration::from_secs(30));
        let mut last_pong = Instant::now();

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Err(e) = self.handle_text_message(&text, tx).await {
                                warn!(error = %e, "failed to parse ws message");
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            last_pong = Instant::now();
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!("kalshi ws received close frame");
                            return Ok(());
                        }
                        Some(Err(e)) => {
                            return Err(Box::new(e));
                        }
                        None => {
                            return Err("ws stream ended".into());
                        }
                        _ => {}
                    }
                }
                _ = ping_interval.tick() => {
                    // Check pong timeout
                    if last_pong.elapsed() > Duration::from_secs(35) {
                        return Err("pong timeout".into());
                    }
                    if let Err(e) = write.send(Message::Ping(vec![].into())).await {
                        return Err(Box::new(e));
                    }
                }
            }
        }
    }

    /// Parse a text message from Kalshi and forward to channel.
    async fn handle_text_message(
        &self,
        text: &str,
        tx: &mpsc::Sender<WsFeedMessage>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Use simd-json for hot-path parsing
        let mut bytes = text.as_bytes().to_vec();
        let raw: WsRawMessage = match simd_json::from_slice(&mut bytes) {
            Ok(v) => v,
            Err(_) => {
                // Fallback to serde_json for non-standard messages
                serde_json::from_str(text)?
            }
        };

        let Some(channel) = &raw.channel else {
            return Ok(()); // heartbeat or ack
        };

        let Some(msg) = &raw.msg else {
            return Ok(());
        };

        match channel.as_str() {
            "orderbook_delta" => {
                if let Some(ticker) = msg.get("market_ticker").and_then(|v| v.as_str()) {
                    // Determine if this is a snapshot or delta based on message shape
                    if let Some(yes) = msg.get("yes") {
                        // Parse bids/asks arrays from the message
                        let bids = parse_price_levels(yes.get("bids"));
                        let asks = parse_price_levels(yes.get("asks"));

                        if !bids.is_empty() || !asks.is_empty() {
                            let _ = tx
                                .send(WsFeedMessage::OrderbookSnapshot {
                                    ticker: ticker.to_string(),
                                    yes_bids: bids,
                                    yes_asks: asks,
                                })
                                .await;
                        }
                    }

                    if let (Some(price), Some(delta)) = (
                        msg.get("price").and_then(|v| v.as_i64()),
                        msg.get("delta").and_then(|v| v.as_i64()),
                    ) {
                        let side = msg
                            .get("side")
                            .and_then(|v| v.as_str())
                            .unwrap_or("yes")
                            .to_string();

                        let _ = tx
                            .send(WsFeedMessage::OrderbookDelta {
                                ticker: ticker.to_string(),
                                side,
                                price_cents: price,
                                delta,
                            })
                            .await;
                    }
                }
            }
            "trade" => {
                if let (Some(ticker), Some(price), Some(count)) = (
                    msg.get("market_ticker").and_then(|v| v.as_str()),
                    msg.get("yes_price").and_then(|v| v.as_i64()),
                    msg.get("count").and_then(|v| v.as_i64()),
                ) {
                    let taker_side = msg
                        .get("taker_side")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let _ = tx
                        .send(WsFeedMessage::Trade {
                            ticker: ticker.to_string(),
                            price_cents: price,
                            count,
                            taker_side,
                        })
                        .await;
                }
            }
            _ => {}
        }

        Ok(())
    }
}

/// Parse price level arrays from JSON value: [[price, size], ...]
fn parse_price_levels(value: Option<&serde_json::Value>) -> Vec<(i64, i64)> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|entry| {
            let pair = entry.as_array()?;
            if pair.len() >= 2 {
                Some((pair[0].as_i64()?, pair[1].as_i64()?))
            } else {
                None
            }
        })
        .collect()
}

impl std::fmt::Debug for KalshiWsFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KalshiWsFeed")
            .field("ws_url", &self.ws_url)
            .finish()
    }
}

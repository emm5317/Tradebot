use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{Instant, interval};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::kalshi::auth::KalshiAuth;

/// Handle for dynamically subscribing tickers to the live WS connection.
#[derive(Clone)]
pub struct WsSubscriptionHandle {
    tx: mpsc::UnboundedSender<Vec<String>>,
}

impl WsSubscriptionHandle {
    /// Subscribe to orderbook/trade/ticker data for the given tickers.
    pub fn subscribe(&self, tickers: Vec<String>) {
        let _ = self.tx.send(tickers);
    }
}

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
    /// Ticker channel update (top-of-book, market status).
    TickerUpdate {
        ticker: String,
        yes_bid: Option<i64>,
        yes_ask: Option<i64>,
        yes_bid_size: Option<i64>,
        yes_ask_size: Option<i64>,
        last_price: Option<i64>,
        last_trade_count: Option<i64>,
        volume: Option<i64>,
        open_interest: Option<i64>,
        market_status: Option<String>,
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
    sub_rx: mpsc::UnboundedReceiver<Vec<String>>,
    cancel: CancellationToken,
}

impl KalshiWsFeed {
    /// Create a new WS feed and return a subscription handle for dynamic subscriptions.
    pub fn new(ws_url: String, auth: KalshiAuth, cancel: CancellationToken) -> (Self, WsSubscriptionHandle) {
        let (sub_tx, sub_rx) = mpsc::unbounded_channel();
        let feed = Self {
            ws_url,
            auth,
            subscriptions: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            sub_rx,
            cancel,
        };
        let handle = WsSubscriptionHandle { tx: sub_tx };
        (feed, handle)
    }

    /// Run the WebSocket feed loop with auto-reconnect.
    /// Returns when cancelled or the channel is closed.
    pub async fn run(mut self, tx: mpsc::Sender<WsFeedMessage>) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            if self.cancel.is_cancelled() {
                info!("kalshi ws feed cancelled");
                return;
            }

            match self.connect_and_stream(&tx).await {
                Ok(()) => {
                    info!("kalshi ws closed cleanly");
                    return;
                }
                Err(e) => {
                    error!(error = %e, "kalshi ws disconnected");
                    if tx.send(WsFeedMessage::Disconnected).await.is_err() {
                        warn!("ws feed receiver dropped, stopping");
                        return;
                    }

                    let delay = Duration::from_secs(backoff_secs);
                    warn!(?delay, "reconnecting to kalshi ws");

                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = self.cancel.cancelled() => {
                            info!("kalshi ws feed cancelled during backoff");
                            return;
                        }
                    }
                    backoff_secs = (backoff_secs * 2).min(max_backoff);
                }
            }
        }
    }

    /// Connect, authenticate via headers, subscribe, and stream until error.
    async fn connect_and_stream(
        &mut self,
        tx: &mpsc::Sender<WsFeedMessage>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let auth_headers = self.auth.sign_request("GET", "/trade-api/ws/v2")?;

        let mut request = self.ws_url.as_str().into_client_request()?;
        let headers = request.headers_mut();
        headers.insert("KALSHI-ACCESS-KEY", auth_headers.api_key.parse()?);
        headers.insert("KALSHI-ACCESS-SIGNATURE", auth_headers.signature.parse()?);
        headers.insert("KALSHI-ACCESS-TIMESTAMP", auth_headers.timestamp.parse()?);

        let (ws_stream, _resp) = tokio_tungstenite::connect_async(request).await?;
        info!("kalshi ws connected");
        if tx.send(WsFeedMessage::Reconnected).await.is_err() {
            return Ok(());
        }

        let (mut write, mut read) = ws_stream.split();
        let mut cmd_id: u64 = 1;

        // Subscribe to current ticker set
        let subs: Vec<String> = self.subscriptions.lock().await.iter().cloned().collect();
        if !subs.is_empty() {
            let cmd = SubscribeCmd {
                id: cmd_id,
                cmd: "subscribe".into(),
                params: SubscribeParams {
                    channels: vec!["orderbook_delta".into(), "trade".into(), "ticker".into()],
                    market_tickers: subs.clone(),
                },
            };
            cmd_id += 1;
            let msg = serde_json::to_string(&cmd)?;
            write.send(Message::Text(msg.into())).await?;
            info!(count = subs.len(), "kalshi ws subscribed (initial)");
        }

        let mut ping_interval = interval(Duration::from_secs(30));
        let mut last_pong = Instant::now();

        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    info!("kalshi ws feed cancelled, closing connection");
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(());
                }
                // Handle dynamic subscription requests
                sub_msg = self.sub_rx.recv() => {
                    match sub_msg {
                        Some(new_tickers) => {
                            // Filter to only truly new tickers
                            let mut subs = self.subscriptions.lock().await;
                            let new: Vec<String> = new_tickers
                                .into_iter()
                                .filter(|t| subs.insert(t.clone()))
                                .collect();

                            if !new.is_empty() {
                                let cmd = SubscribeCmd {
                                    id: cmd_id,
                                    cmd: "subscribe".into(),
                                    params: SubscribeParams {
                                        channels: vec!["orderbook_delta".into(), "trade".into(), "ticker".into()],
                                        market_tickers: new.clone(),
                                    },
                                };
                                cmd_id += 1;
                                let msg = serde_json::to_string(&cmd)?;
                                write.send(Message::Text(msg.into())).await?;
                                info!(count = new.len(), total = subs.len(), "kalshi ws subscribed (dynamic)");
                            }
                        }
                        None => {
                            // Subscription handle dropped — continue without dynamic subs
                        }
                    }
                }
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
        let raw: WsRawMessage = serde_json::from_str(text)?;

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
            "ticker" => {
                if let Some(ticker) = msg.get("market_ticker").and_then(|v| v.as_str()) {
                    let _ = tx
                        .send(WsFeedMessage::TickerUpdate {
                            ticker: ticker.to_string(),
                            yes_bid: msg.get("yes_bid").and_then(|v| v.as_i64()),
                            yes_ask: msg.get("yes_ask").and_then(|v| v.as_i64()),
                            yes_bid_size: msg.get("yes_bid_size").and_then(|v| v.as_i64()),
                            yes_ask_size: msg.get("yes_ask_size").and_then(|v| v.as_i64()),
                            last_price: msg.get("last_price").and_then(|v| v.as_i64()),
                            last_trade_count: msg.get("last_trade_count").and_then(|v| v.as_i64()),
                            volume: msg.get("volume").and_then(|v| v.as_i64()),
                            open_interest: msg.get("open_interest").and_then(|v| v.as_i64()),
                            market_status: msg.get("market_status").and_then(|v| v.as_str()).map(|s| s.to_string()),
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

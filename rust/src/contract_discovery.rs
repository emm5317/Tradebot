//! Contract discovery — discovers active crypto contracts from the database.
//!
//! Phase 3: Provides the crypto evaluator with a cached list of contracts
//! nearing settlement, refreshed every 60 seconds.

use std::sync::RwLock;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::lock_ext::RwLockExt;

use crate::kalshi::websocket::WsSubscriptionHandle;

/// A crypto binary contract nearing settlement.
#[derive(Debug, Clone)]
pub struct CryptoContract {
    pub ticker: String,
    pub strike: f64,
    pub settlement_time: DateTime<Utc>,
    /// True for directional "price up?" contracts (e.g. KXBTC15M).
    /// For these, the strike is set to the current BTC price at eval time.
    pub directional: bool,
}

/// Cached discovery of active crypto contracts from the database.
pub struct ContractDiscovery {
    contracts: RwLock<Vec<CryptoContract>>,
    ws_handle: Option<WsSubscriptionHandle>,
}

impl ContractDiscovery {
    pub fn new() -> Self {
        Self {
            contracts: RwLock::new(Vec::new()),
            ws_handle: None,
        }
    }

    /// Create with a WS subscription handle for dynamic orderbook subscriptions.
    pub fn with_ws_handle(ws_handle: WsSubscriptionHandle) -> Self {
        Self {
            contracts: RwLock::new(Vec::new()),
            ws_handle: Some(ws_handle),
        }
    }

    /// Refresh the contract cache from the database.
    pub async fn refresh(&self, pool: &PgPool) {
        type ContractRow = (String, Option<f64>, DateTime<Utc>, Option<String>);
        let result: Result<Vec<ContractRow>, _> = sqlx::query_as(
            r#"
            SELECT c.ticker,
                   COALESCE(cr.strike::float8, c.threshold::float8) AS strike,
                   c.settlement_time,
                   c.title
            FROM contracts c
            LEFT JOIN contract_rules cr ON cr.market_ticker = c.ticker
            WHERE c.status IN ('active', 'initialized', 'open')
              AND (c.category ILIKE '%crypto%' OR c.category ILIKE '%bitcoin%' OR c.category ILIKE '%btc%'
                   OR cr.contract_type = 'crypto_binary')
              AND c.settlement_time > now()
              AND c.settlement_time < now() + interval '65 minutes'
            ORDER BY c.settlement_time
            "#,
        )
        .fetch_all(pool)
        .await;

        match result {
            Ok(rows) => {
                let contracts: Vec<CryptoContract> = rows
                    .into_iter()
                    .filter_map(|(ticker, strike, settlement_time, title)| {
                        // Detect directional "price up?" contracts (e.g. KXBTC15M)
                        let is_directional = ticker.contains("15M")
                            || title.as_deref().unwrap_or("").contains("price up");

                        if is_directional {
                            // Directional contracts use current price as strike at eval time.
                            // Use a placeholder strike of 0; evaluator will substitute shadow_rti.
                            Some(CryptoContract {
                                ticker,
                                strike: 0.0,
                                settlement_time,
                                directional: true,
                            })
                        } else {
                            // Strike-based binary options — require valid strike
                            let strike = strike?;
                            if strike <= 0.0 {
                                return None;
                            }
                            Some(CryptoContract {
                                ticker,
                                strike,
                                settlement_time,
                                directional: false,
                            })
                        }
                    })
                    .collect();

                let count = contracts.len();

                // Subscribe new tickers to the Kalshi WS feed for orderbook data
                if let Some(ref handle) = self.ws_handle {
                    let tickers: Vec<String> = contracts.iter().map(|c| c.ticker.clone()).collect();
                    if !tickers.is_empty() {
                        handle.subscribe(tickers);
                    }
                }

                let mut cache = self.contracts.write_or_recover();
                let prev_count = cache.len();
                *cache = contracts;

                if count != prev_count {
                    info!(
                        contracts = count,
                        prev = prev_count,
                        "contract discovery refreshed"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "contract discovery query failed");
            }
        }
    }

    /// Get a snapshot of currently active contracts.
    pub fn active_contracts(&self) -> Vec<CryptoContract> {
        self.contracts.read_or_recover().clone()
    }

    /// Run the refresh loop every 60 seconds until cancelled.
    pub async fn run(self: &std::sync::Arc<Self>, pool: PgPool, cancel: CancellationToken) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.refresh(&pool).await;
                }
                _ = cancel.cancelled() => {
                    info!("contract discovery shutting down");
                    break;
                }
            }
        }
    }
}

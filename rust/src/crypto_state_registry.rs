//! Per-asset crypto state registry — maps CryptoAsset to CryptoState.
//!
//! Phase 13: Each asset gets its own CryptoState instance.
//! The registry provides a merged watch channel that fires on ANY asset update.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;

use crate::crypto_asset::CryptoAsset;
use crate::crypto_state::{CryptoState, RtiConfig};

/// Registry that holds one CryptoState per enabled asset.
pub struct CryptoStateRegistry {
    states: HashMap<CryptoAsset, Arc<CryptoState>>,
    merged_tx: watch::Sender<u64>,
}

impl CryptoStateRegistry {
    /// Create a registry with one CryptoState per asset.
    pub fn new(assets: &[CryptoAsset], rti_config: RtiConfig) -> Self {
        let (merged_tx, _) = watch::channel(0u64);
        let mut states = HashMap::new();

        for &asset in assets {
            let cs = Arc::new(CryptoState::with_config(rti_config.clone()));

            // Spawn a forwarding task: when this asset's state updates,
            // increment the merged counter so the evaluator wakes up.
            let mut rx = cs.subscribe();
            let tx = merged_tx.clone();
            tokio::spawn(async move {
                loop {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    tx.send_modify(|v| *v += 1);
                }
            });

            states.insert(asset, cs);
        }

        Self { states, merged_tx }
    }

    /// Get the CryptoState for a specific asset, if enabled.
    pub fn get(&self, asset: CryptoAsset) -> Option<&Arc<CryptoState>> {
        self.states.get(&asset)
    }

    /// Subscribe to the merged watch channel (fires on ANY asset update).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.merged_tx.subscribe()
    }

    /// List all enabled assets.
    pub fn enabled_assets(&self) -> Vec<CryptoAsset> {
        self.states.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_multi_asset_creation() {
        let assets = vec![CryptoAsset::BTC, CryptoAsset::ETH];
        let registry = CryptoStateRegistry::new(&assets, RtiConfig::default());

        assert!(registry.get(CryptoAsset::BTC).is_some());
        assert!(registry.get(CryptoAsset::ETH).is_some());
        assert!(registry.get(CryptoAsset::SOL).is_none());
    }

    #[tokio::test]
    async fn test_per_asset_snapshot_isolation() {
        let assets = vec![CryptoAsset::BTC, CryptoAsset::ETH];
        let registry = CryptoStateRegistry::new(&assets, RtiConfig::default());

        // Update BTC state
        let btc = registry.get(CryptoAsset::BTC).unwrap();
        btc.update_coinbase(95000.0, 0.0, 0.0, 10.0);

        // Update ETH state with different price
        let eth = registry.get(CryptoAsset::ETH).unwrap();
        eth.update_coinbase(3500.0, 0.0, 0.0, 5.0);

        // Snapshots should be independent
        let btc_snap = btc.snapshot();
        let eth_snap = eth.snapshot();

        assert!((btc_snap.coinbase_spot - 95000.0).abs() < 0.01);
        assert!((eth_snap.coinbase_spot - 3500.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_merged_notify_fires() {
        let assets = vec![CryptoAsset::BTC, CryptoAsset::ETH];
        let registry = CryptoStateRegistry::new(&assets, RtiConfig::default());
        let mut rx = registry.subscribe();

        // Initial value
        let initial = *rx.borrow();

        // Update ETH — should trigger merged notify
        let eth = registry.get(CryptoAsset::ETH).unwrap();
        eth.update_coinbase(3500.0, 0.0, 0.0, 5.0);

        // Give the forwarding task a moment to propagate
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // The merged counter should have incremented
        let current = *rx.borrow_and_update();
        assert!(current > initial, "merged counter should increment: was {}, now {}", initial, current);
    }

    #[test]
    fn test_enabled_assets() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let assets = vec![CryptoAsset::BTC, CryptoAsset::SOL, CryptoAsset::DOGE];
            let registry = CryptoStateRegistry::new(&assets, RtiConfig::default());

            let enabled = registry.enabled_assets();
            assert_eq!(enabled.len(), 3);
            assert!(enabled.contains(&CryptoAsset::BTC));
            assert!(enabled.contains(&CryptoAsset::SOL));
            assert!(enabled.contains(&CryptoAsset::DOGE));
        });
    }
}

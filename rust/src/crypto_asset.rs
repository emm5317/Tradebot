//! Multi-asset crypto support — asset enum and per-asset configuration.
//!
//! Phase 13: Extends the trading bot from BTC-only to support ETH, SOL, XRP, and DOGE.
//! All assets settle on CF Benchmarks Real-Time Indices using a 60-second simple average.

use std::fmt;

/// Supported crypto assets for trading.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum CryptoAsset {
    BTC,
    ETH,
    SOL,
    XRP,
    DOGE,
}

impl CryptoAsset {
    /// Parse a Kalshi ticker prefix into an asset.
    /// E.g. "KXBTCD-26MAR08-T98500" → Some(BTC), "KXETH-26MAR12-T3500" → Some(ETH)
    pub fn from_ticker(ticker: &str) -> Option<Self> {
        let upper = ticker.to_uppercase();
        if upper.starts_with("KXBTC") {
            Some(CryptoAsset::BTC)
        } else if upper.starts_with("KXETH") {
            Some(CryptoAsset::ETH)
        } else if upper.starts_with("KXSOL") {
            Some(CryptoAsset::SOL)
        } else if upper.starts_with("KXXRP") {
            Some(CryptoAsset::XRP)
        } else if upper.starts_with("KXDOGE") {
            Some(CryptoAsset::DOGE)
        } else {
            None
        }
    }

    /// Coinbase Advanced Trade product ID.
    pub fn coinbase_product_id(&self) -> &'static str {
        match self {
            CryptoAsset::BTC => "BTC-USD",
            CryptoAsset::ETH => "ETH-USD",
            CryptoAsset::SOL => "SOL-USD",
            CryptoAsset::XRP => "XRP-USD",
            CryptoAsset::DOGE => "DOGE-USD",
        }
    }

    /// Binance spot trade stream symbol (lowercase).
    pub fn binance_symbol(&self) -> &'static str {
        match self {
            CryptoAsset::BTC => "btcusdt",
            CryptoAsset::ETH => "ethusdt",
            CryptoAsset::SOL => "solusdt",
            CryptoAsset::XRP => "xrpusdt",
            CryptoAsset::DOGE => "dogeusdt",
        }
    }

    /// Binance symbol in uppercase (for matching "s" field in trade messages).
    pub fn binance_symbol_upper(&self) -> &'static str {
        match self {
            CryptoAsset::BTC => "BTCUSDT",
            CryptoAsset::ETH => "ETHUSDT",
            CryptoAsset::SOL => "SOLUSDT",
            CryptoAsset::XRP => "XRPUSDT",
            CryptoAsset::DOGE => "DOGEUSDT",
        }
    }

    /// Whether this asset has futures on Binance.us (only BTC).
    pub fn has_futures(&self) -> bool {
        matches!(self, CryptoAsset::BTC)
    }

    /// Whether this asset has DVOL on Deribit (only BTC).
    pub fn has_dvol(&self) -> bool {
        matches!(self, CryptoAsset::BTC)
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            CryptoAsset::BTC => "Bitcoin",
            CryptoAsset::ETH => "Ethereum",
            CryptoAsset::SOL => "Solana",
            CryptoAsset::XRP => "XRP",
            CryptoAsset::DOGE => "Dogecoin",
        }
    }

    /// Short lowercase name for Redis keys and feed health names.
    pub fn short_name(&self) -> &'static str {
        match self {
            CryptoAsset::BTC => "btc",
            CryptoAsset::ETH => "eth",
            CryptoAsset::SOL => "sol",
            CryptoAsset::XRP => "xrp",
            CryptoAsset::DOGE => "doge",
        }
    }

    /// All known assets.
    pub fn all() -> &'static [CryptoAsset] {
        &[
            CryptoAsset::BTC,
            CryptoAsset::ETH,
            CryptoAsset::SOL,
            CryptoAsset::XRP,
            CryptoAsset::DOGE,
        ]
    }
}

impl fmt::Display for CryptoAsset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_ticker_btc_variants() {
        assert_eq!(CryptoAsset::from_ticker("KXBTCD-26MAR08-T98500"), Some(CryptoAsset::BTC));
        assert_eq!(CryptoAsset::from_ticker("KXBTC15M-26MAR08-T1"), Some(CryptoAsset::BTC));
        assert_eq!(CryptoAsset::from_ticker("KXBTC-26MAR08"), Some(CryptoAsset::BTC));
    }

    #[test]
    fn test_from_ticker_all_assets() {
        assert_eq!(CryptoAsset::from_ticker("KXETH-26MAR12-T3500"), Some(CryptoAsset::ETH));
        assert_eq!(CryptoAsset::from_ticker("KXETHD-26MAR12-T1"), Some(CryptoAsset::ETH));
        assert_eq!(CryptoAsset::from_ticker("KXSOL-26MAR12-T150"), Some(CryptoAsset::SOL));
        assert_eq!(CryptoAsset::from_ticker("KXXRP-26MAR12-T2"), Some(CryptoAsset::XRP));
        assert_eq!(CryptoAsset::from_ticker("KXDOGE-26MAR12-T0.15"), Some(CryptoAsset::DOGE));
    }

    #[test]
    fn test_from_ticker_unknown() {
        assert_eq!(CryptoAsset::from_ticker("KXTEMP-NYC-26MAR12"), None);
        assert_eq!(CryptoAsset::from_ticker("UNKNOWN"), None);
        assert_eq!(CryptoAsset::from_ticker(""), None);
    }

    #[test]
    fn test_from_ticker_case_insensitive() {
        assert_eq!(CryptoAsset::from_ticker("kxbtc-test"), Some(CryptoAsset::BTC));
        assert_eq!(CryptoAsset::from_ticker("kxeth-test"), Some(CryptoAsset::ETH));
    }

    #[test]
    fn test_coinbase_product_ids() {
        assert_eq!(CryptoAsset::BTC.coinbase_product_id(), "BTC-USD");
        assert_eq!(CryptoAsset::ETH.coinbase_product_id(), "ETH-USD");
        assert_eq!(CryptoAsset::SOL.coinbase_product_id(), "SOL-USD");
        assert_eq!(CryptoAsset::XRP.coinbase_product_id(), "XRP-USD");
        assert_eq!(CryptoAsset::DOGE.coinbase_product_id(), "DOGE-USD");
    }

    #[test]
    fn test_binance_symbols() {
        assert_eq!(CryptoAsset::BTC.binance_symbol(), "btcusdt");
        assert_eq!(CryptoAsset::ETH.binance_symbol(), "ethusdt");
        assert_eq!(CryptoAsset::SOL.binance_symbol(), "solusdt");
        assert_eq!(CryptoAsset::XRP.binance_symbol(), "xrpusdt");
        assert_eq!(CryptoAsset::DOGE.binance_symbol(), "dogeusdt");
    }

    #[test]
    fn test_has_futures_only_btc() {
        assert!(CryptoAsset::BTC.has_futures());
        assert!(!CryptoAsset::ETH.has_futures());
        assert!(!CryptoAsset::SOL.has_futures());
        assert!(!CryptoAsset::XRP.has_futures());
        assert!(!CryptoAsset::DOGE.has_futures());
    }

    #[test]
    fn test_has_dvol_only_btc() {
        assert!(CryptoAsset::BTC.has_dvol());
        assert!(!CryptoAsset::ETH.has_dvol());
        assert!(!CryptoAsset::SOL.has_dvol());
    }

    #[test]
    fn test_display_name() {
        assert_eq!(CryptoAsset::BTC.display_name(), "Bitcoin");
        assert_eq!(CryptoAsset::ETH.display_name(), "Ethereum");
        assert_eq!(CryptoAsset::SOL.display_name(), "Solana");
        assert_eq!(CryptoAsset::XRP.display_name(), "XRP");
        assert_eq!(CryptoAsset::DOGE.display_name(), "Dogecoin");
    }

    #[test]
    fn test_short_name() {
        assert_eq!(CryptoAsset::BTC.short_name(), "btc");
        assert_eq!(CryptoAsset::ETH.short_name(), "eth");
    }
}

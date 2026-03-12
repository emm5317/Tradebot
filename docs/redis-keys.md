# Redis Key Structure

Tradebot uses Redis as a shared state cache between Rust and Python components.

## Keys

```
orderbook:{ticker}          # Kalshi book state (from Rust, 500ms flush)
crypto:coinbase             # Coinbase BTC-USD spot/bid/ask + trade volume (legacy BTC alias)
crypto:coinbase:btc         # Coinbase BTC-USD per-asset state
crypto:coinbase:eth         # Coinbase ETH-USD per-asset state
crypto:coinbase:sol         # Coinbase SOL-USD per-asset state (when enabled)
crypto:coinbase:xrp         # Coinbase XRP-USD per-asset state (when enabled)
crypto:coinbase:doge        # Coinbase DOGE-USD per-asset state (when enabled)
crypto:binance_spot         # Binance spot BTC (legacy alias)
crypto:binance_spot:btc     # Binance spot BTC per-asset state
crypto:binance_spot:eth     # Binance spot ETH per-asset state
crypto:binance_spot:sol     # Binance spot SOL per-asset state (when enabled)
crypto:binance_spot:xrp     # Binance spot XRP per-asset state (when enabled)
crypto:binance_spot:doge    # Binance spot DOGE per-asset state (when enabled)
crypto:binance_futures      # Binance perp/mark/funding/OBI (BTC-only)
crypto:deribit_dvol         # Deribit BTC volatility index (BTC-only)
model_state:{ticker}        # Model output for dashboard
feed:status:{ticker}        # Feed health/staleness
crypto:rti                  # Real-time index estimate (shadow RTI from Rust)
crypto:oi_delta:{ticker}    # Open interest delta tracking
signal:latest:{ticker}      # Latest signal per ticker (for dashboard)
edge_tracker:{ticker}       # Edge trajectory history
```

## Data Flow

1. **Rust** flushes orderbook and crypto feed state to Redis every 500ms
2. **Python evaluator** reads crypto/orderbook state from Redis on each 10s evaluation cycle (weather only — crypto eval is in Rust)
3. **Python evaluator** writes model state back to Redis for the dashboard
4. **Dashboard** reads model state from Redis for SSE updates

## Dashboard Keys

The Bloomberg terminal dashboard reads these keys for real-time display:
- `model_state:{ticker}` — Latest model output (fair value, edge, components)
- `signal:latest:{ticker}` — Most recent signal per ticker
- `feed:status:{ticker}` — Feed health for the RISK page feed matrix

## TTL Policy

- Orderbook and crypto keys have no explicit TTL — they are overwritten on each flush cycle
- Feed health keys include a timestamp field; the consumer checks staleness rather than relying on TTL

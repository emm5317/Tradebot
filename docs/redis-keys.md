# Redis Key Structure

Tradebot uses Redis as a shared state cache between Rust and Python components.

## Keys

```
orderbook:{ticker}          # Kalshi book state (from Rust, 500ms flush)
crypto:coinbase             # Coinbase BTC-USD spot/bid/ask + trade volume
crypto:binance_spot         # Binance spot + realized/EWMA vol
crypto:binance_futures      # Binance perp/mark/funding/OBI
crypto:deribit_dvol         # Deribit BTC volatility index
model_state:{ticker}        # Model output for dashboard
feed:status:{ticker}        # Feed health/staleness
```

## Data Flow

1. **Rust** flushes orderbook and crypto feed state to Redis every 500ms
2. **Python evaluator** reads crypto/orderbook state from Redis on each 10s evaluation cycle
3. **Python evaluator** writes model state back to Redis for the dashboard
4. **Dashboard** reads model state from Redis for SSE updates

## TTL Policy

- Orderbook and crypto keys have no explicit TTL — they are overwritten on each flush cycle
- Feed health keys include a timestamp field; the consumer checks staleness rather than relying on TTL

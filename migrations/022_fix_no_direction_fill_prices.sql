-- Phase 10.1: Fix fill_price for NO direction paper orders
-- Previously stored YES mid-price instead of NO price (1 - mid)

-- Fix fill_price for NO direction paper orders (store NO price, not YES price)
UPDATE orders SET fill_price = 1.0 - fill_price
WHERE direction = 'no' AND fill_price > 0.50
  AND fill_price IS NOT NULL;

-- Recompute PnL with corrected fill prices
UPDATE orders SET
    pnl_cents = CASE
        WHEN outcome = 'win' THEN (100 - ROUND(fill_price * 100)::integer)
        WHEN outcome = 'loss' THEN (-ROUND(fill_price * 100)::integer)
        ELSE pnl_cents
    END
WHERE outcome IN ('win', 'loss')
  AND fill_price IS NOT NULL;

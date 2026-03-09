"""Calibration Agent Daemon — closes the feedback loop between predictions and outcomes.

Runs an hourly cycle with six jobs:
1. Settle order outcomes (win/loss from contracts.settled_yes)
2. Populate calibration hypertable
3. Compute rolling metrics (Brier, slippage, edge realization)
4. Update station calibration weights from sweep results
5. HRRR skill recalculation
6. Drift detection & Discord alerting
"""

from __future__ import annotations

import asyncio
import signal
from datetime import date, timedelta, timezone, datetime

import asyncpg
import structlog

from config import Settings, get_settings
from models.physics import compute_hrrr_skill_scores

logger = structlog.get_logger()


class CalibrationDaemon:
    """Hourly calibration agent that learns from settlement outcomes."""

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or get_settings()
        self.pool: asyncpg.Pool | None = None
        self._shutdown = asyncio.Event()

    async def run(self) -> None:
        """Initialize connections and run calibration loop."""
        self.pool = await asyncpg.create_pool(
            self.settings.database_url, min_size=2, max_size=5
        )
        logger.info("calibrator_started")

        try:
            await self._calibration_loop()
        finally:
            if self.pool:
                await self.pool.close()
            logger.info("calibrator_stopped")

    async def _calibration_loop(self) -> None:
        """Main loop: run all calibration jobs every hour."""
        # Initial delay to let other services establish data
        await self._sleep_or_shutdown(30)

        while not self._shutdown.is_set():
            try:
                await self.settle_order_outcomes()
                await self.populate_calibration_table()
                await self.compute_rolling_metrics()
                await self.update_station_weights()
                await self.recalculate_hrrr_skill()
                await self.check_drift()
                logger.info("calibration_cycle_complete")
            except Exception:
                logger.exception("calibration_cycle_failed")

            await self._sleep_or_shutdown(3600)  # hourly

    # ------------------------------------------------------------------
    # Job 1: Settle Order Outcomes
    # ------------------------------------------------------------------

    async def settle_order_outcomes(self) -> int:
        """Update orders with win/loss based on contract settlement."""
        assert self.pool is not None

        async with self.pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE orders SET outcome = CASE
                    WHEN (direction = 'yes' AND c.settled_yes = true)
                      OR (direction = 'no' AND c.settled_yes = false)
                    THEN 'win'
                    ELSE 'loss'
                END
                FROM contracts c
                WHERE orders.ticker = c.ticker
                  AND c.settled_yes IS NOT NULL
                  AND orders.outcome = 'pending'
                """
            )

        rows = _parse_update_count(result)
        if rows > 0:
            logger.info("orders_settled", count=rows)
        return rows

    # ------------------------------------------------------------------
    # Job 2: Populate Calibration Table
    # ------------------------------------------------------------------

    async def populate_calibration_table(self) -> int:
        """Join signals against settled contracts to write calibration records."""
        assert self.pool is not None

        async with self.pool.acquire() as conn:
            result = await conn.execute(
                """
                INSERT INTO calibration (
                    ticker, signal_type, model_prob, market_price,
                    actual_outcome, prob_bucket, sigma_used, settled_at
                )
                SELECT
                    s.ticker,
                    s.signal_type,
                    s.model_prob,
                    s.market_price,
                    c.settled_yes,
                    ROUND(s.model_prob::numeric, 1)::text,
                    COALESCE(s.edge, 0),
                    COALESCE(c.settlement_time, now())
                FROM signals s
                JOIN contracts c ON s.ticker = c.ticker
                WHERE c.settled_yes IS NOT NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM calibration cal
                      WHERE cal.ticker = s.ticker
                        AND cal.signal_type = s.signal_type
                        AND cal.model_prob = s.model_prob
                        AND cal.settled_at = COALESCE(c.settlement_time, now())
                  )
                ON CONFLICT DO NOTHING
                """
            )

        rows = _parse_update_count(result)
        if rows > 0:
            logger.info("calibration_populated", count=rows)
        return rows

    # ------------------------------------------------------------------
    # Job 3: Compute Rolling Metrics
    # ------------------------------------------------------------------

    async def compute_rolling_metrics(self) -> None:
        """Compute 30-day rolling Brier, slippage, and edge metrics."""
        assert self.pool is not None

        today = date.today()
        period_start = today - timedelta(days=30)

        async with self.pool.acquire() as conn:
            # Per-strategy rolling metrics
            rows = await conn.fetch(
                """
                SELECT
                    s.signal_type AS strategy,
                    COUNT(*) AS signal_count,
                    AVG(POWER(s.model_prob - CASE WHEN c.settled_yes THEN 1.0 ELSE 0.0 END, 2)) AS brier_score,
                    AVG(s.model_prob) AS avg_predicted,
                    AVG(CASE WHEN c.settled_yes THEN 1.0 ELSE 0.0 END) AS avg_actual,
                    AVG(o.fill_price - o.market_price_at_order) AS avg_slippage,
                    PERCENTILE_CONT(0.95) WITHIN GROUP (
                        ORDER BY ABS(COALESCE(o.fill_price - o.market_price_at_order, 0))
                    ) AS p95_slippage
                FROM signals s
                JOIN contracts c ON s.ticker = c.ticker
                LEFT JOIN orders o ON o.signal_id = s.id
                WHERE c.settled_yes IS NOT NULL
                  AND s.created_at >= $1
                  AND s.created_at <= $2
                GROUP BY s.signal_type
                """,
                datetime.combine(period_start, datetime.min.time()).replace(
                    tzinfo=timezone.utc
                ),
                datetime.combine(today, datetime.max.time()).replace(
                    tzinfo=timezone.utc
                ),
            )

            for row in rows:
                await conn.execute(
                    """
                    INSERT INTO calibration_metrics (
                        strategy, period_start, period_end,
                        brier_score, avg_predicted, avg_actual,
                        signal_count, avg_slippage, p95_slippage
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    """,
                    row["strategy"],
                    period_start,
                    today,
                    row["brier_score"],
                    row["avg_predicted"],
                    row["avg_actual"],
                    row["signal_count"],
                    row["avg_slippage"],
                    row["p95_slippage"],
                )

        if rows:
            logger.info("rolling_metrics_computed", strategies=len(rows))

    # ------------------------------------------------------------------
    # Job 4: Update Station Calibration Weights
    # ------------------------------------------------------------------

    async def update_station_weights(self) -> None:
        """Compare current weights against best recent sweep results."""
        assert self.pool is not None

        async with self.pool.acquire() as conn:
            # Get recent sweep results that beat current calibration
            sweeps = await conn.fetch(
                """
                SELECT DISTINCT ON (br.station)
                    br.station,
                    br.brier_score AS sweep_brier,
                    br.params
                FROM backtest_runs br
                WHERE br.run_date > now() - interval '14 days'
                  AND br.brier_score IS NOT NULL
                ORDER BY br.station, br.brier_score ASC
                """
            )

            for sweep in sweeps:
                station = sweep["station"]
                if station is None:
                    continue

                # Get current best Brier for this station
                current = await conn.fetchrow(
                    """
                    SELECT AVG(
                        POWER(s.model_prob - CASE WHEN c.settled_yes THEN 1.0 ELSE 0.0 END, 2)
                    ) AS brier_score
                    FROM signals s
                    JOIN contracts c ON s.ticker = c.ticker
                    JOIN contract_rules cr ON cr.ticker = c.ticker
                    WHERE c.settled_yes IS NOT NULL
                      AND cr.settlement_station = $1
                      AND s.created_at > now() - interval '30 days'
                    """,
                    station,
                )

                if current is None or current["brier_score"] is None:
                    continue

                current_brier = float(current["brier_score"])
                sweep_brier = float(sweep["sweep_brier"])

                # Only update if sweep beats current by >0.005
                if sweep_brier < current_brier - 0.005:
                    params = sweep["params"] or {}
                    if isinstance(params, str):
                        import json

                        params = json.loads(params)

                    await conn.execute(
                        """
                        UPDATE station_calibration SET
                            weight_physics = COALESCE($2, weight_physics),
                            weight_hrrr = COALESCE($3, weight_hrrr),
                            weight_trend = COALESCE($4, weight_trend),
                            weight_climo = COALESCE($5, weight_climo),
                            updated_at = now()
                        WHERE station = $1
                        """,
                        station,
                        params.get("weight_physics"),
                        params.get("weight_hrrr"),
                        params.get("weight_trend"),
                        params.get("weight_climo"),
                    )
                    logger.info(
                        "calibration_weights_updated",
                        station=station,
                        old_brier=current_brier,
                        new_brier=sweep_brier,
                    )

    # ------------------------------------------------------------------
    # Job 5: HRRR Skill Recalculation
    # ------------------------------------------------------------------

    async def recalculate_hrrr_skill(self) -> None:
        """Recompute HRRR bias/RMSE/skill scores from recent observations."""
        assert self.pool is not None
        await compute_hrrr_skill_scores(self.pool)

    # ------------------------------------------------------------------
    # Job 6: Drift Detection
    # ------------------------------------------------------------------

    async def check_drift(self) -> None:
        """Alert if any strategy's Brier score degrades significantly."""
        assert self.pool is not None

        webhook_url = self.settings.discord_webhook_url
        if not webhook_url:
            return

        for strategy in ("weather", "crypto"):
            recent = await self._rolling_brier(strategy, days=7)
            baseline = await self._rolling_brier(strategy, days=30)

            if recent is None or baseline is None:
                continue

            if recent > baseline + 0.03:
                await self._send_discord_alert(
                    webhook_url,
                    f"Model drift: {strategy} 7d Brier {recent:.3f} "
                    f"vs 30d baseline {baseline:.3f}",
                )
                logger.warning(
                    "drift_detected",
                    strategy=strategy,
                    recent_brier=recent,
                    baseline_brier=baseline,
                )

    async def _rolling_brier(self, strategy: str, days: int) -> float | None:
        """Compute rolling Brier score for a strategy over N days."""
        assert self.pool is not None

        async with self.pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT AVG(
                    POWER(s.model_prob - CASE WHEN c.settled_yes THEN 1.0 ELSE 0.0 END, 2)
                ) AS brier
                FROM signals s
                JOIN contracts c ON s.ticker = c.ticker
                WHERE s.signal_type = $1
                  AND c.settled_yes IS NOT NULL
                  AND s.created_at > now() - make_interval(days => $2)
                """,
                strategy,
                days,
            )

        if row is None or row["brier"] is None:
            return None
        return float(row["brier"])

    async def _send_discord_alert(self, webhook_url: str, message: str) -> None:
        """Send alert to Discord webhook."""
        try:
            import aiohttp

            async with aiohttp.ClientSession() as session:
                await session.post(
                    webhook_url,
                    json={"content": f"⚠️ **Tradebot Alert**: {message}"},
                    timeout=aiohttp.ClientTimeout(total=10),
                )
            logger.info("discord_alert_sent", message=message)
        except Exception:
            logger.warning("discord_alert_failed", exc_info=True)

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    async def _sleep_or_shutdown(self, seconds: float) -> None:
        """Sleep for the given duration, returning early if shutdown is signalled."""
        try:
            await asyncio.wait_for(self._shutdown.wait(), timeout=seconds)
        except asyncio.TimeoutError:
            pass

    def shutdown(self) -> None:
        """Signal the daemon to stop."""
        self._shutdown.set()


def _parse_update_count(result: str) -> int:
    """Parse asyncpg UPDATE/INSERT result string like 'UPDATE 5' to get count."""
    try:
        return int(result.split()[-1])
    except (ValueError, IndexError):
        return 0


async def main() -> None:
    settings = get_settings()
    daemon = CalibrationDaemon(settings)

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda s, f: daemon.shutdown())

    await daemon.run()


if __name__ == "__main__":
    asyncio.run(main())

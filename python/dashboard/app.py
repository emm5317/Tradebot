"""Bloomberg-style terminal dashboard for Tradebot.

FastAPI + Jinja2 + SSE. 6-page tabbed terminal with persistent status bar.
Reads model state from Redis, signal history from DB, live events from NATS.
"""

from __future__ import annotations

import asyncio
import json
from contextlib import asynccontextmanager
from datetime import UTC, datetime

import asyncpg
import nats
import redis.asyncio as aioredis
import structlog
import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import HTMLResponse, RedirectResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from sse_starlette.sse import EventSourceResponse

from config import Settings, get_settings

logger = structlog.get_logger()

settings: Settings = get_settings()
pool: asyncpg.Pool | None = None
redis_client: aioredis.Redis | None = None
nats_client: nats.NATS | None = None


@asynccontextmanager
async def lifespan(app: FastAPI):
    global pool, redis_client, nats_client
    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)
    redis_client = aioredis.from_url(settings.redis_url)
    nats_client = await nats.connect(settings.nats_url)
    logger.info("dashboard_started", port=settings.dashboard_port)
    yield
    if nats_client:
        await nats_client.close()
    if pool:
        await pool.close()
    if redis_client:
        await redis_client.close()


app = FastAPI(title="Tradebot Terminal", lifespan=lifespan)
app.mount("/static", StaticFiles(directory="dashboard/static"), name="static")
templates = Jinja2Templates(directory="dashboard/templates")


# ── Page Routes ──────────────────────────────────────────────────────


@app.get("/", response_class=HTMLResponse)
async def page_main(request: Request):
    return templates.TemplateResponse("main.html", {"request": request, "active_tab": "main"})


@app.get("/signals", response_class=HTMLResponse)
async def page_signals(request: Request):
    return templates.TemplateResponse("signals.html", {"request": request, "active_tab": "signals"})


@app.get("/execution", response_class=HTMLResponse)
async def page_execution(request: Request):
    return templates.TemplateResponse("execution.html", {"request": request, "active_tab": "execution"})


@app.get("/analytics", response_class=HTMLResponse)
async def page_analytics(request: Request):
    return templates.TemplateResponse("analytics.html", {"request": request, "active_tab": "analytics"})


@app.get("/risk", response_class=HTMLResponse)
async def page_risk(request: Request):
    return templates.TemplateResponse("risk.html", {"request": request, "active_tab": "risk"})


@app.get("/weather", response_class=HTMLResponse)
async def page_weather(request: Request):
    return templates.TemplateResponse("weather.html", {"request": request, "active_tab": "weather"})


# Legacy route redirect
@app.get("/calibration")
async def calibration_redirect():
    return RedirectResponse(url="/analytics", status_code=301)


# ── API Endpoints ────────────────────────────────────────────────────


@app.get("/api/health")
async def health():
    checks = {"status": "ok", "timestamp": datetime.now(UTC).isoformat()}
    try:
        async with pool.acquire() as conn:
            await conn.fetchval("SELECT 1")
        checks["postgres"] = "connected"
    except Exception:
        checks["postgres"] = "error"
        checks["status"] = "degraded"

    try:
        await redis_client.ping()
        checks["redis"] = "connected"
    except Exception:
        checks["redis"] = "error"
        checks["status"] = "degraded"

    return checks


@app.get("/api/model-state")
async def model_state():
    """Fetch all current model states from Redis."""
    if redis_client is None:
        return []

    states = []
    cursor = 0
    while True:
        cursor, keys = await redis_client.scan(cursor, match="model_state:*", count=100)
        for key in keys:
            raw = await redis_client.get(key)
            if raw:
                states.append(json.loads(raw))
        if cursor == 0:
            break

    states.sort(key=lambda s: s.get("minutes_remaining", 999))
    return states


@app.get("/api/signals")
async def recent_signals(
    limit: int = 50,
    offset: int = 0,
    signal_type: str | None = None,
    acted_on: bool | None = None,
    hours: int | None = None,
):
    """Fetch recent signals from DB with filtering and pagination."""
    assert pool is not None

    query = """
        SELECT ticker, signal_type, direction, model_prob, market_price,
               edge, kelly_fraction, minutes_remaining, acted_on,
               rejection_reason, created_at
        FROM signals
        WHERE ($1::text IS NULL OR signal_type = $1)
          AND ($3::boolean IS NULL OR acted_on = $3)
          AND ($4::int IS NULL OR created_at >= NOW() - make_interval(hours => $4))
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $5
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, signal_type, limit, acted_on, hours, offset)

    return [dict(r) for r in rows]


@app.get("/api/positions")
async def positions():
    """Fetch open orders/positions from DB."""
    assert pool is not None

    query = """
        SELECT ticker, direction, size_cents, fill_price, status,
               latency_ms, created_at
        FROM orders
        WHERE status IN ('filled', 'pending')
        ORDER BY created_at DESC
        LIMIT 50
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query)

    return [dict(r) for r in rows]


@app.get("/api/daily-summary")
async def daily_summary():
    """Fetch today's trading summary."""
    assert pool is not None

    query = """
        SELECT date, total_signals, total_orders, wins, losses,
               net_pnl_cents, avg_edge
        FROM daily_summary
        ORDER BY date DESC
        LIMIT 7
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query)

    return [dict(r) for r in rows]


@app.get("/api/strategy-performance")
async def strategy_performance(strategy: str | None = None, days: int = 30):
    """Fetch per-strategy performance metrics."""
    assert pool is not None

    query = """
        SELECT strategy, date, signals_generated, signals_executed,
               win_count, loss_count, realized_pnl_cents,
               avg_edge, avg_kelly, brier_score
        FROM strategy_performance
        WHERE ($1::text IS NULL OR strategy = $1)
          AND date >= CURRENT_DATE - $2::int
        ORDER BY date DESC
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, strategy, days)

    return [dict(r) for r in rows]


@app.get("/api/calibration/brier")
async def calibration_brier(strategy: str | None = None, days: int = 30):
    """Brier score trend over time, per-strategy."""
    assert pool is not None

    query = """
        SELECT strategy, date, brier_score, signals_executed, win_count, loss_count
        FROM strategy_performance
        WHERE ($1::text IS NULL OR strategy = $1)
          AND date >= CURRENT_DATE - $2::int
          AND brier_score IS NOT NULL
        ORDER BY date ASC
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, strategy, days)

    return [dict(r) for r in rows]


@app.get("/api/calibration/station/{station}")
async def calibration_station(station: str):
    """Station-specific calibration parameters."""
    assert pool is not None

    query = """
        SELECT station, month, hour, sigma_10min, hrrr_bias_f, hrrr_skill,
               weight_physics, weight_hrrr, weight_trend, weight_climo,
               sample_size, updated_at
        FROM station_calibration
        WHERE station = $1
        ORDER BY month, hour
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, station)

    return [dict(r) for r in rows]


@app.get("/api/performance")
async def performance_metrics(days: int = 30):
    """P&L curve, cumulative returns, drawdown."""
    assert pool is not None

    query = """
        SELECT strategy, date, realized_pnl_cents, win_count, loss_count,
               signals_executed, avg_edge, brier_score
        FROM strategy_performance
        WHERE date >= CURRENT_DATE - $1::int
        ORDER BY date ASC
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, days)

    # Compute cumulative P&L and drawdown
    results = []
    cumulative: dict[str, int] = {"weather": 0, "crypto": 0}
    peak: dict[str, int] = {"weather": 0, "crypto": 0}
    for r in rows:
        d = dict(r)
        strat = d["strategy"]
        cumulative[strat] += d["realized_pnl_cents"]
        d["cumulative_pnl_cents"] = cumulative[strat]
        peak[strat] = max(peak[strat], cumulative[strat])
        d["drawdown_cents"] = peak[strat] - cumulative[strat]
        total = d["win_count"] + d["loss_count"]
        d["win_rate"] = d["win_count"] / total if total > 0 else None
        results.append(d)

    return results


@app.get("/api/system-status")
async def system_status():
    """Aggregated status data for the terminal status bar and top bar.

    Returns BTC price, feed health, daily P&L, position count, signal rate,
    Brier score, average latency, and paper mode status.
    """
    result: dict = {
        "btc_price": None,
        "feeds": {},
        "daily_pnl_cents": 0,
        "positions_count": 0,
        "signal_rate_1h": 0,
        "brier_score": None,
        "avg_latency_ms": None,
        "paper_mode": True,
    }

    try:
        # BTC price from Redis
        if redis_client:
            raw = await redis_client.get("crypto:coinbase")
            if raw:
                data = json.loads(raw)
                result["btc_price"] = data.get("spot") or data.get("price")

            # Feed health from Redis
            feeds = ["coinbase", "binance_spot", "binance_futures", "deribit", "kalshi_ws"]
            for feed in feeds:
                raw = await redis_client.get(f"feed:status:{feed}")
                if raw:
                    fdata = json.loads(raw)
                    result["feeds"][feed] = {
                        "score": fdata.get("score", 0),
                        "age_ms": fdata.get("age_ms", 0),
                    }
                else:
                    result["feeds"][feed] = {"score": 0, "age_ms": 0}
    except Exception:
        logger.warning("system_status_redis_error")

    try:
        if pool:
            async with pool.acquire() as conn:
                # Daily P&L: try strategy_performance first, fall back to orders
                row = await conn.fetchrow("""
                    SELECT COALESCE(SUM(realized_pnl_cents), 0) as pnl
                    FROM strategy_performance
                    WHERE date = CURRENT_DATE
                """)
                if row and row["pnl"]:
                    result["daily_pnl_cents"] = row["pnl"]
                else:
                    # Fall back to summing settled order P&L directly
                    row2 = await conn.fetchrow("""
                        SELECT COALESCE(SUM(pnl_cents), 0) as pnl
                        FROM orders
                        WHERE outcome IN ('win', 'loss')
                          AND settled_at >= CURRENT_DATE
                    """)
                    if row2:
                        result["daily_pnl_cents"] = row2["pnl"]

                # Open positions count (use order_state for granularity)
                count = await conn.fetchval("""
                    SELECT COUNT(*) FROM orders
                    WHERE status IN ('filled', 'pending')
                      AND outcome = 'pending'
                """)
                result["positions_count"] = count or 0

                # Signal rate (last hour)
                sig_count = await conn.fetchval("""
                    SELECT COUNT(*) FROM signals
                    WHERE created_at >= NOW() - INTERVAL '1 hour'
                """)
                result["signal_rate_1h"] = sig_count or 0

                # Latest Brier score
                brier_row = await conn.fetchrow("""
                    SELECT brier_score FROM strategy_performance
                    WHERE brier_score IS NOT NULL
                    ORDER BY date DESC
                    LIMIT 1
                """)
                if brier_row and brier_row["brier_score"] is not None:
                    result["brier_score"] = float(brier_row["brier_score"])

                # Average latency (last hour)
                lat_row = await conn.fetchrow("""
                    SELECT AVG(latency_ms)::int as avg_lat FROM orders
                    WHERE latency_ms IS NOT NULL
                      AND created_at >= NOW() - INTERVAL '1 hour'
                """)
                if lat_row and lat_row["avg_lat"] is not None:
                    result["avg_latency_ms"] = lat_row["avg_lat"]
    except Exception:
        logger.warning("system_status_db_error")

    return result


@app.get("/api/edge-decay")
async def edge_decay(days: int = 30, signal_type: str | None = None):
    """Edge vs minutes_remaining scatter data for analytics page."""
    assert pool is not None

    query = """
        SELECT s.edge, s.minutes_remaining, s.signal_type,
               COALESCE(o.outcome, 'no_order') as outcome
        FROM signals s
        LEFT JOIN orders o ON o.signal_id = s.id
        WHERE s.acted_on = true
          AND s.created_at >= NOW() - make_interval(days => $1)
          AND ($2::text IS NULL OR s.signal_type = $2)
        ORDER BY s.created_at DESC
        LIMIT 500
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, days, signal_type)

    return [
        {
            "edge": float(r["edge"]),
            "minutes_remaining": float(r["minutes_remaining"]),
            "outcome": r["outcome"],
            "signal_type": r["signal_type"],
        }
        for r in rows
    ]


@app.get("/api/calibration-curve")
async def calibration_curve(signal_type: str | None = None):
    """Predicted vs actual probability by bucket for calibration chart."""
    assert pool is not None

    query = """
        SELECT prob_bucket,
               AVG(model_prob) as predicted_avg,
               AVG(CASE WHEN actual_outcome THEN 1.0 ELSE 0.0 END) as actual_avg,
               COUNT(*) as count
        FROM calibration
        WHERE ($1::text IS NULL OR signal_type = $1)
        GROUP BY prob_bucket
        ORDER BY prob_bucket
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, signal_type)

    return [
        {
            "bucket": r["prob_bucket"],
            "predicted_avg": float(r["predicted_avg"]),
            "actual_avg": float(r["actual_avg"]),
            "count": r["count"],
        }
        for r in rows
    ]


@app.get("/api/risk-summary")
async def risk_summary():
    """Risk dashboard data: exposure, positions, feeds, kill switches."""
    assert pool is not None

    result: dict = {
        "positions": [],
        "position_count": 0,
        "max_positions": 10,
        "daily_pnl_cents": 0,
        "daily_loss_cents": 0,
        "max_daily_loss_cents": -5000,
        "exposure_cents": 0,
        "max_exposure_cents": 20000,
        "kill_switches": {"all": False, "crypto": False, "weather": False},
        "feeds": [],
        "crypto_health": 0.0,
        "weather_health": 0.0,
    }

    try:
        if pool:
            async with pool.acquire() as conn:
                # Positions with model context
                pos_rows = await conn.fetch("""
                    SELECT o.ticker, o.direction, o.size_cents, o.fill_price,
                           o.status, o.order_state, s.model_prob, s.market_price
                    FROM orders o
                    LEFT JOIN signals s ON o.signal_id = s.id
                    WHERE o.status IN ('filled', 'pending')
                      AND o.outcome = 'pending'
                    ORDER BY o.created_at DESC
                    LIMIT 20
                """)
                result["positions"] = [dict(r) for r in pos_rows]
                result["position_count"] = len(pos_rows)
                result["exposure_cents"] = sum(r["size_cents"] for r in pos_rows)

                # Daily P&L
                pnl_row = await conn.fetchrow("""
                    SELECT
                        COALESCE(SUM(realized_pnl_cents), 0) as pnl,
                        COALESCE(SUM(CASE WHEN realized_pnl_cents < 0
                                     THEN realized_pnl_cents ELSE 0 END), 0) as loss
                    FROM strategy_performance
                    WHERE date = CURRENT_DATE
                """)
                if pnl_row:
                    result["daily_pnl_cents"] = pnl_row["pnl"]
                    result["daily_loss_cents"] = pnl_row["loss"]
    except Exception:
        logger.warning("risk_summary_db_error")

    try:
        if redis_client:
            # Feed health
            feeds = ["coinbase", "binance_spot", "binance_futures", "deribit", "kalshi_ws"]
            for feed in feeds:
                raw = await redis_client.get(f"feed:status:{feed}")
                if raw:
                    fdata = json.loads(raw)
                    score = fdata.get("score", 0)
                    result["feeds"].append(
                        {
                            "name": feed,
                            "score": score,
                            "age_ms": fdata.get("age_ms", 0),
                            "healthy": score >= 0.5,
                        }
                    )
                else:
                    result["feeds"].append(
                        {
                            "name": feed,
                            "score": 0,
                            "age_ms": 0,
                            "healthy": False,
                        }
                    )

            # Strategy health: best crypto feed, kalshi for weather
            crypto_scores = [f["score"] for f in result["feeds"] if f["name"] in ("coinbase", "binance_spot")]
            result["crypto_health"] = max(crypto_scores) if crypto_scores else 0
            kalshi_feeds = [f["score"] for f in result["feeds"] if f["name"] == "kalshi_ws"]
            result["weather_health"] = min(kalshi_feeds) if kalshi_feeds else 0
    except Exception:
        logger.warning("risk_summary_redis_error")

    return result


@app.get("/api/decision-breakdown")
async def decision_breakdown(hours: int = 24):
    """Rejection reason breakdown from decision_log."""
    assert pool is not None

    query = """
        SELECT rejection_reason, COUNT(*) as count
        FROM decision_log
        WHERE outcome = 'rejected'
          AND rejection_reason IS NOT NULL
          AND created_at >= NOW() - make_interval(hours => $1)
        GROUP BY rejection_reason
        ORDER BY count DESC
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, hours)

    total = sum(r["count"] for r in rows) or 1
    return [{"reason": r["rejection_reason"], "count": r["count"], "pct": r["count"] / total} for r in rows]


@app.get("/api/orders")
async def orders_list(limit: int = 50, offset: int = 0, hours: int | None = None):
    """Fetch order history with pagination."""
    assert pool is not None

    query = """
        SELECT id, ticker, direction, order_type, size_cents, fill_price,
               status, order_state, outcome, pnl_cents, latency_ms, created_at, filled_at
        FROM orders
        WHERE ($1::int IS NULL OR created_at >= NOW() - make_interval(hours => $1))
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $3
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, hours, limit, offset)

    return [dict(r) for r in rows]


@app.get("/api/execution-stats")
async def execution_stats(hours: int = 24):
    """Execution quality metrics from orders table."""
    assert pool is not None

    async with pool.acquire() as conn:
        # Aggregate stats
        row = await conn.fetchrow(
            """
            SELECT
                COUNT(*) as total,
                COUNT(*) FILTER (WHERE order_state IN ('filled', 'partial_fill')) as filled,
                COUNT(*) FILTER (WHERE order_state IN ('cancelled', 'cancel_pending')) as cancelled,
                COUNT(*) FILTER (WHERE order_state = 'rejected') as failed,
                COUNT(*) FILTER (WHERE order_state IN ('pending', 'submitting', 'acknowledged')) as pending,
                AVG(latency_ms) FILTER (WHERE latency_ms IS NOT NULL) as avg_latency,
                PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY latency_ms)
                    FILTER (WHERE latency_ms IS NOT NULL) as p50_latency,
                PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms)
                    FILTER (WHERE latency_ms IS NOT NULL) as p95_latency,
                PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY latency_ms)
                    FILTER (WHERE latency_ms IS NOT NULL) as p99_latency,
                AVG(fill_price - s.market_price) FILTER (WHERE order_state IN ('filled', 'partial_fill'))
                    as avg_slippage
            FROM orders o
            LEFT JOIN signals s ON o.signal_id = s.id
            WHERE o.created_at >= NOW() - make_interval(hours => $1)
        """,
            hours,
        )

        # Latency histogram (10 buckets)
        lat_rows = await conn.fetch(
            """
            SELECT latency_ms FROM orders
            WHERE latency_ms IS NOT NULL
              AND created_at >= NOW() - make_interval(hours => $1)
            ORDER BY latency_ms
        """,
            hours,
        )

    total = row["total"] or 0
    filled = row["filled"] or 0
    cancelled = row["cancelled"] or 0

    # Build histogram from raw latencies
    latencies = [float(r["latency_ms"]) for r in lat_rows]
    histogram = []
    if latencies:
        lo, hi = min(latencies), max(latencies)
        if lo == hi:
            histogram = [len(latencies)]
        else:
            n_bins = 10
            bin_width = (hi - lo) / n_bins
            histogram = [0] * n_bins
            for v in latencies:
                idx = min(int((v - lo) / bin_width), n_bins - 1)
                histogram[idx] += 1

    return {
        "total_orders": total,
        "fill_rate": filled / total if total > 0 else 0,
        "cancel_rate": cancelled / total if total > 0 else 0,
        "avg_latency_ms": round(row["avg_latency"]) if row["avg_latency"] else None,
        "p50_latency_ms": round(row["p50_latency"]) if row["p50_latency"] else None,
        "p95_latency_ms": round(row["p95_latency"]) if row["p95_latency"] else None,
        "p99_latency_ms": round(row["p99_latency"]) if row["p99_latency"] else None,
        "avg_slippage": round(float(row["avg_slippage"]), 4) if row["avg_slippage"] else None,
        "state_counts": {
            "filled": filled,
            "cancelled": cancelled,
            "failed": row["failed"] or 0,
            "pending": row["pending"] or 0,
        },
        "latency_histogram": histogram,
        "latency_min": round(min(latencies)) if latencies else None,
        "latency_max": round(max(latencies)) if latencies else None,
    }


@app.get("/api/microstructure")
async def microstructure(hours: int = 1):
    """Recent microstructure adjustments from decision_log."""
    assert pool is not None

    components = ["micro_trade", "micro_spread", "micro_depth", "micro_vwap", "micro_momentum", "micro_vol_surge"]

    async with pool.acquire() as conn:
        # Latest values
        last_row = await conn.fetchrow("""
            SELECT micro_trade, micro_spread, micro_depth,
                   micro_vwap, micro_momentum, micro_vol_surge, micro_total
            FROM decision_log
            WHERE micro_total IS NOT NULL
            ORDER BY created_at DESC
            LIMIT 1
        """)

        # Averages over window
        avg_row = await conn.fetchrow(
            """
            SELECT AVG(micro_trade) as avg_trade,
                   AVG(micro_spread) as avg_spread,
                   AVG(micro_depth) as avg_depth,
                   AVG(micro_vwap) as avg_vwap,
                   AVG(micro_momentum) as avg_momentum,
                   AVG(micro_vol_surge) as avg_vol_surge,
                   AVG(micro_total) as avg_total
            FROM decision_log
            WHERE micro_total IS NOT NULL
              AND created_at >= NOW() - make_interval(hours => $1)
        """,
            hours,
        )

    result = []
    names = {
        "micro_trade": "trade_flow",
        "micro_spread": "spread_adj",
        "micro_depth": "depth_adj",
        "micro_vwap": "vwap_signal",
        "micro_momentum": "momentum",
        "micro_vol_surge": "vol_surge",
    }

    for col in components:
        last_val = float(last_row[col]) if last_row and last_row[col] is not None else None
        avg_col = f"avg_{col.replace('micro_', '')}"
        avg_val = float(avg_row[avg_col]) if avg_row and avg_row[avg_col] is not None else None
        result.append(
            {
                "component": names.get(col, col),
                "last": round(last_val, 4) if last_val is not None else None,
                "avg": round(avg_val, 4) if avg_val is not None else None,
            }
        )

    # Add total
    last_total = float(last_row["micro_total"]) if last_row and last_row["micro_total"] is not None else None
    avg_total = float(avg_row["avg_total"]) if avg_row and avg_row["avg_total"] is not None else None
    result.append(
        {
            "component": "TOTAL",
            "last": round(last_total, 4) if last_total is not None else None,
            "avg": round(avg_total, 4) if avg_total is not None else None,
        }
    )

    return result


@app.get("/api/station-summary")
async def station_summary():
    """Station overview: latest temp, active contract count, avg HRRR skill."""
    assert pool is not None

    async with pool.acquire() as conn:
        rows = await conn.fetch("""
            WITH latest_obs AS (
                SELECT DISTINCT ON (station)
                    station, temperature_f, observed_at
                FROM observations
                WHERE source = 'asos' AND temperature_f IS NOT NULL
                ORDER BY station, observed_at DESC
            ),
            active_contracts AS (
                SELECT station, COUNT(*) as cnt
                FROM contracts
                WHERE status = 'active'
                  AND settlement_time > NOW()
                GROUP BY station
            ),
            skill AS (
                SELECT station, AVG(hrrr_skill) as avg_skill
                FROM station_calibration
                WHERE hrrr_skill IS NOT NULL
                GROUP BY station
            )
            SELECT
                COALESCE(o.station, c.station, s.station) as station,
                o.temperature_f as latest_temp_f,
                COALESCE(c.cnt, 0) as active_contracts,
                s.avg_skill
            FROM latest_obs o
            FULL OUTER JOIN active_contracts c ON o.station = c.station
            FULL OUTER JOIN skill s ON COALESCE(o.station, c.station) = s.station
            WHERE COALESCE(o.station, c.station, s.station) IS NOT NULL
            ORDER BY COALESCE(c.cnt, 0) DESC, station
        """)

    return [
        {
            "station": r["station"],
            "latest_temp_f": float(r["latest_temp_f"]) if r["latest_temp_f"] else None,
            "active_contracts": r["active_contracts"],
            "avg_skill": float(r["avg_skill"]) if r["avg_skill"] else None,
        }
        for r in rows
    ]


@app.get("/api/settlement-outcomes")
async def settlement_outcomes(days: int = 7):
    """Recent settlement outcomes from daily_settlement_summary."""
    assert pool is not None

    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT station, obs_date, final_max_f, final_min_f,
                   contracts_settled
            FROM daily_settlement_summary
            WHERE obs_date >= CURRENT_DATE - $1::int
            ORDER BY obs_date DESC, station
        """,
            days,
        )

    return [dict(r) for r in rows]


@app.get("/api/hrrr-skill-matrix")
async def hrrr_skill_matrix():
    """HRRR skill scores by station and hour for heatmap display."""
    assert pool is not None

    async with pool.acquire() as conn:
        rows = await conn.fetch("""
            SELECT station, hour, hrrr_skill, sigma_10min, hrrr_bias_f,
                   sample_size
            FROM station_calibration
            WHERE hrrr_skill IS NOT NULL
            ORDER BY station, hour
        """)

    return [
        {
            "station": r["station"],
            "hour": r["hour"],
            "skill": float(r["hrrr_skill"]),
            "sigma": float(r["sigma_10min"]) if r["sigma_10min"] else None,
            "bias": float(r["hrrr_bias_f"]) if r["hrrr_bias_f"] else None,
            "samples": r["sample_size"],
        }
        for r in rows
    ]


@app.get("/api/calibration/stations")
async def calibration_stations():
    """List all stations with calibration data."""
    assert pool is not None

    async with pool.acquire() as conn:
        rows = await conn.fetch("""
            SELECT DISTINCT station FROM station_calibration
            ORDER BY station
        """)

    return [r["station"] for r in rows]


# ── SSE Event Stream ─────────────────────────────────────────────────


@app.get("/api/events")
async def event_stream(request: Request):
    """SSE endpoint for live updates via NATS subscription.

    Streams real-time signals from NATS, periodic model state from Redis,
    and system status for the terminal status bar.
    """
    cycle_count = 0

    async def generate():
        nonlocal cycle_count
        sub = None
        try:
            if nats_client:
                sub = await nats_client.subscribe("tradebot.signals.live")

            while True:
                if await request.is_disconnected():
                    break

                # Drain any pending NATS messages (non-blocking)
                if sub:
                    try:
                        while True:
                            msg = await asyncio.wait_for(sub.next_msg(), timeout=0.1)
                            yield {
                                "event": "signal",
                                "data": msg.data.decode(),
                            }
                    except (TimeoutError, nats.errors.TimeoutError):
                        pass

                # Model state snapshot every cycle (2s)
                states = await model_state()
                yield {
                    "event": "model_state",
                    "data": json.dumps(states),
                }

                # System status every 3rd cycle (~6s)
                cycle_count += 1
                if cycle_count % 3 == 0:
                    try:
                        status = await system_status()
                        yield {
                            "event": "system_status",
                            "data": json.dumps(status),
                        }
                    except Exception:
                        pass

                await asyncio.sleep(2)
        finally:
            if sub:
                await sub.unsubscribe()

    return EventSourceResponse(generate())


if __name__ == "__main__":
    uvicorn.run(
        "dashboard.app:app",
        host=settings.dashboard_host,
        port=settings.dashboard_port,
        reload=False,
    )

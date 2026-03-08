"""Terminal-style web dashboard for Tradebot.

FastAPI + Jinja2 + htmx + SSE. Dark theme, monospace, information-dense.
Reads model state from Redis, signal history from DB, live events from NATS.
"""

from __future__ import annotations

import asyncio
import json
from contextlib import asynccontextmanager
from datetime import datetime, timezone

import asyncpg
import nats
import redis.asyncio as aioredis
import structlog
import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import HTMLResponse
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


app = FastAPI(title="Tradebot Dashboard", lifespan=lifespan)
app.mount("/static", StaticFiles(directory="dashboard/static"), name="static")
templates = Jinja2Templates(directory="dashboard/templates")


@app.get("/", response_class=HTMLResponse)
async def index(request: Request):
    return templates.TemplateResponse("index.html", {"request": request})


@app.get("/api/health")
async def health():
    checks = {"status": "ok", "timestamp": datetime.now(timezone.utc).isoformat()}
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
async def recent_signals(limit: int = 50, signal_type: str | None = None):
    """Fetch recent signals from DB."""
    assert pool is not None

    query = """
        SELECT ticker, signal_type, direction, model_prob, market_price,
               edge, kelly_fraction, minutes_remaining, acted_on,
               rejection_reason, created_at
        FROM signals
        WHERE ($1::text IS NULL OR signal_type = $1)
        ORDER BY created_at DESC
        LIMIT $2
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(query, signal_type, limit)

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


@app.get("/calibration", response_class=HTMLResponse)
async def calibration_page(request: Request):
    return templates.TemplateResponse("calibration.html", {"request": request})


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
    cumulative = {"weather": 0, "crypto": 0}
    peak = {"weather": 0, "crypto": 0}
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


@app.get("/api/events")
async def event_stream(request: Request):
    """SSE endpoint for live updates via NATS subscription.

    Streams both real-time signals from NATS and periodic model state
    from Redis, so the dashboard gets push updates instead of polling.
    """

    async def generate():
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
                            msg = await asyncio.wait_for(
                                sub.next_msg(), timeout=0.1
                            )
                            yield {
                                "event": "signal",
                                "data": msg.data.decode(),
                            }
                    except (asyncio.TimeoutError, nats.errors.TimeoutError):
                        pass

                # Also send model state snapshot every cycle
                states = await model_state()
                yield {
                    "event": "model_state",
                    "data": json.dumps(states),
                }
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

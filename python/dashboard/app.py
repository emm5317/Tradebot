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


@asynccontextmanager
async def lifespan(app: FastAPI):
    global pool, redis_client
    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)
    redis_client = aioredis.from_url(settings.redis_url)
    logger.info("dashboard_started", port=settings.dashboard_port)
    yield
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


@app.get("/api/events")
async def event_stream(request: Request):
    """SSE endpoint for live model state updates."""

    async def generate():
        while True:
            if await request.is_disconnected():
                break

            states = await model_state()
            yield {
                "event": "model_state",
                "data": json.dumps(states),
            }
            await asyncio.sleep(2)

    return EventSourceResponse(generate())


if __name__ == "__main__":
    uvicorn.run(
        "dashboard.app:app",
        host=settings.dashboard_host,
        port=settings.dashboard_port,
        reload=False,
    )

"""Discord webhook notifications for trading events.

Sends formatted alerts on signal entry/exit, errors, and daily summaries.
Rate-limited to avoid Discord throttling.
"""

from __future__ import annotations

import asyncio
import time

import httpx
import structlog

from signals.types import SignalSchema

logger = structlog.get_logger()

# Minimum interval between messages to avoid Discord rate limits
_MIN_INTERVAL_SECONDS = 5.0


class DiscordNotifier:
    """Sends trading notifications to a Discord webhook."""

    def __init__(self, webhook_url: str | None = None) -> None:
        self.webhook_url = webhook_url
        self._last_sent: float = 0.0
        self._queue: asyncio.Queue[dict] = asyncio.Queue(maxsize=50)
        self._client: httpx.AsyncClient | None = None

    @property
    def enabled(self) -> bool:
        return bool(self.webhook_url)

    async def start(self) -> None:
        """Start the background sender loop."""
        if not self.enabled:
            return
        self._client = httpx.AsyncClient(timeout=httpx.Timeout(10.0))
        asyncio.create_task(self._sender_loop())

    async def notify_signal(self, signal: SignalSchema) -> None:
        """Queue a signal notification."""
        if not self.enabled:
            return

        color = 0x00FF00 if signal.direction == "yes" else 0xFF4444
        action = signal.action.value.upper()

        embed = {
            "embeds": [{
                "title": f"{action}: {signal.ticker}",
                "color": color,
                "fields": [
                    {"name": "Direction", "value": signal.direction.upper(), "inline": True},
                    {"name": "Edge", "value": f"{signal.edge:.1%}", "inline": True},
                    {"name": "Kelly", "value": f"{signal.kelly_fraction:.1%}", "inline": True},
                    {"name": "Model", "value": f"{signal.model_prob:.1%}", "inline": True},
                    {"name": "Market", "value": f"{signal.market_price:.1%}", "inline": True},
                    {"name": "Minutes", "value": f"{signal.minutes_remaining:.1f}", "inline": True},
                ],
            }],
        }
        await self._enqueue(embed)

    async def notify_error(self, error: str, context: dict | None = None) -> None:
        """Queue an error notification."""
        if not self.enabled:
            return

        description = error
        if context:
            details = ", ".join(f"{k}={v}" for k, v in context.items())
            description = f"{error}\n```{details}```"

        embed = {
            "embeds": [{
                "title": "Error",
                "description": description,
                "color": 0xFF0000,
            }],
        }
        await self._enqueue(embed)

    async def notify_daily_summary(self, summary: dict) -> None:
        """Queue a daily P&L summary notification."""
        if not self.enabled:
            return

        net_pnl = summary.get("net_pnl_cents", 0) / 100.0
        color = 0x00FF00 if net_pnl >= 0 else 0xFF4444

        embed = {
            "embeds": [{
                "title": "Daily Summary",
                "color": color,
                "fields": [
                    {"name": "Net P&L", "value": f"${net_pnl:+.2f}", "inline": True},
                    {"name": "Signals", "value": str(summary.get("total_signals", 0)), "inline": True},
                    {"name": "Orders", "value": str(summary.get("total_orders", 0)), "inline": True},
                    {"name": "Wins", "value": str(summary.get("wins", 0)), "inline": True},
                    {"name": "Losses", "value": str(summary.get("losses", 0)), "inline": True},
                ],
            }],
        }
        await self._enqueue(embed)

    async def _enqueue(self, payload: dict) -> None:
        try:
            self._queue.put_nowait(payload)
        except asyncio.QueueFull:
            logger.warning("discord_queue_full")

    async def _sender_loop(self) -> None:
        """Background loop that sends queued messages with rate limiting."""
        while True:
            payload = await self._queue.get()
            try:
                elapsed = time.monotonic() - self._last_sent
                if elapsed < _MIN_INTERVAL_SECONDS:
                    await asyncio.sleep(_MIN_INTERVAL_SECONDS - elapsed)

                if self._client and self.webhook_url:
                    resp = await self._client.post(self.webhook_url, json=payload)
                    if resp.status_code == 429:
                        retry_after = float(resp.headers.get("Retry-After", "5"))
                        await asyncio.sleep(retry_after)
                        await self._client.post(self.webhook_url, json=payload)

                self._last_sent = time.monotonic()
            except Exception:
                logger.exception("discord_send_failed")
            finally:
                self._queue.task_done()

    async def close(self) -> None:
        if self._client:
            await self._client.aclose()

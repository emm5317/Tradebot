"""Publishes trading signals to Redis for the Rust execution engine."""

import json
import redis


class SignalPublisher:
    """Sends signals to Redis pub/sub channel."""

    def __init__(self, redis_url: str = "redis://localhost:6379"):
        self.client = redis.from_url(redis_url)
        self.channel = "tradebot:signals"

    def publish(self, signal: dict) -> None:
        """Publish a signal to the Redis channel."""
        self.client.publish(self.channel, json.dumps(signal))

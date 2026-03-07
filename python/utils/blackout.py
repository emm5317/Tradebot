"""Blackout period enforcement – prevents trading during known anomalous events."""

import json
from datetime import datetime
from pathlib import Path


def load_blackout_events(path: str = "config/blackout_events.json") -> list[dict]:
    """Load blackout event definitions from JSON."""
    with open(path) as f:
        return json.load(f)


def is_blackout(now: datetime | None = None, events: list[dict] | None = None) -> bool:
    """Return True if current time falls within a blackout window."""
    # TODO: check current time against event windows
    return False

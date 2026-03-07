"""Mesonet data client for real-time and near-real-time weather observations."""

import httpx


class MesonetClient:
    """Fetches data from various mesonet networks."""

    def __init__(self):
        self.http = httpx.Client(timeout=30)

    def get_latest(self, station: str) -> dict | None:
        """Get the latest observation for a station."""
        # TODO: fetch from mesonet API
        return None

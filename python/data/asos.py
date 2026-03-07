"""Iowa State ASOS (Automated Surface Observing System) data client."""

import httpx


class ASOSClient:
    """Fetches historical hourly weather observations from Iowa State ASOS."""

    BASE_URL = "https://mesonet.agron.iastate.edu/cgi-bin/request/asos.py"

    def __init__(self):
        self.http = httpx.Client(timeout=30)

    def fetch(self, station: str, start: str, end: str) -> list[dict]:
        """Fetch observations for a station between start and end dates."""
        # TODO: build params, request, parse CSV
        return []

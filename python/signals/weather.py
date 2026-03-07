"""Weather-based signal generation for temperature / precipitation contracts."""


class WeatherSignal:
    """Generates trading signals from weather forecast data."""

    def __init__(self, station_id: str):
        self.station_id = station_id

    def generate(self) -> dict | None:
        """Produce a signal dict or None if no edge detected."""
        # TODO: compare forecast vs. market implied probability
        return None

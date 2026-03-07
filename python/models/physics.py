"""Physics-based weather models (temperature interpolation, heat index, etc.)."""

import numpy as np


def interpolate_temperature(
    t_min: float, t_max: float, hour: int, sunrise: int = 6, sunset: int = 18
) -> float:
    """Estimate temperature at a given hour using sinusoidal interpolation."""
    # Simplified model: sinusoidal curve between min/max
    t_range = t_max - t_min
    t_mid = (t_min + t_max) / 2
    phase = 2 * np.pi * (hour - 14) / 24  # peak at ~2pm
    return t_mid + (t_range / 2) * np.cos(phase)


def heat_index(temp_f: float, rh: float) -> float:
    """Compute heat index from temperature (°F) and relative humidity (%)."""
    if temp_f < 80:
        return temp_f
    hi = (
        -42.379
        + 2.04901523 * temp_f
        + 10.14333127 * rh
        - 0.22475541 * temp_f * rh
        - 0.00683783 * temp_f**2
        - 0.05481717 * rh**2
        + 0.00122874 * temp_f**2 * rh
        + 0.00085282 * temp_f * rh**2
        - 0.00000199 * temp_f**2 * rh**2
    )
    return hi

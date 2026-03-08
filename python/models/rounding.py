"""METAR C→F rounding ambiguity model.

METAR reports temperature in whole Celsius. The NWS CLI report converts to
Fahrenheit and rounds. Near threshold boundaries, the Celsius integer can
map to a range of Fahrenheit values that straddle the strike, creating
settlement ambiguity.

Example: METAR reports 7°C → could be 6.5-7.4°C → 43.7-45.3°F.
If strike is 45°F, settlement outcome is ambiguous.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class RoundingResult:
    """Result of rounding ambiguity analysis."""

    min_f: float          # lowest possible Fahrenheit from this METAR reading
    max_f: float          # highest possible Fahrenheit from this METAR reading
    reported_f: float     # nominal F conversion (round(C * 9/5 + 32))
    is_ambiguous: bool    # True if strike falls within [min_f, max_f]
    ambiguity_band: float # max_f - min_f (always 1.8°F for whole-degree C)


def compute_rounding_uncertainty(
    metar_temp_c: int,
    threshold_f: float,
) -> RoundingResult:
    """Compute the Fahrenheit range and ambiguity for a METAR Celsius reading.

    METAR whole-degree Celsius represents the range [C-0.5, C+0.5).
    Converting that range to Fahrenheit:
      min_f = (C - 0.5) * 9/5 + 32
      max_f = (C + 0.5) * 9/5 + 32

    The CLI reports the rounded Fahrenheit value: round(C * 9/5 + 32).

    Args:
        metar_temp_c: Whole-degree Celsius from METAR
        threshold_f: Contract strike threshold in Fahrenheit

    Returns:
        RoundingResult with range, nominal value, and ambiguity flag.
    """
    c = float(metar_temp_c)

    # True temperature range represented by this METAR reading
    min_c = c - 0.5
    max_c = c + 0.5  # exclusive, but we use inclusive for safety

    min_f = min_c * 9.0 / 5.0 + 32.0
    max_f = max_c * 9.0 / 5.0 + 32.0

    # Nominal (what CLI would report)
    reported_f = round(c * 9.0 / 5.0 + 32.0)

    # Ambiguous if the threshold falls within the possible range
    is_ambiguous = min_f <= threshold_f <= max_f

    return RoundingResult(
        min_f=min_f,
        max_f=max_f,
        reported_f=float(reported_f),
        is_ambiguous=is_ambiguous,
        ambiguity_band=max_f - min_f,
    )


def celsius_to_cli_fahrenheit(temp_c: float) -> int:
    """Convert Celsius to Fahrenheit as the NWS CLI would report it.

    The CLI rounds to the nearest whole Fahrenheit degree.
    """
    return round(temp_c * 9.0 / 5.0 + 32.0)


def fahrenheit_to_threshold_celsius(threshold_f: float) -> tuple[float, float]:
    """Given a Fahrenheit threshold, return the Celsius range that maps to it.

    Returns (min_c, max_c) where any METAR reading in this range could
    produce a CLI value at or above the threshold after rounding.
    """
    # A CLI value of threshold_f comes from C values where:
    # round(C * 9/5 + 32) >= threshold_f
    # C * 9/5 + 32 >= threshold_f - 0.5
    # C >= (threshold_f - 32.5) * 5/9
    min_c = (threshold_f - 32.5) * 5.0 / 9.0
    max_c = (threshold_f - 31.5) * 5.0 / 9.0
    return min_c, max_c

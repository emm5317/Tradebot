"""Settlement-aware weather fair-value engine.

Replaces naive Gaussian diffusion with a model that understands:
1. NWS CLI settlement mechanics (max/min for the day in local standard time)
2. Running max/min tracking with "lock" detection (if already exceeded, P≈1)
3. METAR C→F rounding ambiguity near thresholds
4. HRRR forecast blending for remaining-day excursion probability
5. 6-hourly METAR max/min groups as authoritative inputs
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Literal

import structlog

from models.physics import StationCalibration, fast_norm_cdf
from models.rounding import compute_rounding_uncertainty

logger = structlog.get_logger()


@dataclass
class SourceConflict:
    """Detected conflict or outage between METAR and HRRR sources."""

    metar_hrrr_conflict: bool = False   # |METAR - nearest HRRR| > 3F
    metar_missing: bool = False
    hrrr_missing: bool = False
    sigma_multiplier: float = 1.0       # 1.0 normal, 1.5 conflict, 1.25 METAR outage


def detect_source_conflict(
    current_temp_f: float | None,
    hrrr_forecast_temps_f: list[float] | None,
    metar_temp_c: int | None,
) -> SourceConflict:
    """Detect conflicts between METAR observations and HRRR forecasts."""
    hrrr_missing = hrrr_forecast_temps_f is None or len(hrrr_forecast_temps_f) == 0
    metar_missing = metar_temp_c is None

    conflict = SourceConflict(
        hrrr_missing=hrrr_missing,
        metar_missing=metar_missing,
    )

    if not metar_missing and not hrrr_missing and current_temp_f is not None:
        # Find nearest HRRR forecast to current temp
        nearest_hrrr = min(hrrr_forecast_temps_f, key=lambda t: abs(t - current_temp_f))
        if abs(current_temp_f - nearest_hrrr) > 3.0:
            conflict.metar_hrrr_conflict = True

    # Set sigma multiplier
    if conflict.metar_hrrr_conflict:
        conflict.sigma_multiplier = 1.5
    elif conflict.metar_missing:
        conflict.sigma_multiplier = 1.25

    return conflict


@dataclass
class WeatherState:
    """Running state for a weather contract's settlement day."""

    station: str
    obs_date: str                       # ISO date string
    contract_type: Literal["weather_max", "weather_min"]
    strike_f: float
    running_max_f: float | None = None
    running_min_f: float | None = None
    obs_count: int = 0
    locked: bool = False                # True if outcome already determined
    lock_direction: str | None = None   # "above" or "below" when locked
    rounding_ambiguous: bool = False
    last_metar_c: int | None = None


@dataclass
class WeatherFairValue:
    """Output of the weather fair-value engine."""

    probability: float              # P(contract settles YES)
    confidence: float               # 0-1 confidence in estimate
    already_locked: bool            # outcome already determined
    lock_direction: str | None      # "above"/"below" if locked
    rounding_ambiguous: bool        # near C→F rounding boundary
    uncertainty_band: tuple[float, float]  # (low_p, high_p)
    components: dict[str, float] = field(default_factory=dict)


def compute_weather_fair_value(
    *,
    contract_type: str,
    strike_f: float,
    current_temp_f: float,
    minutes_remaining: float,
    sigma_per_10min: float = 0.3,
    state: WeatherState | None = None,
    metar_temp_c: int | None = None,
    hrrr_forecast_temps_f: list[float] | None = None,
    max_temp_6hr_f: float | None = None,
    min_temp_6hr_f: float | None = None,
    recent_temps: list[float] | None = None,
    climo_prob: float | None = None,
    station_cal: StationCalibration | None = None,
) -> WeatherFairValue:
    """Compute settlement-aware probability for a weather contract.

    This replaces compute_ensemble_probability for weather contracts,
    incorporating settlement mechanics that the generic model ignores.

    Args:
        contract_type: "weather_max" or "weather_min"
        strike_f: Contract threshold in Fahrenheit
        current_temp_f: Current observed temperature in Fahrenheit
        minutes_remaining: Minutes until settlement
        sigma_per_10min: Temperature volatility
        state: Running day state (updated in place)
        metar_temp_c: Whole-degree Celsius from latest METAR
        hrrr_forecast_temps_f: Remaining-day HRRR forecast temps
        max_temp_6hr_f: 6-hourly max from METAR 1xxxx group
        min_temp_6hr_f: 6-hourly min from METAR 2xxxx group
        recent_temps: Recent temperature readings for trend
        climo_prob: Climatological probability (from existing climo table)
    """
    components: dict[str, float] = {}

    # --- Step 0: Source conflict/outage detection (Phase 4.6) ---
    conflict = detect_source_conflict(current_temp_f, hrrr_forecast_temps_f, metar_temp_c)

    if conflict.metar_hrrr_conflict:
        logger.warning(
            "metar_hrrr_conflict",
            current_temp_f=current_temp_f,
            contract_type=contract_type,
        )
        # Inflate sigma for conflict
        sigma_per_10min = sigma_per_10min * conflict.sigma_multiplier
    elif conflict.metar_missing and not conflict.hrrr_missing:
        # METAR outage with HRRR available — moderate sigma inflation
        sigma_per_10min = sigma_per_10min * conflict.sigma_multiplier

    # --- Step 1: Update running max/min state ---
    if state is not None:
        state.obs_count += 1
        if state.running_max_f is None or current_temp_f > state.running_max_f:
            state.running_max_f = current_temp_f
        if state.running_min_f is None or current_temp_f < state.running_min_f:
            state.running_min_f = current_temp_f

        # Incorporate 6-hourly groups (these are authoritative)
        if max_temp_6hr_f is not None:
            if state.running_max_f is None or max_temp_6hr_f > state.running_max_f:
                state.running_max_f = max_temp_6hr_f
        if min_temp_6hr_f is not None:
            if state.running_min_f is None or min_temp_6hr_f < state.running_min_f:
                state.running_min_f = min_temp_6hr_f

    # --- Step 2: Check for locked outcome ---
    locked = False
    lock_direction: str | None = None

    if contract_type == "weather_max":
        # "High temperature above X°F" — if running max already >= strike, locked YES
        running_val = state.running_max_f if state else current_temp_f
        if running_val is not None and running_val >= strike_f:
            locked = True
            lock_direction = "above"
    elif contract_type == "weather_min":
        # "Low temperature below X°F" — if running min already <= strike, locked YES
        running_val = state.running_min_f if state else current_temp_f
        if running_val is not None and running_val <= strike_f:
            locked = True
            lock_direction = "below"

    if state is not None:
        state.locked = locked
        state.lock_direction = lock_direction

    if locked:
        return WeatherFairValue(
            probability=0.99,  # not 1.0 to leave tiny margin for data correction
            confidence=0.95,
            already_locked=True,
            lock_direction=lock_direction,
            rounding_ambiguous=False,
            uncertainty_band=(0.95, 1.0),
            components={"locked": 1.0},
        )

    # --- Step 3: Rounding ambiguity check (Phase 4.7: boundary probability) ---
    rounding_ambiguous = False
    rounding_result = None
    if metar_temp_c is not None:
        rounding_result = compute_rounding_uncertainty(metar_temp_c, strike_f)
        rounding_ambiguous = rounding_result.is_ambiguous
        if state is not None:
            state.rounding_ambiguous = rounding_ambiguous
            state.last_metar_c = metar_temp_c

    # --- Step 3b: Apply station-specific sigma if available ---
    if station_cal is not None:
        sigma_per_10min = station_cal.sigma_10min

    # --- Step 4: Physics model (Gaussian diffusion) ---
    if contract_type == "weather_max":
        # P(max exceeds strike in remaining time)
        # Current max is running_max_f, need it to reach strike_f
        reference = state.running_max_f if (state and state.running_max_f) else current_temp_f
        delta = strike_f - reference
    else:
        # P(min drops below strike in remaining time)
        # Current min is running_min_f, need it to reach strike_f
        reference = state.running_min_f if (state and state.running_min_f) else current_temp_f
        delta = reference - strike_f  # positive = currently above strike (need to drop)

    if minutes_remaining <= 0:
        p_physics = 1.0 if delta <= 0 else 0.0
    else:
        sigma_total = sigma_per_10min * math.sqrt(minutes_remaining / 10.0)
        if sigma_total <= 0:
            p_physics = 1.0 if delta <= 0 else 0.0
        else:
            z = delta / sigma_total
            p_terminal = 1.0 - fast_norm_cdf(z)
            # Reflection principle: for barrier options (max/min contracts),
            # P(process ever crosses barrier) = 2 * P(terminal > barrier)
            # when process hasn't yet crossed.
            if delta > 0:
                p_physics = min(2.0 * p_terminal, 1.0)
            else:
                p_physics = p_terminal

    components["physics"] = p_physics

    # --- Step 5: HRRR forecast excursion probability ---
    # Apply HRRR bias correction if station calibration available
    if station_cal is not None and hrrr_forecast_temps_f:
        hrrr_forecast_temps_f = [
            t - station_cal.hrrr_bias_f for t in hrrr_forecast_temps_f
        ]

    p_hrrr: float | None = None
    if hrrr_forecast_temps_f and len(hrrr_forecast_temps_f) > 0:
        if contract_type == "weather_max":
            forecast_max = max(hrrr_forecast_temps_f)
            # Simple: if forecast max exceeds strike, high probability
            forecast_delta = strike_f - forecast_max
            p_hrrr = 1.0 - fast_norm_cdf(forecast_delta / max(sigma_per_10min * 2, 0.1))
        else:
            forecast_min = min(hrrr_forecast_temps_f)
            forecast_delta = forecast_min - strike_f
            p_hrrr = 1.0 - fast_norm_cdf(forecast_delta / max(sigma_per_10min * 2, 0.1))

        components["hrrr"] = p_hrrr

    # --- Step 6: Trend component ---
    p_trend = 0.5
    if recent_temps and len(recent_temps) >= 5:
        n = len(recent_temps)
        sum_x = sum_y = sum_xy = sum_xx = 0.0
        for i, temp in enumerate(recent_temps):
            x = float(i)
            sum_x += x
            sum_y += temp
            sum_xy += x * temp
            sum_xx += x * x
        denom = n * sum_xx - sum_x * sum_x
        if abs(denom) > 1e-12:
            b = (n * sum_xy - sum_x * sum_y) / denom
            a = (sum_y - b * sum_x) / n
            extrapolated = a + b * (n - 1 + minutes_remaining)
            trend_sigma = sigma_per_10min * math.sqrt(minutes_remaining / 10.0) * 0.8

            if contract_type == "weather_max":
                tdelta = strike_f - max(extrapolated, reference)
            else:
                tdelta = min(extrapolated, reference) - strike_f

            if trend_sigma > 0:
                p_trend = 1.0 - fast_norm_cdf(tdelta / trend_sigma)

    components["trend"] = p_trend

    # --- Step 7: Blend components ---
    p_climo = climo_prob if climo_prob is not None else 0.5
    components["climo"] = p_climo

    if station_cal is not None:
        # Use station-specific weights
        w_phys, w_hrrr, w_trend, w_climo = station_cal.weights

        # Scale HRRR weight by skill (low skill → redistribute to others)
        if p_hrrr is not None:
            effective_hrrr_w = w_hrrr * station_cal.hrrr_skill
            redistrib = w_hrrr - effective_hrrr_w
            total_other = w_phys + w_trend + w_climo
            if total_other > 0:
                scale = 1.0 + redistrib / total_other
            else:
                scale = 1.0
            probability = (
                w_phys * scale * p_physics
                + effective_hrrr_w * p_hrrr
                + w_trend * scale * p_trend
                + w_climo * scale * p_climo
            )
        else:
            # No HRRR: redistribute HRRR weight proportionally
            total_other = w_phys + w_trend + w_climo
            if total_other > 0:
                scale = (w_phys + w_hrrr + w_trend + w_climo) / total_other
            else:
                scale = 1.0
            probability = (
                w_phys * scale * p_physics
                + w_trend * scale * p_trend
                + w_climo * scale * p_climo
            )
    elif p_hrrr is not None:
        probability = (
            0.35 * p_physics
            + 0.25 * p_hrrr
            + 0.20 * p_trend
            + 0.20 * p_climo
        )
    else:
        probability = (
            0.45 * p_physics
            + 0.25 * p_trend
            + 0.30 * p_climo
        )

    # --- Step 7b: Rounding component blending (Phase 4.7) ---
    if rounding_ambiguous and rounding_result is not None:
        # Blend boundary probability into ensemble with 10% weight
        p_rounding = rounding_result.boundary_probability
        components["rounding"] = p_rounding
        # Reduce physics weight by 10%, add rounding at 10%
        probability = probability * 0.90 + p_rounding * 0.10

    probability = max(0.01, min(0.99, probability))

    # --- Step 8: Confidence and uncertainty ---
    # Lower confidence when:
    # - rounding is ambiguous
    # - few observations
    # - no HRRR data
    confidence = 0.7
    if state and state.obs_count > 10:
        confidence += 0.1
    if p_hrrr is not None:
        confidence += 0.1
    if rounding_ambiguous:
        confidence -= 0.2
    # Phase 4.7: safe zone bonus
    if rounding_result is not None and rounding_result.safe_zone:
        confidence += 0.05
    confidence = max(0.1, min(1.0, confidence))

    # Uncertainty band
    band_width = 0.15 * (1.0 - confidence)
    if rounding_ambiguous:
        band_width += 0.05
    uncertainty_band = (
        max(0.0, probability - band_width),
        min(1.0, probability + band_width),
    )

    return WeatherFairValue(
        probability=probability,
        confidence=confidence,
        already_locked=False,
        lock_direction=None,
        rounding_ambiguous=rounding_ambiguous,
        uncertainty_band=uncertainty_band,
        components=components,
    )

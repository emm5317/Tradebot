"""BE-4.1: Weather Physics Model — Gaussian diffusion + ensemble.

Uses math.erfc instead of scipy.stats.norm.cdf for 10-50x faster scalar
evaluation on the hot path. No numpy dependency — trend extrapolation
uses hand-rolled least squares for n < 100 data points.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field

import structlog

logger = structlog.get_logger()

# Default sigma (°F per sqrt(10 min)) when no station-specific data available
_DEFAULT_SIGMA = 0.3


def fast_norm_cdf(x: float) -> float:
    """Standard normal CDF using erfc. ~50x faster than scipy.stats.norm.cdf."""
    return 0.5 * math.erfc(-x / math.sqrt(2.0))


def compute_weather_probability(
    current_temp_f: float,
    threshold_f: float,
    minutes_remaining: float,
    sigma_per_10min: float = _DEFAULT_SIGMA,
    contract_type: str = "weather_max",
) -> float:
    """P(temp exceeds threshold at settlement) using Gaussian diffusion.

    For max/min contracts, applies the reflection principle correction:
    P(max X(t) > K) = 2 * P(X(T) > K) when X(0) < K (Brownian motion).
    This corrects the systematic underestimation of excursion probability.

    Args:
        current_temp_f: Current observed temperature in °F.
        threshold_f: Contract threshold temperature in °F.
        minutes_remaining: Minutes until settlement.
        sigma_per_10min: Temperature standard deviation per sqrt(10 min).
        contract_type: "weather_max" or "weather_min" for reflection correction.

    Returns:
        Probability [0, 1] that temperature will be >= threshold at settlement.
    """
    if minutes_remaining <= 0:
        return 1.0 if current_temp_f >= threshold_f else 0.0

    delta = threshold_f - current_temp_f
    sigma_total = sigma_per_10min * math.sqrt(minutes_remaining / 10.0)

    if sigma_total <= 0:
        return 1.0 if current_temp_f >= threshold_f else 0.0

    z = delta / sigma_total
    p_terminal = 1.0 - fast_norm_cdf(z)

    # Reflection principle: for max contracts where current < strike,
    # or min contracts where current > strike, the probability of the
    # process *ever* exceeding the barrier is 2x the terminal probability.
    # P(max B(t) > K, t in [0,T]) = 2 * P(B(T) > K) when B(0) < K.
    if contract_type == "weather_max" and current_temp_f < threshold_f:
        p_excursion = min(2.0 * p_terminal, 1.0)
        return p_excursion
    elif contract_type == "weather_min" and current_temp_f > threshold_f:
        p_excursion = min(2.0 * p_terminal, 1.0)
        return p_excursion

    return p_terminal


def climatological_probability(
    station: str,
    hour: int,
    month: int,
    threshold_f: float,
    current_temp_f: float,
    climo_table: dict[tuple[str, int, int], tuple[float, float]] | None = None,
) -> float:
    """Historical win rate for similar setups from observations data.

    Args:
        station: ASOS station code.
        hour: Hour of day (0-23).
        month: Month (1-12).
        threshold_f: Contract threshold.
        current_temp_f: Current temperature.
        climo_table: Mapping of (station, hour, month) -> (mean_temp, std_temp).

    Returns:
        Probability based on climatological distribution.
    """
    if climo_table is None:
        # No historical data — return uninformative prior
        return 0.5

    key = (station, hour, month)
    stats = climo_table.get(key)
    if stats is None:
        return 0.5

    climo_mean, climo_std = stats
    if climo_std <= 0:
        return 1.0 if climo_mean >= threshold_f else 0.0

    # Blend current observation with climatological mean
    # Weight current observation more when it's recent
    blended_temp = 0.7 * current_temp_f + 0.3 * climo_mean
    z = (threshold_f - blended_temp) / climo_std
    return 1.0 - fast_norm_cdf(z)


def trend_extrapolation_probability(
    recent_temps: list[float],
    threshold_f: float,
    minutes_remaining: float,
    sigma_per_10min: float = _DEFAULT_SIGMA,
) -> float:
    """Linear trend extrapolation from recent observations.

    Fits a simple least-squares line to recent temperature readings
    (assumed 1-minute spacing), extrapolates to settlement time, then
    applies Gaussian uncertainty around the extrapolated value.

    Hand-rolled least squares — no numpy for n < 100.
    """
    n = len(recent_temps)
    if n < 5:
        # Not enough data for trend — return uninformative
        return 0.5

    # Least squares: y = a + b*x, x = minute index
    sum_x = 0.0
    sum_y = 0.0
    sum_xy = 0.0
    sum_xx = 0.0

    for i, temp in enumerate(recent_temps):
        x = float(i)
        sum_x += x
        sum_y += temp
        sum_xy += x * temp
        sum_xx += x * x

    denom = n * sum_xx - sum_x * sum_x
    if abs(denom) < 1e-12:
        # All x values identical (shouldn't happen with minute spacing)
        return 0.5

    b = (n * sum_xy - sum_x * sum_y) / denom  # slope (°F per minute)
    a = (sum_y - b * sum_x) / n  # intercept

    # Extrapolate to settlement
    extrapolated_temp = a + b * (n - 1 + minutes_remaining)

    # Apply Gaussian uncertainty — but reduce sigma because we have trend info
    # Trend-adjusted sigma is smaller than raw sigma
    trend_sigma = sigma_per_10min * math.sqrt(minutes_remaining / 10.0) * 0.8

    if trend_sigma <= 0:
        return 1.0 if extrapolated_temp >= threshold_f else 0.0

    z = (threshold_f - extrapolated_temp) / trend_sigma
    return 1.0 - fast_norm_cdf(z)


def compute_ensemble_probability(
    current_temp_f: float,
    threshold_f: float,
    minutes_remaining: float,
    station: str,
    hour: int,
    month: int,
    recent_temps: list[float] | None = None,
    sigma_table: dict[tuple[str, int, int], float] | None = None,
    climo_table: dict[tuple[str, int, int], tuple[float, float]] | None = None,
    weights: tuple[float, float, float] | None = None,
    contract_type: str = "weather_max",
) -> tuple[float, float, float, float]:
    """Ensemble of physics, climatology, and trend models.

    Args:
        current_temp_f: Current observed temperature.
        threshold_f: Contract threshold.
        minutes_remaining: Minutes until settlement.
        station: ASOS station code.
        hour: Hour of day (0-23).
        month: Month (1-12).
        recent_temps: Last ~60 minutes of temperature readings.
        sigma_table: Per-(station, hour, month) sigma values.
        climo_table: Per-(station, hour, month) climatological stats.
        weights: (physics, climo, trend) weights. Loaded from calibration
                 table at startup, defaults to (0.5, 0.25, 0.25).
        contract_type: "weather_max" or "weather_min" for reflection correction.

    Returns:
        Tuple of (ensemble_prob, physics_prob, climo_prob, trend_prob).
    """
    if weights is None:
        weights = (0.5, 0.25, 0.25)

    # Station-specific sigma from historical observations
    sigma = _DEFAULT_SIGMA
    if sigma_table is not None:
        sigma = sigma_table.get((station, hour, month), _DEFAULT_SIGMA)

    # 1. Physics model (with reflection principle for max/min)
    p_physics = compute_weather_probability(
        current_temp_f, threshold_f, minutes_remaining, sigma, contract_type
    )

    # 2. Climatological prior
    p_climo = climatological_probability(
        station, hour, month, threshold_f, current_temp_f, climo_table
    )

    # 3. Trend extrapolation
    p_trend = trend_extrapolation_probability(
        recent_temps or [], threshold_f, minutes_remaining, sigma
    )

    # Weighted ensemble
    w1, w2, w3 = weights
    p_ensemble = w1 * p_physics + w2 * p_climo + w3 * p_trend

    # Clamp to valid probability range
    p_ensemble = max(0.0, min(1.0, p_ensemble))

    return p_ensemble, p_physics, p_climo, p_trend


async def build_sigma_table(
    pool,
) -> dict[tuple[str, int, int], float]:
    """Build per-(station, hour, month) sigma table from observations.

    Computes the standard deviation of temperature changes per 10-minute
    window from historical ASOS observations. Run at startup and periodically.
    """
    query = """
        SELECT
            station,
            EXTRACT(HOUR FROM observed_at)::int AS hour,
            EXTRACT(MONTH FROM observed_at)::int AS month,
            STDDEV(temperature_f) AS sigma
        FROM observations
        WHERE source = 'asos'
          AND temperature_f IS NOT NULL
          AND observed_at > now() - interval '90 days'
        GROUP BY station, hour, month
        HAVING COUNT(*) >= 30
    """

    async with pool.acquire() as conn:
        rows = await conn.fetch(query)

    table: dict[tuple[str, int, int], float] = {}
    for row in rows:
        station = row["station"]
        hour = row["hour"]
        month = row["month"]
        sigma = row["sigma"]
        if station and sigma and sigma > 0:
            # Convert raw temp stddev to sigma_per_10min scale.
            # Raw stddev is across all 1-minute observations in the hour.
            # With ~60 obs/hour and temperature autocorrelation ~0.98 at
            # 1-minute lag, effective independent samples ≈ 60/(1+2*sum(rho^k))
            # ≈ 60/100 ≈ 0.6. Empirically, 10-min temp changes have stddev
            # of ~0.07x the hourly stddev (vs 0.1x without autocorrelation).
            table[(station, hour, month)] = float(sigma) * 0.07

    logger.info("sigma_table_built", entries=len(table))
    return table


async def build_climo_table(
    pool,
) -> dict[tuple[str, int, int], tuple[float, float]]:
    """Build climatological mean/stddev table from observations.

    Returns (mean_temp, std_temp) per (station, hour, month).
    """
    query = """
        SELECT
            station,
            EXTRACT(HOUR FROM observed_at)::int AS hour,
            EXTRACT(MONTH FROM observed_at)::int AS month,
            AVG(temperature_f) AS mean_temp,
            STDDEV(temperature_f) AS std_temp
        FROM observations
        WHERE source = 'asos'
          AND temperature_f IS NOT NULL
          AND observed_at > now() - interval '1 year'
        GROUP BY station, hour, month
        HAVING COUNT(*) >= 30
    """

    async with pool.acquire() as conn:
        rows = await conn.fetch(query)

    table: dict[tuple[str, int, int], tuple[float, float]] = {}
    for row in rows:
        station = row["station"]
        hour = row["hour"]
        month = row["month"]
        mean_t = row["mean_temp"]
        std_t = row["std_temp"]
        if station and mean_t is not None and std_t is not None and std_t > 0:
            table[(station, hour, month)] = (float(mean_t), float(std_t))

    logger.info("climo_table_built", entries=len(table))
    return table


@dataclass
class StationCalibration:
    """Per-(station, month, hour) calibration parameters."""

    sigma_10min: float = 0.3
    hrrr_bias_f: float = 0.0
    hrrr_skill: float = 0.5
    rounding_bias: float = 0.0
    weights: tuple[float, float, float, float] = (0.45, 0.25, 0.20, 0.10)
    # weights order: (physics, hrrr, trend, climo)


async def build_station_calibration(
    pool,
) -> dict[tuple[str, int, int], StationCalibration]:
    """Load calibration from station_calibration table."""
    query = """
        SELECT station, month, hour,
               sigma_10min, hrrr_bias_f, hrrr_skill, rounding_bias,
               weight_physics, weight_hrrr, weight_trend, weight_climo
        FROM station_calibration
        WHERE sample_size >= 10
    """

    try:
        async with pool.acquire() as conn:
            rows = await conn.fetch(query)
    except Exception:
        logger.warning("station_calibration_load_failed", exc_info=True)
        return {}

    table: dict[tuple[str, int, int], StationCalibration] = {}
    for row in rows:
        station = row["station"]
        month = row["month"]
        hour = row["hour"]
        if not station:
            continue

        cal = StationCalibration(
            sigma_10min=float(row["sigma_10min"]) if row["sigma_10min"] else 0.3,
            hrrr_bias_f=float(row["hrrr_bias_f"] or 0.0),
            hrrr_skill=float(row["hrrr_skill"] or 0.5),
            rounding_bias=float(row["rounding_bias"] or 0.0),
            weights=(
                float(row["weight_physics"] or 0.45),
                float(row["weight_hrrr"] or 0.25),
                float(row["weight_trend"] or 0.20),
                float(row["weight_climo"] or 0.10),
            ),
        )
        table[(station, month, hour)] = cal

    logger.info("station_calibration_loaded", entries=len(table))
    return table


async def compute_hrrr_skill_scores(pool) -> None:
    """Compare stored HRRR forecasts against nearest-in-time METAR actuals.

    Upserts hrrr_bias_f, hrrr_rmse_f, hrrr_skill into station_calibration.
    HRRR skill = 1 - RMSE / climo_std per station.
    """
    query = """
        WITH paired AS (
            SELECT
                h.station,
                EXTRACT(MONTH FROM h.forecast_time)::int AS month,
                EXTRACT(HOUR FROM h.forecast_time)::int AS hour,
                h.temp_2m_f AS hrrr_temp,
                o.temperature_f AS actual_temp
            FROM hrrr_forecasts h
            JOIN LATERAL (
                SELECT temperature_f, observed_at
                FROM observations
                WHERE station = h.station
                  AND source = 'asos'
                  AND temperature_f IS NOT NULL
                  AND observed_at BETWEEN h.forecast_time - interval '30 minutes'
                                      AND h.forecast_time + interval '30 minutes'
                ORDER BY ABS(EXTRACT(EPOCH FROM (observed_at - h.forecast_time)))
                LIMIT 1
            ) o ON true
            WHERE h.forecast_time > now() - interval '90 days'
        ),
        stats AS (
            SELECT
                station, month, hour,
                AVG(hrrr_temp - actual_temp) AS bias,
                SQRT(AVG(POWER(hrrr_temp - actual_temp, 2))) AS rmse,
                STDDEV(actual_temp) AS climo_std,
                COUNT(*) AS n
            FROM paired
            GROUP BY station, month, hour
            HAVING COUNT(*) >= 10
        )
        INSERT INTO station_calibration (station, month, hour, hrrr_bias_f, hrrr_rmse_f, hrrr_skill, sample_size, updated_at)
        SELECT
            station, month, hour,
            bias,
            rmse,
            CASE WHEN climo_std > 0 THEN GREATEST(0.0, 1.0 - rmse / climo_std) ELSE 0.5 END,
            n,
            now()
        FROM stats
        ON CONFLICT (station, month, hour)
        DO UPDATE SET
            hrrr_bias_f = EXCLUDED.hrrr_bias_f,
            hrrr_rmse_f = EXCLUDED.hrrr_rmse_f,
            hrrr_skill = EXCLUDED.hrrr_skill,
            sample_size = EXCLUDED.sample_size,
            updated_at = now()
    """

    try:
        async with pool.acquire() as conn:
            result = await conn.execute(query)
        logger.info("hrrr_skill_scores_computed", result=result)
    except Exception:
        logger.warning("hrrr_skill_computation_failed", exc_info=True)

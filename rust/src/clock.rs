//! Phase 5.5: Clock discipline — NTP offset detection for settlement timing.
//!
//! Checks system clock against NTP servers to detect drift. Settlements are
//! time-sensitive (to the second), so clock accuracy matters.

use std::time::Duration;

use tracing::{error, info, warn};

/// Maximum acceptable clock offset before warning.
const WARN_THRESHOLD: Duration = Duration::from_millis(500);
/// Maximum acceptable clock offset before refusing to start.
const REFUSE_THRESHOLD: Duration = Duration::from_secs(2);

/// Result of a clock check.
#[derive(Debug, Clone)]
pub struct ClockCheck {
    pub offset_ms: i64,
    pub acceptable: bool,
    pub source: String,
}

/// Check system clock offset using HTTP Date headers from well-known servers.
/// This avoids requiring an NTP client library — HTTP Date headers are
/// typically accurate to ~1 second.
pub async fn check_clock_offset() -> ClockCheck {
    let servers = ["https://www.google.com", "https://www.cloudflare.com"];

    for url in &servers {
        match measure_offset(url).await {
            Ok(offset_ms) => {
                let abs_offset = Duration::from_millis(offset_ms.unsigned_abs());
                let acceptable = abs_offset < REFUSE_THRESHOLD;

                if abs_offset >= REFUSE_THRESHOLD {
                    error!(
                        offset_ms = offset_ms,
                        threshold_ms = REFUSE_THRESHOLD.as_millis() as i64,
                        "clock drift exceeds safety threshold"
                    );
                } else if abs_offset >= WARN_THRESHOLD {
                    warn!(
                        offset_ms = offset_ms,
                        "clock drift detected (within tolerance)"
                    );
                } else {
                    info!(offset_ms = offset_ms, "clock offset acceptable");
                }

                return ClockCheck {
                    offset_ms,
                    acceptable,
                    source: url.to_string(),
                };
            }
            Err(e) => {
                warn!(url = url, error = %e, "clock check failed, trying next server");
            }
        }
    }

    // If all servers fail, allow startup with warning
    warn!("all clock check servers unreachable, proceeding with caution");
    ClockCheck {
        offset_ms: 0,
        acceptable: true,
        source: "none (all servers unreachable)".into(),
    }
}

/// Measure clock offset against an HTTP server's Date header.
async fn measure_offset(url: &str) -> anyhow::Result<i64> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let before = chrono::Utc::now();
    let resp = client.head(url).send().await?;
    let after = chrono::Utc::now();

    let date_header = resp
        .headers()
        .get("date")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("no Date header"))?;

    let server_time = chrono::DateTime::parse_from_rfc2822(date_header)
        .map_err(|e| anyhow::anyhow!("parse Date header: {e}"))?
        .with_timezone(&chrono::Utc);

    // Our best estimate of local time when the response was generated
    let local_mid = before + (after - before) / 2;
    let offset = (local_mid - server_time).num_milliseconds();

    Ok(offset)
}

/// Check if system clock is suitable for live trading.
/// Returns Ok(offset_ms) or Err if clock drift exceeds threshold.
pub async fn enforce_clock_discipline(paper_mode: bool) -> anyhow::Result<i64> {
    let check = check_clock_offset().await;

    if !check.acceptable && !paper_mode {
        anyhow::bail!(
            "Clock drift {}ms exceeds {}ms threshold. \
             Fix system clock before live trading.",
            check.offset_ms,
            REFUSE_THRESHOLD.as_millis()
        );
    }

    Ok(check.offset_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thresholds() {
        assert!(WARN_THRESHOLD < REFUSE_THRESHOLD);
        assert_eq!(WARN_THRESHOLD.as_millis(), 500);
        assert_eq!(REFUSE_THRESHOLD.as_secs(), 2);
    }

    #[test]
    fn test_clock_check_acceptable() {
        let check = ClockCheck {
            offset_ms: 100,
            acceptable: true,
            source: "test".into(),
        };
        assert!(check.acceptable);
    }

    #[test]
    fn test_clock_check_unacceptable() {
        let check = ClockCheck {
            offset_ms: 3000,
            acceptable: false,
            source: "test".into(),
        };
        assert!(!check.acceptable);
    }
}

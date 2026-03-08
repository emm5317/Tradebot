//! Phase 5.6: Dead-letter handling for failed signals and orders.
//!
//! Routes unprocessable messages to a dead-letter NATS subject and
//! persists them to the dead_letters DB table for investigation.

use tracing::{info, warn};

const DEAD_LETTER_SUBJECT: &str = "tradebot.deadletter";

/// Reasons a message ends up in dead letter.
#[derive(Debug, Clone)]
pub enum DeadLetterReason {
    DeserializationFailure(String),
    RiskRejection(String),
    OrderSubmissionFailed { ticker: String, attempts: u32 },
    UnknownSignalType(String),
}

impl std::fmt::Display for DeadLetterReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeserializationFailure(e) => write!(f, "deserialization_failure: {e}"),
            Self::RiskRejection(reason) => write!(f, "risk_rejection: {reason}"),
            Self::OrderSubmissionFailed { ticker, attempts } =>
                write!(f, "order_failed: {ticker} after {attempts} attempts"),
            Self::UnknownSignalType(t) => write!(f, "unknown_signal_type: {t}"),
        }
    }
}

/// Send a message to the dead-letter subject and persist to DB.
pub async fn send_dead_letter(
    nats: &async_nats::Client,
    pool: &sqlx::PgPool,
    reason: DeadLetterReason,
    payload: Option<&[u8]>,
    source: &str,
) {
    let error_str = reason.to_string();

    // Publish to NATS dead-letter subject
    if let Err(e) = nats
        .publish(
            DEAD_LETTER_SUBJECT,
            error_str.clone().into(),
        )
        .await
    {
        warn!(error = %e, "failed to publish dead letter to NATS");
    }

    // Persist to database
    let result = sqlx::query(
        "INSERT INTO dead_letters (subject, payload, error, source) \
         VALUES ($1, $2, $3, $4)"
    )
    .bind(DEAD_LETTER_SUBJECT)
    .bind(payload)
    .bind(&error_str)
    .bind(source)
    .execute(pool)
    .await;

    match result {
        Ok(_) => info!(
            reason = %error_str,
            source = source,
            "dead letter recorded"
        ),
        Err(e) => warn!(
            error = %e,
            reason = %error_str,
            "failed to persist dead letter"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dead_letter_reason_display() {
        let reason = DeadLetterReason::DeserializationFailure("bad json".into());
        assert!(reason.to_string().contains("deserialization_failure"));

        let reason = DeadLetterReason::RiskRejection("max positions".into());
        assert!(reason.to_string().contains("risk_rejection"));

        let reason = DeadLetterReason::OrderSubmissionFailed {
            ticker: "KORD-T-95".into(),
            attempts: 3,
        };
        assert!(reason.to_string().contains("order_failed"));
        assert!(reason.to_string().contains("3 attempts"));

        let reason = DeadLetterReason::UnknownSignalType("futures".into());
        assert!(reason.to_string().contains("unknown_signal_type"));
    }
}

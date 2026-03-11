//! Discord webhook alerts — fire-and-forget notifications for critical events.

use tracing::warn;

/// Send a message to a Discord webhook. Logs errors but never panics.
pub async fn send_alert(http: &reqwest::Client, webhook_url: &str, message: &str) {
    let payload = serde_json::json!({ "content": message });
    match http.post(webhook_url).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!("discord alert sent");
        }
        Ok(resp) => {
            warn!(status = %resp.status(), "discord webhook returned non-success");
        }
        Err(e) => {
            warn!(error = %e, "discord webhook request failed");
        }
    }
}

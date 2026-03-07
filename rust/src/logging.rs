use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize structured logging with JSON or pretty output.
///
/// - `level`: filter string, e.g. "info", "debug", "tradebot=debug,sqlx=warn"
/// - `format`: "json" for machine-readable output, anything else for pretty output
pub fn init(level: &str, format: &str) {
    let env_filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));

    if format == "json" {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().json().flatten_event(true))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().pretty())
            .init();
    }
}

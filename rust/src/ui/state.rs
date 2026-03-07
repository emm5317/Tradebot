/// Shared application state passed to Axum handlers.
#[derive(Clone)]
pub struct AppState {
    // TODO: DB pool, position tracker, risk manager handles
}

impl AppState {
    pub fn new() -> Self {
        Self {}
    }
}

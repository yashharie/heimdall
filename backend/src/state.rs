//! Shared application state injected into every Axum handler.
//!
//! [`AppState`] is cheap to clone (it holds an `Arc` internally via
//! [`MySqlPool`]) so it is placed directly in the router with
//! `.with_state(state)`.

use sqlx::MySqlPool;

/// Application-wide shared state.
///
/// Add new shared resources here (e.g. an HTTP client, a cache handle) as
/// the application grows.  Keep it `Clone + Send + Sync`.
#[derive(Clone)]
pub struct AppState {
    /// The MySQL connection pool.  Every handler acquires connections from here.
    pub pool: MySqlPool,
}

impl AppState {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

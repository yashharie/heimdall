//! HTTP route definitions.
//!
//! Each sub-module owns a group of related endpoints.  The [`router`] function
//! assembles the top-level [`axum::Router`] that is handed to the server.

use axum::{routing::get, Router};

use crate::state::AppState;

pub mod health;

/// Build the application router.
///
/// All routes receive shared [`AppState`] via Axum's state extractor.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::handler))
        .with_state(state)
}

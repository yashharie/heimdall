//! `GET /health` — liveness + database reachability check.
//!
//! Returns HTTP 200 when the server is up and the database pool can execute a
//! query.  Returns HTTP 503 if the database is unreachable.
//!
//! Response shape:
//! ```json
//! {
//!   "status": "ok",
//!   "database": "ok",
//!   "pool": { "size": 2, "idle": 2, "max": 10 }
//! }
//! ```

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};

use crate::{db, state::AppState};

/// Health check handler.
pub async fn handler(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match db::health_check(&state.pool).await {
        Ok(()) => {
            let stats = db::pool_stats(&state.pool);
            (
                StatusCode::OK,
                Json(json!({
                    "status":   "ok",
                    "database": "ok",
                    "pool":     stats,
                })),
            )
        }
        Err(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status":   "error",
                "database": reason,
            })),
        ),
    }
}

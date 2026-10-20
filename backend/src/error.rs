//! Application-level error type.
//!
//! [`AppError`] converts into an Axum [`Response`] so handler functions can
//! use `?` freely and errors are serialised to JSON automatically.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// A typed error that Axum can turn into an HTTP response.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum AppError {
    /// The request references a resource that does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A database query failed.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// Any other internal error.  The detail is logged but not sent to clients.
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),

            AppError::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "a database error occurred".to_string(),
                )
            }

            AppError::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "an internal error occurred".to_string(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// Convenience alias for handler return types.
#[allow(dead_code)]
pub type AppResult<T> = Result<T, AppError>;

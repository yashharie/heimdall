//! Typed application configuration loaded from environment variables.
//!
//! All values are read once at startup via [`Config::from_env`].
//! The rest of the application uses the resulting struct — no `std::env::var`
//! calls are scattered through the code.

use std::time::Duration;

use anyhow::{Context, Result};

/// Top-level configuration for the backend.
#[derive(Debug, Clone)]
pub struct Config {
    pub database: DatabaseConfig,
    pub server:   ServerConfig,
    pub tasks:    TasksConfig,
}

/// MySQL connection pool settings.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Full connection URL, e.g. `mysql://user:pass@host:3306/heimdall`
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
}

/// HTTP server bind settings.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

/// Configuration for all background tasks.
#[derive(Debug, Clone)]
pub struct TasksConfig {
    pub retention: RetentionConfig,
}

/// Configuration for the data-retention background task.
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// How often to run the retention sweep, in seconds.
    /// Default: 86400 (24 hours).
    pub schedule_secs: u64,
    /// Rows deleted per batch (each batch is one short DELETE statement).
    /// Default: 500.
    pub batch_size: u64,
}

impl ServerConfig {
    /// Returns the bind address string, e.g. `"127.0.0.1:8080"`.
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Call [`dotenvy::dotenv`] before this to support `.env` files in
    /// development.  In production the variables are injected directly by the
    /// process environment.
    ///
    /// # Errors
    ///
    /// Returns an error if any required variable is missing or cannot be parsed.
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            database: DatabaseConfig {
                url: required_var("DATABASE_URL")?,

                max_connections: optional_var("DATABASE_MAX_CONNECTIONS", 10)?,
                min_connections: optional_var("DATABASE_MIN_CONNECTIONS", 2)?,

                connect_timeout: Duration::from_secs(optional_var(
                    "DATABASE_CONNECT_TIMEOUT_SECS",
                    30,
                )?),
                idle_timeout: Duration::from_secs(optional_var(
                    "DATABASE_IDLE_TIMEOUT_SECS",
                    600,
                )?),
                max_lifetime: Duration::from_secs(optional_var(
                    "DATABASE_MAX_LIFETIME_SECS",
                    1800,
                )?),
            },
            server: ServerConfig {
                host: optional_str_var("SERVER_HOST", "127.0.0.1"),
                port: optional_var("SERVER_PORT", 8080)?,
            },
            tasks: TasksConfig {
                retention: RetentionConfig {
                    schedule_secs: optional_var("RETENTION_SCHEDULE_SECS", 86_400)?,
                    batch_size:    optional_var("RETENTION_BATCH_SIZE",    500)?,
                },
            },
        })
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn required_var(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("missing required env var: {name}"))
}

fn optional_str_var(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn optional_var<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr + std::fmt::Debug,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(val) => val
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("env var {name}={val:?} could not be parsed: {e}")),
        Err(_) => Ok(default),
    }
}

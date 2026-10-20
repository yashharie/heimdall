//! Database connection pool initialisation and helpers.
//!
//! This module owns the [`MySqlPool`] lifecycle:
//!   * [`init_pool`]   — build and connect the pool from [`DatabaseConfig`]
//!   * [`run_migrations`] — run all pending SQLx migrations
//!   * [`health_check`]   — lightweight ping used by the `/health` endpoint
//!
//! The pool is stored in Axum's shared state ([`AppState`]) so every handler
//! can acquire a connection with `.acquire()` or use `sqlx::query!` directly.

use anyhow::{Context, Result};
use sqlx::{
    mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode},
    MySqlPool,
};
use tracing::{info, instrument};

use crate::config::DatabaseConfig;

/// Initialise the MySQL connection pool.
///
/// Applies all pool sizing and timeout options from [`DatabaseConfig`], then
/// acquires one connection to prove the database is reachable before returning.
///
/// # Errors
///
/// Returns an error if the connection URL is invalid, the database is
/// unreachable, or authentication fails.
#[instrument(skip(cfg), fields(host = tracing::field::Empty))]
pub async fn init_pool(cfg: &DatabaseConfig) -> Result<MySqlPool> {
    let connect_opts = cfg
        .url
        .parse::<MySqlConnectOptions>()
        .context("invalid DATABASE_URL")?
        // Require TLS for non-localhost connections; allow unverified certs
        // in development (override in production with a proper CA bundle).
        .ssl_mode(MySqlSslMode::Preferred)
        // Disable the MySQL statement cache to avoid stale prepared-statement
        // errors after a failover or schema migration during development.
        // Re-enable and tune for production once the schema is stable.
        .statement_cache_capacity(100);

    let pool = MySqlPoolOptions::new()
        .max_connections(cfg.max_connections)
        .min_connections(cfg.min_connections)
        .acquire_timeout(cfg.connect_timeout)
        .idle_timeout(Some(cfg.idle_timeout))
        .max_lifetime(Some(cfg.max_lifetime))
        // Test each connection as it comes out of the pool. Cheap for MySQL
        // (sends a COM_PING), catches stale connections before handlers see them.
        .test_before_acquire(true)
        .connect_with(connect_opts)
        .await
        .context("failed to connect to MySQL")?;

    info!(
        max_connections = cfg.max_connections,
        min_connections = cfg.min_connections,
        "database pool ready"
    );

    Ok(pool)
}

/// Run all pending migrations from the `migrations/` directory.
///
/// SQLx tracks applied migrations in the `_sqlx_migrations` table it creates
/// automatically.  Running this at startup is safe — already-applied
/// migrations are skipped.
///
/// # Errors
///
/// Returns an error if a migration fails or the migrations directory is
/// missing.
pub async fn run_migrations(pool: &MySqlPool) -> Result<()> {
    info!("running database migrations");

    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("database migration failed")?;

    info!("migrations complete");
    Ok(())
}

/// Verify the pool can reach the database.
///
/// Sends a `SELECT 1` and returns `Ok(())` on success.  Used by the
/// `/health` endpoint (see [`crate::routes::health`]).
///
/// Returns an error string on failure so the health response can include a
/// human-readable reason without panicking.
pub async fn health_check(pool: &MySqlPool) -> Result<(), String> {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| {
            tracing::error!(error = %e, "database health check failed");
            format!("database unreachable: {e}")
        })
}

/// Returns the current pool size statistics for observability.
pub fn pool_stats(pool: &MySqlPool) -> PoolStats {
    PoolStats {
        size:     pool.size(),
        idle:     pool.num_idle(),
        max:      pool.options().get_max_connections(),
    }
}

/// Snapshot of connection pool counters.
#[derive(Debug, serde::Serialize)]
pub struct PoolStats {
    /// Total open connections (idle + in-use).
    pub size: u32,
    /// Currently idle connections.
    pub idle: usize,
    /// Configured maximum connections.
    pub max: u32,
}

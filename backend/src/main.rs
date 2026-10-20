//! HEIMDALL backend — entry point.
//!
//! Startup sequence:
//!   1. Load `.env` (dev) / environment variables (prod)
//!   2. Initialise structured logging (tracing)
//!   3. Parse typed [`Config`] from environment
//!   4. Connect the MySQL connection pool
//!   5. Run pending SQLx migrations
//!   6. Build the Axum router with shared [`AppState`]
//!   7. Bind and serve

use anyhow::Result;
use tracing::info;

mod config;
mod db;
mod error;
mod queries;
mod routes;
mod state;

#[tokio::main]
async fn main() -> Result<()> {
    // ── 1. Load .env in development (no-op if the file is absent) ─────────────
    dotenvy::dotenv().ok();

    // ── 2. Initialise tracing ─────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // ── 3. Load typed config ──────────────────────────────────────────────────
    let cfg = config::Config::from_env()?;
    info!(bind = %cfg.server.bind_addr(), "starting HEIMDALL backend");

    // ── 4. Connect the database pool ──────────────────────────────────────────
    let pool = db::init_pool(&cfg.database).await?;

    // ── 5. Run migrations ─────────────────────────────────────────────────────
    db::run_migrations(&pool).await?;

    // ── 6. Build the router ───────────────────────────────────────────────────
    let app_state = state::AppState::new(pool);
    let app = routes::router(app_state);

    // ── 7. Bind and serve ─────────────────────────────────────────────────────
    let listener = tokio::net::TcpListener::bind(cfg.server.bind_addr()).await?;
    info!(addr = %listener.local_addr()?, "listening");

    axum::serve(listener, app).await?;

    Ok(())
}

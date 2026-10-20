//! Background Tokio tasks.
//!
//! Each sub-module is a long-running async task spawned at startup.
//! Tasks receive a clone of the pool and their own config section.
//!
//! # Shutdown
//!
//! Every task receives a [`tokio::sync::CancellationToken`] and exits cleanly
//! when it is cancelled.  [`spawn_all`] returns a [`tokio::task::JoinSet`] so
//! the caller can await orderly shutdown.
//!
//! # Adding a new task
//!
//! 1. Create `src/tasks/<name>.rs` with an async `run(pool, cfg, cancel)` fn.
//! 2. Add `pub mod <name>;` below.
//! 3. Spawn it inside [`spawn_all`].

pub mod retention;

use sqlx::MySqlPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::TasksConfig;

/// Spawn all background tasks and return their join handles.
///
/// Call this after the pool and migrations are ready.  Drop the returned
/// [`JoinSet`] to detach all tasks, or call `.abort_all()` for a hard stop.
/// For graceful shutdown, cancel the token and then `join_next()` until empty.
pub fn spawn_all(
    pool:   MySqlPool,
    cfg:    &TasksConfig,
    cancel: CancellationToken,
) -> JoinSet<()> {
    let mut set = JoinSet::new();

    // ── Retention task ────────────────────────────────────────────────────────
    {
        let pool   = pool.clone();
        let cfg    = cfg.retention.clone();
        let cancel = cancel.clone();
        info!(
            schedule_secs = cfg.schedule_secs,
            batch_size     = cfg.batch_size,
            "spawning retention task"
        );
        set.spawn(async move {
            retention::run(pool, cfg, cancel).await;
        });
    }

    set
}

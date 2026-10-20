//! Data retention background task.
//!
//! # What it does
//!
//! On every scheduled run the task:
//!   1. Reads all rows from `retention_policies` where `retain_days > 0`.
//!   2. For each policy, deletes `metrics` rows older than the cutoff in
//!      batches of [`RetentionConfig::batch_size`] rows.
//!   3. Skips rows whose `quality = 'quarantine'` — those are under
//!      maintainer review and must never be deleted automatically.
//!   4. Writes one row to `retention_log` per policy per run (result or error).
//!
//! # Schedule
//!
//! The task sleeps for [`RetentionConfig::schedule_secs`] between runs
//! (default 24 h).  It does not use a wall-clock cron: the first run fires
//! immediately at startup (offset by a short delay so the server is fully
//! ready) and then repeats on the interval.  This means it always runs
//! even if the server restarts mid-day, and never drifts from the process.
//!
//! # Safety
//!
//! * `throughput`, `outage_start`, `outage_end` all have `retain_days = 0`
//!   in the seed data — they are kept forever and never touched here.
//! * Batched deletes keep each transaction short to avoid InnoDB lock
//!   contention on the append-heavy `metrics` table.
//! * On cancellation the current batch completes; no partial batch is left
//!   uncommitted.

use std::time::Duration;

use chrono::Utc;
use sqlx::MySqlPool;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::config::RetentionConfig;

// ── Policy row fetched from the DB ───────────────────────────────────────────

#[derive(Debug)]
struct RetentionPolicy {
    metric_type: String,
    retain_days: u32,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Run the retention loop.  Never returns unless `cancel` is triggered.
pub async fn run(pool: MySqlPool, cfg: RetentionConfig, cancel: CancellationToken) {
    // Short startup delay so migrations and pool warm-up finish first.
    let startup_delay = Duration::from_secs(30);
    info!(delay_secs = 30, "retention task: waiting before first run");

    tokio::select! {
        _ = tokio::time::sleep(startup_delay) => {}
        _ = cancel.cancelled() => {
            info!("retention task: cancelled before first run");
            return;
        }
    }

    loop {
        info!("retention task: starting run");
        run_once(&pool, &cfg).await;

        info!(
            next_secs = cfg.schedule_secs,
            "retention task: run complete, sleeping"
        );

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(cfg.schedule_secs)) => {}
            _ = cancel.cancelled() => {
                info!("retention task: cancelled, exiting");
                return;
            }
        }
    }
}

// ── Single retention pass ─────────────────────────────────────────────────────

/// Execute one full retention pass over all configured policies.
#[instrument(skip_all, name = "retention_run")]
async fn run_once(pool: &MySqlPool, cfg: &RetentionConfig) {
    let policies = match load_policies(pool).await {
        Ok(p) => p,
        Err(e) => {
            error!(error = %e, "retention: failed to load policies");
            return;
        }
    };

    if policies.is_empty() {
        info!("retention: no finite-retention policies found, nothing to do");
        return;
    }

    for policy in &policies {
        process_policy(pool, policy, cfg.batch_size).await;
    }
}

/// Fetch all retention policies that have a finite window (`retain_days > 0`).
async fn load_policies(pool: &MySqlPool) -> Result<Vec<RetentionPolicy>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT metric_type, retain_days
        FROM   retention_policies
        WHERE  retain_days > 0
        ORDER BY metric_type
        "#
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| RetentionPolicy {
            metric_type: r.metric_type,
            retain_days: r.retain_days,
        })
        .collect())
}

/// Delete old rows for one policy, in batches.  Log the result.
#[instrument(
    skip(pool),
    fields(
        metric_type = %policy.metric_type,
        retain_days = policy.retain_days,
    )
)]
async fn process_policy(pool: &MySqlPool, policy: &RetentionPolicy, batch_size: u64) {
    // Cutoff: anything recorded before this moment is eligible for deletion.
    let cutoff = Utc::now().naive_utc()
        - chrono::Duration::days(policy.retain_days as i64);

    info!(%cutoff, "retention: processing policy");

    let (rows_deleted, status, error_message) =
        delete_in_batches(pool, &policy.metric_type, cutoff, batch_size).await;

    // Log this run to retention_log.
    let log_result = sqlx::query!(
        r#"
        INSERT INTO retention_log
            (metric_type, cutoff_date, rows_deleted, status, error_message)
        VALUES
            (?, ?, ?, ?, ?)
        "#,
        policy.metric_type,
        cutoff,
        rows_deleted,
        status,
        error_message,
    )
    .execute(pool)
    .await;

    if let Err(e) = log_result {
        // Non-fatal: the deletion already happened; just warn so operators
        // can investigate the retention_log gap.
        warn!(error = %e, "retention: failed to write retention_log row");
    }

    info!(
        rows_deleted,
        status,
        "retention: policy complete"
    );
}

/// Delete rows for `metric_type` older than `cutoff` in batches.
///
/// Returns `(total_rows_deleted, status, error_message)` where `status` is
/// one of the `retention_log.status` ENUM values: `"ok"`, `"partial"`, or
/// `"error"`.
async fn delete_in_batches(
    pool:        &MySqlPool,
    metric_type: &str,
    cutoff:      chrono::NaiveDateTime,
    batch_size:  u64,
) -> (u64, &'static str, Option<String>) {
    let mut total_deleted: u64 = 0;

    loop {
        // Each batch is its own short transaction — keeps undo log small.
        let result = sqlx::query!(
            r#"
            DELETE FROM metrics
            WHERE  metric_type = ?
              AND  recorded_at < ?
              AND  quality     != 'quarantine'
            LIMIT  ?
            "#,
            metric_type,
            cutoff,
            batch_size,
        )
        .execute(pool)
        .await;

        match result {
            Ok(res) => {
                let deleted = res.rows_affected();
                total_deleted += deleted;

                if deleted < batch_size {
                    // Last batch — fewer rows than the limit means we're done.
                    return (total_deleted, "ok", None);
                }

                // Brief yield between batches to let other queries through.
                tokio::task::yield_now().await;
            }

            Err(e) => {
                error!(
                    error = %e,
                    metric_type,
                    total_deleted,
                    "retention: batch delete failed"
                );
                let status = if total_deleted > 0 { "partial" } else { "error" };
                return (total_deleted, status, Some(e.to_string()));
            }
        }
    }
}

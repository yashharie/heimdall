//! Queries for the `computers` table.
//!
//! Used by the ingest path to:
//!   * Verify the device is known and not revoked before accepting metrics.
//!   * Count active computers per office for the enrolment limit check (FR5).

use sqlx::MySqlPool;

use crate::error::AppResult;

/// Minimal computer record needed by the ingest path.
#[derive(Debug)]
pub struct ComputerRecord {
    pub id:        u64,
    pub office_id: u64,
    /// `Some` means the device is revoked; `None` means active.
    pub revoked_at: Option<chrono::NaiveDateTime>,
    /// `Some` means still awaiting maintainer approval.
    pub pending_approval_at: Option<chrono::NaiveDateTime>,
}

/// Look up a computer by its device UUID.
///
/// Returns `None` when no computer with this `device_id` exists.
/// The caller must check `revoked_at` and `pending_approval_at` before
/// accepting metrics from this device.
pub async fn find_by_device_id(
    pool:      &MySqlPool,
    device_id: &str,
) -> AppResult<Option<ComputerRecord>> {
    let row = sqlx::query_as!(
        ComputerRecord,
        r#"
        SELECT
            id,
            office_id,
            revoked_at  AS "revoked_at: chrono::NaiveDateTime",
            pending_approval_at AS "pending_approval_at: chrono::NaiveDateTime"
        FROM computers
        WHERE device_id = ?
          AND deleted_at IS NULL
        LIMIT 1
        "#,
        device_id,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Count the number of active (not revoked, not deleted) computers for an
/// office.
///
/// Used to enforce the per-office computer limit before approving a new
/// enrolment (FR5).
pub async fn count_active_for_office(
    pool:      &MySqlPool,
    office_id: u64,
) -> AppResult<u64> {
    let row = sqlx::query!(
        r#"
        SELECT COUNT(*) AS count
        FROM computers
        WHERE office_id  = ?
          AND revoked_at IS NULL
          AND deleted_at IS NULL
        "#,
        office_id,
    )
    .fetch_one(pool)
    .await?;

    Ok(row.count as u64)
}

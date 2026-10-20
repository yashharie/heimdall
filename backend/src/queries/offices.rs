//! Queries for the `offices` table.
//!
//! Used by the ingest path to look up office details and enforce limits.

use sqlx::MySqlPool;

use crate::error::AppResult;

/// Minimal office record needed at ingest time.
#[derive(Debug)]
pub struct OfficeRecord {
    pub id:            u64,
    pub name:          String,
    pub max_computers: u16,
    pub is_active:     bool,
}

/// Fetch an office by its surrogate primary key.
///
/// Returns `None` when no active, non-deleted office with this `id` exists.
pub async fn find_by_id(
    pool: &MySqlPool,
    id:   u64,
) -> AppResult<Option<OfficeRecord>> {
    let row = sqlx::query_as!(
        OfficeRecord,
        r#"
        SELECT
            id,
            name,
            max_computers,
            is_active AS "is_active: bool"
        FROM offices
        WHERE id         = ?
          AND is_active  = 1
          AND deleted_at IS NULL
        LIMIT 1
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

//! Queries for the `metrics` table.
//!
//! # Four operations (Task 4)
//!
//! | Function | SQL | Used by |
//! |---|---|---|
//! | [`insert_metric`] | `INSERT INTO metrics …` | ingest handler (Issue #2) |
//! | [`fetch_latest`] | Latest row per computer + type | dashboard live view |
//! | [`fetch_history`] | Time-range rows per computer | dashboard chart data |
//! | [`fetch_office_history`] | Time-range rows for a whole office | office summary page |
//!
//! All timestamps are [`chrono::NaiveDateTime`] (UTC, millisecond precision)
//! matching the `DATETIME(3)` columns in the schema.

use chrono::NaiveDateTime;
use sqlx::MySqlPool;

use crate::error::AppResult;

// ── Input types ───────────────────────────────────────────────────────────────

/// All data needed to write one metric row.
///
/// Built by the ingest handler after it has validated the signed envelope
/// from the agent.
#[derive(Debug)]
#[allow(dead_code)]
pub struct NewMetric {
    pub computer_id: u64,
    pub office_id:   u64,

    /// One of the ENUM values from the schema:
    /// `heartbeat | latency | throughput | link_change |
    ///  outage_start | outage_end | shutdown | boot`
    pub metric_type: String,

    /// Server-assigned authoritative receive time (SR-C2).
    pub recorded_at: NaiveDateTime,
    /// Agent wall-clock time (advisory).
    pub agent_ts:    Option<NaiveDateTime>,
    /// Difference: server_time − agent_time, milliseconds.
    pub agent_clock_skew_ms: Option<i32>,

    /// Strictly-increasing per-device counter; the UNIQUE constraint on
    /// `(computer_id, seq)` rejects replays at the DB level.
    pub seq: u64,

    /// SHA-256 hex of the previous row for this computer (hash chain).
    pub prev_hash:    Option<String>,
    /// SHA-256 hex of this row's canonical payload.
    pub payload_hash: String,
    /// Base64url Ed25519 signature over the canonical envelope.
    pub signature:    String,

    // ── Numeric measurements (all optional) ──────────────────────────────────
    pub latency_ms:       Option<f32>,
    pub jitter_ms:        Option<f32>,
    pub packet_loss_pct:  Option<f32>,
    pub download_mbps:    Option<f32>,
    pub upload_mbps:      Option<f32>,
    pub outage_duration_ms: Option<u64>,

    // ── Link context (optional) ───────────────────────────────────────────────
    pub link_type:       Option<String>,
    pub link_speed_mbps: Option<f32>,
    pub asn:             Option<u32>,
    pub isp_name:        Option<String>,
}

/// Filter for time-range history queries.
#[derive(Debug)]
#[allow(dead_code)]
pub struct TimeRangeFilter {
    pub computer_id: u64,
    /// Inclusive lower bound.
    pub from: NaiveDateTime,
    /// Inclusive upper bound.
    pub to:   NaiveDateTime,
    /// If `Some`, restrict to this metric type only.
    pub metric_type: Option<String>,
    /// Maximum rows to return (caller must supply a sensible cap).
    pub limit: u32,
}

/// Filter for office-level history queries.
#[derive(Debug)]
#[allow(dead_code)]
pub struct OfficeHistoryFilter {
    pub office_id: u64,
    pub from:      NaiveDateTime,
    pub to:        NaiveDateTime,
    /// If `Some`, restrict to this metric type only.
    pub metric_type: Option<String>,
    pub limit: u32,
}

// ── Output types ──────────────────────────────────────────────────────────────

/// A metric row returned from the database.
///
/// Mirrors the `metrics` columns the dashboard actually needs.
/// The `signature`, `prev_hash`, and `payload_hash` integrity fields are
/// omitted here — they are only needed by the Merkle proof endpoint.
#[derive(Debug, serde::Serialize)]
pub struct MetricRow {
    pub id:          u64,
    pub computer_id: u64,
    pub office_id:   u64,
    pub metric_type: String,
    pub recorded_at: NaiveDateTime,
    pub agent_ts:    Option<NaiveDateTime>,

    pub latency_ms:          Option<f32>,
    pub jitter_ms:           Option<f32>,
    pub packet_loss_pct:     Option<f32>,
    pub download_mbps:       Option<f32>,
    pub upload_mbps:         Option<f32>,
    pub outage_duration_ms:  Option<u64>,

    pub link_type:       Option<String>,
    pub link_speed_mbps: Option<f32>,
    pub asn:             Option<u32>,
    pub isp_name:        Option<String>,

    pub quality: String,
}

// ── Queries ───────────────────────────────────────────────────────────────────

/// Insert one metric row.
///
/// The `UNIQUE KEY uq_metrics_device_seq (computer_id, seq)` in the schema
/// means a duplicate sequence number for the same device causes a
/// `sqlx::Error::Database` with MySQL error code 1062 (duplicate entry).
/// The caller should map that to an appropriate HTTP 409 / replay-rejection
/// response.
///
/// Returns the auto-generated `id` of the new row.
pub async fn insert_metric(pool: &MySqlPool, m: &NewMetric) -> AppResult<u64> {
    let result = sqlx::query(
        r#"
        INSERT INTO metrics (
            computer_id, office_id, metric_type,
            recorded_at, agent_ts, agent_clock_skew_ms,
            seq, prev_hash, payload_hash, signature,
            latency_ms, jitter_ms, packet_loss_pct,
            download_mbps, upload_mbps, outage_duration_ms,
            link_type, link_speed_mbps, asn, isp_name,
            quality
        ) VALUES (
            ?, ?, ?,
            ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?,
            ?, ?, ?,
            ?, ?, ?, ?,
            'ok'
        )
        "#,
    )
    .bind(m.computer_id)
    .bind(m.office_id)
    .bind(&m.metric_type)
    .bind(m.recorded_at)
    .bind(m.agent_ts)
    .bind(m.agent_clock_skew_ms)
    .bind(m.seq)
    .bind(&m.prev_hash)
    .bind(&m.payload_hash)
    .bind(&m.signature)
    .bind(m.latency_ms)
    .bind(m.jitter_ms)
    .bind(m.packet_loss_pct)
    .bind(m.download_mbps)
    .bind(m.upload_mbps)
    .bind(m.outage_duration_ms)
    .bind(&m.link_type)
    .bind(m.link_speed_mbps)
    .bind(m.asn)
    .bind(&m.isp_name)
    .execute(pool)
    .await?;

    Ok(result.last_insert_id())
}

/// Fetch the single most-recent `quality = 'ok'` row for a given computer
/// and metric type.
///
/// Uses `idx_metrics_latest_per_computer` — no filesort needed.
///
/// Returns `None` when no matching row exists (computer has not yet sent
/// this metric type, or all rows are quarantined/rejected).
pub async fn fetch_latest(
    pool:        &MySqlPool,
    computer_id: u64,
    metric_type: &str,
) -> AppResult<Option<MetricRow>> {
    let row = sqlx::query_as!(
        MetricRow,
        r#"
        SELECT
            id,
            computer_id,
            office_id,
            metric_type,
            recorded_at,
            agent_ts,
            latency_ms,
            jitter_ms,
            packet_loss_pct,
            download_mbps,
            upload_mbps,
            outage_duration_ms,
            link_type,
            link_speed_mbps,
            asn,
            isp_name,
            quality
        FROM metrics
        WHERE computer_id = ?
          AND metric_type = ?
          AND quality     = 'ok'
        ORDER BY recorded_at DESC
        LIMIT 1
        "#,
        computer_id,
        metric_type,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Fetch metric rows for a computer within a time range.
///
/// Results are ordered oldest-first (ascending `recorded_at`) so callers
/// can feed the slice directly into a chart renderer.
///
/// Uses `idx_metrics_history_range`.
///
/// # Pagination
///
/// Pass `filter.limit` to cap the result set.  For large ranges the caller
/// should paginate by using the `recorded_at` of the last row as the next
/// `filter.from`.
pub async fn fetch_history(
    pool:   &MySqlPool,
    filter: &TimeRangeFilter,
) -> AppResult<Vec<MetricRow>> {
    // When metric_type is None we want all types; when Some we filter.
    // MySQL doesn't short-circuit a `WHERE x = NULL` to true, so we
    // use a conditional: `(? IS NULL OR metric_type = ?)`.
    let rows = sqlx::query_as!(
        MetricRow,
        r#"
        SELECT
            id,
            computer_id,
            office_id,
            metric_type,
            recorded_at,
            agent_ts,
            latency_ms,
            jitter_ms,
            packet_loss_pct,
            download_mbps,
            upload_mbps,
            outage_duration_ms,
            link_type,
            link_speed_mbps,
            asn,
            isp_name,
            quality
        FROM metrics
        WHERE computer_id = ?
          AND quality     = 'ok'
          AND recorded_at BETWEEN ? AND ?
          AND (? IS NULL OR metric_type = ?)
        ORDER BY recorded_at ASC
        LIMIT ?
        "#,
        filter.computer_id,
        filter.from,
        filter.to,
        filter.metric_type,
        filter.metric_type,
        filter.limit,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Fetch metric rows for an entire office within a time range.
///
/// Useful for the office summary page which needs data across all computers
/// in one office.  Uses `idx_metrics_office_summary`.
///
/// Results are ordered by `(recorded_at ASC, computer_id ASC)`.
pub async fn fetch_office_history(
    pool:   &MySqlPool,
    filter: &OfficeHistoryFilter,
) -> AppResult<Vec<MetricRow>> {
    let rows = sqlx::query_as!(
        MetricRow,
        r#"
        SELECT
            id,
            computer_id,
            office_id,
            metric_type,
            recorded_at,
            agent_ts,
            latency_ms,
            jitter_ms,
            packet_loss_pct,
            download_mbps,
            upload_mbps,
            outage_duration_ms,
            link_type,
            link_speed_mbps,
            asn,
            isp_name,
            quality
        FROM metrics
        WHERE office_id = ?
          AND quality   = 'ok'
          AND recorded_at BETWEEN ? AND ?
          AND (? IS NULL OR metric_type = ?)
        ORDER BY recorded_at ASC, computer_id ASC
        LIMIT ?
        "#,
        filter.office_id,
        filter.from,
        filter.to,
        filter.metric_type,
        filter.metric_type,
        filter.limit,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Fetch the most-recent `quality = 'ok'` heartbeat time for every computer
/// in an office.
///
/// Used by the public map to determine live status per office.
/// Returns `(computer_id, last_heartbeat_at)` pairs.
#[derive(Debug, serde::Serialize)]
pub struct ComputerLastSeen {
    pub computer_id:       u64,
    pub last_heartbeat_at: Option<chrono::NaiveDateTime>,
}

pub async fn fetch_office_last_seen(
    pool:      &MySqlPool,
    office_id: u64,
) -> AppResult<Vec<ComputerLastSeen>> {
    let rows = sqlx::query_as!(
        ComputerLastSeen,
        r#"
        SELECT
            computer_id,
            MAX(recorded_at) AS "last_heartbeat_at: NaiveDateTime"
        FROM metrics
        WHERE office_id   = ?
          AND metric_type = 'heartbeat'
          AND quality     = 'ok'
        GROUP BY computer_id
        "#,
        office_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

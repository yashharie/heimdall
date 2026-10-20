-- =============================================================================
-- HEIMDALL — Indexes for Time-Based Queries + Data Retention
-- Migration: 002_indexes_and_retention.sql
-- Database:  heimdall (MySQL Community Edition)
-- Author:    Yaswini (Issue #3)
-- Depends on: 001_initial_schema.sql
-- =============================================================================
-- This migration adds:
--   A. Covering indexes optimised for the four main query patterns:
--        1. Fetch latest metric per computer / office
--        2. Fetch metric history by time range
--        3. Dashboard summary (office + type + time)
--        4. Outage history lookup
--   B. A retention_log table that the background job writes to each run,
--      so every deletion is traceable.
--   C. A scheduled event (MySQL Event Scheduler) that calls the retention
--      stored procedure daily.  The stored procedure itself is a stub —
--      the Rust background task (Issue #5) owns the real logic and calls
--      DELETE directly; the event is a fallback / documentation anchor.
-- =============================================================================


-- =============================================================================
-- A. COVERING INDEXES
-- =============================================================================

-- ---------------------------------------------------------------------------
-- A1. "Fetch latest metric for a computer"
--     Query pattern:
--       SELECT * FROM metrics
--       WHERE computer_id = ?
--         AND metric_type = ?
--         AND quality = 'ok'
--       ORDER BY recorded_at DESC
--       LIMIT 1;
--
--     The index covers (computer_id, metric_type, quality, recorded_at) so
--     MySQL can resolve the WHERE + ORDER BY from the index alone and read
--     only the last matching row.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_metrics_latest_per_computer
    ON metrics (computer_id, metric_type, quality, recorded_at DESC);

-- ---------------------------------------------------------------------------
-- A2. "Fetch metric history for a computer in a time range"
--     Query pattern:
--       SELECT recorded_at, latency_ms, jitter_ms, packet_loss_pct
--       FROM metrics
--       WHERE computer_id = ?
--         AND metric_type = 'latency'
--         AND quality     = 'ok'
--         AND recorded_at BETWEEN ? AND ?
--       ORDER BY recorded_at ASC;
--
--     Same leading columns as A1; MySQL will reuse that index for the range
--     scan. This dedicated index adds DESC ordering on recorded_at for the
--     reverse-chronological history view used in the dashboard sidebar.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_metrics_history_range
    ON metrics (computer_id, metric_type, recorded_at, quality);

-- ---------------------------------------------------------------------------
-- A3. "Office-level dashboard summary"
--     Query pattern (aggregation over the last N hours for an office):
--       SELECT metric_type,
--              AVG(latency_ms), AVG(jitter_ms), AVG(packet_loss_pct),
--              AVG(download_mbps), AVG(upload_mbps)
--       FROM metrics
--       WHERE office_id   = ?
--         AND quality     = 'ok'
--         AND recorded_at >= ?
--       GROUP BY metric_type;
--
--     Covers the office-level filter + time lower-bound + quality gate.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_metrics_office_summary
    ON metrics (office_id, quality, recorded_at, metric_type);

-- ---------------------------------------------------------------------------
-- A4. "Fetch latest metric per office" (public map live pin)
--     Query pattern:
--       SELECT office_id, MAX(recorded_at) AS last_seen
--       FROM metrics
--       WHERE quality = 'ok'
--         AND metric_type = 'heartbeat'
--       GROUP BY office_id;
--
--     Covers quality + metric_type filter with office_id and recorded_at in
--     the index so the GROUP BY can use a loose index scan.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_metrics_office_last_seen
    ON metrics (quality, metric_type, office_id, recorded_at DESC);

-- ---------------------------------------------------------------------------
-- A5. "Outage history for an office"
--     Query pattern:
--       SELECT computer_id, recorded_at, outage_duration_ms
--       FROM metrics
--       WHERE office_id   = ?
--         AND metric_type IN ('outage_start', 'outage_end')
--         AND recorded_at BETWEEN ? AND ?
--       ORDER BY recorded_at ASC;
--
--     Narrow type filter + time range on office; outage_duration_ms is
--     included so the SELECT can be covered without a row lookup.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_metrics_outages
    ON metrics (office_id, metric_type, recorded_at, computer_id, outage_duration_ms);

-- ---------------------------------------------------------------------------
-- A6. "Retention sweep — find rows to delete"
--     Query pattern (run by the retention job):
--       SELECT id FROM metrics
--       WHERE metric_type = ?
--         AND recorded_at < ?         -- older than retention cutoff
--         AND quality != 'quarantine' -- never delete under-review rows
--       LIMIT 1000;                   -- batched delete
--
--     metric_type + recorded_at covers the sweep; quality filters are cheap
--     on the small result set.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_metrics_retention_sweep
    ON metrics (metric_type, recorded_at, quality);

-- ---------------------------------------------------------------------------
-- A7. "Alert dashboard — open alerts for an office"
--     Query pattern:
--       SELECT * FROM alerts
--       WHERE office_id = ?
--         AND status    = 'open'
--       ORDER BY created_at DESC;
-- ---------------------------------------------------------------------------
CREATE INDEX idx_alerts_office_open
    ON alerts (office_id, status, created_at DESC);

-- ---------------------------------------------------------------------------
-- A8. "Computer enrolment pending review"
--     Query pattern:
--       SELECT * FROM computers
--       WHERE pending_approval_at IS NOT NULL
--         AND revoked_at IS NULL
--         AND deleted_at IS NULL
--       ORDER BY pending_approval_at ASC;
-- ---------------------------------------------------------------------------
CREATE INDEX idx_computers_pending
    ON computers (pending_approval_at, revoked_at, deleted_at);

-- ---------------------------------------------------------------------------
-- A9. "Active computers for an office" (enrolment count check, FR5)
--     Query pattern:
--       SELECT COUNT(*) FROM computers
--       WHERE office_id   = ?
--         AND revoked_at  IS NULL
--         AND deleted_at  IS NULL;
-- ---------------------------------------------------------------------------
CREATE INDEX idx_computers_office_active
    ON computers (office_id, revoked_at, deleted_at);

-- ---------------------------------------------------------------------------
-- A10. Audit log — recent actions by a user
--      Query pattern:
--        SELECT * FROM audit_log
--        WHERE user_id = ?
--        ORDER BY created_at DESC
--        LIMIT 50;
--
--      The existing idx_audit_user covers user_id; adding created_at DESC
--      avoids a filesort on this frequently-used admin view.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_audit_user_time
    ON audit_log (user_id, created_at DESC);


-- =============================================================================
-- B. RETENTION LOG TABLE
--    Every time the retention job runs and deletes rows, it writes one summary
--    row here.  Gives maintainers full traceability of what was deleted and
--    when (connects to SR-G3 spirit for automated actions).
-- =============================================================================
CREATE TABLE IF NOT EXISTS retention_log (
    id              BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,

    -- When this retention run executed
    run_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),

    -- Which metric_type was processed ('*' = all types in one pass)
    metric_type     VARCHAR(30)     NOT NULL,

    -- The cutoff date used: rows with recorded_at < cutoff_date were eligible
    cutoff_date     DATETIME(3)     NOT NULL,

    -- How many rows were deleted in this run
    rows_deleted    BIGINT UNSIGNED NOT NULL DEFAULT 0,

    -- Whether the run completed cleanly or was interrupted
    status          ENUM('ok', 'partial', 'error') NOT NULL DEFAULT 'ok',
    error_message   VARCHAR(500)    NULL,

    PRIMARY KEY (id),
    KEY idx_retention_log_run_at      (run_at),
    KEY idx_retention_log_metric_type (metric_type, run_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- =============================================================================
-- C. RETENTION STORED PROCEDURE
--    The Rust background task (Issue #5) owns the actual deletion loop.
--    This procedure is a MySQL-native fallback that the Event Scheduler can
--    call if the Rust service is down, and serves as executable documentation
--    of the deletion logic.
--
--    Deletion rules:
--      * Rows are deleted in batches of 500 to avoid long-running transactions
--        and excessive InnoDB undo log growth.
--      * Rows with quality = 'quarantine' are NEVER deleted (they are under
--        review; a maintainer must resolve them first).
--      * metric_types with retain_days = 0 are skipped (keep forever).
--      * Each batch is committed independently so the table stays accessible.
-- =============================================================================
DROP PROCEDURE IF EXISTS run_retention;

DELIMITER $$

CREATE PROCEDURE run_retention()
retention_proc: BEGIN
    DECLARE v_metric_type   VARCHAR(30);
    DECLARE v_retain_days   INT UNSIGNED;
    DECLARE v_cutoff        DATETIME(3);
    DECLARE v_deleted       BIGINT UNSIGNED DEFAULT 0;
    DECLARE v_batch         BIGINT UNSIGNED;
    DECLARE v_done          INT DEFAULT 0;

    -- Cursor over all retention policies that have a finite retention window
    DECLARE policy_cursor CURSOR FOR
        SELECT metric_type, retain_days
        FROM   retention_policies
        WHERE  retain_days > 0
        ORDER BY metric_type;

    DECLARE CONTINUE HANDLER FOR NOT FOUND SET v_done = 1;

    OPEN policy_cursor;

    policy_loop: LOOP
        FETCH policy_cursor INTO v_metric_type, v_retain_days;
        IF v_done THEN
            LEAVE policy_loop;
        END IF;

        SET v_cutoff  = DATE_SUB(NOW(3), INTERVAL v_retain_days DAY);
        SET v_deleted = 0;

        -- Batched delete loop
        batch_loop: LOOP
            DELETE FROM metrics
            WHERE  metric_type = v_metric_type
              AND  recorded_at < v_cutoff
              AND  quality     != 'quarantine'
            LIMIT  500;

            SET v_batch = ROW_COUNT();
            SET v_deleted = v_deleted + v_batch;

            -- Commit each batch to keep transactions short
            COMMIT;

            IF v_batch < 500 THEN
                LEAVE batch_loop;
            END IF;
        END LOOP batch_loop;

        -- Log the run result
        INSERT INTO retention_log (metric_type, cutoff_date, rows_deleted, status)
        VALUES (v_metric_type, v_cutoff, v_deleted, 'ok');

    END LOOP policy_loop;

    CLOSE policy_cursor;
END$$

DELIMITER ;


-- =============================================================================
-- D. MYSQL EVENT — daily retention sweep
--    Requires the MySQL Event Scheduler to be ON:
--      SET GLOBAL event_scheduler = ON;
--    The Rust service (Issue #5) runs its own cron at 02:00 server time.
--    This event fires at 03:00 as a safety net if the service missed its window.
-- =============================================================================
DROP EVENT IF EXISTS evt_daily_retention;

CREATE EVENT evt_daily_retention
    ON SCHEDULE
        EVERY 1 DAY
        STARTS (DATE(NOW()) + INTERVAL 1 DAY + INTERVAL 3 HOUR)
    ON COMPLETION PRESERVE
    COMMENT 'Fallback daily retention sweep — primary runner is the Rust background task'
    DO CALL run_retention();

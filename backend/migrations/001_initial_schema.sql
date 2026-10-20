-- =============================================================================
-- HEIMDALL — Initial Schema
-- Migration: 001_initial_schema.sql
-- Database:  heimdall (MySQL Community Edition)
-- Author:    Yaswini (Issue #3)
-- =============================================================================
-- Design rules
--   * All PRIMARY KEYs are BIGINT UNSIGNED AUTO_INCREMENT (surrogate).
--   * All foreign keys are named fk_<child_table>_<parent_table>.
--   * Timestamps use DATETIME(3) (millisecond precision) and are always UTC.
--   * TEXT/VARCHAR lengths follow actual content needs; no 255 everywhere.
--   * The metrics table is INSERT-only; no UPDATE or DELETE is ever issued on
--     it by the application.  Enforced at the application role level (see docs).
--   * Soft deletes: most entities use `deleted_at` rather than hard deletes so
--     history is preserved. Exceptions: audit_log (append-only, never deleted).
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. OFFICES
--    One row per registered government institution.
--    office_code is the shared enrolment ticket issued to an institution.
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS offices (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,

    -- Human-readable name, e.g. "Batticaloa District Secretariat"
    name                VARCHAR(200)    NOT NULL,

    -- Province / district the office belongs to (informational, not PK)
    province            VARCHAR(100)    NOT NULL DEFAULT '',
    district            VARCHAR(100)    NOT NULL DEFAULT '',

    -- The enrolment ticket issued to this office.
    -- Format: HMD-XXXX-XXXX-XXXX (Crockford base32, 60 bits entropy).
    -- Stored as the Argon2id hash — never the plaintext.
    office_code_hash    VARCHAR(255)    NOT NULL,

    -- Maximum number of computers this office may enrol.
    -- Equal to the actual computer count declared in the formal request (FR5).
    max_computers       SMALLINT UNSIGNED NOT NULL DEFAULT 1,

    -- Lifecycle
    is_active           TINYINT(1)      NOT NULL DEFAULT 1,
    -- If the office code is revoked the maintainer sets this; a new code can
    -- be issued by creating a new row or updating office_code_hash + resetting
    -- code_revoked_at.
    code_revoked_at     DATETIME(3)     NULL,
    -- Expiry of the current office code (NULL = no expiry set yet)
    code_expires_at     DATETIME(3)     NULL,

    created_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                                ON UPDATE CURRENT_TIMESTAMP(3),
    deleted_at          DATETIME(3)     NULL,

    PRIMARY KEY (id),
    UNIQUE KEY uq_offices_name (name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- -----------------------------------------------------------------------------
-- 2. COMPUTERS
--    One row per enrolled computer.  Computers belong to an office.
--    Identity is established by the device certificate; private keys never
--    leave the computer (SR-A2).
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS computers (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    office_id           BIGINT UNSIGNED NOT NULL,

    -- Server-assigned device identifier (matches cert serial).
    device_id           CHAR(36)        NOT NULL COMMENT 'UUID v4',

    -- Anonymised label shown on the public dashboard, e.g. "Computer 3".
    -- Assigned randomly at enrolment, never in install order (SR-H1).
    display_label       VARCHAR(20)     NOT NULL,

    -- SHA-256 of the device public key (Ed25519), hex-encoded.
    -- Used to detect clones: same fingerprint on two active device_ids → clone.
    pubkey_fingerprint  CHAR(64)        NOT NULL,

    -- Machine fingerprint: HMAC(pepper, stable hardware IDs).
    -- Stored hashed so we can detect re-enrolment (SR-A6) without storing PII.
    machine_fingerprint_hash VARCHAR(255) NOT NULL,

    -- Device certificate serial (issued by HEIMDALL private CA).
    cert_serial         VARCHAR(64)     NOT NULL DEFAULT '',
    cert_expires_at     DATETIME(3)     NULL,

    -- Enrolment & lifecycle
    enrolled_at         DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    -- NULL = fully approved; NOT NULL = held for maintainer review (SR-A4)
    pending_approval_at DATETIME(3)     NULL,
    -- Who approved/rejected (FK to users.id), NULL if still pending
    reviewed_by         BIGINT UNSIGNED NULL,
    reviewed_at         DATETIME(3)     NULL,

    -- Revocation (SR-A5)
    revoked_at          DATETIME(3)     NULL,
    revoked_by          BIGINT UNSIGNED NULL,
    revocation_reason   VARCHAR(200)    NULL,

    -- Soft-delete for fully removed computers
    deleted_at          DATETIME(3)     NULL,

    created_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                                ON UPDATE CURRENT_TIMESTAMP(3),

    PRIMARY KEY (id),
    UNIQUE KEY uq_computers_device_id   (device_id),
    UNIQUE KEY uq_computers_cert_serial (cert_serial),

    KEY idx_computers_office_id     (office_id),
    KEY idx_computers_pubkey_fp     (pubkey_fingerprint),

    CONSTRAINT fk_computers_office
        FOREIGN KEY (office_id) REFERENCES offices (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- -----------------------------------------------------------------------------
-- 3. METRICS
--    Append-only time-series table.  One row per measurement event.
--    The application DB role has INSERT + SELECT only — no UPDATE, no DELETE.
--
--    metric_type values (enum-like, enforced by CHECK):
--      'heartbeat'   — 60 s alive signal; no numeric values required
--      'latency'     — periodic latency/jitter/loss measurement
--      'throughput'  — daily speed test (down + up Mbps)
--      'link_change' — network interface or ISP context change event
--      'outage_start'— computer detected it lost connectivity
--      'outage_end'  — connectivity restored; duration in value_ms
--      'shutdown'    — agent clean-shutdown marker
--      'boot'        — agent boot marker
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS metrics (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    computer_id         BIGINT UNSIGNED NOT NULL,
    office_id           BIGINT UNSIGNED NOT NULL,  -- denormalised for fast queries

    -- Measurement type
    metric_type         ENUM(
                            'heartbeat',
                            'latency',
                            'throughput',
                            'link_change',
                            'outage_start',
                            'outage_end',
                            'shutdown',
                            'boot'
                        ) NOT NULL,

    -- Server receive timestamp (authoritative for the archive — SR-C2).
    -- Indexed for all time-range queries.
    recorded_at         DATETIME(3)     NOT NULL,

    -- Agent-reported wall-clock time (advisory; clock skew is recorded).
    agent_ts            DATETIME(3)     NULL,
    -- Monotonic clock offset vs. server time in milliseconds.
    agent_clock_skew_ms INT             NULL,

    -- Strictly increasing per-device sequence number.
    -- Server rejects seq <= last accepted seq for this device (no replay, SR-C2).
    seq                 BIGINT UNSIGNED NOT NULL,

    -- Hash-chain: SHA-256 hex of the previous metric row for this computer.
    -- NULL only for the very first row after enrolment.
    prev_hash           CHAR(64)        NULL,
    -- SHA-256 hex of the canonical payload for this row (for Merkle inclusion).
    payload_hash        CHAR(64)        NOT NULL,

    -- Ed25519 signature over the canonical envelope (base64url, ~88 chars).
    signature           VARCHAR(120)    NOT NULL,

    -- -------------------------------------------------------------------------
    -- Numeric measurements  (NULL when not applicable for the metric_type)
    -- -------------------------------------------------------------------------

    -- Latency / heartbeat
    latency_ms          FLOAT           NULL COMMENT 'Round-trip time, milliseconds',
    jitter_ms           FLOAT           NULL COMMENT 'Jitter (std-dev of RTT), ms',
    packet_loss_pct     FLOAT           NULL COMMENT 'Packet loss 0–100',

    -- Throughput (speed test)
    download_mbps       FLOAT           NULL,
    upload_mbps         FLOAT           NULL,

    -- Outage duration (populated on outage_end)
    outage_duration_ms  BIGINT UNSIGNED NULL,

    -- -------------------------------------------------------------------------
    -- Link context  (populated on link_change, or first row after boot)
    -- -------------------------------------------------------------------------
    link_type           ENUM('ethernet', 'wifi', 'mobile', 'other', 'unknown')
                                        NULL,
    -- Negotiated link speed from NIC driver, Mbps
    link_speed_mbps     FLOAT           NULL,
    -- ASN of the public IP (number only — /32 address is never stored, PR4)
    asn                 INT UNSIGNED    NULL,
    -- ISP name derived from ASN (informational)
    isp_name            VARCHAR(100)    NULL,

    -- -------------------------------------------------------------------------
    -- Quality flags set by the validator worker
    -- -------------------------------------------------------------------------
    -- 'ok'         — passed all anomaly checks, in the public record
    -- 'quarantine' — held pending maintainer review (SR-C3)
    -- 'approved'   — maintainer reviewed a quarantined row and approved it
    -- 'rejected'   — maintainer rejected it; excluded from public view
    quality             ENUM('ok', 'quarantine', 'approved', 'rejected')
                                        NOT NULL DEFAULT 'ok',
    quarantine_reason   VARCHAR(200)    NULL,
    reviewed_by         BIGINT UNSIGNED NULL,
    reviewed_at         DATETIME(3)     NULL,

    PRIMARY KEY (id),

    -- Core time-range query index (most dashboard queries filter on this)
    KEY idx_metrics_computer_time   (computer_id, recorded_at),
    KEY idx_metrics_office_time     (office_id,   recorded_at),
    KEY idx_metrics_type_time       (metric_type, recorded_at),
    -- Sequence uniqueness per device (prevents replay)
    UNIQUE KEY uq_metrics_device_seq (computer_id, seq),
    -- Payload hash lookup for Merkle proof endpoint
    KEY idx_metrics_payload_hash    (payload_hash),

    CONSTRAINT fk_metrics_computer
        FOREIGN KEY (computer_id) REFERENCES computers (id),
    CONSTRAINT fk_metrics_office
        FOREIGN KEY (office_id)   REFERENCES offices   (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- -----------------------------------------------------------------------------
-- 4. ALERTS
--    Anomaly and system-generated alerts for the admin dashboard.
--    Covers: office-limit exceeded, clone detection, off-site flag, anomaly
--    quarantine triggers, etc.
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS alerts (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,

    -- Optional links to related entities (NULLable — some alerts are global)
    office_id           BIGINT UNSIGNED NULL,
    computer_id         BIGINT UNSIGNED NULL,
    metric_id           BIGINT UNSIGNED NULL,

    alert_type          VARCHAR(60)     NOT NULL COMMENT
                            'e.g. clone_detected, enrol_limit_exceeded, off_site, anomaly_quarantine, replay_attempt',

    severity            ENUM('info', 'warning', 'critical') NOT NULL DEFAULT 'warning',

    -- Human-readable description generated by the system.
    description         VARCHAR(500)    NOT NULL DEFAULT '',

    -- Resolution lifecycle
    -- 'open'     — unacknowledged
    -- 'ack'      — acknowledged by a maintainer; investigation in progress
    -- 'resolved' — closed with a resolution note
    -- 'false_positive' — reviewed and dismissed
    status              ENUM('open', 'ack', 'resolved', 'false_positive')
                                        NOT NULL DEFAULT 'open',
    resolved_by         BIGINT UNSIGNED NULL,
    resolved_at         DATETIME(3)     NULL,
    resolution_note     VARCHAR(500)    NULL,

    created_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                                ON UPDATE CURRENT_TIMESTAMP(3),

    PRIMARY KEY (id),
    KEY idx_alerts_office      (office_id),
    KEY idx_alerts_computer    (computer_id),
    KEY idx_alerts_type_status (alert_type, status),
    KEY idx_alerts_created     (created_at),

    CONSTRAINT fk_alerts_office
        FOREIGN KEY (office_id)   REFERENCES offices   (id),
    CONSTRAINT fk_alerts_computer
        FOREIGN KEY (computer_id) REFERENCES computers (id),
    CONSTRAINT fk_alerts_metric
        FOREIGN KEY (metric_id)   REFERENCES metrics   (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- -----------------------------------------------------------------------------
-- 5. USERS  (admin/maintainer accounts)
--    These are the HEIMDALL maintainers, not office staff.
--    Roles are additive flags stored as SET for simplicity; the backend
--    enforces role separation (SR-G2).
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS users (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,

    username            VARCHAR(60)     NOT NULL,
    email               VARCHAR(254)    NOT NULL,

    -- bcrypt hash of the password.  Multi-step sign-in also requires a
    -- WebAuthn hardware key (SR-G1) — that state is tracked in webauthn_credentials.
    password_hash       VARCHAR(255)    NOT NULL,

    -- Additive role flags (SR-G2).
    -- institution_approver : approve/reject new office enrolments
    -- device_manager       : approve/revoke individual computers
    -- release_publisher    : publish agent releases (requires 2 reviewers, SR-F5)
    -- data_reviewer        : review quarantined metrics
    -- auditor              : read-only access to audit log
    roles               SET(
                            'institution_approver',
                            'device_manager',
                            'release_publisher',
                            'data_reviewer',
                            'auditor'
                        ) NOT NULL DEFAULT 'auditor',

    is_active           TINYINT(1)      NOT NULL DEFAULT 1,
    last_login_at       DATETIME(3)     NULL,
    created_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                                ON UPDATE CURRENT_TIMESTAMP(3),
    deleted_at          DATETIME(3)     NULL,

    PRIMARY KEY (id),
    UNIQUE KEY uq_users_username (username),
    UNIQUE KEY uq_users_email    (email)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- Add FK references that depend on users (computers.reviewed_by, etc.)
ALTER TABLE computers
    ADD CONSTRAINT fk_computers_reviewed_by
        FOREIGN KEY (reviewed_by) REFERENCES users (id),
    ADD CONSTRAINT fk_computers_revoked_by
        FOREIGN KEY (revoked_by)  REFERENCES users (id);

ALTER TABLE metrics
    ADD CONSTRAINT fk_metrics_reviewed_by
        FOREIGN KEY (reviewed_by) REFERENCES users (id);

ALTER TABLE alerts
    ADD CONSTRAINT fk_alerts_resolved_by
        FOREIGN KEY (resolved_by) REFERENCES users (id);


-- -----------------------------------------------------------------------------
-- 6. WEBAUTHN_CREDENTIALS
--    Stores WebAuthn authenticator registrations per user (hardware key, SR-G1).
--    One user may register multiple keys (backup key).
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS webauthn_credentials (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    user_id             BIGINT UNSIGNED NOT NULL,

    -- Credential ID from the authenticator (base64url, up to 1023 bytes per spec)
    credential_id       VARCHAR(1024)   NOT NULL,
    -- CBOR-encoded public key, base64url
    public_key          TEXT            NOT NULL,
    -- AAGUID identifies the authenticator model
    aaguid              CHAR(36)        NOT NULL DEFAULT '',
    -- Sign counter to detect cloned authenticators
    sign_count          BIGINT UNSIGNED NOT NULL DEFAULT 0,

    friendly_name       VARCHAR(100)    NOT NULL DEFAULT 'Security Key',
    registered_at       DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    last_used_at        DATETIME(3)     NULL,

    PRIMARY KEY (id),
    KEY idx_webauthn_user (user_id),

    CONSTRAINT fk_webauthn_user
        FOREIGN KEY (user_id) REFERENCES users (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- -----------------------------------------------------------------------------
-- 7. AUDIT_LOG
--    Append-only record of every maintainer action (SR-G3).
--    The application role has INSERT + SELECT only.
--    Rows are hash-chained; any gap or fork is detectable.
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS audit_log (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,

    -- Actor (NULL = system-generated event)
    user_id             BIGINT UNSIGNED NULL,
    -- IP address of the admin browser session (hashed, not raw — PR4)
    ip_hash             VARCHAR(64)     NULL,

    -- What happened
    action              VARCHAR(80)     NOT NULL COMMENT
                            'e.g. office.approve, computer.revoke, metric.quarantine_approve, release.publish',
    -- Target entity
    target_type         VARCHAR(40)     NULL COMMENT 'e.g. office, computer, metric, release',
    target_id           BIGINT UNSIGNED NULL,

    -- Structured details as JSON (no PII — only IDs and status transitions)
    details             JSON            NULL,

    -- Hash chain: SHA-256(hex) of the previous audit_log row.
    -- NULL for row id=1.
    prev_hash           CHAR(64)        NULL,
    row_hash            CHAR(64)        NOT NULL COMMENT 'SHA-256 of this row content',

    created_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),

    PRIMARY KEY (id),
    KEY idx_audit_user      (user_id),
    KEY idx_audit_action    (action),
    KEY idx_audit_target    (target_type, target_id),
    KEY idx_audit_created   (created_at),

    CONSTRAINT fk_audit_user
        FOREIGN KEY (user_id) REFERENCES users (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;


-- -----------------------------------------------------------------------------
-- 8. DATA RETENTION CONFIG
--    Per-metric-type retention rules, read by the background retention job.
--    Allows the maintainer to tune retention without a code change.
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS retention_policies (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    metric_type         VARCHAR(30)     NOT NULL,  -- matches metrics.metric_type or '*'
    retain_days         INT UNSIGNED    NOT NULL COMMENT 'Days to keep; 0 = forever',
    description         VARCHAR(200)    NOT NULL DEFAULT '',
    updated_at          DATETIME(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                                ON UPDATE CURRENT_TIMESTAMP(3),
    PRIMARY KEY (id),
    UNIQUE KEY uq_retention_metric_type (metric_type)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Default retention rules
INSERT INTO retention_policies (metric_type, retain_days, description) VALUES
    ('heartbeat',    90,  'Heartbeats: keep 90 days (availability view); compress older'),
    ('latency',      365, 'Latency/jitter/loss: keep 1 year full resolution'),
    ('throughput',   0,   'Throughput: keep forever (public archive, SR-C4)'),
    ('link_change',  365, 'Link context changes: keep 1 year'),
    ('outage_start', 0,   'Outage events: keep forever (public archive)'),
    ('outage_end',   0,   'Outage events: keep forever (public archive)'),
    ('shutdown',     180, 'Shutdown markers: keep 6 months'),
    ('boot',         180, 'Boot markers: keep 6 months');

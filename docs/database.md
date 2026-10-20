# HEIMDALL — Database Schema

> **Owner:** Yaswini · **Issue:** #3  
> **Database:** MySQL 8.x Community Edition  
> **Engine:** InnoDB · **Charset:** utf8mb4 / utf8mb4\_unicode\_ci  
> **Timestamps:** all `DATETIME(3)` — millisecond precision, always UTC

---

## Contents

1. [Design rules](#1-design-rules)
2. [Entity-relationship overview](#2-entity-relationship-overview)
3. [Tables](#3-tables)
   - [offices](#31-offices)
   - [computers](#32-computers)
   - [metrics](#33-metrics)
   - [alerts](#34-alerts)
   - [users](#35-users)
   - [webauthn\_credentials](#36-webauthn_credentials)
   - [audit\_log](#37-audit_log)
   - [retention\_policies](#38-retention_policies)
   - [retention\_log](#39-retention_log)
4. [Indexes](#4-indexes)
5. [Data retention](#5-data-retention)
6. [Application roles and permissions](#6-application-roles-and-permissions)
7. [Migrations](#7-migrations)
8. [Connection pool settings](#8-connection-pool-settings)

---

## 1. Design rules

| Rule | Detail |
|---|---|
| Primary keys | `BIGINT UNSIGNED AUTO_INCREMENT` surrogate on every table |
| Foreign keys | Named `fk_<child>_<parent>`, enforced by InnoDB |
| Timestamps | `DATETIME(3)` UTC; `created_at` / `updated_at` on mutable tables |
| Soft deletes | `deleted_at DATETIME(3) NULL`; hard deletes only via retention sweep |
| Append-only tables | `metrics` and `audit_log` — application role granted `INSERT + SELECT` only, no `UPDATE` or `DELETE` |
| No PII stored | Public IP `/32` is never stored — only the ASN number. Machine fingerprints are HMAC-hashed |
| Secrets | Office codes stored as Argon2id hashes, never plaintext |

---

## 2. Entity-relationship overview

```
offices  1──────< computers >──────1  users (reviewed_by / revoked_by)
   |                  |
   |                  |
   └─────────< metrics >──────────── users (reviewed_by)
                  |
                  └──────────< alerts
                                 |
                              users (resolved_by)

users 1──────< webauthn_credentials

audit_log   (append-only, references users)
retention_policies  1──────< retention_log
```

---

## 3. Tables

### 3.1 `offices`

One row per registered government institution. An office must be approved by a maintainer before it can enrol computers.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | Surrogate key |
| `name` | `VARCHAR(200)` UNIQUE | Human-readable name, e.g. *Batticaloa District Secretariat* |
| `province` | `VARCHAR(100)` | Province (informational) |
| `district` | `VARCHAR(100)` | District (informational) |
| `office_code_hash` | `VARCHAR(255)` | Argon2id hash of the enrolment ticket — never the plaintext |
| `max_computers` | `SMALLINT UNSIGNED` | Maximum computers this office may enrol (FR5) |
| `is_active` | `TINYINT(1)` | `1` = active, `0` = suspended |
| `code_revoked_at` | `DATETIME(3)` NULL | Set when the office code is revoked (FR4) |
| `code_expires_at` | `DATETIME(3)` NULL | Expiry of the current office code |
| `created_at` | `DATETIME(3)` | Row creation time |
| `updated_at` | `DATETIME(3)` | Auto-updated on any change |
| `deleted_at` | `DATETIME(3)` NULL | Soft delete |

**Indexes:** `PRIMARY (id)`, `UNIQUE (name)`

---

### 3.2 `computers`

One row per enrolled computer. Identity is established by a device certificate issued by the HEIMDALL private CA; the private key never leaves the computer (SR-A2).

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | Surrogate key |
| `office_id` | `BIGINT UNSIGNED` FK → `offices.id` | Which office this computer belongs to |
| `device_id` | `CHAR(36)` UNIQUE | UUID v4 assigned at enrolment, matches cert serial context |
| `display_label` | `VARCHAR(20)` | Anonymised public label, e.g. *Computer 3* — randomly assigned, never install-ordered (SR-H1) |
| `pubkey_fingerprint` | `CHAR(64)` | SHA-256 hex of the Ed25519 public key — detects cloned installations (SR-A2) |
| `machine_fingerprint_hash` | `VARCHAR(255)` | HMAC(pepper, stable hardware IDs) — detects re-enrolments (SR-A6) |
| `cert_serial` | `VARCHAR(64)` UNIQUE | Device certificate serial from HEIMDALL private CA |
| `cert_expires_at` | `DATETIME(3)` NULL | Certificate expiry; agent auto-renews before this |
| `enrolled_at` | `DATETIME(3)` | When enrolment completed |
| `pending_approval_at` | `DATETIME(3)` NULL | `NOT NULL` = held for maintainer review (SR-A4) |
| `reviewed_by` | `BIGINT UNSIGNED` FK → `users.id` NULL | Maintainer who approved/rejected |
| `reviewed_at` | `DATETIME(3)` NULL | When reviewed |
| `revoked_at` | `DATETIME(3)` NULL | Set on revocation (SR-A5) |
| `revoked_by` | `BIGINT UNSIGNED` FK → `users.id` NULL | Maintainer who revoked |
| `revocation_reason` | `VARCHAR(200)` NULL | Human-readable reason |
| `deleted_at` | `DATETIME(3)` NULL | Soft delete |
| `created_at` | `DATETIME(3)` | — |
| `updated_at` | `DATETIME(3)` | — |

**Indexes:** `PRIMARY (id)`, `UNIQUE (device_id)`, `UNIQUE (cert_serial)`, `idx_computers_office_id`, `idx_computers_pubkey_fp`, `idx_computers_pending`, `idx_computers_office_active`

---

### 3.3 `metrics`

**Append-only.** One row per measurement event from an agent. The application role has `INSERT + SELECT` only — no `UPDATE` or `DELETE`.

Every row is signed by the device at the moment of measurement (SR-C1). The `seq` uniqueness constraint blocks replays at the DB level (SR-C2). Rows are hash-chained per device via `prev_hash` — any gap or fork is detectable.

#### metric\_type values

| Value | Meaning | Key numeric columns |
|---|---|---|
| `heartbeat` | 60 s alive signal | `latency_ms` (optional) |
| `latency` | Periodic latency / jitter / loss measurement | `latency_ms`, `jitter_ms`, `packet_loss_pct` |
| `throughput` | Daily speed test | `download_mbps`, `upload_mbps` |
| `link_change` | NIC or ISP context changed | `link_type`, `link_speed_mbps`, `asn`, `isp_name` |
| `outage_start` | Agent detected connectivity loss | — |
| `outage_end` | Connectivity restored | `outage_duration_ms` |
| `shutdown` | Agent clean-shutdown marker | — |
| `boot` | Agent boot marker | — |

#### quality values

| Value | Meaning |
|---|---|
| `ok` | Passed all anomaly checks — visible in the public record |
| `quarantine` | Flagged by the validator; held for maintainer review (SR-C3) |
| `approved` | Maintainer reviewed a quarantined row and approved it |
| `rejected` | Maintainer rejected it — excluded from public view |

#### Columns

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | — |
| `computer_id` | `BIGINT UNSIGNED` FK → `computers.id` | — |
| `office_id` | `BIGINT UNSIGNED` FK → `offices.id` | Denormalised for fast office-level queries |
| `metric_type` | `ENUM(…)` | See table above |
| `recorded_at` | `DATETIME(3)` | **Server receive time** — authoritative for the archive (SR-C2) |
| `agent_ts` | `DATETIME(3)` NULL | Agent wall-clock time (advisory only) |
| `agent_clock_skew_ms` | `INT` NULL | `server_time − agent_time` in milliseconds |
| `seq` | `BIGINT UNSIGNED` | Strictly increasing per device; UNIQUE with `computer_id` — blocks replays |
| `prev_hash` | `CHAR(64)` NULL | SHA-256 hex of the previous row for this device (hash chain) |
| `payload_hash` | `CHAR(64)` | SHA-256 hex of the canonical payload (used for Merkle proof) |
| `signature` | `VARCHAR(120)` | Base64url Ed25519 signature over the canonical envelope |
| `latency_ms` | `FLOAT` NULL | Round-trip time, ms |
| `jitter_ms` | `FLOAT` NULL | Std-dev of RTT, ms |
| `packet_loss_pct` | `FLOAT` NULL | 0–100 |
| `download_mbps` | `FLOAT` NULL | — |
| `upload_mbps` | `FLOAT` NULL | — |
| `outage_duration_ms` | `BIGINT UNSIGNED` NULL | Populated on `outage_end` |
| `link_type` | `ENUM('ethernet','wifi','mobile','other','unknown')` NULL | — |
| `link_speed_mbps` | `FLOAT` NULL | Negotiated NIC link speed |
| `asn` | `INT UNSIGNED` NULL | ASN of public IP (the `/32` address is never stored — PR4) |
| `isp_name` | `VARCHAR(100)` NULL | Derived from ASN, informational |
| `quality` | `ENUM('ok','quarantine','approved','rejected')` | Default `ok` |
| `quarantine_reason` | `VARCHAR(200)` NULL | — |
| `reviewed_by` | `BIGINT UNSIGNED` FK → `users.id` NULL | — |
| `reviewed_at` | `DATETIME(3)` NULL | — |

**Indexes:** see [§4 Indexes](#4-indexes).

---

### 3.4 `alerts`

System-generated anomaly and security alerts surfaced on the admin dashboard.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | — |
| `office_id` | `BIGINT UNSIGNED` FK → `offices.id` NULL | NULL for global alerts |
| `computer_id` | `BIGINT UNSIGNED` FK → `computers.id` NULL | — |
| `metric_id` | `BIGINT UNSIGNED` FK → `metrics.id` NULL | The metric row that triggered the alert |
| `alert_type` | `VARCHAR(60)` | e.g. `clone_detected`, `enrol_limit_exceeded`, `off_site`, `anomaly_quarantine`, `replay_attempt` |
| `severity` | `ENUM('info','warning','critical')` | — |
| `description` | `VARCHAR(500)` | Human-readable system-generated message |
| `status` | `ENUM('open','ack','resolved','false_positive')` | Lifecycle state |
| `resolved_by` | `BIGINT UNSIGNED` FK → `users.id` NULL | — |
| `resolved_at` | `DATETIME(3)` NULL | — |
| `resolution_note` | `VARCHAR(500)` NULL | — |
| `created_at` | `DATETIME(3)` | — |
| `updated_at` | `DATETIME(3)` | — |

**Indexes:** `PRIMARY (id)`, `idx_alerts_office`, `idx_alerts_computer`, `idx_alerts_type_status`, `idx_alerts_created`, `idx_alerts_office_open`

---

### 3.5 `users`

HEIMDALL maintainer accounts. These are not office staff — they are the people who approve enrolments, manage computers, and publish releases.

Password-only sign-in is never sufficient; a registered WebAuthn hardware key is also required (SR-G1).

#### roles (SET)

| Value | Permission |
|---|---|
| `institution_approver` | Approve or reject new office enrolments |
| `device_manager` | Approve or revoke individual computers |
| `release_publisher` | Publish agent releases (requires 2 reviewers, SR-F5) |
| `data_reviewer` | Review quarantined metric rows |
| `auditor` | Read-only access to the audit log (default) |

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | — |
| `username` | `VARCHAR(60)` UNIQUE | — |
| `email` | `VARCHAR(254)` UNIQUE | — |
| `password_hash` | `VARCHAR(255)` | bcrypt hash |
| `roles` | `SET(…)` | Additive role flags; default `auditor` |
| `is_active` | `TINYINT(1)` | `0` = account suspended |
| `last_login_at` | `DATETIME(3)` NULL | — |
| `created_at` | `DATETIME(3)` | — |
| `updated_at` | `DATETIME(3)` | — |
| `deleted_at` | `DATETIME(3)` NULL | Soft delete |

---

### 3.6 `webauthn_credentials`

One row per registered WebAuthn authenticator per user. A user may register multiple keys (primary + backup).

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | — |
| `user_id` | `BIGINT UNSIGNED` FK → `users.id` | — |
| `credential_id` | `VARCHAR(1024)` | Authenticator credential ID, base64url |
| `public_key` | `TEXT` | CBOR-encoded public key, base64url |
| `aaguid` | `CHAR(36)` | Identifies the authenticator model |
| `sign_count` | `BIGINT UNSIGNED` | Increments each use; detects cloned authenticators |
| `friendly_name` | `VARCHAR(100)` | e.g. *YubiKey 5 NFC* |
| `registered_at` | `DATETIME(3)` | — |
| `last_used_at` | `DATETIME(3)` NULL | — |

---

### 3.7 `audit_log`

**Append-only.** Every maintainer action is recorded here (SR-G3). The application role has `INSERT + SELECT` only. Rows are hash-chained via `prev_hash` — any gap or tampered row breaks the chain.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | — |
| `user_id` | `BIGINT UNSIGNED` FK → `users.id` NULL | NULL for system-generated events |
| `ip_hash` | `VARCHAR(64)` NULL | SHA-256 of the admin browser IP — not the raw address (PR4) |
| `action` | `VARCHAR(80)` | e.g. `office.approve`, `computer.revoke`, `metric.quarantine_approve`, `release.publish` |
| `target_type` | `VARCHAR(40)` NULL | e.g. `office`, `computer`, `metric`, `release` |
| `target_id` | `BIGINT UNSIGNED` NULL | PK of the affected row |
| `details` | `JSON` NULL | Structured change details — IDs and status transitions, no PII |
| `prev_hash` | `CHAR(64)` NULL | SHA-256 hex of the previous row (NULL for row 1) |
| `row_hash` | `CHAR(64)` | SHA-256 hex of this row's content |
| `created_at` | `DATETIME(3)` | — |

**Indexes:** `PRIMARY (id)`, `idx_audit_user`, `idx_audit_action`, `idx_audit_target`, `idx_audit_created`, `idx_audit_user_time`

---

### 3.8 `retention_policies`

Configurable per-type data retention windows. Read by the background retention task on every run. A `retain_days` of `0` means keep forever.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | — |
| `metric_type` | `VARCHAR(30)` UNIQUE | Matches `metrics.metric_type` values |
| `retain_days` | `INT UNSIGNED` | Days to keep; `0` = forever |
| `description` | `VARCHAR(200)` | Human-readable note |
| `updated_at` | `DATETIME(3)` | — |

**Default seed data:**

| metric\_type | retain\_days | Reason |
|---|---:|---|
| `heartbeat` | 90 | Availability overview; older data compressible |
| `latency` | 365 | Full-year resolution for charts |
| `throughput` | **0** | Keep forever — public archive (PRD §14.1) |
| `link_change` | 365 | ISP context history |
| `outage_start` | **0** | Keep forever — public archive |
| `outage_end` | **0** | Keep forever — public archive |
| `shutdown` | 180 | Boot/shutdown lifecycle — 6 months sufficient |
| `boot` | 180 | — |

---

### 3.9 `retention_log`

One row written per policy per retention task run. Provides full traceability of every automated deletion.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGINT UNSIGNED` PK | — |
| `run_at` | `DATETIME(3)` | When this run executed |
| `metric_type` | `VARCHAR(30)` | Which policy was processed |
| `cutoff_date` | `DATETIME(3)` | Rows with `recorded_at < cutoff_date` were eligible |
| `rows_deleted` | `BIGINT UNSIGNED` | Total rows deleted in this run |
| `status` | `ENUM('ok','partial','error')` | `partial` = deleted some rows then hit an error |
| `error_message` | `VARCHAR(500)` NULL | Set when `status != 'ok'` |

---

## 4. Indexes

All covering indexes live in migration `002_indexes_and_retention.sql`.

### `metrics` table indexes

| Index name | Columns | Serves |
|---|---|---|
| `PRIMARY` | `id` | — |
| `uq_metrics_device_seq` | `(computer_id, seq)` UNIQUE | Replay prevention — DB-level guarantee |
| `idx_metrics_computer_time` | `(computer_id, recorded_at)` | General computer time-range scan |
| `idx_metrics_office_time` | `(office_id, recorded_at)` | General office time-range scan |
| `idx_metrics_type_time` | `(metric_type, recorded_at)` | Type-scoped time scan |
| `idx_metrics_payload_hash` | `(payload_hash)` | Merkle proof lookup |
| `idx_metrics_latest_per_computer` | `(computer_id, metric_type, quality, recorded_at DESC)` | **Fetch latest** — single-row read, no filesort |
| `idx_metrics_history_range` | `(computer_id, metric_type, recorded_at, quality)` | **History by time range** — computer chart data |
| `idx_metrics_office_summary` | `(office_id, quality, recorded_at, metric_type)` | Office dashboard aggregation |
| `idx_metrics_office_last_seen` | `(quality, metric_type, office_id, recorded_at DESC)` | Public map live pin (last heartbeat per office) |
| `idx_metrics_outages` | `(office_id, metric_type, recorded_at, computer_id, outage_duration_ms)` | Outage history page |
| `idx_metrics_retention_sweep` | `(metric_type, recorded_at, quality)` | Retention task batched delete |

### `computers` table indexes

| Index name | Columns | Serves |
|---|---|---|
| `uq_computers_device_id` | `(device_id)` UNIQUE | Device lookup by UUID |
| `uq_computers_cert_serial` | `(cert_serial)` UNIQUE | Cert validation |
| `idx_computers_office_id` | `(office_id)` | List computers for an office |
| `idx_computers_pubkey_fp` | `(pubkey_fingerprint)` | Clone detection |
| `idx_computers_pending` | `(pending_approval_at, revoked_at, deleted_at)` | Pending-review queue |
| `idx_computers_office_active` | `(office_id, revoked_at, deleted_at)` | Active count check (FR5) |

### `alerts` table indexes

| Index name | Columns | Serves |
|---|---|---|
| `idx_alerts_office` | `(office_id)` | — |
| `idx_alerts_computer` | `(computer_id)` | — |
| `idx_alerts_type_status` | `(alert_type, status)` | Filter by type and lifecycle state |
| `idx_alerts_created` | `(created_at)` | Recent alerts list |
| `idx_alerts_office_open` | `(office_id, status, created_at DESC)` | Open alerts for an office |

### `audit_log` indexes

| Index name | Columns | Serves |
|---|---|---|
| `idx_audit_user` | `(user_id)` | — |
| `idx_audit_action` | `(action)` | Filter by action type |
| `idx_audit_target` | `(target_type, target_id)` | All actions on one entity |
| `idx_audit_created` | `(created_at)` | Chronological log view |
| `idx_audit_user_time` | `(user_id, created_at DESC)` | Recent actions by a specific maintainer |

---

## 5. Data retention

### How it works

The Rust background task (`backend/src/tasks/retention.rs`) runs on a configurable schedule (default: every 24 hours, first run 30 seconds after startup).

On each run it:

1. Reads `retention_policies` where `retain_days > 0`.
2. For each policy, calculates `cutoff = NOW() − retain_days`.
3. Deletes `metrics` rows in batches of `RETENTION_BATCH_SIZE` (default 500) using short individual DELETE statements to keep InnoDB lock contention low.
4. **Never deletes** rows where `quality = 'quarantine'` — those are under maintainer review.
5. Writes one row to `retention_log` per policy per run.

A MySQL fallback stored procedure (`run_retention`) and daily event (`evt_daily_retention`, fires at 03:00) provide a safety net if the Rust service is down.

### Configuring retention

Update `retention_policies` directly:

```sql
-- Example: extend latency retention to 2 years
UPDATE retention_policies
SET    retain_days = 730,
       description = 'Latency/jitter/loss: keep 2 years'
WHERE  metric_type = 'latency';
```

To keep a type forever, set `retain_days = 0`. The task will skip it entirely.

### What is never deleted

The following types have `retain_days = 0` in the seed data and are kept in full, indefinitely, per PRD §14.1:

- `throughput`
- `outage_start`
- `outage_end`

---

## 6. Application roles and permissions

Two database roles are used — create them after migrations:

```sql
-- Read/write role for the backend API and background tasks
CREATE USER 'heimdall_app'@'%' IDENTIFIED BY '<strong-password>';
GRANT SELECT, INSERT ON heimdall.metrics    TO 'heimdall_app'@'%';
GRANT SELECT, INSERT ON heimdall.audit_log  TO 'heimdall_app'@'%';
GRANT SELECT, INSERT, UPDATE, DELETE ON heimdall.offices              TO 'heimdall_app'@'%';
GRANT SELECT, INSERT, UPDATE, DELETE ON heimdall.computers            TO 'heimdall_app'@'%';
GRANT SELECT, INSERT, UPDATE, DELETE ON heimdall.alerts               TO 'heimdall_app'@'%';
GRANT SELECT, INSERT, UPDATE, DELETE ON heimdall.users                TO 'heimdall_app'@'%';
GRANT SELECT, INSERT, UPDATE, DELETE ON heimdall.webauthn_credentials TO 'heimdall_app'@'%';
GRANT SELECT, INSERT, UPDATE, DELETE ON heimdall.retention_policies   TO 'heimdall_app'@'%';
GRANT SELECT, INSERT                 ON heimdall.retention_log        TO 'heimdall_app'@'%';
FLUSH PRIVILEGES;

-- Read-only role for the public read API (dashboard queries only)
CREATE USER 'heimdall_ro'@'%' IDENTIFIED BY '<strong-password>';
GRANT SELECT ON heimdall.metrics   TO 'heimdall_ro'@'%';
GRANT SELECT ON heimdall.offices   TO 'heimdall_ro'@'%';
GRANT SELECT ON heimdall.computers TO 'heimdall_ro'@'%';
FLUSH PRIVILEGES;
```

> **Important:** The `metrics` and `audit_log` tables must never receive `UPDATE` or `DELETE` grants on `heimdall_app`. The append-only guarantee is enforced at this level, not just by application code.

---

## 7. Migrations

Migrations live in `backend/migrations/` and are run automatically at server startup via SQLx.

| File | Description |
|---|---|
| `001_initial_schema.sql` | All tables, foreign keys, primary indexes, default retention seed data |
| `002_indexes_and_retention.sql` | Covering indexes, `retention_log` table, `run_retention` stored procedure, `evt_daily_retention` MySQL event |

### Running manually

```bash
# Apply all pending migrations
cd backend
cargo sqlx migrate run

# Check migration status
cargo sqlx migrate info
```

### Regenerating the offline query cache

After changing any `sqlx::query!` or `sqlx::query_as!` call, regenerate the cache so CI can build without a live database:

```bash
cd backend
cargo sqlx prepare
# commit the updated .sqlx/ directory
```

---

## 8. Connection pool settings

Configured via environment variables (see `backend/.env.example`):

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | — | **Required.** Full MySQL connection URL |
| `DATABASE_MAX_CONNECTIONS` | `10` | Maximum open connections |
| `DATABASE_MIN_CONNECTIONS` | `2` | Minimum idle connections kept alive |
| `DATABASE_CONNECT_TIMEOUT_SECS` | `30` | Seconds to wait for a pool connection |
| `DATABASE_IDLE_TIMEOUT_SECS` | `600` | Seconds before idle connection is closed |
| `DATABASE_MAX_LIFETIME_SECS` | `1800` | Seconds before a connection is recycled |
| `RETENTION_SCHEDULE_SECS` | `86400` | Retention task run interval (24 h) |
| `RETENTION_BATCH_SIZE` | `500` | Rows deleted per batch |

-- Migration 002: C3 baseline-metric read VIEWs
--
-- Provides two read-only VIEWs used by the C3 instrumentation collectors.
-- Neither VIEW writes to events.db; both project rows that already exist in
-- the canonical `events` table (created in 001_initial.sql).
--
-- IMPORTANT: There is no `c3_mirror` SQL table. The UC-3 mirror sink writes
-- JSONL to ~/.hex-events/mirror/YYYY-MM-DD.jsonl (per the amended mirror-sink
-- contract: docs/superpowers/specs/2026-05-24-iii-hex-mirror-sink-contract.md).
-- If you find yourself adding a `CREATE TABLE c3_mirror` here, STOP — that is
-- explicitly out of scope.

-- ---------------------------------------------------------------------------
-- v_c3_quiet_failure_weekly
--
-- M2 quiet-failure weekly rollup. Buckets failure-class events into a
-- controlled category vocabulary and groups them by ISO week (UTC) using
-- strftime('%Y-%W'). Categories are matched ORDER-SENSITIVELY in the CASE
-- expression so that more-specific patterns win over wildcard policy.failed
-- matches.
--
-- Categories:
--   alert_critical           hex.alert.critical
--   alert_error              hex.alert.error
--   boi_integrity_violation  hex.boi.integrity.violation
--   policy_failed            hex.policy.<name>.failed  (wildcard)
--
-- Consumers (c3-quiet-failure-snapshot.py) filter the VIEW down to the
-- last-completed-week bucket and emit one hex.c3.quiet_failure.weekly_count
-- event per (week, category).
-- ---------------------------------------------------------------------------
CREATE VIEW IF NOT EXISTS v_c3_quiet_failure_weekly AS
SELECT
    strftime('%Y-%W', ts) AS week,
    CASE
        WHEN event_type = 'hex.alert.critical'         THEN 'alert_critical'
        WHEN event_type = 'hex.alert.error'            THEN 'alert_error'
        WHEN event_type = 'hex.boi.integrity.violation' THEN 'boi_integrity_violation'
        WHEN event_type LIKE 'hex.policy.%.failed'     THEN 'policy_failed'
        ELSE                                                'other'
    END AS category,
    COUNT(*) AS count,
    MIN(ts)  AS first_ts,
    MAX(ts)  AS last_ts
FROM events
WHERE event_type = 'hex.alert.critical'
   OR event_type = 'hex.alert.error'
   OR event_type = 'hex.boi.integrity.violation'
   OR event_type LIKE 'hex.policy.%.failed'
GROUP BY week, category;

-- ---------------------------------------------------------------------------
-- v_c3_orphan_scan_daily
--
-- M1 orphan-detection rate, projected one row per emitted scan event. The
-- daily collector (c3-orphan-scan.py) emits exactly one hex.c3.orphan.scan
-- event per run with per-scope breakdowns in payload. This VIEW exposes the
-- per-day projection that downstream tooling and dashboards consume.
--
-- Payload contract (set by c3-orphan-scan.py):
--   payload.total_orphans  INTEGER  total orphan refs across all scopes
--   payload.scopes         OBJECT   per-scope { scanned, orphans } map
--   payload.warnings       ARRAY    cold-start markers (e.g. 'boi_v2_specs_empty')
--
-- The VIEW is intentionally a simple per-row projection — aggregation, if
-- needed, is performed by the consumer. ORDER BY is deliberately omitted from
-- the VIEW definition (SQLite does not guarantee VIEW ordering); callers
-- should add ORDER BY day DESC if they need a sorted result.
-- ---------------------------------------------------------------------------
CREATE VIEW IF NOT EXISTS v_c3_orphan_scan_daily AS
SELECT
    DATE(ts)                                       AS day,
    ts                                             AS scanned_at,
    json_extract(payload, '$.total_orphans')       AS total_orphans,
    json_extract(payload, '$.scopes')              AS scopes,
    json_extract(payload, '$.warnings')            AS warnings,
    source                                         AS source
FROM events
WHERE event_type = 'hex.c3.orphan.scan';

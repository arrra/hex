#!/usr/bin/env python3
"""hex-vitals: rolling-24h system health scorer for hex/BOI."""

import json
import os
import sqlite3
import sys
import time
import urllib.request
import urllib.parse
import urllib.error
from datetime import datetime, timezone, timedelta
from glob import glob

# ── Paths ──────────────────────────────────────────────────────────────────
BOI_DB = os.path.expanduser("~/.boi/boi.db")
FEEDBACK_GLOB = os.path.expanduser(
    os.path.join(_CLAUDE_PROJECT, "feedback_*.md")
)
CACHE_FILE = "/tmp/hex-vitals-prev.json"

# ── Thresholds (from t-2 analysis) ────────────────────────────────────────
THRESHOLDS = {
    "completion_rate": {
        "healthy":  lambda v: v >= 0.80,
        "degraded": lambda v: 0.60 <= v < 0.80,
        # critical: < 0.60
    },
    "task_throughput": {
        "healthy":  lambda v: v >= 30,
        "degraded": lambda v: 10 <= v < 30,
        # critical: < 10
    },
    "zero_task_failures": {
        "healthy":  lambda v: v <= 1,
        "degraded": lambda v: 2 <= v <= 3,
        # critical: >= 4
    },
    "correction_frequency": {
        "healthy":  lambda v: v <= 3,
        "degraded": lambda v: 4 <= v <= 7,
        # critical: >= 8
    },
}


def classify(signal: str, value) -> str:
    t = THRESHOLDS[signal]
    if t["healthy"](value):
        return "healthy"
    if t["degraded"](value):
        return "degraded"
    return "critical"


# ── Data collection ────────────────────────────────────────────────────────
def rolling_cutoff() -> str:
    """ISO8601 timestamp 24h ago in UTC."""
    cutoff = datetime.now(timezone.utc) - timedelta(hours=24)
    return cutoff.isoformat()


def collect_boi_signals() -> dict:
    """Query boi.db for the three productivity signals."""
    cutoff = rolling_cutoff()
    result = {
        "completion_rate": None,
        "task_throughput": 0,
        "zero_task_failures": 0,
        "sample_size": 0,
    }

    if not os.path.exists(BOI_DB):
        result["_error"] = f"boi.db not found at {BOI_DB}"
        return result

    try:
        conn = sqlite3.connect(f"file:{BOI_DB}?mode=ro", uri=True)
        conn.row_factory = sqlite3.Row
        cur = conn.cursor()

        # Completion rate: terminal specs in rolling 24h
        cur.execute(
            """
            SELECT
                COUNT(*) FILTER (WHERE status = 'completed') AS completed,
                COUNT(*) FILTER (WHERE status = 'failed')    AS failed,
                SUM(tasks_done)                               AS throughput
            FROM specs
            WHERE submitted_at >= ? AND status IN ('completed', 'failed')
            """,
            (cutoff,),
        )
        row = cur.fetchone()
        completed = row["completed"] or 0
        failed = row["failed"] or 0
        terminal = completed + failed
        result["task_throughput"] = int(row["throughput"] or 0)
        result["sample_size"] = terminal

        if terminal >= 2:  # avoid single-spec noise
            result["completion_rate"] = round(completed / terminal, 4)
        else:
            result["completion_rate"] = None  # not enough data

        # Zero-task failures: specs that failed having never completed a task
        cur.execute(
            """
            SELECT COUNT(*) AS cnt
            FROM specs
            WHERE submitted_at >= ?
              AND status = 'failed'
              AND tasks_done = 0
              AND tasks_total > 0
            """,
            (cutoff,),
        )
        result["zero_task_failures"] = cur.fetchone()["cnt"] or 0

        conn.close()
    except Exception as exc:
        result["_error"] = str(exc)

    return result


def collect_correction_frequency() -> int:
    """Count feedback_*.md files modified in the last 24h."""
    cutoff_ts = time.time() - 86400
    files = glob(FEEDBACK_GLOB)
    return sum(1 for f in files if os.path.getmtime(f) >= cutoff_ts)


# ── Scoring ────────────────────────────────────────────────────────────────
def score() -> dict:
    boi = collect_boi_signals()
    correction_freq = collect_correction_frequency()

    signals = {}

    # Completion rate
    cr_val = boi["completion_rate"]
    if cr_val is None:
        cr_status = "healthy"  # not enough data → assume healthy
        cr_note = f"sample_size={boi['sample_size']} (< 2 terminal specs — skipping rate)"
    else:
        cr_status = classify("completion_rate", cr_val)
        cr_note = f"sample_size={boi['sample_size']}"
    signals["completion_rate"] = {
        "value": cr_val,
        "status": cr_status,
        "note": cr_note,
    }

    # Task throughput
    tp_val = boi["task_throughput"]
    signals["task_throughput"] = {
        "value": tp_val,
        "status": classify("task_throughput", tp_val),
    }

    # Zero-task failures
    ztf_val = boi["zero_task_failures"]
    signals["zero_task_failures"] = {
        "value": ztf_val,
        "status": classify("zero_task_failures", ztf_val),
    }

    # Correction frequency
    cf_val = correction_freq
    signals["correction_frequency"] = {
        "value": cf_val,
        "status": classify("correction_frequency", cf_val),
    }

    # Overall verdict
    statuses = [s["status"] for s in signals.values()]
    if any(s == "critical" for s in statuses):
        overall = "critical"
    elif any(s == "degraded" for s in statuses):
        overall = "degraded"
    else:
        overall = "healthy"

    output = {
        "ts": datetime.now(timezone.utc).isoformat(),
        "window_hours": 24,
        "overall": overall,
        "signals": signals,
    }

    if "_error" in boi:
        output["_error"] = boi["_error"]

    return output


# ── Human output ───────────────────────────────────────────────────────────
STATUS_EMOJI = {"healthy": "🟢", "degraded": "🟡", "critical": "🔴"}
TREND_UP = "↑"
TREND_DOWN = "↓"
TREND_FLAT = "→"

SIGNAL_LABELS = {
    "completion_rate":    "Completion rate ",
    "task_throughput":    "Task throughput ",
    "zero_task_failures": "Zero-task fails ",
    "correction_freq":    "Correction freq ",
}


def fmt_value(signal: str, value) -> str:
    if value is None:
        return "N/A"
    if signal == "completion_rate":
        return f"{value*100:.0f}%"
    return str(value)


def trend_arrow(signal: str, current, previous) -> str:
    if previous is None or current is None:
        return " "
    # For zero_task_failures and correction_frequency, lower is better
    if signal in ("zero_task_failures", "correction_frequency"):
        if current < previous:
            return TREND_DOWN  # improving
        if current > previous:
            return TREND_UP    # worsening
    else:
        if current > previous:
            return TREND_UP
        if current < previous:
            return TREND_DOWN
    return TREND_FLAT


def human_output(current: dict, prev: dict | None) -> str:
    lines = []
    ts = current["ts"][:19].replace("T", " ")
    overall = current["overall"]
    emoji = STATUS_EMOJI[overall]
    lines.append(f"hex-vitals  {ts} UTC  {emoji} {overall.upper()}")
    lines.append("─" * 44)

    prev_signals = (prev or {}).get("signals", {})

    signal_keys = [
        ("completion_rate",    "completion_rate"),
        ("task_throughput",    "task_throughput"),
        ("zero_task_failures", "zero_task_failures"),
        ("correction_freq",    "correction_frequency"),
    ]

    for label_key, sig_key in signal_keys:
        sig = current["signals"][sig_key]
        val = sig["value"]
        status = sig["status"]
        prev_val = prev_signals.get(sig_key, {}).get("value") if prev_signals else None
        arrow = trend_arrow(sig_key, val, prev_val)
        label = SIGNAL_LABELS[label_key]
        val_str = fmt_value(sig_key, val)
        e = STATUS_EMOJI[status]
        lines.append(f"  {label}  {val_str:>6}  {arrow}  {e}")

    if current.get("_error"):
        lines.append(f"  ⚠ {current['_error']}")

    return "\n".join(lines)


# ── Cache ──────────────────────────────────────────────────────────────────
def load_cache() -> dict | None:
    try:
        with open(CACHE_FILE) as f:
            return json.load(f)
    except Exception:
        return None


def save_cache(data: dict) -> None:
    try:
        tmp = CACHE_FILE + ".tmp"
        with open(tmp, "w") as f:
            json.dump(data, f)
        os.replace(tmp, CACHE_FILE)
    except Exception:
        pass


# ── Entry point ────────────────────────────────────────────────────────────
def main() -> int:
    args = sys.argv[1:]
    want_human = "--human" in args

    data = score()
    prev = load_cache()

    if want_human:
        print(human_output(data, prev))
    else:
        print(json.dumps(data, indent=2))

    save_cache(data)

    overall = data.get("overall", "healthy")
    return 0 if overall == "healthy" else 1


if __name__ == "__main__":
    sys.exit(main())

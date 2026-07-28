//! Pure ping-policy + digest composition for the HITL queue.
//!
//! Everything here is a pure function: no filesystem, no wall clock, no
//! `osascript`. The caller injects `now` (and the small per-day counters the
//! store stamps) and gets back a plain list of ping actions / a digest value.
//! That is what makes the whole policy table-testable — the tests below drive
//! every rule from the scope's policy section with hand-built `Item`s.
//!
//! ## Timing conventions (documented so the tests pin them)
//!
//! - A `deadline` is a bare `NaiveDate`; we anchor it at **00:00:00 UTC of that
//!   day** (`deadline_instant`). "T-48h" / "T-24h" are measured from that
//!   anchor. This is a choice — the spec only says pings cross those
//!   thresholds — but it is consistent and every deadline test uses it.
//! - Quiet hours are the half-open hour range `[quiet_start, quiet_end)` and
//!   wrap past midnight when `quiet_start > quiet_end` (the default 22..8).
//! - A snoozed item is silent while `now.date() < snooze_until`; on/after that
//!   date it is treated as open again (for both pings and the digest).
//!
//! ## The one-ping-per-run rule
//!
//! Each item yields **at most one** ping action per evaluation. If several
//! thresholds have been crossed since the last ping we emit the single most
//! urgent one (T-24h over T-48h over the initial on-file ping), never a burst.

use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};

use super::store::{Config, Item, Mode, Priority, Status};

// ---------------------------------------------------------------------------
// Ping actions
// ---------------------------------------------------------------------------

/// Why a given ping is being sent — carried through to `log.jsonl`/telemetry so
/// the operator can see *why* they were pinged, not just that they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingReason {
    /// First ping, sent when the item is filed (or the first nudge that clears
    /// quiet hours / the cap after it was filed).
    OnFile,
    /// P1 recurring 24h re-ping while still open.
    Recurring,
    /// P2 with a deadline, crossing the T-48h threshold.
    Deadline48h,
    /// P2 with a deadline, crossing the T-24h threshold.
    Deadline24h,
}

impl PingReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            PingReason::OnFile => "on-file",
            PingReason::Recurring => "recurring-24h",
            PingReason::Deadline48h => "deadline-48h",
            PingReason::Deadline24h => "deadline-24h",
        }
    }
}

/// A decision to ping a specific item now. Pure data — the caller performs the
/// actual send (and stamps `last_pinged`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingAction {
    pub item_id: u64,
    pub priority: Priority,
    pub reason: PingReason,
}

// ---------------------------------------------------------------------------
// Small time helpers (pure)
// ---------------------------------------------------------------------------

/// Midnight-UTC anchor for a deadline date.
fn deadline_instant(d: NaiveDate) -> DateTime<Utc> {
    DateTime::<Utc>::from_naive_utc_and_offset(d.and_hms_opt(0, 0, 0).unwrap(), Utc)
}

/// Is `now`'s hour inside the half-open quiet window `[start, end)`, wrapping
/// past midnight when `start > end`? `start == end` ⇒ never quiet.
fn in_quiet_hours(now: DateTime<Utc>, cfg: &Config) -> bool {
    let h = now.hour();
    let (start, end) = (cfg.quiet_start, cfg.quiet_end);
    if start == end {
        false
    } else if start < end {
        h >= start && h < end
    } else {
        // wrap-around window, e.g. 22..8 covers 22,23,0..7
        h >= start || h < end
    }
}

/// Is the snooze still in effect at `now`? A lapsed snooze reads as open.
fn snooze_active(item: &Item, now: DateTime<Utc>) -> bool {
    item.status == Status::Snoozed
        && item
            .snooze_until
            .map(|until| now.date_naive() < until)
            .unwrap_or(false)
}

/// An item counts toward the "live queue" (pings + digest) when it is open, or
/// snoozed-but-lapsed. Closed and actively-snoozed items are excluded.
fn is_live(item: &Item, now: DateTime<Utc>) -> bool {
    match item.status {
        Status::Open => true,
        Status::Snoozed => !snooze_active(item, now),
        Status::Done | Status::Skipped => false,
    }
}

/// Blocked ⇔ any listed dependency exists and is not yet closed (open OR
/// snoozed). Blocked items are never pinged individually; the digest flags them.
fn is_blocked(item: &Item, all: &[Item]) -> bool {
    item.depends_on.iter().any(|dep_id| {
        all.iter()
            .find(|o| o.id == *dep_id)
            .map(|o| !o.status.is_closed())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Per-item ping decision
// ---------------------------------------------------------------------------

/// The single ping (if any) due for one item at `now`, ignoring quiet hours and
/// the daily cap (those are global and applied by [`pings_due`]). Returns the
/// most urgent applicable reason — never more than one.
fn ping_for_item(
    item: &Item,
    all: &[Item],
    cfg: &Config,
    now: DateTime<Utc>,
) -> Option<PingReason> {
    if !is_live(item, now) {
        return None;
    }
    if is_blocked(item, all) {
        return None;
    }
    match item.priority {
        // P1: on-file, then re-ping every 24h while open.
        Priority::P1 => match item.last_pinged {
            None => Some(PingReason::OnFile),
            Some(last) if now.signed_duration_since(last) >= Duration::hours(24) => {
                Some(PingReason::Recurring)
            }
            Some(_) => None,
        },
        // P2: on-file + (if a deadline is set) escalations at T-48h / T-24h.
        //     `batched` mode suppresses all of P2's individual pings.
        Priority::P2 => {
            if cfg.mode == Mode::Batched {
                return None;
            }
            match item.last_pinged {
                None => Some(PingReason::OnFile),
                Some(last) => item.deadline.and_then(|d| {
                    let anchor = deadline_instant(d);
                    let t48 = anchor - Duration::hours(48);
                    let t24 = anchor - Duration::hours(24);
                    if now >= t24 && last < t24 {
                        Some(PingReason::Deadline24h)
                    } else if now >= t48 && last < t48 {
                        Some(PingReason::Deadline48h)
                    } else {
                        None
                    }
                }),
            }
        }
        // P3: never pinged individually — digest only.
        Priority::P3 => None,
    }
}

// ---------------------------------------------------------------------------
// The decision function
// ---------------------------------------------------------------------------

/// Compute the individual pings to send right now.
///
/// Inputs are exactly what the scope specifies: the full item set, config, the
/// injected clock, how many individual pings have already gone out today, and
/// whether the digest was already sent today. No I/O, no wall clock.
///
/// Ordering & cap: candidates are sorted highest-priority-first then
/// oldest-first, and truncated to the remaining daily allowance
/// (`max_pings_per_day - pings_sent_today`). During quiet hours nothing fires.
pub fn pings_due(
    items: &[Item],
    cfg: &Config,
    now: DateTime<Utc>,
    pings_sent_today: u32,
    digest_sent_today: bool,
) -> Vec<PingAction> {
    // The digest-sent flag is part of the specified signature but does not
    // gate individual pings; accepted for contract stability.
    let _ = digest_sent_today;

    // Quiet hours: emit no individual pings; they defer to the next run that
    // clears quiet hours (re-derived from `last_pinged`, so nothing is lost).
    if in_quiet_hours(now, cfg) {
        return Vec::new();
    }

    let mut candidates: Vec<PingAction> = items
        .iter()
        .filter_map(|it| {
            ping_for_item(it, items, cfg, now).map(|reason| PingAction {
                item_id: it.id,
                priority: it.priority,
                reason,
            })
        })
        .collect();

    // Highest priority first (P1 < P2 < P3 by Ord), then older first, then id.
    candidates.sort_by(|a, b| {
        let ia = items.iter().find(|i| i.id == a.item_id).unwrap();
        let ib = items.iter().find(|i| i.id == b.item_id).unwrap();
        a.priority
            .cmp(&b.priority)
            .then(ia.created.cmp(&ib.created))
            .then(a.item_id.cmp(&b.item_id))
    });

    let remaining = cfg.max_pings_per_day.saturating_sub(pings_sent_today) as usize;
    candidates.truncate(remaining);
    candidates
}

// ---------------------------------------------------------------------------
// Digest composition (pure)
// ---------------------------------------------------------------------------

/// One line of the digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestItem {
    pub id: u64,
    pub title: String,
    pub priority: Priority,
    pub deadline: Option<NaiveDate>,
    pub est_minutes: Option<u32>,
    pub blocked: bool,
}

/// Open items for one project, priority-sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestGroup {
    pub project: String,
    pub items: Vec<DigestItem>,
}

/// The composed digest: header counts + per-project groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    pub total_open: usize,
    pub total_est_minutes: u32,
    pub groups: Vec<DigestGroup>,
}

/// Compose the daily digest from the live queue. Returns `None` when the queue
/// is empty (⇒ the caller sends nothing).
///
/// Included: open + lapsed-snooze items (actively-snoozed and closed excluded).
/// Grouped by project, priority-sorted within a group, blocked items flagged.
pub fn compose_digest(items: &[Item], now: DateTime<Utc>) -> Option<Digest> {
    let mut live: Vec<&Item> = items.iter().filter(|it| is_live(it, now)).collect();
    if live.is_empty() {
        return None;
    }
    // Deterministic order: project, then priority, then oldest, then id.
    live.sort_by(|a, b| {
        a.project
            .cmp(&b.project)
            .then(a.priority.cmp(&b.priority))
            .then(a.created.cmp(&b.created))
            .then(a.id.cmp(&b.id))
    });

    let total_open = live.len();
    let total_est_minutes: u32 = live.iter().filter_map(|it| it.est_minutes).sum();

    let mut groups: Vec<DigestGroup> = Vec::new();
    for it in live {
        let di = DigestItem {
            id: it.id,
            title: it.title.clone(),
            priority: it.priority,
            deadline: it.deadline,
            est_minutes: it.est_minutes,
            blocked: is_blocked(it, items),
        };
        match groups.last_mut() {
            Some(g) if g.project == it.project => g.items.push(di),
            _ => groups.push(DigestGroup {
                project: it.project.clone(),
                items: vec![di],
            }),
        }
    }

    Some(Digest {
        total_open,
        total_est_minutes,
        groups,
    })
}

impl Digest {
    /// Render the digest as the single iMessage body. Header line first, then
    /// each project group.
    pub fn render(&self) -> String {
        let mut out = format!(
            "HITL: {} open, ~{} min",
            self.total_open, self.total_est_minutes
        );
        for g in &self.groups {
            out.push_str(&format!("\n\n{}:", g.project));
            for it in &g.items {
                let mut extras: Vec<String> = Vec::new();
                if let Some(m) = it.est_minutes {
                    extras.push(format!("~{m} min"));
                }
                if let Some(d) = it.deadline {
                    extras.push(format!("due {d}"));
                }
                if it.blocked {
                    extras.push("blocked".to_string());
                }
                let suffix = if extras.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", extras.join(", "))
                };
                out.push_str(&format!(
                    "\n  [{}] #{} {}{}",
                    it.priority, it.id, it.title, suffix
                ));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests — table-driven, one table per policy rule.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Minimal open item with sane defaults; tweak fields per case.
    fn item(id: u64, priority: Priority) -> Item {
        Item {
            id,
            title: format!("item {id}"),
            project: "studio".into(),
            body: String::new(),
            priority,
            deadline: None,
            est_minutes: None,
            depends_on: vec![],
            status: Status::Open,
            created: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            snooze_until: None,
            last_pinged: None,
            closed_at: None,
            note: None,
        }
    }

    /// Config with quiet hours pushed far out of the way so timing tests are
    /// not accidentally silenced; caller overrides fields as needed.
    fn cfg() -> Config {
        Config {
            mode: Mode::Immediate,
            imessage_handle: None,
            digest_hour: 9,
            quiet_start: 0,
            quiet_end: 0, // start==end ⇒ never quiet
            max_pings_per_day: 100,
        }
    }

    fn reasons(actions: &[PingAction]) -> Vec<(u64, PingReason)> {
        actions.iter().map(|a| (a.item_id, a.reason)).collect()
    }

    // --- On-file ping (P1 & P2) --------------------------------------------

    #[test]
    fn on_file_ping_fires_for_p1_and_p2_when_never_pinged() {
        // (priority, expected on-file ping?)
        let cases = [
            (Priority::P1, true),
            (Priority::P2, true),
            (Priority::P3, false),
        ];
        for (pri, expect) in cases {
            let it = item(1, pri);
            let got = pings_due(&[it], &cfg(), ts("2026-07-10T12:00:00Z"), 0, false);
            assert_eq!(
                !got.is_empty(),
                expect,
                "on-file ping for {pri:?} should be {expect}"
            );
            if expect {
                assert_eq!(got[0].reason, PingReason::OnFile);
            }
        }
    }

    // --- P1 24h re-ping -----------------------------------------------------

    #[test]
    fn p1_repings_every_24h() {
        // (hours since last_pinged, expect a Recurring ping?)
        let cases = [
            (0i64, false),
            (12, false),
            (23, false),
            (24, true),
            (48, true),
        ];
        let base = ts("2026-07-10T12:00:00Z");
        for (hrs, expect) in cases {
            let mut it = item(1, Priority::P1);
            it.last_pinged = Some(base);
            let now = base + Duration::hours(hrs);
            let got = pings_due(&[it], &cfg(), now, 0, false);
            assert_eq!(!got.is_empty(), expect, "P1 at +{hrs}h expect={expect}");
            if expect {
                assert_eq!(got[0].reason, PingReason::Recurring);
            }
        }
    }

    #[test]
    fn p2_without_deadline_never_repings() {
        let mut it = item(1, Priority::P2);
        it.last_pinged = Some(ts("2026-07-10T12:00:00Z"));
        // Days later, still no re-ping (digest covers it).
        let got = pings_due(&[it], &cfg(), ts("2026-07-20T12:00:00Z"), 0, false);
        assert!(got.is_empty(), "P2 without deadline must not re-ping");
    }

    // --- P2 deadline escalation at T-48h / T-24h ---------------------------

    #[test]
    fn p2_deadline_escalates_at_t48_and_t24() {
        // deadline anchored at 2026-07-10T00:00Z.
        //   T-48h = 2026-07-08T00:00Z, T-24h = 2026-07-09T00:00Z
        // last_pinged is the on-file time on 2026-07-05 (before both thresholds).
        // (now, expected reason)
        let last = ts("2026-07-05T09:00:00Z");
        let cases = [
            ("2026-07-07T12:00:00Z", None), // before T-48h
            ("2026-07-08T01:00:00Z", Some(PingReason::Deadline48h)), // crossed T-48h
            ("2026-07-09T01:00:00Z", Some(PingReason::Deadline24h)), // crossed T-24h
        ];
        for (now_s, expect) in cases {
            let mut it = item(1, Priority::P2);
            it.deadline = Some(day(2026, 7, 10));
            it.last_pinged = Some(last);
            let got = pings_due(&[it], &cfg(), ts(now_s), 0, false);
            match expect {
                None => assert!(got.is_empty(), "at {now_s} expected no ping"),
                Some(r) => {
                    assert_eq!(got.len(), 1, "at {now_s} expected one ping");
                    assert_eq!(got[0].reason, r, "at {now_s}");
                }
            }
        }
    }

    #[test]
    fn p2_deadline_ping_does_not_repeat_after_threshold_pinged() {
        // last_pinged already past T-48h but before T-24h ⇒ T-48h must not fire
        // again; only T-24h remains.
        let mut it = item(1, Priority::P2);
        it.deadline = Some(day(2026, 7, 10));
        it.last_pinged = Some(ts("2026-07-08T06:00:00Z")); // after T-48h
                                                           // now between T-48h and T-24h → nothing new
        let got = pings_due(&[it.clone()], &cfg(), ts("2026-07-08T12:00:00Z"), 0, false);
        assert!(got.is_empty(), "T-48h already covered, T-24h not reached");
        // now past T-24h → T-24h fires
        let got = pings_due(&[it], &cfg(), ts("2026-07-09T06:00:00Z"), 0, false);
        assert_eq!(reasons(&got), vec![(1, PingReason::Deadline24h)]);
    }

    // --- P3 digest-only -----------------------------------------------------

    #[test]
    fn p3_is_never_individually_pinged() {
        let mut it = item(1, Priority::P3);
        it.deadline = Some(day(2026, 7, 10));
        it.last_pinged = None;
        // On file, at deadline, long after — never an individual ping.
        for now_s in [
            "2026-07-01T12:00:00Z",
            "2026-07-09T12:00:00Z",
            "2026-07-20T12:00:00Z",
        ] {
            let got = pings_due(&[it.clone()], &cfg(), ts(now_s), 0, false);
            assert!(got.is_empty(), "P3 must never ping ({now_s})");
        }
        // …but it does appear in the digest.
        let d = compose_digest(&[it], ts("2026-07-05T09:00:00Z")).unwrap();
        assert_eq!(d.total_open, 1);
    }

    // --- Blocked-by-dependency suppression ---------------------------------

    #[test]
    fn blocked_item_is_suppressed_and_flagged() {
        // dep states: (dep status, blocks?)
        let cases = [
            (Status::Open, true),
            (Status::Snoozed, true),
            (Status::Done, false),
            (Status::Skipped, false),
        ];
        for (dep_status, blocks) in cases {
            let mut dep = item(1, Priority::P3);
            dep.status = dep_status;
            if dep_status == Status::Snoozed {
                dep.snooze_until = Some(day(2027, 1, 1));
            }
            let mut blocked = item(2, Priority::P1); // P1 → would ping loudly
            blocked.depends_on = vec![1];
            let items = vec![dep, blocked];

            let got = pings_due(&items, &cfg(), ts("2026-07-10T12:00:00Z"), 0, false);
            let pinged_2 = got.iter().any(|a| a.item_id == 2);
            assert_eq!(
                pinged_2, !blocks,
                "item 2 pinged should be {} when dep is {dep_status:?}",
                !blocks
            );

            // Digest always shows it, flagged iff blocked.
            let d = compose_digest(&items, ts("2026-07-10T12:00:00Z")).unwrap();
            let entry = d
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.id == 2)
                .unwrap();
            assert_eq!(
                entry.blocked, blocks,
                "digest blocked flag for dep {dep_status:?}"
            );
        }
    }

    // --- Snooze -------------------------------------------------------------

    #[test]
    fn snoozed_item_is_silent_until_snooze_passes() {
        // (now date vs snooze_until 2026-07-15, expect ping?, expect in digest?)
        let cases = [
            ("2026-07-10T12:00:00Z", false, false), // before → silent + hidden
            ("2026-07-14T23:00:00Z", false, false), // day before → still silent
            ("2026-07-15T00:00:00Z", true, true),   // on the date → awake
            ("2026-07-20T12:00:00Z", true, true),   // after → awake
        ];
        for (now_s, expect_ping, expect_digest) in cases {
            let mut it = item(1, Priority::P1);
            it.status = Status::Snoozed;
            it.snooze_until = Some(day(2026, 7, 15));
            let items = vec![it];
            let got = pings_due(&items, &cfg(), ts(now_s), 0, false);
            assert_eq!(!got.is_empty(), expect_ping, "ping at {now_s}");
            let digest = compose_digest(&items, ts(now_s));
            assert_eq!(digest.is_some(), expect_digest, "digest at {now_s}");
        }
    }

    // --- Quiet hours --------------------------------------------------------

    #[test]
    fn quiet_hours_suppress_individual_pings() {
        // default quiet window 22..8 (wraps midnight). (hour UTC, expect ping?)
        let mut c = cfg();
        c.quiet_start = 22;
        c.quiet_end = 8;
        let cases = [
            (7u32, false), // inside (early morning)
            (8, true),     // window is half-open at end → 8 is awake
            (12, true),    // midday
            (21, true),    // just before window
            (22, false),   // window start
            (23, false),   // inside
            (0, false),    // inside (after midnight)
        ];
        for (hour, expect) in cases {
            let it = item(1, Priority::P1); // on-file ping otherwise guaranteed
            let now = Utc.with_ymd_and_hms(2026, 7, 10, hour, 0, 0).unwrap();
            let got = pings_due(&[it], &c, now, 0, false);
            assert_eq!(!got.is_empty(), expect, "hour {hour} expect ping={expect}");
        }
    }

    #[test]
    fn quiet_ping_fires_at_next_run_after_quiet_end() {
        // A P1 filed at night stays silent, then fires the same (still None
        // last_pinged) on the first daytime run — nothing is lost.
        let c = {
            let mut c = cfg();
            c.quiet_start = 22;
            c.quiet_end = 8;
            c
        };
        let it = item(1, Priority::P1);
        let night = pings_due(&[it.clone()], &c, ts("2026-07-10T23:30:00Z"), 0, false);
        assert!(night.is_empty(), "no ping at night");
        let morning = pings_due(&[it], &c, ts("2026-07-11T08:30:00Z"), 0, false);
        assert_eq!(reasons(&morning), vec![(1, PingReason::OnFile)]);
    }

    // --- Daily cap with priority ordering ----------------------------------

    #[test]
    fn daily_cap_limits_pings_highest_priority_first() {
        // Three due items: P2(old), P1(new), P2(new). Cap of 2 keeps the P1 and
        // the older P2, dropping the newer P2.
        let mut p2_old = item(1, Priority::P2);
        p2_old.created = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let mut p1_new = item(2, Priority::P1);
        p1_new.created = Utc.with_ymd_and_hms(2026, 7, 5, 0, 0, 0).unwrap();
        let mut p2_new = item(3, Priority::P2);
        p2_new.created = Utc.with_ymd_and_hms(2026, 7, 6, 0, 0, 0).unwrap();
        let items = vec![p2_old, p1_new, p2_new];

        let mut c = cfg();
        c.max_pings_per_day = 2;
        let got = pings_due(&items, &c, ts("2026-07-10T12:00:00Z"), 0, false);
        let ids: Vec<u64> = got.iter().map(|a| a.item_id).collect();
        assert_eq!(ids, vec![2, 1], "P1 first, then older P2; newer P2 dropped");
    }

    #[test]
    fn daily_cap_respects_already_sent_count() {
        let a = item(1, Priority::P1);
        let b = item(2, Priority::P1);
        let mut c = cfg();
        c.max_pings_per_day = 3;
        // Two already sent today, cap 3 → only one more allowed.
        let got = pings_due(
            &[a.clone(), b.clone()],
            &c,
            ts("2026-07-10T12:00:00Z"),
            2,
            false,
        );
        assert_eq!(got.len(), 1, "only one ping left in today's allowance");
        // Cap already reached → nothing.
        let got = pings_due(&[a, b], &c, ts("2026-07-10T12:00:00Z"), 3, false);
        assert!(got.is_empty(), "cap exhausted");
    }

    // --- Batched mode -------------------------------------------------------

    #[test]
    fn batched_mode_suppresses_p2_but_not_p1() {
        let mut c = cfg();
        c.mode = Mode::Batched;

        // P2 on-file: suppressed.
        let p2 = item(1, Priority::P2);
        assert!(pings_due(&[p2], &c, ts("2026-07-10T12:00:00Z"), 0, false).is_empty());

        // P2 with deadline escalation: also suppressed.
        let mut p2d = item(2, Priority::P2);
        p2d.deadline = Some(day(2026, 7, 10));
        p2d.last_pinged = Some(ts("2026-07-05T09:00:00Z"));
        assert!(
            pings_due(&[p2d], &c, ts("2026-07-09T01:00:00Z"), 0, false).is_empty(),
            "batched suppresses P2 deadline escalation"
        );

        // P1 still pings normally.
        let p1 = item(3, Priority::P1);
        let got = pings_due(&[p1], &c, ts("2026-07-10T12:00:00Z"), 0, false);
        assert_eq!(reasons(&got), vec![(3, PingReason::OnFile)]);
    }

    // --- Digest composition -------------------------------------------------

    #[test]
    fn digest_empty_queue_is_none() {
        assert!(compose_digest(&[], ts("2026-07-10T09:00:00Z")).is_none());
        // Only closed / actively-snoozed items ⇒ still nothing to send.
        let mut done = item(1, Priority::P1);
        done.status = Status::Done;
        assert!(compose_digest(&[done], ts("2026-07-10T09:00:00Z")).is_none());
    }

    #[test]
    fn digest_groups_by_project_and_totals_est_minutes() {
        let mut a = item(1, Priority::P2);
        a.project = "studio".into();
        a.est_minutes = Some(20);
        let mut b = item(2, Priority::P1);
        b.project = "studio".into();
        b.est_minutes = Some(15);
        let mut cc = item(3, Priority::P3);
        cc.project = "ops".into(); // no est_minutes

        let d = compose_digest(&[a, b, cc], ts("2026-07-10T09:00:00Z")).unwrap();
        assert_eq!(d.total_open, 3);
        assert_eq!(d.total_est_minutes, 35);
        // Groups ordered by project name; within studio P1 sorts before P2.
        assert_eq!(d.groups.len(), 2);
        assert_eq!(d.groups[0].project, "ops");
        assert_eq!(d.groups[1].project, "studio");
        let studio_ids: Vec<u64> = d.groups[1].items.iter().map(|i| i.id).collect();
        assert_eq!(studio_ids, vec![2, 1], "P1 before P2 within a group");

        let rendered = d.render();
        assert!(rendered.starts_with("HITL: 3 open, ~35 min"), "{rendered}");
        assert!(rendered.contains("studio:"), "{rendered}");
    }
}

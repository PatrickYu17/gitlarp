//! Commit plans: validation, caps, and the rolling 12-month window.
//!
//! One `build_plan` serves both surfaces via caps:
//! - CLI: 100/day (1000 with --force), no total cap, `--force` keeps
//!   out-of-window dates instead of clamping them.
//! - API: 500/day, 500/request total, always clamps.

use serde_json::Value;
use time::Date;

use crate::date;
use crate::Error;

pub const MAX_DAYS: usize = 365;

#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub per_day: u32,
    pub hard_per_day: u32,
    pub total: u32,
}

pub const CLI_CAPS: Caps = Caps { per_day: 100, hard_per_day: 1000, total: 0 };
pub const API_CAPS: Caps = Caps { per_day: 500, hard_per_day: 500, total: 500 };

#[derive(Debug, Clone)]
pub struct Plan {
    pub days: Vec<(Date, u32)>,
    pub clamped: usize,
    pub outside: usize,
}

pub fn build_plan(
    days: &[(Date, u32)],
    force: bool,
    now: Date,
    caps: Caps,
) -> Result<Plan, Error> {
    if days.is_empty() {
        return Err(Error::bad("empty plan is a no-op"));
    }
    if days.len() > MAX_DAYS {
        return Err(Error::bad(format!(
            "{} days is over the hard cap of {MAX_DAYS} days per run",
            days.len()
        )));
    }
    let mut sorted = days.to_vec();
    sorted.sort_by_key(|(d, _)| *d);
    let cap = if force { caps.hard_per_day } else { caps.per_day };
    let min_date = date::add_days(now, -364);
    let mut out: Vec<(Date, u32)> = Vec::new();
    let mut clamped = 0usize;
    let mut outside = 0usize;
    let mut total = 0u64;
    for (d, n) in sorted {
        if n == 0 {
            continue;
        }
        if n > cap {
            return Err(Error::bad(format!("count {n} exceeds cap {cap}/day")));
        }
        let mut dd = d;
        if dd < min_date || dd > now {
            if force {
                outside += 1;
            } else {
                dd = if dd < min_date { min_date } else { now };
                clamped += 1;
            }
        }
        total += n as u64;
        if caps.total > 0 && total > caps.total as u64 {
            return Err(Error::bad(format!(
                "total commits exceed cap {}/request",
                caps.total
            )));
        }
        if let Some((ld, lc)) = out.last_mut() {
            if *ld == dd {
                if *lc + n > cap {
                    return Err(Error::bad(format!("clamped dates exceed cap {cap}/day")));
                }
                *lc += n;
                continue;
            }
        }
        out.push((dd, n));
    }
    if out.is_empty() {
        return Err(Error::bad("empty plan is a no-op (all counts are zero)"));
    }
    Ok(Plan { days: out, clamped, outside })
}

/// The /api/commits body shape: `days: [{ date, count }]`.
/// All JSON-level validation lives here so every shell shares it.
/// Uses the API caps (500/day, 500/request).
pub fn parse_api_plan(raw: &Value, today: Date) -> Result<Plan, Error> {
    parse_api_plan_capped(raw, today, API_CAPS)
}

/// `parse_api_plan` with caller-supplied caps: shells with tighter
/// transport budgets (e.g. the Cloudflare worker, whose subrequest
/// ceiling is far below 500) validate against their own limits, so
/// oversized requests fail fast with a 400 instead of dying mid-run
/// against the platform.
pub fn parse_api_plan_capped(raw: &Value, today: Date, caps: Caps) -> Result<Plan, Error> {
    let need = || Error::bad("need { pat, days: [{ date, count }] }");
    let Some(arr) = raw.as_array() else { return Err(need()) };
    if arr.is_empty() {
        return Err(need());
    }
    if arr.len() > MAX_DAYS {
        return Err(Error::bad("too many days (max 365/request)"));
    }
    let mut entries = Vec::new();
    for item in arr {
        let Some(o) = item.as_object() else {
            return Err(Error::bad("each day needs a YYYY-MM-DD date"));
        };
        let Some(ds) = o.get("date").and_then(|v| v.as_str()) else {
            return Err(Error::bad("each day needs a YYYY-MM-DD date"));
        };
        let Some(count) = o.get("count").and_then(|v| v.as_u64()) else {
            return Err(Error::bad("count must be a non-negative integer"));
        };
        if count > caps.per_day as u64 {
            return Err(Error::bad(format!(
                "count {count} exceeds cap {}/day on this endpoint",
                caps.per_day
            )));
        }
        entries.push((date::parse(ds)?, count as u32));
    }
    build_plan(&entries, false, today, caps)
}

/// Splits a plan into batches of at most `size` commits, preserving
/// date order (an entry may straddle batches).
pub fn chunk_plan(days: &[(Date, u32)], size: u32) -> Vec<Vec<(Date, u32)>> {
    let mut out = Vec::new();
    let mut chunk: Vec<(Date, u32)> = Vec::new();
    let mut used = 0u32;
    for &(d, mut remaining) in days {
        while remaining > 0 {
            let take = remaining.min(size - used);
            chunk.push((d, take));
            remaining -= take;
            used += take;
            if used == size {
                out.push(std::mem::take(&mut chunk));
                used = 0;
            }
        }
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn d(y: i32, m: time::Month, dd: u8) -> Date {
        Date::from_calendar_date(y, m, dd).unwrap()
    }

    #[test]
    fn plan_clamps_old_dates() {
        let now = date!(2026-08-24);
        let p = build_plan(&[(date!(2025-01-01), 3)], false, now, CLI_CAPS).unwrap();
        assert_eq!(p.clamped, 1);
        assert_eq!(p.days[0].0, date::add_days(now, -364));
    }

    #[test]
    fn force_preserves_out_of_window_dates() {
        let now = date!(2026-08-24);
        let p = build_plan(&[(date!(2025-01-01), 3)], true, now, CLI_CAPS).unwrap();
        assert_eq!(p.days[0].0, date!(2025-01-01));
        assert_eq!(p.clamped, 0);
        assert_eq!(p.outside, 1);
    }

    #[test]
    fn plan_merges_clamped_duplicates() {
        let now = date!(2026-08-24);
        let p = build_plan(
            &[(date!(2025-01-01), 1), (date!(2025-01-02), 2)],
            false,
            now,
            CLI_CAPS,
        )
        .unwrap();
        assert_eq!(p.clamped, 2);
        assert_eq!(p.days.len(), 1);
        assert_eq!(p.days[0].1, 3);
    }

    #[test]
    fn plan_caps_clamped_duplicates() {
        let now = date!(2026-08-24);
        assert!(build_plan(
            &[(date!(2025-01-01), 60), (date!(2025-01-02), 60)],
            false,
            now,
            CLI_CAPS
        )
        .is_err());
    }

    #[test]
    fn plan_rejects_over_cap() {
        let now = date!(2026-08-24);
        assert!(build_plan(&[(now, 101)], false, now, CLI_CAPS).is_err());
        assert!(build_plan(&[(now, 1001)], true, now, CLI_CAPS).is_err());
        assert!(build_plan(&[(now, 1000)], true, now, CLI_CAPS).is_ok());
    }

    #[test]
    fn plan_rejects_zero() {
        let now = date!(2026-08-24);
        assert!(build_plan(&[(now, 0)], false, now, CLI_CAPS).is_err());
    }

    #[test]
    fn plan_rejects_huge_range() {
        let now = date!(2026-08-24);
        let mut days = Vec::new();
        for i in 0..366 {
            days.push((date::add_days(now, -i), 1));
        }
        assert!(build_plan(&days, false, now, CLI_CAPS).is_err());
    }

    #[test]
    fn api_plan_validates_and_clamps() {
        let today = date!(2026-08-24);
        let raw = serde_json::json!([
            { "date": "2026-08-20", "count": 2 },
            { "date": "2025-01-01", "count": 1 },
        ]);
        let p = parse_api_plan(&raw, today).unwrap();
        assert_eq!(p.clamped, 1);
        // plan is sorted by date: the clamped old date comes first
        assert_eq!(p.days[0].0, date::add_days(today, -364));
        assert_eq!(p.days[1], (date!(2026-08-20), 2));
    }

    #[test]
    fn api_plan_rejects_bad_input() {
        let today = date!(2026-08-24);
        for raw in [
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!([{ "date": "2026/08/20", "count": 1 }]),
            serde_json::json!([{ "date": "2026-08-20" }]),
            serde_json::json!([{ "date": "2026-08-20", "count": -1 }]),
            serde_json::json!([{ "date": "2026-08-20", "count": 501 }]),
            serde_json::json!([{ "date": "2026-08-20", "count": 0 }]),
        ] {
            assert!(parse_api_plan(&raw, today).is_err(), "{raw}");
        }
    }

    #[test]
    fn api_plan_rejects_total_over_cap() {
        let today = date!(2026-08-24);
        let raw: Value = (0..5)
            .map(|i| {
                serde_json::json!({ "date": format!("2026-08-1{i}"), "count": 101 })
            })
            .collect::<Vec<_>>()
            .into();
        assert!(parse_api_plan(&raw, today).is_err());
    }

    #[test]
    fn api_plan_capped_uses_provided_caps() {
        let today = date!(2026-08-24);
        let caps = Caps { per_day: 40, hard_per_day: 40, total: 40 };
        // per-day: 41 rejected, 40 accepted
        let over = serde_json::json!([{ "date": "2026-08-24", "count": 41 }]);
        assert!(parse_api_plan_capped(&over, today, caps).is_err());
        let at = serde_json::json!([{ "date": "2026-08-24", "count": 40 }]);
        assert!(parse_api_plan_capped(&at, today, caps).is_ok());
        // total: 25 + 25 across two days rejected under a total cap of 40
        let both = serde_json::json!([
            { "date": "2026-08-23", "count": 25 },
            { "date": "2026-08-24", "count": 25 },
        ]);
        assert!(parse_api_plan_capped(&both, today, caps).is_err());
        // and the API caps still apply through the plain entry point
        assert!(parse_api_plan_capped(&at, today, API_CAPS).is_ok());
    }

    #[test]
    fn chunks_respect_size_and_order() {
        let days = [(date!(2026-08-20), 3), (date!(2026-08-21), 5)];
        let chunks = chunk_plan(&days, 4);
        assert_eq!(
            chunks,
            vec![
                vec![(date!(2026-08-20), 3), (date!(2026-08-21), 1)],
                vec![(date!(2026-08-21), 4)],
            ]
        );
        let empty = chunk_plan(&[], 50);
        assert!(empty.is_empty());
    }

    #[test]
    fn date_parse_is_strict() {
        assert!(date::parse("2026-08-24").is_ok());
        for bad in ["2026-8-24", "2026-08-24T00", "26-08-24", "2026-13-01", ""] {
            assert!(date::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(date::fmt(d(2026, time::Month::August, 24)), "2026-08-24");
    }
}

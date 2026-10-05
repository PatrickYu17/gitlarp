//! Schedules: `min..max` commits on each matching day, with an
//! optional date range, weekend filter, and catch-up window.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{Date, Weekday};

use crate::date;
use crate::Error;

pub const DEFAULT_CATCHUP: u32 = 14;
pub const MAX_PER_DAY: u32 = 100;
pub const MAX_CATCHUP: u32 = 365;

/// serde via "YYYY-MM-DD" strings (the wire/stored format).
pub mod date_str {
    use super::*;

    pub fn serialize<S: serde::Serializer>(d: &Date, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&date::fmt(*d))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Date, D::Error> {
        let s = String::deserialize(d)?;
        date::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Option-aware variant for the spec's from/to fields.
pub mod date_str_opt {
    use super::*;

    pub fn serialize<S: serde::Serializer>(d: &Option<Date>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_str(&date::fmt(*d)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Date>, D::Error> {
        let s = Option::<String>::deserialize(d)?;
        match s {
            Some(s) => date::parse(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleSpec {
    #[serde(default, skip_serializing_if = "Option::is_none", with = "date_str_opt")]
    pub from: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "date_str_opt")]
    pub to: Option<Date>,
    pub min: u32,
    pub max: u32,
    #[serde(default = "default_true")]
    pub weekends: bool,
    #[serde(default = "default_catchup")]
    pub catchup: u32,
}

fn default_true() -> bool {
    true
}

fn default_catchup() -> u32 {
    DEFAULT_CATCHUP
}

fn num(v: Option<&Value>, name: &str, lo: u32, hi: u32) -> Result<u32, Error> {
    match v {
        Some(Value::Number(n)) if n.is_u64() => {
            let x = n.as_u64().unwrap();
            if x >= lo as u64 && x <= hi as u64 {
                Ok(x as u32)
            } else {
                Err(Error::bad(format!("{name} must be an integer {lo}..{hi}")))
            }
        }
        _ => Err(Error::bad(format!("{name} must be an integer {lo}..{hi}"))),
    }
}

fn date_str_field(v: Option<&Value>, name: &str) -> Result<Date, Error> {
    match v.and_then(|v| v.as_str()) {
        Some(s) => date::parse(s).map_err(|_| Error::bad(format!("{name} must be a YYYY-MM-DD date"))),
        None => Err(Error::bad(format!("{name} must be a YYYY-MM-DD date"))),
    }
}

/// Validates the JSON body of a schedule-create request (mirrors the
/// TS-side rules exactly).
pub fn parse_spec(raw: &Value) -> Result<ScheduleSpec, Error> {
    let Some(o) = raw.as_object() else {
        return Err(Error::bad("need a schedule spec object"));
    };
    let min = num(o.get("min"), "min", 1, MAX_PER_DAY)?;
    let max = num(o.get("max"), "max", 1, MAX_PER_DAY)?;
    if min > max {
        return Err(Error::bad("min must be <= max"));
    }
    let weekends = match o.get("weekends") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        _ => return Err(Error::bad("weekends must be a boolean")),
    };
    let catchup = match o.get("catchup") {
        None | Some(Value::Null) => DEFAULT_CATCHUP,
        Some(_) => num(o.get("catchup"), "catchup", 1, MAX_CATCHUP)?,
    };
    let mut spec = ScheduleSpec { from: None, to: None, min, max, weekends, catchup };
    if let Some(v) = o.get("from") {
        if !v.is_null() {
            spec.from = Some(date_str_field(Some(v), "from")?);
        }
    }
    if let Some(v) = o.get("to") {
        if !v.is_null() {
            spec.to = Some(date_str_field(Some(v), "to")?);
        }
    }
    if let (Some(f), Some(t)) = (spec.from, spec.to) {
        if f > t {
            return Err(Error::bad("from must be <= to"));
        }
    }
    Ok(spec)
}

pub fn matches_day(s: &ScheduleSpec, d: Date) -> bool {
    s.from.is_none_or(|f| d >= f)
        && s.to.is_none_or(|t| d <= t)
        && (s.weekends || !matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday))
}

/// Days that are due between `last` (exclusive) and `now` (inclusive),
/// bounded by the catch-up window and filtered by the spec. A missing
/// `last` means "no history": only strictly-future days qualify, so
/// enabling a schedule never backfills. `rng(lo, hi)` picks the count.
pub fn due_days(
    s: &ScheduleSpec,
    last: Option<Date>,
    now: Date,
    rng: &mut dyn FnMut(u32, u32) -> u32,
) -> Vec<(Date, u32)> {
    let mut start = match last {
        Some(d) => date::add_days(d, 1),
        None => date::add_days(now, 1),
    };
    let floor = date::add_days(now, -(s.catchup as i64 - 1));
    if start < floor {
        start = floor;
    }
    let mut days = Vec::new();
    let mut d = start;
    while d <= now {
        if matches_day(s, d) {
            days.push((d, rng(s.min, s.max)));
        }
        d = date::add_days(d, 1);
    }
    days
}

/// How many days got cut by the catch-up floor (for CLI warnings).
pub fn catchup_skipped(s: &ScheduleSpec, last: Option<Date>, now: Date) -> i64 {
    let start = match last {
        Some(d) => date::add_days(d, 1),
        None => return 0,
    };
    let floor = date::add_days(now, -(s.catchup as i64 - 1));
    if start < floor {
        (floor - start).whole_days()
    } else {
        0
    }
}

pub fn next_due(s: &ScheduleSpec, last: Option<Date>, now: Date) -> Option<Date> {
    let mut d = match last {
        Some(d) => date::add_days(d, 1),
        None => now,
    };
    let horizon = date::add_days(d, 365);
    while d <= horizon {
        if matches_day(s, d) {
            return Some(d);
        }
        d = date::add_days(d, 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn sched(min: u32, max: u32) -> ScheduleSpec {
        ScheduleSpec { from: None, to: None, min, max, weekends: true, catchup: DEFAULT_CATCHUP }
    }

    fn saturday_near(d: Date) -> Date {
        let mut d = d;
        while d.weekday() != Weekday::Saturday {
            d = date::add_days(d, 1);
        }
        d
    }

    fn fixed(_v: u32) -> impl FnMut(u32, u32) -> u32 {
        move |lo, _| lo
    }

    #[test]
    fn due_days_basic_window() {
        let mut rng = fixed(1);
        let days = due_days(&sched(1, 1), Some(date!(2026-08-20)), date!(2026-08-23), &mut rng);
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].0, date!(2026-08-21));
        assert_eq!(days[2].0, date!(2026-08-23));
        assert!(days.iter().all(|(_, n)| *n == 1));
    }

    #[test]
    fn due_days_respects_weekends() {
        let sat = saturday_near(date!(2026-08-15));
        let s = ScheduleSpec { weekends: false, ..sched(1, 1) };
        let mut rng = fixed(1);
        let days = due_days(&s, Some(date::add_days(sat, -1)), date::add_days(sat, 2), &mut rng);
        let got: Vec<Date> = days.iter().map(|(d, _)| *d).collect();
        assert!(!got.contains(&sat));
        assert!(!got.contains(&date::add_days(sat, 1)));
        assert!(got.contains(&date::add_days(sat, 2)));
    }

    #[test]
    fn due_days_catchup_window() {
        let s = ScheduleSpec { catchup: 3, ..sched(1, 1) };
        let now = date!(2026-08-23);
        let mut rng = fixed(1);
        let days = due_days(&s, Some(date::add_days(now, -30)), now, &mut rng);
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].0, date::add_days(now, -2));
        assert_eq!(catchup_skipped(&s, Some(date::add_days(now, -30)), now), 27);
    }

    #[test]
    fn due_days_from_to_bounds() {
        let s = ScheduleSpec {
            from: Some(date!(2026-08-22)),
            to: Some(date!(2026-08-24)),
            ..sched(1, 1)
        };
        let mut rng = fixed(1);
        let days = due_days(&s, Some(date!(2026-08-20)), date!(2026-08-25), &mut rng);
        let got: Vec<Date> = days.iter().map(|(d, _)| *d).collect();
        assert_eq!(got, vec![date!(2026-08-22), date!(2026-08-23), date!(2026-08-24)]);
    }

    #[test]
    fn due_days_no_state_runs_nothing() {
        let mut rng = fixed(1);
        assert!(due_days(&sched(1, 1), None, date!(2026-08-23), &mut rng).is_empty());
    }

    #[test]
    fn next_due_skips_weekends() {
        let sat = saturday_near(date!(2026-08-15));
        let s = ScheduleSpec { weekends: false, ..sched(1, 1) };
        assert_eq!(
            next_due(&s, Some(date::add_days(sat, -1)), date::add_days(sat, -1)),
            Some(date::add_days(sat, 2))
        );
    }

    #[test]
    fn next_due_none_after_end() {
        let s = ScheduleSpec { to: Some(date!(2026-08-24)), ..sched(1, 1) };
        assert_eq!(next_due(&s, Some(date!(2026-08-24)), date!(2026-08-25)), None);
    }

    #[test]
    fn parse_spec_validates() {
        assert!(parse_spec(&serde_json::json!({ "min": 2, "max": 4 })).is_ok());
        let ok = parse_spec(&serde_json::json!({
            "min": 2, "max": 4, "weekends": false, "catchup": 7,
            "from": "2026-09-01", "to": "2026-09-30"
        }))
        .unwrap();
        assert_eq!(ok.min, 2);
        assert!(!ok.weekends);
        assert_eq!(ok.catchup, 7);
        assert_eq!(ok.from, Some(date!(2026-09-01)));
        for bad in [
            serde_json::json!({ "min": 5, "max": 1 }),
            serde_json::json!({ "min": 0, "max": 1 }),
            serde_json::json!({ "min": 1, "max": 101 }),
            serde_json::json!({ "min": 1, "max": 1, "catchup": 0 }),
            serde_json::json!({ "min": 1 }),
            serde_json::json!({ "min": 1, "max": 1, "weekends": "yes" }),
            serde_json::json!({ "min": 1, "max": 1, "from": "2026-09-30", "to": "2026-09-01" }),
            serde_json::json!("nope"),
        ] {
            assert!(parse_spec(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn spec_serde_roundtrip_via_strings() {
        let s = parse_spec(&serde_json::json!({
            "min": 1, "max": 2, "from": "2026-01-01"
        }))
        .unwrap();
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["from"], "2026-01-01");
        assert!(j.get("to").is_none());
        let back: ScheduleSpec = serde_json::from_value(j).unwrap();
        assert_eq!(back, s);
    }
}

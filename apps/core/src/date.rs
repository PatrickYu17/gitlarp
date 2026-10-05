//! Calendar dates in strict YYYY-MM-DD form, the wire format shared
//! by the config file, the API payloads, and the stored schedules.

pub use time::Date;

use crate::Error;

pub const FD: &[time::format_description::FormatItem] =
    time::macros::format_description!("[year]-[month]-[day]");

pub fn parse(s: &str) -> Result<Date, Error> {
    let d = Date::parse(s, FD).map_err(|_| Error::bad(format!("invalid date: {s}")))?;
    // roundtrip rejects unpadded and trailing input, matching the TS side
    if format!("{d}") != s {
        return Err(Error::bad(format!("invalid date: {s}")));
    }
    Ok(d)
}

pub fn fmt(d: Date) -> String {
    format!("{d}")
}

pub fn add_days(d: Date, n: i64) -> Date {
    d + time::Duration::days(n)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn today_utc() -> Date {
    time::OffsetDateTime::now_utc().date()
}

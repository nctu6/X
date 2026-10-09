//! Four six-hour slots in a configured time zone.
//!
//! A local day is `00–06`, `06–12`, `12–18`, and `18–24`. The same slot on
//! another date is a different slot, so each account can succeed once per
//! window.

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    /// `0` is 00:00–06:00, then `1`, `2`, and `3`.
    pub index: u8,
}

pub fn parse_timezone(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>()
        .map_err(|_| format!("timezone \"{name}\" is not an IANA time zone"))
}

pub fn slot_at(unix: i64, tz: Tz) -> Option<Slot> {
    let utc = DateTime::<Utc>::from_timestamp(unix, 0)?;
    let local = utc.with_timezone(&tz);
    Some(Slot {
        year: local.year(),
        month: local.month(),
        day: local.day(),
        index: (local.hour() / 6) as u8,
    })
}

pub fn same_slot(left: i64, right: i64, tz: Tz) -> bool {
    match (slot_at(left, tz), slot_at(right, tz)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(tz: Tz, year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        tz.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn taipei_slots_follow_six_hour_edges() {
        let tz = parse_timezone("Asia/Taipei").unwrap();
        let day = |hour, minute| at(tz, 2026, 10, 9, hour, minute);
        assert_eq!(slot_at(day(0, 0), tz).unwrap().index, 0);
        assert_eq!(slot_at(day(5, 59), tz).unwrap().index, 0);
        assert_eq!(slot_at(day(6, 0), tz).unwrap().index, 1);
        assert_eq!(slot_at(day(11, 59), tz).unwrap().index, 1);
        assert_eq!(slot_at(day(12, 0), tz).unwrap().index, 2);
        assert_eq!(slot_at(day(17, 59), tz).unwrap().index, 2);
        assert_eq!(slot_at(day(18, 0), tz).unwrap().index, 3);
        assert_eq!(slot_at(day(23, 59), tz).unwrap().index, 3);
        assert!(same_slot(day(1, 0), day(5, 0), tz));
        assert!(!same_slot(day(5, 59), day(6, 0), tz));
        assert!(!same_slot(day(23, 0), at(tz, 2026, 10, 10, 1, 0), tz));
    }

    #[test]
    fn utc_slots_are_independent_of_the_default_zone() {
        let tz = parse_timezone("UTC").unwrap();
        assert_eq!(slot_at(at(tz, 2026, 1, 2, 5, 0), tz).unwrap().index, 0);
        assert_eq!(slot_at(at(tz, 2026, 1, 2, 6, 0), tz).unwrap().index, 1);
        let taipei = parse_timezone("Asia/Taipei").unwrap();
        // 2026-01-02 00:30 in Taipei is still 2026-01-01 in UTC.
        let instant = at(taipei, 2026, 1, 2, 0, 30);
        assert_eq!(slot_at(instant, taipei).unwrap().index, 0);
        assert_eq!(slot_at(instant, tz).unwrap().day, 1);
    }
}

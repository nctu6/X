//! One slot per local day in a configured time zone.
//!
//! A slot runs from 00:00 to 24:00 local time. Each date is a different
//! slot, so each account can succeed once per day.

use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub year: i32,
    pub month: u32,
    pub day: u32,
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
    fn taipei_slots_follow_local_days() {
        let tz = parse_timezone("Asia/Taipei").unwrap();
        let day = |hour, minute| at(tz, 2026, 10, 9, hour, minute);
        assert!(same_slot(day(0, 0), day(23, 59), tz));
        assert!(same_slot(day(5, 59), day(6, 0), tz));
        assert!(same_slot(day(11, 59), day(18, 0), tz));
        assert!(!same_slot(day(23, 59), at(tz, 2026, 10, 10, 0, 0), tz));
        assert!(!same_slot(day(23, 0), at(tz, 2026, 10, 10, 1, 0), tz));
    }

    #[test]
    fn utc_slots_are_independent_of_the_default_zone() {
        let tz = parse_timezone("UTC").unwrap();
        assert!(same_slot(at(tz, 2026, 1, 2, 5, 0), at(tz, 2026, 1, 2, 6, 0), tz));
        let taipei = parse_timezone("Asia/Taipei").unwrap();
        // 2026-01-02 00:30 in Taipei is still 2026-01-01 in UTC.
        let instant = at(taipei, 2026, 1, 2, 0, 30);
        assert_eq!(slot_at(instant, taipei).unwrap().day, 2);
        assert_eq!(slot_at(instant, tz).unwrap().day, 1);
    }
}

//! Refresh slots in a configured time zone.
//!
//! Each local day is split into equal windows of `hours` hours, starting
//! at 00:00 local time. `hours` divides 24, so 24 means one slot per day
//! and 6 means 00:00–06:00, 06:00–12:00, 12:00–18:00 and 18:00–24:00.
//! Each account can succeed once per slot of the tab that requested it.

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;

/// Slot length used when a tab does not set `refresh_hours`.
pub const DEFAULT_REFRESH_HOURS: u32 = 24;

/// Whether `hours` splits a day into equal windows.
pub fn valid_refresh_hours(hours: u32) -> bool {
    hours > 0 && 24 % hours == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    /// Window within the day: local hour divided by the slot length.
    pub window: u32,
}

pub fn parse_timezone(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>()
        .map_err(|_| format!("timezone \"{name}\" is not an IANA time zone"))
}

pub fn slot_at(unix: i64, tz: Tz, hours: u32) -> Option<Slot> {
    let utc = DateTime::<Utc>::from_timestamp(unix, 0)?;
    let local = utc.with_timezone(&tz);
    let hours = if valid_refresh_hours(hours) {
        hours
    } else {
        DEFAULT_REFRESH_HOURS
    };
    Some(Slot {
        year: local.year(),
        month: local.month(),
        day: local.day(),
        window: local.hour() / hours,
    })
}

pub fn same_slot(left: i64, right: i64, tz: Tz, hours: u32) -> bool {
    match (slot_at(left, tz, hours), slot_at(right, tz, hours)) {
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
    fn taipei_daily_slots_follow_local_days() {
        let tz = parse_timezone("Asia/Taipei").unwrap();
        let day = |hour, minute| at(tz, 2026, 10, 9, hour, minute);
        assert!(same_slot(day(0, 0), day(23, 59), tz, 24));
        assert!(same_slot(day(5, 59), day(6, 0), tz, 24));
        assert!(same_slot(day(11, 59), day(18, 0), tz, 24));
        assert!(!same_slot(day(23, 59), at(tz, 2026, 10, 10, 0, 0), tz, 24));
        assert!(!same_slot(day(23, 0), at(tz, 2026, 10, 10, 1, 0), tz, 24));
    }

    #[test]
    fn six_hour_slots_split_the_local_day() {
        let tz = parse_timezone("Asia/Taipei").unwrap();
        let day = |hour, minute| at(tz, 2026, 10, 9, hour, minute);
        assert!(same_slot(day(0, 0), day(5, 59), tz, 6));
        assert!(!same_slot(day(5, 59), day(6, 0), tz, 6));
        assert!(same_slot(day(6, 0), day(11, 59), tz, 6));
        assert!(!same_slot(day(11, 59), day(12, 0), tz, 6));
        assert!(!same_slot(day(17, 59), day(18, 0), tz, 6));
        assert!(same_slot(day(18, 0), day(23, 59), tz, 6));
        assert!(!same_slot(day(23, 59), at(tz, 2026, 10, 10, 0, 0), tz, 6));
        assert_eq!(slot_at(day(13, 0), tz, 6).unwrap().window, 2);
    }

    #[test]
    fn refresh_hours_must_divide_a_day() {
        for hours in [1, 2, 3, 4, 6, 8, 12, 24] {
            assert!(valid_refresh_hours(hours), "{hours}");
        }
        for hours in [0, 5, 7, 9, 10, 25, 48] {
            assert!(!valid_refresh_hours(hours), "{hours}");
        }
    }

    #[test]
    fn utc_slots_are_independent_of_the_default_zone() {
        let tz = parse_timezone("UTC").unwrap();
        assert!(same_slot(
            at(tz, 2026, 1, 2, 5, 0),
            at(tz, 2026, 1, 2, 6, 0),
            tz,
            24
        ));
        let taipei = parse_timezone("Asia/Taipei").unwrap();
        // 2026-01-02 00:30 in Taipei is still 2026-01-01 in UTC.
        let instant = at(taipei, 2026, 1, 2, 0, 30);
        assert_eq!(slot_at(instant, taipei, 24).unwrap().day, 2);
        assert_eq!(slot_at(instant, tz, 24).unwrap().day, 1);
    }
}

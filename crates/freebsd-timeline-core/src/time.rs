//! Calendar math without a date library. Everything is UTC epoch seconds;
//! the civil conversions are Howard Hinnant's well-known algorithms.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let d = d as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utc {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Utc {
    pub fn from_epoch(t: i64) -> Self {
        let days = t.div_euclid(86400);
        let secs = t.rem_euclid(86400);
        let (year, month, day) = civil_from_days(days);
        Utc {
            year,
            month,
            day,
            hour: (secs / 3600) as u32,
            minute: (secs % 3600 / 60) as u32,
            second: (secs % 60) as u32,
        }
    }

    /// `20261014-153000`, used inside snapshot names.
    pub fn compact(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `2026-10-14 15:30 UTC`
    pub fn human(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02} UTC",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }

    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// "just now", "12 min ago", "3 h ago", "2 days ago", then the date.
pub fn relative(t: i64, now: i64) -> String {
    let d = now - t;
    if d < 0 {
        return Utc::from_epoch(t).human();
    }
    match d {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        86400..=172799 => "yesterday".to_string(),
        _ if d < 86400 * 14 => format!("{} days ago", d / 86400),
        _ => Utc::from_epoch(t).date(),
    }
}

pub fn hour_key(t: i64) -> i64 {
    t.div_euclid(3600)
}

pub fn day_key(t: i64) -> i64 {
    t.div_euclid(86400)
}

/// Weeks start on Monday. 1970-01-01 was a Thursday, hence the +3.
pub fn week_key(t: i64) -> i64 {
    (day_key(t) + 3).div_euclid(7)
}

pub fn month_key(t: i64) -> i64 {
    let u = Utc::from_epoch(t);
    u.year * 12 + u.month as i64
}

pub fn year_key(t: i64) -> i64 {
    Utc::from_epoch(t).year
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for z in [-800_000i64, -1, 0, 1, 19_000, 20_740, 2_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 2, 29), 11016);
        // 2026-10-14 15:30:00 UTC
        let u = Utc::from_epoch(1_792_000_000);
        assert_eq!(u.compact().len(), 15);
        assert_eq!(Utc::from_epoch(0).compact(), "19700101-000000");
    }

    #[test]
    fn weeks_start_monday() {
        // 2024-01-01 was a Monday, 2023-12-31 a Sunday.
        let mon = days_from_civil(2024, 1, 1) * 86400;
        let sun = days_from_civil(2023, 12, 31) * 86400;
        assert_ne!(week_key(mon), week_key(sun));
        assert_eq!(week_key(mon), week_key(mon + 6 * 86400));
    }
}

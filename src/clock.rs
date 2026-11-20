use std::fmt;
use std::time::Duration;

use windows::Win32::System::SystemInformation::GetLocalTime;

pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const DAY_MS: u32 = 24 * 60 * 60 * 1000;
/// Sleep a little past midnight so the next read of the clock is already on the new day.
const PAST_MIDNIGHT_MS: u32 = 50;
/// A flush this soon after midnight is the day-boundary flush and still belongs to the old day.
const BOUNDARY_GRACE_MS: u32 = 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl Date {
    /// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    pub fn to_days(self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Self { year, month, day }
    }

    pub fn minus_days(self, n: i64) -> Self {
        Self::from_days(self.to_days() - n)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub date: Date,
    pub ms_of_day: u32,
}

impl LocalTime {
    pub fn now() -> Self {
        let t = unsafe { GetLocalTime() };
        Self {
            date: Date {
                year: i32::from(t.wYear),
                month: u32::from(t.wMonth),
                day: u32::from(t.wDay),
            },
            ms_of_day: ((u32::from(t.wHour) * 60 + u32::from(t.wMinute)) * 60
                + u32::from(t.wSecond))
                * 1000
                + u32::from(t.wMilliseconds),
        }
    }

    pub fn stamp(&self) -> String {
        let s = self.ms_of_day / 1000;
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.date.year,
            self.date.month,
            self.date.day,
            s / 3600,
            s / 60 % 60,
            s % 60
        )
    }
}

pub fn today() -> Date {
    LocalTime::now().date
}

pub fn flush_wait(now: LocalTime) -> Duration {
    let to_midnight = DAY_MS.saturating_sub(now.ms_of_day) + PAST_MIDNIGHT_MS;
    FLUSH_INTERVAL.min(Duration::from_millis(u64::from(to_midnight)))
}

/// The day a flush credits: the day the interval started on, unless the clock is well into a
/// later day (the PC slept through midnight), where the taps are fresh and belong to today.
pub fn credit_date(started: Date, now: LocalTime) -> Date {
    if now.date == started || now.ms_of_day < BOUNDARY_GRACE_MS {
        started
    } else {
        now.date
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    fn at(d: Date, h: u32, m: u32, s: u32, ms: u32) -> LocalTime {
        LocalTime {
            date: d,
            ms_of_day: ((h * 60 + m) * 60 + s) * 1000 + ms,
        }
    }

    #[test]
    fn day_number_round_trip() {
        assert_eq!(date(1970, 1, 1).to_days(), 0);
        assert_eq!(date(2026, 11, 20).to_days(), 20_777);
        for days in [-1000, 0, 59, 60, 10_000, 20_777, 50_000] {
            assert_eq!(Date::from_days(days).to_days(), days);
        }
    }

    #[test]
    fn steps_back_across_months_and_leap_days() {
        assert_eq!(date(2026, 3, 1).minus_days(1), date(2026, 2, 28));
        assert_eq!(date(2028, 3, 1).minus_days(1), date(2028, 2, 29));
        assert_eq!(date(2027, 1, 1).minus_days(1), date(2026, 12, 31));
        assert_eq!(date(2026, 11, 20).minus_days(29), date(2026, 10, 22));
    }

    #[test]
    fn formats_as_iso_date() {
        assert_eq!(date(2026, 1, 5).to_string(), "2026-01-05");
        assert_eq!(
            at(date(2026, 1, 5), 9, 3, 7, 500).stamp(),
            "20260105-090307"
        );
    }

    #[test]
    fn waits_the_interval_during_the_day() {
        assert_eq!(
            flush_wait(at(date(2026, 11, 20), 12, 0, 0, 0)),
            FLUSH_INTERVAL
        );
    }

    #[test]
    fn wakes_just_past_midnight() {
        let wait = flush_wait(at(date(2026, 11, 20), 23, 59, 50, 0));
        assert_eq!(wait, Duration::from_millis(10_050));
    }

    #[test]
    fn boundary_flush_credits_the_old_day() {
        let old = date(2026, 11, 20);
        let new = date(2026, 11, 21);
        assert_eq!(credit_date(old, at(old, 23, 59, 59, 900)), old);
        assert_eq!(credit_date(old, at(new, 0, 0, 0, 50)), old);
        assert_eq!(credit_date(new, at(new, 0, 0, 30, 0)), new);
    }

    #[test]
    fn slept_through_midnight_credits_today() {
        let old = date(2026, 11, 20);
        let later = date(2026, 11, 22);
        assert_eq!(credit_date(old, at(later, 8, 15, 0, 0)), later);
    }
}

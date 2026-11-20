use std::fmt;
use std::time::Duration;

use windows::Win32::System::SystemInformation::GetLocalTime;

pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const HOUR_MS: u32 = 60 * 60 * 1000;
/// Sleep a little past the hour so the next read of the clock is already in the new hour.
const PAST_BOUNDARY_MS: u32 = 50;
/// A flush this soon after the hour is the boundary flush and still belongs to the old hour.
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

    pub fn plus_days(self, n: i64) -> Self {
        Self::from_days(self.to_days() + n)
    }

    /// 0 = Monday .. 6 = Sunday.
    pub fn weekday(self) -> usize {
        (self.to_days() + 3).rem_euclid(7) as usize
    }

    pub fn monday(self) -> Self {
        self.minus_days(self.weekday() as i64)
    }

    pub fn month(self) -> Month {
        Month {
            year: self.year,
            month: self.month,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Month {
    pub year: i32,
    pub month: u32,
}

impl fmt::Display for Month {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

impl Month {
    pub fn parse(text: &str) -> Option<Self> {
        let (year, month) = text.split_once('-')?;
        if year.len() != 4 || month.len() != 2 {
            return None;
        }
        let month = Self {
            year: year.parse().ok()?,
            month: month.parse().ok()?,
        };
        (1..=12).contains(&month.month).then_some(month)
    }

    pub fn first(self) -> Date {
        Date {
            year: self.year,
            month: self.month,
            day: 1,
        }
    }

    pub fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    pub fn previous(self) -> Self {
        if self.month == 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                year: self.year,
                month: self.month - 1,
            }
        }
    }
}

/// One local clock hour. A DST fall-back hour happens twice and both land in the same `Hour`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hour {
    pub date: Date,
    pub hour: u32,
}

impl fmt::Display for Hour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}T{:02}", self.date, self.hour)
    }
}

impl Hour {
    pub fn start_of(date: Date) -> Self {
        Self { date, hour: 0 }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (date, hour) = text.split_once('T')?;
        let (month, day) = date.rsplit_once('-')?;
        let month = Month::parse(month)?;
        if day.len() != 2 || hour.len() != 2 {
            return None;
        }
        let date = Date {
            year: month.year,
            month: month.month,
            day: day.parse().ok()?,
        };
        let hour: u32 = hour.parse().ok()?;
        (hour < 24 && Date::from_days(date.to_days()) == date).then_some(Self { date, hour })
    }

    pub fn index(self) -> i64 {
        self.date.to_days() * 24 + i64::from(self.hour)
    }

    pub fn from_index(index: i64) -> Self {
        Self {
            date: Date::from_days(index.div_euclid(24)),
            hour: index.rem_euclid(24) as u32,
        }
    }

    pub fn plus(self, hours: i64) -> Self {
        Self::from_index(self.index() + hours)
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

    pub fn hour(&self) -> Hour {
        Hour {
            date: self.date,
            hour: self.ms_of_day / HOUR_MS,
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

pub fn now_hour() -> Hour {
    LocalTime::now().hour()
}

pub fn flush_wait(now: LocalTime) -> Duration {
    let to_next_hour = HOUR_MS - now.ms_of_day % HOUR_MS + PAST_BOUNDARY_MS;
    FLUSH_INTERVAL.min(Duration::from_millis(u64::from(to_next_hour)))
}

/// The hour a flush credits: the hour the interval started in, unless the clock is well into a
/// later hour (the PC slept through the boundary), where the taps are fresh and belong to now.
pub fn credit_hour(started: Hour, now: LocalTime) -> Hour {
    if now.hour() == started || now.ms_of_day % HOUR_MS < BOUNDARY_GRACE_MS {
        started
    } else {
        now.hour()
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

    fn hour(d: Date, h: u32) -> Hour {
        Hour { date: d, hour: h }
    }

    #[test]
    fn waits_the_interval_mid_hour() {
        assert_eq!(
            flush_wait(at(date(2026, 11, 20), 12, 10, 0, 0)),
            FLUSH_INTERVAL
        );
    }

    #[test]
    fn wakes_just_past_the_hour() {
        let wait = flush_wait(at(date(2026, 11, 20), 13, 59, 50, 0));
        assert_eq!(wait, Duration::from_millis(10_050));
        let wait = flush_wait(at(date(2026, 11, 20), 23, 59, 50, 0));
        assert_eq!(wait, Duration::from_millis(10_050));
    }

    #[test]
    fn boundary_flush_credits_the_old_hour() {
        let day = date(2026, 11, 20);
        assert_eq!(
            credit_hour(hour(day, 13), at(day, 13, 59, 59, 900)),
            hour(day, 13)
        );
        assert_eq!(
            credit_hour(hour(day, 13), at(day, 14, 0, 0, 50)),
            hour(day, 13)
        );
        assert_eq!(
            credit_hour(hour(day, 14), at(day, 14, 0, 30, 0)),
            hour(day, 14)
        );
    }

    #[test]
    fn midnight_flush_credits_the_old_day() {
        let old = date(2026, 11, 20);
        let new = date(2026, 11, 21);
        assert_eq!(
            credit_hour(hour(old, 23), at(new, 0, 0, 0, 50)),
            hour(old, 23)
        );
    }

    #[test]
    fn slept_through_the_hour_credits_now() {
        let old = date(2026, 11, 20);
        let later = date(2026, 11, 22);
        assert_eq!(
            credit_hour(hour(old, 23), at(later, 8, 15, 0, 0)),
            hour(later, 8)
        );
        assert_eq!(
            credit_hour(hour(old, 9), at(old, 11, 5, 0, 0)),
            hour(old, 11)
        );
    }

    #[test]
    fn hour_keys_round_trip() {
        let h = hour(date(2026, 3, 7), 5);
        assert_eq!(h.to_string(), "2026-03-07T05");
        assert_eq!(Hour::parse("2026-03-07T05"), Some(h));
        for bad in [
            "2026-02-30T05",
            "2026-03-07T24",
            "2026-13-01T00",
            "2026-3-07T05",
            "x",
            "",
        ] {
            assert_eq!(Hour::parse(bad), None, "{bad}");
        }
        assert_eq!(
            Month::parse("2026-11").map(|m| m.to_string()).as_deref(),
            Some("2026-11")
        );
        assert_eq!(Month::parse("2026-11.corrupt-x"), None);
    }

    #[test]
    fn hour_steps_cross_days() {
        let h = hour(date(2026, 12, 31), 23);
        assert_eq!(h.plus(1), hour(date(2027, 1, 1), 0));
        assert_eq!(hour(date(2027, 1, 1), 0).plus(-1), h);
        assert!(hour(date(2026, 11, 20), 23) < hour(date(2026, 11, 21), 0));
    }

    #[test]
    fn weeks_start_on_monday() {
        assert_eq!(date(2026, 11, 16).weekday(), 0);
        assert_eq!(date(2026, 11, 20).weekday(), 4);
        assert_eq!(date(2026, 11, 22).weekday(), 6);
        assert_eq!(date(2026, 11, 22).monday(), date(2026, 11, 16));
        assert_eq!(date(2026, 11, 16).monday(), date(2026, 11, 16));
        assert_eq!(date(2027, 1, 2).monday(), date(2026, 12, 28));
    }

    #[test]
    fn months_wrap_years() {
        let jan = Month {
            year: 2027,
            month: 1,
        };
        let dec = Month {
            year: 2026,
            month: 12,
        };
        assert_eq!(jan.previous(), dec);
        assert_eq!(dec.next(), jan);
        assert_eq!(date(2028, 2, 29).month().first(), date(2028, 2, 1));
    }
}

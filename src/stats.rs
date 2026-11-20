use crate::clock::{Date, Hour};
use crate::counts::Bucket;
use crate::storage::{Hours, sum};

pub const CALENDAR_WEEKS: usize = 53;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Hour,
    Day,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Self; 4] = [Self::Hour, Self::Day, Self::Week, Self::Month];

    pub fn label(self) -> &'static str {
        match self {
            Self::Hour => "Hour",
            Self::Day => "Day",
            Self::Week => "Week",
            Self::Month => "Month",
        }
    }

    pub fn current_label(self) -> &'static str {
        match self {
            Self::Hour => "THIS HOUR",
            Self::Day => "TODAY",
            Self::Week => "THIS WEEK",
            Self::Month => "THIS MONTH",
        }
    }

    pub fn previous_label(self) -> &'static str {
        match self {
            Self::Hour => "Last hour",
            Self::Day => "Yesterday",
            Self::Week => "Last week",
            Self::Month => "Last month",
        }
    }

    /// `(previous start, start, end)`: the period holding `now` is `start..end`, the one before
    /// it is `previous start..start`.
    pub fn bounds(self, now: Hour) -> (Hour, Hour, Hour) {
        let day = |date| Hour::start_of(date);
        match self {
            Self::Hour => (now.plus(-1), now, now.plus(1)),
            Self::Day => (
                day(now.date.minus_days(1)),
                day(now.date),
                day(now.date.plus_days(1)),
            ),
            Self::Week => {
                let monday = now.date.monday();
                (
                    day(monday.minus_days(7)),
                    day(monday),
                    day(monday.plus_days(7)),
                )
            }
            Self::Month => {
                let month = now.date.month();
                (
                    day(month.previous().first()),
                    day(month.first()),
                    day(month.next().first()),
                )
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stats {
    pub now: Hour,
    /// `(current, previous)` per `Period::ALL`.
    pub periods: [(Bucket, Bucket); 4],
    pub total: Bucket,
    /// `[weekday from Monday][hour]`, all-time.
    pub time_of_day: [[Bucket; 24]; 7],
    pub calendar_start: Date,
    /// One per day from `calendar_start`, through the Sunday of this week.
    pub days: Vec<Bucket>,
}

impl Stats {
    pub fn period(&self, period: Period) -> (Bucket, Bucket) {
        self.periods[Period::ALL.iter().position(|p| *p == period).unwrap_or(0)]
    }
}

/// `pending` holds the taps not flushed yet; they all belong to `now`.
pub fn compute(hours: &Hours, total: Bucket, pending: Bucket, now: Hour) -> Stats {
    let periods = Period::ALL.map(|p| {
        let (previous, start, end) = p.bounds(now);
        let mut current = sum(hours, start, end);
        current += pending;
        (current, sum(hours, previous, start))
    });

    let mut time_of_day = [[Bucket::default(); 24]; 7];
    for (hour, b) in hours {
        time_of_day[hour.date.weekday()][hour.hour as usize] += *b;
    }
    time_of_day[now.date.weekday()][now.hour as usize] += pending;

    let calendar_start = now
        .date
        .monday()
        .minus_days(7 * (CALENDAR_WEEKS as i64 - 1));
    let first = calendar_start.to_days();
    let mut days = vec![Bucket::default(); CALENDAR_WEEKS * 7];
    for (hour, b) in hours.range(Hour::start_of(calendar_start)..) {
        if let Some(day) = days.get_mut((hour.date.to_days() - first) as usize) {
            *day += *b;
        }
    }
    if let Some(today) = days.get_mut((now.date.to_days() - first) as usize) {
        *today += pending;
    }

    let mut total = total;
    total += pending;
    Stats {
        now,
        periods,
        total,
        time_of_day,
        calendar_start,
        days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    fn at(d: Date, hour: u32) -> Hour {
        Hour { date: d, hour }
    }

    fn keys(keys: u64) -> Bucket {
        Bucket {
            keys,
            ..Default::default()
        }
    }

    #[test]
    fn week_starts_monday_and_spans_years() {
        let (prev, start, end) = Period::Week.bounds(at(date(2027, 1, 3), 22));
        assert_eq!(prev, at(date(2026, 12, 21), 0));
        assert_eq!(start, at(date(2026, 12, 28), 0));
        assert_eq!(end, at(date(2027, 1, 4), 0));
        let (_, start, _) = Period::Week.bounds(at(date(2026, 11, 16), 0));
        assert_eq!(start, at(date(2026, 11, 16), 0));
    }

    #[test]
    fn month_bounds_wrap() {
        let (prev, start, end) = Period::Month.bounds(at(date(2027, 1, 31), 23));
        assert_eq!(prev, at(date(2026, 12, 1), 0));
        assert_eq!(start, at(date(2027, 1, 1), 0));
        assert_eq!(end, at(date(2027, 2, 1), 0));
        let (prev, _, end) = Period::Month.bounds(at(date(2028, 3, 1), 0));
        assert_eq!(prev, at(date(2028, 2, 1), 0));
        assert_eq!(end, at(date(2028, 4, 1), 0));
    }

    #[test]
    fn hour_and_day_bounds_cross_midnight() {
        let (prev, start, end) = Period::Hour.bounds(at(date(2026, 11, 21), 0));
        assert_eq!(prev, at(date(2026, 11, 20), 23));
        assert_eq!(start, at(date(2026, 11, 21), 0));
        assert_eq!(end, at(date(2026, 11, 21), 1));
        let (prev, _, end) = Period::Day.bounds(at(date(2026, 3, 1), 5));
        assert_eq!(prev, at(date(2026, 2, 28), 0));
        assert_eq!(end, at(date(2026, 3, 2), 0));
    }

    #[test]
    fn periods_sum_their_hours_and_pending() {
        let now = at(date(2026, 11, 20), 14);
        let mut hours = Hours::new();
        hours.insert(now, keys(1));
        hours.insert(at(date(2026, 11, 20), 13), keys(10));
        hours.insert(at(date(2026, 11, 19), 23), keys(100));
        hours.insert(at(date(2026, 11, 16), 0), keys(1_000));
        hours.insert(at(date(2026, 11, 15), 23), keys(10_000));
        hours.insert(at(date(2026, 11, 1), 0), keys(100_000));
        hours.insert(at(date(2026, 10, 31), 23), keys(1_000_000));
        let total = hours.values().fold(Bucket::default(), |mut a, b| {
            a += *b;
            a
        });

        let s = compute(&hours, total, keys(5), now);
        assert_eq!(s.period(Period::Hour), (keys(6), keys(10)));
        assert_eq!(s.period(Period::Day), (keys(16), keys(100)));
        assert_eq!(s.period(Period::Week), (keys(1_116), keys(10_000)));
        assert_eq!(s.period(Period::Month), (keys(111_116), keys(1_000_000)));
        assert_eq!(s.total, keys(1_111_116));
    }

    #[test]
    fn time_of_day_buckets_by_weekday_and_hour() {
        let now = at(date(2026, 11, 20), 14);
        let mut hours = Hours::new();
        hours.insert(at(date(2026, 11, 16), 9), keys(1));
        hours.insert(at(date(2026, 11, 9), 9), keys(2));
        hours.insert(at(date(2026, 11, 22), 23), keys(4));
        let s = compute(&hours, Bucket::default(), keys(8), now);
        assert_eq!(s.time_of_day[0][9], keys(3), "two Mondays at 09");
        assert_eq!(s.time_of_day[6][23], keys(4), "Sunday last");
        assert_eq!(s.time_of_day[4][14], keys(8), "pending lands on now");
    }

    #[test]
    fn calendar_covers_53_weeks_ending_this_week() {
        let now = at(date(2026, 11, 20), 14);
        let mut hours = Hours::new();
        let start = date(2026, 11, 16).minus_days(52 * 7);
        hours.insert(at(start.minus_days(1), 12), keys(99));
        hours.insert(at(start, 0), keys(1));
        hours.insert(at(start, 5), keys(2));
        hours.insert(at(date(2026, 11, 19), 3), keys(7));
        let s = compute(&hours, Bucket::default(), keys(5), now);
        assert_eq!(s.calendar_start, start);
        assert_eq!(s.calendar_start.weekday(), 0);
        assert_eq!(s.days.len(), 53 * 7);
        assert_eq!(s.days[0], keys(3), "the day before the window is left out");
        let today = (now.date.to_days() - start.to_days()) as usize;
        assert_eq!(today, 52 * 7 + 4);
        assert_eq!(s.days[today - 1], keys(7));
        assert_eq!(s.days[today], keys(5));
    }
}

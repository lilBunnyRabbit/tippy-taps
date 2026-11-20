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
    pub avg7: Vec<f32>,
    /// Inputs per hour for the last `RECENT_DAYS` day-shape days, oldest first, hours from
    /// `DAY_START`.
    pub recent: [[u64; 24]; RECENT_DAYS],
    /// Per `RANGES`: how many of the active days had input in each hour.
    pub typical: [Typical; 3],
    pub records: Records,
    pub streaks: Streaks,
    pub weekdays: [[f32; 3]; 7],
}

pub const RANGES: [usize; 3] = [30, 90, 365];
pub const RECENT_DAYS: usize = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Typical {
    pub active_days: u32,
    pub share: [f32; 24],
    /// Median first hour and median end (last active hour + 1), from `DAY_START`.
    pub usual: Option<(u8, u8)>,
    pub mean_hours: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Records {
    pub hour: Option<(Hour, u64)>,
    pub day: Option<(Date, u64)>,
    pub week: Option<(Date, u64)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Streaks {
    /// Active days in a row up to today, or up to yesterday while today is still empty.
    pub current: u32,
    pub longest: u32,
    pub longest_end: Option<Date>,
}

pub fn kcs(b: &Bucket) -> u64 {
    b.keys + b.clicks + b.scrolls
}

struct Day {
    date: Date,
    bucket: Bucket,
}

fn all_hours(hours: &Hours, pending: Bucket, now: Hour) -> impl Iterator<Item = (Hour, Bucket)> {
    let extra = (!hours.contains_key(&now) && !pending.is_empty()).then_some((now, pending));
    hours
        .iter()
        .map(move |(h, b)| {
            let mut b = *b;
            if *h == now {
                b += pending;
            }
            (*h, b)
        })
        .chain(extra)
}

/// A late night belongs to the day before it: day-shape days run from this hour to this hour.
pub const DAY_START: u32 = 4;

fn all_days(hours: &Hours, pending: Bucket, now: Hour) -> Vec<Day> {
    let mut days: Vec<Day> = Vec::new();
    for (hour, b) in all_hours(hours, pending, now) {
        match days.last_mut() {
            Some(day) if day.date == hour.date => day.bucket += b,
            _ => days.push(Day {
                date: hour.date,
                bucket: b,
            }),
        }
    }
    days
}

fn median(values: &mut [u8]) -> u8 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn typical(masks: &[u32]) -> Typical {
    let active: Vec<u32> = masks.iter().copied().filter(|m| *m != 0).collect();
    if active.is_empty() {
        return Typical::default();
    }
    let n = active.len() as f32;
    let mut first: Vec<u8> = active.iter().map(|m| m.trailing_zeros() as u8).collect();
    let mut end: Vec<u8> = active
        .iter()
        .map(|m| 32 - m.leading_zeros() as u8)
        .collect();
    Typical {
        active_days: active.len() as u32,
        share: std::array::from_fn(|h| {
            active.iter().filter(|m| *m & 1 << h != 0).count() as f32 / n
        }),
        usual: Some((median(&mut first), median(&mut end))),
        mean_hours: active.iter().map(|m| m.count_ones()).sum::<u32>() as f32 / n,
    }
}

fn best<T>(items: impl Iterator<Item = (T, u64)>) -> Option<(T, u64)> {
    items
        .filter(|i| i.1 > 0)
        .reduce(|a, b| if b.1 > a.1 { b } else { a })
}

fn records(hours: &Hours, days: &[Day], pending: Bucket, now: Hour) -> Records {
    let mut weeks: Vec<(Date, u64)> = Vec::new();
    for d in days {
        let monday = d.date.monday();
        match weeks.last_mut() {
            Some(w) if w.0 == monday => w.1 += d.bucket.total(),
            _ => weeks.push((monday, d.bucket.total())),
        }
    }
    Records {
        hour: best(all_hours(hours, pending, now).map(|(h, b)| (h, b.total()))),
        day: best(days.iter().map(|d| (d.date, d.bucket.total()))),
        week: best(weeks.into_iter()),
    }
}

fn streaks(days: &[Day], today: Date) -> Streaks {
    let mut s = Streaks::default();
    let mut run = 0u32;
    let mut prev: Option<i64> = None;
    for d in days.iter().filter(|d| d.bucket.total() > 0) {
        let n = d.date.to_days();
        run = if prev == Some(n - 1) { run + 1 } else { 1 };
        prev = Some(n);
        if run > s.longest {
            s.longest = run;
            s.longest_end = Some(d.date);
        }
    }
    if prev.is_some_and(|p| p >= today.to_days() - 1) {
        s.current = run;
    }
    s
}

fn weekdays(days: &[Day], today: Date) -> [[f32; 3]; 7] {
    let Some(first) = days.iter().find(|d| d.bucket.total() > 0).map(|d| d.date) else {
        return [[0.0; 3]; 7];
    };
    let mut sums = [[0u64; 3]; 7];
    for d in days.iter().filter(|d| d.date >= first) {
        let w = d.date.weekday();
        sums[w][0] += d.bucket.keys;
        sums[w][1] += d.bucket.clicks;
        sums[w][2] += d.bucket.scrolls;
    }
    let span = today.to_days() - first.to_days() + 1;
    std::array::from_fn(|w| {
        let offset = (w as i64 - first.weekday() as i64).rem_euclid(7);
        let count = (span - offset + 6).div_euclid(7).max(1) as f32;
        sums[w].map(|n| n as f32 / count)
    })
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

    let all = all_days(hours, pending, now);
    let shift = -i64::from(DAY_START);
    let shape_today = (now.plus(shift).date.to_days() - first) as usize;
    let recent_first = shape_today + 1 - RECENT_DAYS;
    // Per day-shape day: bit `h` is set when hour `DAY_START + h` had input.
    let mut masks = vec![0u32; days.len()];
    let mut recent = [[0u64; 24]; RECENT_DAYS];
    let from = Hour::start_of(calendar_start).plus(-shift);
    let stored = hours.range(from..).map(|(h, b)| (*h, *b));
    let late = (!hours.contains_key(&now)).then_some((now, Bucket::default()));
    for (hour, mut b) in stored.chain(late) {
        if hour == now {
            b += pending;
        }
        let at = hour.plus(shift);
        let i = (at.date.to_days() - first) as usize;
        if b.total() == 0 || i >= days.len() {
            continue;
        }
        masks[i] |= 1 << at.hour;
        if let Some(row) = i.checked_sub(recent_first).and_then(|r| recent.get_mut(r)) {
            row[at.hour as usize] += b.total();
        }
    }
    let typical =
        RANGES.map(|r| typical(&masks[(shape_today + 1).saturating_sub(r)..=shape_today]));
    let mut avg7 = Vec::with_capacity(days.len());
    let mut window = 0u64;
    for (i, d) in days.iter().enumerate() {
        window += kcs(d);
        if i >= 7 {
            window -= kcs(&days[i - 7]);
        }
        avg7.push(window as f32 / (i + 1).min(7) as f32);
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
        avg7,
        recent,
        typical,
        records: records(hours, &all, pending, now),
        streaks: streaks(&all, now.date),
        weekdays: weekdays(&all, now.date),
    }
}

pub const BACKSPACE: usize = 0x0e;
const SPACE: usize = 0x39;

/// Keys the left hand types in touch typing, by scan code: Esc, F1-F4, the digits up to 5, the
/// QWERT/ASDFG/ZXCVB columns and the modifiers left of them. Space is left out; the rest is right.
fn left_hand(slot: usize) -> bool {
    matches!(
        slot,
        0x01 | 0x3b..=0x3e
            | 0x29
            | 0x02..=0x06
            | 0x0f
            | 0x10..=0x14
            | 0x3a
            | 0x1e..=0x22
            | 0x2a
            | 0x56
            | 0x2c..=0x30
            | 0x1d
            | 0x15b
            | 0x38
    )
}

fn board_slot(slot: usize) -> usize {
    match slot {
        0x136 => 0x36,
        0x54 => 0x137,
        s => s,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyInsights {
    pub total: u64,
    pub backspace: u64,
    pub left: u64,
    pub right: u64,
    pub top: Vec<(usize, u64)>,
}

pub fn key_insights(counts: &[u64]) -> KeyInsights {
    let mut merged = vec![0u64; counts.len()];
    for (slot, n) in counts.iter().enumerate() {
        if let Some(m) = merged.get_mut(board_slot(slot)) {
            *m += n;
        }
    }
    let mut k = KeyInsights {
        total: merged.iter().sum(),
        backspace: merged.get(BACKSPACE).copied().unwrap_or(0),
        ..Default::default()
    };
    for (slot, n) in merged.iter().enumerate().filter(|(s, _)| *s != SPACE) {
        if left_hand(slot) {
            k.left += n;
        } else {
            k.right += n;
        }
    }
    let mut ranked: Vec<_> = merged
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(s, n)| (s, *n))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(10);
    k.top = ranked;
    k
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

    #[test]
    fn records_pick_the_busiest_hour_day_and_week() {
        let now = at(date(2026, 11, 20), 14);
        let mut hours = Hours::new();
        hours.insert(at(date(2026, 11, 2), 9), keys(50));
        hours.insert(at(date(2026, 11, 3), 9), keys(30));
        hours.insert(at(date(2026, 11, 3), 10), keys(30));
        hours.insert(at(date(2026, 11, 10), 9), keys(40));
        let s = compute(&hours, Bucket::default(), keys(55), now);
        assert_eq!(s.records.hour, Some((now, 55)), "pending counts toward now");
        assert_eq!(s.records.day, Some((date(2026, 11, 3), 60)));
        assert_eq!(s.records.week, Some((date(2026, 11, 2), 110)));
        assert_eq!(
            compute(&Hours::new(), Bucket::default(), Bucket::default(), now).records,
            Records::default()
        );
    }

    #[test]
    fn streaks_run_through_yesterday_until_today_has_input() {
        let now = at(date(2026, 11, 20), 8);
        let mut hours = Hours::new();
        for d in [1, 2, 3, 4, 10, 17, 18, 19] {
            hours.insert(at(date(2026, 11, d), 12), keys(1));
        }
        let s = compute(&hours, Bucket::default(), Bucket::default(), now);
        assert_eq!(s.streaks.current, 3, "17-19, today still empty");
        assert_eq!(s.streaks.longest, 4);
        assert_eq!(s.streaks.longest_end, Some(date(2026, 11, 4)));
        let s = compute(&hours, Bucket::default(), keys(1), now);
        assert_eq!(s.streaks.current, 4);
        hours.remove(&at(date(2026, 11, 19), 12));
        let s = compute(&hours, Bucket::default(), Bucket::default(), now);
        assert_eq!(s.streaks.current, 0);
    }

    #[test]
    fn weekday_averages_count_empty_days_since_the_first() {
        let now = at(date(2026, 11, 20), 14);
        let mut hours = Hours::new();
        hours.insert(at(date(2026, 11, 2), 9), keys(10));
        hours.insert(at(date(2026, 11, 16), 9), keys(20));
        let s = compute(&hours, Bucket::default(), Bucket::default(), now);
        assert_eq!(s.weekdays[0], [10.0, 0.0, 0.0], "three Mondays from 2 Nov");
        assert_eq!(s.weekdays[4], [0.0; 3]);
    }

    #[test]
    fn seven_day_average_trails_each_day() {
        let now = at(date(2026, 11, 20), 14);
        let mut hours = Hours::new();
        hours.insert(at(date(2026, 11, 13), 9), keys(70));
        let s = compute(&hours, Bucket::default(), Bucket::default(), now);
        let day = |d: Date| (d.to_days() - s.calendar_start.to_days()) as usize;
        assert_eq!(s.avg7[day(date(2026, 11, 13))], 10.0);
        assert_eq!(s.avg7[day(date(2026, 11, 19))], 10.0);
        assert_eq!(s.avg7[day(date(2026, 11, 20))], 0.0);
    }

    #[test]
    fn day_shape_days_start_at_four() {
        let now = at(date(2026, 11, 20), 2);
        let mut hours = Hours::new();
        hours.insert(at(date(2026, 11, 19), 9), keys(1));
        hours.insert(at(date(2026, 11, 19), 23), keys(2));
        hours.insert(at(date(2026, 11, 20), 0), keys(3));
        hours.insert(at(date(2026, 11, 18), 10), keys(4));
        hours.insert(at(date(2026, 11, 18), 20), keys(5));
        let s = compute(&hours, Bucket::default(), keys(6), now);
        let today = s.recent[RECENT_DAYS - 1];
        assert_eq!(
            today[(9 - DAY_START) as usize],
            1,
            "02:00 still belongs to the 19th"
        );
        assert_eq!(
            today[(24 - DAY_START) as usize],
            3,
            "midnight is hour 20 of the day"
        );
        assert_eq!(today[(26 - DAY_START) as usize], 6, "pending at 02:00");
        assert_eq!(s.recent[RECENT_DAYS - 2][(20 - DAY_START) as usize], 5);
        let t = s.typical[0];
        assert_eq!(t.active_days, 2);
        assert_eq!(t.share[(9 - DAY_START) as usize], 0.5);
        assert_eq!(
            t.usual,
            Some((6, 23)),
            "medians of 5,6 and 17,23 pick the upper"
        );
        assert_eq!(t.mean_hours, 3.0);
    }

    #[test]
    fn key_insights_split_hands_and_rank_keys() {
        let mut counts = vec![0u64; 512];
        counts[0x1e] = 40;
        counts[0x24] = 30;
        counts[BACKSPACE] = 10;
        counts[0x39] = 100;
        counts[0x36] = 5;
        counts[0x136] = 7;
        let k = key_insights(&counts);
        assert_eq!(k.total, 192);
        assert_eq!(k.backspace, 10);
        assert_eq!(
            (k.left, k.right),
            (40, 52),
            "A left; J, Backspace, Right Shift right"
        );
        assert_eq!(k.top[0], (0x39, 100));
        assert_eq!(k.top[3], (0x36, 12), "Right Shift's alias slot merges in");
        assert_eq!(k.top.len(), 5);
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::clock::{self, Date, Hour, LocalTime, Month};
use crate::counts::{self, Bucket};
use crate::{keyboard, mouse};

pub const DIR_NAME: &str = "counts";
const VERSION: u32 = 2;
const BACKUP_SUFFIX: &str = ".v1-backup";
const SPLIT_SUFFIX: &str = ".split";

pub type Hours = BTreeMap<Hour, Bucket>;

#[derive(Serialize, Deserialize)]
struct Shard {
    version: u32,
    hours: BTreeMap<String, Bucket>,
}

/// Bumped whenever the stored hours change, so readers can cache what they derive from them.
static REVISION: AtomicU64 = AtomicU64::new(0);

pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

pub fn sum(hours: &Hours, from: Hour, to: Hour) -> Bucket {
    let mut sum = Bucket::default();
    for b in hours.range(from..to).map(|(_, b)| b) {
        sum += *b;
    }
    sum
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum History {
    Loading,
    Loaded,
    Unlisted,
}

type DayRead = (Date, io::Result<Loaded<Hours>>);

pub struct HistoryRead(io::Result<Vec<DayRead>>);

/// One file per local day (`YYYY/MM-DD.json`), so a flush rewrites only the days it touched.
pub struct Storage {
    pub dir: PathBuf,
    pub hours: Hours,
    pub total: Bucket,
    pub error: Option<String>,
    /// Lasts the whole session, unlike `error`, which the next good save clears.
    pub notice: Option<String>,
    dirty: BTreeSet<Date>,
    /// Day files that exist but could not be read. They are never written over: their counts
    /// pile up in memory and every flush retries the read.
    blocked: BTreeSet<Date>,
    /// Days whose file has been read into `hours`. Only these are written, and a history read
    /// that arrives later skips them, so nothing is counted twice.
    read: BTreeSet<Date>,
    history: History,
    /// v0.1 months whose split failed. Their days are held like unreadable files: counts stay in
    /// memory, no day of the month is read or written, and every flush retries the split.
    unsplit: BTreeSet<Month>,
}

impl Storage {
    pub fn open_today(dir: PathBuf, today: Date, stamp: &str) -> Self {
        let mut storage = Self {
            dir,
            hours: Hours::new(),
            total: Bucket::default(),
            error: None,
            notice: None,
            dirty: BTreeSet::new(),
            blocked: BTreeSet::new(),
            read: BTreeSet::new(),
            history: History::Loading,
            unsplit: BTreeSet::new(),
        };
        storage.upgrade(stamp);
        if !storage.held(today) {
            storage.load_day(today, stamp);
        }
        storage
    }

    #[cfg(test)]
    pub fn open(dir: PathBuf, today: Date, stamp: &str) -> Self {
        let mut storage = Self::open_today(dir, today, stamp);
        let history = read_history(&storage.dir, &storage.skip(), stamp);
        storage.merge_history(history);
        storage
    }

    pub fn saving_blocked(&self) -> bool {
        !self.clear()
    }

    pub fn complete(&self) -> bool {
        self.history == History::Loaded && self.clear()
    }

    fn clear(&self) -> bool {
        self.blocked.is_empty() && self.unsplit.is_empty()
    }

    fn held(&self, date: Date) -> bool {
        self.unsplit.contains(&date.month())
    }

    fn skip(&self) -> Skip {
        Skip {
            days: self.read.clone(),
            months: self.unsplit.clone(),
        }
    }

    pub fn day(&self, date: Date) -> Bucket {
        sum(
            &self.hours,
            Hour::start_of(date),
            Hour::start_of(date.plus_days(1)),
        )
    }

    pub fn merge_history(&mut self, history: HistoryRead) {
        match history.0 {
            Ok(days) => {
                let was_unlisted = self.history == History::Unlisted;
                let kept = self.error.take();
                let failed = days.iter().any(|(_, r)| r.is_err());
                self.history = History::Loaded;
                for (date, result) in days {
                    if !self.read.contains(&date) && !self.held(date) {
                        self.apply_day(date, result);
                    }
                }
                if !failed {
                    self.error = if was_unlisted { None } else { kept };
                }
            }
            Err(err) => {
                self.history = History::Unlisted;
                self.error = Some(format!(
                    "Can't open {}: {err}. Counting in memory and retrying every 30 s.",
                    self.dir.display()
                ));
            }
        }
        REVISION.fetch_add(1, Ordering::Relaxed);
    }

    pub fn flush(&mut self, hour: Hour, taken: Bucket, stamp: &str) {
        if !taken.is_empty() {
            *self.hours.entry(hour).or_default() += taken;
            self.total += taken;
            self.dirty.insert(hour.date);
            REVISION.fetch_add(1, Ordering::Relaxed);
        }
        if self.history == History::Unlisted {
            let history = read_history(&self.dir, &self.skip(), stamp);
            self.merge_history(history);
        }
        for month in self.unsplit.clone() {
            self.split(month, stamp);
        }
        for date in self.blocked.clone() {
            self.load_day(date, stamp);
        }
        if self.dirty.is_empty() {
            return;
        }
        let mut failed = None;
        for date in self.dirty.clone() {
            if self.held(date) {
                continue;
            }
            if !self.read.contains(&date) && !self.blocked.contains(&date) {
                self.load_day(date, stamp);
            }
            if self.blocked.contains(&date) {
                continue;
            }
            match save_day(&self.dir, date, &day_hours(&self.hours, date)) {
                Ok(()) => {
                    self.dirty.remove(&date);
                }
                Err(err) => failed = Some(format!("Can't save counts: {err}")),
            }
        }
        if failed.is_some() || self.clear() {
            self.error = failed;
        }
    }

    fn load_day(&mut self, date: Date, stamp: &str) {
        let result = read_day(&self.dir, date, stamp);
        self.apply_day(date, result);
    }

    /// Counts already in memory for this day were never saved (its file was not read yet), so
    /// they are added on top of what the file holds.
    fn apply_day(&mut self, date: Date, result: io::Result<Loaded<Hours>>) {
        match result {
            Ok(loaded) => {
                self.blocked.remove(&date);
                self.read.insert(date);
                for (hour, bucket) in loaded.value.unwrap_or_default() {
                    *self.hours.entry(hour).or_default() += bucket;
                    self.total += bucket;
                }
                if let Some(p) = loaded.moved_to {
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    self.notice = Some(format!(
                        "{} was unreadable and was moved to {name}",
                        day_name(date)
                    ));
                    if !self.day(date).is_empty() {
                        self.dirty.insert(date);
                    }
                }
                if self.clear() {
                    self.error = None;
                }
                REVISION.fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                self.blocked.insert(date);
                self.error = Some(format!(
                    "Can't open {}: {err}. Counting in memory and retrying every 30 s.",
                    day_path(&self.dir, date).display()
                ));
            }
        }
    }

    /// Splits every v0.1 month file into day files. Runs before anything else reads or writes a
    /// day file, so it never races a flush.
    fn upgrade(&mut self, stamp: &str) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let months: BTreeSet<Month> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                let stem = name.to_str()?.strip_suffix(".json")?;
                Month::parse(stem.strip_suffix(SPLIT_SUFFIX).unwrap_or(stem))
            })
            .collect();
        for month in months {
            self.split(month, stamp);
        }
    }

    /// A month split on a retry may hold days that are already in `hours` as unsaved counts;
    /// its files are read on top of them, like any day read late.
    fn split(&mut self, month: Month, stamp: &str) {
        let mut moved = Vec::new();
        let result = split_month(&self.dir, month, stamp, &mut moved);
        for p in moved {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            self.notice = Some(format!("A file was unreadable and was moved to {name}"));
        }
        match result {
            Ok(()) => {
                if self.unsplit.contains(&month) {
                    let days = match list_days(&self.dir) {
                        Ok(days) => days,
                        Err(err) => {
                            self.error = Some(format!(
                                "Can't open {}: {err}. Counting in memory and retrying every 30 s.",
                                self.dir.display()
                            ));
                            return;
                        }
                    };
                    self.unsplit.remove(&month);
                    for date in days {
                        if date.month() == month && !self.read.contains(&date) {
                            self.load_day(date, stamp);
                        }
                    }
                    if self.clear() {
                        self.error = None;
                    }
                }
            }
            Err(err) => {
                self.unsplit.insert(month);
                self.error = Some(format!(
                    "Can't upgrade {month}.json to day files: {err}. Counting in memory and \
                     retrying every 30 s."
                ));
            }
        }
    }
}

fn day_path(dir: &Path, date: Date) -> PathBuf {
    dir.join(format!("{:04}", date.year)).join(day_name(date))
}

fn day_name(date: Date) -> String {
    format!("{:02}-{:02}.json", date.month, date.day)
}

fn day_hours(hours: &Hours, date: Date) -> Hours {
    let from = Hour::start_of(date);
    let to = Hour::start_of(date.plus_days(1));
    hours.range(from..to).map(|(h, b)| (*h, *b)).collect()
}

fn dates(hours: &Hours) -> BTreeSet<Date> {
    hours.keys().map(|h| h.date).collect()
}

pub struct Skip {
    days: BTreeSet<Date>,
    months: BTreeSet<Month>,
}

pub fn read_history(dir: &Path, skip: &Skip, stamp: &str) -> HistoryRead {
    HistoryRead(list_days(dir).map(|days| {
        days.into_iter()
            .filter(|d| !skip.days.contains(d) && !skip.months.contains(&d.month()))
            .map(|d| (d, read_day(dir, d, stamp)))
            .collect()
    }))
}

fn list_days(dir: &Path) -> io::Result<Vec<Date>> {
    let mut days = Vec::new();
    let years = match std::fs::read_dir(dir) {
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(days),
        years => years?,
    };
    for year in years {
        let year = year?;
        let name = year.file_name();
        let Some(y) = name
            .to_str()
            .filter(|y| y.len() == 4 && year.path().is_dir())
        else {
            continue;
        };
        for entry in std::fs::read_dir(year.path())? {
            let name = entry?.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if let Some(hour) = Hour::parse(&format!("{y}-{stem}T00")) {
                days.push(hour.date);
            }
        }
    }
    Ok(days)
}

/// v0.1 month file to day files: plan the merged days into a journal, write and re-read each
/// day file, then keep the month file as a backup. A crash before the journal is complete has
/// touched no day file, so the split starts over; after it, the journal holds the final values
/// and is written again as is, so a resumed split never adds a month twice. Day files that
/// already match the plan are left alone, so a split retried every flush writes nothing new.
///
/// A damaged journal is moved aside and the month is planned again. Some day files may already
/// hold the old plan, and a day that holds at least the month's hours is taken as written: while
/// the journal existed the month was held, so nothing else added to those days.
fn split_month(dir: &Path, month: Month, stamp: &str, moved: &mut Vec<PathBuf>) -> io::Result<()> {
    let shard = dir.join(format!("{month}.json"));
    let journal = dir.join(format!("{month}{SPLIT_SUFFIX}.json"));
    if !shard.exists() {
        return remove_if_exists(&journal);
    }
    let resumed = load_shard(&journal, month, stamp)?;
    let damaged = resumed.moved_to.is_some();
    moved.extend(resumed.moved_to);
    let plan = match resumed.value {
        Some(plan) => plan,
        None => {
            let loaded = load_shard(&shard, month, stamp)?;
            if let Some(p) = loaded.moved_to {
                moved.push(p);
                return Ok(());
            }
            let from_month = loaded.value.unwrap_or_default();
            let mut plan = Hours::new();
            for date in dates(&from_month) {
                let existing = read_day(dir, date, stamp)?;
                moved.extend(existing.moved_to);
                let existing = existing.value.unwrap_or_default();
                let written = damaged && holds(&existing, &day_hours(&from_month, date));
                for (hour, bucket) in day_hours(&from_month, date) {
                    if !written {
                        *plan.entry(hour).or_default() += bucket;
                    }
                }
                for (hour, bucket) in existing {
                    *plan.entry(hour).or_default() += bucket;
                }
            }
            save_hours(&journal, &plan)?;
            plan
        }
    };
    for date in dates(&plan) {
        let day: Hours = day_hours(&plan, date)
            .into_iter()
            .filter(|(_, b)| !b.is_empty())
            .collect();
        let current = read_day(dir, date, stamp)?;
        moved.extend(current.moved_to);
        if current.value.as_ref() == Some(&day) {
            continue;
        }
        save_day(dir, date, &day)?;
        let back = read_day(dir, date, stamp)?;
        moved.extend(back.moved_to);
        if back.value.unwrap_or_default() != day {
            return Err(io::Error::other(format!(
                "{} did not read back as written",
                day_path(dir, date).display()
            )));
        }
    }
    std::fs::rename(&shard, free_backup(dir, month, stamp))?;
    remove_if_exists(&journal)
}

fn holds(file: &Hours, from_month: &Hours) -> bool {
    from_month.iter().all(|(hour, want)| {
        file.get(hour).is_some_and(|have| {
            have.to_array()
                .iter()
                .zip(want.to_array())
                .all(|(have, want)| *have >= want)
        })
    })
}

/// An older backup (a downgrade to v0.1 and back) is never replaced.
fn free_backup(dir: &Path, month: Month, stamp: &str) -> PathBuf {
    let base = format!("{month}.json{BACKUP_SUFFIX}");
    std::iter::once(base.clone())
        .chain((1..).map(|n| format!("{base}-{stamp}-{n}")))
        .map(|name| dir.join(name))
        .find(|path| !path.exists())
        .expect("an unused name")
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

static STORAGE: OnceLock<Mutex<Storage>> = OnceLock::new();

pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tippy-taps")
}

/// Reads today's file only; every other day loads on a background thread and is merged in
/// when it is done, so counting never waits on history.
pub fn init(dir: &Path) {
    let now = LocalTime::now();
    let storage = Storage::open_today(dir.join(DIR_NAME), now.date, &now.stamp());
    let (dir, skip) = (storage.dir.clone(), storage.skip());
    let _ = STORAGE.set(Mutex::new(storage));
    std::thread::Builder::new()
        .name("history".into())
        .spawn(move || {
            let history = read_history(&dir, &skip, &LocalTime::now().stamp());
            get().merge_history(history);
        })
        .expect("spawn history thread");
}

pub fn get() -> MutexGuard<'static, Storage> {
    STORAGE
        .get()
        .expect("storage::init runs first")
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn flush(hour: Hour) {
    let Some(storage) = STORAGE.get() else { return };
    let mut storage = storage.lock().unwrap_or_else(|p| p.into_inner());
    storage.flush(hour, counts::take(), &LocalTime::now().stamp());
    keyboard::flush();
    mouse::flush();
}

/// Flushes on its own thread, not from egui: a hidden window may stop updating.
pub fn spawn_flusher() {
    std::thread::Builder::new()
        .name("flush".into())
        .spawn(|| {
            loop {
                let started = LocalTime::now();
                std::thread::sleep(clock::flush_wait(started));
                flush(clock::credit_hour(started.hour(), LocalTime::now()));
            }
        })
        .expect("spawn flush thread");
}

pub struct Loaded<T> {
    pub value: Option<T>,
    pub moved_to: Option<PathBuf>,
}

/// A missing file loads as `None`. A file that does not parse, or that `accept` rejects, is
/// renamed to `<stem>.corrupt-<stamp>.json` and never overwritten.
pub fn read_json<T: DeserializeOwned, U>(
    path: &Path,
    stamp: &str,
    accept: impl FnOnce(T) -> Option<U>,
) -> io::Result<Loaded<U>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Ok(Loaded {
                value: None,
                moved_to: None,
            });
        }
        Err(err) => return Err(err),
    };
    match serde_json::from_slice(&bytes).ok().and_then(accept) {
        Some(value) => Ok(Loaded {
            value: Some(value),
            moved_to: None,
        }),
        None => {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let moved_to = path.with_file_name(format!("{stem}.corrupt-{stamp}.json"));
            std::fs::rename(path, &moved_to)?;
            Ok(Loaded {
                value: None,
                moved_to: Some(moved_to),
            })
        }
    }
}

/// Writes to a temp file first, then renames it over the old one, so a crash mid-write
/// leaves the previous file intact.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    write_atomic(path, &serde_json::to_vec(value)?)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

fn read_hours(
    path: &Path,
    stamp: &str,
    belongs: impl Fn(Hour) -> bool,
) -> io::Result<Loaded<Hours>> {
    read_json(path, stamp, |shard: Shard| accept_hours(shard, belongs))
}

fn accept_hours(shard: Shard, belongs: impl Fn(Hour) -> bool) -> Option<Hours> {
    if shard.version != VERSION {
        return None;
    }
    shard
        .hours
        .into_iter()
        .map(|(key, bucket)| {
            Hour::parse(&key)
                .filter(|h| belongs(*h))
                .map(|h| (h, bucket))
        })
        .collect()
}

fn load_shard(path: &Path, month: Month, stamp: &str) -> io::Result<Loaded<Hours>> {
    read_hours(path, stamp, |h| h.date.month() == month)
}

/// A day only counts as missing when its folders could hold it: a file in the place of one
/// reads as not found too, and writing that day later would drop what the real file holds.
fn read_day(dir: &Path, date: Date, stamp: &str) -> io::Result<Loaded<Hours>> {
    let path = day_path(dir, date);
    let loaded = read_hours(&path, stamp, |h| h.date == date)?;
    if loaded.value.is_none() && loaded.moved_to.is_none() {
        for folder in path.ancestors().skip(1).take(2) {
            match std::fs::metadata(folder) {
                Ok(m) if !m.is_dir() => {
                    return Err(io::Error::other(format!(
                        "{} is not a folder",
                        folder.display()
                    )));
                }
                Err(err) if err.kind() != ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
    }
    Ok(loaded)
}

fn save_hours(path: &Path, hours: &Hours) -> io::Result<()> {
    let shard = Shard {
        version: VERSION,
        hours: hours
            .iter()
            .filter(|(_, b)| !b.is_empty())
            .map(|(h, b)| (h.to_string(), *b))
            .collect(),
    };
    write_json(path, &shard)
}

fn save_day(dir: &Path, date: Date, hours: &Hours) -> io::Result<()> {
    let path = day_path(dir, date);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    save_hours(&path, hours)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tippy-taps-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn day(&self, month: u32, day: u32) -> PathBuf {
            day_path(&self.0, date(2026, month, day))
        }

        fn write_day(&self, month: u32, day: u32, body: &str) {
            let path = self.day(month, day);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn date(year: i32, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    fn hour(year: i32, month: u32, day: u32, h: u32) -> Hour {
        Hour {
            date: date(year, month, day),
            hour: h,
        }
    }

    fn keys(keys: u64, clicks: u64) -> Bucket {
        Bucket {
            keys,
            clicks,
            ..Default::default()
        }
    }

    const TODAY: Date = Date {
        year: 2026,
        month: 11,
        day: 20,
    };

    fn open(dir: &TempDir) -> Storage {
        Storage::open(dir.0.clone(), TODAY, "x")
    }

    fn saved(path: &Path) -> Hours {
        read_hours(path, "x", |_| true).unwrap().value.unwrap()
    }

    fn files(dir: &Path) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                found.extend(
                    files(&entry.path())
                        .into_iter()
                        .map(|f| format!("{name}/{f}")),
                );
            } else {
                found.insert(name);
            }
        }
        found
    }

    #[test]
    fn missing_dir_starts_empty() {
        let dir = TempDir::new("missing");
        let storage = Storage::open(dir.0.join(DIR_NAME), TODAY, "x");
        assert!(storage.hours.is_empty());
        assert!(!storage.saving_blocked());
        assert!(storage.complete());
        assert!(storage.error.is_none(), "a first run shows no error");
    }

    #[test]
    fn day_files_round_trip_compact() {
        let dir = TempDir::new("roundtrip");
        let mut storage = open(&dir);
        assert!(storage.complete());
        storage.flush(hour(2026, 11, 20, 9), keys(5, 2), "x");
        assert!(!dir.0.join("2026/11-20.json.tmp").exists());

        let text = std::fs::read_to_string(dir.day(11, 20)).unwrap();
        assert_eq!(
            text,
            "{\"version\":2,\"hours\":{\"2026-11-20T09\":{\"keys\":5,\"clicks\":2}}}\n"
        );

        let reloaded = open(&dir);
        assert_eq!(reloaded.hours, storage.hours);
        assert_eq!(reloaded.total, keys(5, 2));
        assert_eq!(reloaded.day(TODAY), keys(5, 2));
    }

    #[test]
    fn midnight_rollover_writes_both_days() {
        let dir = TempDir::new("midnight");
        let mut storage = open(&dir);
        storage.flush(hour(2026, 11, 20, 23), keys(5, 2), "x");
        storage.flush(hour(2026, 11, 21, 0), keys(1, 0), "x");
        assert_eq!(saved(&dir.day(11, 20))[&hour(2026, 11, 20, 23)], keys(5, 2));
        assert_eq!(saved(&dir.day(11, 21))[&hour(2026, 11, 21, 0)], keys(1, 0));
        assert_eq!(saved(&dir.day(11, 21)).len(), 1);

        let reloaded = Storage::open(dir.0.clone(), date(2026, 11, 21), "x");
        assert_eq!(reloaded.total, keys(6, 2));
    }

    #[test]
    fn idle_flush_writes_nothing() {
        let dir = TempDir::new("idle");
        let mut storage = open(&dir);
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "x");
        assert!(files(&dir.0).is_empty());
    }

    #[test]
    fn flush_rewrites_only_dirty_days() {
        let dir = TempDir::new("touched");
        let mut storage = open(&dir);
        storage.flush(hour(2026, 11, 19, 4), keys(1, 0), "x");
        std::fs::write(dir.day(11, 19), "sentinel").unwrap();
        storage.flush(hour(2026, 11, 20, 4), keys(1, 0), "x");
        storage.flush(hour(2026, 11, 20, 5), keys(1, 0), "x");
        assert_eq!(
            std::fs::read_to_string(dir.day(11, 19)).unwrap(),
            "sentinel"
        );
        assert_eq!(saved(&dir.day(11, 20)).len(), 2);
    }

    #[test]
    fn existing_hours_merge() {
        let dir = TempDir::new("merge");
        dir.write_day(
            11,
            19,
            r#"{"version":2,"hours":{"2026-11-19T10":{"keys":10,"clicks":3}}}"#,
        );
        dir.write_day(
            11,
            20,
            r#"{"version":2,"hours":{"2026-11-20T08":{"keys":1,"buttons":2}}}"#,
        );
        let mut storage = open(&dir);
        storage.flush(hour(2026, 11, 20, 8), keys(4, 0), "x");
        storage.flush(hour(2026, 11, 21, 0), keys(1, 0), "x");

        assert_eq!(
            saved(&dir.day(11, 19))[&hour(2026, 11, 19, 10)],
            keys(10, 3)
        );
        assert_eq!(
            saved(&dir.day(11, 20))[&hour(2026, 11, 20, 8)],
            Bucket {
                keys: 5,
                buttons: 2,
                ..Default::default()
            }
        );
        assert_eq!(saved(&dir.day(11, 21))[&hour(2026, 11, 21, 0)], keys(1, 0));
        assert_eq!(storage.total.keys, 16);
    }

    #[test]
    fn per_button_fields_merge_and_round_trip() {
        let dir = TempDir::new("merge-buttons");
        dir.write_day(
            11,
            20,
            r#"{ "version": 2, "hours": {
                 "2026-11-20T08": { "clicks": 3, "m_left": 2, "m_x2": 1,
                                    "buttons": 2, "pad_a": 1, "pad_rs": 1 } } }"#,
        );
        let mut storage = open(&dir);
        let taken = Bucket {
            clicks: 2,
            m_left: 1,
            m_middle: 1,
            buttons: 1,
            pad_a: 1,
            ..Default::default()
        };
        storage.flush(hour(2026, 11, 20, 8), taken, "x");

        let text = std::fs::read_to_string(dir.day(11, 20)).unwrap();
        assert!(!text.contains("m_right"), "zero fields are omitted");
        let expected = Bucket {
            clicks: 5,
            m_left: 3,
            m_middle: 1,
            m_x2: 1,
            buttons: 3,
            pad_a: 2,
            pad_rs: 1,
            ..Default::default()
        };
        assert_eq!(saved(&dir.day(11, 20))[&hour(2026, 11, 20, 8)], expected);
        assert_eq!(storage.total, expected);
        assert_eq!(open(&dir).total, expected);
    }

    #[test]
    fn corrupt_day_file_is_moved_aside_not_overwritten() {
        let dir = TempDir::new("corrupt");
        dir.write_day(11, 3, "{ not json");

        let storage = Storage::open(dir.0.clone(), TODAY, "20261120-101500");
        let moved = dir.0.join("2026/11-03.corrupt-20261120-101500.json");
        assert_eq!(std::fs::read(&moved).unwrap(), b"{ not json");
        assert!(storage.notice.as_deref().unwrap().contains("corrupt"));
        assert!(storage.hours.is_empty());
        assert!(!storage.saving_blocked());
        assert!(storage.complete());
        assert!(!dir.day(11, 3).exists(), "nothing to write back");
    }

    #[test]
    fn wrong_version_or_foreign_hours_count_as_corrupt() {
        let dir = TempDir::new("version");
        let day = date(2026, 11, 20);
        dir.write_day(11, 20, r#"{ "2026-11-20": { "keys": 7 } }"#);
        assert!(read_day(&dir.0, day, "a").unwrap().moved_to.is_some());

        dir.write_day(
            11,
            20,
            r#"{ "version": 2, "hours": { "2026-11-21T00": { "keys": 1 } } }"#,
        );
        assert!(read_day(&dir.0, day, "b").unwrap().moved_to.is_some());

        dir.write_day(11, 20, "");
        assert!(read_day(&dir.0, day, "c").unwrap().moved_to.is_some());
    }

    #[test]
    fn unreadable_day_keeps_counting_then_merges_on_retry() {
        let dir = TempDir::new("retry");
        let path = dir.day(11, 20);
        // A directory in the file's place makes every read fail, like a file locked by another process.
        std::fs::create_dir_all(&path).unwrap();

        let mut storage = open(&dir);
        assert!(storage.saving_blocked());
        assert!(!storage.complete());
        assert!(storage.error.is_some());

        storage.flush(hour(2026, 11, 20, 9), keys(3, 1), "s");
        storage.flush(hour(2026, 11, 20, 9), keys(2, 0), "s");
        storage.flush(hour(2026, 11, 19, 0), keys(1, 0), "s");
        assert!(path.is_dir(), "the unreadable path is never written over");
        assert!(dir.day(11, 19).exists(), "other days still save");
        assert_eq!(storage.day(TODAY), keys(5, 1));

        std::fs::remove_dir(&path).unwrap();
        dir.write_day(
            11,
            20,
            r#"{ "version": 2, "hours": { "2026-11-20T09": { "keys": 10 } } }"#,
        );
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "s");

        assert!(!storage.saving_blocked());
        assert!(storage.complete());
        assert!(storage.error.is_none());
        assert_eq!(saved(&path)[&hour(2026, 11, 20, 9)], keys(15, 1));
        assert_eq!(storage.total, keys(16, 1));
    }

    #[test]
    fn blocked_past_day_is_retried_without_new_counts() {
        let dir = TempDir::new("retry-past");
        let past = dir.day(10, 5);
        std::fs::create_dir_all(&past).unwrap();
        let mut storage = open(&dir);
        assert!(storage.saving_blocked());

        std::fs::remove_dir(&past).unwrap();
        dir.write_day(
            10,
            5,
            r#"{ "version": 2, "hours": { "2026-10-05T09": { "keys": 4 } } }"#,
        );
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "s");
        assert!(!storage.saving_blocked());
        assert!(storage.complete());
        assert!(storage.error.is_none());
        assert_eq!(storage.total, keys(4, 0));
    }

    #[test]
    fn unlisted_dir_retries_before_any_write() {
        let dir = TempDir::new("unlisted");
        let counts = dir.0.join(DIR_NAME);
        std::fs::write(&counts, b"a file where the dir should be").unwrap();
        let mut storage = Storage::open(counts.clone(), TODAY, "s");
        assert!(!storage.complete());
        storage.flush(hour(2026, 11, 20, 9), keys(2, 0), "s");
        assert!(counts.is_file());
        assert!(storage.error.is_some());

        std::fs::remove_file(&counts).unwrap();
        let today = day_path(&counts, TODAY);
        std::fs::create_dir_all(today.parent().unwrap()).unwrap();
        std::fs::write(
            &today,
            r#"{ "version": 2, "hours": { "2026-11-20T09": { "keys": 3 } } }"#,
        )
        .unwrap();
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "s");
        assert!(storage.complete());
        assert_eq!(saved(&today)[&hour(2026, 11, 20, 9)], keys(5, 0));
    }

    fn past_days(dir: &TempDir, n: u64) {
        for back in 1..=n {
            let d = TODAY.minus_days(back as i64);
            let path = day_path(&dir.0, d);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(r#"{{"version":2,"hours":{{"{d}T10":{{"keys":{back},"clicks":1}}}}}}"#),
            )
            .unwrap();
        }
    }

    #[test]
    fn background_load_equals_synchronous_load() {
        let dir = TempDir::new("background");
        past_days(&dir, 400);
        dir.write_day(
            11,
            20,
            r#"{"version":2,"hours":{"2026-11-20T08":{"keys":7}}}"#,
        );
        let sync = open(&dir);

        let mut storage = Storage::open_today(dir.0.clone(), TODAY, "x");
        assert_eq!(storage.total, keys(7, 0), "today is there before history");
        assert!(!storage.complete(), "milestones wait for history");
        let (path, skip) = (storage.dir.clone(), storage.skip());
        let history = std::thread::spawn(move || read_history(&path, &skip, "x"))
            .join()
            .unwrap();
        storage.merge_history(history);
        assert!(storage.complete());
        assert_eq!(storage.hours, sync.hours);
        assert_eq!(storage.total, sync.total);
        assert_eq!(storage.total.keys, 7 + (1..=400).sum::<u64>());
    }

    #[test]
    fn days_flushed_before_history_arrives_are_not_counted_twice() {
        let dir = TempDir::new("race");
        dir.write_day(
            11,
            21,
            r#"{"version":2,"hours":{"2026-11-21T01":{"keys":2}}}"#,
        );
        let mut storage = Storage::open_today(dir.0.clone(), TODAY, "x");
        let history = read_history(&storage.dir, &storage.skip(), "x");
        storage.flush(hour(2026, 11, 20, 23), keys(1, 0), "x");
        storage.flush(hour(2026, 11, 21, 0), keys(1, 0), "x");
        storage.merge_history(history);
        assert_eq!(storage.total, keys(4, 0));
        assert_eq!(open(&dir).total, keys(4, 0));
    }

    const OCT_SHARD: &str = r#"{ "version": 2, "hours": {
        "2026-10-30T09": { "keys": 4, "clicks": 1, "m_left": 1 },
        "2026-10-31T23": { "keys": 6 } } }"#;
    const NOV_SHARD: &str = r#"{ "version": 2, "hours": {
        "2026-11-10T08": { "keys": 3 },
        "2026-11-15T08": { "keys": 5 },
        "2026-11-20T08": { "keys": 2 } } }"#;

    #[test]
    fn month_shards_split_into_day_files() {
        let dir = TempDir::new("split");
        std::fs::write(dir.0.join("2026-10.json"), OCT_SHARD).unwrap();
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        dir.write_day(
            11,
            10,
            r#"{"version":2,"hours":{"2026-11-10T08":{"keys":7},"2026-11-10T09":{"keys":1}}}"#,
        );

        let mut storage = open(&dir);
        assert!(storage.complete());
        assert!(storage.error.is_none());
        assert_eq!(storage.total.keys, 4 + 6 + 3 + 5 + 2 + 7 + 1);
        assert_eq!(storage.day(TODAY), keys(2, 0));
        assert_eq!(
            files(&dir.0),
            [
                "2026-10.json.v1-backup",
                "2026-11.json.v1-backup",
                "2026/10-30.json",
                "2026/10-31.json",
                "2026/11-10.json",
                "2026/11-15.json",
                "2026/11-20.json",
            ]
            .map(String::from)
            .into()
        );
        assert_eq!(saved(&dir.day(11, 10))[&hour(2026, 11, 10, 8)], keys(10, 0));
        assert_eq!(
            std::fs::read_to_string(dir.0.join("2026-11.json.v1-backup")).unwrap(),
            NOV_SHARD,
            "the backup is the untouched v0.1 file"
        );

        storage.flush(hour(2026, 11, 20, 8), keys(1, 0), "x");
        let again = open(&dir);
        assert_eq!(again.total, storage.total, "a second start splits nothing");
        assert_eq!(again.day(TODAY), keys(3, 0));
    }

    #[test]
    fn crashed_split_resumes_without_double_counting() {
        let dir = TempDir::new("split-crash");
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        dir.write_day(
            11,
            10,
            r#"{"version":2,"hours":{"2026-11-10T09":{"keys":1}}}"#,
        );
        // A directory where the temp file goes makes the second day's write fail mid-split.
        let blocker = dir.0.join("2026/11-15.json.tmp");
        std::fs::create_dir_all(&blocker).unwrap();

        let crashed = open(&dir);
        assert!(
            !crashed.complete(),
            "milestones wait while a month is missing"
        );
        assert!(crashed.error.as_deref().unwrap().contains("2026-11.json"));
        assert!(dir.0.join("2026-11.json").exists());
        assert!(dir.0.join("2026-11.split.json").exists());
        assert_eq!(
            saved(&dir.day(11, 10)).len(),
            2,
            "the first day was written"
        );

        std::fs::remove_dir(&blocker).unwrap();
        let resumed = open(&dir);
        assert!(resumed.complete());
        assert_eq!(resumed.total.keys, 3 + 5 + 2 + 1);
        assert_eq!(saved(&dir.day(11, 10))[&hour(2026, 11, 10, 8)], keys(3, 0));
        assert!(!dir.0.join("2026-11.json").exists());
        assert!(!dir.0.join("2026-11.split.json").exists());
        assert_eq!(open(&dir).total, resumed.total);
    }

    #[test]
    fn damaged_journal_is_moved_aside_and_the_split_recovers_once() {
        let dir = TempDir::new("split-bad-journal");
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        dir.write_day(
            11,
            10,
            r#"{"version":2,"hours":{"2026-11-10T08":{"keys":3},"2026-11-10T09":{"keys":1}}}"#,
        );
        dir.write_day(
            11,
            15,
            r#"{"version":2,"hours":{"2026-11-15T09":{"keys":4}}}"#,
        );
        std::fs::write(dir.0.join("2026-11.split.json"), "{ torn").unwrap();

        let mut storage = open(&dir);
        assert!(storage.complete());
        assert!(storage.error.is_none());
        assert!(
            storage
                .notice
                .as_deref()
                .unwrap()
                .contains("2026-11.split.corrupt-x.json")
        );
        assert!(dir.0.join("2026-11.split.corrupt-x.json").exists());
        assert_eq!(
            saved(&dir.day(11, 10))[&hour(2026, 11, 10, 8)],
            keys(3, 0),
            "a day that already holds the month's hours is not added again"
        );
        assert_eq!(saved(&dir.day(11, 15))[&hour(2026, 11, 15, 8)], keys(5, 0));
        assert_eq!(storage.total.keys, 3 + 1 + 5 + 4 + 2);

        storage.flush(hour(2026, 11, 20, 9), keys(10, 0), "x");
        assert_eq!(open(&dir).total, storage.total);
    }

    #[test]
    fn corrupt_day_during_the_split_is_reported() {
        let dir = TempDir::new("split-corrupt-day");
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        dir.write_day(11, 15, "{ torn");
        let storage = open(&dir);
        assert!(
            storage
                .notice
                .as_deref()
                .unwrap()
                .contains("11-15.corrupt-x.json")
        );
        assert_eq!(storage.total.keys, 3 + 5 + 2);
    }

    #[test]
    fn an_older_backup_is_never_replaced() {
        let dir = TempDir::new("split-backup");
        std::fs::write(dir.0.join("2026-11.json.v1-backup"), "older").unwrap();
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        open(&dir);
        assert_eq!(
            std::fs::read_to_string(dir.0.join("2026-11.json.v1-backup")).unwrap(),
            "older"
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join("2026-11.json.v1-backup-x-1")).unwrap(),
            NOV_SHARD
        );
    }

    #[test]
    fn retried_split_rewrites_no_finished_day() {
        let dir = TempDir::new("split-retry-writes");
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        let blocker = dir.0.join("2026/11-15.json.tmp");
        std::fs::create_dir_all(&blocker).unwrap();
        let mut storage = open(&dir);
        let first = std::fs::metadata(dir.day(11, 10))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "x");
        let again = std::fs::metadata(dir.day(11, 10))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(first, again);
    }

    #[test]
    fn a_save_error_outlives_the_history_merge() {
        let dir = TempDir::new("keep-error");
        let mut storage = Storage::open_today(dir.0.clone(), TODAY, "x");
        storage.error = Some("Can't save counts: disk full".into());
        let history = read_history(&storage.dir, &storage.skip(), "x");
        storage.merge_history(history);
        assert_eq!(
            storage.error.as_deref(),
            Some("Can't save counts: disk full")
        );
    }

    #[test]
    fn journal_left_after_the_backup_rename_is_dropped() {
        let dir = TempDir::new("split-orphan");
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        let before = open(&dir).total;
        std::fs::write(dir.0.join("2026-11.split.json"), NOV_SHARD).unwrap();
        assert_eq!(open(&dir).total, before);
        assert!(!dir.0.join("2026-11.split.json").exists());
    }

    #[test]
    fn locked_month_shard_holds_its_days_and_retries() {
        let dir = TempDir::new("split-locked");
        let shard = dir.0.join("2026-11.json");
        std::fs::create_dir(&shard).unwrap();
        let mut storage = open(&dir);
        assert!(!storage.complete());
        assert!(storage.saving_blocked());
        storage.flush(hour(2026, 11, 20, 9), keys(2, 0), "x");
        storage.flush(hour(2026, 10, 31, 9), keys(1, 0), "x");
        assert!(!dir.day(11, 20).exists(), "no day of the month is written");
        assert!(dir.day(10, 31).exists(), "other months still save");
        assert!(storage.error.is_some());

        std::fs::remove_dir(&shard).unwrap();
        std::fs::write(&shard, NOV_SHARD).unwrap();
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "x");
        assert!(storage.complete());
        assert!(storage.error.is_none());
        assert_eq!(storage.total.keys, 3 + 5 + 2 + 2 + 1);
        assert_eq!(saved(&dir.day(11, 20))[&hour(2026, 11, 20, 8)], keys(2, 0));
        assert_eq!(saved(&dir.day(11, 20))[&hour(2026, 11, 20, 9)], keys(2, 0));
        assert_eq!(open(&dir).total, storage.total);
    }

    #[test]
    fn counts_during_a_failed_split_survive_the_resume() {
        let dir = TempDir::new("split-session");
        std::fs::write(dir.0.join("2026-11.json"), NOV_SHARD).unwrap();
        let blocker = dir.0.join("2026/11-15.json.tmp");
        std::fs::create_dir_all(&blocker).unwrap();
        let mut storage = open(&dir);
        storage.flush(hour(2026, 11, 20, 9), keys(100, 0), "x");
        assert_eq!(storage.total.keys, 100);
        drop(storage);

        std::fs::remove_dir(&blocker).unwrap();
        let mut storage = open(&dir);
        assert_eq!(
            storage.total.keys,
            3 + 5 + 2,
            "the unsaved session was lost, not doubled"
        );
        storage.flush(hour(2026, 11, 20, 9), keys(100, 0), "x");
        assert_eq!(open(&dir).total.keys, 3 + 5 + 2 + 100);

        std::fs::write(
            dir.0.join("2026-12.json"),
            NOV_SHARD.replace("-11-", "-12-"),
        )
        .unwrap();
        std::fs::create_dir_all(dir.0.join("2026/12-15.json.tmp")).unwrap();
        let mut storage = Storage::open(dir.0.clone(), date(2026, 12, 20), "x");
        storage.flush(hour(2026, 12, 20, 9), keys(50, 0), "x");
        std::fs::remove_dir(dir.0.join("2026/12-15.json.tmp")).unwrap();
        storage.flush(hour(2026, 12, 20, 9), Bucket::default(), "x");
        let reopened = Storage::open(dir.0.clone(), date(2026, 12, 20), "x");
        assert_eq!(reopened.total.keys, 2 * (3 + 5 + 2) + 100 + 50);
        assert_eq!(reopened.total, storage.total);
    }

    #[test]
    fn corrupt_month_shard_is_moved_aside() {
        let dir = TempDir::new("split-corrupt");
        std::fs::write(dir.0.join("2026-11.json"), "{ nope").unwrap();
        let storage = open(&dir);
        assert!(storage.complete());
        assert!(storage.notice.is_some());
        assert!(dir.0.join("2026-11.corrupt-x.json").exists());
    }

    #[test]
    fn v0_1_shard_loads_with_movement_zero() {
        let dir = TempDir::new("v01");
        std::fs::write(dir.0.join("2026-10.json"), OCT_SHARD).unwrap();
        let mut storage = open(&dir);
        assert!(storage.notice.is_none());
        let b = storage.total;
        assert_eq!((b.keys, b.m_left, b.m_px, b.m_um), (10, 1, 0, 0));
        assert_eq!(b.directions(), [0; 8]);

        let moved = Bucket {
            m_px: 30,
            m_um: 7_900,
            m_dir_e: 30,
            ..Default::default()
        };
        storage.flush(hour(2026, 11, 20, 8), moved, "x");
        let text = std::fs::read_to_string(dir.day(11, 20)).unwrap();
        assert!(!text.contains("m_dir_w"), "zero fields are omitted");
        let saved = open(&dir).total;
        assert_eq!(
            (saved.keys, saved.m_px, saved.m_um, saved.m_dir_e),
            (10, 30, 7_900, 30)
        );
    }
}

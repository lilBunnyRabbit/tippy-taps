use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::clock::{self, Date, Hour, LocalTime, Month};
use crate::counts::{self, Bucket};

const DIR_NAME: &str = "counts";
const VERSION: u32 = 2;

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

/// One file per month, so a flush rewrites only the months it touched.
pub struct Storage {
    pub dir: PathBuf,
    pub hours: Hours,
    pub total: Bucket,
    pub error: Option<String>,
    /// Lasts the whole session, unlike `error`, which the next good save clears.
    pub notice: Option<String>,
    dirty: BTreeSet<Month>,
    /// Shards that exist but could not be read. They are never written over: their counts pile
    /// up in memory and every flush retries the read.
    blocked: BTreeSet<Month>,
    /// Until the directory has been listed, any month could have a shard we have not seen.
    listed: bool,
}

impl Storage {
    pub fn open(dir: PathBuf, stamp: &str) -> Self {
        let mut storage = Self {
            dir,
            hours: Hours::new(),
            total: Bucket::default(),
            error: None,
            notice: None,
            dirty: BTreeSet::new(),
            blocked: BTreeSet::new(),
            listed: false,
        };
        storage.load_all(stamp);
        storage
    }

    pub fn saving_blocked(&self) -> bool {
        !self.listed || !self.blocked.is_empty()
    }

    pub fn day(&self, date: Date) -> Bucket {
        sum(
            &self.hours,
            Hour::start_of(date),
            Hour::start_of(date.plus_days(1)),
        )
    }

    pub fn flush(&mut self, hour: Hour, taken: Bucket, stamp: &str) {
        if !taken.is_empty() {
            *self.hours.entry(hour).or_default() += taken;
            self.total += taken;
            self.dirty.insert(hour.date.month());
            REVISION.fetch_add(1, Ordering::Relaxed);
        }
        if self.dirty.is_empty() {
            return;
        }
        if !self.listed {
            self.load_all(stamp);
        }
        let retry: Vec<Month> = self.blocked.intersection(&self.dirty).copied().collect();
        for month in retry {
            self.load_shard(month, stamp);
        }
        if !self.listed {
            return;
        }
        let mut failed = None;
        for month in self.dirty.clone() {
            if self.blocked.contains(&month) {
                continue;
            }
            match save_shard(&self.shard_path(month), &self.month_hours(month)) {
                Ok(()) => {
                    self.dirty.remove(&month);
                }
                Err(err) => failed = Some(format!("Can't save counts: {err}")),
            }
        }
        if failed.is_some() || self.blocked.is_empty() {
            self.error = failed;
        }
    }

    fn shard_path(&self, month: Month) -> PathBuf {
        self.dir.join(format!("{month}.json"))
    }

    fn month_hours(&self, month: Month) -> Hours {
        let from = Hour::start_of(month.first());
        let to = Hour::start_of(month.next().first());
        self.hours.range(from..to).map(|(h, b)| (*h, *b)).collect()
    }

    fn load_all(&mut self, stamp: &str) {
        let months = std::fs::create_dir_all(&self.dir)
            .and_then(|()| std::fs::read_dir(&self.dir))
            .and_then(|entries| {
                let mut months = Vec::new();
                for entry in entries {
                    let name = entry?.file_name();
                    let name = name.to_string_lossy();
                    if let Some(month) = name.strip_suffix(".json").and_then(Month::parse) {
                        months.push(month);
                    }
                }
                Ok(months)
            });
        match months {
            Ok(months) => {
                self.listed = true;
                self.error = None;
                for month in months {
                    self.load_shard(month, stamp);
                }
            }
            Err(err) => {
                self.error = Some(format!(
                    "Can't open {}: {err}. Counting in memory and retrying every 30 s.",
                    self.dir.display()
                ));
            }
        }
    }

    /// Counts already in memory for this month were never saved (its shard was not loaded yet),
    /// so they are added on top of what the file holds.
    fn load_shard(&mut self, month: Month, stamp: &str) {
        let path = self.shard_path(month);
        match load_shard(&path, month, stamp) {
            Ok(loaded) => {
                self.blocked.remove(&month);
                for (hour, bucket) in loaded.value.unwrap_or_default() {
                    *self.hours.entry(hour).or_default() += bucket;
                    self.total += bucket;
                }
                if let Some(p) = loaded.moved_to {
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    self.notice = Some(format!(
                        "{month}.json was unreadable and was moved to {name}"
                    ));
                    self.dirty.insert(month);
                }
                if self.blocked.is_empty() {
                    self.error = None;
                }
                REVISION.fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                self.blocked.insert(month);
                self.error = Some(format!(
                    "Can't open {}: {err}. Counting in memory and retrying every 30 s.",
                    path.display()
                ));
            }
        }
    }
}

static STORAGE: OnceLock<Mutex<Storage>> = OnceLock::new();

pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tippy-taps")
}

pub fn init(dir: &Path) {
    let storage = Storage::open(dir.join(DIR_NAME), &LocalTime::now().stamp());
    let _ = STORAGE.set(Mutex::new(storage));
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
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

fn load_shard(path: &Path, month: Month, stamp: &str) -> io::Result<Loaded<Hours>> {
    read_json(path, stamp, |shard: Shard| {
        if shard.version != VERSION {
            return None;
        }
        shard
            .hours
            .into_iter()
            .map(|(key, bucket)| {
                Hour::parse(&key)
                    .filter(|h| h.date.month() == month)
                    .map(|h| (h, bucket))
            })
            .collect()
    })
}

fn save_shard(path: &Path, hours: &Hours) -> io::Result<()> {
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
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn hour(year: i32, month: u32, day: u32, h: u32) -> Hour {
        Hour {
            date: Date { year, month, day },
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

    const NOV: Month = Month {
        year: 2026,
        month: 11,
    };

    #[test]
    fn missing_dir_starts_empty() {
        let dir = TempDir::new("missing");
        let storage = Storage::open(dir.0.join(DIR_NAME), "x");
        assert!(storage.hours.is_empty());
        assert!(!storage.saving_blocked());
        assert!(dir.0.join(DIR_NAME).is_dir());
    }

    #[test]
    fn flush_writes_month_shards_and_reloads() {
        let dir = TempDir::new("roundtrip");
        let mut storage = Storage::open(dir.0.clone(), "x");
        storage.flush(hour(2026, 11, 30, 23), keys(5, 2), "x");
        storage.flush(hour(2026, 12, 1, 0), keys(1, 0), "x");
        assert!(!dir.0.join("2026-11.json.tmp").exists());

        let text = std::fs::read_to_string(dir.0.join("2026-11.json")).unwrap();
        assert!(text.contains("\"version\": 2"));
        assert!(text.contains("\"2026-11-30T23\""));
        assert!(text.contains("\"keys\": 5"));
        assert!(!text.contains("scrolls"), "zero fields are omitted");
        assert!(dir.0.join("2026-12.json").exists());

        let reloaded = Storage::open(dir.0.clone(), "x");
        assert_eq!(reloaded.hours, storage.hours);
        assert_eq!(reloaded.total, keys(6, 2));
        assert_eq!(reloaded.day(hour(2026, 11, 30, 0).date), keys(5, 2));
    }

    #[test]
    fn idle_flush_writes_nothing() {
        let dir = TempDir::new("idle");
        let mut storage = Storage::open(dir.0.clone(), "x");
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "x");
        assert!(std::fs::read_dir(&dir.0).unwrap().next().is_none());
    }

    #[test]
    fn flush_rewrites_only_the_touched_month() {
        let dir = TempDir::new("touched");
        let mut storage = Storage::open(dir.0.clone(), "x");
        storage.flush(hour(2026, 10, 3, 4), keys(1, 0), "x");
        let october = dir.0.join("2026-10.json");
        std::fs::write(&october, "sentinel").unwrap();
        storage.flush(hour(2026, 11, 3, 4), keys(1, 0), "x");
        assert_eq!(std::fs::read_to_string(&october).unwrap(), "sentinel");
    }

    #[test]
    fn existing_hours_merge() {
        let dir = TempDir::new("merge");
        std::fs::write(
            dir.0.join("2026-11.json"),
            r#"{ "version": 2, "hours": {
                 "2026-11-19T10": { "keys": 10, "clicks": 3 },
                 "2026-11-20T08": { "keys": 1, "buttons": 2 } } }"#,
        )
        .unwrap();
        let mut storage = Storage::open(dir.0.clone(), "x");
        storage.flush(hour(2026, 11, 20, 8), keys(4, 0), "x");
        storage.flush(hour(2026, 11, 21, 0), keys(1, 0), "x");

        let saved = load_shard(&dir.0.join("2026-11.json"), NOV, "x")
            .unwrap()
            .value
            .unwrap();
        assert_eq!(saved[&hour(2026, 11, 19, 10)], keys(10, 3));
        assert_eq!(
            saved[&hour(2026, 11, 20, 8)],
            Bucket {
                keys: 5,
                buttons: 2,
                ..Default::default()
            }
        );
        assert_eq!(saved[&hour(2026, 11, 21, 0)], keys(1, 0));
        assert_eq!(storage.total.keys, 16);
    }

    #[test]
    fn corrupt_shard_is_moved_aside_not_overwritten() {
        let dir = TempDir::new("corrupt");
        let path = dir.0.join("2026-11.json");
        std::fs::write(&path, b"{ not json").unwrap();

        let storage = Storage::open(dir.0.clone(), "20261120-101500");
        let moved = dir.0.join("2026-11.corrupt-20261120-101500.json");
        assert_eq!(std::fs::read(&moved).unwrap(), b"{ not json");
        assert!(storage.notice.as_deref().unwrap().contains("corrupt"));
        assert!(storage.hours.is_empty());
        assert!(!storage.saving_blocked());
    }

    #[test]
    fn wrong_version_or_foreign_hours_count_as_corrupt() {
        let dir = TempDir::new("version");
        let path = dir.0.join("2026-11.json");
        std::fs::write(&path, r#"{ "2026-11-20": { "keys": 7 } }"#).unwrap();
        assert!(load_shard(&path, NOV, "a").unwrap().moved_to.is_some());

        std::fs::write(
            &path,
            r#"{ "version": 2, "hours": { "2026-12-01T00": { "keys": 1 } } }"#,
        )
        .unwrap();
        assert!(load_shard(&path, NOV, "b").unwrap().moved_to.is_some());

        std::fs::write(&path, b"").unwrap();
        assert!(load_shard(&path, NOV, "c").unwrap().moved_to.is_some());
    }

    #[test]
    fn unreadable_shard_keeps_counting_then_merges_on_retry() {
        let dir = TempDir::new("retry");
        let path = dir.0.join("2026-11.json");
        // A directory in the file's place makes every read fail, like a file locked by another process.
        std::fs::create_dir(&path).unwrap();

        let mut storage = Storage::open(dir.0.clone(), "s");
        assert!(storage.saving_blocked());
        assert!(storage.error.is_some());

        storage.flush(hour(2026, 11, 20, 9), keys(3, 1), "s");
        storage.flush(hour(2026, 11, 20, 9), keys(2, 0), "s");
        storage.flush(hour(2026, 10, 1, 0), keys(1, 0), "s");
        assert!(path.is_dir(), "the unreadable path is never written over");
        assert!(
            dir.0.join("2026-10.json").exists(),
            "other months still save"
        );
        assert_eq!(
            sum(
                &storage.hours,
                hour(2026, 11, 20, 9),
                hour(2026, 11, 20, 10)
            ),
            keys(5, 1)
        );

        std::fs::remove_dir(&path).unwrap();
        std::fs::write(
            &path,
            r#"{ "version": 2, "hours": {
                 "2026-11-19T00": { "keys": 9 },
                 "2026-11-20T09": { "keys": 10 } } }"#,
        )
        .unwrap();
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "s");

        assert!(!storage.saving_blocked());
        assert!(storage.error.is_none());
        let saved = load_shard(&path, NOV, "s").unwrap().value.unwrap();
        assert_eq!(saved[&hour(2026, 11, 19, 0)], keys(9, 0));
        assert_eq!(saved[&hour(2026, 11, 20, 9)], keys(15, 1));
        assert_eq!(storage.total, keys(25, 1));
    }

    #[test]
    fn unlisted_dir_retries_before_any_write() {
        let dir = TempDir::new("unlisted");
        let counts = dir.0.join(DIR_NAME);
        std::fs::write(&counts, b"a file where the dir should be").unwrap();
        let mut storage = Storage::open(counts.clone(), "s");
        assert!(storage.saving_blocked());
        storage.flush(hour(2026, 11, 20, 9), keys(2, 0), "s");
        assert!(counts.is_file());

        std::fs::remove_file(&counts).unwrap();
        std::fs::create_dir(&counts).unwrap();
        std::fs::write(
            counts.join("2026-11.json"),
            r#"{ "version": 2, "hours": { "2026-11-20T09": { "keys": 3 } } }"#,
        )
        .unwrap();
        storage.flush(hour(2026, 11, 20, 9), Bucket::default(), "s");
        assert!(!storage.saving_blocked());
        let saved = load_shard(&counts.join("2026-11.json"), NOV, "s")
            .unwrap()
            .value
            .unwrap();
        assert_eq!(saved[&hour(2026, 11, 20, 9)], keys(5, 0));
    }
}

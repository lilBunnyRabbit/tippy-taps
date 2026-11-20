use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::clock::{self, Date, LocalTime};
use crate::counts::{self, Counts};

const FILE_NAME: &str = "counts.json";

/// Counts per local date, keyed `YYYY-MM-DD`.
pub type Days = BTreeMap<String, Counts>;

pub struct Storage {
    pub path: PathBuf,
    pub days: Days,
    pub error: Option<String>,
    /// Lasts the whole session, unlike `error`, which the next good save clears.
    pub notice: Option<String>,
    dirty: bool,
    writable: bool,
}

impl Storage {
    pub fn day(&self, date: Date) -> Counts {
        self.days
            .get(&date.to_string())
            .copied()
            .unwrap_or_default()
    }

    /// A file that could not be read is never overwritten: counts pile up in memory and every
    /// flush retries the load until it works.
    pub fn open(path: PathBuf, stamp: &str) -> Self {
        let mut storage = Self {
            path,
            days: Days::new(),
            error: None,
            notice: None,
            dirty: false,
            writable: false,
        };
        storage.try_load(stamp);
        storage
    }

    pub fn saving_blocked(&self) -> bool {
        !self.writable
    }

    pub fn flush(&mut self, day: Date, taken: Counts, stamp: &str) {
        if !taken.is_zero() {
            add(&mut self.days, day, taken);
            self.dirty = true;
        }
        if !self.writable {
            self.try_load(stamp);
        }
        if self.dirty && self.writable {
            match save(&self.path, &self.days) {
                Ok(()) => {
                    self.dirty = false;
                    self.error = None;
                }
                Err(err) => self.error = Some(format!("Can't save counts: {err}")),
            }
        }
    }

    fn try_load(&mut self, stamp: &str) {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        match std::fs::create_dir_all(dir).and_then(|()| load(&self.path, stamp)) {
            Ok(Loaded { mut days, moved_to }) => {
                for (date, counts) in std::mem::take(&mut self.days) {
                    *days.entry(date).or_default() += counts;
                }
                self.days = days;
                self.writable = true;
                self.error = None;
                if let Some(p) = moved_to {
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    self.notice = Some(format!(
                        "counts.json was unreadable and was moved to {name}"
                    ));
                }
            }
            Err(err) => {
                self.error = Some(format!(
                    "Can't open {}: {err}. Counting in memory and retrying every 30 s.",
                    self.path.display()
                ));
            }
        }
    }

    pub fn total(&self) -> Counts {
        self.days.values().fold(Counts::default(), |mut sum, c| {
            sum += *c;
            sum
        })
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
    let storage = Storage::open(dir.join(FILE_NAME), &LocalTime::now().stamp());
    let _ = STORAGE.set(Mutex::new(storage));
}

pub fn get() -> MutexGuard<'static, Storage> {
    STORAGE
        .get()
        .expect("storage::init runs first")
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn flush(day: Date) {
    let Some(storage) = STORAGE.get() else { return };
    let mut storage = storage.lock().unwrap_or_else(|p| p.into_inner());
    storage.flush(day, counts::take(), &LocalTime::now().stamp());
}

/// Flushes on its own thread, not from egui: a hidden window may stop updating.
pub fn spawn_flusher() {
    std::thread::Builder::new()
        .name("flush".into())
        .spawn(|| {
            loop {
                let started = LocalTime::now();
                std::thread::sleep(clock::flush_wait(started));
                flush(clock::credit_date(started.date, LocalTime::now()));
            }
        })
        .expect("spawn flush thread");
}

pub fn add(days: &mut Days, day: Date, counts: Counts) {
    *days.entry(day.to_string()).or_default() += counts;
}

pub struct Loaded {
    pub days: Days,
    pub moved_to: Option<PathBuf>,
}

pub fn load(path: &Path, stamp: &str) -> io::Result<Loaded> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Ok(Loaded {
                days: Days::new(),
                moved_to: None,
            });
        }
        Err(err) => return Err(err),
    };
    match serde_json::from_slice(&bytes) {
        Ok(days) => Ok(Loaded {
            days,
            moved_to: None,
        }),
        Err(_) => {
            let moved_to = path.with_file_name(format!("counts.corrupt-{stamp}.json"));
            std::fs::rename(path, &moved_to)?;
            Ok(Loaded {
                days: Days::new(),
                moved_to: Some(moved_to),
            })
        }
    }
}

/// Writes to a temp file first, then renames it over the old one, so a crash mid-write
/// leaves the previous file intact.
pub fn save(path: &Path, days: &Days) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    serde_json::to_writer_pretty(&mut file, days)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
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

    fn date(year: i32, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    fn counts(keys: u64, clicks: u64, scrolls: u64) -> Counts {
        Counts {
            keys,
            clicks,
            scrolls,
        }
    }

    #[test]
    fn missing_file_starts_empty() {
        let dir = TempDir::new("missing");
        let loaded = load(&dir.0.join(FILE_NAME), "x").unwrap();
        assert!(loaded.days.is_empty());
        assert!(loaded.moved_to.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new("roundtrip");
        let path = dir.0.join(FILE_NAME);
        let mut days = Days::new();
        add(&mut days, date(2026, 11, 20), counts(5, 2, 1));
        save(&path, &days).unwrap();
        assert!(!path.with_extension("json.tmp").exists());

        let loaded = load(&path, "x").unwrap();
        assert_eq!(loaded.days, days);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"2026-11-20\""));
        assert!(text.contains("\"keys\": 5"));
    }

    #[test]
    fn add_merges_into_existing_day() {
        let dir = TempDir::new("merge");
        let path = dir.0.join(FILE_NAME);
        std::fs::write(
            &path,
            r#"{ "2026-11-19": { "keys": 10, "clicks": 3, "scrolls": 0 },
                 "2026-11-20": { "keys": 1, "clicks": 1, "scrolls": 1 } }"#,
        )
        .unwrap();

        let mut days = load(&path, "x").unwrap().days;
        add(&mut days, date(2026, 11, 20), counts(4, 0, 2));
        add(&mut days, date(2026, 11, 21), counts(1, 0, 0));

        assert_eq!(days["2026-11-19"], counts(10, 3, 0));
        assert_eq!(days["2026-11-20"], counts(5, 1, 3));
        assert_eq!(days["2026-11-21"], counts(1, 0, 0));
    }

    #[test]
    fn missing_fields_default_to_zero() {
        let dir = TempDir::new("partial");
        let path = dir.0.join(FILE_NAME);
        std::fs::write(&path, r#"{ "2026-11-20": { "keys": 7 } }"#).unwrap();
        let days = load(&path, "x").unwrap().days;
        assert_eq!(days["2026-11-20"], counts(7, 0, 0));
    }

    #[test]
    fn corrupt_file_is_moved_aside_not_overwritten() {
        let dir = TempDir::new("corrupt");
        let path = dir.0.join(FILE_NAME);
        std::fs::write(&path, b"{ not json").unwrap();

        let loaded = load(&path, "20261120-101500").unwrap();
        let moved = dir.0.join("counts.corrupt-20261120-101500.json");
        assert_eq!(loaded.moved_to.as_deref(), Some(moved.as_path()));
        assert!(loaded.days.is_empty());
        assert!(!path.exists());
        assert_eq!(std::fs::read(&moved).unwrap(), b"{ not json");
    }

    #[test]
    fn failed_load_keeps_counting_then_merges_on_retry() {
        let dir = TempDir::new("retry");
        let path = dir.0.join(FILE_NAME);
        // A directory in the file's place makes every read fail, like a file locked by another process.
        std::fs::create_dir(&path).unwrap();

        let mut storage = Storage::open(path.clone(), "s");
        assert!(storage.saving_blocked());
        assert!(storage.error.is_some());

        storage.flush(date(2026, 11, 20), counts(3, 1, 0), "s");
        storage.flush(date(2026, 11, 20), counts(2, 0, 4), "s");
        assert!(storage.saving_blocked());
        assert!(path.is_dir(), "the unreadable path is never written over");
        assert_eq!(storage.day(date(2026, 11, 20)), counts(5, 1, 4));

        std::fs::remove_dir(&path).unwrap();
        std::fs::write(
            &path,
            r#"{ "2026-11-19": { "keys": 9, "clicks": 9, "scrolls": 9 },
                 "2026-11-20": { "keys": 10, "clicks": 0, "scrolls": 0 } }"#,
        )
        .unwrap();
        storage.flush(date(2026, 11, 20), Counts::default(), "s");

        assert!(!storage.saving_blocked());
        assert!(storage.error.is_none());
        let saved = load(&path, "s").unwrap().days;
        assert_eq!(saved["2026-11-19"], counts(9, 9, 9));
        assert_eq!(saved["2026-11-20"], counts(15, 1, 4));
        assert_eq!(saved, storage.days);
    }

    #[test]
    fn corrupt_file_found_on_retry_is_still_reported() {
        let dir = TempDir::new("retry-corrupt");
        let path = dir.0.join(FILE_NAME);
        std::fs::create_dir(&path).unwrap();
        let mut storage = Storage::open(path.clone(), "s");

        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        storage.flush(date(2026, 11, 20), counts(1, 0, 0), "r");

        assert!(!storage.saving_blocked());
        assert!(storage.error.is_none());
        assert!(
            storage
                .notice
                .as_deref()
                .unwrap()
                .contains("counts.corrupt-r.json")
        );
        assert!(dir.0.join("counts.corrupt-r.json").exists());
        assert_eq!(
            load(&path, "x").unwrap().days["2026-11-20"],
            counts(1, 0, 0)
        );
    }

    #[test]
    fn empty_file_counts_as_corrupt() {
        let dir = TempDir::new("empty");
        let path = dir.0.join(FILE_NAME);
        std::fs::write(&path, b"").unwrap();
        let loaded = load(&path, "s").unwrap();
        assert!(loaded.moved_to.is_some());
        assert!(dir.0.join("counts.corrupt-s.json").exists());
    }
}

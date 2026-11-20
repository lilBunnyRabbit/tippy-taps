use std::collections::BTreeMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::clock::LocalTime;
use crate::{settings, storage};

pub const SLOTS: usize = 512;
const FILE_NAME: &str = "keys.json";
const VERSION: u32 = 1;

pub type Counts = [u64; SLOTS];

static ENABLED: AtomicBool = AtomicBool::new(false);
static PENDING: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// Bumped when counts drop outside a flush (opt-out, delete), so cached views refresh.
static REVISION: AtomicU64 = AtomicU64::new(0);

pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

pub fn slot(scan: u32, extended: bool) -> usize {
    (scan & 0xff) as usize | if extended { 0x100 } else { 0 }
}

/// Runs inside the keyboard hook: one atomic add, nothing else.
#[inline]
pub fn press(scan: u32, extended: bool) {
    if ENABLED.load(Ordering::Relaxed) {
        PENDING[slot(scan, extended)].fetch_add(1, Ordering::Relaxed);
    }
}

fn take_pending() -> Counts {
    std::array::from_fn(|i| PENDING[i].swap(0, Ordering::Relaxed))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Ansi,
    Iso,
    Jis,
}

impl Shape {
    pub const ALL: [Self; 3] = [Self::Ansi, Self::Iso, Self::Jis];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ansi => "ANSI",
            Self::Iso => "ISO",
            Self::Jis => "JIS",
        }
    }
}

/// Primary language ids whose keyboards are ISO (an extra key beside left Shift, tall Enter).
const ISO_LANGUAGES: [u16; 41] = [
    0x02, 0x03, 0x05, 0x06, 0x07, 0x08, 0x0a, 0x0b, 0x0c, 0x0e, 0x0f, 0x10, 0x13, 0x14, 0x15, 0x16,
    0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1f, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2d, 0x2f,
    0x38, 0x3a, 0x3b, 0x3c, 0x52, 0x56, 0x62, 0x6e, 0x7e,
];
const LANG_ENGLISH: u16 = 0x09;
const LANG_JAPANESE: u16 = 0x11;
/// English (United Kingdom) and English (Ireland) boards are ISO; other English ones are ANSI.
const ENGLISH_ISO: [u16; 2] = [0x0809, 0x1809];

pub fn guess_shape(langid: u16) -> Shape {
    let primary = langid & 0x3ff;
    if primary == LANG_JAPANESE {
        Shape::Jis
    } else if ISO_LANGUAGES.contains(&primary)
        || (primary == LANG_ENGLISH && ENGLISH_ISO.contains(&langid))
    {
        Shape::Iso
    } else {
        Shape::Ansi
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    keys: BTreeMap<u16, u64>,
}

/// All-time presses per key position. No periods and no timestamps, by design.
struct Store {
    path: PathBuf,
    totals: Box<Counts>,
    dirty: bool,
    /// An unreadable file is never overwritten; counts stay in memory until a read works.
    loaded: bool,
    error: Option<String>,
    notice: Option<String>,
}

impl Store {
    fn open(path: PathBuf, stamp: &str) -> Self {
        let mut store = Self {
            path,
            totals: Box::new([0; SLOTS]),
            dirty: false,
            loaded: false,
            error: None,
            notice: None,
        };
        store.try_load(stamp);
        store
    }

    /// Counts already in memory were never saved, so the file's are added on top.
    fn try_load(&mut self, stamp: &str) {
        let accept = |file: File| {
            (file.version == VERSION && file.keys.keys().all(|k| usize::from(*k) < SLOTS))
                .then_some(file.keys)
        };
        match storage::read_json(&self.path, stamp, accept) {
            Ok(loaded) => {
                for (slot, n) in loaded.value.unwrap_or_default() {
                    self.totals[usize::from(slot)] += n;
                }
                if let Some(p) = loaded.moved_to {
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    self.notice = Some(format!(
                        "{FILE_NAME} was unreadable and was moved to {name}"
                    ));
                    self.dirty = self.totals.iter().any(|n| *n > 0);
                }
                self.loaded = true;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(format!("Can't open {}: {err}", self.path.display()));
            }
        }
    }

    /// Writes only when counts changed. Taps taken while turned off are dropped, so a press racing
    /// the opt-out can never recreate the file.
    fn flush(&mut self, taken: &Counts, enabled: bool, stamp: &str) {
        if enabled && taken.iter().any(|n| *n > 0) {
            for (total, n) in self.totals.iter_mut().zip(taken) {
                *total += n;
            }
            self.dirty = true;
        }
        if !self.dirty {
            return;
        }
        if !self.loaded {
            self.try_load(stamp);
        }
        if self.loaded {
            match save(&self.path, &self.totals) {
                Ok(()) => {
                    self.dirty = false;
                    self.error = None;
                }
                Err(err) => self.error = Some(format!("Can't save per-key counts: {err}")),
            }
        }
    }

    fn delete(&mut self) -> io::Result<()> {
        *self.totals = [0; SLOTS];
        self.dirty = false;
        match std::fs::remove_file(&self.path) {
            Err(err) if err.kind() != ErrorKind::NotFound => {
                self.error = Some(format!("Can't delete {}: {err}", self.path.display()));
                Err(err)
            }
            _ => {
                self.loaded = true;
                self.error = None;
                Ok(())
            }
        }
    }
}

fn save(path: &Path, totals: &Counts) -> io::Result<()> {
    let keys = totals
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(slot, n)| (slot as u16, *n))
        .collect();
    storage::write_json(
        path,
        &File {
            version: VERSION,
            keys,
        },
    )
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> Option<MutexGuard<'static, Store>> {
    STORE
        .get()
        .map(|s| s.lock().unwrap_or_else(|p| p.into_inner()))
}

/// `dir` is the counts directory: keys.json sits beside the month shards.
pub fn init(dir: &Path) {
    let store = Store::open(dir.join(FILE_NAME), &LocalTime::now().stamp());
    ENABLED.store(settings::read(|s| s.key_heatmap), Ordering::Relaxed);
    let _ = STORE.set(Mutex::new(store));
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turning off keeps the presses made while it was on: they are saved before the flag drops them.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if !on && let Some(mut store) = store() {
        let taken = take_pending();
        store.flush(&taken, true, &LocalTime::now().stamp());
    }
    REVISION.fetch_add(1, Ordering::Relaxed);
    settings::update(|s| s.key_heatmap = on);
}

pub fn delete() {
    take_pending();
    if let Some(mut store) = store() {
        let _ = store.delete();
    }
    REVISION.fetch_add(1, Ordering::Relaxed);
}

pub fn flush() {
    let Some(mut store) = store() else { return };
    let taken = take_pending();
    store.flush(&taken, enabled(), &LocalTime::now().stamp());
}

pub fn counts() -> Box<Counts> {
    let mut counts = Box::new([0; SLOTS]);
    if let Some(store) = store() {
        *counts = *store.totals;
        for (c, p) in counts.iter_mut().zip(&PENDING) {
            *c += p.load(Ordering::Relaxed);
        }
    }
    counts
}

pub fn messages() -> Vec<String> {
    store()
        .map(|s| s.notice.iter().chain(&s.error).cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tippy-taps-keys-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn taps(pairs: &[(usize, u64)]) -> Box<Counts> {
        let mut c = Box::new([0; SLOTS]);
        for (slot, n) in pairs {
            c[*slot] = *n;
        }
        c
    }

    #[test]
    fn slots_use_the_extended_bit() {
        assert_eq!(slot(0x1c, false), 0x1c, "Enter");
        assert_eq!(slot(0x1c, true), 0x11c, "numpad Enter");
        assert_eq!(slot(0x1d, true), 0x11d, "right Ctrl");
        assert_eq!(slot(0x45, true), 0x145, "Num Lock arrives extended");
        assert_eq!(slot(0x45, false), 0x45, "Pause");
        assert_eq!(slot(0xff, true), SLOTS - 1);
        assert_eq!(slot(0x1_23, false), 0x23, "stray high bits stay in range");
    }

    #[test]
    fn shape_follows_the_layout_language() {
        assert_eq!(guess_shape(0x0409), Shape::Ansi, "en-US");
        assert_eq!(guess_shape(0x0809), Shape::Iso, "en-GB");
        assert_eq!(guess_shape(0x1809), Shape::Iso, "en-IE");
        assert_eq!(guess_shape(0x0c09), Shape::Ansi, "en-AU");
        assert_eq!(guess_shape(0x0407), Shape::Iso, "de-DE");
        assert_eq!(guess_shape(0x0424), Shape::Iso, "sl-SI");
        assert_eq!(guess_shape(0x040c), Shape::Iso, "fr-FR");
        assert_eq!(guess_shape(0x0419), Shape::Iso, "ru-RU");
        assert_eq!(guess_shape(0x0411), Shape::Jis, "ja-JP");
        assert_eq!(guess_shape(0x0412), Shape::Ansi, "ko-KR");
        assert_eq!(guess_shape(0x0804), Shape::Ansi, "zh-CN");
    }

    #[test]
    fn flush_writes_only_when_dirty_and_round_trips() {
        let dir = temp("roundtrip");
        let path = dir.join(FILE_NAME);
        let mut store = Store::open(path.clone(), "s");
        store.flush(&taps(&[]), true, "s");
        assert!(!path.exists(), "nothing pressed, nothing written");

        store.flush(&taps(&[(0x1e, 3), (0x11c, 1)]), true, "s");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"30\": 3"));
        assert!(text.contains("\"284\": 1"));
        std::fs::write(&path, "sentinel").unwrap();
        store.flush(&taps(&[]), true, "s");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel");

        std::fs::remove_file(&path).unwrap();
        store.flush(&taps(&[(0x1e, 2)]), true, "s");
        let reopened = Store::open(path, "s");
        assert_eq!(reopened.totals[0x1e], 5);
        assert_eq!(reopened.totals[0x11c], 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opt_out_delete_removes_the_file_and_drops_late_taps() {
        let dir = temp("delete");
        let path = dir.join(FILE_NAME);
        let mut store = Store::open(path.clone(), "s");
        store.flush(&taps(&[(0x10, 7)]), true, "s");
        assert!(path.exists());

        store.delete().unwrap();
        assert!(!path.exists());
        assert!(store.totals.iter().all(|n| *n == 0));
        store.flush(&taps(&[(0x10, 1)]), false, "s");
        assert!(!path.exists(), "a press racing the opt-out is dropped");
        assert!(store.delete().is_ok(), "deleting twice is fine");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_file_is_kept_and_merged_later() {
        let dir = temp("locked");
        let path = dir.join(FILE_NAME);
        std::fs::create_dir(&path).unwrap();
        let mut store = Store::open(path.clone(), "s");
        assert!(store.error.is_some());
        store.flush(&taps(&[(0x20, 2)]), true, "s");
        assert!(path.is_dir());

        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, r#"{ "version": 1, "keys": { "32": 5, "33": 1 } }"#).unwrap();
        store.flush(&taps(&[]), true, "s");
        assert!(store.error.is_none());
        let saved = Store::open(path, "s");
        assert_eq!((saved.totals[0x20], saved.totals[0x21]), (7, 1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_or_out_of_range_file_is_moved_aside() {
        let dir = temp("corrupt");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{ "version": 1, "keys": { "512": 5 } }"#).unwrap();
        let store = Store::open(path.clone(), "t");
        assert!(dir.join("keys.corrupt-t.json").exists());
        assert!(store.notice.is_some());
        assert!(store.totals.iter().all(|n| *n == 0));
        let _ = std::fs::remove_dir_all(dir);
    }
}

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::clock::LocalTime;
use crate::keyboard::Shape;
use crate::storage;

const FILE_NAME: &str = "settings.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub overlay: bool,
    pub overlay_pos: Option<[f32; 2]>,
    pub fireworks: bool,
    pub celebrated: BTreeSet<String>,
    pub key_heatmap: bool,
    pub key_shape: Option<Shape>,
    /// The installed layout picked for key labels, as its HKL in hex.
    pub key_layout: Option<String>,
}

struct Store {
    path: PathBuf,
    settings: Settings,
    /// An unreadable file is never overwritten; changes stay in memory until a read works.
    loaded: bool,
    error: Option<String>,
}

impl Store {
    fn open(path: PathBuf, stamp: &str) -> Self {
        let mut store = Self {
            path,
            settings: Settings::default(),
            loaded: false,
            error: None,
        };
        store.try_load(stamp);
        store
    }

    /// The in-memory choices are newer than the file's, except the celebrated set, which only
    /// grows and is merged.
    fn try_load(&mut self, stamp: &str) {
        match storage::read_json(&self.path, stamp, Some::<Settings>) {
            Ok(loaded) => {
                if let Some(file) = loaded.value {
                    let celebrated = std::mem::take(&mut self.settings.celebrated);
                    if self.settings == Settings::default() {
                        self.settings = file;
                    } else {
                        self.settings.celebrated = file.celebrated;
                    }
                    self.settings.celebrated.extend(celebrated);
                }
                self.loaded = true;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(format!("Can't open {}: {err}", self.path.display()));
            }
        }
    }

    fn save(&mut self, stamp: &str) {
        if !self.loaded {
            self.try_load(stamp);
        }
        if self.loaded {
            self.error = storage::write_json(&self.path, &self.settings)
                .err()
                .map(|err| format!("Can't save settings: {err}"));
        }
    }
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
static FIREWORKS: AtomicBool = AtomicBool::new(false);

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE
        .get()
        .expect("settings::init runs first")
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

pub fn init(dir: &Path) {
    let store = Store::open(dir.join(FILE_NAME), &LocalTime::now().stamp());
    FIREWORKS.store(store.settings.fireworks, Ordering::Relaxed);
    let _ = STORE.set(Mutex::new(store));
}

pub fn fireworks() -> bool {
    FIREWORKS.load(Ordering::Relaxed)
}

pub fn error() -> Option<String> {
    store().error.clone()
}

pub fn read<T>(f: impl FnOnce(&Settings) -> T) -> T {
    f(&store().settings)
}

pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut store = store();
    f(&mut store.settings);
    FIREWORKS.store(store.settings.fireworks, Ordering::Relaxed);
    store.save(&LocalTime::now().stamp());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tippy-taps-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_are_off_and_round_trip() {
        let dir = temp("roundtrip");
        let path = dir.join(FILE_NAME);
        let mut store = Store::open(path.clone(), "s");
        assert!(!store.settings.fireworks);
        assert!(!store.settings.overlay);

        store.settings.fireworks = true;
        store.settings.celebrated.insert("keys:1000".into());
        store.save("s");
        let reopened = Store::open(path, "s");
        assert_eq!(reopened.settings, store.settings);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_file_is_not_overwritten_and_merges_later() {
        let dir = temp("locked");
        let path = dir.join(FILE_NAME);
        std::fs::create_dir(&path).unwrap();
        let mut store = Store::open(path.clone(), "s");
        assert!(store.error.is_some());
        store.settings.celebrated.insert("keys:10000".into());
        store.save("s");
        assert!(path.is_dir());

        std::fs::remove_dir(&path).unwrap();
        std::fs::write(
            &path,
            r#"{ "fireworks": true, "celebrated": ["keys:1000"] }"#,
        )
        .unwrap();
        store.save("s");
        assert!(store.error.is_none());
        let saved = Store::open(path, "s").settings;
        assert!(saved.celebrated.contains("keys:1000"));
        assert!(saved.celebrated.contains("keys:10000"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

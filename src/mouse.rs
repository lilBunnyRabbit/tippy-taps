use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors,
    GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, EDD_GET_DEVICE_INTERFACE_NAME, GetMessageW,
    MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, RegisterClassW, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_POWERBROADCAST,
    WM_SETTINGCHANGE, WNDCLASSW,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::clock::LocalTime;
use crate::counts::{self, DIRECTIONS, Field};
use crate::storage;

pub const MAX_MONITORS: usize = 8;
const LONG_SIDE: u32 = 48;
pub const MAX_CELLS: usize = (LONG_SIDE * LONG_SIDE) as usize;
const FILE_NAME: &str = "mouse.json";
const VERSION: u32 = 1;
const DEFAULT_DPI: u32 = 96;
const UM_PER_INCH: u32 = 25_400;
/// tan(22.5°): sector edges sit 22.5° either side of each axis.
const TAN_NUM: i64 = 408;
const TAN_DEN: i64 = 985;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

pub fn grid(width: i32, height: i32) -> (u32, u32) {
    let (w, h) = (width.max(1) as u32, height.max(1) as u32);
    let short = |s: u32, l: u32| ((LONG_SIDE * s + l / 2) / l).clamp(1, LONG_SIDE);
    if w >= h {
        (LONG_SIDE, short(h, w))
    } else {
        (short(w, h), LONG_SIDE)
    }
}

fn um_per_px(dpi: u32) -> u32 {
    let dpi = if dpi == 0 { DEFAULT_DPI } else { dpi };
    (UM_PER_INCH * 1000 + dpi / 2) / dpi
}

pub fn sector(dx: i32, dy: i32) -> Option<usize> {
    if dx == 0 && dy == 0 {
        return None;
    }
    let (ax, ay) = (i64::from(dx).abs(), i64::from(dy).abs());
    let (east, south) = (dx > 0, dy > 0);
    Some(if ay * TAN_DEN <= ax * TAN_NUM {
        if east { 2 } else { 6 }
    } else if ax * TAN_DEN <= ay * TAN_NUM {
        if south { 4 } else { 0 }
    } else {
        match (east, south) {
            (true, false) => 1,
            (true, true) => 3,
            (false, true) => 5,
            (false, false) => 7,
        }
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry {
    rect: Rect,
    cols: u32,
    rows: u32,
    um_per_px: u32,
}

impl Entry {
    fn cell(&self, x: i32, y: i32) -> Option<usize> {
        let r = self.rect;
        if x < r.left || x >= r.right || y < r.top || y >= r.bottom {
            return None;
        }
        let col = (i64::from(x) - i64::from(r.left)) * i64::from(self.cols) / i64::from(r.width());
        let row = (i64::from(y) - i64::from(r.top)) * i64::from(self.rows) / i64::from(r.height());
        Some(row as usize * self.cols as usize + col as usize)
    }
}

/// `(slot, cell, um_per_px)` of the monitor under a point.
type Hit = (usize, usize, u32);

fn find(entries: impl IntoIterator<Item = Entry>, x: i32, y: i32) -> Option<Hit> {
    entries
        .into_iter()
        .enumerate()
        .find_map(|(slot, e)| e.cell(x, y).map(|cell| (slot, cell, e.um_per_px)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
    px: u64,
    fine: u64,
    sector: usize,
}

/// Hook thread only. A point from another epoch (start, display change, resume) is not a previous point.
#[derive(Clone, Copy, Debug)]
struct Tracker {
    prev: Option<(i32, i32)>,
    epoch: u32,
    carry: u64,
}

impl Tracker {
    const NEW: Self = Self {
        prev: None,
        epoch: 0,
        carry: 0,
    };

    fn step(&mut self, x: i32, y: i32, epoch: u32, max_d2: u64) -> Option<Step> {
        let prev = self.prev.filter(|_| self.epoch == epoch);
        self.prev = Some((x, y));
        self.epoch = epoch;
        let (px, py) = prev?;
        let (dx, dy) = (i64::from(x) - i64::from(px), i64::from(y) - i64::from(py));
        let d2 = (dx * dx + dy * dy) as u64;
        if d2 == 0 || d2 > max_d2 {
            return None;
        }
        let fine = (d2 << 16).isqrt();
        self.carry += fine;
        let whole = self.carry >> 8;
        self.carry &= 0xff;
        Some(Step {
            px: whole,
            fine,
            sector: sector(dx as i32, dy as i32)?,
        })
    }
}

thread_local! {
    static TRACKER: Cell<Tracker> = const { Cell::new(Tracker::NEW) };
}

struct Slot {
    left: AtomicI32,
    top: AtomicI32,
    right: AtomicI32,
    bottom: AtomicI32,
    cols: AtomicU32,
    rows: AtomicU32,
    um_per_px: AtomicU32,
}

impl Slot {
    const fn new() -> Self {
        Self {
            left: AtomicI32::new(0),
            top: AtomicI32::new(0),
            right: AtomicI32::new(0),
            bottom: AtomicI32::new(0),
            cols: AtomicU32::new(0),
            rows: AtomicU32::new(0),
            um_per_px: AtomicU32::new(0),
        }
    }

    fn load(&self) -> Entry {
        let r = Ordering::Relaxed;
        Entry {
            rect: Rect {
                left: self.left.load(r),
                top: self.top.load(r),
                right: self.right.load(r),
                bottom: self.bottom.load(r),
            },
            cols: self.cols.load(r),
            rows: self.rows.load(r),
            um_per_px: self.um_per_px.load(r),
        }
    }

    fn store(&self, e: Entry) {
        let r = Ordering::Relaxed;
        self.left.store(e.rect.left, r);
        self.top.store(e.rect.top, r);
        self.right.store(e.rect.right, r);
        self.bottom.store(e.rect.bottom, r);
        self.cols.store(e.cols, r);
        self.rows.store(e.rows, r);
        self.um_per_px.store(e.um_per_px, r);
    }
}

/// The monitor table the hook reads, guarded by a sequence lock: odd while it is being rewritten,
/// and every rewrite moves it on, which also starts a new epoch for the previous point.
static SEQ: AtomicU32 = AtomicU32::new(0);
static TABLE: [Slot; MAX_MONITORS] = [const { Slot::new() }; MAX_MONITORS];
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// Squared virtual desktop diagonal: a longer single jump is a warp, not a move.
static MAX_D2: AtomicU64 = AtomicU64::new(0);
static MOVES: [[AtomicU64; MAX_CELLS]; MAX_MONITORS] =
    [const { [const { AtomicU64::new(0) }; MAX_CELLS] }; MAX_MONITORS];
static CLICKS: [[AtomicU64; MAX_CELLS]; MAX_MONITORS] =
    [const { [const { AtomicU64::new(0) }; MAX_CELLS] }; MAX_MONITORS];
static REVISION: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code, reason = "read by the Mouse tab")]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// Hook path: loads, integer math and atomic adds only. A point read mid-rewrite is dropped.
#[inline]
fn locate(x: i32, y: i32) -> Option<(u32, Option<Hit>)> {
    let epoch = SEQ.load(Ordering::Acquire);
    if epoch & 1 != 0 {
        return None;
    }
    let live = LIVE.load(Ordering::Relaxed).min(MAX_MONITORS);
    let hit = find(TABLE[..live].iter().map(Slot::load), x, y);
    fence(Ordering::Acquire);
    (SEQ.load(Ordering::Relaxed) == epoch).then_some((epoch, hit))
}

pub fn moved(x: i32, y: i32) {
    let Some((epoch, hit)) = locate(x, y) else {
        return;
    };
    let max_d2 = MAX_D2.load(Ordering::Relaxed);
    let step = TRACKER.with(|t| {
        let mut tracker = t.get();
        let step = tracker.step(x, y, epoch, max_d2);
        t.set(tracker);
        step
    });
    let Some(step) = step else { return };
    let um = hit.map_or_else(|| um_per_px(DEFAULT_DPI), |(_, _, um)| um);
    counts::add(Field::m_um, step.fine * u64::from(um) / 256_000);
    if step.px > 0 {
        counts::add(Field::m_px, step.px);
        counts::add(DIRECTIONS[step.sector], step.px);
        if let Some((slot, cell, _)) = hit {
            MOVES[slot][cell].fetch_add(step.px, Ordering::Relaxed);
        }
    }
}

pub fn clicked(x: i32, y: i32) {
    if let Some((_, Some((slot, cell, _)))) = locate(x, y) {
        CLICKS[slot][cell].fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Last-known position in physical virtual-desktop pixels.
    pub rect: Rect,
    pub dpi: u32,
    pub cols: u32,
    pub rows: u32,
    /// Pixels moved per cell, row-major: cell `i` is column `i % cols`, row `i / cols`.
    pub movement: Vec<u64>,
    pub clicks: Vec<u64>,
}

impl Monitor {
    fn new(rect: Rect, dpi: u32) -> Self {
        let (cols, rows) = grid(rect.width(), rect.height());
        let cells = (cols * rows) as usize;
        Self {
            name: None,
            rect,
            dpi,
            cols,
            rows,
            movement: vec![0; cells],
            clicks: vec![0; cells],
        }
    }

    fn valid(&self) -> bool {
        let cells = (self.cols * self.rows) as usize;
        (1..=LONG_SIDE).contains(&self.cols)
            && (1..=LONG_SIDE).contains(&self.rows)
            && self.movement.len() == cells
            && self.clicks.len() == cells
    }

    fn entry(&self) -> Entry {
        Entry {
            rect: self.rect,
            cols: self.cols,
            rows: self.rows,
            um_per_px: um_per_px(self.dpi),
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.movement.iter().chain(&self.clicks).all(|n| *n == 0)
    }

    fn add(&mut self, other: &Self) {
        let (cols, rows) = (other.cols as usize, other.rows as usize);
        let (to_cols, to_rows) = (self.cols as usize, self.rows as usize);
        let layers = [
            (&mut self.movement, &other.movement),
            (&mut self.clicks, &other.clicks),
        ];
        for (into, from) in layers {
            for (i, n) in from.iter().enumerate().filter(|(_, n)| **n > 0) {
                let (c, r) = (i % cols, i / cols);
                let (to_r, to_c) = (
                    (2 * r + 1) * to_rows / (2 * rows),
                    (2 * c + 1) * to_cols / (2 * cols),
                );
                into[to_r * to_cols + to_c] += n;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Display {
    pub id: String,
    pub name: Option<String>,
    pub rect: Rect,
    pub dpi: u32,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    monitors: BTreeMap<String, Monitor>,
}

struct Store {
    path: PathBuf,
    monitors: BTreeMap<String, Monitor>,
    live: Vec<String>,
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
            monitors: BTreeMap::new(),
            live: Vec::new(),
            dirty: false,
            loaded: false,
            error: None,
            notice: None,
        };
        store.try_load(stamp);
        store
    }

    /// Counts already in memory were never saved, so the file's are added on top. Live monitors
    /// keep their current position and name; the file keeps its grid size.
    fn try_load(&mut self, stamp: &str) {
        let accept = |file: File| {
            (file.version == VERSION && file.monitors.values().all(Monitor::valid))
                .then_some(file.monitors)
        };
        match storage::read_json(&self.path, stamp, accept) {
            Ok(loaded) => {
                for (id, mut saved) in loaded.value.unwrap_or_default() {
                    if let Some(current) = self.monitors.get(&id) {
                        saved.add(current);
                        if self.live.contains(&id) {
                            saved.rect = current.rect;
                            saved.name = current.name.clone();
                            saved.dpi = current.dpi;
                        }
                    }
                    self.monitors.insert(id, saved);
                }
                if let Some(p) = loaded.moved_to {
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    self.notice = Some(format!(
                        "{FILE_NAME} was unreadable and was moved to {name}"
                    ));
                    self.dirty = !self.monitors.is_empty();
                }
                self.loaded = true;
                self.error = None;
                REVISION.fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                self.error = Some(format!("Can't open {}: {err}", self.path.display()));
            }
        }
    }

    fn differs(&self, displays: &[Display]) -> bool {
        self.live.len() != displays.len()
            || displays.iter().zip(&self.live).any(|(d, id)| {
                d.id != *id
                    || self
                        .monitors
                        .get(id)
                        .is_none_or(|m| (m.rect, m.dpi, &m.name) != (d.rect, d.dpi, &d.name))
            })
    }

    /// Returns whether the hook's table must change.
    fn set_displays(&mut self, displays: &[Display]) -> bool {
        let changed = self.differs(displays);
        for d in displays {
            let m = self.monitors.entry(d.id.clone()).or_insert_with(|| {
                self.dirty = true;
                Monitor::new(d.rect, d.dpi)
            });
            if (m.rect, m.dpi, &m.name) != (d.rect, d.dpi, &d.name) {
                (m.rect, m.dpi, m.name) = (d.rect, d.dpi, d.name.clone());
                self.dirty = true;
            }
        }
        self.live = displays.iter().map(|d| d.id.clone()).collect();
        changed
    }

    fn entries(&self) -> Vec<Entry> {
        self.live
            .iter()
            .map(|id| self.monitors[id].entry())
            .collect()
    }

    /// Reads every slot's whole array. An add that passed the hook's check just before a rewrite
    /// is booked on whichever monitor now holds its slot, or dropped when that slot or cell is
    /// gone: at most one event per display change.
    fn drain(&mut self) {
        for slot in 0..MAX_MONITORS {
            let mut monitor = self.live.get(slot).and_then(|id| self.monitors.get_mut(id));
            for layer in 0..2 {
                let pending = if layer == 0 {
                    &MOVES[slot]
                } else {
                    &CLICKS[slot]
                };
                for (i, p) in pending.iter().enumerate() {
                    // A plain load first, so idle slots stay clean, untouched pages.
                    if p.load(Ordering::Relaxed) == 0 {
                        continue;
                    }
                    let taken = p.swap(0, Ordering::Relaxed);
                    let cell = monitor.as_mut().and_then(|m| {
                        let cells = if layer == 0 {
                            &mut m.movement
                        } else {
                            &mut m.clicks
                        };
                        cells.get_mut(i)
                    });
                    if taken > 0
                        && let Some(n) = cell
                    {
                        *n += taken;
                        self.dirty = true;
                    }
                }
            }
        }
    }

    /// Rewrites the hook's table after `change`. Pending cells are booked under the old slots
    /// first, while the odd sequence keeps the hook out.
    fn rewrite(&mut self, change: impl FnOnce(&mut Self)) {
        SEQ.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.drain();
        change(self);
        let entries = self.entries();
        for (slot, entry) in TABLE.iter().zip(&entries) {
            slot.store(*entry);
        }
        LIVE.store(entries.len(), Ordering::Relaxed);
        MAX_D2.store(max_jump(&entries), Ordering::Relaxed);
        SEQ.fetch_add(1, Ordering::Release);
        REVISION.fetch_add(1, Ordering::Relaxed);
    }

    fn publish(&mut self, displays: &[Display]) {
        self.rewrite(|s| {
            s.set_displays(displays);
        });
    }

    fn flush(&mut self, stamp: &str) {
        self.drain();
        if !self.dirty {
            return;
        }
        if !self.loaded {
            // Loading can change a live monitor's grid size, so the hook must not add meanwhile.
            // A file that still can't be opened skips the rewrite, which would cost a move delta.
            match std::fs::File::open(&self.path) {
                Err(err) if err.kind() != ErrorKind::NotFound => {
                    self.error = Some(format!("Can't open {}: {err}", self.path.display()));
                }
                _ => self.rewrite(|s| s.try_load(stamp)),
            }
        }
        if self.loaded {
            let file = File {
                version: VERSION,
                monitors: self.monitors.clone(),
            };
            match storage::write_json_compact(&self.path, &file) {
                Ok(()) => {
                    self.dirty = false;
                    self.error = None;
                }
                Err(err) => self.error = Some(format!("Can't save mouse positions: {err}")),
            }
        }
    }
}

fn max_jump(entries: &[Entry]) -> u64 {
    let Some(first) = entries.first() else {
        return 0;
    };
    let mut b = first.rect;
    for e in entries {
        b.left = b.left.min(e.rect.left);
        b.top = b.top.min(e.rect.top);
        b.right = b.right.max(e.rect.right);
        b.bottom = b.bottom.max(e.rect.bottom);
    }
    let (w, h) = (i64::from(b.width()), i64::from(b.height()));
    (w * w + h * h) as u64
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> Option<MutexGuard<'static, Store>> {
    STORE
        .get()
        .map(|s| s.lock().unwrap_or_else(|p| p.into_inner()))
}

pub fn init(dir: &Path) {
    let store = Store::open(dir.join(FILE_NAME), &LocalTime::now().stamp());
    let _ = STORE.set(Mutex::new(store));
}

pub fn flush() {
    if let Some(mut store) = store() {
        store.flush(&LocalTime::now().stamp());
    }
}

#[allow(dead_code, reason = "read by the Mouse tab")]
pub fn messages() -> Vec<String> {
    store()
        .map(|s| s.notice.iter().chain(&s.error).cloned().collect())
        .unwrap_or_default()
}

#[allow(dead_code, reason = "read by the Mouse tab")]
#[derive(Clone, Debug)]
pub struct Screen {
    pub id: String,
    pub connected: bool,
    pub monitor: Monitor,
}

/// Pending counts included. Read on `revision` or pending change, not per frame.
#[allow(dead_code, reason = "read by the Mouse tab")]
pub fn snapshot() -> Vec<Screen> {
    let Some(store) = store() else {
        return Vec::new();
    };
    store
        .monitors
        .iter()
        .map(|(id, m)| {
            let mut monitor = m.clone();
            let slot = store.live.iter().position(|l| l == id);
            if let Some(slot) = slot {
                let layers = [
                    (&mut monitor.movement, &MOVES[slot]),
                    (&mut monitor.clicks, &CLICKS[slot]),
                ];
                for (into, pending) in layers {
                    for (n, p) in into.iter_mut().zip(pending.iter()) {
                        *n += p.load(Ordering::Relaxed);
                    }
                }
            }
            Screen {
                id: id.clone(),
                connected: slot.is_some(),
                monitor,
            }
        })
        .collect()
}

/// `reset` starts a new epoch even when nothing changed, as after a resume from sleep.
fn refresh(reset: bool) {
    let mut displays = displays();
    let Some(mut store) = store() else { return };
    // A name lookup that failed mid display change keeps the name already known.
    for d in &mut displays {
        if d.name.is_none() {
            d.name = store.monitors.get(&d.id).and_then(|m| m.name.clone());
        }
    }
    if reset || store.differs(&displays) {
        store.publish(&displays);
    }
}

fn wide(text: &[u16]) -> String {
    let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

fn displays() -> Vec<Display> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let handles = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        handles.push(monitor);
        TRUE
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut handles as *mut _ as isize),
        );
    }
    let names = friendly_names();
    handles
        .into_iter()
        .filter_map(|h| display(h, &names))
        .take(MAX_MONITORS)
        .collect()
}

struct Named {
    output: String,
    path: String,
    name: String,
}

/// The EDID model names ("DELL U2720Q") that `EnumDisplayDevicesW` reports only as "Generic PnP
/// Monitor".
fn friendly_names() -> Vec<Named> {
    // The configuration can change between sizing and querying; Microsoft's docs say to retry.
    let mut paths = Vec::new();
    let mut path_count = 0;
    for _ in 0..3 {
        let mut mode_count = 0;
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        } != ERROR_SUCCESS
        {
            return Vec::new();
        }
        paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let queried = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        match queried {
            ERROR_SUCCESS => break,
            ERROR_INSUFFICIENT_BUFFER => paths.clear(),
            _ => return Vec::new(),
        }
    }
    paths.truncate(path_count as usize);
    paths
        .iter()
        .filter_map(|p| {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: p.sourceInfo.adapterId,
                    id: p.sourceInfo.id,
                },
                ..Default::default()
            };
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                ..Default::default()
            };
            let ok = unsafe {
                DisplayConfigGetDeviceInfo(&mut source.header) == 0
                    && DisplayConfigGetDeviceInfo(&mut target.header) == 0
            };
            let name = wide(&target.monitorFriendlyDeviceName);
            (ok && !name.is_empty()).then(|| Named {
                output: wide(&source.viewGdiDeviceName),
                path: wide(&target.monitorDevicePath),
                name,
            })
        })
        .collect()
}

/// Identifies a monitor by its device interface path, which survives reboots and re-plugs, with the
/// output name and resolution as a fallback.
fn display(monitor: HMONITOR, names: &[Named]) -> Option<Display> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) };
    if !ok.as_bool() {
        return None;
    }
    let r = info.monitorInfo.rcMonitor;
    let rect = Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    };
    if rect.width() <= 0 || rect.height() <= 0 {
        return None;
    }
    let (mut dpi, mut dpi_y) = (DEFAULT_DPI, DEFAULT_DPI);
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) };

    let output = PCWSTR(info.szDevice.as_ptr());
    let mut device = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let mut found = None;
    for i in 0..MAX_MONITORS as u32 {
        if !unsafe { EnumDisplayDevicesW(output, i, &mut device, EDD_GET_DEVICE_INTERFACE_NAME) }
            .as_bool()
        {
            break;
        }
        if (device.StateFlags & DISPLAY_DEVICE_ACTIVE).0 != 0 {
            found = Some((wide(&device.DeviceID), wide(&device.DeviceString)));
            break;
        }
    }
    let (path, name) = found.unwrap_or_default();
    let id = if path.is_empty() {
        format!(
            "{} {}x{}",
            wide(&info.szDevice),
            rect.width(),
            rect.height()
        )
    } else {
        path
    };
    let gdi_name = wide(&info.szDevice);
    let name = names
        .iter()
        .find(|n| n.path.eq_ignore_ascii_case(&id))
        .or_else(|| names.iter().find(|n| n.output == gdi_name))
        .map(|n| n.name.clone())
        .or(Some(name).filter(|n| !n.is_empty() && !n.starts_with("Generic ")));
    Some(Display {
        id,
        name,
        rect,
        dpi,
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => refresh(false),
        WM_POWERBROADCAST
            if matches!(
                wparam.0 as u32,
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND
            ) =>
        {
            refresh(true)
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Hidden top-level window, not message-only: message-only windows get no `WM_DISPLAYCHANGE` broadcast.
pub fn spawn() {
    std::thread::Builder::new()
        .name("displays".into())
        .spawn(|| unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            refresh(true);
            let Ok(instance) = GetModuleHandleW(None) else {
                return;
            };
            let class_name = w!("tippy-taps-displays");
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&class);
            if CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("tippy-taps displays"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .is_err()
            {
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("spawn displays thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn entry(r: Rect) -> Entry {
        let (cols, rows) = grid(r.width(), r.height());
        Entry {
            rect: r,
            cols,
            rows,
            um_per_px: um_per_px(96),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tippy-taps-mouse-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sectors_by_slope() {
        assert_eq!(sector(0, -5), Some(0), "screen up is N");
        assert_eq!(sector(4, -4), Some(1));
        assert_eq!(sector(5, 0), Some(2));
        assert_eq!(sector(3, 3), Some(3));
        assert_eq!(sector(0, 7), Some(4));
        assert_eq!(sector(-2, 2), Some(5));
        assert_eq!(sector(-9, 0), Some(6));
        assert_eq!(sector(-1, -1), Some(7));
        assert_eq!(sector(0, 0), None);
        assert_eq!(sector(1, 0), Some(2), "one pixel on the axis");
        assert_eq!(sector(0, -1), Some(0));
        assert_eq!(sector(1000, -415), Some(1), "just past 22.5° is diagonal");
        assert_eq!(
            sector(1000, -414),
            Some(2),
            "just under 22.5° stays on the axis"
        );
        assert_eq!(sector(-414, 1000), Some(4));
        assert_eq!(sector(-415, 1000), Some(5));
        assert_eq!(sector(i32::MAX, i32::MIN), Some(1), "no overflow");
    }

    #[test]
    fn first_move_and_warps_do_not_count() {
        let max = 100 * 100;
        let mut t = Tracker::NEW;
        assert_eq!(t.step(10, 10, 2, max), None, "first event");
        let s = t.step(13, 14, 2, max).unwrap();
        assert_eq!((s.px, s.fine, s.sector), (5, 5 * 256, 3));
        assert_eq!(t.step(13, 14, 2, max), None, "no movement");
        assert_eq!(t.step(13, 200, 2, max), None, "warp past the diagonal");
        assert_eq!(
            t.step(13, 201, 2, max).map(|s| s.px),
            Some(1),
            "counts again after a warp"
        );
        assert_eq!(
            t.step(50, 50, 4, max),
            None,
            "new epoch: display change or resume"
        );
        assert_eq!(t.step(50, 49, 4, max).map(|s| s.sector), Some(0));
    }

    #[test]
    fn sub_pixel_moves_carry_over() {
        let mut t = Tracker::NEW;
        t.step(0, 0, 0, u64::MAX >> 17);
        let total: u64 = (1..=100)
            .filter_map(|i| t.step(i, i, 0, u64::MAX >> 17))
            .map(|s| s.px)
            .sum();
        assert_eq!(total, 141, "100 diagonal steps are 141.4 px");
    }

    #[test]
    fn grids_follow_the_aspect_ratio() {
        assert_eq!(grid(2560, 1440), (48, 27));
        assert_eq!(grid(1920, 1080), (48, 27));
        assert_eq!(grid(5120, 1440), (48, 14));
        assert_eq!(grid(1080, 1920), (27, 48));
        assert_eq!(grid(1000, 1000), (48, 48));
        assert_eq!(grid(10_000, 1), (48, 1));
        assert_eq!(um_per_px(96), 264_583);
        assert_eq!(um_per_px(144), 176_389);
        assert_eq!(um_per_px(0), 264_583);
    }

    #[test]
    fn points_map_to_cells_per_monitor() {
        let primary = entry(rect(0, 0, 2560, 1440));
        let left = entry(rect(-1920, 0, 0, 1080));
        let above = entry(rect(0, -1080, 1920, 0));
        let table = [primary, left, above];
        assert_eq!(find(table, 0, 0), Some((0, 0, primary.um_per_px)));
        assert_eq!(find(table, 2559, 1439).map(|h| h.1), Some(27 * 48 - 1));
        assert_eq!(find(table, 2560, 10), None, "right edge is outside");
        assert_eq!(find(table, -1920, 0).map(|h| (h.0, h.1)), Some((1, 0)));
        assert_eq!(find(table, -1, 0).map(|h| (h.0, h.1)), Some((1, 47)));
        assert_eq!(
            find(table, -1, 1079).map(|h| (h.0, h.1)),
            Some((1, 27 * 48 - 1))
        );
        assert_eq!(
            find(table, -960, 540).map(|h| (h.0, h.1)),
            Some((1, 13 * 48 + 24))
        );
        assert_eq!(find(table, 0, -1).map(|h| (h.0, h.1)), Some((2, 26 * 48)));
        assert_eq!(find(table, 100, -1080).map(|h| (h.0, h.1)), Some((2, 2)));
        assert_eq!(find(table, -5, -5), None, "the corner no monitor covers");
        assert_eq!(find(table, 3000, 3000), None);
        assert_eq!(find([], 0, 0), None);
    }

    #[test]
    fn max_jump_spans_every_screen() {
        assert_eq!(max_jump(&[]), 0);
        let both = [entry(rect(0, 0, 300, 400)), entry(rect(-300, -400, 0, 0))];
        assert_eq!(max_jump(&both), 600 * 600 + 800 * 800);
    }

    fn display(id: &str, r: Rect) -> Display {
        Display {
            id: id.into(),
            name: Some(format!("{id} screen")),
            rect: r,
            dpi: 96,
        }
    }

    #[test]
    fn file_round_trips_and_writes_only_when_dirty() {
        let dir = temp("roundtrip");
        let path = dir.join(FILE_NAME);
        let mut store = Store::open(path.clone(), "s");
        store.flush("s");
        assert!(!path.exists(), "nothing seen, nothing written");

        store.set_displays(&[
            display("a", rect(0, 0, 1920, 1080)),
            display("b", rect(-1080, 0, 0, 1920)),
        ]);
        store.monitors.get_mut("a").unwrap().movement[5] = 40;
        store.monitors.get_mut("b").unwrap().clicks[47] = 2;
        store.flush("s");
        std::fs::write(&path, "sentinel").unwrap();
        store.flush("s");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel");

        std::fs::remove_file(&path).unwrap();
        store.dirty = true;
        store.flush("s");
        let reopened = Store::open(path, "s");
        assert_eq!(reopened.monitors, store.monitors);
        let b = &reopened.monitors["b"];
        assert_eq!((b.cols, b.rows, b.clicks[47]), (27, 48, 2));
        assert_eq!(b.name.as_deref(), Some("b screen"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn same_screens_change_nothing_and_new_resolution_keeps_the_grid() {
        let mut store = Store::open(PathBuf::from("unused"), "s");
        let a = display("a", rect(0, 0, 1920, 1080));
        assert!(store.set_displays(std::slice::from_ref(&a)));
        store.dirty = false;
        assert!(!store.set_displays(std::slice::from_ref(&a)));
        assert!(!store.dirty);

        let bigger = display("a", rect(0, 0, 3840, 2160));
        assert!(store.set_displays(&[bigger]));
        let m = &store.monitors["a"];
        assert_eq!((m.cols, m.rows, m.rect.right), (48, 27, 3840));
        assert!(store.set_displays(&[]), "unplugged");
        assert!(store.monitors.contains_key("a"), "kept for drawing");
    }

    #[test]
    fn unreadable_file_is_kept_and_merged_later() {
        let dir = temp("locked");
        let path = dir.join(FILE_NAME);
        std::fs::create_dir(&path).unwrap();
        let mut store = Store::open(path.clone(), "s");
        assert!(store.error.is_some());
        store.set_displays(&[display("a", rect(0, 0, 1920, 1080))]);
        store.monitors.get_mut("a").unwrap().movement[0] = 3;
        store.flush("s");
        assert!(path.is_dir(), "never written over");

        std::fs::remove_dir(&path).unwrap();
        let mut saved = Monitor::new(rect(0, 0, 1000, 1000), 120);
        saved.movement[0] = 10;
        saved.movement[48 * 48 - 1] = 1;
        let file = File {
            version: VERSION,
            monitors: BTreeMap::from([
                ("a".to_string(), saved),
                ("gone".to_string(), Monitor::new(rect(5, 5, 10, 10), 96)),
            ]),
        };
        storage::write_json_compact(&path, &file).unwrap();
        store.flush("s");
        assert!(store.error.is_none());

        let merged = &Store::open(path, "s").monitors;
        let a = &merged["a"];
        assert_eq!((a.cols, a.rows), (48, 48), "the saved grid size wins");
        assert_eq!(a.rect.right, 1920, "a live monitor keeps its current rect");
        assert_eq!((a.movement[0], a.movement[48 * 48 - 1]), (13, 1));
        assert!(merged.contains_key("gone"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_or_invalid_file_is_moved_aside() {
        let dir = temp("corrupt");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, b"{ not json").unwrap();
        let store = Store::open(path.clone(), "t");
        assert!(dir.join("mouse.corrupt-t.json").exists());
        assert!(store.notice.is_some());
        assert!(store.monitors.is_empty());

        let bad = r#"{ "version": 1, "monitors": { "a": { "rect": { "left": 0, "top": 0,
            "right": 10, "bottom": 10 }, "dpi": 96, "cols": 2, "rows": 2,
            "movement": [1, 2, 3], "clicks": [0, 0, 0, 0] } } }"#;
        std::fs::write(&path, bad).unwrap();
        let store = Store::open(path.clone(), "u");
        assert!(
            dir.join("mouse.corrupt-u.json").exists(),
            "wrong cell count"
        );
        assert!(store.monitors.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mismatched_grids_merge_by_position() {
        let mut big = Monitor::new(rect(0, 0, 1000, 1000), 96);
        let mut small = Monitor::new(rect(0, 0, 1920, 1080), 96);
        small.movement[0] = 1;
        small.movement[27 * 48 - 1] = 2;
        small.clicks[47] = 4;
        big.add(&small);
        assert_eq!(big.movement[0], 1);
        assert_eq!(big.movement[48 * 48 - 1], 2);
        assert_eq!(big.clicks[47], 4);
        assert!(!big.is_empty());
    }
}

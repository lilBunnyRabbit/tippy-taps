use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::time::Instant;

use image::imageops::{self, FilterType};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, GetMonitorInfoW, HBITMAP,
    HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GWL_EXSTYLE,
    GetWindowLongPtrW, GetWindowRect, HC_ACTION, HHOOK, HTCAPTION, IDC_SIZEALL, KBDLLHOOKSTRUCT,
    LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, LoadCursorW, MA_NOACTIVATE, MSG,
    MSLLHOOKSTRUCT, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW,
    PostMessageW, QS_ALLINPUT, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SetCursor,
    SetWindowLongPtrW, SetWindowsHookExW, ShowWindow, TranslateMessage, ULW_ALPHA,
    UnhookWindowsHookEx, UpdateLayeredWindow, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_EXITSIZEMOVE, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_MOUSEACTIVATE, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_NCHITTEST, WM_QUIT, WM_RBUTTONDOWN,
    WM_SETCURSOR, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use crate::settings;

const CLASS: windows::core::PCWSTR = w!("tippy-taps-rabbit");
const WM_WAKE: u32 = WM_APP + 1;
const BASE_PX: u32 = 128;
const MARGIN_PX: i32 = 8;
const FRAME_MS: u64 = 33;
const HOLD_MS: u64 = 110;
const CHEER_MS: u64 = 3500;
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const BOTH: u8 = LEFT | RIGHT;
const CARD: [u8; 4] = [0x2a, 0x40, 0x4a, 150];
const CARD_EDGE: [u8; 4] = [0xe3, 0xf9, 0xb2, 255];

const ART: [[&[u8]; 2]; 5] = [
    [
        include_bytes!("../assets/rabbit/idle-128.png"),
        include_bytes!("../assets/rabbit/idle-256.png"),
    ],
    [
        include_bytes!("../assets/rabbit/left-128.png"),
        include_bytes!("../assets/rabbit/left-256.png"),
    ],
    [
        include_bytes!("../assets/rabbit/right-128.png"),
        include_bytes!("../assets/rabbit/right-256.png"),
    ],
    [
        include_bytes!("../assets/rabbit/both-128.png"),
        include_bytes!("../assets/rabbit/both-256.png"),
    ],
    [
        include_bytes!("../assets/rabbit/cheer-128.png"),
        include_bytes!("../assets/rabbit/cheer-256.png"),
    ],
];

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
static VISIBLE: AtomicBool = AtomicBool::new(false);
static MOVING: AtomicBool = AtomicBool::new(false);
static SYNC: AtomicBool = AtomicBool::new(true);
static MOVED: AtomicBool = AtomicBool::new(false);
static CHEER: AtomicBool = AtomicBool::new(false);
static SIDES: AtomicU8 = AtomicU8::new(0);

/// The thread starts on the first show and then stays, parked in its message wait while off.
pub fn sync() {
    let on = settings::read(|s| s.overlay);
    if !on {
        MOVING.store(false, Ordering::Relaxed);
    }
    SYNC.store(true, Ordering::Release);
    if on && !STARTED.swap(true, Ordering::AcqRel) {
        spawn();
    } else {
        wake();
    }
}

pub fn moving() -> bool {
    MOVING.load(Ordering::Relaxed)
}

pub fn set_moving(on: bool) {
    MOVING.store(on, Ordering::Relaxed);
    sync();
}

pub fn visible() -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

pub fn cheer() {
    if visible() {
        CHEER.store(true, Ordering::Release);
        wake();
    }
}

fn wake() {
    let hwnd = HWND(WINDOW.load(Ordering::Acquire) as *mut _);
    if !hwnd.is_invalid() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frame {
    Idle,
    Left,
    Right,
    Both,
    Cheer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Image {
    Frame(Frame),
    Unlocked,
}

/// When each paw goes back up, in ms since the overlay thread started.
#[derive(Default)]
struct Paws {
    left_until: u64,
    right_until: u64,
    cheer_until: u64,
}

impl Paws {
    fn input(&mut self, sides: u8, now: u64) {
        if sides & LEFT != 0 {
            self.left_until = now + HOLD_MS;
        }
        if sides & RIGHT != 0 {
            self.right_until = now + HOLD_MS;
        }
    }

    fn cheer(&mut self, now: u64) {
        self.cheer_until = now + CHEER_MS;
    }

    fn frame(&self, now: u64) -> Frame {
        if now < self.cheer_until {
            return Frame::Cheer;
        }
        match (now < self.left_until, now < self.right_until) {
            (true, true) => Frame::Both,
            (true, false) => Frame::Left,
            (false, true) => Frame::Right,
            (false, false) => Frame::Idle,
        }
    }

    fn next_change(&self, now: u64) -> Option<u64> {
        [self.left_until, self.right_until, self.cheer_until]
            .into_iter()
            .filter(|t| *t > now)
            .min()
    }
}

/// Which paw a key drums: the half of the board it sits on, by scan code so it holds for every
/// layout. "Left" is the paw on the viewer's left. Space is a thumb key, so both.
fn key_side(scan: u32, extended: bool) -> u8 {
    match (scan, extended) {
        (0x39, false) => BOTH,
        (0x01..=0x07 | 0x0f..=0x14 | 0x1d | 0x1e..=0x22 | 0x29..=0x30 | 0x38, false) => LEFT,
        (0x3a..=0x40 | 0x56, false) => LEFT,
        (0x5b, true) => LEFT,
        _ => RIGHT,
    }
}

fn mouse_side(message: u32) -> u8 {
    match message {
        WM_LBUTTONDOWN => LEFT,
        WM_RBUTTONDOWN => RIGHT,
        WM_MBUTTONDOWN | WM_XBUTTONDOWN | WM_MOUSEWHEEL | WM_MOUSEHWHEEL => BOTH,
        _ => 0,
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if info.flags.0 & LLKHF_INJECTED.0 == 0 {
            let side = key_side(info.scanCode, info.flags.0 & LLKHF_EXTENDED.0 != 0);
            SIDES.fetch_or(side, Ordering::Relaxed);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let side = mouse_side(wparam.0 as u32);
        if side != 0 {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if info.flags & LLMHF_INJECTED == 0 {
                SIDES.fetch_or(side, Ordering::Relaxed);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn art_px(dpi: u32) -> u32 {
    (BASE_PX * dpi + 48) / 96
}

/// UpdateLayeredWindow blends premultiplied BGRA.
fn to_bgra(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = p[3] as u32;
            let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            [m(p[2]), m(p[1]), m(p[0]), p[3]]
        })
        .collect()
}

/// Layered windows let clicks through fully transparent pixels, so the unlocked card must have
/// alpha everywhere to be draggable by any point.
fn on_card(bgra: &mut [u8], size: u32, border: u32) {
    let card = to_bgra(&CARD);
    let edge = to_bgra(&CARD_EDGE);
    for (i, px) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i as u32 % size, i as u32 / size);
        let on_edge = x < border || y < border || x + border >= size || y + border >= size;
        let bg = if on_edge { &edge } else { &card };
        let rest = 255 - px[3] as u32;
        for c in 0..4 {
            px[c] += ((bg[c] as u32 * rest + 127) / 255) as u8;
        }
    }
}

fn place(saved: Option<POINT>, size: i32, work: RECT, margin: i32) -> POINT {
    let at = saved.unwrap_or(POINT {
        x: work.right - size - margin,
        y: work.bottom - size - margin,
    });
    POINT {
        x: at.x.min(work.right - size).max(work.left),
        y: at.y.min(work.bottom - size).max(work.top),
    }
}

struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

fn bitmap(bgra: &[u8], size: u32) -> Option<Bitmap> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let handle =
        unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;
    let bitmap = Bitmap(handle);
    if bits.is_null() {
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast(), bgra.len()) };
    Some(bitmap)
}

struct Frames {
    dpi: u32,
    size: i32,
    art: Vec<Bitmap>,
    unlocked: Bitmap,
}

impl Frames {
    fn build(dpi: u32) -> Option<Self> {
        let size = art_px(dpi);
        let source = usize::from(size > BASE_PX);
        let mut art = Vec::with_capacity(ART.len());
        let mut idle = Vec::new();
        for (i, pngs) in ART.iter().enumerate() {
            let png = image::load_from_memory_with_format(pngs[source], image::ImageFormat::Png)
                .ok()?
                .to_rgba8();
            let png = if png.width() == size {
                png
            } else {
                imageops::resize(&png, size, size, FilterType::Lanczos3)
            };
            let bgra = to_bgra(png.as_raw());
            art.push(bitmap(&bgra, size)?);
            if i == 0 {
                idle = bgra;
            }
        }
        on_card(&mut idle, size, (2 * dpi).div_ceil(96));
        Some(Self {
            dpi,
            size: size as i32,
            art,
            unlocked: bitmap(&idle, size)?,
        })
    }

    fn get(&self, image: Image) -> &Bitmap {
        match image {
            Image::Frame(frame) => &self.art[frame as usize],
            Image::Unlocked => &self.unlocked,
        }
    }
}

fn monitor(at: POINT, fallback_primary: bool) -> (RECT, u32) {
    let flags = if fallback_primary {
        MONITOR_DEFAULTTOPRIMARY
    } else {
        MONITOR_DEFAULTTONEAREST
    };
    unsafe {
        let monitor = MonitorFromPoint(at, flags);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
        (info.rcWork, dpi)
    }
}

struct Overlay {
    hwnd: HWND,
    dc: HDC,
    blank: HGDIOBJ,
    frames: Option<Frames>,
    hooks: Vec<HHOOK>,
    shown: Option<Image>,
    unlocked: bool,
    paws: Paws,
    last_draw: u64,
}

impl Overlay {
    fn visible(&self) -> bool {
        self.frames.is_some()
    }

    fn sync(&mut self) {
        let (on, saved) = settings::read(|s| (s.overlay, s.overlay_pos));
        if !on {
            self.hide();
            return;
        }
        let at = if self.visible() {
            Some(self.origin())
        } else {
            saved.map(|[x, y]| POINT {
                x: x as i32,
                y: y as i32,
            })
        };
        self.show_at(at);
        self.set_unlocked(MOVING.load(Ordering::Relaxed));
    }

    fn origin(&self) -> POINT {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        POINT {
            x: rect.left,
            y: rect.top,
        }
    }

    fn show_at(&mut self, at: Option<POINT>) -> Option<POINT> {
        let (work, dpi) = monitor(at.unwrap_or_default(), at.is_none());
        if self.frames.as_ref().is_none_or(|f| f.dpi != dpi) {
            self.release_frames();
            self.frames = Some(Frames::build(dpi)?);
        }
        let size = self.frames.as_ref()?.size;
        let margin = MARGIN_PX * dpi as i32 / 96;
        let pos = place(at, size, work, margin);
        let image = self.shown.unwrap_or(Image::Frame(Frame::Idle));
        self.draw(image, Some(pos));
        if !VISIBLE.swap(true, Ordering::Relaxed) {
            self.hook();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        Some(pos)
    }

    fn hide(&mut self) {
        for hook in self.hooks.drain(..) {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.set_unlocked(false);
        self.release_frames();
        self.shown = None;
        self.paws = Paws::default();
        SIDES.store(0, Ordering::Relaxed);
        VISIBLE.store(false, Ordering::Relaxed);
    }

    fn hook(&mut self) {
        let module = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
        for (id, hook) in [
            (WH_KEYBOARD_LL, keyboard_hook as _),
            (WH_MOUSE_LL, mouse_hook as _),
        ] {
            if let Ok(handle) = unsafe { SetWindowsHookExW(id, Some(hook), module, 0) } {
                self.hooks.push(handle);
            }
        }
    }

    fn set_unlocked(&mut self, unlocked: bool) {
        if self.unlocked == unlocked {
            return;
        }
        self.unlocked = unlocked;
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let transparent = WS_EX_TRANSPARENT.0 as isize;
            let style = if unlocked {
                style & !transparent
            } else {
                style | transparent
            };
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style);
        }
    }

    /// A bitmap still selected into the DC cannot be deleted.
    fn release_frames(&mut self) {
        if !self.blank.is_invalid() {
            unsafe { SelectObject(self.dc, self.blank) };
        }
        self.frames = None;
    }

    fn draw(&mut self, image: Image, pos: Option<POINT>) {
        let Some(frames) = &self.frames else { return };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE {
            cx: frames.size,
            cy: frames.size,
        };
        unsafe {
            let old = SelectObject(self.dc, frames.get(image).0.into());
            if self.blank.is_invalid() {
                self.blank = old;
            }
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                pos.as_ref().map(|p| p as *const _),
                Some(&size),
                Some(self.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
        self.shown = Some(image);
    }

    fn moved(&mut self) {
        if let Some(pos) = self.show_at(Some(self.origin())) {
            settings::update(|s| s.overlay_pos = Some([pos.x as f32, pos.y as f32]));
        }
    }

    /// How long the thread may sleep; None = until the next message.
    fn tick(&mut self, now: u64) -> Option<u64> {
        if SYNC.swap(false, Ordering::Acquire) {
            self.sync();
        }
        if !self.visible() {
            return None;
        }
        if MOVED.swap(false, Ordering::Relaxed) {
            self.moved();
        }
        if CHEER.swap(false, Ordering::Acquire) {
            self.paws.cheer(now);
        }
        self.paws.input(SIDES.swap(0, Ordering::Relaxed), now);

        let target = if self.unlocked {
            Image::Unlocked
        } else {
            Image::Frame(self.paws.frame(now))
        };
        let mut wake = self.paws.next_change(now);
        if self.shown != Some(target) {
            let due = self.last_draw + FRAME_MS;
            if now >= due {
                self.draw(target, None);
                self.last_draw = now;
            } else {
                wake = Some(wake.map_or(due, |w| w.min(due)));
            }
        }
        wake.map(|w| w.saturating_sub(now))
    }
}

/// Own LL hooks, installed only while the rabbit shows: a hook call is delivered as a sent
/// message, which wakes the message wait, so the callbacks stay atomic-only (no PostMessage)
/// and an idle desk means no wakeups.
fn spawn() {
    std::thread::Builder::new()
        .name("overlay".into())
        .spawn(|| unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let Ok(instance) = GetModuleHandleW(None) else {
                return;
            };
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                CLASS,
                w!("Tippy Taps rabbit"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            ) else {
                STARTED.store(false, Ordering::Release);
                return;
            };
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return;
            }
            WINDOW.store(hwnd.0 as isize, Ordering::Release);

            let mut overlay = Overlay {
                hwnd,
                dc,
                blank: HGDIOBJ::default(),
                frames: None,
                hooks: Vec::new(),
                shown: None,
                unlocked: false,
                paws: Paws::default(),
                last_draw: 0,
            };
            let start = Instant::now();
            let mut msg = MSG::default();
            loop {
                let now = start.elapsed().as_millis() as u64;
                let timeout = overlay
                    .tick(now)
                    .map_or(u32::MAX, |ms| ms.min(u32::MAX as u64 - 1) as u32);
                MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        })
        .expect("spawn overlay thread");
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST if moving() => LRESULT(HTCAPTION as isize),
        WM_SETCURSOR if moving() => {
            if let Ok(cursor) = unsafe { LoadCursorW(None, IDC_SIZEALL) } {
                unsafe { SetCursor(Some(cursor)) };
            }
            LRESULT(1)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_EXITSIZEMOVE => {
            MOVED.store(true, Ordering::Relaxed);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            SYNC.store(true, Ordering::Release);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_drum_the_paw_on_their_side() {
        for scan in [
            0x10, 0x1e, 0x2c, 0x22, 0x02, 0x01, 0x0f, 0x3a, 0x2a, 0x1d, 0x38,
        ] {
            assert_eq!(key_side(scan, false), LEFT, "scan {scan:#x}");
        }
        assert_eq!(key_side(0x5b, true), LEFT, "left Windows key");
        for scan in [0x15, 0x23, 0x31, 0x1c, 0x36, 0x0e, 0x44] {
            assert_eq!(key_side(scan, false), RIGHT, "scan {scan:#x}");
        }
        assert_eq!(key_side(0x1d, true), RIGHT, "right Ctrl");
        assert_eq!(key_side(0x38, true), RIGHT, "right Alt");
        assert_eq!(key_side(0x48, true), RIGHT, "arrow up");
        assert_eq!(key_side(0x39, false), BOTH, "space");
    }

    #[test]
    fn mouse_buttons_drum_their_side() {
        assert_eq!(mouse_side(WM_LBUTTONDOWN), LEFT);
        assert_eq!(mouse_side(WM_RBUTTONDOWN), RIGHT);
        assert_eq!(mouse_side(WM_MOUSEWHEEL), BOTH);
        assert_eq!(mouse_side(WM_KEYDOWN), 0);
    }

    #[test]
    fn paws_go_down_on_input_and_back_up() {
        let mut paws = Paws::default();
        assert_eq!(paws.frame(0), Frame::Idle);
        assert_eq!(paws.next_change(0), None);

        paws.input(LEFT, 100);
        assert_eq!(paws.frame(100), Frame::Left);
        paws.input(RIGHT, 150);
        assert_eq!(paws.frame(150), Frame::Both);
        assert_eq!(paws.next_change(150), Some(100 + HOLD_MS));
        assert_eq!(paws.frame(100 + HOLD_MS), Frame::Right);
        assert_eq!(paws.frame(150 + HOLD_MS), Frame::Idle);
        assert_eq!(paws.next_change(150 + HOLD_MS), None);

        paws.input(0, 1_000);
        assert_eq!(paws.frame(1_000), Frame::Idle);
        paws.cheer(1_000);
        paws.input(BOTH, 1_100);
        assert_eq!(paws.frame(1_100), Frame::Cheer);
        assert_eq!(paws.frame(1_000 + CHEER_MS), Frame::Idle);
    }

    #[test]
    fn art_scales_with_dpi() {
        assert_eq!(art_px(96), 128);
        assert_eq!(art_px(144), 192);
        assert_eq!(art_px(192), 256);
    }

    #[test]
    fn premultiplies_into_bgra() {
        assert_eq!(to_bgra(&[255, 128, 0, 255]), [0, 128, 255, 255]);
        assert_eq!(to_bgra(&[255, 255, 255, 128]), [128, 128, 128, 128]);
        assert_eq!(to_bgra(&[10, 20, 30, 0]), [0, 0, 0, 0]);

        let mut px = to_bgra(&[0, 0, 0, 0].repeat(9));
        on_card(&mut px, 3, 1);
        assert_eq!(&px[0..4], to_bgra(&CARD_EDGE).as_slice());
        assert_eq!(&px[16..20], to_bgra(&CARD).as_slice(), "the middle pixel");
        assert!(
            px.chunks(4).all(|p| p[3] > 0),
            "every pixel catches the mouse"
        );
    }

    #[test]
    fn placement_stays_on_screen() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let p = place(None, 128, work, 8);
        assert_eq!((p.x, p.y), (1784, 904));
        let p = place(Some(POINT { x: 100, y: 200 }), 128, work, 8);
        assert_eq!((p.x, p.y), (100, 200));
        let p = place(Some(POINT { x: 5000, y: -50 }), 128, work, 8);
        assert_eq!((p.x, p.y), (1792, 0));
    }
}

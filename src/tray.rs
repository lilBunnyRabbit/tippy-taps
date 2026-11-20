use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    FindWindowW, GetMessageW, MSG, PostMessageW, RegisterClassW, SetTimer, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_ENDSESSION, WM_QUERYENDSESSION, WM_TIMER, WNDCLASSW,
};
use windows::core::w;

use crate::counts::{self, Counts};
use crate::{autostart, clock, storage, ui};

const CLASS: windows::core::PCWSTR = w!("tippy-taps-instance");
const WM_SHOW: u32 = WM_APP + 1;
const WM_SYNC: u32 = WM_APP + 2;
const WM_QUIT_APP: u32 = WM_APP + 3;
const REFRESH_TIMER: usize = 1;
const REFRESH_MS: u32 = 2000;
const ICON_SIZE: u32 = 32;
const FIND_ATTEMPTS: u32 = 30;
const FIND_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

static CTX: OnceLock<egui::Context> = OnceLock::new();
static OPEN: OnceLock<Mutex<Sender<()>>> = OnceLock::new();
static WINDOW: AtomicIsize = AtomicIsize::new(0);

struct Tray {
    icon: TrayIcon,
    autostart: CheckMenuItem,
    tooltip: String,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// Retries for a moment: a quick double launch can get here before the first instance has
/// created its tray window.
pub fn show_running_instance() -> bool {
    for _ in 0..FIND_ATTEMPTS {
        if let Ok(hwnd) = unsafe { FindWindowW(CLASS, None) } {
            unsafe {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                return PostMessageW(Some(hwnd), WM_SHOW, WPARAM(0), LPARAM(0)).is_ok();
            }
        }
        std::thread::sleep(FIND_RETRY);
    }
    false
}

pub fn sync_autostart() {
    post(WM_SYNC);
}

pub fn attach(ctx: egui::Context) {
    let _ = CTX.set(ctx);
}

/// Own thread and message loop: menu, tooltip, second-launch and session-end messages must work
/// while the egui window is hidden or not created yet. `open` asks main to create the window.
pub fn spawn(open: Sender<()>) {
    let _ = OPEN.set(Mutex::new(open));
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(|| unsafe {
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
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("tippy-taps"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            ) else {
                return;
            };
            WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);

            match build() {
                Ok(tray) => TRAY.with(|t| *t.borrow_mut() = Some(tray)),
                Err(err) => eprintln!("tray icon: {err}"),
            }
            let _ = SetTimer(Some(hwnd), REFRESH_TIMER, REFRESH_MS, None);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("spawn tray thread");
}

fn build() -> Result<Tray, Box<dyn std::error::Error>> {
    let open = MenuItem::new("Open", true, None);
    let autostart = CheckMenuItem::new("Start with Windows", true, autostart::enabled(), None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::with_items(&[&open, &autostart, &PredefinedMenuItem::separator(), &quit])?;

    let tooltip = current_tooltip();
    let icon = TrayIconBuilder::new()
        .with_icon(icon()?)
        .with_tooltip(&tooltip)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;

    let (open_id, autostart_id, quit_id) =
        (open.id().clone(), autostart.id().clone(), quit.id().clone());
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == open_id {
            show();
        } else if event.id == autostart_id {
            let _ = autostart::set(!autostart::enabled());
            post(WM_SYNC);
        } else if event.id == quit_id {
            post(WM_QUIT_APP);
        }
    }));
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            show();
        }
    }));

    Ok(Tray {
        icon,
        autostart,
        tooltip,
    })
}

fn icon() -> Result<tray_icon::Icon, Box<dyn std::error::Error>> {
    let png = image::load_from_memory_with_format(
        include_bytes!("../assets/icon.png"),
        image::ImageFormat::Png,
    )?;
    let small = image::imageops::resize(
        &png.to_rgba8(),
        ICON_SIZE,
        ICON_SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    Ok(tray_icon::Icon::from_rgba(
        small.into_raw(),
        ICON_SIZE,
        ICON_SIZE,
    )?)
}

fn current_tooltip() -> String {
    let storage = storage::get();
    let mut c = storage.day(clock::today());
    c += counts::pending();
    tooltip(c, storage.saving_blocked())
}

pub fn tooltip(c: Counts, saving_blocked: bool) -> String {
    let mut text = format!(
        "Today: {} keys · {} clicks · {} scrolls",
        counts::group(c.keys),
        counts::group(c.clicks),
        counts::group(c.scrolls)
    );
    if saving_blocked {
        text.push_str(
            "
Not saving: can't open counts.json, retrying",
        );
    }
    text
}

fn show() {
    if let Some(ctx) = CTX.get() {
        ui::show(ctx);
    } else if let Some(open) = OPEN.get() {
        let _ = open.lock().map(|tx| tx.send(()));
    }
}

fn post(message: u32) {
    let hwnd = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
    if !hwnd.is_invalid() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0));
        }
    }
}

fn refresh() {
    TRAY.with(|t| {
        let mut t = t.borrow_mut();
        let Some(tray) = t.as_mut() else { return };
        let text = current_tooltip();
        if text != tray.tooltip {
            let _ = tray.icon.set_tooltip(Some(&text));
            tray.tooltip = text;
        }
        let on = autostart::enabled();
        if tray.autostart.is_checked() != on {
            tray.autostart.set_checked(on);
        }
    });
}

fn quit() -> ! {
    storage::flush(clock::today());
    // Dropping the icon removes it now; process::exit alone would leave a ghost icon in the tray.
    TRAY.with(|t| drop(t.borrow_mut().take()));
    std::process::exit(0);
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SHOW => show(),
        WM_SYNC | WM_TIMER => refresh(),
        WM_QUIT_APP => quit(),
        WM_QUERYENDSESSION => {
            storage::flush(clock::today());
            return LRESULT(1);
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                storage::flush(clock::today());
            }
        }
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_text() {
        let c = Counts {
            keys: 12345,
            clicks: 678,
            scrolls: 90,
        };
        assert_eq!(
            tooltip(c, false),
            "Today: 12,345 keys · 678 clicks · 90 scrolls"
        );
        assert_eq!(
            tooltip(c, true),
            "Today: 12,345 keys · 678 clicks · 90 scrolls
Not saving: can't open counts.json, retrying"
        );
        assert!(tooltip(c, true).encode_utf16().count() < 128);
    }
}

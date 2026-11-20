use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
};

use crate::counts::{self, CLICKS, HeldKeys, KEYS, SCROLLS};

static HELD: HeldKeys = counts::held_keys();
static ERROR: OnceLock<String> = OnceLock::new();

pub fn error() -> Option<&'static str> {
    ERROR.get().map(String::as_str)
}

/// Installs the low-level hooks on a dedicated thread that does nothing but pump their messages,
/// so a busy UI can never make Windows drop the hooks for being slow.
pub fn spawn() {
    std::thread::Builder::new()
        .name("hooks".into())
        .spawn(|| unsafe {
            let module = GetModuleHandleW(None).ok().map(Into::into);
            let installed = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0)
                .and_then(|_| SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0));
            if let Err(err) = installed {
                let _ = ERROR.set(format!("Can't watch input: {err}"));
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("spawn hook thread");
}

const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;
const HID_USAGE_GENERIC_KEYBOARD: u16 = 0x06;

/// winit registers this process for raw keyboard input, and while one of its windows has focus
/// Windows then skips the process's low-level keyboard hook, so keys typed into our own window
/// went uncounted. egui needs no raw device events, so the registration is dropped.
pub fn drop_raw_input() {
    let devices =
        [HID_USAGE_GENERIC_MOUSE, HID_USAGE_GENERIC_KEYBOARD].map(|usage| RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: usage,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: Default::default(),
        });
    unsafe {
        let _ = RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32);
    }
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = info.flags.0 & LLKHF_INJECTED.0 != 0;
        match wparam.0 as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN if !injected => {
                if counts::press(&HELD, info.vkCode) {
                    KEYS.fetch_add(1, Ordering::Relaxed);
                }
            }
            // Releases always clear the held bit, so an injected key-up cannot leave a key stuck.
            WM_KEYUP | WM_SYSKEYUP => counts::release(&HELD, info.vkCode),
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam.0 as u32;
        if matches!(
            message,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_MOUSEWHEEL
                | WM_MOUSEHWHEEL
        ) {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if info.flags & LLMHF_INJECTED == 0 {
                if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
                    let delta = (info.mouseData >> 16) as u16 as i16;
                    SCROLLS.fetch_add(counts::notches(delta), Ordering::Relaxed);
                } else {
                    CLICKS.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

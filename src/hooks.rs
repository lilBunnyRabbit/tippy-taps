use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SetWindowsHookExW, TranslateMessage,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_XBUTTONDOWN, XBUTTON2,
};

use crate::counts::{self, Field, HeldKeys};
use crate::{keyboard, mouse};

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
                    counts::add(Field::keys, 1);
                    keyboard::press(info.scanCode, info.flags.0 & LLKHF_EXTENDED.0 != 0);
                }
            }
            // Releases always clear the held bit, so an injected key-up cannot leave a key stuck.
            WM_KEYUP | WM_SYSKEYUP => counts::release(&HELD, info.vkCode),
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The per-button count for a mouse message. `clicks` is derived from these when flushing.
fn click_field(message: u32, mouse_data: u32) -> Option<Field> {
    match message {
        WM_LBUTTONDOWN => Some(Field::m_left),
        WM_RBUTTONDOWN => Some(Field::m_right),
        WM_MBUTTONDOWN => Some(Field::m_middle),
        WM_XBUTTONDOWN if (mouse_data >> 16) as u16 == XBUTTON2 => Some(Field::m_x2),
        WM_XBUTTONDOWN => Some(Field::m_x1),
        _ => None,
    }
}

unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam.0 as u32;
        if matches!(
            message,
            WM_MOUSEMOVE
                | WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_MOUSEWHEEL
                | WM_MOUSEHWHEEL
        ) {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if info.flags & LLMHF_INJECTED == 0 {
                if message == WM_MOUSEMOVE {
                    mouse::moved(info.pt.x, info.pt.y);
                } else if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
                    let delta = (info.mouseData >> 16) as u16 as i16;
                    counts::add(Field::scrolls, counts::notches(delta));
                } else if let Some(field) = click_field(message, info.mouseData) {
                    counts::add(field, 1);
                    mouse::clicked(info.pt.x, info.pt.y);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_map_to_their_button() {
        assert_eq!(click_field(WM_LBUTTONDOWN, 0), Some(Field::m_left));
        assert_eq!(click_field(WM_RBUTTONDOWN, 0), Some(Field::m_right));
        assert_eq!(click_field(WM_MBUTTONDOWN, 0), Some(Field::m_middle));
        assert_eq!(click_field(WM_XBUTTONDOWN, 1 << 16), Some(Field::m_x1));
        assert_eq!(click_field(WM_XBUTTONDOWN, 2 << 16), Some(Field::m_x2));
        assert_eq!(click_field(WM_MOUSEWHEEL, 120 << 16), None);
    }
}

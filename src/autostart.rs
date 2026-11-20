use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::w;

static ENABLED: AtomicBool = AtomicBool::new(false);

const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: windows::core::PCWSTR = w!("tippy-taps");

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turns autostart on for a first run, and points an existing entry at the current exe so a
/// moved exe heals itself.
pub fn init(first_run: bool) {
    let on = first_run || is_registered();
    let _ = set(on);
}

pub fn set(on: bool) -> windows::core::Result<()> {
    let result = if on { register() } else { unregister() };
    ENABLED.store(is_registered(), Ordering::Relaxed);
    result
}

pub fn command(exe: &Path) -> String {
    format!("\"{}\" --background", exe.display())
}

fn register() -> windows::core::Result<()> {
    let exe = std::env::current_exe()?;
    let wide: Vec<u16> = command(&exe).encode_utf16().chain([0]).collect();
    let bytes: Vec<u8> = wide.iter().flat_map(|c| c.to_le_bytes()).collect();
    let key = RunKey::open(KEY_SET_VALUE)?;
    unsafe { RegSetValueExW(key.0, VALUE, None, REG_SZ, Some(&bytes)) }.ok()
}

fn unregister() -> windows::core::Result<()> {
    let key = RunKey::open(KEY_SET_VALUE)?;
    match unsafe { RegDeleteValueW(key.0, VALUE) } {
        ERROR_FILE_NOT_FOUND => Ok(()),
        err => err.ok(),
    }
}

fn is_registered() -> bool {
    let Ok(key) = RunKey::open(KEY_QUERY_VALUE) else {
        return false;
    };
    let status = unsafe { RegGetValueW(key.0, None, VALUE, RRF_RT_REG_SZ, None, None, None) };
    status == ERROR_SUCCESS
}

struct RunKey(HKEY);

impl RunKey {
    fn open(access: REG_SAM_FLAGS) -> windows::core::Result<Self> {
        let mut key = HKEY::default();
        let status: WIN32_ERROR =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, access, &mut key) };
        status.ok()?;
        Ok(Self(key))
    }
}

impl Drop for RunKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_the_path() {
        assert_eq!(
            command(Path::new(r"C:\Program Files\tippy-taps\tippy-taps.exe")),
            r#""C:\Program Files\tippy-taps\tippy-taps.exe" --background"#
        );
    }
}

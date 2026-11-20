use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyNameTextW, GetKeyboardLayout, GetKeyboardLayoutList, HKL, MAPVK_VK_TO_CHAR,
    MAPVK_VSC_TO_VK_EX, MapVirtualKeyExW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{HSTRING, PCWSTR, w};

use crate::keyboard::Shape;

/// One keycap, in key units from the top-left of the board.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub slot: usize,
    /// Another slot the same key can report, summed into it.
    pub alias: Option<usize>,
}

pub const BOARD_WIDTH: f32 = 22.5;
pub const BOARD_HEIGHT: f32 = 6.25;
const GAP: u16 = 0;
const TALL: f32 = 2.0;

type Item = (u16, f32, f32);

const fn k(slot: u16) -> Item {
    (slot, 1.0, 1.0)
}

const fn wide(slot: u16, w: f32) -> Item {
    (slot, w, 1.0)
}

const fn gap(w: f32) -> Item {
    (GAP, w, 1.0)
}

const F_ROW: [Item; 20] = [
    k(0x01),
    gap(1.0),
    k(0x3b),
    k(0x3c),
    k(0x3d),
    k(0x3e),
    gap(0.5),
    k(0x3f),
    k(0x40),
    k(0x41),
    k(0x42),
    gap(0.5),
    k(0x43),
    k(0x44),
    k(0x57),
    k(0x58),
    gap(0.25),
    k(0x137),
    k(0x46),
    k(0x45),
];

const SIDE: [&[Item]; 5] = [
    &[
        k(0x152),
        k(0x147),
        k(0x149),
        gap(0.25),
        k(0x145),
        k(0x135),
        k(0x37),
        k(0x4a),
    ],
    &[
        k(0x153),
        k(0x14f),
        k(0x151),
        gap(0.25),
        k(0x47),
        k(0x48),
        k(0x49),
        (0x4e, 1.0, TALL),
    ],
    &[gap(3.25), k(0x4b), k(0x4c), k(0x4d)],
    &[
        gap(1.0),
        k(0x148),
        gap(1.0),
        gap(0.25),
        k(0x4f),
        k(0x50),
        k(0x51),
        (0x11c, 1.0, TALL),
    ],
    &[
        k(0x14b),
        k(0x150),
        k(0x14d),
        gap(0.25),
        wide(0x52, 2.0),
        k(0x53),
    ],
];

fn run(from: u16, to: u16) -> impl Iterator<Item = Item> {
    (from..=to).map(k)
}

fn main_rows(shape: Shape) -> [Vec<Item>; 5] {
    let iso_enter = (0x1c, 1.25, TALL);
    let digits: Vec<Item> = std::iter::once(k(0x29)).chain(run(0x02, 0x0d)).collect();
    let bottom = |space: f32| -> Vec<Item> {
        match shape {
            Shape::Jis => vec![
                wide(0x1d, 1.25),
                k(0x15b),
                wide(0x38, 1.25),
                k(0x7b),
                wide(0x39, space),
                k(0x79),
                k(0x70),
                wide(0x138, 1.25),
                k(0x15c),
                k(0x15d),
                wide(0x11d, 1.5),
            ],
            _ => vec![
                wide(0x1d, 1.25),
                wide(0x15b, 1.25),
                wide(0x38, 1.25),
                wide(0x39, space),
                wide(0x138, 1.25),
                wide(0x15c, 1.25),
                wide(0x15d, 1.25),
                wide(0x11d, 1.25),
            ],
        }
    };
    let cat = |parts: &[&[Item]]| parts.concat();
    let letters_top: Vec<Item> = run(0x10, 0x1b).collect();
    let letters_mid: Vec<Item> = run(0x1e, 0x28).collect();
    let letters_low: Vec<Item> = run(0x2c, 0x35).collect();
    match shape {
        Shape::Ansi => [
            cat(&[&digits, &[wide(0x0e, 2.0)]]),
            cat(&[&[wide(0x0f, 1.5)], &letters_top, &[wide(0x2b, 1.5)]]),
            cat(&[&[wide(0x3a, 1.75)], &letters_mid, &[wide(0x1c, 2.25)]]),
            cat(&[&[wide(0x2a, 2.25)], &letters_low, &[wide(0x36, 2.75)]]),
            bottom(6.25),
        ],
        Shape::Iso | Shape::Jis => {
            let jis = shape == Shape::Jis;
            [
                if jis {
                    cat(&[&digits, &[k(0x7d), k(0x0e)]])
                } else {
                    cat(&[&digits, &[wide(0x0e, 2.0)]])
                },
                cat(&[&[wide(0x0f, 1.5)], &letters_top, &[gap(0.25), iso_enter]]),
                cat(&[&[wide(0x3a, 1.75)], &letters_mid, &[k(0x2b), gap(1.25)]]),
                if jis {
                    cat(&[
                        &[wide(0x2a, 2.25)],
                        &letters_low,
                        &[k(0x73), wide(0x36, 1.75)],
                    ])
                } else {
                    cat(&[
                        &[wide(0x2a, 1.25), k(0x56)],
                        &letters_low,
                        &[wide(0x36, 2.75)],
                    ])
                },
                bottom(if jis { 3.75 } else { 6.25 }),
            ]
        }
    }
}

/// Some keys arrive under a second slot depending on the driver or modifiers.
fn alias(slot: u16) -> Option<usize> {
    match slot {
        0x36 => Some(0x136),
        0x137 => Some(0x54),
        _ => None,
    }
}

pub fn board(shape: Shape) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut place = |items: &[Item], x0: f32, y: f32| {
        let mut x = x0;
        for &(slot, w, h) in items {
            if slot != GAP {
                keys.push(Key {
                    x,
                    y,
                    w,
                    h,
                    slot: usize::from(slot),
                    alias: alias(slot),
                });
            }
            x += w;
        }
    };
    place(&F_ROW, 0.0, 0.0);
    for (row, items) in main_rows(shape).iter().enumerate() {
        let y = 1.25 + row as f32;
        place(items, 0.0, y);
        place(SIDE[row], 15.25, y);
    }
    keys
}

fn is_character(slot: usize) -> bool {
    matches!(slot, 0x02..=0x0d | 0x10..=0x1b | 0x1e..=0x29 | 0x2b..=0x35 | 0x56 | 0x73 | 0x7d)
}

fn fixed_label(slot: usize) -> &'static str {
    const F: [&str; 10] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10"];
    match slot {
        0x3b..=0x44 => F[slot - 0x3b],
        0x01 => "Esc",
        0x57 => "F11",
        0x58 => "F12",
        0x137 => "PrtSc",
        0x46 => "ScrLk",
        0x45 => "Pause",
        0x0e => "Bksp",
        0x0f => "Tab",
        0x3a => "Caps",
        0x1c => "Enter",
        0x2a | 0x36 => "Shift",
        0x1d | 0x11d => "Ctrl",
        0x15b | 0x15c => "Win",
        0x38 | 0x138 => "Alt",
        0x15d => "Menu",
        0x152 => "Ins",
        0x147 => "Home",
        0x149 => "PgUp",
        0x153 => "Del",
        0x14f => "End",
        0x151 => "PgDn",
        0x148 => "↑",
        0x150 => "↓",
        0x14b => "←",
        0x14d => "→",
        0x145 => "Num",
        0x135 => "/",
        0x37 => "*",
        0x4a => "-",
        0x4e => "+",
        0x11c => "Ent",
        0x47 => "7",
        0x48 => "8",
        0x49 => "9",
        0x4b => "4",
        0x4c => "5",
        0x4d => "6",
        0x4f => "1",
        0x50 => "2",
        0x51 => "3",
        0x52 => "0",
        0x53 => ".",
        0x7b => "Muh",
        0x79 => "Hen",
        0x70 => "Kana",
        0x73 => "Ro",
        0x7d => "¥",
        0x29 => "Han",
        _ => "",
    }
}

/// The unshifted character the layout puts on a key, upper-cased like a keycap.
fn layout_char(slot: usize, hkl: HKL) -> Option<String> {
    let scan = (slot & 0xff) as u32 | if slot & 0x100 != 0 { 0xe000 } else { 0 };
    let vk = unsafe { MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, Some(hkl)) };
    if vk == 0 {
        return None;
    }
    // Bit 31 marks a dead key; the low word is still its character.
    let ch = unsafe { MapVirtualKeyExW(vk, MAPVK_VK_TO_CHAR, Some(hkl)) } & 0xffff;
    char::from_u32(ch)
        .filter(|c| !c.is_control() && *c != ' ')
        .map(|c| c.to_uppercase().collect())
}

pub fn labels(keys: &[Key], hkl: Layout) -> Vec<String> {
    keys.iter()
        .map(|key| {
            is_character(key.slot)
                .then(|| layout_char(key.slot, hkl.handle()))
                .flatten()
                .unwrap_or_else(|| fixed_label(key.slot).to_string())
        })
        .collect()
}

pub fn key_name(slot: usize) -> String {
    let lparam = ((slot & 0xff) << 16 | (slot & 0x100) << 16) as i32;
    let mut buf = [0u16; 64];
    let len = unsafe { GetKeyNameTextW(lparam, &mut buf) };
    match String::from_utf16_lossy(&buf[..len.max(0) as usize]) {
        name if name.is_empty() => fixed_label(slot).to_string(),
        name => name,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout(pub usize);

impl Layout {
    fn handle(self) -> HKL {
        HKL(self.0 as *mut _)
    }

    pub fn langid(self) -> u16 {
        self.0 as u16
    }

    pub fn to_setting(self) -> String {
        format!("{:x}", self.0)
    }

    pub fn from_setting(text: &str) -> Option<Self> {
        usize::from_str_radix(text, 16).ok().map(Self)
    }

    /// The layout of the window being typed into, so the legends follow layout switches.
    pub fn active() -> Self {
        let hkl = unsafe {
            let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
            GetKeyboardLayout(thread)
        };
        Self(hkl.0 as usize)
    }

    pub fn installed() -> Vec<Self> {
        let count = unsafe { GetKeyboardLayoutList(None) };
        let mut list = vec![HKL::default(); count.max(0) as usize];
        let got = unsafe { GetKeyboardLayoutList(Some(&mut list)) };
        list.truncate(got.max(0) as usize);
        list.into_iter().map(|h| Self(h.0 as usize)).collect()
    }

    /// The layout's name from the registry ("German", "United States-International"), or its
    /// HKL when the registry has none.
    pub fn name(self) -> String {
        let klid = klid(self.0);
        klid.and_then(|klid| reg_string(&format!(r"{LAYOUTS}\{klid}"), w!("Layout Text")))
            .unwrap_or_else(|| format!("Layout {:08X}", self.0 as u32))
    }
}

const LAYOUTS: &str = r"SYSTEM\CurrentControlSet\Control\Keyboard Layouts";

/// An HKL's high word is the layout: a KLID directly, or with 0xF in its top nibble, an index
/// matched against each layout's "Layout Id".
fn klid(hkl: usize) -> Option<String> {
    let device = (hkl >> 16) as u16;
    if device & 0xf000 != 0xf000 {
        return Some(format!("{device:08x}"));
    }
    let id = format!("{:04x}", device & 0x0fff);
    layout_keys().into_iter().find(|klid| {
        reg_string(&format!(r"{LAYOUTS}\{klid}"), w!("Layout Id"))
            .is_some_and(|v| v.eq_ignore_ascii_case(&id))
    })
}

fn layout_keys() -> Vec<String> {
    let mut key = HKEY::default();
    let path = HSTRING::from(LAYOUTS);
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &path, None, KEY_READ, &mut key) }
        != ERROR_SUCCESS
    {
        return Vec::new();
    }
    let mut names = Vec::new();
    for index in 0.. {
        let mut buf = [0u16; 64];
        let mut len = buf.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(windows::core::PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    names
}

fn reg_string(path: &str, value: PCWSTR) -> Option<String> {
    let path = HSTRING::from(path);
    let mut buf = [0u16; 128];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &path,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then(|| {
        let len = (size as usize / 2).saturating_sub(1);
        String::from_utf16_lossy(&buf[..len])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where each main-block row ends; a tall key closes the row below it too.
    fn rows(keys: &[Key]) -> Vec<f32> {
        (0..5)
            .map(|row| {
                let y = 1.25 + row as f32;
                keys.iter()
                    .filter(|k| k.x < 15.0 && k.y <= y && y < k.y + k.h)
                    .map(|k| k.x + k.w)
                    .fold(0.0, f32::max)
            })
            .collect()
    }

    #[test]
    fn every_main_row_is_fifteen_units() {
        for shape in Shape::ALL {
            let keys = board(shape);
            assert_eq!(rows(&keys), [15.0; 5], "{shape:?}");
            let right = keys.iter().map(|k| k.x + k.w).fold(0.0, f32::max);
            assert_eq!(right, BOARD_WIDTH, "{shape:?}");
            let bottom = keys.iter().map(|k| k.y + k.h).fold(0.0, f32::max);
            assert_eq!(bottom, BOARD_HEIGHT, "{shape:?}");
        }
    }

    #[test]
    fn shapes_differ_in_their_extra_keys() {
        let has = |shape, slot| board(shape).iter().any(|k| k.slot == slot);
        assert!(!has(Shape::Ansi, 0x56) && has(Shape::Iso, 0x56));
        assert!(has(Shape::Ansi, 0x2b) && has(Shape::Iso, 0x2b));
        for jis in [0x73, 0x7d, 0x79, 0x7b, 0x70] {
            assert!(has(Shape::Jis, jis) && !has(Shape::Iso, jis));
        }
        assert_eq!(board(Shape::Ansi).len(), 104);
        assert_eq!(board(Shape::Iso).len(), 105);
        assert_eq!(board(Shape::Jis).len(), 109);
    }

    #[test]
    fn slots_are_unique() {
        for shape in Shape::ALL {
            let mut slots: Vec<usize> = board(shape).iter().map(|k| k.slot).collect();
            slots.sort();
            let n = slots.len();
            slots.dedup();
            assert_eq!(slots.len(), n, "{shape:?}");
        }
    }

    #[test]
    fn hkl_setting_round_trips() {
        let hkl = Layout(0xffff_ffff_f002_0409);
        assert_eq!(Layout::from_setting(&hkl.to_setting()), Some(hkl));
        assert_eq!(hkl.langid(), 0x0409);
        assert_eq!(klid(0x0409_0409).as_deref(), Some("00000409"));
        assert_eq!(klid(0x0407_0424).as_deref(), Some("00000407"));
    }
}

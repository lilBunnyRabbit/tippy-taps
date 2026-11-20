use std::ops::AddAssign;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub static KEYS: AtomicU64 = AtomicU64::new(0);
pub static CLICKS: AtomicU64 = AtomicU64::new(0);
pub static SCROLLS: AtomicU64 = AtomicU64::new(0);

const WHEEL_DELTA: u32 = 120;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    #[serde(default)]
    pub keys: u64,
    #[serde(default)]
    pub clicks: u64,
    #[serde(default)]
    pub scrolls: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.keys + self.clicks + self.scrolls
    }

    pub fn is_zero(&self) -> bool {
        self.total() == 0
    }
}

impl AddAssign for Counts {
    fn add_assign(&mut self, other: Self) {
        self.keys += other.keys;
        self.clicks += other.clicks;
        self.scrolls += other.scrolls;
    }
}

pub fn pending() -> Counts {
    Counts {
        keys: KEYS.load(Ordering::Relaxed),
        clicks: CLICKS.load(Ordering::Relaxed),
        scrolls: SCROLLS.load(Ordering::Relaxed),
    }
}

pub fn take() -> Counts {
    Counts {
        keys: KEYS.swap(0, Ordering::Relaxed),
        clicks: CLICKS.swap(0, Ordering::Relaxed),
        scrolls: SCROLLS.swap(0, Ordering::Relaxed),
    }
}

pub type HeldKeys = [AtomicU64; 4];

pub const fn held_keys() -> HeldKeys {
    [const { AtomicU64::new(0) }; 4]
}

/// Marks `vk` as held and returns true only on an up-to-down transition, so auto-repeat is not counted.
pub fn press(held: &HeldKeys, vk: u32) -> bool {
    let (word, bit) = slot(vk);
    held[word].fetch_or(bit, Ordering::Relaxed) & bit == 0
}

pub fn release(held: &HeldKeys, vk: u32) {
    let (word, bit) = slot(vk);
    held[word].fetch_and(!bit, Ordering::Relaxed);
}

fn slot(vk: u32) -> (usize, u64) {
    let vk = vk & 0xff;
    ((vk / 64) as usize, 1 << (vk % 64))
}

/// Wheel notches in one event. Smooth-scroll mice send deltas below 120, which still count as one.
pub fn notches(delta: i16) -> u64 {
    u64::from((i32::from(delta).unsigned_abs() / WHEEL_DELTA).max(1))
}

pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_is_not_counted() {
        let held = held_keys();
        assert!(press(&held, 0x41));
        assert!(!press(&held, 0x41));
        assert!(!press(&held, 0x41));
        release(&held, 0x41);
        assert!(press(&held, 0x41));
    }

    #[test]
    fn keys_are_tracked_separately() {
        let held = held_keys();
        assert!(press(&held, 0x10));
        assert!(press(&held, 0x41));
        assert!(press(&held, 0xff));
        assert!(press(&held, 0x00));
        release(&held, 0x10);
        assert!(!press(&held, 0x41));
        assert!(press(&held, 0x10));
    }

    #[test]
    fn release_without_press_is_harmless() {
        let held = held_keys();
        release(&held, 0x41);
        assert!(press(&held, 0x41));
    }

    #[test]
    fn notch_math() {
        assert_eq!(notches(120), 1);
        assert_eq!(notches(-120), 1);
        assert_eq!(notches(240), 2);
        assert_eq!(notches(-360), 3);
        assert_eq!(notches(30), 1);
        assert_eq!(notches(-1), 1);
        assert_eq!(notches(0), 1);
        assert_eq!(notches(250), 2);
        assert_eq!(notches(i16::MIN), 273);
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(12345), "12,345");
        assert_eq!(group(1234567), "1,234,567");
    }

    #[test]
    fn counts_add() {
        let mut a = Counts {
            keys: 1,
            clicks: 2,
            scrolls: 3,
        };
        a += Counts {
            keys: 10,
            clicks: 20,
            scrolls: 30,
        };
        assert_eq!(
            a,
            Counts {
                keys: 11,
                clicks: 22,
                scrolls: 33
            }
        );
        assert_eq!(a.total(), 66);
    }
}

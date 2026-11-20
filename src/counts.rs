use std::ops::AddAssign;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub static KEYS: AtomicU64 = AtomicU64::new(0);
pub static CLICKS: AtomicU64 = AtomicU64::new(0);
pub static SCROLLS: AtomicU64 = AtomicU64::new(0);
static BUTTONS: AtomicU64 = AtomicU64::new(0);
static LT_PULLS: AtomicU64 = AtomicU64::new(0);
static RT_PULLS: AtomicU64 = AtomicU64::new(0);
static LT_MS: AtomicU64 = AtomicU64::new(0);
static RT_MS: AtomicU64 = AtomicU64::new(0);

const WHEEL_DELTA: u32 = 120;
/// 3 lines per notch at 16 px per line, 96 DPI.
const MM_PER_NOTCH: f64 = 12.7;

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bucket {
    #[serde(skip_serializing_if = "is_zero")]
    pub keys: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub clicks: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub scrolls: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub buttons: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub lt_pulls: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub rt_pulls: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub lt_ms: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub rt_ms: u64,
}

impl Bucket {
    /// Every counted input. Engaged trigger time is a duration, not an input, so it stays out.
    pub fn total(&self) -> u64 {
        self.keys + self.clicks + self.scrolls + self.buttons + self.lt_pulls + self.rt_pulls
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn has_controller(&self) -> bool {
        self.buttons + self.lt_pulls + self.rt_pulls + self.lt_ms + self.rt_ms > 0
    }
}

impl AddAssign for Bucket {
    fn add_assign(&mut self, other: Self) {
        self.keys += other.keys;
        self.clicks += other.clicks;
        self.scrolls += other.scrolls;
        self.buttons += other.buttons;
        self.lt_pulls += other.lt_pulls;
        self.rt_pulls += other.rt_pulls;
        self.lt_ms += other.lt_ms;
        self.rt_ms += other.rt_ms;
    }
}

const CONTROLLER: [&AtomicU64; 5] = [&BUTTONS, &LT_PULLS, &RT_PULLS, &LT_MS, &RT_MS];

pub fn add_controller(delta: Bucket) {
    let values = [
        delta.buttons,
        delta.lt_pulls,
        delta.rt_pulls,
        delta.lt_ms,
        delta.rt_ms,
    ];
    for (counter, value) in CONTROLLER.into_iter().zip(values) {
        if value > 0 {
            counter.fetch_add(value, Ordering::Relaxed);
        }
    }
}

fn read(get: impl Fn(&AtomicU64) -> u64) -> Bucket {
    Bucket {
        keys: get(&KEYS),
        clicks: get(&CLICKS),
        scrolls: get(&SCROLLS),
        buttons: get(&BUTTONS),
        lt_pulls: get(&LT_PULLS),
        rt_pulls: get(&RT_PULLS),
        lt_ms: get(&LT_MS),
        rt_ms: get(&RT_MS),
    }
}

pub fn pending() -> Bucket {
    read(|a| a.load(Ordering::Relaxed))
}

pub fn take() -> Bucket {
    read(|a| a.swap(0, Ordering::Relaxed))
}

pub fn scroll_distance(notches: u64) -> String {
    let metres = notches as f64 * MM_PER_NOTCH / 1000.0;
    if metres < 999.95 {
        format!("≈ {metres:.1} m")
    } else {
        format!("≈ {:.2} km", metres / 1000.0)
    }
}

pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{:.1}s", ms as f64 / 1000.0),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s / 60 % 60),
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
    fn bucket_math() {
        let mut a = Bucket {
            keys: 1,
            clicks: 2,
            scrolls: 3,
            buttons: 4,
            lt_pulls: 5,
            rt_pulls: 6,
            lt_ms: 700,
            rt_ms: 800,
        };
        a += a;
        assert_eq!(a.keys, 2);
        assert_eq!(a.rt_pulls, 12);
        assert_eq!(a.rt_ms, 1600);
        assert_eq!(a.total(), 42);
        assert!(a.has_controller());

        let keys_only = Bucket {
            keys: 3,
            ..Default::default()
        };
        assert!(!keys_only.has_controller());
        assert!(!keys_only.is_empty());
        let held_only = Bucket {
            lt_ms: 5,
            ..Default::default()
        };
        assert_eq!(held_only.total(), 0);
        assert!(!held_only.is_empty());
        assert!(Bucket::default().is_empty());
    }

    #[test]
    fn bucket_omits_zero_fields() {
        let b = Bucket {
            keys: 5,
            lt_ms: 9,
            ..Default::default()
        };
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, r#"{"keys":5,"lt_ms":9}"#);
        assert_eq!(serde_json::from_str::<Bucket>(&json).unwrap(), b);
        assert_eq!(
            serde_json::from_str::<Bucket>("{}").unwrap(),
            Bucket::default()
        );
    }

    #[test]
    fn scroll_distance_units() {
        assert_eq!(scroll_distance(0), "≈ 0.0 m");
        assert_eq!(scroll_distance(100), "≈ 1.3 m");
        assert_eq!(scroll_distance(78_740), "≈ 1.00 km");
        assert_eq!(scroll_distance(100_000), "≈ 1.27 km");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(4_200), "4.2s");
        assert_eq!(duration(192_000), "3m 12s");
        assert_eq!(duration(7_500_000), "2h 05m");
    }
}

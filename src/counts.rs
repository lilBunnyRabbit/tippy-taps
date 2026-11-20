use std::ops::AddAssign;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

const WHEEL_DELTA: u32 = 120;
/// 3 lines per notch at 16 px per line, 96 DPI.
const MM_PER_NOTCH: f64 = 12.7;

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// One list of fields drives the struct, its serde form and the pending counters, so a new count
/// is one line here.
macro_rules! bucket {
    ($($field:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct Bucket {
            $(
                #[serde(skip_serializing_if = "is_zero")]
                pub $field: u64,
            )*
        }

        #[allow(non_camel_case_types, dead_code)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Field {
            $($field),*
        }

        impl Bucket {
            pub const FIELDS: usize = [$(Field::$field),*].len();

            pub fn to_array(self) -> [u64; Self::FIELDS] {
                [$(self.$field),*]
            }

            pub fn from_array([$($field),*]: [u64; Self::FIELDS]) -> Self {
                Self { $($field),* }
            }
        }
    };
}

bucket!(
    keys, clicks, scrolls, buttons, lt_pulls, rt_pulls, lt_ms, rt_ms, //
    m_left, m_right, m_middle, m_x1, m_x2, //
    pad_a, pad_b, pad_x, pad_y, pad_up, pad_down, pad_left, pad_right, //
    pad_lb, pad_rb, pad_start, pad_back, pad_ls, pad_rs,
);

impl Bucket {
    /// Every counted input. Engaged trigger time is a duration, not an input, and the per-button
    /// fields break down `clicks` and `buttons`, so they stay out.
    pub fn total(&self) -> u64 {
        self.keys + self.clicks + self.scrolls + self.buttons + self.lt_pulls + self.rt_pulls
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn has_controller(&self) -> bool {
        self.buttons + self.lt_pulls + self.rt_pulls + self.lt_ms + self.rt_ms > 0
    }

    pub fn mouse_buttons(&self) -> u64 {
        self.m_left + self.m_right + self.m_middle + self.m_x1 + self.m_x2
    }
}

impl AddAssign for Bucket {
    fn add_assign(&mut self, other: Self) {
        let mut sum = self.to_array();
        for (a, b) in sum.iter_mut().zip(other.to_array()) {
            *a += b;
        }
        *self = Self::from_array(sum);
    }
}

/// Taps not flushed yet, one counter per `Field`. The hooks add to these and nothing else.
static PENDING: [AtomicU64; Bucket::FIELDS] = [const { AtomicU64::new(0) }; Bucket::FIELDS];

#[inline]
pub fn add(field: Field, n: u64) {
    PENDING[field as usize].fetch_add(n, Ordering::Relaxed);
}

pub fn add_bucket(delta: Bucket) {
    for (counter, value) in PENDING.iter().zip(delta.to_array()) {
        if value > 0 {
            counter.fetch_add(value, Ordering::Relaxed);
        }
    }
}

/// The mouse hook adds only the per-button count, so `clicks` is derived here.
fn read(get: impl Fn(&AtomicU64) -> u64) -> Bucket {
    let mut b = Bucket::from_array(std::array::from_fn(|i| get(&PENDING[i])));
    b.clicks += b.mouse_buttons();
    b
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
            m_left: 1,
            m_x2: 1,
            pad_a: 3,
            pad_rs: 1,
            ..Default::default()
        };
        a += a;
        assert_eq!(a.keys, 2);
        assert_eq!(a.rt_pulls, 12);
        assert_eq!(a.rt_ms, 1600);
        assert_eq!((a.m_left, a.m_x2, a.pad_a, a.pad_rs), (2, 2, 6, 2));
        assert_eq!(a.mouse_buttons(), 4);
        assert_eq!(
            a.total(),
            42,
            "per-button fields break down clicks and buttons"
        );
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
            m_x1: 2,
            pad_back: 1,
            ..Default::default()
        };
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, r#"{"keys":5,"lt_ms":9,"m_x1":2,"pad_back":1}"#);
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

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::Win32::UI::Input::XboxController::{
    XINPUT_GAMEPAD_TRIGGER_THRESHOLD, XINPUT_STATE, XInputGetState, XUSER_MAX_COUNT,
};

use crate::counts::{self, Bucket};

/// A, B, X, Y, d-pad, bumpers, start, back and stick clicks, in XInput's bit layout. Any other
/// backend maps its buttons onto these bits.
pub const BUTTON_MASK: u16 = 0xF3FF;
pub const TRIGGER_THRESHOLD: f32 = XINPUT_GAMEPAD_TRIGGER_THRESHOLD.0 as f32 / 255.0;
const SLOTS: usize = XUSER_MAX_COUNT as usize;
const POLL: Duration = Duration::from_millis(8);
const PROBE: Duration = Duration::from_secs(10);
/// A device-change message can arrive before XInput sees the pad, so probe again shortly after.
const FOLLOW_UP: Duration = Duration::from_secs(1);
const FOLLOW_UPS: u8 = 3;
/// Caps one poll step, so a stalled thread or a sleeping PC never credits hours of trigger time.
const MAX_STEP: Duration = Duration::from_millis(250);

/// One controller reading, independent of the backend. Triggers run 0.0..=1.0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State {
    pub buttons: u16,
    pub lt: f32,
    pub rt: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Trigger {
    held: bool,
    carry_us: u128,
}

impl Trigger {
    /// The time since the last poll counts as engaged when the trigger was held at that poll.
    fn step(&mut self, value: f32, elapsed: Duration, pulls: &mut u64, ms: &mut u64) {
        if self.held {
            self.carry_us += elapsed.as_micros();
            *ms += (self.carry_us / 1000) as u64;
            self.carry_us %= 1000;
        }
        let above = value > TRIGGER_THRESHOLD;
        if above && !self.held {
            *pulls += 1;
        }
        self.held = above;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Pad {
    connected: bool,
    buttons: u16,
    lt: Trigger,
    rt: Trigger,
}

impl Pad {
    /// `None` means disconnected. The first reading after a connect only sets the baseline, so
    /// buttons already held at connect are not counted.
    pub fn step(&mut self, state: Option<State>, elapsed: Duration) -> Bucket {
        let mut out = Bucket::default();
        let Some(s) = state else {
            *self = Self::default();
            return out;
        };
        let buttons = s.buttons & BUTTON_MASK;
        if self.connected {
            out.buttons = u64::from((buttons & !self.buttons).count_ones());
            self.lt
                .step(s.lt, elapsed, &mut out.lt_pulls, &mut out.lt_ms);
            self.rt
                .step(s.rt, elapsed, &mut out.rt_pulls, &mut out.rt_ms);
        } else {
            self.connected = true;
            self.lt.held = s.lt > TRIGGER_THRESHOLD;
            self.rt.held = s.rt > TRIGGER_THRESHOLD;
        }
        self.buttons = buttons;
        out
    }
}

/// Decides which slots to read when: connected slots every poll, empty slots only on a probe,
/// because reading an empty XInput slot is slow.
pub struct Poller {
    pads: [Pad; SLOTS],
    last: Option<Instant>,
    next_probe: Instant,
    follow_ups: u8,
}

impl Poller {
    pub fn new(now: Instant) -> Self {
        Self {
            pads: [Pad::default(); SLOTS],
            last: None,
            next_probe: now,
            follow_ups: 0,
        }
    }

    pub fn any_connected(&self) -> bool {
        self.pads.iter().any(|p| p.connected)
    }

    pub fn device_changed(&mut self, now: Instant) {
        self.next_probe = now;
        self.follow_ups = FOLLOW_UPS;
    }

    pub fn poll(&mut self, now: Instant, mut read: impl FnMut(usize) -> Option<State>) -> Bucket {
        let elapsed = self.last.map_or(Duration::ZERO, |last| {
            now.saturating_duration_since(last).min(MAX_STEP)
        });
        self.last = Some(now);
        let probe = now >= self.next_probe;
        if probe {
            self.next_probe = if self.follow_ups > 0 {
                self.follow_ups -= 1;
                now + FOLLOW_UP
            } else {
                now + PROBE
            };
        }
        let mut out = Bucket::default();
        for (slot, pad) in self.pads.iter_mut().enumerate() {
            if pad.connected || probe {
                out += pad.step(read(slot), elapsed);
            }
        }
        out
    }

    pub fn wait(&self, now: Instant) -> Duration {
        let to_probe = self.next_probe.saturating_duration_since(now);
        if self.any_connected() {
            POLL.min(to_probe)
        } else {
            to_probe
        }
    }
}

fn read_xinput(slot: usize) -> Option<State> {
    let mut state = XINPUT_STATE::default();
    if unsafe { XInputGetState(slot as u32, &mut state) } != ERROR_SUCCESS.0 {
        return None;
    }
    let pad = state.Gamepad;
    Some(State {
        buttons: pad.wButtons.0,
        lt: f32::from(pad.bLeftTrigger) / 255.0,
        rt: f32::from(pad.bRightTrigger) / 255.0,
    })
}

static THREAD: OnceLock<Thread> = OnceLock::new();
static CHANGED: AtomicBool = AtomicBool::new(false);

/// Called on `WM_DEVICECHANGE`; probes the slots now instead of at the next slow probe.
pub fn device_changed() {
    CHANGED.store(true, Ordering::Relaxed);
    if let Some(thread) = THREAD.get() {
        thread.unpark();
    }
}

/// A high-resolution waitable timer gives a real 8 ms poll without raising the system-wide timer
/// resolution, which `timeBeginPeriod` would do at a cost to every process.
struct Timer(HANDLE);

impl Timer {
    fn new() -> Option<Self> {
        unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .ok()
        .map(Self)
    }

    fn sleep(&self, duration: Duration) {
        let due = -((duration.as_nanos() / 100) as i64);
        unsafe {
            if SetWaitableTimer(self.0, &due, 0, None, None, false).is_ok() {
                WaitForSingleObject(self.0, INFINITE);
            } else {
                std::thread::sleep(duration);
            }
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Polls only while a controller is connected. Otherwise the thread parks until a device change
/// or the slow fallback probe.
pub fn spawn() {
    let handle = std::thread::Builder::new()
        .name("controller".into())
        .spawn(|| {
            let timer = Timer::new();
            let mut poller = Poller::new(Instant::now());
            loop {
                let now = Instant::now();
                if CHANGED.swap(false, Ordering::Relaxed) {
                    poller.device_changed(now);
                }
                let delta = poller.poll(now, read_xinput);
                if !delta.is_empty() {
                    counts::add_controller(delta);
                }
                let wait = poller.wait(Instant::now());
                match (&timer, poller.any_connected()) {
                    (Some(timer), true) => timer.sleep(wait),
                    _ => std::thread::park_timeout(wait),
                }
            }
        })
        .expect("spawn controller thread");
    let _ = THREAD.set(handle.thread().clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u16 = 0x1000;
    const B: u16 = 0x2000;
    const DPAD_UP: u16 = 0x0001;
    const MS: Duration = Duration::from_millis(1);

    fn state(buttons: u16, lt: f32, rt: f32) -> Option<State> {
        Some(State { buttons, lt, rt })
    }

    #[test]
    fn buttons_count_on_press_only() {
        let mut pad = Pad::default();
        assert!(pad.step(state(0, 0.0, 0.0), MS).is_empty());
        assert_eq!(pad.step(state(A, 0.0, 0.0), MS).buttons, 1);
        assert_eq!(pad.step(state(A, 0.0, 0.0), MS).buttons, 0, "held");
        assert_eq!(pad.step(state(A | B | DPAD_UP, 0.0, 0.0), MS).buttons, 2);
        assert_eq!(pad.step(state(0, 0.0, 0.0), MS).buttons, 0, "release");
        assert_eq!(pad.step(state(B, 0.0, 0.0), MS).buttons, 1);
    }

    #[test]
    fn unknown_bits_are_ignored() {
        let mut pad = Pad::default();
        pad.step(state(0, 0.0, 0.0), MS);
        assert_eq!(pad.step(state(0x0C00, 0.0, 0.0), MS).buttons, 0);
    }

    #[test]
    fn held_at_connect_is_not_counted() {
        let mut pad = Pad::default();
        assert!(pad.step(state(A, 1.0, 0.0), MS).is_empty());
        let out = pad.step(state(A, 1.0, 0.0), MS * 8);
        assert_eq!(out.buttons, 0);
        assert_eq!(out.lt_pulls, 0);
        assert_eq!(out.lt_ms, 8, "engaged time still counts");
    }

    #[test]
    fn trigger_pull_crosses_the_threshold_upward() {
        let mut pad = Pad::default();
        pad.step(state(0, 0.0, 0.0), MS);
        let at = TRIGGER_THRESHOLD;
        assert_eq!(
            pad.step(state(0, at, 0.0), MS).lt_pulls,
            0,
            "at is not above"
        );
        assert_eq!(pad.step(state(0, 0.5, 0.0), MS).lt_pulls, 1);
        assert_eq!(pad.step(state(0, 1.0, 0.0), MS).lt_pulls, 0);
        assert_eq!(pad.step(state(0, 0.05, 0.0), MS).lt_pulls, 0);
        assert_eq!(pad.step(state(0, 0.5, 0.9), MS).lt_pulls, 1);
        let xinput_30 = 30.0 / 255.0;
        let xinput_31 = 31.0 / 255.0;
        let mut pad = Pad::default();
        pad.step(state(0, 0.0, 0.0), MS);
        assert_eq!(pad.step(state(0, 0.0, xinput_30), MS).rt_pulls, 0);
        assert_eq!(pad.step(state(0, 0.0, xinput_31), MS).rt_pulls, 1);
    }

    #[test]
    fn engaged_time_accumulates_real_elapsed_time() {
        let mut pad = Pad::default();
        pad.step(state(0, 0.0, 0.0), MS);
        assert_eq!(
            pad.step(state(0, 1.0, 0.0), MS * 8).lt_ms,
            0,
            "pressed at this poll"
        );
        let mut total = 0;
        for _ in 0..3 {
            total += pad
                .step(state(0, 1.0, 0.0), Duration::from_micros(8_400))
                .lt_ms;
        }
        assert_eq!(total, 25, "sub-ms remainders carry over: 3 x 8.4 ms");
        total += pad
            .step(state(0, 0.0, 0.0), Duration::from_micros(8_400))
            .lt_ms;
        assert_eq!(total, 33, "the step that sees the release still counts");
        assert_eq!(pad.step(state(0, 0.0, 0.0), MS * 8).lt_ms, 0);
    }

    #[test]
    fn disconnect_resets_the_pad() {
        let mut pad = Pad::default();
        pad.step(state(0, 0.0, 0.0), MS);
        pad.step(state(0, 1.0, 0.0), MS);
        assert!(pad.step(None, MS).is_empty());
        assert!(
            pad.step(state(A, 1.0, 0.0), MS).is_empty(),
            "baseline again"
        );
    }

    #[test]
    fn empty_slots_are_only_probed() {
        let start = Instant::now();
        let mut poller = Poller::new(start);
        let mut reads = Vec::new();
        poller.poll(start, |slot| {
            reads.push(slot);
            None
        });
        assert_eq!(reads, [0, 1, 2, 3], "first poll probes every slot");
        assert_eq!(poller.wait(start), PROBE);

        reads.clear();
        poller.poll(start + MS * 8, |slot| {
            reads.push(slot);
            None
        });
        assert!(reads.is_empty(), "no reads between probes");

        poller.device_changed(start + MS * 20);
        assert_eq!(poller.wait(start + MS * 20), Duration::ZERO);
        poller.poll(start + MS * 20, |slot| {
            (slot == 2).then_some(State::default())
        });
        assert!(poller.any_connected());
        assert_eq!(poller.wait(start + MS * 20), POLL);

        reads.clear();
        poller.poll(start + MS * 28, |slot| {
            reads.push(slot);
            Some(State::default())
        });
        assert_eq!(reads, [2], "only the connected slot between probes");
    }

    #[test]
    fn device_change_schedules_follow_up_probes() {
        let start = Instant::now();
        let mut poller = Poller::new(start);
        poller.poll(start, |_| None);
        poller.device_changed(start);
        let mut now = start;
        for _ in 0..FOLLOW_UPS {
            poller.poll(now, |_| None);
            assert_eq!(poller.wait(now), FOLLOW_UP);
            now += FOLLOW_UP;
        }
        poller.poll(now, |_| None);
        assert_eq!(poller.wait(now), PROBE);
    }

    #[test]
    fn long_gaps_are_capped() {
        let start = Instant::now();
        let mut poller = Poller::new(start);
        let held = |slot| {
            (slot == 0).then_some(State {
                lt: 1.0,
                ..Default::default()
            })
        };
        poller.poll(start, held);
        let out = poller.poll(start + Duration::from_secs(3600), held);
        assert_eq!(out.lt_ms, MAX_STEP.as_millis() as u64);
    }
}

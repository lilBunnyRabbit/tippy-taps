# PlayStation controller input (DS4 + DualSense)

Task t6, 2026-11-20. Question: how should a hidden, never-focused tray app count DualShock 4 and DualSense input (buttons, L2/R2 pulls, trigger engaged time) over USB and Bluetooth, inside the spec's near-zero idle budget, with no crate beyond `windows` features? Xbox already goes through XInput.

## Verdict

Use **Raw Input HID** (`RegisterRawInputDevices` with `RIDEV_INPUTSINK | RIDEV_DEVNOTIFY` on a message-only window), parse the Sony reports by hand, and **drain with `GetRawInputBuffer` on the existing ~125 Hz controller tick** instead of handling one `WM_INPUT` per report. Keep XInput for Xbox. Everything needed is already in the enabled `windows` features (`Win32_UI_Input`, `Win32_UI_WindowsAndMessaging`, `Win32_System_LibraryLoader`). No new feature and no new crate.

Windows.Gaming.Input fails on the first requirement: it only delivers input to the focused app. GameInput needs a redistributable, has no bindings in `windows` 0.62, and is off for background input by default.

## Comparison

| | (a) Windows.Gaming.Input `RawGameController` | (b) Raw Input HID + report parsing | (c) GameInput |
|---|---|---|---|
| Input while hidden or unfocused | **No.** "A Windows application must have focus to receive input from a controller." | **Yes.** `RIDEV_INPUTSINK`: "enables the caller to receive the input even when the caller is not in the foreground". `hwndTarget` is required. `WM_INPUT` arrives with `wParam = RIM_INPUTSINK`. | Only after opt-in. Docs: "By default, GameInput will not provide background input." v4 removed `SetFocusPolicy` and replaced it with `IGameInput::EnableBackgroundInput`. |
| Bindings in `windows` 0.62.2 | Yes: feature `Gaming_Input` (pulls `windows-future`/`windows-collections`, already transitive deps) | Yes: already-enabled `Win32_UI_Input` | **None.** No GameInput module in `windows-0.62.2/src`. COM vtables would have to be hand-written. |
| Runtime | Inbox | Inbox (since XP) | The docs say "PC games must incorporate the GameInput redistributable into their respective installers". An inbox copy exists but its version is not controlled. From v4 a sideloaded `GameInputRedist.dll` is also possible, but it still has to be shipped. |
| Wakeups while connected and idle | Polling `GetCurrentReading` at our tick: about 125/s | Pads stream continuously (sensors and counter), so every report is a `WM_INPUT`: DS4 USB 250 Hz (4 ms default per SDL), DualSense USB 250 Hz, DualSense BT full mode about 1000 Hz (SDL comment: "appears"). **With `GetRawInputBuffer` drained on an 8 ms timer, wakeups drop to about 125/s**, the same as the XInput budget. | Callbacks or polling, about 125/s (unverified) |
| Trigger mapping | Unnamed axis array in HID descriptor order. Index differs per pad (unverified). It is not a `Gamepad`. | Fixed byte offsets, from the Linux `hid-playstation.c` structs (see below) | `GameInputGamepadState.leftTrigger` if the pad is mapped as a gamepad. DualSense coverage is unverified. |
| Add and remove events | `RawGameControllerAdded`/`Removed` | `WM_INPUT_DEVICE_CHANGE` (`GIDC_ARRIVAL`/`GIDC_REMOVAL`) via `RIDEV_DEVNOTIFY` | Device callbacks |
| Virtual-pad double count | Sees the physical pad and any virtual X360 pad. VID/PID are available via `HardwareVendorId`. | Sees only HID. Filter to Sony VID 054C, so a virtual X360 pad is never read here. The XInput side must drop mirrors (see Dedupe). | Same problem |
| Replace XInput for Xbox? | No. It needs focus, so it is strictly worse than XInput for this app. | Not worth it: Xbox HID reports differ per model, and XInput already works in the background. | It could, but costs a redist and bindings. |

Rank by weight (platform feature < existing code < new code < new dependency): (b) is a platform API with bindings already compiled in, plus about 150 lines of new code. (a) fails the hard requirement. (c) is effectively a new dependency (redist plus hand-written COM) and is off by default anyway.

## Report layouts (b)

Source: Linux `drivers/hid/hid-playstation.c` (torvalds master, fetched 2026-11-20) and SDL `src/joystick/hidapi/SDL_hidapi_ps4.c` / `SDL_hidapi_ps5.c` (main @ e21b424, 2026-11-20).

On Windows, raw input `RAWHID.bRawData` starts with the report ID. Each report is `dwSizeHid` bytes, and there are `dwCount` reports.

Devices to accept (VID `0x054C`): DS4 `0x05C4`, DS4 v2 `0x09CC`, DS4 wireless dongle `0x0BA0`, DualSense `0x0CE6`, DualSense Edge `0x0DF2`. Register HID usage page `0x01`, usages `0x04` (joystick) and `0x05` (gamepad), then filter by VID/PID from `GetRawInputDeviceInfoW(RIDI_DEVICEINFO)`.

| Pad / mode | Report ID | Base offset `o` | L2 | R2 | buttons b0, b1, b2 |
|---|---|---|---|---|---|
| DS4 USB | `0x01` (64 B) | 1 | `o+7` | `o+8` | `o+4..o+6` |
| DS4 BT minimal (default until someone switches it) | `0x01` (10 B, may arrive padded) | 1 | `o+7` | `o+8` | `o+4..o+6` |
| DS4 BT full | `0x11` | 3 | `o+7` | `o+8` | `o+4..o+6` |
| DualSense USB | `0x01`, `dwSizeHid == 64` | 1 | `o+4` | `o+5` | `o+7..o+9` |
| DualSense BT simple | `0x01`, any other size (SDL accepts 10 or 78) | 1 | `o+7` | `o+8` | `o+4..o+6` |
| DualSense BT full | `0x31` | 2 | `o+4` | `o+5` | `o+7..o+9` |

DS4 report `0x01` has the same prefix over USB and BT minimal, so no bus detection is needed. Only DualSense `0x01` splits on size. Never write output or feature reports to switch a pad into full mode. SDL calls enhanced mode "a one-way ticket", and switching it would change what other apps see. Parse whatever mode the pad is already in.

Button bits are the same on both pads (from `hid-playstation.c`):
- **b0:** low nibble = hat (0..7 = N, NE, E, SE, S, SW, W, NW; 8 = neutral). bit4 Square, bit5 Cross, bit6 Circle, bit7 Triangle.
- **b1:** bit0 L1, bit1 R1, bit2 L2-digital, bit3 R2-digital, bit4 Share/Create, bit5 Options, bit6 L3, bit7 R3.
- **b2:** bit0 PS, bit1 touchpad click, bit2 mic mute (DualSense only). The upper bits are a frame counter on DS4 and in the simple report (SDL field `rgucButtonsHatAndCounter`), so mask b2 with `0x03` (DS4 and simple) or `0x07` (DualSense full).

Mapping to the spec's buckets, for parity with XInput:
- **`buttons`:** a rising edge on face buttons, d-pad (expand the hat into 4 direction bits so a diagonal counts like XInput's two bits), L1/R1, Options/Share, L3/R3. Leave out the L2/R2 digital bits, because triggers have their own counts. PS, touchpad and mic are an owner call: XInput cannot see Guide, so leave them out for parity, or count them.
- **`lt_pulls`/`rt_pulls`:** an upward crossing of 30 on L2/R2 (0..255), the same value as `XINPUT_GAMEPAD_TRIGGER_THRESHOLD`.
- **`lt_ms`/`rt_ms`:** real elapsed time between drain ticks while the last-seen value is above 30, the same as the XInput rule. Within one drain, every report is walked in order, so a tap shorter than 8 ms still produces a pull.

## Idle cost

- **No PS pad present:** the controller thread blocks in `GetMessageW`. Zero wakeups. A new pad wakes it through `WM_INPUT_DEVICE_CHANGE`. That also satisfies the spec's "probe on device change" for XInput, because Xbox pads are HID too (the 10 s fallback probe stays).
- **PS pad connected and idle:** the pad keeps streaming 250–1000 reports/s whatever we do. Raw input queues them in our thread's queue, but the thread is not woken unless it waits on `QS_RAWINPUT`. It sleeps on an 8 ms waitable timer (or `MsgWaitForMultipleObjects` with a timeout and a wake mask *without* `QS_RAWINPUT`), then calls `GetRawInputBuffer`. The docs say this reads the data and "corresponding WM_INPUT messages are removed from the calling thread's message queue". Each tick parses about 2–8 small reports. That is about 125 wakeups/s and well under 0.1% CPU (unverified: measure per the spec's "Measure it" rule).
- **Per-message `WM_INPUT` handling** would cost 250–1000 wakeups/s per pad. Avoid it.
- **Disk:** unchanged. Counts only move when a press happens.

## Dedupe with Steam Input / DS4Windows

When Steam Input or DS4Windows is active, the same physical PS pad shows up twice: on raw HID (the Sony device) and as a virtual XInput pad. Raw HID is the source of truth for a Sony pad, so drop the XInput mirror:

1. **Steam virtual pad:** on each XInput slot connect, call the undocumented `XInputGetCapabilitiesEx` (`xinput1_4.dll` ordinal 108, via `GetProcAddress`, which is already enabled). SDL uses the same call. The struct is `XINPUT_CAPABILITIES` followed by `VendorId`, `ProductId`, `ProductVersion` (u16 each), `unk1: u16` and `unk2: u32`. VID `0x28DE` / PID `0x11FF` is Steam's virtual gamepad (SDL `usb_ids.h`). While any Sony device is present on raw input, skip that slot. Steam can move a different pad into a slot, so re-check on reconnect.
2. **ViGEm X360 (DS4Windows default):** it reports `045E:028E`, the same as a real wired 360 pad, so the ID cannot tell them apart. Use correlation instead. While a Sony device is active, hold a new XInput slot's counts as pending. If its next 3 button rising edges each match a mapped Sony edge (Cross = A, and so on) within ±2 ticks, mark the slot a mirror until it disconnects, and discard the pending counts. On the first mismatch, commit the pending counts and treat the slot as a real pad. Steam/DS4Windows remapping can defeat this, so it is best effort.
3. **HidHide** (often used with DS4Windows) hides the physical pad from other processes. Whether that also blocks raw input is unverified. If it does, only the virtual XInput pad is visible and nothing is double counted. If it does not, rule 2 applies.
4. **DS4Windows "emulate DS4" mode** creates a virtual Sony pad on raw HID next to the real one. This is rare. Apply the same correlation between Sony devices (unverified device path markers).

## Sketch

One `controller` thread owns a message-only window, the XInput slots and the Sony devices. It blocks while nothing is connected and ticks at 8 ms while anything is.

```rust
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const SONY: u32 = 0x054C;
const PIDS: [u32; 5] = [0x05C4, 0x09CC, 0x0BA0, 0x0CE6, 0x0DF2];
const TRIGGER: u8 = 30;

unsafe fn register(hwnd: HWND) -> windows::core::Result<()> {
    let dev = |usage| RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: usage,
        dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
        hwndTarget: hwnd,
    };
    unsafe { RegisterRawInputDevices(&[dev(0x04), dev(0x05)], size_of::<RAWINPUTDEVICE>() as u32) }
}

#[derive(Default, Clone, Copy)]
struct Pad { buttons: u16, l2: u8, r2: u8 }

fn parse(dualsense: bool, data: &[u8]) -> Option<Pad> {
    let (o, full_ds) = match (data.first()?, dualsense) {
        (0x01, false) => (1, false),
        (0x11, false) => (3, false),
        (0x01, true) => (1, data.len() == 64),
        (0x31, true) => (2, true),
        _ => return None,
    };
    let (l2, r2, b) = if full_ds { (o + 4, o + 5, o + 7) } else { (o + 7, o + 8, o + 4) };
    let (b0, b1) = (*data.get(b)?, *data.get(b + 1)?);
    let hat = match b0 & 0x0F { 0 => 0b0001, 1 => 0b0011, 2 => 0b0010, 3 => 0b0110,
                                4 => 0b0100, 5 => 0b1100, 6 => 0b1000, 7 => 0b1001, _ => 0 };
    let face = (b0 >> 4) as u16;
    let shoulders = (b1 & 0b1111_0011) as u16;
    Some(Pad { buttons: hat | face << 4 | shoulders << 8, l2: data[l2], r2: data[r2] })
}

fn apply(prev: &mut Pad, next: Pad) {
    BUTTONS.fetch_add((next.buttons & !prev.buttons).count_ones() as u64, Relaxed);
    if prev.l2 <= TRIGGER && next.l2 > TRIGGER { LT_PULLS.fetch_add(1, Relaxed); }
    if prev.r2 <= TRIGGER && next.r2 > TRIGGER { RT_PULLS.fetch_add(1, Relaxed); }
    *prev = next;
}

unsafe fn drain(pads: &mut Pads, buf: &mut [u64; 1024]) {
    loop {
        let mut size = size_of_val(buf) as u32;
        let n = unsafe { GetRawInputBuffer(Some(buf.as_mut_ptr().cast()), &mut size,
                                           size_of::<RAWINPUTHEADER>() as u32) };
        if n == 0 || n == u32::MAX { break }
        let mut p = buf.as_ptr() as *const u8;
        for _ in 0..n {
            let ri = unsafe { &*(p as *const RAWINPUT) };
            if ri.header.dwType == RIM_TYPEHID.0 {
                if let Some(pad) = pads.get_mut(ri.header.hDevice) {
                    let hid = unsafe { &ri.data.hid };
                    let len = (hid.dwSizeHid * hid.dwCount) as usize;
                    let raw = unsafe { std::slice::from_raw_parts(hid.bRawData.as_ptr(), len) };
                    for report in raw.chunks_exact(hid.dwSizeHid as usize) {
                        if let Some(next) = parse(pad.dualsense, report) { apply(&mut pad.state, next) }
                    }
                }
            }
            p = unsafe { p.add((ri.header.dwSize as usize + 7) & !7) };
        }
    }
}

// Thread loop:
//   nothing connected -> GetMessageW (blocks); WM_INPUT_DEVICE_CHANGE GIDC_ARRIVAL -> check
//     VID/PID via GetRawInputDeviceInfoW(RIDI_DEVICEINFO), add pad; also probe XInput slots.
//   anything connected -> every 8 ms: drain(); poll XInput slots; add elapsed ms to lt_ms/rt_ms
//     for each pad whose state.l2/r2 > TRIGGER; PeekMessageW(PM_REMOVE) for WM_INPUT_DEVICE_CHANGE.
//     Wait with MsgWaitForMultipleObjects(None, false, 8, QS_POSTMESSAGE | QS_SENDMESSAGE),
//     never QS_RAWINPUT, so reports don't wake the thread.
```

The `+7 & !7` step is `NEXTRAWINPUTBLOCK` on 64-bit, because `windows` has no macro for it. Hold the buffer as `[u64; N]` so it is 8-byte aligned. Real code must check `dwSizeHid` bounds before indexing.

## Risks

- **No-wake while ticking:** whether `QS_RAWINPUT` input without that wake bit really leaves the thread asleep, and whether the queue tolerates 8 ms of backlog without loss, comes from the documented behaviour, not a test. Measure wakeups (e.g. Process Explorer context-switch delta) with a pad connected.
- **Padded reports:** DualSense BT `0x01` may arrive padded to the collection's max report size. Any non-64 size is treated as simple (SDL does the same with 10/78).
- **Clones and third-party "PS4" pads** with other VIDs are not counted. Some clones never leave BT minimal mode, which is handled.
- **Mirror correlation** is heuristic. A Steam remap that moves Cross off A breaks rule 2, and the XInput slot gets counted too.
- **`XInputGetCapabilitiesEx`** is undocumented. If `GetProcAddress` returns null, skip rule 1 and rely on rule 2.
- **winit raw-input registrations:** `hooks::drop_raw_input` only removes keyboard and mouse. Gamepad usages are separate TLCs, so registering 0x04/0x05 to our message-only window does not collide. Raw input allows one target per TLC per process.

## Reuse

- `src/hooks.rs`: the `RegisterRawInputDevices` / `RAWINPUTDEVICE` pattern and the dedicated-thread `GetMessageW` loop.
- `src/tray.rs`: message-only window plus `SetTimer` pattern.
- `Cargo.toml`: features `Win32_UI_Input`, `Win32_UI_Input_XboxController`, `Win32_System_LibraryLoader`, `Win32_UI_WindowsAndMessaging`. All APIs above are already compiled in. Verified in `windows-0.62.2/src`: `GetRawInputBuffer`, `GetRawInputDeviceInfoW`, `RIDEV_INPUTSINK`, `RIDEV_DEVNOTIFY`, `WM_INPUT_DEVICE_CHANGE`, `HWND_MESSAGE`, `MsgWaitForMultipleObjects`; XInput links `xinput1_4.dll`.

## Sources

- RAWINPUTDEVICE (flags, INPUTSINK, DEVNOTIFY), learn.microsoft.com, updated 2024-06-26: https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawinputdevice
- WM_INPUT (`RIM_INPUTSINK`), updated 2025-07-16: https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-input
- GetRawInputBuffer (removes WM_INPUT from the queue, QS_RAWINPUT), updated 2025-05-21: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getrawinputbuffer
- Windows.Gaming.Input namespace, remark "must have focus", updated 2023-10-27: https://learn.microsoft.com/en-us/uwp/api/windows.gaming.input
- RawGameController class, updated 2025-11-21: https://learn.microsoft.com/en-us/uwp/api/windows.gaming.input.rawgamecontroller
- GameInput NuGet / redistributable, GDK 2610, updated 2026-11-04: https://learn.microsoft.com/en-us/gaming/gdk/docs/features/common/input/overviews/input-nuget
- GameInputFocusPolicy (deprecated in v4, background off by default), updated 2026-11-04: https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/input/gameinput/enums/gameinputfocuspolicy
- Linux `hid-playstation.c`, `hid-ids.h` (torvalds master, fetched 2026-11-20): https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/drivers/hid/hid-playstation.c
- SDL main @ e21b424 (2026-11-20): `src/joystick/hidapi/SDL_hidapi_ps4.c` (4 ms default interval), `SDL_hidapi_ps5.c` (250/1000 Hz, simple packet), `src/joystick/usb_ids.h` (28DE:11FF), `src/core/windows/SDL_xinput.c` + `.h` (ordinal 108, CAPABILITIES_EX), `src/joystick/windows/SDL_xinputjoystick.c` (Steam slot detection): https://github.com/libsdl-org/SDL
- Corroboration that WGI stops in the background: Unity issue tracker, "inputsystem does not register controller inputs in the background…": https://issuetracker.unity3d.com/issues/inputsystem-does-not-register-controller-inputs-in-the-background-when-inputsettings-dot-backgroundbehavior-is-set-to-ignorefocus

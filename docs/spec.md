# tippy-taps v0.1 spec

Request: a keystroke/input counter that never needs the internet (Bongo Cat only counts after an online start, and taps get lost), runs as a native program, and starts with the PC.

Stack mirrors `../looking-glass` (read its `src/main.rs`, `Cargo.toml` and `build.rs` for style, brand colours and egui setup): Rust, eframe/egui (glow), single binary, Windows only, embedded icon via winresource, tag-triggered GitHub release. Dependencies are already in `Cargo.toml`. Workers cannot add packages: if you need one, say so under `Open:`.

## Decided (owner, 2026-11-20)

- Count **keystrokes, mouse clicks and scroll notches**, as three separate counts.
- **Tray icon + stats window.**
- Input capture via the `windows` crate: `WH_KEYBOARD_LL` + `WH_MOUSE_LL`. Autostart via the HKCU Run key.
- Public GitHub repo with releases (the lead handles the repo and pushes).

## Behaviour

**Counting** (dedicated hook thread with its own `GetMessageW` loop)
- Key: count `WM_KEYDOWN`/`WM_SYSKEYDOWN` only on an up-to-down transition. Track the held state per vk so auto-repeat is not counted.
- Click: left, right, middle and X button down events.
- Scroll: `WM_MOUSEWHEEL` + `WM_MOUSEHWHEEL`. Count `|delta| / 120` notches, minimum 1 per event, so smooth-scroll mice still count.
- Skip injected events (`LLKHF_INJECTED` / `LLMHF_INJECTED`).
- Keep the hook callback minimal: atomic increments, then `CallNextHookEx`. Windows silently drops slow LL hooks.
- **Privacy:** never store or log which key was pressed. Counts only. The README says so.

**Storage**
- `%APPDATA%\tippy-taps\counts.json`: per local date `{ "YYYY-MM-DD": { keys, clicks, scrolls } }`.
- Flush every 30 s and on quit or session end. Write atomically (tmp file + rename). At most 30 s of taps may be lost on a power cut.
- Credit counts to the local date at flush time. Handle midnight rollover so taps before midnight land on the old day (flush at the day boundary).
- If the file is corrupt, rename it to `counts.corrupt-<timestamp>.json` and start fresh. Never silently overwrite it.
- Flushing must not depend on the egui `update` loop running. A hidden eframe window may stop repainting.

**Startup / lifecycle**
- Single instance via a named mutex. A second launch brings up the running instance's window, then exits.
- Autostart: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `tippy-taps` = `"<exe path>" --background`. Enable it on first run. On each start, if enabled, rewrite it with the current exe path so a moved exe self-heals. Toggle it from the tray menu and the window.
- `--background`: start with the tray only and no window. A manual launch opens the window.
- Closing the window hides it to the tray. Only tray → Quit exits, flushing first.

**Tray**
- Icon from `assets/icon.png`, embedded with `include_bytes!`.
- Tooltip: `Today: 12,345 keys · 678 clicks · 90 scrolls`, refreshed at least every few seconds.
- Left click opens the window. Menu: Open, Start with Windows (checkbox), Quit.
- Tray menu events must work while the window is hidden. Use the `tray-icon`/`muda` event handler, call `ctx.send_viewport_cmd` + `request_repaint`, and don't wait for `update`.

**Window** (brand palette from looking-glass, small, about 360x480)
- Today: three big numbers (keys, clicks, scrolls), live.
- All-time totals.
- History: the last 30 days, a simple per-day bar or row list showing a day's total and its three counts.
- Start with Windows checkbox.
- Footer: "Counts only, never which keys. Stored locally at <path>."

## First-release additions (owner, 2026-11-20)

Requester asks for: hour/day/week/month counters, time-of-day stats, a gross total hidden until clicked, fireworks at milestones, Xbox controller input with trigger engaged time, and a rabbit theme. Nothing has been released yet, so these all ship together as the first release. v0.1 was never published, so the file format can change freely with no migration.

**Storage v2** (replaces the per-day format)
- `counts.json` = `{ "version": 2, "hours": { "YYYY-MM-DDTHH": Bucket } }`, keyed by local hour. A DST fall-back hour merges into one bucket, which is accepted.
- `Bucket` = `{ keys, clicks, scrolls, buttons, lt_pulls, rt_pulls, lt_ms, rt_ms }`. Omit zero fields when serialising.
- Same flush, atomic-write, corrupt-file and locked-file rules as v0.1.
- `settings.json` sits beside it: overlay on/off and its position, fireworks opt-in, and milestones already celebrated. Autostart stays in the registry only.

**Xbox controller** (XInput through the `windows` crate, enabling feature `Win32_UI_Input_XboxController`; no new dependency)
- Poll thread at about 125 Hz for connected slots. Probe disconnected slots only every 2 s, because XInputGetState on an empty slot is slow.
- `buttons`: an up-to-down transition on any digital button (A/B/X/Y, d-pad, bumpers, start/back, stick clicks).
- Triggers: a press is counted when the value crosses `XINPUT_GAMEPAD_TRIGGER_THRESHOLD` (30) upward. `lt_ms`/`rt_ms` = time spent above the threshold, accumulated per poll from real elapsed time.
- Sticks are not counted.

**Stats window**
- Period tabs: Hour (this hour), Day, Week (Monday start), Month, each with the counts for that period and the one before it.
- Time of day: a weekday x hour heatmap (7x24) of the total inputs.
- Year calendar heatmap: one cell per day, GitHub-style, for the last 53 weeks.
- Hovering or clicking a heatmap cell shows that cell's breakdown.
- Controller section (shown once any controller data exists): buttons, LT/RT pulls, LT/RT engaged time.
- Gross total: all-time sum of every count, hidden behind a "tap to reveal" control and hidden again on close.

**Milestones + fireworks** (opt-in, off by default, toggled in the window and the tray menu)
- Thresholds per count and for the gross total: 1k, 10k, 50k, 100k, 250k, 500k, 1M, then every 1M. Each fires once; the celebrated set is kept in settings.json.
- Fireworks play in the window and on the overlay rabbit, wherever they are visible. When neither is visible, queue them for the next time the window opens. A tray balloon/notification is allowed only if no new dependency is needed for it.

**Rabbit theme** (art from the designer, task t3)
- An original rabbit character. It must not copy Bongo Cat's look or layout.
- The window header has the rabbit drumming its paws on input.
- Optional corner overlay rabbit, off by default, toggled from the tray. It is always on top, clicks pass through it, and it is transparent like looking-glass. A "Move rabbit" tray item unlocks it for dragging, and its position persists.
- The app icon becomes the rabbit.

## Performance budget (owner, 2026-11-20: "tracking should have basically 0 impact on the pc")

Every design choice must meet these:
- **Idle (window hidden, no controller):** ~0% CPU. The only periodic wakeups are the flush timer and the tooltip refresh, and both skip their work when nothing changed. No busy loops and no fixed-rate repaint.
- **Hooks:** the callback does atomic increments plus `CallNextHookEx` and nothing else: no allocation, no locks, no syscalls.
- **Controller:** poll fast (~125 Hz) only while a controller is connected. With none connected, don't poll: probe the slots on `WM_DEVICECHANGE` (the hook thread's or a message-only window), plus a slow fallback probe every 10 s.
- **Disk:** write only when counts changed, so an idle PC writes nothing. Shard storage by month (`counts/YYYY-MM.json`, same bucket format), so a flush rewrites only the current month (~70 KB max), not the whole history.
- **Window:** repaint on data change at most ~2 Hz while visible, never while hidden. Compute heatmap and period aggregates when the data changes, not every frame. Animations (fireworks, rabbit) repaint only while they're playing.
- **Overlay rabbit:** repaints are driven by input events and capped at ~30 fps, with zero repaints when idle.
- **Memory:** aim for < 50 MB working set with the window hidden.
- **Measure it:** report idle CPU % (Task Manager or `Get-Process` CPU delta over 60 s) and the working set, hidden and visible, in the task report.

## Done when

- `cargo build --release --locked` and `cargo clippy -- -D warnings` pass.
- Unit tests cover the pure logic: the repeat filter, notch math, day rollover, storage load/merge and corrupt-file handling.
- `--background`, a second launch, autostart registration and the tray quit flush were all tried by hand on this machine, with notes in the report on what was verified and how.
- The README follows looking-glass's shape: what it is, Use, privacy line, Build.

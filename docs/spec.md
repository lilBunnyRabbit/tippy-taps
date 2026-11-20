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

## Done when

- `cargo build --release --locked` and `cargo clippy -- -D warnings` pass.
- Unit tests cover the pure logic: the repeat filter, notch math, day rollover, storage load/merge and corrupt-file handling.
- `--background`, a second launch, autostart registration and the tray quit flush were all tried by hand on this machine, with notes in the report on what was verified and how.
- The README follows looking-glass's shape: what it is, Use, privacy line, Build.

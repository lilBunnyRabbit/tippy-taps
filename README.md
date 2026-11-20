# tippy-taps

Offline keystroke, click and scroll counter for Windows. It starts with Windows, sits in the tray and counts from the moment you log in. No account, no internet, nothing to sign in to before it starts counting.

## Use

1. Download `tippy-taps-*.exe` from [Releases](https://github.com/lilBunnyRabbit/tippy-taps/releases) and run it.
2. The window shows today's keys, clicks and scrolls, your all-time totals and the last 30 days.
3. Closing the window hides it to the tray. Click the tray icon to bring it back; hover it for today's counts.
4. Tray menu: **Open**, **Start with Windows**, **Quit**. Only Quit stops counting.

The first run turns on **Start with Windows** (an entry under `HKCU\...\CurrentVersion\Run`). Turn it off from the tray menu or the window.

What counts:

| Count | Rule |
| --- | --- |
| keys | each key press; holding a key down is one press, auto-repeat is ignored |
| clicks | left, right, middle and side button presses |
| scrolls | wheel notches, vertical and horizontal; a smooth-scroll tick counts as at least one |

Input sent by other programs (macros, remote tools, on-screen keyboards) is not counted.

**Privacy:** tippy-taps counts only. It never stores or logs which keys you press. The counts live in `%APPDATA%\tippy-taps\counts.json` and never leave your PC.

Counts are saved every 30 seconds and on quit, so a power cut loses at most the last 30 seconds.

## Build

```sh
cargo build --release
```

Push a `v*` tag to build and publish a release.

# tippy-taps

Offline keystroke, click and scroll counter for Windows. It starts with Windows, sits in the tray and counts from the moment you log in. No account, no internet, nothing to sign in to before it starts counting.

## Use

1. Download `tippy-taps-*.exe` from [Releases](https://github.com/lilBunnyRabbit/tippy-taps/releases) and run it.
2. The window has a drumming rabbit up top and five tabs: **Counts** (this hour, day, week or month next to the one before, all-time totals, how far you have scrolled, and a gross total hidden until you tap it), **When** (year calendar and time-of-day heatmaps; hover or click a cell for its numbers), **Keyboard**, **Mouse** and **Controller**. The gear holds the settings.
3. Closing the window hides it to the tray. Click the tray icon to bring it back; hover it for today's counts.
4. Tray menu: **Open**, **Start with Windows**, **Fireworks at milestones**, **Show rabbit**, **Move rabbit**, **Quit**. Only Quit stops counting.
5. **Show rabbit** puts a small rabbit in the screen corner that drums along as you type and click. It stays on top and clicks pass through it; turn on **Move rabbit** to drag it somewhere else. Off by default.
6. Fireworks at milestones is off by default. Turned on, the window and the rabbit celebrate 1k, 10k, 50k, 100k, 250k, 500k and 1M of any count, then every million.
7. **Keyboard**, **Mouse** and **Controller** show each device tinted by how often each button is pressed; mouse and controller for any period. The keyboard heatmap is off by default; turned on, it counts presses per key position, all-time, with legends from your keyboard layout. Turning it off asks whether to delete those counts.

The first run turns on **Start with Windows** (an entry under `HKCU\...\CurrentVersion\Run`). Turn it off from the tray menu or the gear.

What counts:

| Count | Rule |
| --- | --- |
| keys | each key press; holding a key down is one press, auto-repeat is ignored |
| clicks | left, right, middle and side button presses, also counted per button |
| scrolls | wheel notches, vertical and horizontal; a smooth-scroll tick counts as at least one |
| buttons | Xbox controller button presses (XInput), also counted per button; holding a button is one press |
| LT / RT | trigger pulls past the trigger threshold, plus the time each trigger is held past it |

Input sent by other programs (macros, remote tools, on-screen keyboards) is not counted.

**Privacy:** Counts only. With the keyboard heatmap turned on, it also counts presses per key position. It never records what you typed, the order, or when. The counts live in `%APPDATA%\tippy-taps\counts\` (one file per month, plus `keys.json` for the keyboard heatmap, beside `settings.json`) and never leave your PC.

Counts are saved every 30 seconds and on quit, only when something changed, so a power cut loses at most the last 30 seconds and an idle PC writes nothing.

## Build

```sh
cargo build --release
```

Push a `v*` tag to build and publish a release.

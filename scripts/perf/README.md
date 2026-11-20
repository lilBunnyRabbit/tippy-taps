# perf

Whole-process sampler for the performance budget. Samples CPU %, working set, private bytes, handles, threads and IO bytes every second.

```powershell
.\matrix.ps1 -Base <old.exe> -Head <new.exe> -Out results\samples.csv -Runs 3
python summarize.py results\samples.csv
```

Each run uses a fresh `APPDATA` under the output folder, so it never touches the real counts. `mover.ps1` moves the cursor with `SendInput` at 1000 Hz. The app skips injected input, so a moving run only exercises the movement path in a build that counts injected events, and that build needs its own single-instance mutex name to run beside an installed copy.

A fresh `APPDATA` counts as a first run, which turns on Start with Windows and points `HKCU\...\Run	ippy-taps` at the sampled exe. Check that value after a session and restore it.

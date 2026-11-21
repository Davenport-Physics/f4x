# Spike S1: launch and inject under Proton. Result: GO

`cargo xtask spike-launch [--headless] [--runs N] [--no-build]`:

- builds `f4x_loader.exe` and `f4x_runtime.dll`;
- launches the game through umu and GE-Proton with the runtime injected;
- reads the session file, authenticates over TCP, pings until the main-loop hook counter moves, and quits.

Result on 2026-11-21:

| Mode | Runs passed | `{injected, tcp, frame_hook}` | Time per run |
|---|---|---|---|
| Headless (`gamescope --backend headless`) | 3/3 | all true | ~23 s |
| Desktop | 3/3 | all true | ~23 s |

The first main-loop pump comes about 18 s after injection.

## How it works
- **Loader:**
  - `CreateProcessW(CREATE_SUSPENDED)`;
  - pass the DLL path into the child (below);
  - `CreateRemoteThread(LoadLibraryW)`, wait, check the module handle;
  - `ResumeThread`;
  - wait for the game and return its exit code, so umu and pressure-vessel stay alive for the whole session.
- **Runtime `DllMain`:** an IAT hook on `USER32!PeekMessageA` (the main-loop message pump; imports are already bound by then), plus the control thread. Nothing else runs under the loader lock.
- **Control channel:**
  - newline-delimited JSON-RPC 2.0 on `127.0.0.1:<ephemeral>`;
  - token auth;
  - the port and token are published atomically in `session.json` in `F4X_SESSION_DIR`, a `Z:\` path the host maps to its own path;
  - `heartbeat.json` once a second;
  - `runtime.log` and `loader.log`.
- **Opt-in:** the server only starts when `F4X_CONTROL=1`.

## Findings that shaped the design
1. **`PROTON_VERB=run` is required.** umu's default `waitforexitandrun` blocks until every wineserver in the prefix exits, so it hangs forever whenever Vortex, MO2 or another session is using the prefix.
2. **Cross-container wineserver.** If another pressure-vessel container (e.g. Vortex) already owns the prefix's wineserver, that wineserver is in a different user namespace and **cannot write our child's memory**. `WriteProcessMemory` then fails with error 299. Operations the target process performs itself through APCs still work.
   - Fix: the loader passes the DLL path through a pagefile-backed section, mapped into the child with `NtMapViewOfSection`. `WriteProcessMemory` stays as a fallback.
3. **Headless needs its own wineserver.** Under a wineserver whose desktop (explorer) is on another X display, a game window on gamescope's Xwayland fails fatally with `BadWindow (X_UnmapWindow)` once its window maps. Wine can't mix displays in one session.
   - Fix: f4x uses a **dedicated test prefix**, `~/.local/share/f4x/prefixes/test` (~680 MB after Proton populates it), seeded with the user's three game inis.
   - This also isolates saves and inis from the user's own setup, and never touches the Vortex/Heroic prefix.
4. **`PeekMessageA` is only a pump signal,** called about 30 times per second here, not once per frame. Phase 1 replaces it with a real frame hook.

## Knobs

| Env var | Effect |
|---|---|
| `F4X_PREFIX` | Test prefix |
| `F4X_SOURCE_PREFIX` | Prefix to seed inis from |
| `F4X_GAME_DIR`, `F4X_PROTON`, `F4X_UMU` | Paths to the game, Proton and umu |
| `F4X_WINEDEBUG` | Wine debug channels (default `-all`) |
| `F4X_GAMESCOPE_ARGS` | gamescope arguments (default `-W 1280 -H 720 -r 60`) |

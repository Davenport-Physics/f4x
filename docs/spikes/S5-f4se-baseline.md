# Spike S5: real-F4SE black-box baseline. Result: GO (first slice)

Run it with `cargo xtask baseline [--headless]`:
- creates or uses the isolated profile `f4se-baseline` (symlink farm of vanilla data, F4SE 0.6.23 runtime files and `.pex`, its own prefix);
- loads `probes/f4se-probe` as an ordinary F4SE plugin and starts `f4se_loader.exe`;
- the probe writes `probe.jsonl`, and the game exits 8 s after `GameLoaded`;
- the artifact is copied to `baselines/1.10.163/probe-f4se.jsonl`.

The probe only observes F4SE from the plugin side: values passed, callbacks, threads, and stacks into game code (module+RVA). It never inspects F4SE's DLL.

## Observed facts (F4SE 0.6.23, GOG 1.10.163, 2026-11-21)

**`F4seInterface`**

| Field | Value |
|---|---|
| `f4se_version` | `0x00060170` (0.6.23.0) |
| `runtime_version` | `0x010A0A30` (1.10.163.0) |
| `editor_version`, `is_editor` | 0 |
| `GetReleaseIndex()` | 25 |
| `GetPluginHandle()` | 1 (the only plugin); the same value in Query and Load |

**`GetPluginInfo`**
- Returns null during Load for `"f4x_probe"`, `"F4SE"` and an unknown name.
- Whether it is filled later is still to be probed.

**`QueryInterface`**

| id | Interface | `interface_version` |
|---|---|---|
| 1 | Messaging | 1 |
| 2 | Scaleform | 1 |
| 3 | Papyrus | 2 |
| 4 | Serialization | 1 |
| 5 | Task | 2 |
| 6 | Object | 1 |
| 7 | Trampoline | 1 |

- ids 0 and 8–12 return null. F4SE logs `unknown QueryInterface` for each.

**`RegisterListener`** returns true for both sender `"F4SE"` and a null sender.

**Load-time ordering**, all on the injected loader thread (not the game's main thread):
1. Query, then Load.
2. `PostLoad`, then `PostPostLoad`, dispatched synchronously right after all plugins load (~2 ms), before the game's own startup.
3. Both carry `data = null`, `data_len = 0`.

**Tasks**
- The UI task runs on the game's main thread at ~17.6 s, before the main loop's first `PeekMessageA`.
  - Called from `Fallout4.exe+0x20432c5`.
- The main task runs on the main thread inside the loop (third pump).
  - Called from `Fallout4.exe+0xd39710`.
- Both are destroyed right after `Run` through vtable slot 0 with flags = 1 (scalar delete).

**Scaleform callbacks**
- One call per movie: 4 by the main menu.
- They come from the main thread and from another thread (movies load off-thread).
- The first comes from `Fallout4.exe+0x2110c5d`, which is likely the movie-load path.

**Papyrus `Register` callbacks** fire on a non-main thread at ~18.5 s.

**`GameDataReady` then `GameLoaded`**
- They arrive back to back on a worker thread at ~22.6 s, when the main menu is ready.
- `GameDataReady` passes its bool **in the pointer**: `data = 0x1`, `data_len = 1`. Hosts must never dereference it, and the probe originally crashed doing so.
- Stack: `Fallout4.exe+0xd3f26d` ← `+0xd535a9` ← `+0x1b1cfed`.

**Not seen on the way to the main menu:** `InputLoaded` and `GameDataReady(false)`. The plan expected both.

**F4SE's own log** reports branch and local pool use ("allocated 268 bytes from branch pool", "345 from local pool").

## Still to capture (later phases, needs in-game automation)
- Save, load and new-game messages and their payloads.
- The co-save grammar (patterned records).
- The `root.f4se` member snapshot.
- `GetPluginInfo` after load.
- Multiple-plugin handle numbering and load order.
- `Dispatch` between plugins.

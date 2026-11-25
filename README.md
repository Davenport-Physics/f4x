# f4x

An MIT-licensed script extender for Fallout 4, written in Rust: a drop-in replacement for
F4SE that existing mods can use unchanged, built so that mods can be tested automatically,
in-game.

**Status:** early. It targets Fallout 4 **1.10.163** (GOG, and Steam copies on that
version) and **next-gen 1.11.240** (Steam). GOG has no next-gen build. It is developed and
tested under Proton on Linux (GOG via Heroic, Steam through the Steam client); native
Windows is untested.

## What it does

- **Runs F4SE mods.** It loads F4SE plugin DLLs through the same plugin API: messaging,
  Papyrus, Scaleform, serialization, tasks and trampolines. Co-saves (`.f4se`) are written
  in F4SE's format, so saves move freely between the two.
- **Implements F4SE's script and UI surface.** This covers:
  - all 224 of F4SE's Papyrus natives, plus its 5 scripted functions. The 16 that F4SE
    runs as latent functions (the calling script waits for the main thread) are latent
    here too, and return in the same frame as under F4SE;
  - every F4SE event: control, key, camera, furniture, external events;
  - `root.f4se` in every menu (MCM works): `GetMembers`, `AllowTextInput`,
    `SendExternalEvent`, `CallFunctionNoWait`, `CallGlobalFunctionNoWait`,
    `GetDirectoryListing`, `MountImage`/`UnmountImage` (`img://` images), `version`,
    plugin objects, and `onF4SEObjCreated(codeObject)` on the root and its first-level
    children;
  - the HUD tint hook: a display object's `onApplyColorChange(r, g, b, brightness)` may
    return `[r, g, b, brightness]`;
  - custom menus (`UI.RegisterCustomMenu`), with F4SE's `UI:MenuData` defaults;
  - `Interface\Translations\<plugin>_<language>.txt` for each active plugin in
    `plugins.txt` (UCS-2 LE with BOM, `$KEY<TAB>text`, existing keys kept);
  - `Data\F4SE\CustomControlMap.txt`, which replaces the game's control map (lines that
    would crash the game are skipped);
  - `bEnableGFXLog=1` under `[Interface]` in `Data\F4SE\f4se.ini`: Scaleform's log goes
    to f4x's log;
  - the version on the main menu (`v (f4x <version>) 1.10.163.0.0`) and the console's
    `GetF4SEVersion` (plus `GetF4XVersion`);
  - F4SE's messages from the same points in the game (save, load, delete, new game,
    data ready), Papyrus registration right after the game's own natives, and camera
    events from every `PlayerCamera::SetState`.

  For a game without F4SE's own script files, f4x generates the declarations F4SE adds
  (functions, structs with their defaults, all 172 constants) from the user's vanilla
  scripts.
- **Adds:**
  - console commands with typed arguments;
  - crash reports (`Documents/My Games/Fallout4/f4x/`), symbolized by the `f4x` CLI;
  - structured logs, and a map of which module patched what.
- **Testing and automation:**
  - a headless game launcher;
  - an in-game test runner;
  - a control interface (CLI, JSON-RPC, MCP) to drive the game: console, input, script
    calls, menu inspection, screenshots;
  - a differential suite that runs the same scenarios under F4SE and f4x and compares them.

## Compatibility

f4x is built once per game generation (cargo feature `ng`). The loader picks the runtime
that matches the game exe, so one install works on either.

- On 1.10.163 f4x behaves like F4SE 0.6.23.
- On next-gen it behaves like F4SE 0.7.9: its declarative plugin versions and
  compatibility rules, preloading, `GetSaveFolderName`, `AddTaskPermanent`,
  `Form.GetEditorID` and gamepad-button events (`RegisterForGamepadButton` →
  `OnGamepadButtonUp`). As with real F4SE 0.7.x, old-gen plugin DLLs don't load on
  next-gen; plugins must be built for it.

Checked against F4SE 0.6.23 with `cargo xtask compat`, with these installed: Buffout 4 (with
xSE PluginPreloader), LooksMenu, MCM, Settle Anywhere, BackportedBA2Support, Sim
Settlements, Start Me Up and others. The checks compare:
- co-saves, byte for byte;
- the menu-side `f4se` object, including plugin objects;
- script results and plugin save data;
- Papyrus errors;
- screenshots.

Every F4SE native is also compared call by call against real F4SE:
`tools/f4se_natives_check.py` calls each one with arguments generated from its declared
types, under both hosts, and diffs the results.

On next-gen (Steam 1.11.240) the same checks run against F4SE 0.7.9
(`tools/compat/compat.py --ng`, and the natives check with its 0.7.9 signatures):
- the natives agree on 225 of 228. The other 3 are F4SE crashing on
  `Form.SetEnchantmentValue` and the state reset that crash causes, as on 1.10.163;
- the plugin ABI was compared over 51 test plugins with different version data, and
  agrees on everything except the object interface below;
- co-saves (including F4SE 0.7's gamepad table) and `root.f4se` are compared as above.

Next-gen engine addresses come from f4x's own analysis of the exe. No Address Library
was used. Its layouts come from the last MIT-licensed commit of the next-gen
CommonLibF4 fork (see `THIRD_PARTY.md`).

Beyond results, the hook points were compared: the places where F4SE 0.6.23 patches
the game's code (found with the `patches` RPC, never by reading F4SE) were each traced
to a feature, and f4x implements every one of those features except the object
interface below.

Not implemented:
- F4SE's object interface (plugin interface 6: delay functors and persistent objects for
  plugins). Its objects are defined only in F4SE's source, so plugins get "not
  available". CommonLibF4-based plugins ask for it but cannot use it; none of the tested
  plugins needs it.

## Install

Build (see below), then either:

- run `cargo xtask install-game` (GOG via Heroic) or
  `cargo xtask install-game --game-dir "<Steam library>/steamapps/common/Fallout 4"`.
  It copies `f4x_loader.exe`, `f4x_runtime.dll` and `f4x_runtime_ng.dll` into the game
  folder and replaces `f4se_loader.exe` with the f4x loader (backed up). On GOG it points
  Heroic at the loader. On Steam it replaces `Fallout4Launcher.exe`, which Steam's Play
  button starts, with the loader (backed up). Undo with `cargo xtask uninstall-game
  [--game-dir ...]`. Or
- copy the files into the game folder by hand and launch `f4x_loader.exe` instead of
  `f4se_loader.exe`.

Plugins stay in `Data/F4SE/Plugins`.

## Build and test

Requirements:
- Rust;
- [cargo-xwin](https://github.com/rust-cross/cargo-xwin) for the Windows build;
- clang, only to regenerate engine tables;
- Proton, umu and gamescope for in-game tests.

Paths are set with environment variables or `~/.config/f4x/env`; see
`crates/f4x-host/src/env.rs`.

```sh
cargo test --workspace                                    # host-side tests
cargo xwin build --release --target x86_64-pc-windows-msvc \
  -p f4x-loader -p f4x-runtime -p f4x-agent-f4se -p f4x-smoke-tests
cargo xwin build --release --target x86_64-pc-windows-msvc \
  --target-dir target/ng -p f4x-runtime --features f4x-runtime/ng   # next-gen runtime
cargo build -p f4x-cli                                    # the `f4x` CLI
target/debug/f4x test --headless \
  --plugin target/x86_64-pc-windows-msvc/release/f4x_smoke_tests.dll
cargo xtask compat                                        # F4SE vs f4x, needs F4SE installed
```

`CLAUDE.md` documents the layout and the agent/test interface. `docs/` has the plan and
the formats f4x observed.

## How it was made

- **Clean room.** F4SE's source code was never read. Its terms don't permit modified
  versions or redistribution, so nothing in f4x is derived from it. The sources used
  instead:
  - [CommonLibF4](https://github.com/Ryan-rsm-McKenzie/CommonLibF4) (MIT) for engine
    layouts, address IDs and the plugin ABI; for next-gen, the last MIT commit of its
    fork [libxse/commonlibf4](https://github.com/libxse/commonlibf4) (layouts only,
    parsed with our own shims for its GPL helper library, which was never used);
  - our own reverse engineering of `Fallout4.exe`;
  - black-box observation of the F4SE binary. Probe plugins recorded the messages it
    sends and when, the data and files it produces, its co-saves, and its `root.f4se`
    object. F4SE's DLLs were not disassembled;
  - F4SE's distributed `.pex` files, for Papyrus function signatures only;
  - public documentation: F4SE's published changelog, as the list of features to match,
    and [xEdit](https://github.com/TES5Edit/TES5Edit)'s record definitions (MPL-2.0) for
    the names of four weapon flags;
  - the Address Library, only as a local hint to locate functions, never shipped.

  Every fact is logged with its source in `provenance/`, and `cargo xtask provenance-lint`
  rejects references to F4SE's source.
- **Compatibility first.** Behaviour was matched to F4SE by running identical scenarios
  under both and comparing results: byte-for-byte co-saves, menu objects, script results.
  Mods should not be able to tell the difference.
- **No hard-coded offsets.** Engine functions are found by masked byte signatures, checked
  against the exe, so f4x tolerates other mods patching the game. It is also meant to make
  new game versions easier to support.
- **Developed with an AI coding agent** (Anthropic's Claude Code), working under the
  project owner's direction, with the testing interface above as its feedback loop.

## License

MIT; see `LICENSE`. Third-party notices are in `THIRD_PARTY.md`. f4x is not affiliated with
the F4SE team or Bethesda. You need your own copy of Fallout 4.

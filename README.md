# f4x

An MIT-licensed script extender for Fallout 4, written in Rust: a drop-in replacement for
F4SE that existing mods can use unchanged, built so that mods can be tested automatically,
in-game.

**Status:** early. It targets Fallout 4 **1.10.163** (pre-"next-gen"). It is developed and
tested under Proton on Linux (GOG version via Heroic); native Windows is untested.

## What it does

- **Runs F4SE mods.** It loads F4SE plugin DLLs through the same plugin API: messaging,
  Papyrus, Scaleform, serialization, tasks and trampolines. Co-saves (`.f4se`) are written
  in F4SE's format, so saves move freely between the two.
- **Implements F4SE's script and UI surface used by mods.** This covers `root.f4se` in every
  menu (MCM works), external events, control events, and F4SE Papyrus functions. Of F4SE's
  ~224 Papyrus functions, 9 are implemented so far (plus the 2 control events): the ones
  the tested mods use.
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

Checked against F4SE 0.6.23 with `cargo xtask compat`, with these installed: Buffout 4 (with
xSE PluginPreloader), LooksMenu, MCM, Settle Anywhere, BackportedBA2Support, Sim
Settlements, Start Me Up and others. The checks compare:
- co-saves, byte for byte;
- the menu-side `f4se` object, including plugin objects;
- script results and plugin save data;
- Papyrus errors;
- screenshots.

Not yet implemented:
- most F4SE Papyrus functions;
- F4SE's object interface (plugin interface 6);
- the `DeleteGame` message;
- next-gen game versions.

## Install

Build (see below), then either:

- run `cargo xtask install-game`. It copies `f4x_loader.exe` and `f4x_runtime.dll` into the
  game folder, replaces `f4se_loader.exe` with the f4x loader (backed up), and points
  Heroic at it. Undo with `cargo xtask uninstall-game`. Or
- copy the two files into the game folder by hand and launch `f4x_loader.exe` instead of
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
    layouts, address IDs and the plugin ABI;
  - our own reverse engineering of `Fallout4.exe`;
  - black-box observation of the F4SE binary. Probe plugins recorded the messages it
    sends and when, the data and files it produces, its co-saves, and its `root.f4se`
    object. F4SE's DLLs were not disassembled;
  - F4SE's distributed `.pex` files, for Papyrus function signatures only;
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

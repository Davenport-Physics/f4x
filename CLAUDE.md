# f4x

f4x is an MIT-licensed Rust script extender for Fallout 4. It is a loader and an injected runtime. It is compatible with F4SE plugins, F4SE Papyrus scripts, Scaleform UI mods and co-saves. On top of that it has a native Rust engine API and an in-game control server for automated testing and observability.

The plan is in `docs/PLAN.md`. Read it before starting a phase.

## Clean-room rules (MUST follow)

f4x must never contain code or facts taken from F4SE's source, because F4SE has no usable license.

- **Never read F4SE source.** If a copy exists on the machine, it lives outside this repository in a folder named `f4se-quarantine`. Deny rules are in `.claude/settings.json`; per-machine absolute-path rules go in `.claude/settings.local.json`, which is not committed. Don't try to get around them: no `cat`, `grep` or `find` into it, and no web copies of F4SE source either, e.g. GitHub `ianpatt/f4se`.
- **Don't disassemble or decompile F4SE's DLLs.**
  - You may observe F4SE as a black box through our probe plugins: the messages it sends and when, the data it passes, the files it writes, the versions it reports, and the `root.f4se` Scaleform object.
  - Return addresses into *game* code are fine, and so is the `patches` RPC (which bytes of *game* code F4SE changed, and into which module they jump). Never follow such a jump into F4SE's code.
- **F4SE's public docs** (its website's readme and `whatsnew` changelog) may be read and cited as `doc`: they say which features exist. Behaviour still comes from black-box observation.
- **F4SE's distributed `.pex` files** may be read for function signatures only (names, parameter and return types). Never copy `.psc` comments. Constant values and struct defaults come from CommonLibF4, the exe, public docs (e.g. xEdit's definitions) or F4SE's observed behaviour (`crates/f4x-pex/data/constants/`, `struct-defaults/`), never from F4SE's files.
- **Treat `../f4se-rs` as untrusted.** It is the earlier Rust SDK. Several of its files cite F4SE source:
  - `ffi.rs`, `version.rs`, `declaration.rs`, `messaging.rs`, `patch.rs`, `rel.rs`, `game/mod.rs` offsets, `tools/mock-f4se`.
  - Don't copy them. Re-derive each from the clean sources below.
  - `pe.rs`, `game/rtti.rs` and `error.rs` may be ported after review.
- **The next-gen CommonLibF4 fork** (`../commonlibf4-ng`, libxse/commonlibf4) was relicensed to GPL-3 after commit `40a93f69` (2026-07-26). Use only that commit (`git -C ../commonlibf4-ng show 40a93f69:<path>`, or the working tree detached there). Never read a later commit. Never read its `lib/commonlib-shared` submodule (GPL since its first commit) or any copy of it. `crates/f4x-codegen/shim-ng/` holds our own MIT stand-ins. Cite it as `{"kind":"clf4","repo":"libxse/commonlibf4","commit":"40a93f69",...}`.
- **Address Library** `.bin` files may only confirm an address that our own recipe found. Never commit or ship bin data. There is no next-gen bin here: next-gen addresses are all our own RE (`crates/f4x-engine-sys/wanted-ng_1_11_240.txt`).
- **Allowed sources, each recorded in `provenance/*.jsonl`:**

  | Tag | Source and what to record |
  |---|---|
  | `clf4` | `../CommonLibF4` (MIT): commit + file:line |
  | `re` | Our own analysis of `Fallout4.exe`: tool, args, output hash |
  | `bb` | Black-box F4SE observation: probe, session, artifact hash |
  | `f4se-pex` | F4SE's `.pex` signatures |
  | `doc` | Public docs: URL + date |
  | `data` | The user's game files, processed locally, never committed |
  | `al-hint` | Address Library hint, never committed |

- **Never write these** in code or comments: `PluginAPI.h`, `f4se_version.h`, `RelocAddr`, `BranchTrampoline`, or paths into F4SE source. `cargo xtask provenance-lint` rejects them.

## Hard design rule: engine internals only

Rust engine bindings call engine functions and structures directly. They **never** call the implementation of a vanilla Papyrus native function. Natives are only used as a catalog, to find which engine function they wrap. Calling Papyrus through the VM is allowed only in oracle tests and when the user explicitly asks to drive scripts.

## Layout

| Path | What |
|---|---|
| `crates/*` | Libraries and the in-game cdylibs (`f4x-runtime`, `f4x-loader`) |
| `tools/*` | Host-side tools |
| `probes/*` | F4SE-mode black-box probe plugins |
| `xtask/` | Build, staging, spikes, verification, lints |
| `provenance/` | Fact logs |
| `baselines/` | Recorded black-box artifacts from real F4SE |

Crates that only make sense on Windows compile to empty stubs on other targets, so `cargo test --workspace` works on the Linux host.

## Two game generations

f4x supports **1.10.163** ("OG": GOG, and Steam copies on that version) and **next-gen 1.11.240** ("NG": Steam). GOG publishes no next-gen build. The user plays on GOG 1.10.163, so never regress OG.

- **One build per generation.** Cargo feature `ng` selects:
  - `f4x_engine_sys::layout`: `og_1_10_163` or `ng_1_11_240`;
  - `f4x_engine_sys::addresses`: the same names in both tables;
  - `f4x_engine_sys::NG` and `f4x_engine::pick!(og, ng)` for hand-written per-generation values.
- Next-gen builds go to `target/ng`. The loader picks `f4x_runtime_ng.dll` (next to it) for a next-gen exe, and a runtime refuses engine work on the other generation.
- **Next-gen tables:**
  - layouts: `cargo run --release -p f4x-codegen -- dump --flavor ng`, `verify --flavor ng --exe <exe>`, then `tables --flavor ng`. OG field names renamed in NG get aliases in `crates/f4x-codegen/ng-aliases.txt`.
  - addresses: `wanted-ng_1_11_240.txt`, then `cargo run -p f4x-codegen -- addresses --flavor ng --exe <exe>`.
- **RE tools** (`cargo build --release -p f4x-re`):
  - `f4x-re match|suggest|compare|query|func|disasm|xrefs|strings`;
  - `f4x-vtcmp` (vtables slot by slot);
  - `f4x-insngrep`.

  Identical-looking functions are common: confirm a match by its callers or vtable slot, not by its shape.
- **F4SE versions:** next-gen F4SE is 0.7.9 (Nexus only). Its pinned archive goes into the f4x cache from `F4X_F4SE_NG_ARCHIVE`. Only its runtime files and `.pex` are extracted, never `src/` or `.psc`. Its signature inventory is `crates/f4x-pex/data/f4se-0.7.9-signatures.json`; natives added in 0.7 are listed in `papyrus_natives::NG_ONLY`.
- **Steam builds** start through the Steam client: the `steam.exe` helper of Proton under `SteamLinuxRuntime_sniper`, not umu (`crates/f4x-host/src/launch.rs`).
  - Steam must be running and logged in.
  - Each next-gen session shows the user as playing Fallout 4 on Steam, so keep sessions short.
  - The runtime starts at the CRT's last call before `WinMain` on DRM-stub exes, and preloads at `_initterm_e` on next-gen (`crates/f4x-runtime/src/bootstrap.rs`).

## Running the game (agents)

**Build the Windows side:**

```sh
cargo xwin build --release --target x86_64-pc-windows-msvc -p f4x-loader -p f4x-runtime -p f4x-agent-f4se -p f4x-smoke-tests
cargo build -p f4x-cli                       # host CLI: target/debug/f4x
# next-gen (into target/ng; the loader is shared):
cargo xwin build --release --target x86_64-pc-windows-msvc --target-dir target/ng \
  -p f4x-runtime -p f4x-agent-f4se -p f4x-smoke-tests -p f4x-probe \
  --features f4x-runtime/ng,f4x-agent-f4se/ng,f4x-smoke-tests/ng,f4x-probe/ng
```

**Profiles.** `cargo xtask profile create NAME --host f4se|f4x [--game-dir DIR]`. A profile remembers its install and store, and `f4x launch --profile` picks the matching build. Next-gen profiles are `vanilla-f4x-ng` (f4x) and `f4se-ng-baseline` (F4SE 0.7.9).

**In-game tests.** Each run launches the game headless, runs the tests, quits, and exits 0 only if all of them pass:

```sh
f4x test --headless --plugin target/x86_64-pc-windows-msvc/release/f4x_smoke_tests.dll --skip expected_
f4x test --host f4se ...                     # same tests under real F4SE (differential)
```

**Interactive session.** Always pass `--session`: other agents may run sessions too.

```sh
S=$(f4x launch --headless --main-loop --test-mode | tail -1)
f4x rpc --session "$S" state
f4x rpc --session "$S" engine.player
f4x screenshot --session "$S" -o shot.png
f4x quit --session "$S"
```

**Engine introspection** (`engine.*`, read-only):
- `engine.player`, `engine.form {id}`, `engine.files`, `engine.forms.count`
- `engine.forms.of_type {type, limit}`, `engine.cell.refs {limit}`, `engine.singletons`

**Input** (engine-level, works under both hosts):
- `input.press {event, key?, device?, hold_frames?}` puts a real `ButtonEvent` into the engine's input queue. The press is down on the next frame and up `hold_frames` later.
  - Answer a message box: `{"event":"Accept","key":28}`.
  - Loading a save while mods are active always asks "Mods are currently loaded… continue?", so tests must answer it.
- `input.recent` lists the last button events seen, with their user-event names.
- `input.press {..., repeat: true}` also sends a held event each frame in between, as a physical button does.
- `papyrus.events {filter?, since_frame?, clear?}` lists the script events and method calls queued through the VM (`SendEvent` / `DispatchMethodCall`), with frame numbers, under either host. Recording starts with the first call. Use it to see black-box what a host sends scripts, and when. Some deliveries take other paths (F4SE 0.7.9's furniture events do); for those, put tracing handlers into a test copy of a script.
- `image.dump {path?}` writes the exe with its in-memory `.text`, for offline RE of builds whose code is encrypted on disk.

**Papyrus** (for tests and when asked to drive scripts; goes through the VM):
- `papyrus.call {function: "Script.Function", self?: "form id", args?: [...], timeout_s?}`.
  - Arguments: bool, int, float (or `{"float": x}`), string, null, `{"form": "id"}`.
  - Any other type: `{"type": "Int[]", "value": [...]}`, `{"type": "script#struct", "value": {"struct": {...}}}`, `{"type": "Var", "value": {"var": v}}`.
  - Results use the same shapes: forms as `{"form", "type"}`, other objects as `{"handle", "script"}`, floats as `{"float": x}`.
- `engine.forms.all {type, limit}`: forms from the global form map, placed references included.
- F4SE natives live in `crates/f4x-runtime/src/f4se/papyrus_natives/` (one file per family) and are bound by name with types from `crates/f4x-pex/data/f4se-0.6.23-signatures.json`. Check them against real F4SE with `tools/f4se_natives_check.py` (see its docstring).
- Next-gen: `tools/f4se_natives_check.py host f4se f4se-ng-baseline target/natives-ng/f4se.json --signatures crates/f4x-pex/data/f4se-0.7.9-signatures.json`, then the same with `host f4x vanilla-f4x-ng ... --plan target/natives-ng/f4se.json`.
- Natives listed in `papyrus_natives::LATENT` (the 16 F4SE binds latent) run on the main thread at the start of the VM's frame update; the calling script waits. Their handlers may use the engine directly. Others run on VM threads: post main-thread work with `f4x_agent::pump::post`.
- Scripts only run in-game. At the main menu, calls are accepted but never finish (timeout).

**Scaleform** (a menu's movie; runs inside that menu's next `AdvanceMovie`):
- `ui.get {menu, path, fields?, depth?}`: the value at an ActionScript path, e.g. `root.f4se`.
- `ui.has {menu, path, names}`: which of `names` are members, with types. Use it to enumerate black-box.
- `ui.invoke {menu, path, method, args?, fields?}`. Numbers are passed as AS3 Numbers; use `{"int": n}` or `{"uint": n}` for code-object functions that check the type (e.g. MainMenu `BGSCodeObj.DeleteSave`). Also `{"path": "root.x"}`, `{"object": {...}}` and `{"array": [...]}`.
- `ui.menu {menu, dump?}`: a registered menu's IMenu flags, depth, input context, scale mode and alignment (custom menus included), optionally a raw dump.
- `ui.translations {keys?, prefix?, limit?}`: the Scaleform translation table. `ui.scaleform_states`, `ui.gfxlog.emit {text, id?}`.
- `engine.controlmap {contexts?, events?}`: the control map. `console.commands {filter?}`, `console.text {tail?}`.
- Dates come back with `time_ms`.

**Differential compatibility:** `cargo xtask compat [--only surface,saveload,mcm,natives]` (next-gen: `python3 -I tools/compat/compat.py --ng`).
- It runs the same scenarios under real F4SE and under f4x and diffs them: `root.f4se`, co-saves byte for byte, Papyrus logs, MCM screenshots, and the F4SE natives.
- It exits 1 on any difference. The report goes to `target/compat/report.json`.
- It needs the `user-mods-*`, `f4se-baseline` and `vanilla-f4x` profiles; see `tools/compat/compat.py`.

**MCP:** `.mcp.json` registers the `f4x` server (`tools/f4x-mcp.sh`). Smoke test it with `python3 -I tools/mcp_smoke.py tools/f4x-mcp.sh`.

**Crashes:** `f4x status --session "$S"` prints symbolized crash reports. The files are `crash-*.json` in the session directory.

**Rules for game sessions:**
- Launches queue on `~/.local/share/f4x/launch.lock`.
- f4x only uses its own prefixes and profiles under `~/.local/share/f4x`, never the user's Heroic prefix.
- Every test launch must be muted (the f4x prefixes' inis do this).
- Engine reads happen on the main thread (`f4x_agent::pump::on_main`).
- Never call virtuals through unchecked pointers read from engine fields: use `Form::checked`.

## Installing into a real game

- `cargo xtask install-game [--game-dir DIR] [--reinstall]`:
  - copies `f4x_loader.exe`, `f4x_runtime.dll` and (when built) `f4x_runtime_ng.dll` into the game folder;
  - for a Steam install (`--game-dir`), replaces `Fallout4Launcher.exe` (what Steam's Play starts) with the loader, backed up;
  - points Heroic's `targetExe` at the loader, if a Heroic config exists for `F4X_HEROIC_APP` (default: GOG Fallout 4 GOTY);
  - records everything, with backups, in `~/.local/share/f4x/install/manifest.json` (other installs: `install/<store>-<folder>/`).
- `cargo xtask uninstall-game [--game-dir DIR]` reverts it.
- `f4se_loader.exe` is replaced by a copy of the f4x loader (the original is backed up), so stale launch paths still run f4x. F4SE's DLLs stay in place, unused.
- f4x never writes into F4SE's folders (`My Games\Fallout4\F4SE` holds F4SE's and the plugins' own logs).
- Logs while playing go to `Documents\My Games\Fallout4\f4x\latest` (the run before is in `previous`).
- Only install into a user's game when they ask. Never run install or uninstall while their game is running. Heroic must be restarted to pick up the new launch target.

## Commands

```sh
cargo test --workspace            # host tests
cargo win -p <crate>              # Windows build via cargo-xwin (x86_64-pc-windows-msvc, static CRT)
cargo xtask <task>                # see xtask/src/main.rs
```

## Environment

Settings come from environment variables, else `~/.config/f4x/env` (`KEY=value` lines, per machine, not committed), else defaults for a stock Heroic + GOG install (`crates/f4x-host/src/env.rs`):

| Setting | Default |
|---|---|
| `F4X_GAME_DIR` | `~/Games/Heroic/Fallout 4 GOTY` (Fallout 4 1.10.163) |
| `F4X_SOURCE_PREFIX` | `~/Games/Heroic/Prefixes/default/Fallout 4 Game of the Year Edition` (read-only: seeds test prefixes) |
| `F4X_PROTON` | `~/.config/heroic/tools/proton/GE-Proton-latest` |
| `F4X_UMU` | `~/.config/heroic/tools/runtimes/umu/umu_run.py` |
| `F4X_STORE`, `F4X_UMU_GAMEID` | `gog`, `umu-377160` |

- **Don't modify the game install** outside f4x profiles, except through `install-game`. Write anything there through a manifest so it can be undone.

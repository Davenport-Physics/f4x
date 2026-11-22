# f4x: an MIT, Rust script extender for Fallout 4, built for agentic coding

## Status (2026-11-21)

**Phase 0 (spikes S1–S6): done.** Reports are in `docs/spikes/`.

**Phase 1 (agentic loop): done.** Exit criteria met:
- `f4x test` passes 3/3 headless on host f4x and on host f4se;
- a deliberate crash gives a symbolized fatal report, and the session is marked as crashed;
- the live MCP smoke test passes.

**Phase 2 (type DB + core engine): in progress.**
- Done:
  - generated offset tables for 937 classes (`f4x-engine-sys`), verified against the exe (400 of 411 vtable lengths match);
  - `f4x-engine` forms, references, data handler and player, found through RTTI and structural recipes, with no function addresses;
  - `engine.*` RPC.
- Next:
  - recipe engine plus non-virtual function addresses (needs the user's Address Library bin as a hint);
  - typedb/provenance for found globals;
  - console command execution;
  - event sinks.

**Since the first status update:**
- **Phase 3:**
  - F4SE plugin host: the user's DLLs load, and BackportedBA2Support writes the same patches as under F4SE.
  - Save/load lifecycle and co-saves, byte-identical to F4SE's.
  - GameDataReady comes from the engine's own data-ready call.
- **Console commands with typed arguments:**
  - The user asked for this: F4SE 0.6.23 has no helper for reading console arguments.
  - Plugins register commands through the service table / SDK (`ctx.console_command`).
  - f4x takes free slots (named commands with release no-op handlers).
  - Arguments are decoded by the engine's own `Script::ParseParameters`: int, float, string, reference, form, ….
  - Works under f4x and under real F4SE (through the agent).
- **Settle Anywhere's F4SE natives (Phase 4, first slice):**
  - Implemented on engine internals: EncounterZone Is/SetWorkshop and Is/SetNeverResetable, Location.GetEncounterZone, and Register/UnregisterForControl with OnControlDown/Up.
  - Rust natives are bound through the VM's own `NativeFunctionBase::Call`.
  - Control registrations are saved in F4SE's co-save format.
    - On the user's mod set, co-saves are byte-identical to F4SE's across save → load → save.
  - A differential run with `tools/natives_diff.py` (`papyrus.call`, vanilla profiles, QASmoke) gave byte-identical results under F4SE and f4x:
    - every Location: GetEncounterZone(false) and GetEncounterZone(true), 487 locations;
    - every EncounterZone: IsWorkshop and IsNeverResetable, 362 zones;
    - a SetWorkshop round trip.
  - Script declarations: f4x patches the user's own vanilla scripts into f4x profiles. `pex declare --verify` finds 0 mismatches with F4SE's signatures.
- **Agent:** `input.press`/`input.recent` (engine-level input injection) and `papyrus.call`.
- **Scaleform (Phase 5, first slice):**
  - `root.f4se` is injected and plugin Scaleform callbacks run, both from where F4SE does it.
  - It implements AllowTextInput, GetDirectoryListing, SendExternalEvent, plugins and version (`docs/formats/scaleform-f4se.md`).
  - The surface was captured with `ui.*` on both hosts; the captures are byte-identical.
  - MCM's menu opens and lists the same mods as under F4SE.
- **NewGame:** sent from the same call site as F4SE, with the same quest in the same state (`docs/formats/cosave.md`).
- **Next:**
  - Papyrus external events (Register/UnregisterForExternalEvent and the EXEV co-save section);
  - the remaining F4SE natives, ordered by what mods use.

Plan deviations that came out of the spikes:
- Tests run in dedicated Proton prefixes (`~/.local/share/f4x`), never the user's prefix.
- The loader passes the DLL path through a shared section.
- Engine objects without RTTI are found by structural recipes.

## Context

F4SE has no usable license ("no redistribution or modified versions", and plugins must publish source), so we can't build on its code. We want a replacement that:
- is MIT licensed and written in Rust;
- loads existing F4SE mods;
- gives Rust a complete native API to the game engine, where no function has to go through the Papyrus VM;
- is built so agents can develop it: automated in-game tests of mods, and observability as a first-class feature.

What exists today:
- `./f4se-rs`: a Rust SDK for F4SE plugins. Uncommitted, and something else was editing it during the survey. Many parts of it cite F4SE source.
- `./CommonLibF4`: MIT C++ reverse-engineering library, covering 1.10.163 only.
- `./f4se`: F4SE's source. Off limits.
- `./mod-setup`: Python script that installs F4SE 0.6.23 safely.
- Game: GOG Fallout 4 GOTY **1.10.163**, launched by Heroic through gogdl, umu and GE-Proton (DXVK). Prefix is `~/Games/Heroic/Prefixes/default/Fallout 4 Game of the Year Edition`.
- Toolchain: Rust 1.99.1, cargo-xwin, clang 22, gamescope.

## Decisions made with the user

| Topic | Decision |
|---|---|
| Name | `f4x`, a new workspace at `./f4x`. f4se-rs is read-only reference: audit it and port parts. |
| Game builds | Old-gen 1.10.163 (GOG + Steam) is implemented and verified. Next-gen (1.10.98x / 1.11.x) is designed now and verified later, when an exe is available. |
| Engine surface | Engine internals only. Vanilla Papyrus natives are never called from bindings; they serve only as a catalog. Every area is in scope: forms/refs/actors/inventory, UI/Scaleform/input, the Papyrus VM as a subsystem, scene graph/render/physics. |
| F4SE compatibility | Native DLL plugins, F4SE Papyrus scripts, Scaleform UI mods, and co-save interop in both directions. |
| Agent interface | In-game control server, a Linux `f4x` CLI, and an MCP server. Runs on the desktop or headless under gamescope. |
| Address Library | Hint-assisted. The user's locally installed `.bin` may serve only as a locator hint. Every committed address carries our own recipe that re-finds it. Bin data is never committed or shipped. |
| `./f4se` source | Moved out of the tree, plus a deny rule. |

## Clean-room rules (go into `f4x/CLAUDE.md` and are enforced in CI)

**Allowed sources.** Each fact is tagged in `provenance/*.jsonl`:

| Tag | Source and what to record |
|---|---|
| `clf4` | CommonLibF4 (commit + file:line), with attribution in `THIRD_PARTY.md` |
| `re` | Our own analysis of `Fallout4.exe` (tool, args, output hash) |
| `bb` | Black-box observation of the real F4SE 0.6.23 binary through our probe plugins (interface and timing only) |
| `f4se-pex` | Signatures from F4SE's distributed `.pex` (no comments) |
| `doc` | Public docs (URL + date) |
| `data` | The user's game files, processed locally, never committed |
| `al-hint` | Address Library hint, verified locally, never committed |

**Forbidden:**
- the F4SE source tree;
- disassembling F4SE's DLL internals (return addresses into *game* code are fine);
- copying `.psc` comments.

**`cargo xtask provenance-lint`** fails if:
- any DB entry has no source;
- banned citations appear (`PluginAPI.h`, `f4se_version.h`, `RelocAddr`, `BranchTrampoline`, paths into `f4se/`).

**Rewrites from tainted sources:** each tainted f4se-rs item is rewritten in a fresh session, from a written spec, by an agent that never opens the tainted file (table at the end).

## Architecture

```
f4x/
  crates/
    f4x-abi         no_std #[repr(C)] F4SE-compat ABI (from CommonLibF4 Interfaces.h + community PluginVersionData)
                    + the f4x C-ABI service table (struct_size + version, append-only)
    f4x-pe          PE/.pdata/.reloc/RTTI scanning (ported from f4se-rs pe.rs, rtti.rs after audit)
    f4x-typedb      schema + per-build data: symbols → RVA + recipe + provenance; layouts; accessor offset tables
    f4x-engine-sys  generated layouts, vtables, per-build accessor offsets
    f4x-engine      handwritten safe API: containers (BSTArray/BSTHashMap/BSFixedString/NiPointer),
                    game allocator (GameBox), thread-affinity typing (MainThread token), forms/refs/actors/…
    f4x-papyrus     VM types, Rust NativeFunction, script calls + IStackCallbackFunctor, F4SE native declaration DB
    f4x-agent       control server, test runner, tracing/JSONL, flight recorder, crash capture, screenshots, patch map
    f4x-runtime     cdylib: two-stage bootstrap, plugin host (F4SE + f4x-native), F4SE compat interfaces, hook manager
    f4x-loader      f4x_loader.exe (suspended CreateProcess + LoadLibrary stub) + f4x-proxy cdylib (winhttp.dll mode)
    f4x-sdk         native plugin SDK (successor to f4se-rs crate); "F4SE mode" (runs on real F4SE) and "f4x mode"
    f4x-macros      plugin!, #[f4x::test], #[papyrus_native]
    f4x-proto       JSON-RPC types + JSON schema
    f4x-cosave      .f4se reader/writer (byte-for-byte pass-through of unknown chunks)
    f4x-pex, f4x-ba2  pex read/write (full: objects, structs, props, states, debug info); BA2 GNRL reader
    f4x-re          host: iced-x86 analyzers, recipe engine, native-body analyzer, OG→NG function matcher
    f4x-codegen     host: clang JSON AST/record layouts of CommonLibF4 → typedb → Rust
    f4x-cli         Linux: launch / profile / test / logs / console / screenshot / install / coverage / mcp (rmcp)
  probes/           F4SE-mode black-box probe plugins (message timeline, co-save patterns, f4se-object snapshot)
  tests/compat/     a CommonLibF4 ExampleProject plugin built with clang-cl + xwin; canary-mod manifests
  tools/mock-host   rewritten mock F4SE host (from f4x-abi + bb facts, not the old mock-f4se)
  provenance/       JSONL fact logs
  xtask/            build-windows, stage, verify-db, provenance-lint, ci
```

**Design rules.**
1. **Ownership of shared state.** `f4x-engine` is statically linked into every plugin, so it holds only caches. Anything that must be unique is owned by the runtime and reached through the C-ABI table: hooks, event sinks, native registrations, task queues, log routing, the test registry, Scaleform handlers.
2. **Fields behind accessors.** Generated accessors read per-build offset tables, so next-gen layout changes are data, not source changes. Where they apply, layouts are also checked at compile time with `offset_of!` asserts.
3. **Engine code on the main thread.** Engine calls run only on the main-thread pump, enforced with a `MainThread` token type. The control server enqueues work for the pump and awaits the result.
4. **Inbound boundaries.**
   - Every inbound boundary is wrapped in `catch_unwind`.
   - Build with `+crt-static`.
   - The crash handler uses a vectored handler that only records (it never claims the unhandled-exception filter), so Buffout 4 keeps working.
5. **MSVC ABI in codegen.** Member functions that return user-defined types are lowered to `(this, *mut Ret, …)`.
6. **Coexisting with C++ plugins.**
   - Large trampoline pools within ±2 GB of the exe, handed out per plugin; f4x's own hooks use a separate pool.
   - Hooks go at call sites as rel32 call/jmp, so other plugins can chain onto them.
   - f4x reports F4SE version 0.6.23 on old-gen; the exact values are confirmed by the bb probe.
   - f4x-only features sit behind a new `QueryInterface` id.
7. **Hooks vs engine events.** Prefer engine event sinks (`BSTEventSource` found by RTTI name) over code patches for lifecycle triggers, because they survive build changes.

## Phases. Every exit criterion is a command an agent can run.

### Phase 0: bootstrap and spikes
**Repo setup:**
- Move `./f4se` (F4SE source) out of the tree, to a `f4se-quarantine` folder outside the repository.
- Create `f4x/` (git init, MIT `LICENSE`, `THIRD_PARTY.md`, `CLAUDE.md` with the clean-room rules).
- Add `.claude/settings.json` deny rules for Read/Grep/Glob on the quarantine path.
- Port `.cargo/config.toml` (crt-static, xwin alias) and `rustfmt.toml` from f4se-rs.

**Spikes.** Each is go/no-go and has its own script.

| Spike | What it does | Exit |
|---|---|---|
| S1 launch + inject | Minimal loader + DLL under GE-Proton through umu: heartbeat file, TCP ping, frame hook. Session port/token file reached via `Z:`. Cargo target dir exposed with `PRESSURE_VESSEL_FILESYSTEMS_RW`. Ini settings `bAlwaysActive=1`, empty `sIntroSequence`, winedbg crash dialog off, wineserver watchdog. | `cargo xtask spike-launch [--headless]` returns `{injected, tcp, frame_hook}` all true within 90 s, 3 runs in a row, on both desktop and `gamescope --backend headless` |
| S2 clang over CommonLibF4 | `clang++ --target=x86_64-pc-windows-msvc -fms-compatibility -std=c++20 -fsyntax-only` with `-ast-dump=json`, `-fdump-record-layouts` and `-fdump-vtable-layouts`, against the xwin CRT/SDK | `RE/Fallout.h` compiles with zero errors; all 926 `static_assert`s hold; layouts are dumped |
| S3 native catalog | Hook GameVM `BindNativeMethod` (vtable slot 0x1B) and dump script, function name, callback RVA (offset confirmed by our own RE) | Every callback is a `.pdata` function start; the count matches the natives declared in the vanilla `.pex` |
| S4 pex/BA2 | Extract `Fallout4 - Misc.ba2` to scratch and round-trip it | All 7,875 vanilla `.pex` round-trip byte-identical |
| S5 real-F4SE baseline | `f4x profile create f4se-baseline`: a symlink-farm game dir with its own `Data/F4SE`, `Data/Scripts` and save path. F4SE 0.6.23 installed using mod-setup's sha-pinned, no-overwrite method. A probe records the message timeline (thread, return chain, data, frame, state predicates), patterned co-saves, the `root.f4se` member/type snapshot, and the reported versions. | Artifacts written as JSON under `baselines/1.10.163/` |
| S6 C++ compat canary | Build CommonLibF4 `ExampleProject` on Linux with clang-cl + lld-link + xwin | The DLL loads under the S5 profile |

### Phase 1: agentic loop (before any breadth work)
**f4x-agent:**
- JSON-RPC 2.0 on 127.0.0.1, opt-in, token auth. Methods: `ping`, `state`, `console.exec`, `screenshot` (in-process via a D3D11 `Present` hook + staging copy), `logs.subscribe`, `trace.subscribe`, `input.send` (engine-level), `quit`.
- `tracing` to per-session JSONL, plus a flight-recorder ring buffer.
- Crash capture: minidump + symbolized stack (our typedb names for exe frames, PDBs for Rust frames) + the owning module.
- `MessageBoxA/W` interception, so headless runs never hang.
- Patch map: in-memory `.text`/`.rdata` (after relocations) diffed against the disk exe, with jump targets attributed to modules.

**Tests:** `#[f4x::test]` with an async executor on the frame pump (`wait_frames`, `wait_until`, timeouts). Fixtures:
- `main_menu`;
- `coc qasmoke`;
- `save:<name>`, using fixture saves created by tests.

Results are written as JSON and JUnit.

**`f4x` CLI:** `launch`, `test`, `logs`, `console`, `screenshot`, `profile`, `mcp`. The MCP server exposes the same methods as tools; register it in `f4x/.mcp.json`.

**Plugins load straight from the cargo target dir**, via a plugin-dir config/env, so nothing is copied into the game dir.

**Two hosts:** a minimal `f4x-runtime`, and the agent built in F4SE mode so it can run under real F4SE for differential runs.

**Exit:**
- `f4x test smoke --host f4x --headless` and `--host f4se` pass 3 times in a row;
- a deliberate-crash test yields a symbolized report and a failed result, not a hang;
- `cargo xtask mcp-smoke` lists the MCP tools and calls them.

### Phase 2: type DB and core engine (1.10.163)
**Deliverables:**
- `f4x-codegen` (clang JSON AST → typedb → `f4x-engine-sys`). Function IDs come out with their exact C++ signatures from `REL::Relocation<func_t>{REL::ID(n)}`.
- Recipe engine. Recipe kinds: rtti, vtable+slot, string-xref, call-chain-from-anchor, masked signature. Address Library hints are allowed only as an equality check.
- Templates are handwritten.
- Forms, `TESDataHandler`, refs/actors/`PlayerCharacter`, RTTI dynamic cast, `BSTEventSource` discovery by RTTI name, game allocator/`ScrapHeap`.

**Exit:**
- `cargo xtask verify-db --exe <Fallout4.exe>`: 100% of entries verify offline. Functions are at `.pdata` starts; vtables lead to the right RTTI COL; globals sit in `.data`/`.bss`.
- `provenance-lint` is clean.
- In-game tests: form 0x14 casts to `PlayerCharacter`; data-handler form counts are consistent; a `MenuOpenCloseEvent` sink fires.

### Phase 3: F4SE plugin host
**Deliverables:**
- Two-stage bootstrap. Stage 0 installs an IAT hook at injection. Stage 1 runs when that hook fires: patching and plugin `Query`/`Load`, after static init and before WinMain does real work. This is the same path on GOG and Steam (SteamStub), technique from CommonLibF4 `F4SEStub/steam_loader`.
- Case-insensitive, sorted plugin enumeration.
- Every interface: Messaging (including plugin-to-plugin dispatch), Scaleform, Papyrus, Serialization, Task/UI task, Trampoline, `GetPluginInfo`. Object is stubbed until it is black-boxed.
- Message triggers chosen from the S5 state predicates.
- Per-plugin call attribution in traces.
- `f4x-cosave`: grammar learned black-box; F4SE's own chunks passed through byte for byte at first.
- `tools/mock-host`.

**Exit:**
- `f4x test compat-timeline --diff baselines/` shows the same order and predicates as real F4SE;
- an in-game test round-trips co-saves F4SE → f4x → F4SE;
- the S6 canary plus user-chosen canary mods (e.g. Buffout 4) load with no patch-map conflicts.

### Phase 4: Papyrus
**Deliverables:**
- VM inspection (objects, stacks, properties), calling scripts, Rust natives through runtime-owned thunks, streams for VM errors, stacks and profiling.
- F4SE native-declaration DB taken from F4SE's `.pex` signatures, and a `.pex` generator that patches the user's own vanilla `.pex` from their BA2. It adds natives, structs, empty event functions and new objects, without duplicating anything already present. It only writes files into a profile, and nothing is committed.
- `RegisterObjectType` for new form-backed script types.
- F4SE natives implemented on `f4x-engine`. Behaviour reference: CommonLibF4 `F4SEStub/runtime` (MIT) plus differential tests.
- Registrations persisted in F4SE's chunk format, decoded by diffing co-saves.

**Exit:**
- `f4x test papyrus-diff`: every F4SE native gives the same result on both hosts;
- `f4x pex verify`: generated declarations equal F4SE's signatures.

### Phase 5: Scaleform, UI, input
**Deliverables:**
- `root.f4se` and `plugins.<name>` objects injected through `BSScaleformManager::LoadMovie` (ID 1526234), in the order AS3 first-frame code expects.
- `GFx::FunctionHandler` vtables live in runtime-owned memory.
- Menus, `UIMessageQueue`, input events and control maps.

**Exit:**
- the `f4se` object snapshot equals the baseline;
- a chosen UI mod gives the same AS3 trace and a screenshot diff below threshold on both hosts.

### Phase 6: breadth, driven by the coverage matrix
- Inventory and full ExtraData, scene graph (NiRTTI), cameras, BSGraphics renderer, Havok/raycasts, console/script command bindings (tables at IDs 901511 / 75173), settings, save/load manager, workshop, AI/packages.
- Hot reload for f4x-native test plugins only:
  - all registrations go through runtime-owned thunks;
  - unload points those thunks at an "unloaded" stub;
  - each load uses a uniquely named copy of the DLL + PDB.

**Exit:** the coverage targets per domain (below) are met, and `f4x coverage` reports no regression.

### Phase 7: next-gen (ongoing, see the strategy below)

## Completeness method under "engine internals only"

**Catalogs.** These are read from the engine and never called by bindings:
1. Vanilla natives, from the S3 hook, with signatures taken from the `.pex`.
2. F4SE natives.
3. Console and script command tables.
4. RTTI/vtables (11,888 / 10,111) and NiRTTI (347).
5. `BSTEventSource` instances, menus and settings.

**Analyzer (`f4x-re`).** Disassembles each native callback to depth 2:
- direct calls;
- virtual calls, resolved to (class, slot) through the type of `self`;
- tail jumps;
- task/latent functors, followed into their `Run`.

Each native is classified as `direct`, `virtual`, `field-access`, `composite`, `task-deferred` or `vm-only` (for example `Utility.Wait` maps to `wait_frames`). The analyzer also records thread affinity, and its field-access candidates cross-check typedb offsets.

**Verification.** A generated Rust binding plus an in-game oracle test. The test calls the vanilla native through the VM (only in tests) and the Rust binding with the same fixture, then compares results and state probes.

**`f4x coverage`** (JSON + Markdown; CI fails on regression):
- One row per catalog entry. Columns: binding, kind, address verified offline, layout verified, oracle test, thread affinity, next-gen status.
- Levels L0 to L4.
- Per-class metrics: % of `sizeof` verified, % of vtable slots named.

## Next-gen strategy (no exe yet)

- **Build identity:** PE timestamp + SizeOfImage + `.text` SHA-256, grouped into equivalence classes so Steam and GOG builds can share data.
- **Addresses are recipes, not numbers.** On old-gen, every recipe must reproduce the committed RVA. On a next-gen exe, the recipes re-run, and an OG→NG function matcher (CFG, constant and xref similarity) handles the rest.
- **Layouts:** store field-access fingerprints (instruction + function) on old-gen and re-read the offsets after matching. Get `sizeof` from allocation sites and vtable length from RTTI COL bounds.
- **ABI:** `PluginVersionData` (0x45C, from CommonLibF4 community) is implemented now. `Preload`, `GetSaveFolderName` and `AddTaskPermanent` are behind a capability flag until a black-box run against F4SE 0.7.x.
- **Exit once an exe exists:** the recipe hit rate is ≥ 90%, the rest is triaged by an agent, and the same suites pass.

## f4se-rs audit and port (f4se-rs itself is never modified)

| f4se-rs item | Action |
|---|---|
| `pe.rs`, `game/rtti.rs`, `error.rs` (`guard`), logging mechanics, task vtable glue | Port after audit. Re-cite the log dir from CommonLibF4 `log_directory` and the slot order from CommonLibF4's `ITaskDelegate` declaration. |
| `ffi.rs` | Rewrite from CommonLibF4 `F4SE/Interfaces.h` + community `PluginVersionData`. Next-gen extras come from black-box runs. |
| `version.rs`, `declaration.rs` | Runtime constants from CommonLibF4 `F4SE/Version.h`. The compatibility policy is our own, checked against a black-box matrix of probe flag variants. |
| `messaging.rs` | Payload conventions come from the S5 probe. |
| `patch.rs`, `rel.rs` | Our own design (pools, detours via iced-x86 relocation, rel32 call sites), using CommonLibF4 `REL`/`Trampoline.h` as reference. |
| `game/mod.rs` addresses | Re-derived through the Phase 2 pipeline. Never diffed against the old values in a commit. |
| `tools/mock-f4se` | Rewritten as `tools/mock-host`. |

## Key reference files (MIT)

**CommonLibF4:**
- `CommonLibF4/CommonLibF4/include/F4SE/Interfaces.h`, `src/F4SE/API.cpp` (plugin ABI, and how C++ plugins use the trampoline and address IDs)
- `CommonLibF4/CommonLibF4/include/REL/Relocation.h` (ID database format, relocation patterns)
- `CommonLibF4/CommonLibF4/include/RE/Bethesda/BSScript.h`, `GameScript.h` (VM, natives)
- `CommonLibF4/CommonLibF4/include/RE/Bethesda/Script.h` (command tables)
- `CommonLibF4/CommonLibF4/include/RE/RTTI_IDs.h`, `VTABLE_IDs.h`, `NiRTTI_IDs.h`
- `CommonLibF4/F4SEStub/loader`, `F4SEStub/steam_loader` (injection and early-hook technique), `F4SEStub/runtime` (behaviour of F4SE natives)
- `git show origin/community:<path>` (PluginVersionData, BGSSaveLoadManager, TaskQueueInterface, more IMenu and renderer)

**Our own code:**
- `mod-setup/setup-fallout4-mods.py`: sha-pinned no-overwrite install and `[Archive]` ini edit, reused for profiles.

## User actions needed along the way
- Download the Address Library 1.10.163 `.bin` from Nexus (needs a login) into the baseline profile. It's used for hints and by C++ canary plugins.
- Pick 3 to 5 canary mods (DLL, Papyrus, UI) for the compatibility phases.
- Optionally, get a next-gen exe later to start Phase 7 verification.

## Verification (end to end)
- `cargo xtask ci` runs:
  - host unit tests;
  - `cargo xwin test` under Wine (hooks, ABI layouts);
  - `tools/mock-host` driving the runtime and plugins;
  - `verify-db --exe` against the real 1.10.163 exe;
  - `provenance-lint`;
  - `pex` round-trip.
- `f4x test smoke --host f4x --headless`: in-game lifecycle, console, screenshot, crash capture.
- `f4x test compat --host f4se --host f4x --diff baselines/1.10.163`: message timeline, co-save round trip, Papyrus natives, the `f4se` object.
- `f4x coverage --fail-on-regression`.
- The MCP smoke test from Claude Code: launch the game, wait for the main menu, `coc qasmoke`, run a test, take a screenshot, quit.

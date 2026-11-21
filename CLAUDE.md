# f4x

f4x is an MIT-licensed Rust script extender for Fallout 4. It is a loader and an injected runtime. It is compatible with F4SE plugins, F4SE Papyrus scripts, Scaleform UI mods and co-saves. On top of that it has a native Rust engine API and an in-game control server for automated testing and observability.

The plan is in `docs/PLAN.md`. Read it before starting a phase.

## Clean-room rules (MUST follow)

f4x must never contain code or facts taken from F4SE's source, because F4SE has no usable license.

- **Never read F4SE source.** It is quarantined at `/path/to/projects/Software/f4se-quarantine`, and `.claude/settings.json` has deny rules for it. Don't try to get around them: no `cat`, `grep` or `find` into it, and no web copies of F4SE source either, e.g. GitHub `ianpatt/f4se`.
- **Don't disassemble or decompile F4SE's DLLs.**
  - You may observe F4SE as a black box through our probe plugins: the messages it sends and when, the data it passes, the files it writes, the versions it reports, and the `root.f4se` Scaleform object.
  - Return addresses into *game* code are fine.
- **F4SE's distributed `.pex` files** may be read for function signatures only (names, parameter and return types). Never copy `.psc` comments.
- **Treat `../f4se-rs` as untrusted.** It is the earlier Rust SDK. Several of its files cite F4SE source:
  - `ffi.rs`, `version.rs`, `declaration.rs`, `messaging.rs`, `patch.rs`, `rel.rs`, `game/mod.rs` offsets, `tools/mock-f4se`.
  - Don't copy them. Re-derive each from the clean sources below.
  - `pe.rs`, `game/rtti.rs` and `error.rs` may be ported after review.
- **Address Library** `.bin` files may only confirm an address that our own recipe found. Never commit or ship bin data.
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

## Running the game (agents)

**Build the Windows side:**

```sh
cargo xwin build --release --target x86_64-pc-windows-msvc -p f4x-loader -p f4x-runtime -p f4x-agent-f4se -p f4x-smoke-tests
cargo build -p f4x-cli                       # host CLI: target/debug/f4x
```

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

**MCP:** `.mcp.json` registers the `f4x` server (`tools/f4x-mcp.sh`). Smoke test it with `python3 -I tools/mcp_smoke.py tools/f4x-mcp.sh`.

**Crashes:** `f4x status --session "$S"` prints symbolized crash reports. The files are `crash-*.json` in the session directory.

**Rules for game sessions:**
- Launches queue on `~/.local/share/f4x/launch.lock`.
- f4x only uses its own prefixes and profiles under `~/.local/share/f4x`, never the user's Heroic prefix.
- Every test launch must be muted (the f4x prefixes' inis do this).
- Engine reads happen on the main thread (`f4x_agent::pump::on_main`).
- Never call virtuals through unchecked pointers read from engine fields: use `Form::checked`.

## Commands

```sh
cargo test --workspace            # host tests
cargo win -p <crate>              # Windows build via cargo-xwin (x86_64-pc-windows-msvc, static CRT)
cargo xtask <task>                # see xtask/src/main.rs
```

## Environment

- **Game:** GOG Fallout 4 1.10.163 at `/path/to/games/HeroicGames/Fallout 4 GOTY`.
- **Proton prefix:** `~/Games/Heroic/Prefixes/default/Fallout 4 Game of the Year Edition`.
- **Proton:** GE-Proton10-30 at `~/.config/heroic/tools/proton/GE-Proton-latest`, launched through `~/.config/heroic/tools/runtimes/umu/umu_run.py`.
- **Don't modify the game install** outside f4x profiles. Write anything there through a manifest so it can be undone.

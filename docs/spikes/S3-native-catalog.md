# Spike S3: native function catalog. Result: GO

- **Capture:** `cargo xtask spike-launch [--headless]` writes `natives.jsonl` into the session directory.
- **Check:** `cargo xtask catalog-check [--natives FILE]` checks it against the game on disk.

## How it works
The runtime finds `BSScript::Internal::VirtualMachine`'s primary vtable by RTTI name (`f4x-pe::rtti::RttiIndex`, ~160 ms in-game). It then swaps vtable slot 0x1B, `IVirtualMachine::BindNativeMethod(IFunction*)` (CommonLibF4 `BSScript.h`), with a recorder that calls through to the original.

Every native the engine binds is recorded with:
- script, name, state;
- the Papyrus signature (`NativeFunctionBase` descriptor table, `TypeInfo` decoded to names);
- whether it is static or latent;
- the C++ template class from RTTI (e.g. `NativeFunction1<Object, void, TESForm*>`, which gives the C++ argument types);
- the callback RVA.

Natives are **never called**. The catalog is the map of engine functionality that the Phase 2/6 analyzer follows to engine internals.

## Result (1.10.163, 2026-11-21)

**876** natives bound, against **891** declared in the 7,875 vanilla scripts.
- Bound but not declared: 0.
- Declared but not bound: 15. All are dev or legacy natives the release exe leaves unbound:
  - `Actor.ForceMovement*` / `ForceTarget*` / `CanMoveVertical` / `CanStrafe` / `ClearForcedMovement`;
  - `Debug.dbSendPlayerPosition`, `Debug.SetFootIK`;
  - `ObjectReference.TetherToHorse`.

The **callback at +0x50** is confirmed for all 876 bindings:
- 550 are `.pdata` function starts;
- 326 are leaf functions: 16-byte-aligned, in `.text`, outside every unwind range.

## Corrections to the plan
- "Every callback is a `.pdata` start" was too strict. MSVC emits leaf functions without unwind data. The check is now `PeFile::is_function_start`.
- "Count equals the pex natives" becomes: bound ⊆ declared, and the declared-only list is recorded.

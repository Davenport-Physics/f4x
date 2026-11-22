# The F4SE co-save format (`<save>.f4se`)

Learned black-box on 2026-11-21 from a co-save written by F4SE 0.6.23. The setup: the f4x probe plugin wrote patterned records in its save callback, inside the isolated `f4se-baseline` profile. The fixture is `crates/f4x-cosave/tests/fixtures/f4se-0.6.23-probe.f4se`. The implementation is `crates/f4x-cosave`.

All integers are little-endian. Four-character codes are stored as little-endian `u32`s of MSVC multi-character constants, so `'TST1'` is the bytes `31 54 53 54` ("1TST").

```text
header:  "F4SE"  u32 format(1)  u32 f4se_version(0x00060170)  u32 runtime(0x010A0A30)  u32 plugin_count
plugin:  u32 uid  u32 chunk_count  u32 length(bytes of its chunks, headers included)
chunk:   u32 type  u32 version  u32 length  data[length]
```

**F4SE's own block** has uid 0. With nothing registered it holds 14 chunks:

| Chunk | Version | Data |
|---|---|---|
| `PLGN` | 0 | The load order: `u16 count`, then per file `u8 index, u16 len, name` |
| `KEYR` + `REGE` | 1 | Empty |
| `CTLR` + `REGE` | 1 | Empty |
| `EXEV` + `REGE` | 1 | Empty |
| `CAMR` | 1 | `u32` 0 |
| `FRNR` | 1 | `u32` 0 |
| `OBMG` | 1 | 8 bytes, zero |
| `DFMG` | 1 | Empty |
| `FUNQ` | 1 | `u32` 0 |
| `WLST` | 1 | 12 bytes, zero |
| `____` | 1 | Empty: the end marker |

**A plugin's block** uses the uid from `SetUniqueID`. `WriteRecord` produces one chunk. `OpenRecord` followed by several `WriteRecordData` calls produces one chunk with the pieces concatenated. An empty record survives as a zero-length chunk.

**Lifecycle** (same session):
- `PreSaveGame(name)` → save callbacks → `PostSaveGame(name)`, all on the main thread. `name` is the save name without extension and without NUL; `data_len` is its length.
- `PreLoadGame(name)` runs on a worker thread. It is followed by revert callbacks, then load callbacks, then `PostLoadGame` with `data = 1` (success, passed in the pointer).
- `coc` from the main menu sends neither load messages nor `NewGame`.

Still to learn:
- light (ESL) entries in `PLGN`;
- the non-empty registration tables;
- `DeleteGame`;
- `NewGame`.

## Light plugins in `PLGN` (black-box, user-mods profile, 2026-11-21)

- Entries are in load order, with full and light files interleaved.
- A light file has index byte `0xFE`, then a `u16` light index, then the usual `u16` name length and the name.
  - Example: `FE 00 00 18 00 "CRP Radio Towers 2.5.esp"`.
- Under the same load order, f4x's `PLGN` is byte-identical to F4SE 0.6.23's (489 bytes).
- A plugin that registered serialization callbacks but wrote no records gets no block (mcm.dll is an example).

## `REGS` (control registrations, seen but not implemented yet)

From Settle Anywhere's `RegisterForControl("WorkshopMode")`, `REGS` v1 holds, for one control name:
- `u16` length, then the name;
- `u32` count;
- per registration: a `u64` VM handle, then a `u16` length and the script name.

Example: `0C 00 "WorkshopMode" 01 00 00 00 1C 00 00 0C 01 00 00 00 13 00 "SAPlayerAliasScript"`.

## `NewGame` (black-box, F4SE 0.6.23, vanilla profile, 2026-11-21)

- The trigger was MainMenu's `BGSCodeObj.StartNewGame()`, called with `ui.invoke`.
- The message is `NewGame` (kind 8) with `data` = the starting `TESQuest*` (data_len 8). In vanilla that's `0001ED86`.
- It's sent on the main thread from the new-game starter's quest-start call: return address `Fallout4.exe+0x12a243f`, the call at RVA 0x12a243a.
- It's sent after that call returns: the quest already has `alreadyRun = 1` and flags `0x0501`.
- No serialization revert callbacks run before it, when the game is started from the main menu.
- f4x reproduces all of this (`lifecycle.rs`, `install_new_game`).

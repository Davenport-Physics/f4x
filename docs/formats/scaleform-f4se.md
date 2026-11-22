# `root.f4se` and F4SE Scaleform callbacks (black-box)

F4SE 0.6.23 was observed on the user's mod set. The probe (`probes/f4se-probe`) marks the
value it gets, and the agent's `ui.*` methods look into the movie. f4x reproduces
everything below (`crates/f4x-runtime/src/f4se/scaleform.rs`). `tools/f4se_object.py`
captures the surface of a session. Captures under F4SE and f4x are byte-identical.

## When and where

- Plugin callbacks (`ScaleformInterface::Register(name, cb)`) run once per movie
  (`bool cb(GFxMovieView*, GFxValue*)`).
  - The call comes from `BSScaleformManager::LoadMovie`, just after the movie instance
    is created. The return address is LoadMovie+0x18d, after the game's call at +0x188.
- At callback time `root` (a display object) has no `f4se` member yet. The object a
  plugin receives becomes `root.f4se.plugins.<registered name>`.

## Members

How they were enumerated:
- `ui.has` was asked about every identifier found in the vanilla `Interface` SWFs and the
  installed mods' SWFs (`tools/swf_strings.py`), about 8000 names, plus guesses.
- Function objects and `plugins` were then walked further.

| Member | Kind | Behaviour |
|---|---|---|
| `version` | object | `{major, minor, beta, releaseIdx}`, all `int`, all 0 |
| `plugins` | object | one object per registered plugin name |
| `AllowTextInput(bool)` | function | `true`: `ControlMap::byTextEntryCount += 1`; `false`: `-= 1`, not below 0. Returns undefined. F4SE crashes without an argument. |
| `GetDirectoryListing(path, pattern = "*")` | function | See below. |
| `SendExternalEvent(name, ...)` | function | Returns undefined. It goes to Papyrus external-event registrations; no installed script makes any. |

`GetDirectoryListing(path, pattern = "*")`:
- `path` is relative to the game folder: `Data/MCM/Config` works, `MCM/Config` finds nothing.
- It returns an array of entries in FindFirstFile order, `.` and `..` included. It is not recursive; a third argument changes nothing.
- Each entry is `{name, nativePath, isDirectory, isHidden, lastModified, creationDate}`:
  - `name` and `nativePath` are both `path + "\" + file`;
  - the two dates are AS3 `Date`s built from the file time's UTC parts, which the
    constructor treats as local time. So `getTime()` is the file time plus the UTC offset.
- F4SE crashes when called without arguments.

## Who uses what

MCM 1.38's `MCM.swf` uses:
- `f4se.GetDirectoryListing`, to find `Data/MCM/Config/*` and each mod's `config.json` and `lib.swf`;
- `f4se.SendExternalEvent` (`OnMCMOpen`, `OnMCMClose`);
- `f4se.AllowTextInput`;
- `f4se.plugins.f4mcm`, which mcm.dll fills.

With f4x, MCM's "MOD CONFIG" entry and panel look the same as under F4SE. Screenshots
differ by at most 16 levels per pixel (background blur).

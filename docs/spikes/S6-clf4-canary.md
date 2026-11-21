# Spike S6: CommonLibF4 compat canary. Result: GO (build); in-game load blocked on the Address Library bin

```sh
tests/compat/clf4-example/build.sh     # ~9 s on 24 threads; prints target/compat/clf4-example/ExampleProject.dll
```

The script is a thin `sh` wrapper around `build.py`, which is run with `python3 -I`. Every flag is in the "Flags" section at the top of `build.py`. Each run starts from an empty `target/compat/clf4-example/`, so nothing stale is reused. `build-info.json` records every argv, its exit status, time and warning count, plus the DLL's size and SHA-256.

Inputs. The script only reads these; it never writes to them:

- `../CommonLibF4` at `67173f8d` (MIT). The script warns if `HEAD` differs. Override the path with `F4X_CLF4`.
- The S2 third-party cache `target/third_party/`. The script checks each dependency's `.f4x-verified` stamp against the S2 SHA-256 pins. If it is missing, run `cargo run --release -p f4x-codegen -- dump` once.
- The cargo-xwin CRT/SDK at `~/.cache/cargo-xwin/xwin`: STL 14.44 (`_MSVC_STL_UPDATE 202503L`) and SDK 10.0.26100. Override the path with `F4X_XWIN`. If it is missing, any `cargo win` run fills it.
- Fedora `clang`, `lld` and `llvm` 22.1.8, which provide `clang-cl`, `lld-link`, `llvm-lib` and `llvm-rc`.

Result on 2026-11-21:

| Exit criterion | Result |
|---|---|
| (a) ExampleProject plus the CommonLibF4 library builds as an x64 DLL with clang-cl + lld-link + xwin, `/MT` | **Met.** 33 TUs, 0 errors, 31 compiler warnings plus 1 from llvm-rc (listed below). It needed one source fix, applied to a copy (below). The output is bit-for-bit reproducible: two clean builds gave the same SHA-256. |
| (b) Exports `F4SEPlugin_Query` and `F4SEPlugin_Load` | **Met.** Checked with `llvm-objdump -p` and `winedump -j export`. |
| (c) Loads under real F4SE 0.6.23 | **Failure recorded, as expected.** F4SE loaded the DLL and called `F4SEPlugin_Query`, which succeeded. It then called `F4SEPlugin_Load`. There, `F4SE::Init` → `REL::IDDatabase` could not open `Data/F4SE/Plugins/version-1-10-163-0.bin`, logged `critical` and raised a MessageBox. The game hung and was killed after 150 s. The bin was not downloaded: the user has to fetch it from Nexus. |

## The DLL

`target/compat/clf4-example/ExampleProject.dll`:

- 584,704 bytes; SHA-256 `880cf3afad5666bfb9990d3bb443b050c113fbfd40d4318f05cd7cc8a161dc9a`.
- Its PDB is 6.3 MB. The DLL also contains a `VERSIONINFO` resource with FileVersion 1.0.0.0.

```text
$ llvm-objdump -p ExampleProject.dll
Export Table:
 DLL name: ExampleProject.dll
 Ordinal base: 1
 Ordinal      RVA  Name
       1   0x1c00  F4SEPlugin_Load
       2   0x1000  F4SEPlugin_Query
    DLL Name: KERNEL32.dll
    DLL Name: SHELL32.dll
    DLL Name: ole32.dll
    DLL Name: VERSION.dll
    DLL Name: USER32.dll
```

There are no `VCRUNTIME140`/`MSVCP140`/`api-ms-win-crt-*` imports: the CRT and STL are linked statically. That matters on old-gen 1.10.163, which ships only the VS2012 runtime. The DLL has these sections: `.text` 0x65476, `.rdata`, `.data`, `.pdata`, `.gfids`, `.tls`, `_RDATA`, `.rsrc`, `.reloc`. The `.tls` section is the static TLS for spdlog and the STL's `thread_local` objects. It works for a `LoadLibrary`'d DLL on Vista and later, and under Wine.

## Build steps and command lines

`$XWIN` is the xwin cache, `$TP` is `target/third_party`, `$CLF4` is `../CommonLibF4` and `$OUT` is `target/compat/clf4-example`. `build-info.json` has the literal argv of every step.

1. **Configure.** `Version.h.in` and `version.rc.in` are expanded the way CMake's `configure_file(@ONLY)` does it: project `ExampleProject`, version 1.0.0.
2. **Compile.** Each TU is a separate `clang-cl` call, and the calls run in parallel. These calls share a prefix:
   ```sh
   clang-cl --no-default-config --target=x86_64-pc-windows-msvc /nologo /c /X \
     -fms-compatibility-version=19.44 /std:c++20 /permissive- /Zc:twoPhase- \
     -Wno-delayed-template-parsing-in-cxx20 /EHsc /GR /MT /O2 /Ob2 /Gy /Gw /Z7 /utf-8 /Brepro \
     -fno-color-diagnostics /DNDEBUG \
     /imsvc $XWIN/crt/include /imsvc $XWIN/sdk/include/ucrt /imsvc $XWIN/sdk/include/um /imsvc $XWIN/sdk/include/shared
   ```
   Each library then adds its own options:

   | Library | TUs | Adds |
   |---|---|---|
   | fmt 12.1.0 | `src/format.cc`, `src/os.cc` | `/I fmt/include` |
   | spdlog 1.16.0 | `src/{spdlog,stdout_sinks,color_sinks,file_sinks,async,cfg}.cpp`: the `SPDLOG_SRCS` list for external fmt | `/DSPDLOG_COMPILED_LIB /DSPDLOG_FMT_EXTERNAL /I spdlog/include /imsvc fmt/include` |
   | rsm-mmio 2.0.0 | `src/mmio/mmio.cpp` | `/I mmio/include /I mmio/src` |
   | CommonLibF4 | the 23 `src/*.cpp` that `CommonLibF4/cmake/sourcelist.cmake` lists | the CommonLibF4 PUBLIC defines plus the spdlog defines, `/imsvc` for fmt, spdlog, mmio and the 3 Boost trees, `/I $CLF4/CommonLibF4/include /FIF4SE/Impl/PCH.h` |
   | ExampleProject | `ExampleProject/src/main.cpp` | the same defines plus `/D_UNICODE`, CommonLibF4's include directory as `/imsvc`, `/I $OUT/include /I $CLF4/ExampleProject/src /FIPCH.h /W4 /WX` |

   The `/FI` force-includes stand in for CMake's `target_precompile_headers`. A real PCH is not worth it: the slowest TU, `RE/Fallout.cpp`, takes 6.7 s, and the whole build takes about 9 s wall time.
3. **Archive.** `llvm-lib /nologo /OUT:$OUT/<lib>.lib <objs>` builds `fmt.lib`, `spdlog.lib`, `mmio.lib` and `CommonLibF4.lib` (15 MB). These are static libraries, as in CMake and vcpkg, so the linker pulls in only the objects it needs.
4. **Resource.** `llvm-rc /nologo /C 65001 /X /I <the 4 xwin include dirs> /FO $OUT/version.res $OUT/version.rc`.
5. **Link.**
   ```sh
   lld-link /nologo /DLL /MACHINE:X64 /DEBUG:FULL /OPT:REF /OPT:ICF /INCREMENTAL:NO /Brepro /WX /ignore:4099 \
     /LIBPATH:$XWIN/crt/lib/x86_64 /LIBPATH:$XWIN/sdk/lib/ucrt/x86_64 /LIBPATH:$XWIN/sdk/lib/um/x86_64 \
     /OUT:$OUT/ExampleProject.dll /PDB:$OUT/ExampleProject.pdb \
     main.obj version.res fmt.lib spdlog.lib mmio.lib CommonLibF4.lib \
     kernel32.lib user32.lib shell32.lib ole32.lib advapi32.lib version.lib
   ```
   The CRT comes from the `/DEFAULTLIB` directives that `/MT` puts in the objects: `libcmt`, `libcpmt`, `libvcruntime`, `libucrt` and `oldnames`.

## Why each flag and define

| Flag / define | Why |
|---|---|
| `--no-default-config` | Ignores Fedora's `/etc/clang/*.cfg`, as in S2. |
| `/X` + `/imsvc` | `/X` ignores `%INCLUDE%`; `/imsvc` adds the xwin CRT/SDK as system headers. The xwin layout (`crt/include`, `sdk/include/ucrt`) is not the `/winsysroot` layout, so it is spelled out. |
| `-fms-compatibility-version=19.44` | Matches `_MSC_VER` to the xwin STL 14.44, as in S2. clang-cl defaults to 19.33. |
| `/std:c++20` for every TU | CommonLibF4 needs C++20. fmt, spdlog and mmio only set C++11/17 as a minimum. Compiling every TU with one standard avoids ODR surprises in their inline headers. |
| `/permissive-` | CommonLibF4 sets it PUBLIC. |
| `/Zc:twoPhase-` + `-Wno-delayed-template-parsing-in-cxx20` | **Needed.** This is clang-cl's spelling of `-fdelayed-template-parsing`. Without it, `RE/Fallout.cpp` and `main.cpp` each fail with the two latent `BSScriptUtil.h:558/:569` bugs that S2 found (re-checked here: 2 errors per TU). |
| `/EHsc /GR /MT` | MSVC defaults, with the static CRT to match the rest of f4x (`.cargo/config.toml` uses `+crt-static`). Upstream ExampleProject uses `/MD` (vcpkg `x64-windows-static-md`). |
| `/O2 /Ob2 /Gy /Gw /DNDEBUG` | CMake Release. `/Gy /Gw` give `/OPT:REF /OPT:ICF` something to drop. `NDEBUG` also selects ExampleProject's file logger (`basic_file_sink`) rather than `msvc_sink` (OutputDebugString). |
| `/Z7` + `/DEBUG:FULL` | CodeView in each object; lld-link writes the PDB. |
| `/utf-8` | fmt (`FMT_UNICODE`) and spdlog require it on MSVC. ExampleProject sets it too. |
| `/Brepro` (cl and link) | No timestamps, so builds are reproducible. |
| `/W4 /WX` (main.cpp only) | ExampleProject's own warning policy. CommonLibF4's headers are `/imsvc` (system) in this TU only, so `/WX` holds just the example's code. main.cpp is clean under clang `-Wall -Wextra`. |
| `SPDLOG_COMPILED_LIB`, `SPDLOG_FMT_EXTERNAL` | As in S2 and vcpkg's spdlog port: compiled spdlog with external fmt. Header-only spdlog would pull `<windows.h>` into every TU, and its macros break `F4SE::WinAPI::MessageBox` (S2). fmt is compiled as well (`format.cc`, `os.cc`) rather than `FMT_HEADER_ONLY`, matching vcpkg. |
| `BOOST_STL_INTERFACES_DISABLE_CONCEPTS`, `WINVER=0x0601`, `_WIN32_WINNT=0x0601` | CommonLibF4 PUBLIC defines. |
| `_UNICODE` | ExampleProject PRIVATE define. |
| `/ignore:4099` | The xwin `libcmt`/`libcpmt` objects reference PDBs that xwin does not ship. With `/WX`, those 20 LNK4099 warnings were link errors. `.cargo/config.toml` ignores the same warning. |
| No `/sdl /JMC- /Zc:inline /Zc:preprocessor /MP` | Checked with `clang-cl -###`: the first three change nothing in clang-cl, `/Zc:preprocessor` is reported as unused (clang's preprocessor already conforms), and `/MP` is unused because the script runs TUs in parallel itself. |
| No LTO | ExampleProject sets `INTERPROCEDURAL_OPTIMIZATION`, which is `/GL /LTCG` under MSVC. It was left out; clang-cl's version would be `-flto=thin`. It makes no difference for a canary. |
| No MSVC `/wdNNNN` | clang-cl does not map MSVC warning numbers such as 4200, 4201 or 4324. The clang warnings are listed below instead. |

## Source fix: one, applied to a copy

`CommonLibF4/src/F4SE/API.cpp:29` declares `std::function<const void*(F4SEAPI)(const char*)>`. MSVC accepts a calling convention in parentheses with no declarator. clang parses it as "a function returning a function" and gives 3 errors, at :29, :72 and :74. `build.py` (`PATCHES`) writes `target/compat/clf4-example/patched/CommonLibF4/src/F4SE/API.cpp` with `std::function<const void* F4SEAPI(const char*)>` and compiles that copy instead. The type is the same; `__cdecl` is a no-op on x64 anyway. If the expected text stops appearing exactly once, the script fails. This fix could be sent upstream.

## Warnings: 31 from clang, 1 from llvm-rc, all left alone

| Count | Where | Warning | Note |
|---|---|---|---|
| 9 | `RE/Bethesda/TESForms.h:492` | `-Wenum-compare-typo` | `kLocationSpecRefs = 1u < 28`: a real CommonLibF4 typo, so the value is 1. Also seen in S2. |
| 7 | `F4SE/Interfaces.cpp:14…148` | `-Wmicrosoft-cast` | function pointer ↔ `void*`; fine on MSVC targets. |
| 3 | `Scaleform/Kernel/SF_RefCount.h:37` | `-Wdeprecated-volatile` | `++` on a `volatile int`. |
| 3 | `IMenu.h:567` (×2), `bhkCharacterController.h:102` | `-Wuninitialized` | Reference and const members in engine-owned types. |
| 2 | `F4SE/Interfaces.h:199` | `-Wreturn-type` | `MessagingInterface::GetEventDispatcher` has no `return`. This is a real CommonLibF4 bug: calling it is UB. MSVC only diagnoses it when the function is code-generated. |
| 1 | `NiTArray.h:168` | `-Wmicrosoft-template-shadow` | |
| 1 | `F4SE/Impl/WinAPI.cpp:251` | `-Winvalid-noreturn` | The `TerminateProcess` wrapper is declared `[[noreturn]]`. |
| 4 | `mmio/mmio.cpp:215/223/276/284` | `-Wconstant-evaluated`, `-Wreturn-type` | `if constexpr (!std::is_constant_evaluated())` is always false, so an unmapped `errno` or Win32 error falls off the end. This is an upstream mmio bug and behaves the same under MSVC. |
| 1 | `fmt/base.h:930` | `-Wunused-result` | |
| 1 | xwin `um/winver.h:18` (llvm-rc) | `-Wnonportable-include-path` | `<SpecStrings.h>` is resolved through xwin's case symlink. llvm-rc cannot pass `-W` flags to its preprocessor. |

The spdlog TUs, fmt's `os.cc`, the other 10 CommonLibF4 TUs and `main.cpp` (at `/W4`) have none.

## In-game result (2026-11-21, F4SE 0.6.23, GOG 1.10.163, GE-Proton10-30, headless gamescope)

```sh
cargo xtask profile create clf4-canary --host f4se
ln -sf "$PWD/target/compat/clf4-example/ExampleProject.dll" ~/.local/share/f4x/profiles/clf4-canary/game/Data/F4SE/Plugins/
cargo xtask baseline --profile clf4-canary --headless --no-build   # session target/f4x-sessions/1795302598-baseline-f4se
```

The profile's `Data/F4SE/Plugins/` holds `ExampleProject.dll` and `f4x_probe.dll`, both symlinked. F4SE processes them in that order. The logs are in `~/.local/share/f4x/profiles/clf4-canary/prefix/pfx/drive_c/users/steamuser/Documents/My Games/Fallout4/F4SE/`.

`f4se.log` (complete):

```text
F4SE runtime: initialize (version = 0.6.23 010A0A30 01DD7A0D707D09B1, os = 6.2 (9200))
imagebase = 0000000140000000
reloc mgr imagebase = 0000000140000000
plugin directory = X:\.local\share\f4x\profiles\clf4-canary\game\Data\F4SE\Plugins\
checking plugin X:\.local\share\f4x\profiles\clf4-canary\game\Data\F4SE\Plugins\\ExampleProject.dll
```

`ExampleProject.log` (complete):

```text
…/CommonLibF4/ExampleProject/src/main.cpp(27): [info] ExampleProject v1.0.0
…/CommonLibF4/CommonLibF4/include/REL/Relocation.h(600): [critical] failed to open: Data/F4SE/Plugins/version-1-10-163-0.bin
```

`f4se_loader.log`, last lines (the S5 profile logs `hook thread complete` / `launching` here instead):

```text
Process::InstallHook: waiting for thread = WAIT_TIMEOUT
Couldn't inject DLL.
```

What this shows:

- F4SE mapped the clang-built DLL and resolved both exports. Static CRT start-up worked under Proton.
- `F4SEPlugin_Query` ran: `SHGetKnownFolderPath`, the spdlog file sink and fmt all worked, and it returned true. The runtime-version check passed.
- `F4SEPlugin_Load` ran. `REL::Module` found `Fallout4.exe` and read its version resource through `version.dll`, which is where the `1-10-163-0` in the path comes from.
- `REL::IDDatabase` then failed. `stl::report_and_fail` logged the critical line, called `MessageBox`, and would have called `TerminateProcess` once the box was dismissed. Headless, nobody dismisses it.
  - The launch log shows Proton starting `xalia.exe`, its UI-automation helper for desktop windows. That is consistent with a box being shown, but the log has no timestamps to prove it.
- F4SE writes `plugin … (00000001 ExampleProject 00000001) loaded correctly` only after `Load` returns, so its log stops at `checking plugin`.
- The loader's wait for its injection thread timed out. Seen from outside, plugin loading appears to run inside that thread.
- The probe was never reached, so the session has no `probe.jsonl`. `baseline` stopped the session after its 150 s timeout and exited with "no probe.jsonl". `baselines/1.10.163/probe-f4se.jsonl` was not touched; its SHA-256 was checked before and after.

**Next step (user):** put the Nexus Address Library `version-1-10-163-0.bin` into `~/.local/share/f4x/profiles/clf4-canary/game/Data/F4SE/Plugins/` (never committed; `*.bin` is gitignored) and re-run. The expected result is `f4se.log` reaching `plugin …ExampleProject.dll (00000001 ExampleProject 00000001) loaded correctly`, and `ExampleProject.log` reaching `[info] hello world!`. **Caution:** `baseline` always writes `baselines/1.10.163/probe-f4se.jsonl`, whatever `--profile` is. A successful canary run would therefore overwrite the S5 reference with a two-plugin run. Back that file up first, or wait for the xtask change below.

## For xtask: making this a routine compat test

1. `baseline` should name its output after the profile, or take `--out`, so that non-reference profiles never overwrite `probe-f4se.jsonl`.
2. Add a `compat` task (for example `cargo xtask compat clf4-example --host f4se|f4x`). It would:
   - run `build.sh`;
   - link the DLL into the profile;
   - launch the game;
   - judge the run from `f4se.log` (the `loaded correctly` line) and the plugin's own log, not only from `probe.jsonl`.
3. After each run, copy `My Games/Fallout4/F4SE/*.log` (`f4se.log`, `f4se_loader.log`, plugin logs) into the session directory. The next launch overwrites them.
4. Fail fast. Watch the plugin log for `[critical]`, or watch for a top-level MessageBox or `xalia.exe`, and end the session at once instead of waiting 150 s.
5. A profile option to link a user-supplied Address Library bin, for example from `F4X_ADDRESS_LIBRARY`, into `Plugins/`. Missing-bin runs would then be reported as "skipped: no Address Library" rather than as hangs.
6. `profile create … --plugin PATH` (repeatable), so canary profiles are declared rather than hand-symlinked.

## Risks

1. **Toolchain drift.** A clang update can add warnings, and `main.cpp` is `/WX`. cargo-xwin may also install a newer STL than the 14.44 assumed by `-fms-compatibility-version`. `build-info.json` records the clang version, and S2 already records the STL version.
2. **`/MT` vs upstream `/MD`.** Upstream ExampleProject links the dynamic CRT. Under `/MT` each plugin has its own CRT heap, and that is the f4x convention. ExampleProject hands F4SE only a static string (`PluginInfo::name`), so no heap memory crosses the boundary.
3. **Delayed template parsing** hides the same uninstantiated bodies as in S2. If a future CommonLibF4 instantiates `PackVariable` for `cobject` types, those two bugs become real errors under any compiler.
4. **The patched `API.cpp` copy** has to track upstream. The exact-match check turns drift into a build failure instead of a silent skip.
5. **Nothing third-party ships.** fmt, spdlog, mmio (MIT) and the Boost headers (BSL-1.0) are compiled only into this test DLL under `target/`. None of it is committed or ends up in f4x.

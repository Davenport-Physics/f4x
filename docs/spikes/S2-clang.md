# Spike S2: clang over CommonLibF4. Result: GO

```sh
cargo run --release -p f4x-codegen -- dump     # ~10 s warm; the first run also fetches 6 tarballs (~1.5 MB)
cargo run --release -p f4x-codegen -- layouts  # re-parse records.txt only
cargo test -p f4x-codegen                      # unit tests + sanity checks against the dump
```

`dump` writes `target/clangdump/` (gitignored): `ast.json`, `records.txt`, `vtables.txt`, `layouts.json`, `relocs.json`, `dump-info.json` (every argv, timing, count and hash) and `diag-*.txt`.

Result on 2026-11-21 (clang 22.1.8, CommonLibF4 `67173f8d`, clean):

| Exit criterion | Result |
|---|---|
| `RE/Fallout.h` parses for `x86_64-pc-windows-msvc` with zero errors | **Met.** 0 errors, 5 warnings; with `F4SE/F4SE.h` added to the TU, 0 errors and 6 warnings. Needs MSVC-style `-fdelayed-template-parsing` (below). |
| All `static_assert`s hold | **Met.** All 914 live `static_assert(sizeof…)` lines are evaluated; the other 17 of the 931 `grep` hits are commented out. In total, 961 of 978 `static_assert` sites are evaluated. Of the other 17, 16 sit in uninstantiated templates (4 of those are `static_assert(false && …)` "unhandled case" guards). The 17th is PCH.h:37, the global-scope `time_t` check: clang evaluates it, but it lies outside the dumped namespaces. Zero errors means every evaluated assert held. |
| Machine-readable AST, MS-ABI record layouts and vtable layouts | **Met.** 341 MB JSON AST, 13,976 record layouts, 1,056 vftables. `layouts.json` covers 937 of the 937 non-template `RE::` record definitions in the AST. |

The sanity sizes are checked by `tests/sanity.rs`, and all of them match: `TESForm` 0x20, `TESObjectREFR` 0x110, `Actor` 0x490, `PlayerCharacter` 0xE10, `TESDataHandler` 0x1020, `BSScript::Stack` 0xA8, `Scaleform::GFx::Value` 0x20, `UI` 0x250.

## Inputs

- **clang** 22.1.8 (`/usr/bin/clang++`). The tool skips `/usr/lib64/ccache` wrappers on `PATH`; override with `--clang` or `F4X_CLANG`.
- **MSVC CRT/STL and Windows SDK** from the cargo-xwin cache `~/.cache/cargo-xwin/xwin`: STL 14.44 (`_MSVC_STL_UPDATE 202503L`) and SDK 10.0.26100. Only `crt/include` and `sdk/include/ucrt` are actually reached. No CommonLibF4 header includes `<Windows.h>`; only its `src/*.cpp` files do.
- **Translation unit** (`target/clangdump/tu.cpp`): `#include "RE/Fallout.h"` and `#include "F4SE/F4SE.h"`, with `-I CommonLibF4/include`.
- **Third-party headers.** CommonLibF4's vcpkg dependencies are pinned in `src/thirdparty.rs`. Each is fetched with `curl` into `target/third_party/<name>-<ver>/`, SHA-256-checked, and unpacked into its own empty directory. Nothing is committed. The versions are the ones vcpkg shipped around the CommonLibF4 commit (Nov 2025).

| Dependency | Version | License | SHA-256 of the GitHub tag tarball | Used by |
|---|---|---|---|---|
| fmt | 12.1.0 | MIT | `ea7de4299689e12b6dddd392f9896f08fb0777ac7168897a244a6d6085043fea` | `fmt::format` in PCH.h, Relocation.h, BSScript.h |
| spdlog | 1.16.0 | MIT | `8741753e488a78dd0d0024c980e1fb5b5c85888447e309d9cb9d949bdb52aa3e` | `spdlog::log` in PCH.h, Logger.h |
| rsm-mmio | 2.0.0 | MIT | `360dddf74a97bd0a7eb41378cc59f2a69871dabfd36c55bf027429ac54930d5b` | `mmio::mapped_file_source` in Relocation.h |
| Boost.STLInterfaces | 1.89.0 | BSL-1.0 | `515b160567b9c684ad2ea6e9237887cd8481e6a47cf06083c689a449917800d8` | iterators of BSTArray, BSTHashMap, BSTList, NiTArray, … |
| Boost.Config | 1.89.0 | BSL-1.0 | `a16a53f8f0dde2a9ff485fb134a069e54aca738009d825837dac9a35cd4afe0a` | included by STLInterfaces |
| Boost.Assert | 1.89.0 | BSL-1.0 | `e25e42fbc5f77dcb203a3479eaf2e9303511f2a5bb8cbdf54e9c562fcbf29da1` | included by STLInterfaces |

Neither the rest of Boost nor Boost.TypeTraits is reached. No stubs were needed, so `clang-shims/` does not exist.

## Command lines

Every pass shares this prefix. `$XWIN` is the xwin cache, `$TP` is `target/third_party`, and `$CLF4` is `../CommonLibF4`. `dump-info.json` records the literal argv.

```sh
clang++ --no-default-config --target=x86_64-pc-windows-msvc \
  -fms-compatibility -fms-extensions -fms-compatibility-version=19.44 -std=c++20 \
  -fdelayed-template-parsing -Wno-delayed-template-parsing-in-cxx20 \
  -ferror-limit=0 -fno-color-diagnostics \
  -isystem $XWIN/crt/include -isystem $XWIN/sdk/include/ucrt \
  -isystem $XWIN/sdk/include/um -isystem $XWIN/sdk/include/shared \
  -isystem $TP/fmt-12.1.0/src/fmt-12.1.0/include \
  -isystem $TP/spdlog-1.16.0/src/spdlog-1.16.0/include \
  -isystem $TP/rsm-mmio-2.0.0/src/mmio-2.0.0/include \
  -isystem $TP/boost-stl_interfaces-1.89.0/src/stl_interfaces-boost-1.89.0/include \
  -isystem $TP/boost-config-1.89.0/src/config-boost-1.89.0/include \
  -isystem $TP/boost-assert-1.89.0/src/assert-boost-1.89.0/include \
  -DSPDLOG_COMPILED_LIB -DSPDLOG_FMT_EXTERNAL -DBOOST_STL_INTERFACES_DISABLE_CONCEPTS \
  -I $CLF4/CommonLibF4/include
```

| Pass | Suffix | Time | Output |
|---|---|---|---|
| records | `-fsyntax-only -Xclang -fdump-record-layouts-complete -Xclang -fdump-record-layouts-canonical tu.cpp` | 2.7 s | `records.txt` 5.6 MB |
| ast-RE | `-fsyntax-only -Xclang -ast-dump=json -Xclang -ast-dump-filter=RE tu.cpp` | 5.6 s | 750 MB pretty JSON, streamed |
| ast-F4SE | `-fsyntax-only -Xclang -ast-dump=json -Xclang -ast-dump-filter=F4SE tu.cpp` | 2.9 s | 87 MB pretty JSON, streamed |
| vtables | `-S -emit-llvm -o /dev/null -g -gcodeview -fstandalone-debug -Xclang -fdump-vtable-layouts vtables-probe.cpp` | 2.9 s | `vtables.txt` 1.8 MB |

The records pass and both AST passes run in parallel; the vtables pass waits for records. A warm run takes 10.0 s wall time and peaks at 455 MB RSS. Every pass reports 0 errors and 6 warnings:

- `-Wenum-compare-typo` at TESForms.h:492;
- `-Wdeprecated-volatile` at SF_RefCount.h:37;
- 2× `-Wuninitialized` (PipboySubMenu, bhkCharacterController);
- `-Wmicrosoft-template-shadow` at NiTArray.h:168;
- `-Wreturn-type` at F4SE/Interfaces.h:199 (the only one from `F4SE.h`).

## Why each flag and define

| Flag / define | Why |
|---|---|
| `--no-default-config` | Ignores Fedora's `/etc/clang/*.cfg` (`--gcc-triple`), so runs are reproducible. Without it nothing breaks. |
| `-fms-compatibility -fms-extensions` | MSVC dialect: `__declspec(novtable)`, MS layout rules, `__int64`, etc. Both are already the default for `*-windows-msvc`; they are kept explicit. |
| `-fms-compatibility-version=19.44` | Claims the `_MSC_VER` that matches the xwin STL (14.44 = VS 17.14). clang's default is 19.33. Both parse cleanly, but a mismatched pair invites STL `#if` surprises. |
| `-std=c++20` | CommonLibF4 sets `cxx_std_20`. |
| `-fdelayed-template-parsing` | **Needed for zero errors.** CommonLibF4 has two latent bugs in an uninstantiated function template, `BSScriptUtil.h:558` and `:569` (`PackVariable(Variable&, T)` for `detail::cobject T`): `return success;` inside `success`'s own initializer, and a `static_cast` from `TypeInfo` to the unrelated `ObjectTypeInfo`. Such code is "ill-formed, no diagnostic required". MSVC never diagnoses it. clang does, because it parses template bodies eagerly in C++20. This flag restores MSVC behaviour (clang still uses it by default for C++17 on MSVC targets), and `-Wno-delayed-template-parsing-in-cxx20` silences its deprecation note. `--no-delayed-templates` turns it off, which gives exactly those 2 errors. |
| `-ferror-limit=0 -fno-color-diagnostics` | Complete, parseable `diag-*.txt`. |
| `SPDLOG_COMPILED_LIB` | **Required.** In header-only mode spdlog's `-inl.h` files include `<windows.h>`. Its `MessageBox`/`GetModuleFileName`/… macros then rewrite CommonLibF4's `F4SE::WinAPI::MessageBox(...)` calls, causing 7 errors. vcpkg's spdlog is a compiled library, so CommonLibF4 never sees header-only mode. |
| `SPDLOG_FMT_EXTERNAL` | Fidelity. Without it spdlog includes its bundled fmt next to CommonLibF4's `<fmt/format.h>`. Both use the `FMT_FORMAT_H_` guard, so whichever comes first wins. It still parses cleanly, but vcpkg's port uses external fmt too. |
| `BOOST_STL_INTERFACES_DISABLE_CONCEPTS` | Fidelity. CommonLibF4's CMake sets it `PUBLIC`. It selects the v1 `iterator_interface` that CommonLibF4 was written against, which changes which empty base classes the containers have. |

These were tested and are not needed: `WINVER`/`_WIN32_WINNT` (CMake sets them, but no SDK header is reached), `_CRT_SECURE_NO_WARNINGS` (no CRT deprecation warnings appear), `F4SE_SUPPORT_XBYAK` (it only adds the xbyak overloads in `Trampoline.h`; leaving it undefined avoids the xbyak dependency), and Boost.TypeTraits.

## The three dumps

**Records.**

- Plain `-fdump-record-layouts` under `-fsyntax-only` only prints records whose layout Sema happened to compute: 3,714 for `Fallout.h`.
- `-fdump-record-layouts-complete` prints every complete, non-dependent record: 13,976 in total, of which 2,201 are `RE::` (960 non-template, 526 polymorphic).
- `-fdump-record-layouts-canonical` prints desugared, qualified types (`unsigned int formID`, `const class RE::TESFileArray *`). The typedef intent (`std::uint32_t`) is still in the AST.
- `f4x-codegen layouts` turns this into `layouts.json`: one record per line, `{name, kind, size, align, nvsize, nvalign, empty?, fields:[{name, offset, type, bit_offset?, bit_width?, empty?}], bases:[{name, offset, virtual, primary?, empty?}], vfptr?, vfptrs?:[{offset, class}], vbptr?, vtordisps?:[{offset, class}]}`. `vfptr` is the lowest vftable pointer in the record's own base chain; pointers inside member subobjects are not counted. Anonymous members have `name: ""`. They show up as a trailing space in the dump, and the type of an anonymous record can contain a path with spaces.

**Vtables.**

- `-fdump-vtable-layouts` is a CodeGen dump, so `-fsyntax-only` prints nothing. An MS-ABI vftable is only computed when something needs it, and a header-only TU needs none.
- To force it, `dump` writes `vtables-probe.cpp`. It holds one `::RE::X* vt_N = nullptr;` line for each polymorphic record in `layouts.json`, inside `namespace RE::f4x_probe` so that template arguments printed without their namespace still resolve.
- It is compiled with `-g -fstandalone-debug`. Full debug info for each type makes `CGDebugInfo` ask `MicrosoftVTableContext` for every virtual method's slot, and that prints the table.
- 528 probes; 1 dropped automatically because `F4SE::TaskInterface::TaskDelegate` is private.
- Result: 1,056 `VFTable for` tables, one for each vfptr subobject of each class, plus 529 `VFTable indices` tables. Entries include `[this adjustment]` and `[pure]` markers, and a `Thunks for` section for each class that needs thunks.
- Quirk: the table headers print template classes without their arguments (`'RE::BSTEventSink' in 'RE::PlayerCharacter'`). The entry lines do carry them (`RE::BSTEventSink<RE::MenuOpenCloseEvent>::ProcessEvent`).

**AST.**

- The unfiltered TU dumps 2.67 GB, mostly the STL.
- `-ast-dump-filter` is a substring match on qualified names. `RE` dumps every `namespace RE` and `namespace REL` block whole (226 + 2), plus 10 strays such as `_CRT_REPORT_HOOK` and `MEM_RELEASE`, which `dump` drops. `F4SE` dumps the 8 `namespace F4SE` blocks.
- clang prints the matches as concatenated JSON values. `dump` streams them through a minifier into a single document, `{"kind":"TranslationUnitDecl","inner":[…236 namespaces…]}`: 341 MB and 607,574 nodes.
- Three format quirks the reader handles:
  1. Locations are delta-compressed: `file`/`line` are omitted when equal to the previously printed location. Positions must be replayed in document order (`loc`, then `range.begin`/`.end`, then `inner`; `spellingLoc` before `expansionLoc`). Each filtered declaration starts with fresh state.
  2. `UsingType` nodes carry `"type"` twice (1,523 times).
  3. A `UsingShadowDecl` re-dumps its target in full. For example, `using Render::Color;` in `GFx` contains a second complete `CXXRecordDecl Color`, which must not be counted as a definition.

## How `REL::ID` / `REL::Relocation` appear in the JSON AST

Excerpts from `ast.json`, with `id`, `loc` and `range` removed.

**1. `TESDataHandler::GetSingleton`** (TESDataHandler.h:35): `REL::Relocation<TESDataHandler**> singleton{ REL::ID(711558) };`

```text
CXXMethodDecl GetSingleton            type "TESDataHandler *()"
 CompoundStmt
  DeclStmt
   VarDecl singleton                  type {qualType "REL::Relocation<TESDataHandler **>", desugaredQualType "REL::Relocation<RE::TESDataHandler **>"}
    CXXConstructExpr                  type REL::Relocation<…>, ctorType "void (ID)"
     CXXFunctionalCastExpr            castKind ConstructorConversion, type "REL::ID"
      CXXConstructExpr                type "REL::ID", ctorType "void (std::uint64_t) noexcept"
       ImplicitCastExpr IntegralCast  type std::uint64_t
        IntegerLiteral                value "711558"
  ReturnStmt … CXXOperatorCallExpr operator* on DeclRefExpr singleton
```

**2. `TESDataHandler::CheckModsLoaded`** (TESDataHandler.h:42): `using func_t = decltype(&TESDataHandler::CheckModsLoaded); REL::Relocation<func_t> func{ REL::ID(1432894) };`. The shape is the same, and `desugaredQualType` resolves the alias to the exact signature: `"REL::Relocation<bool (RE::TESDataHandler::*)(bool)>"`. A `TypeAliasDecl func_t` precedes it with the same desugared member-pointer type.

**3. The `Script.h` command tables.**

- `SCRIPT_FUNCTION::GetConsoleFunctions` (Script.h:262): `REL::Relocation<SCRIPT_FUNCTION(*)[522]> functions{ REL::ID(901511) }` gives the target type `RE::SCRIPT_FUNCTION (*)[522]`.
- `GetScriptFunctions` (:268): ID 75173, `(*)[819]`.
- A third form is a default member initializer (Script.h:281): `compileFunction{ REL::Relocation<CompileFunction_t*>{ REL::ID(638606) }.get() }` is `FieldDecl compileFunction → ExprWithCleanups → InitListExpr → CXXMemberCallExpr get → MemberExpr → MaterializeTemporaryExpr → CXXTemporaryObjectExpr (type REL::Relocation<bool (*)(unsigned short, const RE::SCRIPT_PARAMETER *, …)>) → CXXFunctionalCastExpr REL::ID → CXXConstructExpr REL::ID → … → IntegerLiteral 638606`.

**Extraction recipe.** A prototype of this is implemented in `ast::Walker`, which writes `relocs.json`.

1. Walk each top-level value in document order, replaying locations to get `file:line`.
2. Keep a stack of enclosing scopes (namespace, record, function with its written type, var/field), and a template state: in a template pattern, in an instantiation, or neither.
3. Any node whose type, desugared and minus `const`, is `REL::Relocation<T>` pushes `T`. This covers the `VarDecl`, the `CXXConstructExpr` and `CXXTemporaryObjectExpr`.
4. A `CXXConstructExpr` of type `REL::ID` opens an ID frame. Its first `IntegerLiteral` is the ID.
5. Emit `{id, target_type: T, function, function_type, var, file, line}`. With no `Relocation` frame, it is a bare ID; that is what the RTTI/VTABLE tables are.
6. Classify by the shape of `T`, not by the enclosing method:
   - `R (C::*)(A…) [const]`: a member function. Call it as `extern "C"` `fn(*C, A…) -> R`, with MSVC's hidden return pointer for UDT returns (PLAN rule 5).
   - `R (*)(A…)` or `R (A…)`: a free or static function.
   - `T*` or `T**`: data or a singleton.
   - `T (*)[N]`: an array.

Output today:

- 214 relocations with 211 unique IDs: 134 member functions, 30 function pointers, 45 data, 5 arrays.
- 24,144 table IDs: `RTTI_IDs.h` 11,888, `VTABLE_IDs.h` 11,909, `NiRTTI_IDs.h` 347.
- 211 of the 215 literal IDs in the headers (outside the `*_IDs.h` tables) are found. The 4 that are not sit in bodies that delayed template parsing never reads:
  - `Script::ParseParameters` (1607, Script.h:307);
  - `BSScript::GetTypeInfo` (648543, BSScriptUtil.h:409);
  - `BSPointerHandleManagerInterface<T, M>::GetHandle` / `GetSmartPointer` (901626, 967277, BSPointerHandle.h:164/171).

  With `--no-delayed-templates` all 215 are found, but those 4 have dependent target types (`func_t`), and the run reports the 2 errors above.
- The 433 `REL::ID` constructions without a literal are copies, such as `static constexpr auto VTABLE{ VTABLE::TESForm }`, and `REL` internals.

## Risks

1. **Delayed parsing hides uninstantiated template bodies.** Today that is 4 IDs and 16 `static_assert` sites. Mitigation: have the generator append explicit instantiations to the TU (`template class RE::BSPointerHandleManagerInterface<RE::TESObjectREFR, …>;`, which forces member bodies with concrete types), or upstream a fix for the two `BSScriptUtil.h` bugs so the strict parse is clean.
2. **clang vs MSVC layout.** clang's MS-ABI record layout is mature, and every evaluated `sizeof` assert agrees. Field offsets, bit-fields and `[[msvc::no_unique_address]]` without asserts are unverified. Mitigation: cross-check against game RTTI/vtables and build the S6 clang-cl canary; later, compare against a real `cl.exe` `/d1reportAllClassLayout` once that is available.
3. **Printed names are not keys.** The dumps spell types for humans:
   - `_Bool` for `bool`;
   - template arguments as written (`BSPointerHandle<TESObjectREFR>` unqualified);
   - vtable headers without template arguments.

   Joining layouts, vtables and AST by string works for non-template classes (100% here) but not reliably for instantiations. The generator should key on AST declarations and canonical types, or have clang print a mangled name.
4. **Tool and format drift.** clang's JSON AST is not a stable interface (the duplicate keys, the location compression). cargo-xwin installs the *latest* CRT/STL, which changes `std::` layouts and the `_MSC_VER` we claim. Mitigation: pin the clang major version, record the STL version (it is in `dump-info.json`), and fail loudly on unknown shapes. The parser already refuses non-JSON bytes and truncated streams.
5. **The vtable trick depends on a CodeGen side effect.** Debug info is what computes the vftable slots. If clang stops doing that, the fallback is a generated virtual call per method, which also computes them.
6. **Fetch integrity.** GitHub tag tarballs have changed hashes before (2023). A change makes `dump` fail on the SHA-256 check rather than use different headers. `--offline` and `--third-party` allow a vetted local copy.
7. **Scale.** 341 MB of AST takes about 10 s and 455 MB RSS with serde. That is fine for a generator run. If it grows, narrow the filter (for example `RE::` plus `REL`) or move to libclang.
8. **The source is only as good as CommonLibF4.** Asserts pin sizes, not semantics. Every generated fact keeps the `file:line` that clang reports, for the `clf4` provenance tag.

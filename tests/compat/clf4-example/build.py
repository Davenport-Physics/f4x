"""Spike S6: build CommonLibF4's ExampleProject as an F4SE plugin DLL on Linux.

Run through build.sh (python3 -I). It compiles the ExampleProject, the CommonLibF4
library sources and the compiled parts of fmt, spdlog and rsm-mmio with clang-cl,
archives each library with llvm-lib, compiles version.rc with llvm-rc and links the
DLL with lld-link against the cargo-xwin MSVC CRT/SDK. The static CRT (/MT) is used
throughout. Everything is written to target/compat/clf4-example/ and the last line
printed is the path of the DLL.

Inputs, none of which is modified:
  ../CommonLibF4                     MIT, read-only (override: F4X_CLF4)
  target/third_party/                pinned deps fetched by S2: cargo run --release -p f4x-codegen -- dump
  ~/.cache/cargo-xwin/xwin           MSVC CRT/STL + Windows SDK (override: F4X_XWIN)

Options: --jobs N, --verbose (show the compiler output of every TU, not only of failures).
build-info.json in the output directory has every argv.
"""

import concurrent.futures
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent.parent
CLF4 = Path(os.environ.get("F4X_CLF4", ROOT.parent / "CommonLibF4")).resolve()
XWIN = Path(os.environ.get("F4X_XWIN", Path.home() / ".cache/cargo-xwin/xwin"))
TP = ROOT / "target/third_party"
OUT = ROOT / "target/compat/clf4-example"

CLF4_COMMIT = "67173f8d44b9a928dbab983d25e576ac5f1e83ac"
PROJECT, VERSION = "ExampleProject", (1, 0, 0)  # ExampleProject/CMakeLists.txt project()

TOOLS = {name: os.environ.get(env, name) for name, env in [
    ("clang-cl", "F4X_CLANG_CL"), ("lld-link", "F4X_LLD_LINK"),
    ("llvm-lib", "F4X_LLVM_LIB"), ("llvm-rc", "F4X_LLVM_RC")]}

# Third-party trees as S2 unpacks them (crates/f4x-codegen/src/thirdparty.rs):
# name -> (directory under target/third_party, archive top directory, SHA-256 of the tarball).
DEPS = {
    "fmt": ("fmt-12.1.0", "fmt-12.1.0", "ea7de4299689e12b6dddd392f9896f08fb0777ac7168897a244a6d6085043fea"),
    "spdlog": ("spdlog-1.16.0", "spdlog-1.16.0", "8741753e488a78dd0d0024c980e1fb5b5c85888447e309d9cb9d949bdb52aa3e"),
    "mmio": ("rsm-mmio-2.0.0", "mmio-2.0.0", "360dddf74a97bd0a7eb41378cc59f2a69871dabfd36c55bf027429ac54930d5b"),
    "stl_interfaces": ("boost-stl_interfaces-1.89.0", "stl_interfaces-boost-1.89.0", "515b160567b9c684ad2ea6e9237887cd8481e6a47cf06083c689a449917800d8"),
    "boost_config": ("boost-config-1.89.0", "config-boost-1.89.0", "a16a53f8f0dde2a9ff485fb134a069e54aca738009d825837dac9a35cd4afe0a"),
    "boost_assert": ("boost-assert-1.89.0", "assert-boost-1.89.0", "e25e42fbc5f77dcb203a3479eaf2e9303511f2a5bb8cbdf54e9c562fcbf29da1"),
}


def dep(name: str) -> Path:
    directory, top, _ = DEPS[name]
    return TP / directory / "src" / top


# ---------------------------------------------------------------------------------
# Flags. Everything the build passes to a tool is defined in this section.
# ---------------------------------------------------------------------------------

# Every translation unit.
CXXFLAGS = [
    "--no-default-config",               # ignore /etc/clang/*.cfg (reproducible)
    "--target=x86_64-pc-windows-msvc",
    "/nologo", "/c", "/X",               # /X: no %INCLUDE%; only the xwin paths below
    "-fms-compatibility-version=19.44",  # _MSC_VER of the xwin STL 14.44 (clang default 19.33)
    "/std:c++20",                        # CommonLibF4: cxx_std_20; deps built with the same standard
    "/permissive-",                      # CommonLibF4 PUBLIC
    "/Zc:twoPhase-",                     # -fdelayed-template-parsing, as S2: BSScriptUtil.h:558/:569
    "-Wno-delayed-template-parsing-in-cxx20",
    "/EHsc", "/GR",
    "/MT",                               # static CRT: libcmt, libcpmt, libvcruntime, libucrt
    "/O2", "/Ob2", "/Gy", "/Gw",         # CMake Release + function/data sections for /OPT:REF
    "/Z7",                               # CodeView in each .obj; lld-link writes the PDB
    "/utf-8",                            # fmt and spdlog require it (FMT_UNICODE)
    "/Brepro",                           # no timestamps in .obj
    "-fno-color-diagnostics",
]
DEFINES = ["NDEBUG"]                     # Release: ExampleProject logs to a file, not OutputDebugString

# The xwin CRT/STL and Windows SDK, as system include directories.
SYSTEM_INCLUDES = [XWIN / p for p in ("crt/include", "sdk/include/ucrt", "sdk/include/um", "sdk/include/shared")]

FMT_INC = dep("fmt") / "include"
SPDLOG_INC = dep("spdlog") / "include"
MMIO_INC = dep("mmio") / "include"
BOOST_INCS = [dep(n) / "include" for n in ("stl_interfaces", "boost_config", "boost_assert")]
SPDLOG_PUBLIC_DEFINES = ["SPDLOG_COMPILED_LIB", "SPDLOG_FMT_EXTERNAL"]  # vcpkg's spdlog port
CLF4_PUBLIC_DEFINES = ["BOOST_STL_INTERFACES_DISABLE_CONCEPTS", "WINVER=0x0601", "_WIN32_WINNT=0x0601"]
# Headers CommonLibF4's consumers see (third-party ones as system headers).
CLF4_PUBLIC_SYSTEM = [FMT_INC, SPDLOG_INC, MMIO_INC, *BOOST_INCS]

EXAMPLE = CLF4 / "ExampleProject"
LIBRARIES = {
    # name: sources, extra flags, defines, includes, system includes
    "fmt": dict(
        sources=[dep("fmt") / "src" / f for f in ("format.cc", "os.cc")],
        defines=[], includes=[FMT_INC], system=[], flags=[]),
    "spdlog": dict(
        sources=[dep("spdlog") / "src" / f for f in
                 ("spdlog.cpp", "stdout_sinks.cpp", "color_sinks.cpp", "file_sinks.cpp", "async.cpp", "cfg.cpp")],
        defines=SPDLOG_PUBLIC_DEFINES, includes=[SPDLOG_INC], system=[FMT_INC], flags=[]),
    "mmio": dict(
        sources=[dep("mmio") / "src/mmio/mmio.cpp"],
        defines=[], includes=[MMIO_INC, dep("mmio") / "src"], system=[], flags=[]),
    # Sources from CommonLibF4/cmake/sourcelist.cmake (filled in below). Its PCH is force-included.
    "CommonLibF4": dict(
        sources=[],
        defines=[*CLF4_PUBLIC_DEFINES, *SPDLOG_PUBLIC_DEFINES],
        includes=[CLF4 / "CommonLibF4/include"], system=CLF4_PUBLIC_SYSTEM,
        flags=["/FIF4SE/Impl/PCH.h"]),
}
# The plugin itself: ExampleProject/src/main.cpp with its PCH force-included, /W4 like its CMake.
# CommonLibF4's headers are system headers here so /WX holds the example's own code only.
PLUGIN = dict(
    sources=[EXAMPLE / "src/main.cpp"],
    defines=[*CLF4_PUBLIC_DEFINES, *SPDLOG_PUBLIC_DEFINES, "_UNICODE"],
    includes=[OUT / "include", EXAMPLE / "src"],
    system=[CLF4 / "CommonLibF4/include", *CLF4_PUBLIC_SYSTEM],
    flags=["/FIPCH.h", "/W4", "/WX"])

# llvm-rc preprocesses with clang; its one warning (winver.h includes <SpecStrings.h>,
# xwin ships specstrings.h plus a case symlink) cannot be silenced from llvm-rc.
RCFLAGS = ["/nologo", "/C", "65001", "/X", *[f for p in SYSTEM_INCLUDES for f in ("/I", str(p))]]

LDFLAGS = [
    "/nologo", "/DLL", "/MACHINE:X64",
    "/DEBUG:FULL", "/OPT:REF", "/OPT:ICF", "/INCREMENTAL:NO",  # ExampleProject Release link options
    "/Brepro", "/WX",
    "/ignore:4099",  # the xwin CRT .libs reference PDBs that are not shipped (as .cargo/config.toml)
    *[f"/LIBPATH:{XWIN / p}" for p in ("crt/lib/x86_64", "sdk/lib/ucrt/x86_64", "sdk/lib/um/x86_64")],
]
# CommonLibF4 links Version.lib; WinAPI.cpp/Logger.cpp need user32 (MessageBox), shell32
# (SHGetKnownFolderPath), ole32 (CoTaskMemFree), advapi32. The CRT comes from /MT's /DEFAULTLIBs.
SYSTEM_LIBS = ["kernel32.lib", "user32.lib", "shell32.lib", "ole32.lib", "advapi32.lib", "version.lib"]

# Source fixes for constructs clang rejects. Each is applied to a copy under
# target/compat/clf4-example/patched/; CommonLibF4 itself is never written.
# (path under CommonLibF4/, exact text, replacement, why)
PATCHES = [
    ("CommonLibF4/src/F4SE/API.cpp",
     "std::function<const void*(F4SEAPI)(const char*)>",
     "std::function<const void* F4SEAPI(const char*)>",
     "MSVC accepts a calling convention in parentheses with no declarator, `R(__cdecl)(A)`; "
     "clang parses it as a function returning a function. Same type, standard spelling."),
]

# ---------------------------------------------------------------------------------


def fail(message: str) -> None:
    print(f"build.sh: error: {message}", file=sys.stderr)
    sys.exit(1)


def check_inputs() -> None:
    for tool in TOOLS.values():
        if shutil.which(tool) is None:
            fail(f"{tool} not found on PATH (Fedora: dnf install clang lld llvm)")
    if not (EXAMPLE / "src/main.cpp").is_file():
        fail(f"CommonLibF4 not found at {CLF4} (set F4X_CLF4)")
    head = subprocess.run(["git", "-C", str(CLF4), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    if head != CLF4_COMMIT:
        print(f"build.sh: warning: CommonLibF4 is at {head or '?'}, provenance pins {CLF4_COMMIT}", file=sys.stderr)
    if not all((XWIN / p).is_dir() for p in ("crt/include", "crt/lib/x86_64", "sdk/include/um", "sdk/lib/um/x86_64")):
        fail(f"no xwin CRT/SDK at {XWIN}; run `cargo win -p f4x-probe` once (cargo-xwin fills it) or set F4X_XWIN")
    for name, (directory, top, sha) in DEPS.items():
        stamp = TP / directory / ".f4x-verified"
        try:
            ok = stamp.read_text().strip() == sha and (TP / directory / "src" / top).is_dir()
        except OSError:
            ok = False
        if not ok:
            fail(f"{name} is missing or unverified in {TP / directory}; "
                 "fill the S2 cache with `cargo run --release -p f4x-codegen -- dump`")


def clf4_sources() -> list[Path]:
    text = (CLF4 / "CommonLibF4/cmake/sourcelist.cmake").read_text()
    return [CLF4 / "CommonLibF4" / s for s in re.findall(r"^\s*(src/\S+\.cpp)\s*$", text, re.M)]


def configure(template: Path, out: Path) -> None:
    """CMake configure_file(... @ONLY) for the two ExampleProject templates."""
    major, minor, patch = VERSION
    values = {
        "PROJECT_NAME": PROJECT, "PROJECT_VERSION": f"{major}.{minor}.{patch}",
        "PROJECT_VERSION_MAJOR": str(major), "PROJECT_VERSION_MINOR": str(minor), "PROJECT_VERSION_PATCH": str(patch),
    }
    text = re.sub(r"@(\w+)@", lambda m: values[m.group(1)], template.read_text())
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text)


def patched(source: Path) -> Path:
    """The source to compile: `source`, or its patched copy if PATCHES names it."""
    rel = source.relative_to(CLF4).as_posix() if source.is_relative_to(CLF4) else None
    fixes = [p for p in PATCHES if p[0] == rel]
    if not fixes:
        return source
    text = source.read_text()
    for _, old, new, _ in fixes:
        if text.count(old) != 1:
            fail(f"patch for {rel} no longer applies (expected one `{old}`); revisit PATCHES")
        text = text.replace(old, new)
    copy = OUT / "patched" / rel
    copy.parent.mkdir(parents=True, exist_ok=True)
    copy.write_text(text)
    return copy


def compile_command(unit: dict, source: Path, obj: Path) -> list[str]:
    return [
        TOOLS["clang-cl"], *CXXFLAGS,
        *[f"/D{d}" for d in DEFINES + unit["defines"]],
        *[f for p in SYSTEM_INCLUDES + unit["system"] for f in ("/imsvc", str(p))],
        *[f for p in unit["includes"] for f in ("/I", str(p))],
        *unit["flags"],
        str(source), f"/Fo{obj}",
    ]


def run(argv: list[str], log: list) -> tuple[int, str]:
    started = time.monotonic()
    proc = subprocess.run(argv, capture_output=True, text=True, cwd=OUT)
    output = proc.stdout + proc.stderr
    log.append({"argv": argv, "status": proc.returncode, "seconds": round(time.monotonic() - started, 2),
                "warnings": len(re.findall(r": warning: ", output))})
    return proc.returncode, output


def main() -> None:
    args = sys.argv[1:]
    verbose = "--verbose" in args
    jobs = int(args[args.index("--jobs") + 1]) if "--jobs" in args else os.cpu_count() or 4
    check_inputs()
    LIBRARIES["CommonLibF4"]["sources"] = clf4_sources()

    if OUT.exists():
        shutil.rmtree(OUT)
    (OUT / "obj").mkdir(parents=True)
    configure(EXAMPLE / "cmake/Version.h.in", OUT / "include/Version.h")
    configure(EXAMPLE / "cmake/version.rc.in", OUT / "version.rc")

    units = [(name, unit) for name, unit in LIBRARIES.items()] + [(PROJECT, PLUGIN)]
    jobs_list = []
    for name, unit in units:
        (OUT / "obj" / name).mkdir()
        for source in unit["sources"]:
            rel = source.relative_to(CLF4 / "CommonLibF4/src") if name == "CommonLibF4" else Path(source.name)
            obj = OUT / "obj" / name / (str(rel.with_suffix("")).replace("/", "_") + ".obj")
            jobs_list.append((name, source, obj, compile_command(unit, patched(source), obj)))

    log: list = []
    started = time.monotonic()
    failed = False
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {pool.submit(run, argv, log): (name, source) for name, source, _, argv in jobs_list}
        for future in concurrent.futures.as_completed(futures):
            name, source = futures[future]
            status, output = future.result()
            warnings = len(re.findall(r": warning: ", output))
            print(f"  cc  {name:<14} {source.name:<28} {'FAILED' if status else 'ok'}"
                  f"{f' ({warnings} warnings)' if warnings else ''}", file=sys.stderr)
            if output and (status or verbose):
                print(output, file=sys.stderr)
            failed |= status != 0
    if failed:
        fail("compilation failed")

    libs = []
    for name, _ in units[:-1]:
        lib = OUT / f"{name}.lib"
        objs = sorted(str(obj) for n, _, obj, _ in jobs_list if n == name)
        status, output = run([TOOLS["llvm-lib"], "/nologo", f"/OUT:{lib}", *objs], log)
        if status:
            fail(f"llvm-lib {name}:\n{output}")
        libs.append(str(lib))

    res = OUT / "version.res"
    status, output = run([TOOLS["llvm-rc"], *RCFLAGS, "/FO", str(res), str(OUT / "version.rc")], log)
    if status or output.strip():
        print(output, file=sys.stderr)
    if status:
        fail("llvm-rc failed")

    dll = OUT / f"{PROJECT}.dll"
    plugin_objs = [str(obj) for n, _, obj, _ in jobs_list if n == PROJECT]
    status, output = run([TOOLS["lld-link"], *LDFLAGS, f"/OUT:{dll}", f"/PDB:{dll.with_suffix('.pdb')}",
                          *plugin_objs, str(res), *libs, *SYSTEM_LIBS], log)
    if output.strip():
        print(output, file=sys.stderr)
    if status:
        fail("lld-link failed")

    data = dll.read_bytes()
    version = subprocess.run([TOOLS["clang-cl"], "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    info = {
        "clang": version,
        "commonlibf4": CLF4_COMMIT,
        "xwin": str(XWIN),
        "seconds": round(time.monotonic() - started, 1),
        "dll": {"path": str(dll), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()},
        "warnings": sum(entry["warnings"] for entry in log),
        "commands": sorted(log, key=lambda entry: entry["argv"][-1]),
    }
    (OUT / "build-info.json").write_text(json.dumps(info, indent=1) + "\n")
    print(f"built {dll.name}: {len(data)} bytes, sha256 {info['dll']['sha256']}, "
          f"{info['warnings']} warnings, {info['seconds']} s", file=sys.stderr)
    print(dll)


if __name__ == "__main__":
    main()

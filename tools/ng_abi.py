"""Next-gen F4SE plugin-ABI checks with the `f4x_ng_abi.dll` test plugin
(`tests/plugins/ng-abi`). Build it, make patched copies that declare different version
data, load them all in one session per host, then compare:

    cargo xwin build --release --target x86_64-pc-windows-msvc --target-dir target/ng -p f4x-ng-abi-test
    cargo xwin build ... --target-dir target/ng-abi-query -p f4x-ng-abi-test --features query     # optional
    cargo xwin build ... --target-dir target/ng-abi-legacy -p f4x-ng-abi-test --features legacy   # optional
    python3 -I tools/ng_abi.py variants target/ng/x86_64-pc-windows-msvc/release/f4x_ng_abi.dll OUT/ \
        [target/ng-abi-query/.../f4x_ng_abi.dll target/ng-abi-legacy/.../f4x_ng_abi.dll]
    f4x launch --host f4se --profile f4se-ng-baseline --headless --main-loop --test-mode $(for f in OUT/*.dll; do printf -- '--plugin %s ' "$f"; done)
    f4x launch --profile vanilla-f4x-ng ...same plugins...
    python3 -I tools/ng_abi.py summary SESSION            # per copy: loaded or not, why, what it saw
    python3 -I tools/ng_abi.py diff SESSION_F4SE SESSION_F4X
    python3 -I tools/ng_abi.py diff baselines/1.11.240/ng-abi-f4se.jsonl SESSION_F4X   # recorded F4SE 0.7.9 run

`variants` only rewrites the copies' exported `F4SEPlugin_Version` data (layout: the
next-gen CommonLibF4 fork's `PluginVersionData`, see `crates/f4x-abi/src/f4se.rs`).
Each copy gets its own plugin name, so duplicate-name handling never interferes.
"""
import json, os, re, struct, sys

V = lambda a, b, c, d=0: (a & 0xFF) << 24 | (b & 0xFF) << 16 | (c & 0xFFF) << 4 | (d & 0xF)
R_1_11_221, R_1_11_240 = V(1, 11, 221), V(1, 11, 240)
SIG, AL980, AL137 = 1 << 0, 1 << 1, 1 << 2
NOSTRUCT, L980, L137 = 1 << 0, 1 << 1, 1 << 2

# The copies: name -> version data fields. Unset fields keep the plugin's own declaration
# (the fork template: Address Library 1.11.137+, layout 1.11.137+, compatible [1.11.221]).
# `IND` (signature scanning, no structures, no list) is the declaration F4SE 0.7.9 accepts
# on any runtime; the isolation copies start from it so only one thing changes.
IND = dict(addr=SIG, structs=NOSTRUCT, versions=[])
VARIANTS = {
    # Which runtimes a list matches.
    "a_listed_exact": dict(addr=0, structs=0, versions=[R_1_11_240]),
    "b_listed_sub": dict(addr=0, structs=0, versions=[V(1, 11, 240, 5)]),
    "b2_listed_second": dict(addr=0, structs=0, versions=[R_1_11_221, R_1_11_240]),
    "b3_listed_after_zero": dict(addr=0, structs=0, versions=[R_1_11_221, 0, R_1_11_240]),
    "b4_listed_16th": dict(addr=0, structs=0, versions=[R_1_11_221] * 15 + [R_1_11_240]),
    "c_unlisted_noflags": dict(addr=0, structs=0, versions=[R_1_11_221]),
    # Independence bits, one side at a time (the other side independent).
    "d_al980": dict(addr=AL980, structs=NOSTRUCT, versions=[R_1_11_221]),
    "e_al137": dict(addr=AL137, structs=NOSTRUCT, versions=[R_1_11_221]),
    "e3_addr_bit3": dict(addr=1 << 3, structs=NOSTRUCT, versions=[R_1_11_221]),
    "e4_addr_bit4": dict(addr=1 << 4, structs=NOSTRUCT, versions=[R_1_11_221]),
    "e5_addr_bit5": dict(addr=1 << 5, structs=NOSTRUCT, versions=[R_1_11_221]),
    "e6_addr_bit6": dict(addr=1 << 6, structs=NOSTRUCT, versions=[R_1_11_221]),
    "e7_addr_bit7": dict(addr=1 << 7, structs=NOSTRUCT, versions=[R_1_11_221]),
    "g1_layout980": dict(addr=SIG, structs=L980, versions=[R_1_11_221]),
    "g2_layout137": dict(addr=SIG, structs=L137, versions=[R_1_11_221]),
    "g3_struct_bit3": dict(addr=SIG, structs=1 << 3, versions=[R_1_11_221]),
    "g4_struct_bit4": dict(addr=SIG, structs=1 << 4, versions=[R_1_11_221]),
    "g5_struct_bit5": dict(addr=SIG, structs=1 << 5, versions=[R_1_11_221]),
    "g6_struct_bit6": dict(addr=SIG, structs=1 << 6, versions=[R_1_11_221]),
    "g7_struct_bit7": dict(addr=SIG, structs=1 << 7, versions=[R_1_11_221]),
    "h_sig_only": dict(addr=SIG, structs=0, versions=[R_1_11_221]),
    "h2_nostruct_only": dict(addr=0, structs=NOSTRUCT, versions=[R_1_11_221]),
    "h3_independent": dict(IND),
    "h4_template": dict(),
    # One field changed from an accepted declaration.
    "i_xse_newer": dict(IND, xse=V(0, 7, 10)),
    "i2_xse_equal": dict(IND, xse=V(0, 7, 9)),
    "i3_xse_sub": dict(IND, xse=V(0, 7, 9, 1)),
    "i4_xse_older": dict(IND, xse=V(0, 7, 8)),
    "k_dataver2": dict(IND, data_version=2),
    "k2_dataver0": dict(IND, data_version=0),
    "m_breaking": dict(IND, reserved_breaking=1),
    "m2_nonbreaking": dict(IND, reserved_non_breaking=1),
    "o_noname": dict(IND, name=""),
    "s1_dupname": dict(IND, name="f4x_ng_abi_dup"),
    "s2_dupname": dict(IND, name="f4x_ng_abi_dup"),
    "t_preload_false": dict(IND),
    "u_load_false": dict(IND),
    "w_listed_and_xse_newer": dict(addr=0, structs=0, versions=[R_1_11_240], xse=V(0, 7, 10)),
    "k3_dataver99": dict(IND, data_version=99),
    # Which check wins when two apply.
    "n1_al137_listed": dict(addr=AL137, structs=L137, versions=[R_1_11_240]),
    "n2_al137_sig": dict(addr=SIG | AL137, structs=NOSTRUCT, versions=[]),
    "n3_al137_listed_nostruct": dict(addr=AL137, structs=NOSTRUCT, versions=[R_1_11_240]),
    "n4_breaking_listed": dict(addr=0, structs=0, versions=[R_1_11_240], reserved_breaking=1),
    "n5_breaking_xse_newer": dict(IND, reserved_breaking=1, xse=V(0, 7, 10)),
    "n6_noname_dataver0": dict(IND, name="", data_version=0),
    "n7_al137_noname": dict(addr=AL137, structs=L137, versions=[R_1_11_221], name=""),
    "n8_al980_listed_xse_newer": dict(addr=AL980, structs=L137, versions=[R_1_11_221], xse=V(0, 7, 10)),
    # Entry points missing (the export renamed in place).
    "z1_no_load": dict(IND, rename={"F4SEPlugin_Load": "F4SEPlugin_Loae"}),
    "z2_no_preload": dict(IND, rename={"F4SEPlugin_Preload": "F4SEPlugin_Preloae"}),
}


def pe_export(data, wanted):
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    optsz = struct.unpack_from("<H", data, pe + 20)[0]
    secs = []
    off = pe + 24 + optsz
    for _ in range(nsec):
        vsize, va, rawsize, rawptr = struct.unpack_from("<IIII", data, off + 8)
        secs.append((va, max(vsize, rawsize), rawptr))
        off += 40

    def to_off(rva):
        for va, size, raw in secs:
            if va <= rva < va + size:
                return raw + rva - va
        raise ValueError(hex(rva))

    exp_rva = struct.unpack_from("<I", data, pe + 24 + 112)[0]
    e = to_off(exp_rva)
    nfuncs, nnames, funcs, names, ords = struct.unpack_from("<IIIII", data, e + 20)
    for i in range(nnames):
        name_rva = struct.unpack_from("<I", data, to_off(names) + 4 * i)[0]
        n = to_off(name_rva)
        name = data[n:data.index(b"\0", n)].decode()
        if name == wanted:
            ordinal = struct.unpack_from("<H", data, to_off(ords) + 2 * i)[0]
            return to_off(struct.unpack_from("<I", data, to_off(funcs) + 4 * ordinal)[0])
    raise KeyError(wanted)


def patch(data, at, name=None, addr=None, structs=None, versions=None, xse=None, data_version=None,
          reserved_breaking=None, reserved_non_breaking=None, rename=None):
    data = bytearray(data)
    for old, new in (rename or {}).items():
        # Same length and same sort position, so the export name table stays sorted.
        assert len(old) == len(new)
        at_name = bytes(data).index(old.encode() + b"\0")
        data[at_name:at_name + len(new)] = new.encode()
    if data_version is not None:
        struct.pack_into("<I", data, at + 0x000, data_version)
    if name is not None:
        data[at + 0x008:at + 0x108] = name.encode()[:255].ljust(256, b"\0")
    if addr is not None:
        struct.pack_into("<I", data, at + 0x208, addr)
    if structs is not None:
        struct.pack_into("<I", data, at + 0x20C, structs)
    if versions is not None:
        struct.pack_into("<16I", data, at + 0x210, *(versions + [0] * (16 - len(versions))))
    if xse is not None:
        struct.pack_into("<I", data, at + 0x250, xse)
    if reserved_non_breaking is not None:
        struct.pack_into("<I", data, at + 0x254, reserved_non_breaking)
    if reserved_breaking is not None:
        struct.pack_into("<I", data, at + 0x258, reserved_breaking)
    return bytes(data)


def variants(dll, out, query=None, legacy=None):
    """Patched copies of `dll`; plus, when given, the `query` build (version data and
    `F4SEPlugin_Query`, declared independent) and the `legacy` build (no version data)."""
    data = open(dll, "rb").read()
    at = pe_export(data, "F4SEPlugin_Version")
    os.makedirs(out, exist_ok=True)
    stem = os.path.splitext(os.path.basename(dll))[0]
    made = []
    for label, fields in VARIANTS.items():
        fields = dict(fields)
        fields.setdefault("name", f"{stem}_{label}")
        path = os.path.join(out, f"{stem}_{label}.dll")
        open(path, "wb").write(patch(data, at, **fields))
        made.append(path)
    if query:
        q = open(query, "rb").read()
        path = os.path.join(out, f"{stem}_x_query.dll")
        open(path, "wb").write(patch(q, pe_export(q, "F4SEPlugin_Version"), name=f"{stem}_x_query", **IND))
        made.append(path)
    if legacy:
        path = os.path.join(out, f"{stem}_y_legacy.dll")
        open(path, "wb").write(open(legacy, "rb").read())
        made.append(path)
    print("\n".join(made))


def events(session):
    """A session directory's ng-abi.jsonl, or such a file itself (e.g. the F4SE 0.7.9
    baseline, `baselines/1.11.240/ng-abi-f4se.jsonl`)."""
    path = session if session.endswith(".jsonl") else os.path.join(session, "ng-abi.jsonl")
    if not os.path.exists(path):
        return []
    return [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]


def host_log_lines(session):
    """Lines of the host's own logs that mention a test copy (F4SE: f4se-logs/)."""
    out = []
    for sub in ("f4se-logs",):
        d = os.path.join(session, sub)
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            try:
                text = open(os.path.join(d, name), encoding="utf-8", errors="replace").read()
            except OSError:
                continue
            out += [f"{name}: {line}" for line in text.splitlines() if re.search(r"ng_abi", line, re.I)]
    return out


def per_module(session):
    by = {}
    for e in events(session):
        m = by.setdefault(e["module"], {"events": []})
        m["events"].append(e["event"])
        if e["event"] in ("preload", "load", "query", "checks", "tasks_queued") and e["event"] not in m:
            m[e["event"]] = e
    return by


def summary(session):
    by = per_module(session)
    for module in sorted(by):
        m = by[module]
        kinds = sorted(set(m["events"]))
        print(f"{module}: {', '.join(kinds)}")
        for kind in ("preload", "load", "query"):
            e = m.get(kind)
            if e:
                keep = {k: e.get(k) for k in ("f4se_version", "f4se_version_raw", "runtime_version", "runtime_version_raw",
                                               "release_index", "plugin_handle", "save_folder_name", "interfaces",
                                               "plugin_info", "trampoline", "thread", "process_ms", "stack", "modules",
                                               "preloaded", "returning") if k in e}
                print(f"  {kind}: {json.dumps(keep)}")
        if "checks" in m:
            print(f"  checks: {json.dumps({k: v for k, v in m['checks'].items() if k not in ('event', 'module', 'seq')})}")
        if "tasks_queued" in m:
            print(f"  tasks: {json.dumps(m['tasks_queued'])}")
    for e in events(session):
        if e["event"] in ("message", "task_run", "task_destroy", "papyrus_callback", "scaleform_callback") and e["module"].lower() == "f4x_ng_abi.dll":
            print(f"  [{e['process_ms']}ms t{e['thread']}] {e['event']} {json.dumps({k: v for k, v in e.items() if k not in ('event', 'module', 'seq', 'process_ms', 'thread')})}")
    lines = host_log_lines(session)
    if lines:
        print("host log:")
        print("\n".join("  " + l for l in lines))


# Values that differ by host or by run and are not part of the ABI contract.
VOLATILE = {"seq", "process_ms", "thread", "stack", "modules", "permanent_runs"}
# Differences that follow from the hosts' setups: under F4SE the agent is a plugin too
# (`f4x_agent.dll`, handle 1, so every copy's handle is one higher and GetPluginInfo
# finds it).
HOST_SETUP = {"plugin_handle", "load_handle", "preload_handle", "plugin_info.f4x_agent", "plugin_info.F4X_AGENT"}


def flatten(value, prefix=""):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            out.update(flatten(v, f"{prefix}.{k}" if prefix else k))
        return out
    return {prefix: value}


def facts(session):
    out = {}
    for module, m in per_module(session).items():
        f = {"entry_points": sorted(k for k in ("preload", "query", "load") if k in m)}
        for kind in ("preload", "load", "query", "checks"):
            if kind in m:
                for k, v in flatten({k: v for k, v in m[kind].items() if k not in VOLATILE | {"event", "module"}}).items():
                    if not any(f".{h}." in f".{k}." for h in HOST_SETUP):
                        f[f"{kind}.{k}"] = v
        out[module] = f
    return out


def diff(a, b):
    """Compare two sessions' ng-abi.jsonl (A, B): which copies ran which entry points, and
    every recorded value, minus run-to-run noise and the known host-setup differences."""
    fa, fb = facts(a), facts(b)
    differences = 0
    for module in sorted(set(fa) | set(fb)):
        ea, eb = fa.get(module, {}), fb.get(module, {})
        keys = [k for k in sorted(set(ea) | set(eb)) if ea.get(k) != eb.get(k)]
        if not keys:
            continue
        differences += 1
        print(f"{module}:")
        for key in keys:
            print(f"  {key}: A {json.dumps(ea.get(key))} | B {json.dumps(eb.get(key))}")
    print(f"{differences} module(s) differ")
    return 1 if differences else 0


if __name__ == "__main__":
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "variants":
        variants(*args)
    elif cmd == "summary":
        summary(*args)
    elif cmd == "diff":
        sys.exit(diff(*args))
    else:
        sys.exit(__doc__)

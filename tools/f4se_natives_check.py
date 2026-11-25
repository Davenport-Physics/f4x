"""Call every F4SE Papyrus native (from the signature inventory) with arguments chosen from
the declared types, and record the results. Run once per host, in-game, then compare:

    python3 -I tools/f4se_natives_check.py host <f4se|f4x> <profile> <out.json> [--plan plan.json] [--only Script,...] [--signatures FILE]
    python3 -I tools/f4se_natives_check.py diff <f4se.json> <f4x.json>

`--signatures` picks the inventory (default F4SE 0.6.23's; next-gen:
crates/f4x-pex/data/f4se-0.7.9-signatures.json).

`host` launches the game itself (headless, muted, in an f4x profile), plans the calls once
(or reuses `--plan`, e.g. the other host's output, so both hosts make identical calls),
and when a call crashes the game it records "CRASH", relaunches and carries on.

`self` is a placed reference for Actor/ObjectReference, else a base form of the script's
type; arguments are small ints, both bools, short strings, forms of the declared type,
structs built from their declared fields, and arrays of those. Calls run in a fixed order,
so state changes (setters) happen identically on both hosts.
"""
import itertools, json, os, socket, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SIGNATURES = os.path.join(ROOT, "crates", "f4x-pex", "data", "f4se-0.6.23-signatures.json")

# Script type -> record signature of forms to use (references for Actor/ObjectReference).
FORM_TYPES = {
    "actor": "ACHR", "objectreference": "REFR", "actorbase": "NPC_", "armor": "ARMO", "armoraddon": "ARMA",
    "cell": "CELL", "component": "CMPO", "constructibleobject": "COBJ", "encounterzone": "ECZN",
    "equipslot": "EQUP", "headpart": "HDPT", "location": "LCTN", "matswap": "MSWP", "miscobject": "MISC",
    "objectmod": "OMOD", "perk": "PERK", "watertype": "WATR", "weapon": "WEAP", "keyword": "KYWD",
    "race": "RACE", "spell": "SPEL", "actorvalue": "AVIF", "globalvariable": "GLOB", "formlist": "FLST",
    "leveleditem": "LVLI", "enchantment": "ENCH", "projectile": "PROJ", "ammo": "AMMO", "outfit": "OTFT",
    "form": "MISC", "defaultobject": "DFOB", "scriptobject": "MISC",
}
# Calls that open menus or otherwise take over the session.
SKIP = {"ui.openmenu", "ui.closemenu", "ui.registercustommenu", "ui.registerbasiccustommenu"}


class Session:
    def __init__(self, path):
        info = json.load(open(os.path.join(path, "session.json")))
        self.sock = socket.create_connection(("127.0.0.1", info["port"]))
        self.f = self.sock.makefile("rw")
        self.seq = 0
        self.call("auth", {"token": info["token"]})

    def call(self, method, params=None):
        self.seq += 1
        self.f.write(json.dumps({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params}) + "\n")
        self.f.flush()
        while True:
            line = self.f.readline()
            if not line:
                raise RuntimeError(f"connection closed during {method} {json.dumps(params)[:200]} (crash?)")
            msg = json.loads(line)
            if msg.get("id") == self.seq:
                return msg.get("result", {"error": (msg.get("error") or {}).get("message")})


def plan_calls(s, only):
    db = json.load(open(SIGNATURES))
    structs = {}
    for sc in db["scripts"]:
        for st in sc["structs"]:
            structs[f"{sc['script']}#{st['name']}".lower()] = st["members"]
    forms_cache = {}

    def forms(sig, n=2):
        if sig not in forms_cache:
            method = "engine.forms.all" if sig in ("ACHR", "REFR", "CELL") else "engine.forms.of_type"
            r = s.call(method, {"type": sig, "limit": 6})
            ids = [f["id"] for f in r] if isinstance(r, list) else []
            if sig == "ACHR":
                ids = ["00000014"] + [i for i in ids if i != "00000014"]
            forms_cache[sig] = ids
        return forms_cache[sig][:n]

    def values(t, depth=0):
        tl = t.lower()
        if tl.endswith("[]"):
            elems = values(t[:-2], depth + 1)[:1]
            return [{"type": t, "value": []}] + ([{"type": t, "value": [strip(elems[0])]}] if elems else [])
        if tl == "bool":
            return [False, True]
        if tl == "int":
            return [0, 1, 3]
        if tl == "float":
            return [{"float": 0.5}]
        if tl == "string":
            return ["", "f4xTest"]
        if tl == "var":
            return [{"type": "Var", "value": {"var": 1}}]
        if "#" in tl:
            members = structs.get(tl)
            if members is None or depth > 1:
                return []
            fields = {}
            for name, mtype in members:
                vs = values(mtype, depth + 1)
                fields[name] = strip(vs[0]) if vs else None
            return [{"type": t, "value": {"struct": fields}}]
        sig = FORM_TYPES.get(tl)
        return [{"form": i} for i in forms(sig)] if sig else []

    def strip(v):
        return v["value"] if isinstance(v, dict) and "type" in v else v

    calls = []
    for sc in db["scripts"]:
        script = sc["script"]
        if only and script.lower() not in only:
            continue
        for fn in sc["functions"]:
            if fn["kind"] == "stub":
                continue
            key = f"{script}.{fn['name']}"
            if key.lower() in SKIP:
                continue
            selves = [None]
            if not fn["global"]:
                sig = FORM_TYPES.get(script.lower())
                selves = forms(sig) if sig else []
                if not selves:
                    continue
            arg_sets = [values(t) or [None] for t, _ in fn["params"]]
            combos = list(itertools.islice(itertools.product(*arg_sets), 3)) if arg_sets else [()]
            for self_id in selves[:2]:
                for args in combos:
                    calls.append({"call": key, "self": self_id, "args": list(args)})
    return calls


def launch(host, profile):
    import subprocess, time
    f4x = os.path.join(ROOT, "target", "debug", "f4x")
    out = subprocess.run([f4x, "launch", "--host", host, "--profile", profile, "--headless", "--main-loop", "--test-mode", "--lifetime", "3600"],
                         capture_output=True, text=True)  # no timeout: launches queue on a shared lock
    path = out.stdout.strip().splitlines()[-1]
    s = Session(path)
    def wait(menu, n=150):
        for _ in range(n):
            st = s.call("state")
            if menu in st.get("menus", []) and "LoadingMenu" not in st.get("menus", []):
                return
            time.sleep(1)
        raise RuntimeError(f"timeout waiting for {menu}")
    wait("MainMenu"); time.sleep(3)
    s.call("console.exec", {"command": "coc QASmoke"})
    wait("HUDMenu"); time.sleep(6)
    return path, s


def quit_session(path):
    import subprocess
    subprocess.run([os.path.join(ROOT, "target", "debug", "f4x"), "quit", "--session", path], capture_output=True, timeout=120)


def main_host(host, profile, out_path, plan_path, only):
    results = json.load(open(out_path + ".partial")) if os.path.exists(out_path + ".partial") else []
    calls = None
    if plan_path:
        calls = [{k: c.get(k) for k in ("call", "self", "args")} for c in json.load(open(plan_path))]
        if only:
            calls = [c for c in calls if c["call"].split(".")[0].lower() in only]
    while True:
        path, s = launch(host, profile)
        if calls is None:
            calls = plan_calls(s, only)
        try:
            while len(results) < len(calls):
                c = calls[len(results)]
                params = {"function": c["call"], "args": c["args"], "timeout_s": 3}
                if c["self"]:
                    params["self"] = c["self"]
                r = s.call("papyrus.call", params)
                results.append({**c, "result": r})
                if len(results) % 50 == 0:
                    json.dump(results, open(out_path + ".partial", "w"))
                    print(f"{len(results)}/{len(calls)}", flush=True)
            quit_session(path)
            break
        except RuntimeError:
            c = calls[len(results)]
            print(f"CRASH at {c['call']} self={c['self']} args={c['args']}", flush=True)
            results.append({**c, "result": "CRASH"})
            json.dump(results, open(out_path + ".partial", "w"))
            quit_session(path)
    json.dump(results, open(out_path, "w"), indent=1, sort_keys=True)
    if os.path.exists(out_path + ".partial"):
        os.remove(out_path + ".partial")
    print(f"{len(results)} calls, {sum(1 for r in results if r['result'] == 'CRASH')} crashes -> {out_path}")


def fold(v):
    """Struct field names are case-insensitive in Papyrus; their spelling comes from
    whichever spelling the game's string pool stored first. Compare them folded."""
    if isinstance(v, dict):
        return {k.lower(): fold(x) for k, x in v.items()}
    if isinstance(v, list):
        return [fold(x) for x in v]
    return v


def main_diff(a_path, b_path):
    a, b = json.load(open(a_path)), json.load(open(b_path))
    for r in a + b:
        r["result"] = fold(r.get("result"))
    # Compare only the scripts the second run covered (runs with --only).
    covered = {r["call"].split(".")[0] for r in b}
    a = [r for r in a if r["call"].split(".")[0] in covered]
    key = lambda r: (r["call"], r.get("self"), json.dumps(r.get("args"), sort_keys=True))
    bm = {key(r): r for r in b}
    per = {}
    for r in a:
        other = bm.get(key(r))
        same = other is not None and other.get("result") == r.get("result")
        per.setdefault(r["call"], []).append(same)
    # Where F4SE crashed, f4x only has to survive.
    for r in a:
        if r.get("result") == "CRASH":
            other = bm.get(key(r))
            per[r["call"]][-1] = other is not None and other.get("result") != "CRASH"
    ok = sorted(c for c, v in per.items() if all(v))
    bad = sorted(c for c, v in per.items() if not all(v))
    print(f"{len(ok)} natives identical, {len(bad)} differ")
    for c in bad:
        ex = next(r for r in a if r["call"] == c and bm.get(key(r), {}).get("result") != r.get("result"))
        print(f"  {c}: F4SE {json.dumps(ex.get('result'))[:110]} | f4x {json.dumps(bm.get(key(ex), {}).get('result'))[:110]}")
    return 0 if not bad else 1


if __name__ == "__main__":
    if "--signatures" in sys.argv:
        SIGNATURES = os.path.abspath(sys.argv[sys.argv.index("--signatures") + 1])
    if sys.argv[1] == "host":
        only = None
        if "--only" in sys.argv:
            only = {x.lower() for x in sys.argv[sys.argv.index("--only") + 1].split(",")}
        plan = sys.argv[sys.argv.index("--plan") + 1] if "--plan" in sys.argv else None
        main_host(sys.argv[2], sys.argv[3], sys.argv[4], plan, only)
    else:
        sys.exit(main_diff(sys.argv[2], sys.argv[3]))

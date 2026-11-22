"""Differential compatibility run: the same scenarios under real F4SE and under f4x.

usage: python3 -I tools/compat/compat.py [--out DIR] [--only NAME,...] [--hosts f4se,f4x]

Scenarios (each runs once per host, headless, muted, in f4x-owned profiles):
  surface   root.f4se in MainMenu: members, directory listings, AllowTextInput's
            effect on the text-entry count, where plugin objects land (user-mods-*).
  saveload  coc QASmoke -> save -> quit; relaunch -> load from the main menu (answer the
            mods prompt) -> press WorkshopMode -> save. Compares the .f4se co-saves
            byte for byte and the Papyrus logs (user-mods-*).
  mcm       pause menu -> MOD CONFIG; screenshots compared within a tolerance
            (user-mods-*; needs Pillow for the pixel comparison).
  natives   every Location/EncounterZone through papyrus.call (vanilla profiles).
  looks     LooksMenu (if installed): its Papyrus API and co-save block
            (tools/looks_check.py), and its root.f4se.plugins.F4EE object inside the
            open LooksMenu (user-mods-*).

Profiles: user-mods-f4se/user-mods-f4x (`cargo xtask profile create NAME --host H --data
install --plugin <f4x_probe.dll>`), f4se-baseline/vanilla-f4x. Writes <out>/report.json
and exits 1 if anything differs.
"""
import argparse, json, os, re, socket, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
F4X = os.path.join(ROOT, "target", "debug", "f4x")
TOOLS = os.path.join(ROOT, "tools")
PROFILES = os.path.expanduser("~/.local/share/f4x/profiles")
MY_GAMES = "prefix/pfx/drive_c/users/steamuser/Documents/My Games/Fallout4"


PROBE = os.path.join(ROOT, "target", "x86_64-pc-windows-msvc", "release", "f4x_probe.dll")


class Session:
    def __init__(self, host, profile, lifetime=900):
        cmd = [F4X, "launch", "--host", host, "--profile", profile, "--headless", "--main-loop", "--test-mode", "--lifetime", str(lifetime)]
        if host == "f4se":
            cmd += ["--plugin", PROBE]  # f4x profiles carry the probe in Data/F4SE/Plugins
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=400)
        self.dir = out.stdout.strip().splitlines()[-1]
        info_path = os.path.join(self.dir, "session.json")
        for _ in range(120):
            if os.path.exists(info_path):
                break
            time.sleep(1)
        info = json.load(open(info_path))
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
                raise RuntimeError(f"{method}: connection closed (crash? see {self.dir})")
            msg = json.loads(line)
            if msg.get("id") == self.seq:
                if "error" in msg:
                    return {"error": msg["error"].get("message")}
                return msg["result"]

    def menus(self):
        return self.call("state").get("menus", [])

    def wait_menu(self, menu, timeout=150):
        for _ in range(timeout):
            m = self.menus()
            if menu in m and "LoadingMenu" not in m:
                return True
            time.sleep(1)
        raise RuntimeError(f"timeout waiting for {menu}")

    def console(self, command):
        return self.call("console.exec", {"command": command})

    def press(self, event, key):
        return self.call("input.press", {"event": event, "key": key})

    def screenshot(self, path):
        subprocess.run([F4X, "screenshot", "--session", self.dir, "-o", path], capture_output=True, timeout=60)

    def quit(self):
        subprocess.run([F4X, "quit", "--session", self.dir], capture_output=True, timeout=120)


def my_games(profile):
    return os.path.join(PROFILES, profile, MY_GAMES)


def load_from_main_menu(s, save):
    s.wait_menu("MainMenu")
    time.sleep(3)
    s.console(f"load {save}")
    try:
        s.wait_menu("MessageBoxMenu", 60)  # "Mods are currently loaded... continue?"
        time.sleep(1)
        s.press("Accept", 28)
    except RuntimeError:
        pass
    s.wait_menu("HUDMenu")
    time.sleep(8)


# ---- scenarios: each returns a JSON-able capture ----

def scenario_surface(host, out):
    cand = os.path.join(out, "candidates.json")
    s = Session(host, f"user-mods-{host}")
    try:
        s.wait_menu("MainMenu")
        time.sleep(5)
        capture = os.path.join(out, f"surface-{host}.json")
        subprocess.run([sys.executable, "-I", os.path.join(TOOLS, "f4se_object.py"), s.dir, cand, capture], check=True, timeout=300)
        return json.load(open(capture))
    finally:
        s.quit()


def scenario_saveload(host, out):
    profile = f"user-mods-{host}"
    saves = os.path.join(my_games(profile), "Saves")
    papyrus = os.path.join(my_games(profile), "Logs", "Script", "Papyrus.0.log")
    if os.path.exists(papyrus):
        os.remove(papyrus)
    s = Session(host, profile)
    try:
        s.wait_menu("MainMenu")
        s.console("coc QASmoke")
        s.wait_menu("HUDMenu")
        time.sleep(8)
        s.console("save compat1")
        time.sleep(10)
    finally:
        s.quit()
    s = Session(host, profile)
    try:
        load_from_main_menu(s, "compat1")
        s.press("WorkshopMode", 47)
        time.sleep(4)
        s.console("save compat2")
        time.sleep(10)
    finally:
        s.quit()
    result = {}
    for name in ("compat1", "compat2"):
        p = os.path.join(saves, f"{name}.f4se")
        result[name] = open(p, "rb").read().hex() if os.path.exists(p) else None
    if os.path.exists(papyrus):
        # Heap addresses in messages differ run to run: normalise them.
        # Normalise what differs run to run: heap addresses and ids of forms created at
        # run time (FFxxxxxx). Compare distinct messages: how often a timer-driven
        # script retries depends on how long the run lasted.
        def norm(l):
            l = re.sub(r"0x[0-9A-Fa-f]{8,16}", "0xADDR", l.split("] ", 1)[-1].rstrip())
            return re.sub(r"\(FF[0-9A-Fa-f]{6}\)", "(FFxxxxxx)", l)
        lines = [norm(l) for l in open(papyrus, encoding="utf-8", errors="replace")]
        result["papyrus_errors"] = sorted({l for l in lines if "error" in l.lower()})
    return result


def scenario_mcm(host, out):
    s = Session(host, f"user-mods-{host}")
    try:
        load_from_main_menu(s, "compat1")
        s.press("Pause", 1)
        time.sleep(4)
        s.screenshot(os.path.join(out, f"mcm-pause-{host}.png"))
        s.press("Accept", 28)
        time.sleep(5)
        menus = s.menus()
        s.screenshot(os.path.join(out, f"mcm-open-{host}.png"))
        return {"menus_after_open": sorted(menus)}
    finally:
        s.quit()


def scenario_natives(host, out):
    s = Session(host, "f4se-baseline" if host == "f4se" else "vanilla-f4x", lifetime=1500)
    try:
        s.wait_menu("MainMenu")
        time.sleep(3)
        s.console("coc QASmoke")
        s.wait_menu("HUDMenu")
        time.sleep(5)
        capture = os.path.join(out, f"natives-{host}.json")
        subprocess.run([sys.executable, "-I", os.path.join(TOOLS, "natives_diff.py"), s.dir, capture], check=True, timeout=1200)
        return json.load(open(capture))
    finally:
        s.quit()


def scenario_looks(host, out):
    profile = f"user-mods-{host}"
    if not os.path.exists(os.path.join(PROFILES, profile, "game", "Data", "F4SE", "Plugins", "f4ee.dll")):
        return {"skipped": "LooksMenu not installed"}
    s = Session(host, profile)
    try:
        s.wait_menu("MainMenu")
        time.sleep(4)
        s.console("coc QASmoke")
        s.wait_menu("HUDMenu")
        time.sleep(8)
        capture = os.path.join(out, f"looks-{host}.json")
        subprocess.run([sys.executable, "-I", os.path.join(TOOLS, "looks_check.py"), s.dir, my_games(profile), capture], check=True, timeout=300)
        result = json.load(open(capture))
        result["blocks"] = [b for b in result.get("blocks", []) if b != "F4XP"]
        s.console("slm 14")
        s.wait_menu("LooksMenu", 60)
        time.sleep(5)
        names = json.load(open(os.path.join(out, "candidates.json")))
        r = s.call("ui.has", {"menu": "LooksMenu", "path": "root.f4se.plugins.F4EE", "names": names})
        result["f4ee_object"] = sorted(r.get("members", {})) if isinstance(r, dict) else r
        return result
    finally:
        s.quit()


SCENARIOS = {"surface": scenario_surface, "saveload": scenario_saveload, "mcm": scenario_mcm, "natives": scenario_natives, "looks": scenario_looks}


def candidates(out):
    """Identifiers from the vanilla Interface SWFs and the installed mods' SWFs."""
    data = os.path.join(PROFILES, "user-mods-f4x", "game", "Data")
    swf_dir = os.path.join(out, "swf")
    os.makedirs(swf_dir, exist_ok=True)
    pextool = os.path.join(ROOT, "target", "debug", "f4x-pextool")
    subprocess.run([pextool, "ba2", "extract", os.path.join(data, "Fallout4 - Interface.ba2"), "interface/*.swf", swf_dir],
                   capture_output=True, timeout=300)
    # SWFs packed in mod archives too (e.g. LooksMenu's).
    for archive in sorted(os.listdir(data)):
        if archive.lower().endswith(".ba2") and not archive.lower().startswith(("fallout4 - ", "dlc", "cc")):
            subprocess.run([pextool, "ba2", "extract", os.path.join(data, archive), "*.swf", os.path.join(swf_dir, "mods")], capture_output=True, timeout=300)
    paths = []
    for base in (swf_dir, os.path.join(data, "interface"), os.path.join(data, "Interface"), os.path.join(data, "MCM")):
        for dirpath, _, files in os.walk(base):
            paths += [os.path.join(dirpath, f) for f in files if f.lower().endswith(".swf")]
    names = {"plugins", "version", "major", "minor", "beta", "releaseIdx", "f4mcm", "f4xprobe"}
    plugins_dir = os.path.join(data, "F4SE", "Plugins")
    if os.path.isdir(plugins_dir):  # plugins often register under their DLL name
        names |= {os.path.splitext(f)[0] for f in os.listdir(plugins_dir) if f.lower().endswith(".dll")}
    for p in paths:
        r = subprocess.run([sys.executable, "-I", os.path.join(TOOLS, "swf_strings.py"), p], capture_output=True, text=True)
        names |= {s for s in r.stdout.splitlines() if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,63}", s)}
    json.dump(sorted(names), open(os.path.join(out, "candidates.json"), "w"))


def image_diff(a, b):
    try:
        from PIL import Image, ImageChops
    except ImportError:
        return {"skipped": "Pillow not installed"}
    if not (os.path.exists(a) and os.path.exists(b)):
        return {"missing": True}
    d = ImageChops.difference(Image.open(a).convert("RGB"), Image.open(b).convert("RGB")).convert("L")
    h = d.histogram()
    total = sum(h)
    return {"changed": (total - h[0]) / total, "over_16": sum(h[17:]) / total}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(ROOT, "target", "compat"))
    ap.add_argument("--only", default=",".join(SCENARIOS))
    ap.add_argument("--hosts", default="f4se,f4x")
    args = ap.parse_args()
    out = args.out
    os.makedirs(out, exist_ok=True)
    only = [s for s in args.only.split(",") if s]
    hosts = args.hosts.split(",")
    if "surface" in only or "looks" in only:
        candidates(out)
    report = {}
    failed = False
    for name in only:
        captures = {}
        for host in hosts:
            print(f"== {name} / {host}", flush=True)
            try:
                captures[host] = SCENARIOS[name](host, out)
            except Exception as e:  # keep going; record the failure
                captures[host] = {"error": str(e)}
        entry = {"hosts": hosts}
        if name == "mcm":
            entry["images"] = {k: image_diff(os.path.join(out, f"mcm-{k}-f4se.png"), os.path.join(out, f"mcm-{k}-f4x.png")) for k in ("pause", "open")}
            # Rendering noise (blur, animation timing) moves a few pixels a lot.
            same = captures.get("f4se") == captures.get("f4x") and all(
                v.get("over_16", 0) <= 0.0005 for v in entry["images"].values() if "over_16" in v)
        else:
            same = len(hosts) == 2 and captures[hosts[0]] == captures[hosts[1]] and "error" not in captures[hosts[0]]
            if not same and isinstance(captures.get("f4se"), dict) and isinstance(captures.get("f4x"), dict):
                entry["differing_keys"] = sorted(k for k in set(captures["f4se"]) | set(captures["f4x"])
                                                 if captures["f4se"].get(k) != captures["f4x"].get(k))
        entry["same"] = same
        failed |= not same
        report[name] = entry
        json.dump(captures, open(os.path.join(out, f"{name}.json"), "w"), indent=1, sort_keys=True)
        print(f"   {'SAME' if same else 'DIFFERENT'} {json.dumps({k: v for k, v in entry.items() if k != 'hosts'})[:300]}", flush=True)
    json.dump(report, open(os.path.join(out, "report.json"), "w"), indent=1, sort_keys=True)
    print("all scenarios match" if not failed else "DIFFERENCES FOUND", f"(report: {out}/report.json)")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()

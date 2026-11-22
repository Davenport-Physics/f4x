"""Differential check of the F4SE natives f4x implements, through papyrus.call (agent RPC).
Run once per host, in-game (scripts do not run at the main menu), then diff the outputs.
usage: python3 -I tools/natives_diff.py <session dir> <out.json>"""
import json, os, socket, sys, time

session, out_path = sys.argv[1], sys.argv[2]
info = json.load(open(os.path.join(session, "session.json")))
sock = socket.create_connection(("127.0.0.1", info["port"]))
f = sock.makefile("rw")
seq = [0]

def call(method, params=None):
    seq[0] += 1
    req = {"jsonrpc": "2.0", "id": seq[0], "method": method}
    if params is not None:
        req["params"] = params
    f.write(json.dumps(req) + "\n"); f.flush()
    while True:
        msg = json.loads(f.readline())
        if msg.get("id") == seq[0]:
            if "error" in msg:
                return {"error": msg["error"].get("message")}
            return msg["result"]

call("auth", {"token": info["token"]})
locations = call("engine.forms.of_type", {"type": "LCTN", "limit": 5000})
zones = call("engine.forms.of_type", {"type": "ECZN", "limit": 5000})
out = {"locations": {}, "zones": {}}
def short(r):
    if isinstance(r, dict) and "result" in r:
        v = r["result"]
        if isinstance(v, dict) and "form" in v:
            return (v.get("form") or {}).get("id") or v.get("handle")
        return v
    return r
for loc in locations:
    fid = loc["id"]
    out["locations"][fid] = {
        "edid": loc.get("editor_id"),
        "zone": short(call("papyrus.call", {"function": "Location.GetEncounterZone", "self": fid, "args": [False]})),
        "zone_recursive": short(call("papyrus.call", {"function": "Location.GetEncounterZone", "self": fid, "args": [True]})),
    }
for z in zones:
    fid = z["id"]
    out["zones"][fid] = {
        "edid": z.get("editor_id"),
        "workshop": short(call("papyrus.call", {"function": "EncounterZone.IsWorkshop", "self": fid})),
        "never_reset": short(call("papyrus.call", {"function": "EncounterZone.IsNeverResetable", "self": fid})),
    }
# A set/get round trip on the first zone: flip workshop, read, flip back.
if zones:
    fid = zones[0]["id"]
    before = short(call("papyrus.call", {"function": "EncounterZone.IsWorkshop", "self": fid}))
    call("papyrus.call", {"function": "EncounterZone.SetWorkshop", "self": fid, "args": [not before]})
    after = short(call("papyrus.call", {"function": "EncounterZone.IsWorkshop", "self": fid}))
    call("papyrus.call", {"function": "EncounterZone.SetWorkshop", "self": fid, "args": [before]})
    restored = short(call("papyrus.call", {"function": "EncounterZone.IsWorkshop", "self": fid}))
    out["roundtrip"] = {"zone": fid, "before": before, "after": after, "restored": restored}
json.dump(out, open(out_path, "w"), indent=1, sort_keys=True)
print(len(locations), "locations,", len(zones), "zones ->", out_path)

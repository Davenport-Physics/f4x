"""Capture the root.f4se surface of a running session (MainMenu). usage: f4se_object.py <session> <candidates.json> <out.json>"""
import json, os, socket, sys
session, cand, out_path = sys.argv[1:4]
names = json.load(open(cand))
info = json.load(open(os.path.join(session, "session.json")))
sock = socket.create_connection(("127.0.0.1", info["port"])); f = sock.makefile("rw"); seq = [0]
def call(method, params=None):
    seq[0] += 1
    f.write(json.dumps({"jsonrpc": "2.0", "id": seq[0], "method": method, "params": params}) + "\n"); f.flush()
    while True:
        line = f.readline()
        if not line: raise SystemExit("connection closed (game crashed?)")
        msg = json.loads(line)
        if msg.get("id") == seq[0]:
            return msg.get("result", {"error": msg.get("error")})
call("auth", {"token": info["token"]})
out = {}
def walk(p, d):
    r = call("ui.has", {"menu": "MainMenu", "path": p, "names": names})
    if "error" in r: return {"error": str(r["error"])}
    node = {"type": r["type"], "members": {}}
    for name, entry in sorted(r["members"].items()):
        if name in ("hasOwnProperty", "toString", "apply", "call", "prototype", "length"):
            continue  # Object/Function builtins
        node["members"][name] = walk(p + "." + name, d - 1) if entry["type"] in ("object", "array") and d > 1 else entry
    return node
out["tree"] = walk("root.f4se", 3)
out["listings"] = {}
for args in (["Data/MCM/Config", "*"], ["Data/MCM/Config/SimSettlements", "*.json"], ["MCM/Config", "*"], ["Data/Interface", "*.swf"], ["Data/MCM/Config"], ["Data/MCM/Config", "nothing*"]):
    r = call("ui.invoke", {"menu": "MainMenu", "path": "root.f4se", "method": "GetDirectoryListing", "args": args, "fields": ["name", "nativePath", "isDirectory", "isHidden", "lastModified", "creationDate"], "depth": 2})
    out["listings"][json.dumps(args)] = r
seq_text = []
for args in ([True], [True], [False], [False], [False]):
    before = call("input.state")
    r = call("ui.invoke", {"menu": "MainMenu", "path": "root.f4se", "method": "AllowTextInput", "args": [a for a in args if a is not None]})
    after = call("input.state")
    seq_text.append({"args": args, "before": before, "result": r, "after": after})
out["allow_text_input"] = seq_text
out["send_external_event"] = call("ui.invoke", {"menu": "MainMenu", "path": "root.f4se", "method": "SendExternalEvent", "args": ["f4xProbeEvent", 1, "two"]})
out["probe_mark"] = call("ui.has", {"menu": "MainMenu", "path": "root.f4se.plugins.f4xprobe", "names": ["f4xProbeMark"]})
json.dump(out, open(out_path, "w"), indent=1, sort_keys=True)
print(json.dumps(out)[:1500])

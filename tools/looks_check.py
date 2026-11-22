"""LooksMenu (F4EE) behaviour through its Papyrus API, plus its co-save block.
usage: looks_check.py <session dir> <profile My Games dir> <out.json>  (in-game)"""
import json, os, socket, struct, sys, time
session, my_games, out_path = sys.argv[1:4]
info = json.load(open(os.path.join(session, "session.json")))
sock = socket.create_connection(("127.0.0.1", info["port"])); f = sock.makefile("rw"); seq = [0]
def call(method, params=None):
    seq[0] += 1
    f.write(json.dumps({"jsonrpc": "2.0", "id": seq[0], "method": method, "params": params}) + "\n"); f.flush()
    while True:
        line = f.readline()
        if not line: raise SystemExit("connection closed (crash?)")
        msg = json.loads(line)
        if msg.get("id") == seq[0]:
            return msg.get("result", {"error": msg.get("error")})
call("auth", {"token": info["token"]})
P = {"form": "00000014"}
steps = [
    ("BodyGen.SetMorph", [P, False, "f4xMorphA", None, 0.25]),
    ("BodyGen.SetMorph", [P, False, "f4xMorphB", None, 0.75]),
    ("BodyGen.GetMorph", [P, False, "f4xMorphA", None]),
    ("BodyGen.GetMorph", [P, False, "f4xMorphB", None]),
    ("BodyGen.GetMorph", [P, False, "f4xMissing", None]),
    ("BodyGen.GetMorphs", [P, False]),
    ("BodyGen.UpdateMorphs", [P]),
    ("BodyGen.SetSkinOverride", [P, "f4xSkin"]),
    ("Overlays.AddEntry", [P, False, 1, "f4xTemplate"]),
    ("Overlays.GetAll", [P, False]),
    ("Overlays.Update", [P]),
]
out = {"calls": []}
for fn, args in steps:
    r = call("papyrus.call", {"function": fn, "args": args, "timeout_s": 10})
    out["calls"].append([fn, r])
    time.sleep(0.3)
call("console.exec", {"command": "save lookscheck"}); time.sleep(10)
b = open(os.path.join(my_games, "Saves", "lookscheck.f4se"), "rb").read(); at = 20
blocks = {}
for _ in range(struct.unpack_from("<I", b, 16)[0]):
    uid, count, length = struct.unpack_from("<III", b, at); at += 12; chunks = []
    for _ in range(count):
        kind, ver, ln = struct.unpack_from("<III", b, at); at += 12
        chunks.append([kind.to_bytes(4, "big").decode("latin1"), ver, b[at:at + ln].hex()]); at += ln
    blocks[uid.to_bytes(4, "big").decode("latin1")] = chunks
out["f4ee_block"] = blocks.get("F4EE")
out["blocks"] = sorted(blocks)
json.dump(out, open(out_path, "w"), indent=1, sort_keys=True)
print(json.dumps(out)[:1800])

"""External-event behaviour of a running session (in-game): registration, dispatch order,
argument conversion, unregister, and the EXEV co-save section.
usage: extev_check.py <session dir> <profile My Games dir> <out.json>"""
import json, os, re, socket, struct, sys, time

session, my_games, out_path = sys.argv[1:4]
info = json.load(open(os.path.join(session, "session.json")))
sock = socket.create_connection(("127.0.0.1", info["port"])); f = sock.makefile("rw"); seq = [0]
def call(method, params=None):
    seq[0] += 1
    f.write(json.dumps({"jsonrpc": "2.0", "id": seq[0], "method": method, "params": params}) + "\n"); f.flush()
    while True:
        line = f.readline()
        if not line: raise SystemExit("connection closed")
        msg = json.loads(line)
        if msg.get("id") == seq[0]:
            return msg.get("result", {"error": msg.get("error")})
call("auth", {"token": info["token"]})
def reg(form, event, cb):
    return call("papyrus.call", {"function": "ScriptObject.RegisterForExternalEvent", "self": form, "args": [event, cb]})
def send(*args):
    call("ui.invoke", {"menu": "HUDMenu", "path": "root.f4se", "method": "SendExternalEvent", "args": list(args)}); time.sleep(1)
GLOBAL = "0000003A"  # TimeScale
def gv(): return call("papyrus.call", {"function": "GlobalVariable.GetValue", "self": GLOBAL}).get("result")
def sv(v): call("papyrus.call", {"function": "GlobalVariable.SetValue", "self": GLOBAL, "args": [v]})
out = {}
events = ["evQuasarC", "evAlphaA", "evZuluD", "evMikeB"]
for e in events:
    reg("0001ED86", e, "Cb_" + e)
reg("0001ED86", "evAlphaA", "Cb_Ignored")              # second registration keeps the first callback
for q in ("00000039", "0001ED86", "000229E5", "0000003A"):  # forms whose bound script is deterministic
    reg(q, "evMulti", "Cb_Multi")
reg(GLOBAL, "evNum", "SetValue"); reg(GLOBAL, "evInt", "SetValueInt")
conv = {}
for args in (("evNum", 10), ("evNum", 10.5), ("evNum", "7"), ("evNum", True), ("evNum",), ("evInt", 10.75), ("evInt", -3)):
    original = gv(); sv(0.0); send(*args); conv[json.dumps(args)] = gv(); sv(original)
out["conversion"] = conv
call("papyrus.call", {"function": "ScriptObject.UnregisterForExternalEvent", "self": "0001ED86", "args": ["evZuluD"]})
send("evMulti"); time.sleep(2)
call("console.exec", {"command": "save extevcheck"}); time.sleep(10)
b = open(os.path.join(my_games, "Saves", "extevcheck.f4se"), "rb").read(); at = 20
uid, count, length = struct.unpack_from("<III", b, at); at += 12
section, entries = None, []
for _ in range(count):
    kind, ver, ln = struct.unpack_from("<III", b, at); at += 12
    k = kind.to_bytes(4, "big").decode("latin1"); d = b[at:at + ln]; at += ln
    if k in ("KEYR", "CTLR", "EXEV"): section = k; continue
    if k == "REGE": section = None; continue
    if k == "REGS" and section == "EXEV":
        n = struct.unpack_from("<H", d, 0)[0]; name = d[2:2 + n].decode(); i = 2 + n
        cnt = struct.unpack_from("<I", d, i)[0]; i += 4; regs = []
        for _ in range(cnt):
            h = struct.unpack_from("<Q", d, i)[0]; i += 8
            l = struct.unpack_from("<H", d, i)[0]; s = d[i + 2:i + 2 + l].decode(); i += 2 + l
            l = struct.unpack_from("<H", d, i)[0]; c = d[i + 2:i + 2 + l].decode(); i += 2 + l
            regs.append([hex(h), s, c])
        entries.append([name, regs])
names = [e[0] for e in entries]
ptr = {e["string"]: int(e["entry"], 16) for e in call("papyrus.intern", {"strings": names})}
out["exev"] = sorted(entries)                                  # content, order-independent
out["exev_in_pool_order"] = names == sorted(names, key=lambda n: ptr[n])
out["registrants_in_handle_order"] = all(r == sorted(r, key=lambda x: int(x[0], 16)) for _, r in entries)
log = os.path.join(my_games, "Logs", "Script", "Papyrus.0.log")
if os.path.exists(log):
    lines = [re.sub(r"^\[[^]]*\] ", "", l.rstrip()) for l in open(log, encoding="utf-8", errors="replace")]
    out["multi_dispatch"] = [l for l in lines if "Cb_Multi" in l]
json.dump(out, open(out_path, "w"), indent=1, sort_keys=True)
print(json.dumps(out)[:1500])

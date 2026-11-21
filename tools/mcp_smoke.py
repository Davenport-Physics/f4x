"""Live MCP smoke test: python3 -I tools/mcp_smoke.py tools/f4x-mcp.sh

Initializes the f4x MCP server, lists its tools, then launches a headless game,
queries status and the player, takes a screenshot, tails logs and quits."""
import json, subprocess, sys
proc = subprocess.Popen([sys.argv[1]], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
n = [0]
def send(method, params=None, notify=False):
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None: msg["params"] = params
    if not notify:
        n[0] += 1; msg["id"] = n[0]
    proc.stdin.write(json.dumps(msg) + "\n"); proc.stdin.flush()
    if notify: return None
    while True:
        reply = json.loads(proc.stdout.readline())
        if reply.get("id") == n[0]: return reply
r = send("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "0"}})
print("initialize:", r["result"]["serverInfo"], r["result"]["protocolVersion"])
send("notifications/initialized", notify=True)
tools = send("tools/list")["result"]["tools"]
print("tools:", [t["name"] for t in tools])
def call(name, args):
    r = send("tools/call", {"name": name, "arguments": args})["result"]
    kinds = [c["type"] for c in r["content"]]
    text = next((c["text"] for c in r["content"] if c["type"] == "text"), "")
    print(f"{name}: isError={r.get('isError', False)} content={kinds} {text[:160]!r}")
    return r
call("game_launch", {"headless": True})
call("game_status", {})
call("game_rpc", {"method": "engine.player"})
shot = call("game_screenshot", {})
img = next((c for c in shot["content"] if c["type"] == "image"), None)
print("image bytes (base64):", len(img["data"]) if img else None, img and img.get("mimeType"))
call("game_logs", {"count": 3})
call("game_quit", {})
proc.stdin.close(); proc.wait(timeout=60)

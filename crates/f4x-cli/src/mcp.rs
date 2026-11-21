//! `f4x mcp`: a Model Context Protocol server over stdio (protocol 2025-06-18, tools
//! only), hand-rolled JSON-RPC: one JSON message per line on stdin/stdout. Nothing
//! else may write to stdout; diagnostics go to stderr.
//!
//! The server drives at most one game session at a time (the *active* session):
//! `game_launch` starts one; the other tools use it, or adopt the newest running
//! session (e.g. one started by `f4x launch`) when there is none. `game_status` with a
//! `session` path attaches to a given session. When stdin closes, a session this server
//! launched is quit, so it never holds the launch lock after the agent is gone.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::Duration;

use base64::Engine;
use f4x_host::Client;
use f4x_host::client::code;
use f4x_host::contract::{DEFAULT_STARTUP_TIMEOUT_S, LaunchOptions};
use f4x_host::profile::Host;
use f4x_host::session::{self, SessionDir, format_log_line, level_rank, passes_level, tail_lines};
use f4x_proto::{RpcError, Screenshot, ScreenshotParams, method};
use serde_json::{Value, json};

use crate::{start, testing};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "\
Drive Fallout 4 with the f4x in-game agent. Typical loop: game_launch (waits for the \
main menu by default) -> game_status / game_rpc / game_screenshot / game_logs / \
game_tests_run -> game_quit. Only one game runs on this machine at a time; a launch \
queues behind other sessions. Always call game_quit when done.";

struct Active {
    dir: SessionDir,
    client: Option<Client>,
    /// The supervisor, when this server launched the session.
    supervisor: Option<Child>,
}

impl Active {
    fn attach(dir: SessionDir) -> Self {
        Active { dir, client: None, supervisor: None }
    }

    fn owned(&self) -> bool {
        self.supervisor.is_some()
    }

    fn reap(&mut self) {
        if let Some(child) = &mut self.supervisor {
            let _ = child.try_wait();
        }
    }

    fn client(&mut self) -> Result<&mut Client, RpcError> {
        if self.client.as_ref().is_none_or(|c| c.closed().is_some()) {
            self.client = None;
            self.client = Some(self.dir.connect()?);
        }
        Ok(self.client.as_mut().unwrap())
    }
}

/// A tool's result: MCP content blocks, and whether it is an error.
pub struct ToolOutput {
    pub content: Vec<Value>,
    pub is_error: bool,
}

fn text(value: &Value) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    json!({ "type": "text", "text": text })
}

fn ok(value: Value) -> Result<ToolOutput, ToolOutput> {
    Ok(ToolOutput { content: vec![text(&value)], is_error: false })
}

fn fail(value: Value) -> ToolOutput {
    ToolOutput { content: vec![text(&value)], is_error: true }
}

pub struct Server {
    /// The workspace whose `target/f4x-sessions` holds the sessions.
    root: PathBuf,
    active: Option<Active>,
}

impl Server {
    pub fn new(root: PathBuf) -> Self {
        Server { root, active: None }
    }

    /// Handle one incoming message; returns the response, if it needs one.
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message["method"].as_str().unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (initialized, cancelled, ...) and stray responses need no reply.
        let id = id?;
        if method.is_empty() {
            return None;
        }
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "f4x", "title": "f4x: Fallout 4 game control", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call_tool(&params),
            _ => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("method not found: {method}"))),
        };
        Some(match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        })
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, RpcError> {
        let name = params["name"].as_str().unwrap_or("");
        let args = match &params["arguments"] {
            Value::Null => json!({}),
            Value::Object(_) => params["arguments"].clone(),
            _ => return Err(RpcError::new(code::INVALID_PARAMS, "arguments must be an object")),
        };
        let output = match name {
            "game_launch" => self.launch(&args),
            "game_status" => self.status(&args),
            "game_rpc" => self.rpc(&args),
            "game_screenshot" => self.screenshot(&args),
            "game_logs" => self.logs(&args),
            "game_tests_run" => self.tests_run(&args),
            "game_quit" => self.quit(&args),
            _ => return Err(RpcError::new(code::INVALID_PARAMS, format!("unknown tool: {name}"))),
        };
        let output = output.unwrap_or_else(|e| e);
        Ok(json!({ "content": output.content, "isError": output.is_error }))
    }

    /// The active session, adopting the newest running one if there is none.
    fn active(&mut self) -> Result<&mut Active, ToolOutput> {
        if let Some(active) = &mut self.active {
            active.reap();
        }
        if self.active.is_none() {
            match session::latest_live(&self.root) {
                Some(dir) => self.active = Some(Active::attach(dir)),
                None => {
                    return Err(fail(json!({ "error": "no game session is running; call game_launch first" })));
                }
            }
        }
        Ok(self.active.as_mut().unwrap())
    }

    fn launch(&mut self, args: &Value) -> Result<ToolOutput, ToolOutput> {
        if let Some(active) = &mut self.active {
            active.reap();
            if active.dir.is_live() {
                return Err(fail(json!({
                    "error": "this server already has a running session; call game_quit first",
                    "session": active.dir.path.display().to_string(),
                })));
            }
        }
        self.active = None;
        let host = Host::parse(args["host"].as_str().unwrap_or("f4x")).map_err(|e| fail(json!({ "error": e })))?;
        let mut options = LaunchOptions::new(host);
        options.profile = args["profile"].as_str().map(str::to_owned);
        options.headless = args["headless"].as_bool().unwrap_or(true);
        options.test_mode = args["test_mode"].as_bool().unwrap_or(false);
        options.build = args["build"].as_bool().unwrap_or(false);
        options.plugins = args["plugins"]
            .as_array()
            .map(|list| list.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
            .unwrap_or_default();
        options.startup_timeout_s = args["startup_timeout_s"].as_u64().unwrap_or(DEFAULT_STARTUP_TIMEOUT_S);
        options.lifetime_s = Some(args["lifetime_s"].as_u64().unwrap_or(3600));
        options.label = format!("mcp-{host}");
        options.workspace = self.root.clone();
        let lock_timeout = Duration::from_secs(args["lock_timeout_s"].as_u64().unwrap_or(600));

        let started = start::start(&options, Some(lock_timeout)).map_err(|e| {
            fail(json!({
                "error": e.message,
                "session": e.session.as_ref().map(|p| p.display().to_string()),
                "hint": "see launch.log, supervisor.log, loader.log and runtime.log in the session directory (game_logs can read them once a session is active)",
            }))
        })?;
        let mut result = json!({
            "session": started.dir.path.display().to_string(),
            "endpoint": started.endpoint.public(),
            "host": host.name(),
            "headless": options.headless,
        });
        let wait = args["wait_main_loop"].as_bool().unwrap_or(true);
        let main_loop =
            wait.then(|| start::wait_for_main_loop(&started, Duration::from_secs(args["main_loop_timeout_s"].as_u64().unwrap_or(120))));
        self.active = Some(Active { dir: started.dir, client: None, supervisor: Some(started.supervisor) });
        match main_loop {
            Some(Ok(state)) => result["state"] = state,
            Some(Err(e)) => {
                result["error"] = format!("the game started but its main loop did not: {e}").into();
                result["crashes"] = self.active.as_ref().unwrap().dir.crash_summaries().into();
                return Err(fail(result));
            }
            None => {}
        }
        ok(result)
    }

    fn status(&mut self, args: &Value) -> Result<ToolOutput, ToolOutput> {
        if let Some(path) = args["session"].as_str() {
            let dir = resolve(&self.root, path).map_err(|e| fail(json!({ "error": e })))?;
            self.active = Some(Active::attach(dir));
        }
        let active = self.active()?;
        let mut status = active.dir.status(true);
        status["owned_by_this_server"] = active.owned().into();
        ok(status)
    }

    fn rpc(&mut self, args: &Value) -> Result<ToolOutput, ToolOutput> {
        let name = args["method"].as_str().ok_or_else(|| fail(json!({ "error": "`method` is required" })))?.to_owned();
        let params = args.get("params").cloned().unwrap_or(json!({}));
        let timeout = Duration::from_secs_f64(args["timeout_s"].as_f64().unwrap_or(30.0).max(0.1));
        let active = self.active()?;
        let reply = active.client().and_then(|c| c.call_timeout(&name, params, timeout));
        match reply {
            Ok(result) => ok(result),
            Err(error) => Err(rpc_failure(&active.dir, &error)),
        }
    }

    fn screenshot(&mut self, args: &Value) -> Result<ToolOutput, ToolOutput> {
        let params = ScreenshotParams { name: args["name"].as_str().map(str::to_owned) };
        let active = self.active()?;
        let reply = active.client().and_then(|c| c.call_timeout(method::SCREENSHOT, json!(params), Duration::from_secs(20)));
        let value = reply.map_err(|e| rpc_failure(&active.dir, &e))?;
        let shot: Screenshot =
            serde_json::from_value(value.clone()).map_err(|e| fail(json!({ "error": format!("unexpected reply: {e}"), "reply": value })))?;
        let path = active.dir.file(&shot.file);
        let bytes = read_when_ready(&path).map_err(|e| fail(json!({ "error": e })))?;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let info = json!({ "file": path.display().to_string(), "width": shot.width, "height": shot.height, "frame": shot.frame });
        Ok(ToolOutput { content: vec![json!({ "type": "image", "data": data, "mimeType": "image/png" }), text(&info)], is_error: false })
    }

    fn logs(&mut self, args: &Value) -> Result<ToolOutput, ToolOutput> {
        let count = args["count"].as_u64().unwrap_or(100).clamp(1, 5000) as usize;
        let min = match args["level"].as_str() {
            Some(level) => Some(level_rank(level).ok_or_else(|| fail(json!({ "error": format!("unknown level {level:?}") })))?),
            None => None,
        };
        let active = self.active()?;
        let path = active.dir.log_path(args["source"].as_str()).map_err(|e| fail(json!({ "error": e })))?;
        // Read extra lines so filtering by level still fills `count`.
        let read = if min.is_some() { count * 20 } else { count };
        let (lines, _) = tail_lines(&path, read).map_err(|e| fail(json!({ "error": e })))?;
        let lines: Vec<String> = lines.iter().filter(|l| passes_level(l, min)).map(|l| format_log_line(l)).collect();
        let shown = &lines[lines.len().saturating_sub(count)..];
        ok(Value::String(format!("{} (last {} lines)\n{}", path.display(), shown.len(), shown.join("\n"))))
    }

    fn tests_run(&mut self, args: &Value) -> Result<ToolOutput, ToolOutput> {
        let selection = testing::Selection {
            filter: args["filter"].as_str().map(str::to_owned),
            skip: args["skip"]
                .as_array()
                .map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
        };
        let timeout_s = args["timeout_s"].as_u64().unwrap_or(testing::DEFAULT_TEST_TIMEOUT_S);
        let active = self.active()?;
        let dir = active.dir.clone();
        let client = active.client().map_err(|e| rpc_failure(&dir, &e))?;
        let outcome = testing::run_tests(client, &selection, timeout_s, Duration::from_secs(120), |_| {});
        if outcome.lost_connection {
            active.dir.wait_gone(Duration::from_secs(15));
        }
        let failed = outcome.failed();
        let crashes = active.dir.crash_summaries();
        let crashed = active.dir.fatal_crash() || (outcome.lost_connection && !crashes.is_empty());
        let result = json!({
            "ok": outcome.error.is_none() && !crashed && failed == 0 && !outcome.results.is_empty(),
            "crashed": crashed,
            "passed": outcome.results.len() - failed,
            "failed": failed,
            "results": outcome.results,
            "error": outcome.error,
            "crashes": crashes,
            "summary": outcome.results.iter().map(testing::result_line).collect::<Vec<_>>(),
        });
        if result["ok"] == true { ok(result) } else { Err(fail(result)) }
    }

    fn quit(&mut self, args: &Value) -> Result<ToolOutput, ToolOutput> {
        let force = args["force"].as_bool().unwrap_or(false);
        let mut active = match self.active.take() {
            Some(active) => active,
            None => match session::latest_live(&self.root) {
                Some(dir) => Active::attach(dir),
                None => return ok(json!({ "note": "no game session is running" })),
            },
        };
        active.client = None;
        let report = active.dir.quit(force, Duration::from_secs(45));
        active.reap();
        if report["exited"] == true { ok(report) } else { Err(fail(report)) }
    }

    /// Stdin closed: quit a session this server launched.
    pub fn shutdown(&mut self) {
        if let Some(mut active) = self.active.take()
            && active.owned()
            && active.dir.is_live()
        {
            eprintln!("f4x mcp: client gone; quitting {}", active.dir.path.display());
            active.client = None;
            active.dir.quit(false, Duration::from_secs(30));
            active.reap();
        }
    }
}

/// An RPC error as a tool error, with crash reports if the game seems to be gone.
fn rpc_failure(dir: &SessionDir, error: &RpcError) -> ToolOutput {
    let mut value = json!({ "error": error, "session": dir.path.display().to_string() });
    if error.code == code::TRANSPORT || error.code == code::NO_SESSION {
        value["live"] = dir.is_live().into();
        value["crashes"] = dir.crash_summaries().into();
        if let Some(state) = dir.supervisor() {
            value["supervisor"] = json!(state);
        }
    }
    fail(value)
}

fn read_when_ready(path: &Path) -> Result<Vec<u8>, String> {
    for _ in 0..50 {
        if let Ok(bytes) = std::fs::read(path)
            && !bytes.is_empty()
        {
            return Ok(bytes);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("{} did not appear", path.display()))
}

/// A session directory given as a path, or as a name under `target/f4x-sessions`.
pub fn resolve(root: &Path, given: &str) -> Result<SessionDir, String> {
    let path = PathBuf::from(given);
    let candidates = [path.clone(), f4x_host::launch::sessions_dir(root).join(&path)];
    candidates
        .iter()
        .find(|p| p.is_dir())
        .and_then(|p| p.canonicalize().ok())
        .map(SessionDir::new)
        .ok_or_else(|| format!("no session directory {given:?}"))
}

/// The tool definitions.
pub fn tools() -> Value {
    json!([
        {
            "name": "game_launch",
            "title": "Launch Fallout 4",
            "description": "Start Fallout 4 with the f4x in-game control agent and make it this server's active session. Blocks until the agent is reachable and, by default, until the game's main loop runs (main menu; roughly 20-40 s). Only one game runs on the machine at a time: if another f4x session holds the launch lock this call queues (up to lock_timeout_s). The game is always muted and runs in an isolated Proton prefix. Fails if this server already has a running session (call game_quit first). Returns the session directory, the agent endpoint (no token) and the game state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "enum": ["f4x", "f4se"], "default": "f4x", "description": "f4x: our own loader and runtime. f4se: real F4SE 0.6.23 in the f4se-baseline profile, with the agent loaded as an F4SE plugin (for differential runs)." },
                    "headless": { "type": "boolean", "default": true, "description": "Run inside gamescope's headless backend (no window on the user's desktop). Set false to show the game window." },
                    "plugins": { "type": "array", "items": { "type": "string" }, "description": "Absolute Linux paths of test plugin DLLs to load (usually under target/x86_64-pc-windows-msvc/release/)." },
                    "test_mode": { "type": "boolean", "default": false, "description": "Enable test-only agent methods such as debug.crash (F4X_TEST_MODE=1)." },
                    "profile": { "type": "string", "description": "Profile name under ~/.local/share/f4x/profiles. Default: none for f4x (real install, test prefix); f4se-baseline for f4se." },
                    "build": { "type": "boolean", "default": false, "description": "Build the Windows crates (cargo xwin) before launching." },
                    "wait_main_loop": { "type": "boolean", "default": true, "description": "Wait until state.main_loop is true before returning." },
                    "startup_timeout_s": { "type": "integer", "minimum": 10, "default": 120, "description": "Kill the game if the agent has not published its session within this many seconds of the start." },
                    "main_loop_timeout_s": { "type": "integer", "minimum": 1, "default": 120, "description": "How long to wait for the main loop once the agent is reachable." },
                    "lock_timeout_s": { "type": "integer", "minimum": 0, "default": 600, "description": "How long to queue behind another f4x session." },
                    "lifetime_s": { "type": "integer", "minimum": 0, "default": 3600, "description": "Safety net: the session is killed after this many seconds (0 = never)." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "game_status",
            "title": "Game session status",
            "description": "Report the active session: whether it is live, the supervisor phase (waiting-lock, starting, running, stopping, exited) and exit reason, the agent endpoint, the last heartbeat, the live game state from the agent (frames, pumps, main_loop, open menus, loaded plugins, test count) and summaries of any crash reports. Pass `session` (a session directory path or name) to attach this server to a session started elsewhere, e.g. by `f4x launch`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Session directory (absolute path, or a name under target/f4x-sessions) to attach to." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "game_rpc",
            "title": "Call an agent method",
            "description": "Call any JSON-RPC method of the in-game agent on the active session and return its result. Call method `rpc.methods` to list what the agent serves. Common methods: ping, state, console.exec {command}, screenshot, logs.tail {count}, tests.list, tests.run {filter, timeout_s}, input.send, quit. Errors come back with the agent's code and message; if the connection dropped, the result says whether the game is still alive and includes crash summaries.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "method": { "type": "string", "description": "Method name, e.g. \"state\" or \"console.exec\"." },
                    "params": { "type": ["object", "array"], "description": "Method parameters (default {})." },
                    "timeout_s": { "type": "number", "minimum": 0.1, "default": 30, "description": "How long to wait for the reply." }
                },
                "required": ["method"],
                "additionalProperties": false
            }
        },
        {
            "name": "game_screenshot",
            "title": "Screenshot",
            "description": "Capture the next presented frame of the active session and return it as a PNG image, plus its file path in the session directory, size and frame number.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "File name inside the session directory (default screenshot-<n>.png)." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "game_logs",
            "title": "Session logs",
            "description": "Return the last lines of a log of the active session (it works after the game exited or crashed too). Default source: the agent's structured trace (trace.jsonl) if present, else runtime.log. Lines are formatted as `[seconds] LEVEL target: message {fields}`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "count": { "type": "integer", "minimum": 1, "maximum": 5000, "default": 100, "description": "Number of lines." },
                    "level": { "type": "string", "enum": ["trace", "debug", "info", "warn", "error"], "description": "Minimum level (structured trace only)." },
                    "source": { "type": "string", "enum": ["trace", "runtime", "loader", "launch", "supervisor"], "description": "trace: agent events; runtime: runtime.log; loader: loader.log; launch: umu/Proton output; supervisor: the host-side session supervisor." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "game_tests_run",
            "title": "Run in-game tests",
            "description": "Run the in-game tests registered by loaded test plugins on the active session (waits for the main loop first) and return every result with a summary. isError is true unless at least one test ran and all passed. If the game crashes during the run, the result has the crash summaries. Launch with test_mode=true for tests that need test-only agent methods.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": { "type": "string", "description": "Only tests whose name contains this substring." },
                    "skip": { "type": "array", "items": { "type": "string" }, "description": "Leave out tests whose name contains any of these substrings (e.g. [\"expected_\"] for the smoke suite's deliberate failures)." },
                    "timeout_s": { "type": "integer", "minimum": 1, "default": 60, "description": "Per-test timeout in seconds." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "game_quit",
            "title": "Quit the game",
            "description": "Quit the active session's game and wait until every process of the session is gone and the launch lock is free. Asks the agent to quit first; kills the session if it does not exit within 45 s. Use force=true to kill right away (a hung game).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "force": { "type": "boolean", "default": false, "description": "Kill the game instead of asking it to quit." }
                },
                "additionalProperties": false
            }
        }
    ])
}

/// Serve MCP on stdin/stdout until stdin closes.
pub fn serve(root: PathBuf) -> std::io::Result<()> {
    let mut server = Server::new(root);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) if message.is_object() => server.handle(&message),
            Ok(_) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": RpcError::new(code::INVALID_REQUEST, "expected a JSON-RPC object") })),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": RpcError::new(code::PARSE_ERROR, e.to_string()) })),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    server.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use f4x_host::mock::{MockAgent, PNG};

    fn temp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("f4x-cli-test-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(server: &mut Server, id: u64, method: &str, params: Value) -> Value {
        server.handle(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).unwrap()
    }

    fn tool(server: &mut Server, name: &str, arguments: Value) -> Value {
        call(server, 9, "tools/call", json!({ "name": name, "arguments": arguments }))["result"].clone()
    }

    fn tool_json(result: &Value) -> Value {
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn initialize_and_list_tools() {
        let mut server = Server::new(temp("init"));
        let init = call(&mut server, 1, "initialize", json!({
            "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "test", "version": "1" }
        }));
        assert_eq!(init["id"], 1);
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(init["result"]["capabilities"], json!({ "tools": { "listChanged": false } }));
        assert!(server.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());

        let list = call(&mut server, 2, "tools/list", json!({}));
        let tools = list["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["game_launch", "game_status", "game_rpc", "game_screenshot", "game_logs", "game_tests_run", "game_quit"]);
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{}", tool["name"]);
            assert!(tool["description"].as_str().unwrap().len() > 60);
        }
        assert_eq!(call(&mut server, 3, "ping", json!({}))["result"], json!({}));
        assert_eq!(call(&mut server, 4, "resources/list", json!({}))["error"]["code"], code::METHOD_NOT_FOUND);
        assert_eq!(call(&mut server, 5, "tools/call", json!({ "name": "nope" }))["error"]["code"], code::INVALID_PARAMS);
    }

    #[test]
    fn tools_against_a_mock_agent() {
        let root = temp("tools");
        let agent = MockAgent::start(&root.join("target/f4x-sessions/1-mock"));
        let mut server = Server::new(root);

        // No session yet: a clear error, not a protocol error.
        let none = tool(&mut server, "game_rpc", json!({ "method": "ping" }));
        assert_eq!(none["isError"], true);
        assert!(none["content"][0]["text"].as_str().unwrap().contains("game_launch"));

        let status = tool(&mut server, "game_status", json!({ "session": agent.dir.display().to_string() }));
        assert_eq!(status["isError"], false);
        assert_eq!(tool_json(&status)["endpoint"]["port"], agent.port);

        let ping = tool(&mut server, "game_rpc", json!({ "method": "ping" }));
        assert_eq!(ping["isError"], false, "{ping}");
        assert_eq!(tool_json(&ping)["pumps"], 20);

        let missing = tool(&mut server, "game_rpc", json!({ "method": "no.such" }));
        assert_eq!(missing["isError"], true);
        assert_eq!(tool_json(&missing)["error"]["code"], code::METHOD_NOT_FOUND);

        let shot = tool(&mut server, "game_screenshot", json!({}));
        assert_eq!(shot["isError"], false, "{shot}");
        assert_eq!(shot["content"][0]["type"], "image");
        assert_eq!(shot["content"][0]["mimeType"], "image/png");
        let data = base64::engine::general_purpose::STANDARD.decode(shot["content"][0]["data"].as_str().unwrap()).unwrap();
        assert_eq!(data, PNG);

        let tests = tool(&mut server, "game_tests_run", json!({ "filter": "smoke" }));
        assert_eq!(tests["isError"], true, "one mock test fails");
        let report = tool_json(&tests);
        assert_eq!((report["passed"].as_u64(), report["failed"].as_u64()), (Some(2), Some(1)));
        let passing = tool(&mut server, "game_tests_run", json!({ "filter": "main_menu" }));
        assert_eq!(passing["isError"], false, "{passing}");
        let skipped = tool(&mut server, "game_tests_run", json!({ "skip": ["broken"] }));
        assert_eq!(skipped["isError"], false, "{skipped}");
        assert_eq!(tool_json(&skipped)["passed"], 2);

        std::fs::write(agent.dir.join("runtime.log"), "[    0.001] f4x runtime attached\n[    0.002] hooked\n").unwrap();
        let logs = tool(&mut server, "game_logs", json!({ "count": 1, "source": "runtime" }));
        let text = logs["content"][0]["text"].as_str().unwrap();
        assert!(text.ends_with("[    0.002] hooked") && !text.contains("attached"), "{text}");

        assert!(agent.requests().contains(&"tests.run".to_owned()));
    }
}

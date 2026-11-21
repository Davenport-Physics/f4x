//! The f4x control protocol.
//!
//! Transport: newline-delimited JSON-RPC 2.0 over TCP on `127.0.0.1`. The in-game agent
//! listens on an ephemeral port and publishes [`SessionInfo`] as `session.json` in the
//! session directory (`F4X_SESSION_DIR`). A client must call [`method::AUTH`] with the
//! session token before anything else. The agent pushes [`Notification`]s (logs, test
//! results, crashes) on connections that subscribed to them.
//!
//! The server only runs when the launcher sets `F4X_CONTROL=1`; it is a development and
//! test facility, never on by default.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped on breaking changes to any message below.
pub const PROTOCOL_VERSION: u32 = 1;

/// Files the agent writes into the session directory.
pub mod files {
    /// [`super::SessionInfo`], written atomically once the server listens.
    pub const SESSION: &str = "session.json";
    /// Once a second: [`super::Heartbeat`].
    pub const HEARTBEAT: &str = "heartbeat.json";
    /// Every log/trace event, one [`super::LogEvent`] per line.
    pub const TRACE: &str = "trace.jsonl";
    /// Crash reports: `crash-<n>.json` ([`super::CrashReport`]).
    pub const CRASH_PREFIX: &str = "crash-";
    /// Screenshots: `screenshot-<n>.png`.
    pub const SCREENSHOT_PREFIX: &str = "screenshot-";
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Request { jsonrpc: "2.0".into(), id: id.into(), method: method.into(), params }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Response { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }

    pub fn err(id: Value, error: RpcError) -> Self {
        Response { jsonrpc: "2.0".into(), id, result: None, error: Some(error) }
    }
}

/// A server-to-client message without an id.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

impl Notification {
    pub fn new(method: &str, params: Value) -> Self {
        Notification { jsonrpc: "2.0".into(), method: method.into(), params }
    }
}

/// Any line a client can receive.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ServerMessage {
    Response(Response),
    Notification(Notification),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError { code, message: message.into(), data: None }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// Error codes (JSON-RPC reserved range plus f4x-specific ones).
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// Not authenticated, or a wrong token.
    pub const UNAUTHORIZED: i64 = -32001;
    /// The game is not far enough along (e.g. no frame rendered yet).
    pub const NOT_READY: i64 = -32002;
    pub const TIMEOUT: i64 = -32003;
    /// An engine call failed.
    pub const ENGINE: i64 = -32010;
}

/// Method names.
pub mod method {
    /// `{token}` → `{ok}`. Must be first on every connection.
    pub const AUTH: &str = "auth";
    /// → [`super::Ping`].
    pub const PING: &str = "ping";
    /// → [`super::State`].
    pub const STATE: &str = "state";
    /// → `[name]`: every method this agent serves.
    pub const METHODS: &str = "rpc.methods";
    /// `{exit_code?}` → `{ok}`, then the game exits.
    pub const QUIT: &str = "quit";
    /// [`super::ScreenshotParams`] → [`super::Screenshot`]. Waits for the next frame.
    pub const SCREENSHOT: &str = "screenshot";
    /// [`super::SubscribeParams`] → `{ok}`; then `log` notifications.
    pub const LOGS_SUBSCRIBE: &str = "logs.subscribe";
    /// `{count?}` → `[LogEvent]` from the flight recorder (most recent last).
    pub const LOGS_TAIL: &str = "logs.tail";
    /// → `[TestInfo]`.
    pub const TESTS_LIST: &str = "tests.list";
    /// [`super::TestsRunParams`] → [`super::TestsRun`]; `test.result` notifications as
    /// each test finishes.
    pub const TESTS_RUN: &str = "tests.run";
    /// `{kind: "access_violation" | "panic" | "abort"}`: crash on purpose (tests of the
    /// crash pipeline). Only served when the session runs in test mode.
    pub const DEBUG_CRASH: &str = "debug.crash";
}

/// Notification names.
pub mod notification {
    /// [`super::LogEvent`].
    pub const LOG: &str = "log";
    /// [`super::TestResult`].
    pub const TEST_RESULT: &str = "test.result";
    /// [`super::CrashReport`] (best effort: the process may die first).
    pub const CRASH: &str = "crash";
}

/// `session.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub protocol: u32,
    pub pid: u32,
    pub port: u16,
    pub token: String,
    /// `"f4x"` (our runtime) or `"f4se"` (the agent running as a plugin under F4SE).
    pub host: String,
    pub agent_version: String,
    /// Packed game version as text, e.g. `1.10.163.0`.
    pub game_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Heartbeat {
    pub uptime_ms: u64,
    pub frames: u64,
    pub pumps: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Ping {
    pub pid: u32,
    pub uptime_ms: u64,
    /// Frames presented (swap-chain `Present` calls).
    pub frames: u64,
    /// Main-loop iterations seen by the main-thread pump.
    pub pumps: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub host: String,
    pub uptime_ms: u64,
    pub frames: u64,
    pub pumps: u64,
    /// The main loop is running (the pump has fired at least once).
    pub main_loop: bool,
    /// Menus that ticked during the last second, by class name (e.g. `MainMenu`).
    pub menus: Vec<String>,
    pub plugins: Vec<PluginState>,
    pub tests: usize,
    /// Exceptions recorded this session (see crash reports).
    pub exceptions: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PluginState {
    pub name: String,
    pub path: String,
    pub loaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ScreenshotParams {
    /// File name inside the session directory; default `screenshot-<n>.png`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    /// File name inside the session directory.
    pub file: String,
    pub frame: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SubscribeParams {
    /// Minimum level: `trace`, `debug`, `info` (default), `warn`, `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Only events whose target starts with one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LogEvent {
    pub t_ms: f64,
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub fields: Value,
    pub thread: u32,
    /// Plugin (module) that emitted it, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TestInfo {
    pub name: String,
    pub plugin: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TestsRunParams {
    /// Substring filter on the test name; all tests if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Per-test timeout in seconds (default 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Passed,
    Failed,
    TimedOut,
    Panicked,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TestResult {
    pub name: String,
    pub plugin: String,
    pub status: TestStatus,
    pub duration_ms: u64,
    /// Main-loop iterations the test spanned.
    pub pumps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TestsRun {
    pub results: Vec<TestResult>,
    pub passed: usize,
    pub failed: usize,
}

/// A code location: `module+rva`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    pub address: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rva: Option<u64>,
    /// Symbol, when the agent or host could resolve one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CrashReport {
    pub t_ms: f64,
    pub thread: u32,
    pub code: u32,
    pub code_name: String,
    /// `true` once the exception reached the unhandled-exception filter (the process is
    /// going down); `false` for a first-chance record that might still be handled.
    pub fatal: bool,
    pub address: Frame,
    /// For access violations: `read`/`write`/`execute` and the target address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<(String, u64)>,
    pub frames: Vec<Frame>,
    pub registers: Value,
    /// The last events before the crash.
    pub flight_recorder: Vec<LogEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn envelopes_round_trip() {
        let request = Request::new(7, method::PING, Value::Null);
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(text, r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#);
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);

        let ok: ServerMessage = serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"result":{"x":1}}"#).unwrap();
        assert!(matches!(ok, ServerMessage::Response(Response { result: Some(_), error: None, .. })));
        let note: ServerMessage = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"log","params":{}}"#).unwrap();
        assert!(matches!(note, ServerMessage::Notification(_)));
        let err = Response::err(json!(1), RpcError::new(code::UNAUTHORIZED, "auth first"));
        assert_eq!(serde_json::to_value(&err).unwrap()["error"]["code"], code::UNAUTHORIZED);
    }

    #[test]
    fn test_status_is_snake_case() {
        assert_eq!(serde_json::to_value(TestStatus::TimedOut).unwrap(), json!("timed_out"));
    }
}

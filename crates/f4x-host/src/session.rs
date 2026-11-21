//! Session directories (`target/f4x-sessions/<stamp>-<label>`) and what host tools do
//! with them: find the agent's endpoint, wait for it, check liveness, read crash
//! reports and logs, report status and quit.
//!
//! Files in a session directory:
//! - written by the agent: `session.json`, `heartbeat.json`, `trace.jsonl`,
//!   `runtime.log`, `crash-*.json`, `screenshot-*.png` (see `f4x_proto::files`);
//! - written by the loader: `loader.log`;
//! - written by the host: `launch.json` (the launch plan), `launch.log` (umu/Proton
//!   output), `supervisor.json` + `supervisor.log` (see [`crate::supervisor`]) and the
//!   request files `quit-request` / `stop-request`.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use f4x_proto::{PROTOCOL_VERSION, RpcError, files, method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{Client, code};
use crate::launch::{GameGuard, kill_session_processes, pid_alive, session_processes, sessions_dir};
use crate::supervisor::{self, Phase, SupervisorState};
use crate::wine_path;

/// The part of `session.json` a client needs. Fields beyond `port` and `token` are
/// optional so sessions of older agents (the S1 spike runtime) still work.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionEndpoint {
    pub port: u16,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_version: Option<String>,
}

impl SessionEndpoint {
    /// Read and check `<dir>/session.json`.
    pub fn read(dir: &Path) -> Result<SessionEndpoint, RpcError> {
        let path = dir.join(files::SESSION);
        let text = fs::read_to_string(&path)
            .map_err(|e| RpcError::new(code::NO_SESSION, format!("{}: {e}", path.display())))?;
        let endpoint: SessionEndpoint = serde_json::from_str(&text)
            .map_err(|e| RpcError::new(code::NO_SESSION, format!("{}: {e}", path.display())))?;
        if let Some(protocol) = endpoint.protocol
            && protocol != PROTOCOL_VERSION
        {
            return Err(RpcError::new(
                code::NO_SESSION,
                format!("the agent speaks protocol {protocol}; this host speaks {PROTOCOL_VERSION}"),
            ));
        }
        Ok(endpoint)
    }

    /// Everything but the token.
    pub fn public(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(object) = value.as_object_mut() {
            object.remove("token");
        }
        value
    }
}

/// Something that can tell whether the launched game is still there.
pub trait Alive {
    fn alive(&mut self) -> bool;
}

impl Alive for GameGuard {
    fn alive(&mut self) -> bool {
        !self.exited()
    }
}

impl<F: FnMut() -> bool> Alive for F {
    fn alive(&mut self) -> bool {
        self()
    }
}

/// Wait until the agent publishes `session.json` in `dir`. Fails as soon as `alive`
/// says the game is gone, or at `deadline`.
pub fn wait_for_session(dir: &Path, deadline: Instant, alive: &mut dyn Alive) -> Result<SessionEndpoint, String> {
    loop {
        match SessionEndpoint::read(dir) {
            Ok(endpoint) => return Ok(endpoint),
            Err(e) if dir.join(files::SESSION).is_file() && e.message.contains("protocol") => return Err(e.message),
            Err(_) => {}
        }
        if !alive.alive() {
            return Err("the launcher exited before the agent published session.json".into());
        }
        if crate::signals::interrupted() {
            return Err("interrupted".into());
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for session.json".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Wait until the game's main loop runs: `state.main_loop == true`. Agents without a
/// `state` method (the S1 runtime) are asked for `ping` and count as running once
/// their pump counter (`pumps`, or the spike's `pump_calls`) moved.
pub fn wait_main_loop(client: &mut Client, deadline: Instant) -> Result<Value, RpcError> {
    let mut use_ping = false;
    loop {
        let reply =
            if use_ping { client.call(method::PING, json!({})) } else { client.call(method::STATE, json!({})) };
        match reply {
            Ok(state) if !use_ping && state["main_loop"] == true => return Ok(state),
            Ok(ping) if use_ping && pump_count(&ping) > 0 => {
                return Ok(json!({ "main_loop": true, "via": "ping", "ping": ping }));
            }
            Ok(_) => {}
            Err(e) if e.code == code::METHOD_NOT_FOUND && !use_ping => {
                use_ping = true;
                continue;
            }
            Err(e) if e.code == code::NOT_READY => {}
            Err(e) => return Err(e),
        }
        if Instant::now() > deadline {
            return Err(RpcError::new(code::TIMEOUT, "the game's main loop did not start in time"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn pump_count(ping: &Value) -> u64 {
    ping["pumps"].as_u64().or_else(|| ping["pump_calls"].as_u64()).unwrap_or(0)
}

/// A session directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDir {
    pub path: PathBuf,
}

impl SessionDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SessionDir { path: path.into() }
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// The `F4X_SESSION_DIR` value the game's processes carry.
    pub fn marker(&self) -> String {
        wine_path(&self.path)
    }

    pub fn endpoint(&self) -> Result<SessionEndpoint, RpcError> {
        SessionEndpoint::read(&self.path)
    }

    pub fn connect(&self) -> Result<Client, RpcError> {
        Client::connect_session(&self.path)
    }

    pub fn heartbeat(&self) -> Option<Value> {
        read_json(&self.file(files::HEARTBEAT))
    }

    pub fn supervisor(&self) -> Option<SupervisorState> {
        let text = fs::read_to_string(self.file(supervisor::STATE_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The supervisor process is running.
    pub fn supervised(&self) -> bool {
        self.supervisor().is_some_and(|s| s.phase != Phase::Exited && pid_alive(s.pid))
    }

    /// The session's supervisor or any of its game processes is still running.
    pub fn is_live(&self) -> bool {
        self.supervised() || !session_processes(&self.marker()).is_empty()
    }

    /// Ask the supervisor to kill the game now.
    pub fn request_stop(&self) {
        let _ = fs::write(self.file(supervisor::STOP_REQUEST), b"");
    }

    pub fn stop_requested(&self) -> bool {
        self.file(supervisor::STOP_REQUEST).exists()
    }

    /// Crash reports, oldest first: `(path, report)`.
    pub fn crashes(&self) -> Vec<(PathBuf, Value)> {
        let Ok(entries) = fs::read_dir(&self.path) else { return Vec::new() };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.starts_with(files::CRASH_PREFIX) && n.ends_with(".json") && !n.ends_with(".tmp")
                })
            })
            .collect();
        found.sort();
        found.into_iter().map(|p| (p.clone(), read_json(&p).unwrap_or(Value::Null))).collect()
    }

    /// A fatal crash report exists (the exception reached the unhandled filter).
    pub fn fatal_crash(&self) -> bool {
        self.crashes().iter().any(|(_, report)| report["fatal"] == true)
    }

    /// Crash summaries for reports: `[{file, fatal, summary}]`.
    pub fn crash_summaries(&self) -> Vec<Value> {
        self.crashes()
            .iter()
            .map(|(path, report)| {
                json!({
                    "file": path.display().to_string(),
                    "fatal": report["fatal"],
                    "summary": crash_summary(report),
                })
            })
            .collect()
    }

    /// What is known about the session, as JSON. With `probe`, also ask a live agent
    /// for `state` (falling back to `ping`).
    pub fn status(&self, probe: bool) -> Value {
        let live = self.is_live();
        let endpoint = self.endpoint();
        let mut status = json!({
            "session": self.path.display().to_string(),
            "live": live,
            "supervisor": self.supervisor(),
            "endpoint": endpoint.as_ref().map(SessionEndpoint::public).ok(),
            "heartbeat": self.heartbeat(),
            "crashes": self.crash_summaries(),
        });
        if probe && live && endpoint.is_ok() {
            match self.connect() {
                Ok(mut client) => {
                    client.set_timeout(Duration::from_secs(5));
                    match client.call(method::STATE, json!({})) {
                        Ok(state) => status["state"] = state,
                        Err(e) if e.code == code::METHOD_NOT_FOUND => match client.call(method::PING, json!({})) {
                            Ok(ping) => status["ping"] = ping,
                            Err(e) => status["rpc_error"] = json!(e),
                        },
                        Err(e) => status["rpc_error"] = json!(e),
                    }
                }
                Err(e) => status["rpc_error"] = json!(e),
            }
        }
        status
    }

    /// Quit the game and wait until the session has released the launch lock.
    ///
    /// Asks the agent to `quit` (unless `force`), waits up to `wait` for every process
    /// of the session to exit, then asks the supervisor to kill it; a session without a
    /// supervisor has its processes killed directly.
    pub fn quit(&self, force: bool, wait: Duration) -> Value {
        let started = Instant::now();
        let mut report = json!({ "session": self.path.display().to_string() });
        let _ = fs::write(self.file(supervisor::QUIT_REQUEST), b"");
        if force {
            report["rpc"] = "skipped (force)".into();
        } else {
            report["rpc"] = match self.connect() {
                Err(e) => json!({ "error": e }),
                Ok(mut client) => match client.call_timeout(method::QUIT, json!({}), Duration::from_secs(5)) {
                    Ok(_) => "ok".into(),
                    // The agent may terminate before its reply is flushed.
                    Err(e) if e.code == code::TRANSPORT => "ok (connection closed)".into(),
                    Err(e) => json!({ "error": e }),
                },
            };
        }
        let mut forced = force;
        // Without an agent (still starting, or queued on the launch lock) nobody can
        // honour a graceful quit: stop it right away.
        let graceful = if force || self.endpoint().is_err() { Duration::ZERO } else { wait };
        if !self.wait_gone(graceful) {
            forced = true;
            self.request_stop();
            if !self.wait_gone(Duration::from_secs(20)) {
                kill_session_processes(&self.marker());
                self.wait_gone(Duration::from_secs(5));
            }
        }
        report["exited"] = (!self.is_live()).into();
        report["forced"] = forced.into();
        report["seconds"] = round1(started.elapsed().as_secs_f64()).into();
        if let Some(state) = self.supervisor() {
            report["exit"] = json!(state.exit);
        }
        let crashes = self.crash_summaries();
        if !crashes.is_empty() {
            report["crashes"] = crashes.into();
        }
        report
    }

    /// Wait up to `timeout` until nothing of the session runs.
    pub fn wait_gone(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_live() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// The log file for `source`: `trace` (`trace.jsonl`), `runtime`, `loader`,
    /// `launch` or `supervisor`. Without a source: the trace if the agent writes one,
    /// else `runtime.log`.
    pub fn log_path(&self, source: Option<&str>) -> Result<PathBuf, String> {
        let name = match source {
            None => {
                if self.file(files::TRACE).is_file() {
                    files::TRACE
                } else {
                    "runtime.log"
                }
            }
            Some("trace") => files::TRACE,
            Some("runtime") => "runtime.log",
            Some("loader") => "loader.log",
            Some("launch") => "launch.log",
            Some("supervisor") => "supervisor.log",
            Some(other) => return Err(format!("unknown log source {other:?} (trace|runtime|loader|launch|supervisor)")),
        };
        Ok(self.file(name))
    }
}

/// Sessions under `<root>/target/f4x-sessions`, newest first.
pub fn list_sessions(root: &Path) -> Vec<SessionDir> {
    let Ok(entries) = fs::read_dir(sessions_dir(root)) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    // Names start with a fixed-width unix timestamp.
    dirs.sort();
    dirs.into_iter().rev().map(SessionDir::new).collect()
}

/// The newest running session (among the 20 newest), preferring one whose agent has
/// published `session.json` over one still starting or queued on the launch lock.
/// Several agents may launch at once: pass an explicit session when that matters.
pub fn latest_live(root: &Path) -> Option<SessionDir> {
    let live: Vec<SessionDir> = list_sessions(root).into_iter().take(20).filter(SessionDir::is_live).collect();
    live.iter().find(|dir| dir.endpoint().is_ok()).cloned().or_else(|| live.into_iter().next())
}

/// The newest live session, else the newest session.
pub fn latest(root: &Path) -> Option<SessionDir> {
    latest_live(root).or_else(|| list_sessions(root).into_iter().next())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

pub(crate) fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn frame_text(frame: &Value) -> String {
    if let Some(symbol) = frame["symbol"].as_str() {
        return symbol.to_owned();
    }
    match (frame["module"].as_str(), frame["rva"].as_u64()) {
        (Some(module), Some(rva)) => format!("{module}+{rva:#x}"),
        _ => frame["address"].as_u64().map(|a| format!("{a:#x}")).unwrap_or_else(|| "?".into()),
    }
}

/// One paragraph describing a crash report (`f4x_proto::CrashReport`, read leniently).
pub fn crash_summary(report: &Value) -> String {
    let name = report["code_name"].as_str().unwrap_or("exception");
    let code = report["code"].as_u64().map(|c| format!(" ({c:#010x})")).unwrap_or_default();
    let fatal = if report["fatal"] == true { "fatal" } else { "first-chance" };
    let mut text = format!("{name}{code} at {}, {fatal}", frame_text(&report["address"]));
    if let Some(thread) = report["thread"].as_u64() {
        text += &format!(", thread {thread}");
    }
    if let Some(access) = report["access"].as_array()
        && let (Some(kind), Some(target)) = (access.first().and_then(Value::as_str), access.get(1).and_then(Value::as_u64))
    {
        text += &format!("; {kind} of {target:#x}");
    }
    if let Some(frames) = report["frames"].as_array()
        && !frames.is_empty()
    {
        let shown: Vec<String> = frames.iter().take(8).map(frame_text).collect();
        text += &format!("\n  stack: {}", shown.join(" <- "));
        if frames.len() > 8 {
            text += &format!(" <- ... ({} more)", frames.len() - 8);
        }
    }
    if let Some(events) = report["flight_recorder"].as_array()
        && let Some(last) = events.last()
    {
        text += &format!(
            "\n  last event: {} {}: {}",
            last["level"].as_str().unwrap_or("?"),
            last["target"].as_str().unwrap_or("?"),
            last["message"].as_str().unwrap_or("")
        );
    }
    text
}

/// `trace` < `debug` < `info` < `warn` < `error`.
pub fn level_rank(level: &str) -> Option<u8> {
    match level.to_ascii_lowercase().as_str() {
        "trace" => Some(0),
        "debug" => Some(1),
        "info" => Some(2),
        "warn" | "warning" => Some(3),
        "error" => Some(4),
        _ => None,
    }
}

/// Does a log line pass a minimum level? Lines that are not JSON log events (plain
/// text logs) always pass.
pub fn passes_level(line: &str, min: Option<u8>) -> bool {
    let Some(min) = min else { return true };
    match serde_json::from_str::<Value>(line) {
        Ok(event) => event["level"].as_str().and_then(level_rank).is_none_or(|rank| rank >= min),
        Err(_) => true,
    }
}

/// Human form of a log line: JSON log events become `[t] LEVEL target: message`.
pub fn format_log_line(line: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(event) if event.get("message").is_some() => {
            let mut text = format!(
                "[{:>9.3}] {:<5} {}: {}",
                event["t_ms"].as_f64().unwrap_or(0.0) / 1000.0,
                event["level"].as_str().unwrap_or("?").to_ascii_uppercase(),
                event["target"].as_str().unwrap_or("?"),
                event["message"].as_str().unwrap_or("")
            );
            if let Some(fields) = event["fields"].as_object()
                && !fields.is_empty()
            {
                text += &format!(" {}", Value::Object(fields.clone()));
            }
            text
        }
        _ => line.to_owned(),
    }
}

/// The last `n` lines of a file, reading backwards so large logs stay cheap, and the
/// offset they end at (start a [`LogFollower`] there).
pub fn tail_lines(path: &Path, n: usize) -> Result<(Vec<String>, u64), String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut start = len;
    let mut chunk = 64 * 1024u64;
    let mut bytes = Vec::new();
    loop {
        let from = start.saturating_sub(chunk);
        let mut block = vec![0u8; (start - from) as usize];
        file.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
        file.read_exact(&mut block).map_err(|e| e.to_string())?;
        block.extend_from_slice(&bytes);
        bytes = block;
        start = from;
        let newlines = bytes.iter().filter(|b| **b == b'\n').count();
        if start == 0 || newlines > n {
            break;
        }
        chunk *= 2;
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // probably partial
    }
    let skip = lines.len().saturating_sub(n);
    Ok((lines[skip..].iter().map(|l| (*l).to_owned()).collect(), len))
}

/// Follows a growing log file (`tail -f`).
pub struct LogFollower {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl LogFollower {
    /// Start at the current end of the file (or at 0 if it does not exist yet).
    pub fn at_end(path: PathBuf) -> Self {
        let offset = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        LogFollower::at(path, offset)
    }

    /// Start at `offset` (e.g. where [`tail_lines`] stopped).
    pub fn at(path: PathBuf, offset: u64) -> Self {
        LogFollower { path, offset, partial: Vec::new() }
    }

    /// Complete lines appended since the last poll.
    pub fn poll(&mut self) -> Vec<String> {
        let Ok(mut file) = fs::File::open(&self.path) else { return Vec::new() };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0; // truncated or replaced
            self.partial.clear();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut new = Vec::new();
        if file.read_to_end(&mut new).is_err() {
            return Vec::new();
        }
        self.offset += new.len() as u64;
        self.partial.extend_from_slice(&new);
        let mut lines = Vec::new();
        while let Some(at) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=at).collect();
            lines.push(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_owned());
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_reads_spike_and_full_session_files() {
        let dir = crate::test_dir("endpoint");
        fs::write(dir.join("session.json"), r#"{"pid":5,"port":1234,"token":"abc"}"#).unwrap();
        let endpoint = SessionEndpoint::read(&dir).unwrap();
        assert_eq!((endpoint.port, endpoint.token.as_str(), endpoint.pid), (1234, "abc", Some(5)));
        assert!(endpoint.public().get("token").is_none());

        fs::write(dir.join("session.json"), r#"{"protocol":999,"pid":5,"port":1234,"token":"abc","host":"f4x","agent_version":"0","game_version":"1"}"#).unwrap();
        assert!(SessionEndpoint::read(&dir).unwrap_err().message.contains("protocol 999"));
    }

    #[test]
    fn wait_for_session_stops_when_the_game_is_gone() {
        let dir = crate::test_dir("wait-gone");
        let error = wait_for_session(&dir, Instant::now() + Duration::from_secs(5), &mut || false).unwrap_err();
        assert!(error.contains("exited"), "{error}");
        let error = wait_for_session(&dir, Instant::now(), &mut || true).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
    }

    #[test]
    fn wait_for_session_sees_a_late_session_file() {
        let dir = crate::test_dir("wait-late");
        let writer = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                fs::write(dir.join("session.json.tmp"), r#"{"port":9,"token":"t"}"#).unwrap();
                fs::rename(dir.join("session.json.tmp"), dir.join("session.json")).unwrap();
            })
        };
        let endpoint = wait_for_session(&dir, Instant::now() + Duration::from_secs(5), &mut || true).unwrap();
        assert_eq!(endpoint.port, 9);
        writer.join().unwrap();
    }

    #[test]
    fn crash_summaries_name_the_location_and_stack() {
        let report = json!({
            "t_ms": 1.0, "thread": 7, "code": 0xC0000005u32, "code_name": "EXCEPTION_ACCESS_VIOLATION",
            "fatal": true, "address": {"address": 1, "module": "Fallout4.exe", "rva": 0x1234},
            "access": ["write", 0], "frames": [{"address": 2, "symbol": "my_plugin::boom"}],
            "registers": {}, "flight_recorder": [{"t_ms": 0.5, "level": "info", "target": "t", "message": "about to crash", "thread": 7}],
        });
        let text = crash_summary(&report);
        assert!(text.starts_with("EXCEPTION_ACCESS_VIOLATION (0xc0000005) at Fallout4.exe+0x1234, fatal, thread 7; write of 0x0"), "{text}");
        assert!(text.contains("stack: my_plugin::boom") && text.contains("about to crash"), "{text}");
    }

    #[test]
    fn crash_files_and_liveness() {
        let dir = SessionDir::new(crate::test_dir("crash"));
        assert!(!dir.is_live());
        fs::write(dir.file("crash-1.json"), r#"{"fatal":false}"#).unwrap();
        assert!(!dir.fatal_crash());
        fs::write(dir.file("crash-2.json"), r#"{"fatal":true}"#).unwrap();
        assert!(dir.fatal_crash());
        assert_eq!(dir.crash_summaries().len(), 2);
    }

    #[test]
    fn log_tail_follow_and_levels() {
        let dir = crate::test_dir("logs");
        let path = dir.join("trace.jsonl");
        let lines: Vec<String> =
            (0..5000).map(|i| format!(r#"{{"t_ms":{i},"level":"{}","target":"t","message":"m{i}","thread":1}}"#, if i % 2 == 0 { "debug" } else { "warn" })).collect();
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        let (tail, end) = tail_lines(&path, 3).unwrap();
        assert_eq!(end, fs::metadata(&path).unwrap().len());
        assert_eq!(tail, lines[4997..].to_vec());
        assert!(passes_level(&tail[0], level_rank("info")), "4997 is a warning");
        assert!(!passes_level(&tail[1], level_rank("info")), "4998 is debug");
        assert!(passes_level("plain text", level_rank("error")));
        assert!(format_log_line(&tail[2]).contains("WARN  t: m4999"), "{}", format_log_line(&tail[2]));

        let mut follower = LogFollower::at_end(path.clone());
        assert!(follower.poll().is_empty());
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "one\ntw").unwrap();
        assert_eq!(follower.poll(), vec!["one".to_owned()]);
        writeln!(file, "o").unwrap();
        assert_eq!(follower.poll(), vec!["two".to_owned()]);
    }
}

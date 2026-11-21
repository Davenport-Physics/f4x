//! An in-process mock of the in-game agent for tests of host tools: a TCP server on
//! 127.0.0.1 that speaks `f4x-proto` and publishes `session.json` in a directory.
//!
//! Besides the protocol's methods it serves `mock.sleep {ms}` (reply late),
//! `mock.notify {count}` (notifications, then the reply) and `mock.garbage` (an
//! unparseable line).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use f4x_proto::{
    LogEvent, Notification, PROTOCOL_VERSION, Ping, Response, RpcError, Screenshot, SessionInfo, State, TestInfo,
    TestResult, TestStatus, TestsRun, TestsRunParams, code, files, method, notification,
};
use serde_json::{Value, json};

pub const TOKEN: &str = "mock-token";

/// The tests the mock knows: `(name, passes)`.
pub const TESTS: &[(&str, bool)] = &[("smoke::main_menu", true), ("smoke::console", true), ("smoke::broken", false)];

/// A 1x1 PNG.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

struct Shared {
    dir: PathBuf,
    started: Instant,
    state_calls: AtomicU64,
    screenshots: AtomicU64,
    requests: Mutex<Vec<String>>,
    stop: AtomicBool,
}

pub struct MockAgent {
    pub dir: PathBuf,
    pub port: u16,
    shared: Arc<Shared>,
}

impl MockAgent {
    /// Listen and write `<dir>/session.json`.
    pub fn start(dir: &Path) -> MockAgent {
        std::fs::create_dir_all(dir).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = Arc::new(Shared {
            dir: dir.to_owned(),
            started: Instant::now(),
            state_calls: AtomicU64::new(0),
            screenshots: AtomicU64::new(0),
            requests: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let info = SessionInfo {
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
            port,
            token: TOKEN.into(),
            host: "f4x".into(),
            agent_version: "mock".into(),
            game_version: "1.10.163.0".into(),
        };
        std::fs::write(dir.join(files::SESSION), serde_json::to_string(&info).unwrap()).unwrap();
        let accept_shared = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if accept_shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let shared = accept_shared.clone();
                std::thread::spawn(move || {
                    let _ = serve(&shared, stream);
                });
            }
        });
        MockAgent { dir: dir.to_owned(), port, shared }
    }

    /// The methods received so far, in order (all connections).
    pub fn requests(&self) -> Vec<String> {
        self.shared.requests.lock().unwrap().clone()
    }
}

impl Drop for MockAgent {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn send(writer: &mut TcpStream, value: &impl serde::Serialize) -> std::io::Result<()> {
    writeln!(writer, "{}", serde_json::to_string(value).unwrap())?;
    writer.flush()
}

fn notify(writer: &mut TcpStream, name: &str, params: Value) -> std::io::Result<()> {
    send(writer, &Notification::new(name, params))
}

fn serve(shared: &Shared, stream: TcpStream) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    let mut authed = false;
    for line in reader.lines() {
        let line = line?;
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(&mut writer, &Response::err(Value::Null, RpcError::new(code::PARSE_ERROR, e.to_string())))?;
                continue;
            }
        };
        let id = request["id"].clone();
        let name = request["method"].as_str().unwrap_or("").to_owned();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        shared.requests.lock().unwrap().push(name.clone());
        let uptime = shared.started.elapsed().as_millis() as u64;

        let reply: Result<Value, RpcError> = match name.as_str() {
            method::AUTH => {
                if params["token"] == TOKEN {
                    authed = true;
                    Ok(json!({"ok": true}))
                } else {
                    Err(RpcError::new(code::UNAUTHORIZED, "bad token"))
                }
            }
            _ if !authed => Err(RpcError::new(code::UNAUTHORIZED, "auth first")),
            method::PING => {
                Ok(serde_json::to_value(Ping { pid: std::process::id(), uptime_ms: uptime, frames: 10, pumps: 20 }).unwrap())
            }
            method::STATE => {
                // Not in the main loop on the first call.
                let calls = shared.state_calls.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::to_value(State {
                    host: "f4x".into(),
                    uptime_ms: uptime,
                    frames: calls * 10,
                    pumps: calls * 20,
                    main_loop: calls > 0,
                    menus: vec!["MainMenu".into()],
                    plugins: Vec::new(),
                    tests: TESTS.len(),
                    exceptions: 0,
                })
                .unwrap())
            }
            method::METHODS => Ok(json!([
                method::AUTH, method::PING, method::STATE, method::METHODS, method::QUIT, method::SCREENSHOT,
                method::LOGS_SUBSCRIBE, method::LOGS_TAIL, method::TESTS_LIST, method::TESTS_RUN
            ])),
            method::TESTS_LIST => Ok(serde_json::to_value(
                TESTS.iter().map(|(n, _)| TestInfo { name: (*n).into(), plugin: "mock_tests".into() }).collect::<Vec<_>>(),
            )
            .unwrap()),
            method::TESTS_RUN => {
                let run: TestsRunParams = serde_json::from_value(params.clone()).unwrap_or_default();
                let mut results = Vec::new();
                for (test, passes) in TESTS {
                    if run.filter.as_deref().is_some_and(|f| !test.contains(f))
                        || (!run.names.is_empty() && !run.names.iter().any(|n| n == test))
                        || run.skip.iter().any(|s| test.contains(s.as_str()))
                    {
                        continue;
                    }
                    let result = TestResult {
                        name: (*test).into(),
                        plugin: "mock_tests".into(),
                        status: if *passes { TestStatus::Passed } else { TestStatus::Failed },
                        duration_ms: 5,
                        pumps: 3,
                        message: (!passes).then(|| "assertion failed: 1 == 2".to_owned()),
                    };
                    notify(&mut writer, notification::TEST_RESULT, serde_json::to_value(&result).unwrap())?;
                    results.push(result);
                }
                let passed = results.iter().filter(|r| r.status == TestStatus::Passed).count();
                let failed = results.len() - passed;
                Ok(serde_json::to_value(TestsRun { results, passed, failed }).unwrap())
            }
            method::SCREENSHOT => {
                let n = shared.screenshots.fetch_add(1, Ordering::SeqCst) + 1;
                let file = params["name"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{}{n}.png", files::SCREENSHOT_PREFIX));
                std::fs::write(shared.dir.join(&file), PNG)?;
                Ok(serde_json::to_value(Screenshot { width: 1, height: 1, file, frame: n * 10 }).unwrap())
            }
            method::LOGS_SUBSCRIBE => {
                send(&mut writer, &Response::ok(id.clone(), json!({"ok": true})))?;
                for i in 0..2 {
                    let event = LogEvent {
                        t_ms: uptime as f64,
                        level: "info".into(),
                        target: "mock".into(),
                        message: format!("event {i}"),
                        fields: Value::Null,
                        thread: 1,
                        plugin: None,
                    };
                    notify(&mut writer, notification::LOG, serde_json::to_value(event).unwrap())?;
                }
                continue;
            }
            "mock.sleep" => {
                std::thread::sleep(Duration::from_millis(params["ms"].as_u64().unwrap_or(1000)));
                Ok(json!({"slept": true}))
            }
            "mock.notify" => {
                let count = params["count"].as_u64().unwrap_or(1);
                for n in 0..count {
                    notify(&mut writer, "mock.tick", json!({ "n": n }))?;
                }
                Ok(json!({ "sent": count }))
            }
            "mock.garbage" => {
                writeln!(writer, "this is not json")?;
                continue;
            }
            method::QUIT => {
                send(&mut writer, &Response::ok(id, json!({"ok": true})))?;
                return Ok(());
            }
            _ => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("unknown method {name}"))),
        };
        let response = match reply {
            Ok(result) => Response::ok(id, result),
            Err(error) => Response::err(id, error),
        };
        send(&mut writer, &response)?;
    }
    Ok(())
}

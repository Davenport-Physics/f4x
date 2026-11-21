//! Structured logging: every event goes to `trace.jsonl`, a flight-recorder ring buffer
//! (attached to crash reports and served by `logs.tail`), and live subscribers.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use f4x_proto::{LogEvent, Notification, SubscribeParams, notification};
use serde_json::Value;

const RING: usize = 4096;

pub struct Sink {
    ring: Mutex<VecDeque<LogEvent>>,
    file: Mutex<Option<File>>,
    subscribers: Mutex<Vec<Subscriber>>,
}

struct Subscriber {
    min_level: u32,
    targets: Vec<String>,
    tx: Sender<String>,
}

pub fn sink() -> &'static Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| Sink { ring: Mutex::new(VecDeque::with_capacity(RING)), file: Mutex::new(None), subscribers: Mutex::new(Vec::new()) })
}

pub fn level_name(level: u32) -> &'static str {
    match level {
        1 => "error",
        2 => "warn",
        3 => "info",
        4 => "debug",
        _ => "trace",
    }
}

pub fn level_from_name(name: &str) -> u32 {
    match name {
        "error" => 1,
        "warn" => 2,
        "info" => 3,
        "debug" => 4,
        _ => 5,
    }
}

impl Sink {
    /// Write events to `path` (JSON Lines) from now on.
    pub fn open_file(&self, path: &std::path::Path) {
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
            && let Ok(mut slot) = self.file.lock()
        {
            *slot = Some(file);
        }
    }

    pub fn emit(&self, level: u32, target: &str, message: String, fields: Value, plugin: Option<&str>) {
        let event = LogEvent {
            t_ms: crate::clock::t_ms(),
            level: level_name(level).to_owned(),
            target: target.to_owned(),
            message,
            fields,
            thread: crate::clock::thread_id(),
            plugin: plugin.map(str::to_owned),
        };
        let line = serde_json::to_string(&event).unwrap_or_default();
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = writeln!(file, "{line}");
        }
        if let Ok(mut subscribers) = self.subscribers.lock()
            && !subscribers.is_empty()
        {
            let note = serde_json::to_string(&Notification::new(
                notification::LOG,
                serde_json::to_value(&event).unwrap_or_default(),
            ))
            .unwrap_or_default();
            subscribers.retain(|s| {
                if level > s.min_level || (!s.targets.is_empty() && !s.targets.iter().any(|t| target.starts_with(t.as_str()))) {
                    return true;
                }
                s.tx.send(note.clone()).is_ok()
            });
        }
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() == RING {
                ring.pop_front();
            }
            ring.push_back(event);
        }
    }

    /// The most recent `count` events, oldest first.
    pub fn tail(&self, count: usize) -> Vec<LogEvent> {
        let Ok(ring) = self.ring.lock() else { return Vec::new() };
        ring.iter().skip(ring.len().saturating_sub(count)).cloned().collect()
    }

    pub fn subscribe(&self, params: &SubscribeParams, tx: Sender<String>) {
        let min_level = level_from_name(params.level.as_deref().unwrap_or("info"));
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(Subscriber { min_level, targets: params.targets.clone(), tx });
        }
    }

    pub fn flush(&self) {
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = file.flush();
        }
    }
}

/// `log` crate backend: `log::info!` etc. anywhere in the process land in the sink.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        sink().emit(record.level() as u32, record.target(), record.args().to_string(), Value::Null, None);
    }

    fn flush(&self) {
        sink().flush();
    }
}

/// Route the `log` crate into the sink (once per process; later calls are no-ops).
pub fn install_logger() {
    static LOGGER: Logger = Logger;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

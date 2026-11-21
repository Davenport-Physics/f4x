//! Built-in control methods that work on every platform. Windows-only methods
//! (screenshot, debug.crash, catalog) are registered by `attach`.

use std::sync::OnceLock;
use std::time::Duration;

use f4x_proto::{Ping, State, SubscribeParams, TestsRunParams, method};
use serde_json::{Value, json};

use crate::server::{self, params};
use crate::{clock, logsink, tests};

/// Host-specific state providers (set by `attach` on Windows).
pub struct StateProviders {
    pub menus: fn() -> Vec<String>,
    pub exceptions: fn() -> u64,
    pub quit: fn(i32),
}

pub static PROVIDERS: OnceLock<StateProviders> = OnceLock::new();
static HOST: OnceLock<&'static str> = OnceLock::new();

pub fn register_builtin(host: &'static str) {
    let _ = HOST.set(host);
    crate::pump::add_ticker(tests::step);

    server::register(method::PING, |_, _| {
        Ok(serde_json::to_value(Ping {
            pid: std::process::id(),
            uptime_ms: clock::uptime_ms(),
            frames: clock::frames(),
            pumps: clock::pumps(),
        })
        .unwrap_or_default())
    });
    server::register(method::STATE, |_, _| Ok(serde_json::to_value(state()).unwrap_or_default()));
    server::register(method::METHODS, |_, _| Ok(json!(server::method_names())));
    server::register(method::LOGS_TAIL, |_, p| {
        let count = p.get("count").and_then(Value::as_u64).unwrap_or(200) as usize;
        Ok(serde_json::to_value(logsink::sink().tail(count)).unwrap_or_default())
    });
    server::register(method::LOGS_SUBSCRIBE, |conn, p| {
        let p: SubscribeParams = params(p)?;
        logsink::sink().subscribe(&p, conn.notify.clone());
        Ok(json!({"ok": true}))
    });
    server::register(method::TESTS_LIST, |_, _| Ok(serde_json::to_value(tests::list()).unwrap_or_default()));
    server::register(method::TESTS_RUN, |conn, p| {
        let p: TestsRunParams = params(p)?;
        let done = tests::begin(&p, Some(conn.notify.clone()))?;
        // Each test has its own timeout; the whole run is bounded only by the game living.
        let summary = done
            .recv()
            .map_err(|_| f4x_proto::RpcError::new(f4x_proto::code::INTERNAL, "test runner stopped"))?;
        Ok(serde_json::to_value(summary).unwrap_or_default())
    });
    server::register(method::QUIT, |conn, p| {
        let code = p.get("exit_code").and_then(Value::as_i64).unwrap_or(0) as i32;
        log::info!(target: "f4x::server", "quit requested (exit code {code})");
        logsink::sink().flush();
        let quit = PROVIDERS.get().map(|p| p.quit);
        let notify = conn.notify.clone();
        // Reply first, then exit shortly after so the response reaches the client.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(notify);
            match quit {
                Some(quit) => quit(code),
                None => std::process::exit(code),
            }
        });
        Ok(json!({"ok": true}))
    });
}

pub fn state() -> State {
    let providers = PROVIDERS.get();
    State {
        host: HOST.get().copied().unwrap_or("unknown").to_owned(),
        uptime_ms: clock::uptime_ms(),
        frames: clock::frames(),
        pumps: clock::pumps(),
        main_loop: clock::pumps() > 0,
        menus: providers.map(|p| (p.menus)()).unwrap_or_default(),
        plugins: crate::PLUGINS.lock().map(|p| p.clone()).unwrap_or_default(),
        tests: tests::count(),
        exceptions: providers.map(|p| (p.exceptions)()).unwrap_or(0),
    }
}

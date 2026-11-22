//! The main-thread pump.
//!
//! The host calls [`pump`] from a hook on the game's main loop. Engine work must run
//! there: [`on_main`] queues a closure and waits for its result; the test runner steps
//! each running test once per pump.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::time::Duration;

use f4x_proto::{RpcError, code};

use crate::clock;

type Job = Box<dyn FnOnce() + Send>;

static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());

/// Things that run on every pump after the queue (observers, the test runner).
static TICKERS: Mutex<Vec<fn()>> = Mutex::new(Vec::new());

pub fn add_ticker(ticker: fn()) {
    if let Ok(mut tickers) = TICKERS.lock() {
        tickers.push(ticker);
    }
}

/// Set while [`pump`] runs. Engine calls made from a job or a test (e.g. running a
/// console command) can pump window messages themselves and so re-enter the main-loop
/// hook; the nested call must not run jobs/tests again (it would deadlock on their locks).
static IN_PUMP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called by the host on the game's main thread, once per main-loop iteration.
pub fn pump() {
    if IN_PUMP.swap(true, Relaxed) {
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            IN_PUMP.store(false, Relaxed);
        }
    }
    let _reset = Reset;
    let n = clock::PUMPS.fetch_add(1, Relaxed);
    if n == 0 {
        clock::MAIN_THREAD.store(clock::thread_id(), Relaxed);
        log::info!(target: "f4x::pump", "main loop running");
    }
    // Run what is queued now; jobs queued by jobs run next pump.
    let jobs: Vec<Job> = QUEUE.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default();
    for job in jobs {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
            log::error!(target: "f4x::pump", "a main-thread job panicked");
        }
    }
    let tickers: Vec<fn()> = TICKERS.lock().map(|t| t.clone()).unwrap_or_default();
    for ticker in tickers {
        let _ = std::panic::catch_unwind(ticker);
    }
}

/// Queue `job` for the next pump without waiting.
pub fn post(job: impl FnOnce() + Send + 'static) {
    if let Ok(mut queue) = QUEUE.lock() {
        queue.push_back(Box::new(job));
    }
}

/// Run `f` on the main thread and return its result. Runs inline when already on it.
pub fn on_main<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static, timeout: Duration) -> Result<R, RpcError> {
    if clock::on_main_thread() {
        return Ok(f());
    }
    if clock::pumps() == 0 {
        return Err(RpcError::new(code::NOT_READY, "the game's main loop is not running yet"));
    }
    let (tx, rx) = mpsc::channel();
    post(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout)
        .map_err(|_| RpcError::new(code::TIMEOUT, format!("main thread did not run the job within {timeout:?}")))
}

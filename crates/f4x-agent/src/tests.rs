//! The in-game test runner.
//!
//! Plugins register tests through the service table ([`register`]). A run steps the
//! selected tests one after another from the main-thread pump: `start`, then one `poll`
//! per main-loop iteration until the test finishes or times out.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use f4x_abi::service::{Str, TestVtbl, test_status};
use f4x_proto::{Notification, RpcError, TestInfo, TestResult, TestStatus, TestsRun, TestsRunParams, code, notification};

struct Registered {
    plugin: String,
    name: String,
    vtbl: *const TestVtbl,
    user: *mut c_void,
}

// SAFETY: the vtable and user data are only used on the main thread by the runner; the
// plugin guarantees they live for the process.
unsafe impl Send for Registered {}

static REGISTRY: Mutex<Vec<Registered>> = Mutex::new(Vec::new());

pub fn register(plugin: &str, name: &str, vtbl: *const TestVtbl, user: *mut c_void) -> bool {
    let Ok(mut registry) = REGISTRY.lock() else { return false };
    if registry.iter().any(|t| t.plugin == plugin && t.name == name) {
        return false;
    }
    log::debug!(target: "f4x::tests", "registered {plugin}::{name}");
    registry.push(Registered { plugin: plugin.to_owned(), name: name.to_owned(), vtbl, user });
    true
}

pub fn list() -> Vec<TestInfo> {
    REGISTRY
        .lock()
        .map(|r| r.iter().map(|t| TestInfo { name: t.name.clone(), plugin: t.plugin.clone() }).collect())
        .unwrap_or_default()
}

pub fn count() -> usize {
    REGISTRY.lock().map(|r| r.len()).unwrap_or(0)
}

struct Active {
    index: usize,
    instance: *mut c_void,
    started: Instant,
    start_pumps: u64,
}

struct Run {
    queue: VecDeque<usize>,
    current: Option<Active>,
    results: Vec<TestResult>,
    timeout: Duration,
    done: Sender<TestsRun>,
    notify: Option<Sender<String>>,
}

// SAFETY: only touched under the RUN lock, and instances are only used on the main thread.
unsafe impl Send for Run {}

static RUN: Mutex<Option<Run>> = Mutex::new(None);

/// Start a run; the receiver yields the summary when every selected test has finished.
pub fn begin(params: &TestsRunParams, notify: Option<Sender<String>>) -> Result<Receiver<TestsRun>, RpcError> {
    let registry = REGISTRY.lock().map_err(|_| RpcError::new(code::INTERNAL, "registry poisoned"))?;
    let queue: VecDeque<usize> = registry
        .iter()
        .enumerate()
        .filter(|(_, t)| params.filter.as_deref().is_none_or(|f| t.name.contains(f) || format!("{}::{}", t.plugin, t.name).contains(f)))
        .map(|(i, _)| i)
        .collect();
    drop(registry);
    let mut run = RUN.lock().map_err(|_| RpcError::new(code::INTERNAL, "runner poisoned"))?;
    if run.is_some() {
        return Err(RpcError::new(code::INVALID_REQUEST, "a test run is already in progress"));
    }
    let (done, rx) = mpsc::channel();
    *run = Some(Run {
        queue,
        current: None,
        results: Vec::new(),
        timeout: Duration::from_secs(params.timeout_s.unwrap_or(60)),
        done,
        notify,
    });
    Ok(rx)
}

/// Advance the active run by one step. Called from the main-thread pump.
pub fn step() {
    let Ok(mut guard) = RUN.lock() else { return };
    let Some(run) = guard.as_mut() else { return };
    let registry = match REGISTRY.lock() {
        Ok(r) => r,
        Err(_) => return,
    };

    if run.current.is_none() {
        let Some(index) = run.queue.pop_front() else {
            // Finished.
            let passed = run.results.iter().filter(|r| r.status == TestStatus::Passed).count();
            let summary = TestsRun { passed, failed: run.results.len() - passed, results: std::mem::take(&mut run.results) };
            log::info!(target: "f4x::tests", "run finished: {} passed, {} failed", summary.passed, summary.failed);
            let _ = run.done.send(summary);
            drop(registry);
            *guard = None;
            return;
        };
        let test = &registry[index];
        log::info!(target: "f4x::tests", "start {}::{}", test.plugin, test.name);
        // SAFETY: the plugin registered a valid vtable for the process lifetime.
        let instance = unsafe { ((*test.vtbl).start)(test.user) };
        run.current = Some(Active { index, instance, started: Instant::now(), start_pumps: crate::clock::pumps() });
    }

    let active = run.current.as_ref().expect("set above");
    let test = &registry[active.index];
    // SAFETY: as above; `instance` came from this test's `start`.
    let status = unsafe { ((*test.vtbl).poll)(active.instance) };
    let elapsed = active.started.elapsed();
    let outcome = match status {
        test_status::PENDING if elapsed > run.timeout => Some((TestStatus::TimedOut, Some(format!("timed out after {:?}", run.timeout)))),
        test_status::PENDING => None,
        other => {
            let mut message = Str::EMPTY;
            // SAFETY: as above.
            unsafe { ((*test.vtbl).message)(active.instance, &mut message) };
            // SAFETY: valid until drop, which happens below.
            let text = unsafe { message.as_str() }.to_owned();
            let status = match other {
                test_status::PASSED => TestStatus::Passed,
                test_status::FAILED => TestStatus::Failed,
                _ => TestStatus::Panicked,
            };
            Some((status, (!text.is_empty()).then_some(text)))
        }
    };
    if let Some((status, message)) = outcome {
        // SAFETY: drop exactly once.
        unsafe { ((*test.vtbl).drop)(active.instance) };
        let result = TestResult {
            name: test.name.clone(),
            plugin: test.plugin.clone(),
            status,
            duration_ms: elapsed.as_millis() as u64,
            pumps: crate::clock::pumps() - active.start_pumps,
            message,
        };
        log::info!(target: "f4x::tests", "{:?} {}::{} in {} ms", result.status, result.plugin, result.name, result.duration_ms);
        if let Some(notify) = &run.notify {
            let note = Notification::new(notification::TEST_RESULT, serde_json::to_value(&result).unwrap_or_default());
            let _ = notify.send(serde_json::to_string(&note).unwrap_or_default());
        }
        run.results.push(result);
        run.current = None;
    }
}

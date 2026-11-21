//! `f4x test` and the MCP `game_tests_run` tool: run the in-game tests of a session.

use std::time::{Duration, Instant};

use f4x_host::Client;
use f4x_host::client::code;
use f4x_host::contract::LaunchOptions;
use f4x_host::junit;
use f4x_host::session::{SessionDir, wait_main_loop};
use f4x_host::supervisor::ExitInfo;
use f4x_proto::{TestResult, TestStatus, TestsRun, TestsRunParams, method, notification};
use serde_json::{Value, json};

use crate::start;

/// Per-test timeout when none is given (the agent's default too).
pub const DEFAULT_TEST_TIMEOUT_S: u64 = 60;

/// What came out of `tests.run` on one session.
#[derive(Debug, Default)]
pub struct Outcome {
    pub results: Vec<TestResult>,
    /// Why the run did not complete (transport error, timeout...).
    pub error: Option<String>,
    /// The connection dropped mid-run: the game probably died.
    pub lost_connection: bool,
}

impl Outcome {
    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| r.status != TestStatus::Passed).count()
    }
}

pub fn status_word(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Passed => "PASS",
        TestStatus::Failed => "FAIL",
        TestStatus::TimedOut => "TIME",
        TestStatus::Panicked => "PANIC",
    }
}

pub fn result_line(result: &TestResult) -> String {
    let mut line = format!("{:<5} {} ({} ms)", status_word(result.status), result.name, result.duration_ms);
    if let Some(message) = &result.message
        && result.status != TestStatus::Passed
    {
        line += &format!(": {message}");
    }
    line
}

/// Which tests to run: names containing `filter` (all if absent) and none of `skip`.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub filter: Option<String>,
    pub skip: Vec<String>,
}

impl Selection {
    pub fn wants(&self, name: &str) -> bool {
        self.filter.as_deref().is_none_or(|f| name.contains(f)) && !self.skip.iter().any(|s| name.contains(s.as_str()))
    }
}

/// Wait for the main loop, then `tests.run`, handing each `test.result` to `on_result`
/// as it arrives.
///
/// `tests.run` takes one substring filter. With `skip` patterns the selection is made
/// here from `tests.list`, and each selected test runs through its own `tests.run`
/// (results for other tests whose names contain it are ignored).
pub fn run_tests(
    client: &mut Client,
    selection: &Selection,
    timeout_s: u64,
    main_loop_timeout: Duration,
    mut on_result: impl FnMut(&TestResult),
) -> Outcome {
    let mut outcome = Outcome::default();
    if let Err(e) = wait_main_loop(client, Instant::now() + main_loop_timeout) {
        outcome.error = Some(format!("waiting for the main loop: {e}"));
        return outcome;
    }
    let listed: Option<Vec<String>> = client.call(method::TESTS_LIST, json!({})).ok().and_then(|list| {
        list.as_array().map(|tests| tests.iter().filter_map(|t| t["name"].as_str().map(str::to_owned)).collect())
    });
    // Each call may take every test it covers up to its timeout.
    let budget = |covered: usize| Duration::from_secs(timeout_s.saturating_mul(covered.max(1) as u64) + 60);

    // The agent applies filter and skip itself (f4x-proto `TestsRunParams`).
    let covered = listed.as_ref().map(|l| l.iter().filter(|n| selection.wants(n)).count()).unwrap_or(30);
    let batches: Vec<(Option<String>, Option<String>, usize)> = vec![(selection.filter.clone(), None, covered)];

    for (filter, exact, covered) in batches {
        let keep = |r: &TestResult| exact.as_deref().is_none_or(|e| r.name == e);
        let params = serde_json::to_value(TestsRunParams {
            filter,
            names: Vec::new(),
            skip: selection.skip.clone(),
            timeout_s: Some(timeout_s),
        })
        .unwrap_or(Value::Null);
        let mut streamed: Vec<TestResult> = Vec::new();
        let reply = client.call_with(method::TESTS_RUN, params, budget(covered), |n| {
            if n.method == notification::TEST_RESULT
                && let Ok(result) = serde_json::from_value::<TestResult>(n.params.clone())
                && keep(&result)
            {
                on_result(&result);
                streamed.push(result);
            }
        });
        match reply {
            Ok(value) => match serde_json::from_value::<TestsRun>(value) {
                Ok(run) => {
                    for result in run.results.into_iter().filter(|r| keep(r)) {
                        // Results only in the reply (no notification) still get reported.
                        if !streamed.iter().any(|s| s.name == result.name && s.plugin == result.plugin) {
                            on_result(&result);
                        }
                        outcome.results.push(result);
                    }
                }
                Err(e) => {
                    outcome.results.extend(streamed);
                    outcome.error = Some(format!("tests.run: unexpected result: {e}"));
                    return outcome;
                }
            },
            Err(e) => {
                outcome.results.extend(streamed);
                outcome.lost_connection = e.code == code::TRANSPORT;
                outcome.error = Some(if e.code == code::TRANSPORT {
                    format!("the connection to the game was lost during tests.run ({})", e.message)
                } else {
                    format!("tests.run: {e}")
                });
                return outcome;
            }
        }
    }
    outcome
}

/// One `f4x test` run (one game session).
#[derive(Debug)]
pub struct Run {
    pub index: u32,
    pub session: Option<String>,
    pub outcome: Outcome,
    /// Every crash report of the session (first-chance ones too).
    pub crashes: Vec<Value>,
    /// The game crashed: a fatal report, or it died before `quit` with any report.
    pub crashed: bool,
    pub exit: Option<ExitInfo>,
    pub seconds: f64,
}

impl Run {
    pub fn ok(&self) -> bool {
        self.outcome.error.is_none() && !self.crashed && !self.outcome.results.is_empty() && self.outcome.failed() == 0
    }

    /// Why the run failed outside individual tests.
    pub fn problem(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(error) = &self.outcome.error {
            parts.push(error.clone());
        }
        if self.crashed {
            for crash in &self.crashes {
                parts.push(format!("crash: {}", crash["summary"].as_str().unwrap_or("?")));
            }
        }
        if parts.is_empty() && self.outcome.results.is_empty() {
            parts.push("no tests ran (check --filter and that the test plugins loaded)".into());
        }
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    pub fn to_json(&self) -> Value {
        json!({
            "run": self.index,
            "ok": self.ok(),
            "session": self.session,
            "passed": self.outcome.results.len() - self.outcome.failed(),
            "failed": self.outcome.failed(),
            "results": self.outcome.results,
            "error": self.problem(),
            "crashed": self.crashed,
            "crashes": self.crashes,
            "exit": self.exit,
            "seconds": self.seconds,
        })
    }
}

pub struct Plan {
    pub launch: LaunchOptions,
    pub lock_timeout: Option<Duration>,
    pub selection: Selection,
    pub timeout_s: u64,
    pub main_loop_timeout: Duration,
    pub runs: u32,
}

/// Launch, test and quit `plan.runs` times. `progress` gets a line per event.
pub fn run_plan(plan: &Plan, mut progress: impl FnMut(&str)) -> Vec<Run> {
    let mut runs = Vec::new();
    for index in 1..=plan.runs {
        let started = Instant::now();
        let mut launch = plan.launch.clone();
        launch.test_mode = true;
        launch.label = format!("test-{}-{index}", launch.host);
        progress(&format!("run {index}/{}: launching ({} host)", plan.runs, launch.host));
        let run = match start::start(&launch, plan.lock_timeout) {
            Err(e) => {
                let dir = e.session.clone().map(SessionDir::new);
                let crashes = dir.as_ref().map(SessionDir::crash_summaries).unwrap_or_default();
                Run {
                    index,
                    session: e.session.as_ref().map(|p| p.display().to_string()),
                    outcome: Outcome {
                        error: Some(format!("launch failed: {}", e.message)),
                        ..Outcome::default()
                    },
                    crashed: !crashes.is_empty(),
                    crashes,
                    exit: dir.and_then(|d| d.supervisor()).and_then(|s| s.exit),
                    seconds: 0.0,
                }
            }
            Ok(mut started_session) => {
                let dir = started_session.dir.clone();
                let outcome = match Client::connect_endpoint(&started_session.endpoint) {
                    Ok(mut client) => {
                        run_tests(&mut client, &plan.selection, plan.timeout_s, plan.main_loop_timeout, |r| {
                            progress(&format!("  {}", result_line(r)));
                        })
                    }
                    Err(e) => Outcome { error: Some(format!("connect: {e}")), ..Outcome::default() },
                };
                if outcome.lost_connection {
                    // Give a dying game a moment to finish its crash report and exit.
                    dir.wait_gone(Duration::from_secs(15));
                }
                let died = outcome.lost_connection || !dir.is_live();
                // After Ctrl-C the agent cannot be asked politely (calls return at once).
                let quit = dir.quit(f4x_host::signals::interrupted(), Duration::from_secs(30));
                let _ = started_session.supervisor.try_wait();
                if quit["exited"] != true {
                    progress(&format!("warning: session {} did not exit cleanly: {quit}", dir.path.display()));
                }
                Run {
                    index,
                    session: Some(dir.path.display().to_string()),
                    outcome,
                    crashed: dir.fatal_crash() || (died && !dir.crashes().is_empty()),
                    crashes: dir.crash_summaries(),
                    exit: dir.supervisor().and_then(|s| s.exit),
                    seconds: 0.0,
                }
            }
        };
        let run = Run { seconds: (started.elapsed().as_secs_f64() * 10.0).round() / 10.0, ..run };
        let failed = run.outcome.failed();
        let passed = run.outcome.results.len() - failed;
        progress(&format!(
            "run {index}/{}: {} — {passed} passed, {failed} failed ({:.1} s)",
            plan.runs,
            if run.ok() { "ok" } else { "FAILED" },
            run.seconds
        ));
        if let Some(problem) = run.problem() {
            for line in problem.lines() {
                progress(&format!("  {line}"));
            }
        }
        runs.push(run);
        if f4x_host::signals::interrupted() {
            progress("interrupted; remaining runs skipped");
            break;
        }
    }
    runs
}

pub fn junit_suites(runs: &[Run], host: &str) -> Vec<junit::Suite> {
    runs.iter()
        .map(|run| {
            let mut properties = vec![("host".to_owned(), host.to_owned())];
            if let Some(session) = &run.session {
                properties.push(("session".into(), session.clone()));
            }
            junit::Suite {
                name: format!("f4x {host} run {}", run.index),
                timestamp: None,
                results: run.outcome.results.clone(),
                error: run.problem(),
                seconds: run.seconds,
                properties,
            }
        })
        .collect()
}

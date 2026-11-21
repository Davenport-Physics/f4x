//! A blocking client for the f4x control protocol (`f4x-proto`): newline-delimited
//! JSON-RPC 2.0 over TCP on 127.0.0.1.
//!
//! A reader thread parses every line the agent sends. Responses are matched to the
//! pending call by id; notifications are handed to the callback of the call in
//! progress ([`Client::call_with`]) or queued for [`Client::next_notification`].
//! Transport failures and timeouts are reported as [`RpcError`]s with the codes in
//! [`code`] and `f4x_proto::code::TIMEOUT`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use f4x_proto::{Notification, Request, Response, RpcError, ServerMessage, method};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::session::SessionEndpoint;

/// Host-side error codes (JSON-RPC's implementation-defined range).
pub mod code {
    /// The connection failed, closed (the game exited?) or sent something unparseable.
    pub const TRANSPORT: i64 = f4x_proto::code::TRANSPORT;
    /// The user interrupted the call (Ctrl-C, SIGTERM).
    pub const INTERRUPTED: i64 = f4x_proto::code::INTERRUPTED;
    /// `session.json` is missing, unreadable or for an incompatible protocol.
    pub const NO_SESSION: i64 = f4x_proto::code::NO_SESSION;
    pub use f4x_proto::code::*;
}

/// Default per-call timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Notifications kept for [`Client::next_notification`] before the oldest are dropped.
const BACKLOG_LIMIT: usize = 10_000;

enum Incoming {
    Response(Response),
    Notification(Notification),
    Closed(String),
}

pub struct Client {
    writer: TcpStream,
    incoming: Receiver<Incoming>,
    backlog: VecDeque<Notification>,
    next_id: u64,
    timeout: Duration,
    closed: Option<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

pub fn transport(message: impl Into<String>) -> RpcError {
    RpcError::new(code::TRANSPORT, message)
}

impl Client {
    /// Connect without authenticating.
    pub fn connect(addr: SocketAddr, connect_timeout: Duration) -> Result<Client, RpcError> {
        let stream = TcpStream::connect_timeout(&addr, connect_timeout)
            .map_err(|e| transport(format!("connecting to {addr}: {e}")))?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
        let read = stream.try_clone().map_err(|e| transport(e.to_string()))?;
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("f4x-client-reader".into())
            .spawn(move || read_loop(read, tx))
            .map_err(|e| transport(e.to_string()))?;
        Ok(Client {
            writer: stream,
            incoming: rx,
            backlog: VecDeque::new(),
            next_id: 1,
            timeout: DEFAULT_TIMEOUT,
            closed: None,
            reader: Some(reader),
        })
    }

    /// Connect to the agent of a session directory and authenticate with its token.
    pub fn connect_session(dir: &Path) -> Result<Client, RpcError> {
        let endpoint = SessionEndpoint::read(dir)?;
        Client::connect_endpoint(&endpoint)
    }

    pub fn connect_endpoint(endpoint: &SessionEndpoint) -> Result<Client, RpcError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], endpoint.port));
        let mut client = Client::connect(addr, Duration::from_secs(5))?;
        client.auth(&endpoint.token)?;
        Ok(client)
    }

    pub fn auth(&mut self, token: &str) -> Result<(), RpcError> {
        self.call_timeout(method::AUTH, json!({ "token": token }), Duration::from_secs(10)).map(drop)
    }

    /// The timeout used by [`Client::call`].
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The connection has closed (the reason), if known.
    pub fn closed(&self) -> Option<&str> {
        self.closed.as_deref()
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call_timeout(method, params, self.timeout)
    }

    pub fn call_timeout(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let mut backlog = std::mem::take(&mut self.backlog);
        let result = self.call_with(method, params, timeout, |n| {
            if backlog.len() == BACKLOG_LIMIT {
                backlog.pop_front();
            }
            backlog.push_back(n.clone());
        });
        self.backlog = backlog;
        result
    }

    /// Typed call: serialise `params`, deserialise the result.
    pub fn call_typed<P: Serialize, R: DeserializeOwned>(&mut self, method: &str, params: &P) -> Result<R, RpcError> {
        let params = serde_json::to_value(params).map_err(|e| RpcError::new(code::INVALID_PARAMS, e.to_string()))?;
        let value = self.call(method, params)?;
        serde_json::from_value(value).map_err(|e| transport(format!("{method}: unexpected result: {e}")))
    }

    /// Call `method` and wait up to `timeout` for its response, passing every
    /// notification that arrives meanwhile to `on_notification` (on this thread, in
    /// order). Notifications already queued are delivered first.
    pub fn call_with(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        mut on_notification: impl FnMut(&Notification),
    ) -> Result<Value, RpcError> {
        while let Some(n) = self.backlog.pop_front() {
            on_notification(&n);
        }
        if let Some(reason) = &self.closed {
            return Err(transport(format!("{method}: connection closed: {reason}")));
        }
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&Request::new(id, method, params)).map_err(|e| transport(e.to_string()))?;
        if let Err(e) = writeln!(self.writer, "{line}").and_then(|()| self.writer.flush()) {
            self.closed = Some(e.to_string());
            return Err(transport(format!("{method}: sending: {e}")));
        }

        let deadline = Instant::now() + timeout;
        loop {
            if crate::signals::interrupted() {
                return Err(RpcError::new(code::INTERRUPTED, format!("{method}: interrupted")));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RpcError::new(
                    code::TIMEOUT,
                    format!("{method}: no response within {:.1} s", timeout.as_secs_f64()),
                ));
            }
            match self.incoming.recv_timeout((deadline - now).min(Duration::from_millis(200))) {
                Ok(Incoming::Notification(n)) => on_notification(&n),
                Ok(Incoming::Response(response)) => {
                    // An error the agent could not attach to a request (id null) belongs
                    // to the only call in flight. Responses to calls that already timed
                    // out are dropped.
                    let ours = response.id == id || (response.id.is_null() && response.error.is_some());
                    if !ours {
                        continue;
                    }
                    return match response.error {
                        Some(error) => Err(error),
                        None => Ok(response.result.unwrap_or(Value::Null)),
                    };
                }
                Ok(Incoming::Closed(reason)) => {
                    self.closed = Some(reason.clone());
                    return Err(transport(format!("{method}: connection closed: {reason}")));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let reason = self.closed.get_or_insert_with(|| "reader stopped".into()).clone();
                    return Err(transport(format!("{method}: connection closed: {reason}")));
                }
            }
        }
    }

    /// The next notification, waiting up to `timeout`. `Ok(None)` on timeout; an error
    /// once the connection has closed and nothing is left.
    pub fn next_notification(&mut self, timeout: Duration) -> Result<Option<Notification>, RpcError> {
        if let Some(n) = self.backlog.pop_front() {
            return Ok(Some(n));
        }
        if let Some(reason) = &self.closed {
            return Err(transport(format!("connection closed: {reason}")));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if crate::signals::interrupted() {
                return Err(RpcError::new(code::INTERRUPTED, "interrupted"));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            match self.incoming.recv_timeout((deadline - now).min(Duration::from_millis(200))) {
                Ok(Incoming::Notification(n)) => return Ok(Some(n)),
                Ok(Incoming::Response(_)) => {} // a late reply to a timed-out call
                Ok(Incoming::Closed(reason)) => {
                    self.closed = Some(reason.clone());
                    return Err(transport(format!("connection closed: {reason}")));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(transport("connection closed"));
                }
            }
        }
    }

    /// Every notification received so far that nobody consumed.
    pub fn drain_notifications(&mut self) -> Vec<Notification> {
        while let Ok(message) = self.incoming.try_recv() {
            match message {
                Incoming::Notification(n) => self.backlog.push_back(n),
                Incoming::Response(_) => {}
                Incoming::Closed(reason) => self.closed = Some(reason),
            }
        }
        self.backlog.drain(..).collect()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.writer.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_loop(stream: TcpStream, tx: mpsc::Sender<Incoming>) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let reason = loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break "the agent closed the connection".to_owned(),
            Ok(_) => {}
            Err(e) => break e.to_string(),
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let message = match serde_json::from_str::<ServerMessage>(text) {
            Ok(ServerMessage::Response(r)) => Incoming::Response(r),
            Ok(ServerMessage::Notification(n)) => Incoming::Notification(n),
            // Not a protocol message: surface it as an error for the call in flight.
            Err(e) => Incoming::Response(Response::err(
                Value::Null,
                transport(format!("unparseable message from the agent ({e}): {}", truncate(text, 200))),
            )),
        };
        if tx.send(message).is_err() {
            return;
        }
    };
    let _ = tx.send(Incoming::Closed(reason));
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockAgent;
    use f4x_proto::{TestStatus, TestsRun, TestsRunParams, notification};

    fn session() -> (MockAgent, Client) {
        let agent = MockAgent::start(&crate::test_dir("client"));
        let client = Client::connect_session(&agent.dir).unwrap();
        (agent, client)
    }

    #[test]
    fn authenticates_and_calls() {
        let (agent, mut client) = session();
        let ping = client.call(method::PING, json!({})).unwrap();
        assert_eq!(ping["pumps"], 20);
        let methods: Vec<String> = client.call_typed(method::METHODS, &json!({})).unwrap();
        assert!(methods.iter().any(|m| m == method::TESTS_RUN));
        assert_eq!(agent.requests()[..2], [method::AUTH.to_owned(), method::PING.to_owned()]);
    }

    #[test]
    fn rejects_bad_tokens_and_unauthenticated_calls() {
        let agent = MockAgent::start(&crate::test_dir("client-auth"));
        let addr = SocketAddr::from(([127, 0, 0, 1], agent.port));
        let mut client = Client::connect(addr, Duration::from_secs(2)).unwrap();
        assert_eq!(client.call(method::PING, json!({})).unwrap_err().code, code::UNAUTHORIZED);
        assert_eq!(client.auth("wrong").unwrap_err().code, code::UNAUTHORIZED);
        client.auth(crate::mock::TOKEN).unwrap();
        assert!(client.call(method::PING, json!({})).is_ok());
    }

    #[test]
    fn reports_server_errors() {
        let (_agent, mut client) = session();
        let error = client.call("no.such.method", json!({})).unwrap_err();
        assert_eq!(error.code, code::METHOD_NOT_FOUND);
        let error = client.call("mock.garbage", json!({})).unwrap_err();
        assert_eq!(error.code, code::TRANSPORT);
        assert!(error.message.contains("unparseable"), "{error}");
    }

    #[test]
    fn streams_notifications_during_a_call() {
        let (_agent, mut client) = session();
        let mut seen = Vec::new();
        let params = serde_json::to_value(TestsRunParams { filter: Some("smoke".into()), timeout_s: Some(5), ..Default::default() }).unwrap();
        let run = client
            .call_with(method::TESTS_RUN, params, Duration::from_secs(5), |n| {
                assert_eq!(n.method, notification::TEST_RESULT);
                seen.push(n.params["name"].as_str().unwrap().to_owned());
            })
            .unwrap();
        let run: TestsRun = serde_json::from_value(run).unwrap();
        assert_eq!(seen, ["smoke::main_menu", "smoke::console", "smoke::broken"]);
        assert_eq!((run.passed, run.failed), (2, 1));
        assert_eq!(run.results[2].status, TestStatus::Failed);
    }

    #[test]
    fn queues_notifications_outside_calls() {
        let (_agent, mut client) = session();
        assert_eq!(client.call("mock.notify", json!({"count": 3})).unwrap()["sent"], 3);
        for n in 0..3 {
            let note = client.next_notification(Duration::from_secs(1)).unwrap().unwrap();
            assert_eq!((note.method.as_str(), note.params["n"].as_u64()), ("mock.tick", Some(n)));
        }
        assert!(client.next_notification(Duration::from_millis(50)).unwrap().is_none());

        client.call(method::LOGS_SUBSCRIBE, json!({"level": "info"})).unwrap();
        let first = client.next_notification(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!((first.method.as_str(), first.params["message"].as_str()), ("log", Some("event 0")));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(client.drain_notifications().len(), 1);
    }

    #[test]
    fn times_out_and_recovers() {
        let (_agent, mut client) = session();
        let error = client.call_timeout("mock.sleep", json!({"ms": 600}), Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.code, code::TIMEOUT);
        // The late reply to the timed-out call must not be taken for this one.
        let ping = client.call(method::PING, json!({})).unwrap();
        assert_eq!(ping["frames"], 10);
    }

    #[test]
    fn notices_a_closed_connection() {
        let (_agent, mut client) = session();
        client.call(method::QUIT, json!({})).unwrap();
        let error = client.call(method::PING, json!({})).unwrap_err();
        assert_eq!(error.code, code::TRANSPORT);
        assert!(client.closed().is_some());
        assert!(client.next_notification(Duration::from_millis(10)).is_err());
    }

    #[test]
    fn waits_for_the_main_loop() {
        let (_agent, mut client) = session();
        let state = crate::session::wait_main_loop(&mut client, Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(state["main_loop"], true);
        assert_eq!(state["menus"][0], "MainMenu");
    }

    #[test]
    fn missing_session_files_are_errors() {
        let error = Client::connect_session(&crate::test_dir("client-none")).err().unwrap();
        assert_eq!(error.code, code::NO_SESSION);
    }
}

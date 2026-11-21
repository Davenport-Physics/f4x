//! The JSON-RPC control server (see `f4x-proto`).
//!
//! One reader thread per connection runs the handlers (a handler may block, e.g. while
//! a test run executes on the main thread); a writer thread per connection sends
//! responses and pushed notifications, so streaming works while a handler waits.

use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, RwLock};

use f4x_proto::{Request, Response, RpcError, SessionInfo, code, method};
use serde_json::{Value, json};

/// Per-connection context handed to handlers.
pub struct Conn {
    /// Push a serialized notification line to this client.
    pub notify: Sender<String>,
}

pub type Handler = Arc<dyn Fn(&Conn, Value) -> Result<Value, RpcError> + Send + Sync>;

fn methods() -> &'static RwLock<BTreeMap<String, Handler>> {
    static METHODS: std::sync::OnceLock<RwLock<BTreeMap<String, Handler>>> = std::sync::OnceLock::new();
    METHODS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Serve `name` with `handler` (replacing any previous handler).
pub fn register(name: &str, handler: impl Fn(&Conn, Value) -> Result<Value, RpcError> + Send + Sync + 'static) {
    if let Ok(mut methods) = methods().write() {
        methods.insert(name.to_owned(), Arc::new(handler));
    }
}

pub fn method_names() -> Vec<String> {
    methods().read().map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

/// Bind on 127.0.0.1 (ephemeral port), publish `session.json`, and serve forever on a
/// background thread. Returns the port.
pub fn start(host: &str, game_version: &str) -> Result<u16, String> {
    let session = crate::session::get().ok_or("no session directory (F4X_SESSION_DIR)")?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("bind: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let state = RandomState::new();
    let token = format!("{:016x}{:016x}", state.hash_one(1u8), state.hash_one(2u8));
    let info = SessionInfo {
        protocol: f4x_proto::PROTOCOL_VERSION,
        pid: std::process::id(),
        port,
        token: token.clone(),
        host: host.to_owned(),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        game_version: game_version.to_owned(),
    };
    session
        .write_atomic(f4x_proto::files::SESSION, serde_json::to_string(&info).unwrap_or_default().as_bytes())
        .map_err(|e| format!("writing session.json: {e}"))?;
    log::info!(target: "f4x::server", "listening on 127.0.0.1:{port}");
    std::thread::Builder::new()
        .name("f4x-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let token = token.clone();
                let _ = std::thread::Builder::new().name("f4x-client".into()).spawn(move || {
                    if let Err(error) = serve(stream, &token) {
                        log::debug!(target: "f4x::server", "client ended: {error}");
                    }
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(port)
}

/// Serve one connection (also used by tests over a loopback socket).
pub fn serve(stream: TcpStream, token: &str) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let (tx, rx) = mpsc::channel::<String>();
    let writer_thread = std::thread::spawn(move || {
        for line in rx {
            if writeln!(writer, "{line}").and_then(|()| writer.flush()).is_err() {
                break;
            }
        }
    });
    let conn = Conn { notify: tx.clone() };
    let mut authed = false;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Err(error) => Response::err(Value::Null, RpcError::new(code::PARSE_ERROR, error.to_string())),
            Ok(request) => {
                let id = request.id.clone();
                match handle(&conn, &mut authed, token, request) {
                    Ok(result) => Response::ok(id, result),
                    Err(error) => Response::err(id, error),
                }
            }
        };
        if tx.send(serde_json::to_string(&response).unwrap_or_default()).is_err() {
            break;
        }
    }
    drop(conn);
    drop(tx);
    let _ = writer_thread.join();
    Ok(())
}

fn handle(conn: &Conn, authed: &mut bool, token: &str, request: Request) -> Result<Value, RpcError> {
    if request.method == method::AUTH {
        return if request.params.get("token").and_then(Value::as_str) == Some(token) {
            *authed = true;
            Ok(json!({"ok": true}))
        } else {
            Err(RpcError::new(code::UNAUTHORIZED, "bad token"))
        };
    }
    if !*authed {
        return Err(RpcError::new(code::UNAUTHORIZED, "call auth first"));
    }
    let handler = methods().read().ok().and_then(|m| m.get(&request.method).cloned());
    let Some(handler) = handler else {
        return Err(RpcError::new(code::METHOD_NOT_FOUND, format!("unknown method {:?}", request.method)));
    };
    let name = request.method.clone();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(conn, request.params))) {
        Ok(result) => result,
        Err(_) => Err(RpcError::new(code::INTERNAL, format!("handler for {name} panicked"))),
    }
}

/// Parse `params` into `T` (missing params = default).
pub fn params<T: serde::de::DeserializeOwned + Default>(params: Value) -> Result<T, RpcError> {
    if params.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(params).map_err(|e| RpcError::new(code::INVALID_PARAMS, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn auth_then_call() {
        register("test.echo", |_, params| Ok(params));
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve(stream, "secret").unwrap();
        });
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut call = |line: &str| {
            writeln!(stream, "{line}").unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            serde_json::from_str::<Value>(&reply).unwrap()
        };
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":1,"method":"test.echo"}"#)["error"]["code"], code::UNAUTHORIZED);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":2,"method":"auth","params":{"token":"secret"}}"#)["result"]["ok"], true);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":3,"method":"test.echo","params":{"x":1}}"#)["result"]["x"], 1);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":4,"method":"nope"}"#)["error"]["code"], code::METHOD_NOT_FOUND);
    }
}

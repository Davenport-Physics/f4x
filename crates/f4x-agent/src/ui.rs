//! `ui.*`: look into and call into a menu's Scaleform movie.
//!
//! Each request runs inside the menu's next `AdvanceMovie` (the thread that owns the
//! movie), through `observe::on_menu`. The movie is `IMenu::uiMovie` (CommonLibF4
//! layout).
//!
//! - `ui.get {menu, path, fields?, depth?}`: the value at an ActionScript path
//!   (`root.f4se`, `root.f4se.version`), converted to JSON. Objects show the `fields`
//!   that exist; arrays show their elements.
//! - `ui.has {menu, path, names}`: which of `names` are members of the object at `path`,
//!   with their types. This is how an object is enumerated black-box: by asking about
//!   every candidate name.
//! - `ui.invoke {menu, path, method, args?, fields?}`: call `path.method(args...)`.
//!   Arguments are JSON scalars; results are converted like `ui.get`.

use std::ffi::{CString, c_void};
use std::sync::mpsc;
use std::time::Duration;

use f4x_engine::layout::RE;
use f4x_engine::scaleform::{Movie, Value, kind};
use f4x_proto::{RpcError, code};
use serde_json::{Map, Value as Json, json};

use crate::observe;

const TIMEOUT: Duration = Duration::from_secs(5);

fn movie_of(menu: *mut c_void) -> Movie {
    // SAFETY: a live IMenu; uiMovie is a BSTSmartPointer<GFx::Movie> (a raw pointer).
    Movie(unsafe { f4x_engine::mem::read::<*mut c_void>(menu as *const u8, RE::IMenu::uiMovie) })
}

/// Run `f` with the menu's movie inside its next AdvanceMovie and wait for the result.
fn in_menu<T: Send + 'static>(menu: &str, f: impl FnOnce(Movie) -> Result<T, RpcError> + Send + 'static) -> Result<T, RpcError> {
    let (tx, rx) = mpsc::channel();
    observe::on_menu(
        menu,
        Box::new(move |imenu| {
            let movie = movie_of(imenu);
            let result = if movie.0.is_null() { Err(RpcError::new(code::INTERNAL, "menu has no movie")) } else { f(movie) };
            let _ = tx.send(result);
        }),
    );
    rx.recv_timeout(TIMEOUT)
        .map_err(|_| RpcError::new(code::INTERNAL, format!("menu {menu} did not advance within {} s (is it open?)", TIMEOUT.as_secs())))?
}

pub fn to_json(value: &Value, fields: &[String], depth: usize) -> Json {
    match value.kind() {
        kind::UNDEFINED => json!({"type": "undefined"}),
        kind::NULL => Json::Null,
        kind::BOOL => json!(value.as_bool()),
        kind::INT | kind::UINT | kind::NUMBER => json!(value.as_number()),
        kind::STRING => json!(value.as_string()),
        kind::ARRAY => {
            let len = value.length().unwrap_or(0);
            let items: Vec<Json> = if depth == 0 {
                Vec::new()
            } else {
                (0..len.min(256)).map(|i| value.element(i).map_or(Json::Null, |v| to_json(&v, fields, depth - 1))).collect()
            };
            json!({"type": "array", "length": len, "items": items})
        }
        _ => {
            let mut members = Map::new();
            if depth > 0 {
                for f in fields {
                    if value.has_member(f)
                        && let Some(v) = value.get_member(f)
                    {
                        members.insert(f.clone(), to_json(&v, fields, depth - 1));
                    }
                }
            }
            let mut out = json!({"type": value.type_name(), "members": members});
            // Dates (and anything else with getTime): include the time in ms.
            if value.has_member("getTime")
                && let Some(t) = value.invoke("getTime", &[]).and_then(|t| t.as_number())
            {
                out["time_ms"] = json!(t);
            }
            out
        }
    }
}

/// The value at a dotted path: the first segment through `GetVariable` (e.g. `root`),
/// the rest member by member (deep `GetVariable` paths can fault inside Scaleform).
fn resolve(movie: Movie, path: &str) -> Result<Value, RpcError> {
    let mut parts = path.split('.');
    let first = parts.next().unwrap_or_default();
    let mut value = movie.get_variable(first).ok_or_else(|| RpcError::new(code::INTERNAL, format!("no variable {first}")))?;
    for part in parts {
        if !value.is_object() || !value.has_member(part) {
            return Err(RpcError::new(code::INTERNAL, format!("{path}: no member {part}")));
        }
        value = value.get_member(part).ok_or_else(|| RpcError::new(code::INTERNAL, format!("{path}: cannot read {part}")))?;
    }
    Ok(value)
}

fn string_list(p: &Json, key: &str) -> Vec<String> {
    p.get(key).and_then(Json::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

fn menu_and_path(p: &Json) -> Result<(String, String), RpcError> {
    let menu = p.get("menu").and_then(Json::as_str).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.menu: a menu class, e.g. MainMenu"))?;
    let path = p.get("path").and_then(Json::as_str).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.path: e.g. root.f4se"))?;
    Ok((menu.to_owned(), path.to_owned()))
}

pub fn register() {
    crate::server::register("ui.get", |_, p| {
        let (menu, path) = menu_and_path(&p)?;
        let fields = string_list(&p, "fields");
        let depth = p.get("depth").and_then(Json::as_u64).unwrap_or(2) as usize;
        in_menu(&menu, move |movie| {
            let value = resolve(movie, &path)?;
            Ok(to_json(&value, &fields, depth))
        })
    });
    crate::server::register("ui.has", |_, p| {
        let (menu, path) = menu_and_path(&p)?;
        let names = string_list(&p, "names");
        in_menu(&menu, move |movie| {
            let value = resolve(movie, &path)?;
            let mut found = Map::new();
            for name in names {
                if value.has_member(&name) {
                    let entry = match value.get_member(&name) {
                        Some(v) if v.is_object() => json!({"type": v.type_name()}),
                        Some(v) => json!({"type": v.type_name(), "value": to_json(&v, &[], 0)}),
                        None => json!({"type": "?"}),
                    };
                    found.insert(name, entry);
                }
            }
            Ok(json!({"type": value.type_name(), "members": found}))
        })
    });
    crate::server::register("ui.invoke", |_, p| {
        let (menu, path) = menu_and_path(&p)?;
        let method = p.get("method").and_then(Json::as_str).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.method"))?.to_owned();
        let args = p.get("args").and_then(Json::as_array).cloned().unwrap_or_default();
        let fields = string_list(&p, "fields");
        let depth = p.get("depth").and_then(Json::as_u64).unwrap_or(3) as usize;
        in_menu(&menu, move |movie| {
            let target = resolve(movie, &path)?;
            let strings: Vec<CString> = args.iter().map(|a| CString::new(a.as_str().unwrap_or_default()).unwrap_or_default()).collect();
            let values: Vec<Value> = args
                .iter()
                .zip(&strings)
                .map(|(a, s)| match a {
                    Json::Null => Value::null(),
                    Json::Bool(b) => Value::bool(*b),
                    Json::Number(n) => Value::number(n.as_f64().unwrap_or(0.0)),
                    Json::String(_) => Value::borrowed_str(s),
                    _ => Value::undefined(),
                })
                .collect();
            let result = target.invoke(&method, &values).ok_or_else(|| RpcError::new(code::INTERNAL, format!("{path}.{method} failed")))?;
            Ok(json!({"result": to_json(&result, &fields, depth)}))
        })
    });
}

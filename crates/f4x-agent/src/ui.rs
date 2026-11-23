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
//!   Arguments are JSON scalars, `{"int": n}` / `{"uint": n}`, `{"path": "root.x"}` (the
//!   value at that path), `{"object": {name: arg}}` (a new object) or `{"array": [arg]}`;
//!   results are converted like `ui.get`.
//! - `ui.menu {menu, dump?}`: the engine fields of the menu registered as `menu` (any
//!   menu, custom ones included; read on the main thread): `IMenu` flags, depth priority,
//!   input context, the movie's scale mode and alignment (`GFx::Movie` slots 0x0F/0x11),
//!   and with `dump` (a byte count) the object's first bytes as hex (plus, for a dump of
//!   at least 0x90 bytes, the 0xB0 bytes of `GameMenuBase::filterHolder` at +0x88).

use std::ffi::c_void;
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

/// A `ui.invoke` argument as a Scaleform value of `movie` (see the module docs).
fn arg_value(movie: Movie, a: &Json, depth: u32) -> Result<Value, RpcError> {
    let bad = || RpcError::new(code::INVALID_PARAMS, format!("unsupported argument {a}"));
    Ok(match a {
        Json::Null => Value::null(),
        Json::Bool(b) => Value::bool(*b),
        Json::Number(n) => Value::number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => movie.create_string(s).ok_or_else(bad)?,
        // Typed numbers, for code-object functions that check the GFx type.
        Json::Object(o) if o.contains_key("int") => Value::int(o["int"].as_i64().unwrap_or(0) as i32),
        Json::Object(o) if o.contains_key("uint") => Value::uint(o["uint"].as_u64().unwrap_or(0) as u32),
        Json::Object(o) if o.contains_key("path") => resolve(movie, o["path"].as_str().ok_or_else(bad)?)?,
        Json::Object(o) if depth < 8 && o.get("object").is_some_and(Json::is_object) => {
            let mut object = movie.create_object().ok_or_else(bad)?;
            for (name, v) in o["object"].as_object().into_iter().flatten() {
                object.set_member(name, &arg_value(movie, v, depth + 1)?);
            }
            object
        }
        Json::Object(o) if depth < 8 && o.get("array").is_some_and(Json::is_array) => {
            let mut array = movie.create_array().ok_or_else(bad)?;
            for v in o["array"].as_array().into_iter().flatten() {
                array.push_back(&arg_value(movie, v, depth + 1)?);
            }
            array
        }
        _ => Value::undefined(),
    })
}

fn string_list(p: &Json, key: &str) -> Vec<String> {
    p.get(key).and_then(Json::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

fn menu_and_path(p: &Json) -> Result<(String, String), RpcError> {
    let menu = p.get("menu").and_then(Json::as_str).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.menu: a menu class, e.g. MainMenu"))?;
    let path = p.get("path").and_then(Json::as_str).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.path: e.g. root.f4se"))?;
    Ok((menu.to_owned(), path.to_owned()))
}

/// `ui.menu`: a registered menu's engine fields (CommonLibF4 `IMenu` layout).
fn menu_fields(name: &str, dump: usize) -> Json {
    use f4x_engine::mem::{fixed_string, read, readable};
    let registered = f4x_engine::ui::is_registered(name);
    let fields = f4x_engine::ui::with_menu(name, |menu| {
        let m = menu.as_ptr();
        // SAFETY: a live IMenu (found readable for IMenu::SIZE, referenced for the call).
        unsafe {
            let movie: *const u8 = read(m, RE::IMenu::uiMovie);
            let (scale, align) = if movie.is_null() || !readable(movie as usize, 8) {
                (Json::Null, Json::Null)
            } else {
                let vtbl: *const usize = read(movie, 0);
                let get = |slot: usize| std::mem::transmute::<usize, unsafe extern "C" fn(*const u8) -> i32>(*vtbl.add(slot))(movie);
                (json!(get(0x0F)), json!(get(0x11)))
            };
            let bytes: Vec<u8> = if dump > 0 && readable(m as usize, dump) { (0..dump).map(|i| read::<u8>(m, i)).collect() } else { Vec::new() };
            // GameMenuBase::filterHolder (+0x88, a BSGFxShaderFXTarget of 0xB0 bytes), when
            // the dump covers it and it points somewhere readable.
            let holder: *const u8 = if bytes.len() >= 0x90 { read(m, 0x88) } else { std::ptr::null() };
            let holder_bytes: Vec<u8> = if !holder.is_null() && readable(holder as usize, 0xB0) { (0..0xB0).map(|i| read::<u8>(holder, i)).collect() } else { Vec::new() };
            json!({
                "flags": read::<u32>(m, RE::IMenu::menuFlags),
                "depth": read::<u8>(m, RE::IMenu::depthPriority),
                "input_context": read::<i32>(m, RE::IMenu::inputContext),
                "passes_top_menu_test": read::<u8>(m, RE::IMenu::passesTopMenuTest),
                "can_be_visible": read::<u8>(m, RE::IMenu::menuCanBeVisible),
                "menu_name": fixed_string(read(m, RE::IMenu::menuName)),
                "renderer": fixed_string(read(m, RE::IMenu::customRendererName)),
                "has_movie": !movie.is_null(),
                "scale_mode": scale,
                "align": align,
                "vtable_rva": read::<usize>(m, 0).wrapping_sub(f4x_engine::game().base()),
                "dump": bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "filter_holder": format!("{holder:p}"),
                "filter_holder_dump": holder_bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            })
        }
    });
    json!({"registered": registered, "exists": fields.is_some(), "menu": fields})
}

pub fn register() {
    crate::server::register("ui.menu", |_, p| {
        let menu = p.get("menu").and_then(Json::as_str).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.menu: a registered menu name"))?.to_owned();
        let dump = p.get("dump").and_then(Json::as_u64).unwrap_or(0).min(0x400) as usize;
        crate::pump::on_main(move || menu_fields(&menu, dump), TIMEOUT)
    });
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
            let values = args.iter().map(|a| arg_value(movie, a, 0)).collect::<Result<Vec<_>, _>>()?;
            let result = target.invoke(&method, &values).ok_or_else(|| RpcError::new(code::INTERNAL, format!("{path}.{method} failed")))?;
            Ok(json!({"result": to_json(&result, &fields, depth)}))
        })
    });
}

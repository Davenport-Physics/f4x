//! Engine reads for F4SE's data-file and console features, the same under both hosts
//! (for black-box observation of F4SE and differential checks of f4x):
//!
//! - `ui.translations {keys?, prefix?, limit?}`: the Scaleform translation table
//!   (`$KEY` → text): its size, the value of each of `keys` (null when absent), and up to
//!   `limit` entries whose key starts with `prefix`, sorted;
//! - `engine.controlmap {contexts?, events?}`: the engine's control map per input context
//!   and device, optionally only some contexts (names or indices) and events;
//! - `console.text {tail?}`: the console's text (its last `tail` characters).

use std::collections::BTreeMap;
use std::time::Duration;

use f4x_engine::control_map;
use f4x_engine::translation;
use f4x_proto::{RpcError, code};
use serde_json::{Value, json};

use crate::pump::on_main;
use crate::server;

const TIMEOUT: Duration = Duration::from_secs(10);

fn strings(p: &Value, key: &str) -> Vec<String> {
    p.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

pub fn register() {
    server::register("ui.translations", |_, p| {
        let keys = strings(&p, "keys");
        let prefix = p.get("prefix").and_then(Value::as_str).map(str::to_owned);
        let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(200) as usize;
        on_main(
            move || {
                let translator = translation::translator_like()
                    .ok_or_else(|| RpcError::new(code::ENGINE, "no translator with a readable map"))?;
                let (count, capacity) = translation::stats(translator);
                let entries = translation::entries(translator);
                let map: BTreeMap<&str, &str> =
                    entries.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                let values: serde_json::Map<String, Value> = keys
                    .iter()
                    .map(|k| (k.clone(), map.get(k.as_str()).map_or(Value::Null, |v| json!(v))))
                    .collect();
                let matches: Vec<Value> = match &prefix {
                    Some(prefix) => map
                        .iter()
                        .filter(|(k, _)| k.starts_with(prefix.as_str()))
                        .take(limit)
                        .map(|(k, v)| json!([k, v]))
                        .collect(),
                    None => Vec::new(),
                };
                Ok(json!({
                    "translator": format!("{:#x}", translator as usize),
                    "count": count,
                    "capacity": capacity,
                    "read": entries.len(),
                    "language": translation::ini_string("sLanguage:General"),
                    "values": values,
                    "matches": matches,
                }))
            },
            TIMEOUT,
        )?
    });
    server::register("ui.scaleform_states", |_, _| {
        // The GFx loader's translator and log states: where their vtables live (module,
        // RVA, and the class when it is a game class); for the log state, also its log.
        on_main(
            || {
                let loader =
                    translation::loader().ok_or_else(|| RpcError::new(code::ENGINE, "no GFx loader yet"))?;
                let describe = |object: *mut u8| -> Value {
                    if object.is_null() || !f4x_engine::mem::readable(object as usize, 8) {
                        return json!({"address": format!("{:#x}", object as usize)});
                    }
                    // SAFETY: readable (checked).
                    let vtable: usize = unsafe { f4x_engine::mem::read(object, 0) };
                    let frame = crate::crash::frame(vtable as u64);
                    let game = f4x_engine::game();
                    let class = (frame.module.as_deref() == Some("Fallout4.exe"))
                        .then(|| unsafe { game.class_name(object) })
                        .flatten();
                    json!({"address": format!("{:#x}", object as usize), "vtable_module": frame.module, "vtable_rva": frame.rva.map(|r| format!("{r:#x}")), "class": class})
                };
                let mut out = json!({"loader": describe(loader)});
                if let Some(state) = translation::loader_state(loader, translation::STATE_TRANSLATOR) {
                    out["translator"] = describe(state);
                    out["translator_map_readable"] = json!(translation::translator_like().is_some());
                }
                if let Some(state) = translation::loader_state(loader, translation::STATE_LOG) {
                    out["log_state"] = describe(state);
                    if f4x_engine::mem::readable(state as usize, 0x28) {
                        // SAFETY: readable (checked); the LogState's Ptr<Log> at +0x20.
                        out["log"] = describe(unsafe { f4x_engine::mem::read::<*mut u8>(state, 0x20) });
                    }
                }
                Ok(out)
            },
            TIMEOUT,
        )?
    });
    server::register("ui.gfxlog.emit", |_, p| {
        // Call the GFx loader's current log (`GFx::Log::LogMessageVarg`, vtable slot 1)
        // with `"%s"` and `text`, as Scaleform would: shows what the host's log does
        // with a message (`id` is the LogMessageId value).
        let text = p.get("text").and_then(Value::as_str).unwrap_or("f4x gfx log probe").to_owned();
        let id = p.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
        on_main(
            move || {
                let loader =
                    translation::loader().ok_or_else(|| RpcError::new(code::ENGINE, "no GFx loader yet"))?;
                let state = translation::loader_state(loader, translation::STATE_LOG)
                    .ok_or_else(|| RpcError::new(code::ENGINE, "no log state"))?;
                if !f4x_engine::mem::readable(state as usize, 0x28) {
                    return Err(RpcError::new(code::ENGINE, "log state not readable"));
                }
                // SAFETY: the LogState's Ptr<Log> at +0x20 (checked readable below).
                let log: *mut u8 = unsafe { f4x_engine::mem::read(state, 0x20) };
                if !f4x_engine::mem::readable(log as usize, 0x10) {
                    return Err(RpcError::new(code::ENGINE, "no log object"));
                }
                // SAFETY: readable (checked).
                let vtable: *const usize = unsafe { f4x_engine::mem::read(log, 0) };
                if !f4x_engine::mem::readable(vtable as usize, 0x10) {
                    return Err(RpcError::new(code::ENGINE, "log vtable not readable"));
                }
                let message = std::ffi::CString::new(text).unwrap_or_default();
                let format = c"%s";
                // A Windows x64 va_list: a pointer to 8-byte argument slots.
                let args: [u64; 2] = [message.as_ptr() as u64, 0];
                type LogMessageVarg = unsafe extern "C" fn(*mut u8, i32, *const std::ffi::c_char, *const u64);
                // SAFETY: GFx::Log::LogMessageVarg(this, LogMessageId, fmt, va_list).
                unsafe {
                    let f: LogMessageVarg = std::mem::transmute(*vtable.add(1));
                    f(log, id, format.as_ptr(), args.as_ptr());
                }
                Ok(json!({"ok": true, "log_module": crate::crash::frame(vtable as u64).module}))
            },
            TIMEOUT,
        )?
    });
    server::register("engine.controlmap", |_, p| {
        let wanted: Vec<String> = p
            .get("contexts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| {
                        v.as_u64().map_or_else(|| v.as_str().unwrap_or("").to_owned(), |i| i.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let events = strings(&p, "events");
        on_main(
            move || {
                f4x_engine::controls::control_map()
                    .ok_or_else(|| RpcError::new(code::ENGINE, "no ControlMap yet"))?;
                let mut contexts = serde_json::Map::new();
                for (index, name) in control_map::CONTEXTS.iter().enumerate() {
                    if !wanted.is_empty()
                        && !wanted.iter().any(|w| w.eq_ignore_ascii_case(name) || *w == index.to_string())
                    {
                        continue;
                    }
                    let mut devices = serde_json::Map::new();
                    for (d, device) in control_map::DEVICES.iter().enumerate() {
                        let Some(mappings) = control_map::mappings(index, d) else { continue };
                        let list: Vec<Value> = mappings
                            .iter()
                            .filter(|m| {
                                events.is_empty() || events.iter().any(|e| e.eq_ignore_ascii_case(&m.event))
                            })
                            .map(|m| {
                                json!([
                                    m.event,
                                    format!("{:#x}", m.key),
                                    m.index,
                                    m.remappable,
                                    m.linked,
                                    format!("{:#x}", m.flags)
                                ])
                            })
                            .collect();
                        devices
                            .insert((*device).to_owned(), json!({"count": mappings.len(), "mappings": list}));
                    }
                    contexts.insert(format!("{index}:{name}"), Value::Object(devices));
                }
                let linked: Vec<Value> = control_map::linked_mappings()
                    .iter()
                    .map(|l| json!([l.name, l.context, l.device, l.from_context, l.from_name]))
                    .collect();
                Ok(json!({"contexts": contexts, "linked": linked}))
            },
            TIMEOUT,
        )?
    });
    server::register("console.commands", |_, p| {
        // Console command table entries whose name or short name contains `filter`.
        let filter = p.get("filter").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
        on_main(
            move || {
                use f4x_engine::console::{CONSOLE_FUNCTION_COUNT, console_functions, is_noop_handler};
                let table = console_functions()
                    .ok_or_else(|| RpcError::new(code::ENGINE, "no console command table"))?;
                let text = |p: *const std::ffi::c_char| {
                    // SAFETY: entry strings are static C strings or null.
                    (!p.is_null())
                        .then(|| unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned())
                };
                let mut out = Vec::new();
                for i in 0..CONSOLE_FUNCTION_COUNT {
                    // SAFETY: the engine's SCRIPT_FUNCTION[522].
                    let entry = unsafe { &*table.add(i) };
                    let (name, short) =
                        (text(entry.name).unwrap_or_default(), text(entry.short_name).unwrap_or_default());
                    if !filter.is_empty()
                        && !name.to_ascii_lowercase().contains(&filter)
                        && !short.to_ascii_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    let params: Vec<Value> = (0..entry.param_count as usize)
                        .filter(|_| !entry.params.is_null())
                        // SAFETY: `param_count` SCRIPT_PARAMETERs.
                        .map(|j| unsafe { &*entry.params.add(j) })
                        .map(|p| json!([text(p.name), p.kind, p.optional]))
                        .collect();
                    let execute = entry.execute.map(|f| f as usize).unwrap_or(0);
                    let frame = crate::crash::frame(execute as u64);
                    out.push(json!({
                        "slot": i, "name": name, "short": short, "help": text(entry.help),
                        "output": entry.output, "reference_function": entry.reference_function, "params": params,
                        "execute_module": frame.module, "execute_rva": frame.rva.map(|r| format!("{r:#x}")),
                        "execute_noop": is_noop_handler(entry.execute), "compile": !entry.compile.is_null(),
                        "condition": !entry.condition.is_null(), "editor_filter": entry.editor_filter,
                    }));
                }
                Ok(json!(out))
            },
            TIMEOUT,
        )?
    });
    server::register("console.text", |_, p| {
        let tail = p.get("tail").and_then(Value::as_u64).unwrap_or(4000) as usize;
        on_main(
            move || {
                let text = f4x_engine::console::text()
                    .ok_or_else(|| RpcError::new(code::ENGINE, "no console log yet"))?;
                let chars: Vec<char> = text.chars().collect();
                let start = chars.len().saturating_sub(tail);
                Ok(json!({"length": chars.len(), "text": chars[start..].iter().collect::<String>()}))
            },
            TIMEOUT,
        )?
    });
}

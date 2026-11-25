//! `engine.*` control methods: read-only engine introspection through `f4x-engine`.
//! Every call runs on the game's main thread (through the pump).

use std::time::Duration;

use f4x_engine::form_type::FormType;
use f4x_engine::{DataHandler, Form, FormId, refr};
use f4x_proto::{RpcError, code};
use serde_json::{Value, json};

use crate::pump::on_main;
use crate::server;

const TIMEOUT: Duration = Duration::from_secs(10);

fn engine_error(message: impl Into<String>) -> RpcError {
    RpcError::new(code::ENGINE, message)
}

pub fn form_json(form: Form) -> Value {
    json!({
        "id": form.id().to_string(),
        "type": form.form_type().signature(),
        "class": form.class_name(),
        "editor_id": form.editor_id(),
        "name": form.full_name(),
        "deleted": form.is_deleted(),
    })
}

fn reference_json(reference: f4x_engine::Reference) -> Value {
    let p = reference.position();
    let r = reference.rotation();
    json!({
        "form": form_json(reference.form()),
        "base": reference.base_object().map(form_json),
        "position": [p.x, p.y, p.z],
        "rotation": [r.x, r.y, r.z],
        "cell": reference.parent_cell().map(form_json),
    })
}

fn data_handler() -> Result<DataHandler, RpcError> {
    DataHandler::get().ok_or_else(|| engine_error("TESDataHandler not available yet"))
}

pub fn register() {
    server::register("engine.player", |_, _| {
        on_main(
            || refr::player().map(reference_json).ok_or_else(|| engine_error("no player yet")),
            TIMEOUT,
        )?
    });
    server::register("engine.form", |_, p| {
        let id = p
            .get("id")
            .and_then(Value::as_str)
            .and_then(FormId::parse)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.id: hex form id, e.g. \"0000000F\""))?;
        on_main(
            move || {
                let form = data_handler()?
                    .lookup(id)
                    .or_else(|| f4x_engine::data::form_by_id(id))
                    .ok_or_else(|| engine_error(format!("no form {id}")))?;
                let mut value = form_json(form);
                if let Some(reference) = form.as_reference() {
                    value["reference"] = reference_json(reference);
                }
                Ok(value)
            },
            TIMEOUT,
        )?
    });
    server::register("papyrus.call", |_, p| papyrus_call(&p));
    server::register("engine.forms.all", |_, p| {
        // Every form of a type from the global form map (placed references included).
        let signature = p.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
        let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
        let ty = FormType::from_signature(&signature).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.type: a record signature"))?;
        on_main(move || Ok(json!(f4x_engine::data::all_forms_of_type(ty, 200_000).into_iter().take(limit).map(form_json).collect::<Vec<_>>())), TIMEOUT)?
    });
    server::register("papyrus.intern", |_, p| {
        // The string pool entry for each string (interning it if new): pool order is
        // what F4SE's registration maps sort by.
        let strings: Vec<String> =
            p.get("strings").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        Ok(json!(strings
            .iter()
            .map(|s| {
                let entry = f4x_engine::papyrus::FixedString::new(s);
                json!({"string": s, "entry": entry.map(|e| format!("{:#x}", e.as_raw() as usize))})
            })
            .collect::<Vec<_>>()))
    });
    server::register("engine.files", |_, _| {
        on_main(
            || {
                let files = data_handler()?.files();
                Ok(json!(files
                    .iter()
                    .map(|f| json!({"name": f.name, "index": f.index, "light_index": f.light_index}))
                    .collect::<Vec<_>>()))
            },
            TIMEOUT,
        )?
    });
    server::register("engine.forms.count", |_, _| {
        on_main(
            || {
                let handler = data_handler()?;
                let mut by_type = serde_json::Map::new();
                for t in 0..FormType::COUNT as u8 {
                    let n = handler.count_of_type(FormType(t));
                    if n > 0 {
                        by_type.insert(FormType(t).signature().to_owned(), n.into());
                    }
                }
                Ok(json!({"total": handler.form_count(), "by_type": by_type}))
            },
            TIMEOUT,
        )?
    });
    server::register("engine.forms.of_type", |_, p| {
        let signature = p.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
        let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
        let ty = FormType::from_signature(&signature)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.type: a record signature, e.g. \"WEAP\""))?;
        on_main(
            move || Ok(json!(data_handler()?.forms_of_type(ty).take(limit).map(form_json).collect::<Vec<_>>())),
            TIMEOUT,
        )?
    });
    server::register("engine.cell.refs", |_, p| {
        let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
        on_main(
            move || {
                let player = refr::player().ok_or_else(|| engine_error("no player yet"))?;
                let cell = player.parent_cell().ok_or_else(|| engine_error("player is not in a cell"))?;
                let refs = refr::cell_references(cell);
                Ok(json!({
                    "cell": form_json(cell),
                    "count": refs.len(),
                    "refs": refs.into_iter().take(limit).map(reference_json).collect::<Vec<_>>(),
                }))
            },
            TIMEOUT,
        )?
    });
    server::register("engine.debug.scan_arrays", |_, p| {
        // Diagnostic: for globals pointing at heap objects, count how many have a
        // BSTArray at `offset` whose first element is a form of `type`.
        use f4x_engine::layout::{RE, form_type};
        // Default: TESDataHandler::formArrays[WEAP] (a BSTArray is 0x18 bytes).
        let default = RE::TESDataHandler::formArrays + 0x18 * usize::from(form_type::WEAP);
        let offset = p.get("offset").and_then(Value::as_u64).map_or(default, |o| o as usize);
        let ty = p.get("type").and_then(Value::as_u64).map_or(form_type::WEAP, |t| t as u8);
        on_main(
            move || {
                use f4x_engine::mem::{Regions, read};
                let game = f4x_engine::game();
                let regions = Regions::snapshot();
                let base = game.base();
                let end = base + f4x_pe::Image::sections(game.image()).iter().map(|s| (s.rva + s.virtual_size) as usize).max().unwrap_or(0);
                let (mut pointers, mut arrays, mut typed) = (0u64, 0u64, Vec::new());
                for section in f4x_pe::Image::sections(game.image()).iter().filter(|s| s.is_writable() && !s.is_executable()) {
                    let bytes = f4x_pe::Image::section_bytes(game.image(), section);
                    for (i, chunk) in bytes.as_chunks::<8>().0.iter().enumerate() {
                        let value = usize::from_le_bytes(*chunk);
                        if value < 0x10000 || value % 8 != 0 || (value >= base && value < end) || !regions.contains(value, offset + 0x18) {
                            continue;
                        }
                        pointers += 1;
                        let array = value + offset;
                        let (data, cap, size): (usize, u32, u32) = unsafe { (read(array as *const u8, 0), read(array as *const u8, 8), read(array as *const u8, 0x10)) };
                        if size == 0 || cap < size || !regions.contains(data, 8) {
                            continue;
                        }
                        arrays += 1;
                        let first: usize = unsafe { read(data as *const u8, 0) };
                        if regions.contains(first, RE::TESForm::SIZE) && unsafe { read::<u8>(first as *const u8, RE::TESForm::formType) } == ty && typed.len() < 10 {
                            typed.push(json!({"global_rva": format!("{:#x}", section.rva + 8 * i as u32), "object": format!("{value:#x}"), "size": size}));
                        }
                    }
                }
                Ok(json!({"offset": format!("{offset:#x}"), "pointers": pointers, "arrays": arrays, "typed": typed}))
            },
            Duration::from_secs(60),
        )?
    });
    server::register("console.exec", |_, p| {
        let command = p
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.command: a console command"))?
            .to_owned();
        log::info!(target: "f4x::console", "console: {command}");
        on_main(move || f4x_engine::console::execute(&command).map_err(engine_error), TIMEOUT)??;
        Ok(json!({"ok": true}))
    });
    server::register("engine.singletons", |_, _| {
        on_main(
            || {
                let _ = DataHandler::get();
                let _ = refr::player();
                let game = f4x_engine::game();
                Ok(json!(game
                    .known_singletons()
                    .into_iter()
                    .map(|(class, rvas)| json!({"class": class, "globals": rvas.iter().map(|r| format!("{r:#x}")).collect::<Vec<_>>()}))
                    .collect::<Vec<_>>()))
            },
            TIMEOUT,
        )?
    });
}

// ---- papyrus.call ----

/// `papyrus.call {function: "Script.Function", self?: "form id", args?: [...], timeout_s?}`.
/// Arguments: booleans, integers, floats, strings, null (None), `{"form": "id"}`.
/// For testing and for driving scripts on request: it goes through the Papyrus VM.
fn papyrus_call(p: &Value) -> Result<Value, RpcError> {
    use f4x_engine::papyrus::{Arg, Vm};
    let function = p
        .get("function")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.function: \"Script.Function\""))?;
    let (script, name) = function
        .split_once('.')
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.function: \"Script.Function\""))?;
    let self_id = match p.get("self") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().and_then(FormId::parse).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.self: hex form id"))?),
    };
    let args_json: Vec<Value> = p.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    let timeout = Duration::from_secs_f64(p.get("timeout_s").and_then(Value::as_f64).unwrap_or(5.0));
    let vm = Vm(crate::observe::virtual_machine().ok_or_else(|| engine_error("no Papyrus VM yet"))? as *mut _);

    let pending = on_main(
        move || {
            let handler = data_handler()?;
            let object_for = |id: FormId, script: Option<&str>| -> Result<*mut std::ffi::c_void, RpcError> {
                let form = handler
                    .lookup(id)
                    .or_else(|| f4x_engine::data::form_by_id(id))
                    .ok_or_else(|| engine_error(format!("no form {id}")))?;
                let form_type = form.form_type();
                let script = match script {
                    Some(s) => s.to_owned(),
                    None => vm.script_for_type(u32::from(form_type.0)).ok_or_else(|| engine_error(format!("no script type for {}", form_type.signature())))?,
                };
                vm.object_for_form(form.as_ptr(), form_type, &script)
                    .ok_or_else(|| engine_error(format!("could not get a {script} object for {id}")))
            };
            let mut args = Vec::new();
            for (i, a) in args_json.iter().enumerate() {
                args.push(match a {
                    Value::Null => Arg::None,
                    Value::Bool(b) => Arg::Bool(*b),
                    Value::Number(n) if n.is_i64() => Arg::Int(n.as_i64().unwrap_or(0) as i32),
                    Value::Number(n) => Arg::Float(n.as_f64().unwrap_or(0.0) as f32),
                    Value::String(s) => Arg::String(s.clone()),
                    Value::Object(o) if o.contains_key("float") => Arg::Float(o["float"].as_f64().unwrap_or(0.0) as f32),
                    Value::Object(o) if o.contains_key("type") => {
                        let t = o["type"].as_str().unwrap_or_default().to_owned();
                        Arg::Typed(t, json_to_pvalue(handler, o.get("value").unwrap_or(&Value::Null))?)
                    }
                    Value::Object(o) => {
                        let id = o.get("form").and_then(Value::as_str).and_then(FormId::parse).ok_or_else(|| {
                            RpcError::new(code::INVALID_PARAMS, format!("args[{i}]: {{\"form\": \"id\"}}"))
                        })?;
                        Arg::Object(object_for(id, o.get("script").and_then(Value::as_str))?)
                    }
                    Value::Array(_) => return Err(RpcError::new(code::INVALID_PARAMS, format!("args[{i}]: arrays are not supported"))),
                });
            }
            let pending = match self_id {
                Some(id) => vm.call_method(object_for(id, Some(&script))?, &name, args),
                None => vm.call_static(&script, &name, args),
            };
            pending.ok_or_else(|| engine_error(format!("the VM refused the call to {script}.{name}")))
        },
        TIMEOUT,
    )??;

    let started = std::time::Instant::now();
    let returned = loop {
        if let Some(r) = pending.get() {
            break r;
        }
        if started.elapsed() > timeout {
            return Err(engine_error(format!("{function}: no result within {:.1} s", timeout.as_secs_f64())));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let value = match returned {
        Ok(v) => on_main(move || Ok::<_, RpcError>(pvalue_to_json(&v)), TIMEOUT)??,
        Err(e) => return Err(engine_error(format!("{function}: {e}"))),
    };
    Ok(json!({"result": value}))
}

/// A Papyrus value as JSON: forms as `{"form": id, "type": sig}`, other script objects as
/// `{"handle", "script"}`, structs as `{"struct": {field: value}}`, `Var` as `{"var": v}`.
pub fn pvalue_to_json(v: &f4x_engine::papyrus_value::PValue) -> Value {
    use f4x_engine::papyrus_value::PValue;
    match v {
        PValue::None => Value::Null,
        PValue::Bool(b) => json!(b),
        PValue::Int(i) => json!(i),
        PValue::Float(f) => json!({"float": f}),
        PValue::String(s) => json!(s),
        PValue::Form(f) => match Form::checked(*f) {
            Some(form) => json!({"form": form.id().to_string(), "type": form.form_type().signature()}),
            None => json!({"form": null}),
        },
        PValue::Object { handle, script } => json!({"handle": format!("{handle:#x}"), "script": script}),
        PValue::Array(items) => json!(items.iter().map(pvalue_to_json).collect::<Vec<_>>()),
        PValue::Struct(fields) => {
            json!({"struct": fields.iter().map(|(k, v)| (k.clone(), pvalue_to_json(v))).collect::<serde_json::Map<_, _>>()})
        }
        PValue::Var(inner) => json!({"var": pvalue_to_json(inner)}),
    }
}

/// JSON to a Papyrus value (inverse of [`pvalue_to_json`]): numbers with a fraction or
/// `{"float": x}` are floats, `{"form": id}` forms, `{"struct": {...}}` structs, arrays
/// arrays, `{"var": v}` variants.
fn json_to_pvalue(handler: DataHandler, v: &Value) -> Result<f4x_engine::papyrus_value::PValue, RpcError> {
    use f4x_engine::papyrus_value::PValue;
    Ok(match v {
        Value::Null => PValue::None,
        Value::Bool(b) => PValue::Bool(*b),
        Value::Number(n) if n.is_i64() => PValue::Int(n.as_i64().unwrap_or(0) as i32),
        Value::Number(n) => PValue::Float(n.as_f64().unwrap_or(0.0) as f32),
        Value::String(s) => PValue::String(s.clone()),
        Value::Array(items) => PValue::Array(items.iter().map(|i| json_to_pvalue(handler, i)).collect::<Result<_, _>>()?),
        Value::Object(o) => {
            if let Some(f) = o.get("float").and_then(Value::as_f64) {
                PValue::Float(f as f32)
            } else if let Some(id) = o.get("form") {
                if id.is_null() {
                    PValue::None
                } else {
                    let id = id.as_str().and_then(FormId::parse).ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "form: hex id"))?;
                    let form = handler
                        .lookup(id)
                        .or_else(|| f4x_engine::data::form_by_id(id))
                        .ok_or_else(|| engine_error(format!("no form {id}")))?;
                    PValue::Form(form.as_ptr())
                }
            } else if let Some(fields) = o.get("struct").and_then(Value::as_object) {
                PValue::Struct(fields.iter().map(|(k, v)| Ok((k.clone(), json_to_pvalue(handler, v)?))).collect::<Result<_, RpcError>>()?)
            } else if let Some(inner) = o.get("var") {
                PValue::Var(Box::new(json_to_pvalue(handler, inner)?))
            } else {
                return Err(RpcError::new(code::INVALID_PARAMS, "object values: {form}, {float}, {struct}, {var}"));
            }
        }
    })
}

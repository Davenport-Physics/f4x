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
                let form = data_handler()?.lookup(id).ok_or_else(|| engine_error(format!("no form {id}")))?;
                let mut value = form_json(form);
                if let Some(reference) = form.as_reference() {
                    value["reference"] = reference_json(reference);
                }
                Ok(value)
            },
            TIMEOUT,
        )?
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
        let offset = p.get("offset").and_then(Value::as_u64).unwrap_or(0x68 + 0x18 * 0x2B) as usize;
        let ty = p.get("type").and_then(Value::as_u64).unwrap_or(0x2B) as u8;
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
                        if regions.contains(first, 0x20) && unsafe { read::<u8>(first as *const u8, 0x1A) } == ty && typed.len() < 10 {
                            typed.push(json!({"global_rva": format!("{:#x}", section.rva + 8 * i as u32), "object": format!("{value:#x}"), "size": size}));
                        }
                    }
                }
                Ok(json!({"offset": format!("{offset:#x}"), "pointers": pointers, "arrays": arrays, "typed": typed}))
            },
            Duration::from_secs(60),
        )?
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

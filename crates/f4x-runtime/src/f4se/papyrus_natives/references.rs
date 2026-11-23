//! ObjectReference, FavoritesManager.
//!
//! Behaviour reference: CommonLibF4 `F4SEStub/runtime/src/Papyrus/ObjectReference.h` (MIT)
//! for display names, inventories, mods, material swaps and power links; checked against
//! real F4SE with `tools/f4se_natives_check.py` and targeted black-box calls. F4SEStub has
//! nothing for FavoritesManager or for the workshop natives; their behaviour here is
//! black-box, built on engine functions found by our own analysis of `Fallout4.exe`:
//!
//! - Empty arrays come back as None (F4SE never returns an empty array).
//! - `GetFavorites` is always the 12 favorite slots (None where empty); `SetFavorites`
//!   writes the first 12 entries and clears the rest (None clears all).
//! - Tagged forms are the engine's "tag for search" components
//!   (`FavoritesManager::favoritedComponents`), changed through the engine's own toggle;
//!   any form can be tagged.
//! - `Scrap(workshopRef)` is workshop-mode scrapping (`Workshop::ScrapReference`) with
//!   `workshopRef` as the current workshop: `self` is disabled (and deleted when it comes
//!   from a plugin), its inventory and scrap components go to the workshop's container
//!   (nothing is credited when `workshopRef` is not a workshop, e.g. the player), and it
//!   is unlinked from its workshop. Scrapping again credits again. With None, the
//!   workshop whose area holds `self` among the loaded cells is used; with none, the
//!   result is false and nothing happens. Otherwise true. The change is saved.
//! - `ApplyMaterialSwap(swap, renameMaterial)` changes only the loaded 3D (not
//!   ExtraMaterialSwap; reloading the 3D reverts it). Each lighting or effect geometry
//!   whose material key (the shader property's name without `Materials\`) the swap maps
//!   gets the target material, a lighting one also the entry's color remapping index; with
//!   `renameMaterial` the property is renamed `Materials\<target>`, so later swaps match
//!   the new name. The result is the undo data, in reverse scene-graph order: `source` =
//!   the new material, `target` = the old key, `colorIndex` = the old lighting material's
//!   color remapping index (`FLT_MAX` for effect materials). None without 3D, without a
//!   swap or when nothing matches.
//! - F4SE runs both (and AttachWire, GetConnectPoints, TransmitConnectedPower,
//!   GetInventoryItems and the FavoritesManager natives) as latent natives on the main
//!   thread; so does f4x (`papyrus_natives::LATENT`): the calling script waits until the
//!   handler has run on the main-loop pump, so `IsDisabled` right after `Scrap` sees it.

use f4x_engine::form::Form;
use f4x_engine::form_type::FormType;
use f4x_engine::layout::RE;
use f4x_engine::matswap;
use f4x_engine::mem;
use f4x_engine::objref::{self, ExtraList, RefOrInventory};
use f4x_engine::papyrus::NativeCall;
use f4x_engine::papyrus_value::PValue;
use f4x_engine::refr::Reference;

use super::Entry;

fn sig(signature: &str) -> FormType {
    FormType::from_signature(signature).unwrap_or(FormType(0xFF))
}

/// An array result: None when empty (as F4SE returns).
fn array(items: Vec<PValue>) -> PValue {
    if items.is_empty() { PValue::None } else { PValue::Array(items) }
}

fn form_value(p: *mut u8) -> PValue {
    if Form::checked(p).is_some() { PValue::Form(p) } else { PValue::None }
}

/// `self` as a placed reference or an inventory object.
fn self_target(c: &NativeCall) -> Option<RefOrInventory> {
    objref::ref_or_inventory(c.vm, c.self_object()?)
}

/// `self` as a placed reference.
fn self_ref(c: &NativeCall) -> Option<Reference> {
    let (_, p) = c.vm.game_object(c.self_object()?)?;
    Form::checked(p)?.as_reference()
}

/// Argument `i` as a placed reference.
fn arg_ref(c: &NativeCall, i: usize) -> Option<Reference> {
    match c.value(i) {
        PValue::Form(p) => Form::checked(p)?.as_reference(),
        _ => None,
    }
}

/// Argument `i` as a form.
fn arg_form(c: &NativeCall, i: usize) -> Option<Form> {
    match c.value(i) {
        PValue::Form(p) => Form::checked(p),
        _ => None,
    }
}

/// Argument `i` as a list of forms (None entries as null).
fn arg_forms(c: &NativeCall, i: usize) -> Vec<*mut u8> {
    match c.value(i) {
        PValue::Array(items) => items
            .into_iter()
            .map(|v| match v {
                PValue::Form(p) if Form::checked(p).is_some() => p,
                _ => std::ptr::null_mut(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------- ObjectReference

fn get_display_name(c: &mut NativeCall) -> Result<(), String> {
    let name = self_target(c)
        .and_then(|t| {
            objref::with_reference_data(t, |base, extra| {
                if let Some(name) = extra.and_then(|x| x.text_display_name(base)) {
                    return name;
                }
                base.and_then(Form::full_name).unwrap_or_default()
            })
        })
        .unwrap_or_default();
    c.ret_string(&name);
    Ok(())
}

fn get_inventory_items(c: &mut NativeCall) -> Result<(), String> {
    let items = self_ref(c)
        .and_then(|r| objref::with_inventory(r, |items| items.iter().map(|i| form_value(i.object)).collect::<Vec<_>>()))
        .unwrap_or_default();
    c.ret_value(&array(items))
}

fn get_inventory_weight(c: &mut NativeCall) -> Result<(), String> {
    let weight = self_ref(c).and_then(objref::weight_in_container).unwrap_or(0.0);
    c.ret_float(weight);
    Ok(())
}

fn get_all_mods(c: &mut NativeCall) -> Result<(), String> {
    let ids = self_target(c).and_then(|t| objref::with_reference_data(t, |_, extra| extra.and_then(ExtraList::mod_ids))).flatten().unwrap_or_default();
    let mods: Vec<PValue> = ids.into_iter().map(|id| objref::lookup_form(id).map_or(PValue::None, |f| PValue::Form(f.as_ptr()))).collect();
    c.ret_value(&array(mods))
}

fn get_material_swap(c: &mut NativeCall) -> Result<(), String> {
    let swap = self_target(c).and_then(|t| objref::with_reference_data(t, |_, extra| extra.and_then(ExtraList::material_swap))).flatten();
    c.ret_value(&swap.map_or(PValue::None, |f| PValue::Form(f.as_ptr())))
}

fn set_material_swap(c: &mut NativeCall) -> Result<(), String> {
    let swap = arg_form(c, 0).map_or(std::ptr::null_mut(), Form::as_ptr);
    if let Some(t) = self_target(c) {
        objref::with_reference_data(t, |_, extra| {
            if let Some(x) = extra {
                x.set_material_swap(swap);
            }
        });
    }
    Ok(())
}

/// Run `job` now on the main thread (where f4x runs these natives, latent as in F4SE),
/// else queue it for the next main-thread pump.
fn on_main_or_post(job: impl FnOnce() + Send + 'static) {
    if f4x_agent::clock::on_main_thread() {
        job();
    } else {
        f4x_agent::pump::post(job);
    }
}

/// Apply a material swap to the loaded 3D of a reference: each lighting or effect
/// geometry whose material the swap maps is given the target material through the
/// engine, and its shader property is renamed `Materials\<target>` when asked.
fn apply_swap_to_3d(reference: Reference, swap: Form, rename: bool) {
    let Some(root) = objref::loaded_3d(reference) else { return };
    for m in matswap::swap_matches(root, swap) {
        if matswap::apply_to_geometry(m.slot.geometry, m.slot.kind, swap) && rename {
            matswap::set_property_name(m.slot.property, &format!("Materials\\{}", m.target));
        }
    }
}

/// `ApplyMaterialSwap`: the undo data of the swap, one entry per changed geometry in
/// reverse scene-graph order: `source` the new material, `target` the material key the
/// property had, `colorIndex` the old lighting material's color remapping index
/// (`FLT_MAX` for effect materials). None without loaded 3D, without a swap, or when
/// nothing matches. The 3D change is not saved (ExtraMaterialSwap is untouched).
fn apply_material_swap(c: &mut NativeCall) -> Result<(), String> {
    let rename = c.bool(1);
    let items = (|| {
        let reference = self_ref(c)?;
        let swap = arg_form(c, 0)?;
        let root = objref::loaded_3d(reference)?;
        let matches = matswap::swap_matches(root, swap);
        if matches.is_empty() {
            return None;
        }
        let items: Vec<PValue> = matches
            .iter()
            .rev()
            .map(|m| {
                PValue::Struct(vec![
                    ("source".into(), PValue::String(m.target.clone())),
                    ("target".into(), PValue::String(m.key.clone())),
                    ("colorIndex".into(), PValue::Float(m.color_index)),
                ])
            })
            .collect();
        let (r, w) = (reference.form().as_ptr() as usize, swap.as_ptr() as usize);
        on_main_or_post(move || {
            if let (Some(reference), Some(swap)) = (Form::checked(r as *mut u8).and_then(Form::as_reference), Form::checked(w as *mut u8)) {
                apply_swap_to_3d(reference, swap, rename);
            }
        });
        Some(items)
    })();
    c.ret_value(&array(items.unwrap_or_default()))
}

fn get_connected_objects(c: &mut NativeCall) -> Result<(), String> {
    let links = self_ref(c).and_then(ExtraList::of).and_then(ExtraList::power_links).unwrap_or_default();
    let refs: Vec<PValue> = links
        .into_iter()
        .filter_map(|(id, _)| objref::lookup_form(id).filter(|f| f.as_reference().is_some()))
        .map(|f| PValue::Form(f.as_ptr()))
        .collect();
    c.ret_value(&array(refs))
}

// ---------------------------------------------------------------- workshop: wires, power, scrap

/// The workshop wire spline: the `WorkshopSplineObject` default object (Fallout4.esm
/// `0019ECDD`, checked by its editor ID; `BGSDefaultObject::form`).
fn default_spline() -> Option<Form> {
    let dfob = objref::lookup_form(0x0019_ECDD)?;
    let game = f4x_engine::game();
    if !game.is_instance_of(dfob.as_ptr() as usize, RE::BGSDefaultObject::RTTI) {
        return None;
    }
    // SAFETY: a live BGSDefaultObject: formEditorID (BSFixedString) and form pointer.
    let (edid, form) = unsafe {
        (mem::fixed_string(mem::read(dfob.as_ptr(), RE::BGSDefaultObject::formEditorID)), mem::read::<*mut u8>(dfob.as_ptr(), RE::BGSDefaultObject::form))
    };
    edid.is_some_and(|e| e.eq_ignore_ascii_case("WorkshopSplineObject")).then(|| Form::checked(form)).flatten()
}

/// The power link type a wire uses at `item`: the kind of its first wire connect point,
/// preferring `prefer` (2 = output at the source end, 1 = input at the target end).
fn wire_link(item: Reference, prefer: i32) -> Option<i32> {
    let root = objref::loaded_3d(item)?;
    let kinds: Vec<i32> = objref::connect_points(root).iter().filter_map(|p| objref::wire_link_type(&p.name)).collect();
    kinds.iter().copied().find(|&k| k == prefer).or_else(|| kinds.iter().copied().find(|&k| k == 0)).or_else(|| kinds.first().copied())
}

/// `AttachWire` / `CreateWire`: a new wire reference between `self` and `akRef`, linked to
/// `self`'s workshop. The reference is created synchronously, as `PlaceAtMe` does; its
/// placement, power grid and power updates run on the main thread.
fn attach_wire(c: &mut NativeCall) -> Result<(), String> {
    let wire = (|| {
        let end1 = self_ref(c)?;
        let end2 = arg_ref(c, 0)?;
        if end1 == end2 || end1.form().is_deleted() || end2.form().is_deleted() {
            return None;
        }
        let spline = arg_form(c, 1).filter(|f| f.form_type() == sig("BNDS")).or_else(default_spline)?;
        let (link1, link2) = (wire_link(end1, 2)?, wire_link(end2, 1)?);
        let keyword = objref::workshop_item_keyword();
        let workshop_ref = keyword.and_then(|kw| objref::linked_ref(end1, kw).or_else(|| objref::linked_ref(end2, kw)));
        let workshop = objref::workshop_of(end1).or_else(|| objref::workshop_of(end2));
        let wire = objref::create_reference(spline, end1)?;
        if let (Some(ws), Some(kw)) = (workshop_ref, keyword) {
            objref::set_linked_ref(wire, ws, kw);
        }
        let (e1, e2, w) = (end1.form().as_ptr() as usize, end2.form().as_ptr() as usize, wire.form().as_ptr() as usize);
        let wx = workshop.map(|(_, x)| x as usize);
        on_main_or_post(move || {
            let refs = [e1, e2, w].map(|p| Form::checked(p as *mut u8).and_then(Form::as_reference));
            if let [Some(a), Some(b), Some(w)] = refs {
                objref::connect_wire(a, link1, b, link2, w, wx.map(|x| x as *mut u8));
            }
        });
        Some(wire)
    })();
    c.ret_value(&wire.map_or(PValue::None, |w| PValue::Form(w.form().as_ptr())))
}

fn transmit_connected_power(c: &mut NativeCall) -> Result<(), String> {
    let target = self_ref(c).and_then(|r| objref::workshop_of(r).map(|(_, wx)| (r, wx)));
    if let Some((r, wx)) = target {
        let (r, wx) = (r.form().as_ptr() as usize, wx as usize);
        on_main_or_post(move || {
            if let Some(r) = Form::checked(r as *mut u8).and_then(Form::as_reference) {
                objref::transmit_power(r, wx as *mut u8);
            }
        });
    }
    c.ret_bool(target.is_some());
    Ok(())
}

/// `Scrap`: scrap `self` into `workshopRef` as workshop mode does (the engine's
/// `Workshop::ScrapReference` with that workshop as the current one). With None, the
/// workshop whose area holds `self` among the loaded cells; false (and nothing happens)
/// when there is none. True otherwise, for any reference as the workshop (one that is not
/// a workshop container receives nothing).
fn scrap(c: &mut NativeCall) -> Result<(), String> {
    let scrapped = (|| {
        let item = self_ref(c)?;
        let workshop = arg_ref(c, 0).or_else(|| objref::find_workshop(item))?;
        let (i, w) = (item.form().as_ptr() as usize, workshop.form().as_ptr() as usize);
        on_main_or_post(move || {
            let refs = [i, w].map(|p| Form::checked(p as *mut u8).and_then(Form::as_reference));
            if let [Some(item), Some(workshop)] = refs {
                objref::scrap(item, workshop);
            }
        });
        Some(())
    })();
    c.ret_bool(scrapped.is_some());
    Ok(())
}

/// Euler angles (x = pitch, y = roll, z = yaw; radians) of a unit quaternion (w, x, y, z).
fn quat_euler([w, x, y, z]: [f32; 4]) -> (f32, f32, f32) {
    let pitch = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
    let roll = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin();
    let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
    (pitch, roll, yaw)
}

fn get_connect_points(c: &mut NativeCall) -> Result<(), String> {
    let points = self_ref(c).and_then(objref::loaded_3d).map(objref::connect_points).unwrap_or_default();
    let items: Vec<PValue> = points
        .into_iter()
        .map(|p| {
            let (pitch, roll, yaw) = quat_euler(p.rotation);
            PValue::Struct(vec![
                ("parent".into(), PValue::String(p.parent)),
                ("name".into(), PValue::String(p.name)),
                ("x".into(), PValue::Float(p.translation[0])),
                ("y".into(), PValue::Float(p.translation[1])),
                ("z".into(), PValue::Float(p.translation[2])),
                ("yaw".into(), PValue::Float(yaw.to_degrees())),
                ("pitch".into(), PValue::Float(pitch.to_degrees())),
                ("roll".into(), PValue::Float(roll.to_degrees())),
                ("scale".into(), PValue::Float(p.scale)),
            ])
        })
        .collect();
    c.ret_value(&array(items))
}

// ---------------------------------------------------------------- FavoritesManager

fn get_favorites(c: &mut NativeCall) -> Result<(), String> {
    let manager = objref::favorites_manager().ok_or("no FavoritesManager")?;
    let slots: Vec<PValue> = objref::favorites(manager).iter().map(|&p| form_value(p)).collect();
    c.ret_value(&PValue::Array(slots))
}

fn set_favorites(c: &mut NativeCall) -> Result<(), String> {
    let manager = objref::favorites_manager().ok_or("no FavoritesManager")?;
    let forms = arg_forms(c, 0);
    let mut slots = [std::ptr::null_mut(); objref::FAVORITE_SLOTS];
    for (slot, form) in slots.iter_mut().zip(forms) {
        *slot = form;
    }
    objref::set_favorites(manager, slots);
    Ok(())
}

fn get_tagged_forms(c: &mut NativeCall) -> Result<(), String> {
    let manager = objref::favorites_manager().ok_or("no FavoritesManager")?;
    let forms: Vec<PValue> = objref::tagged_components(manager).into_iter().filter(|&p| Form::checked(p).is_some()).map(PValue::Form).collect();
    c.ret_value(&array(forms))
}

fn is_tagged_form(c: &mut NativeCall) -> Result<(), String> {
    let tagged = match (objref::favorites_manager(), arg_form(c, 0)) {
        (Some(manager), Some(form)) => objref::is_tagged(manager, form.as_ptr()),
        _ => false,
    };
    c.ret_bool(tagged);
    Ok(())
}

fn set_tagged(c: &mut NativeCall, tag: bool) -> Result<(), String> {
    let manager = objref::favorites_manager().ok_or("no FavoritesManager")?;
    for form in arg_forms(c, 0).into_iter().filter(|p| !p.is_null()) {
        if objref::is_tagged(manager, form) != tag {
            objref::toggle_tagged(manager, form);
        }
    }
    Ok(())
}

pub(crate) const NATIVES: &[Entry] = &[
    ("ObjectReference", "GetDisplayName", get_display_name),
    ("ObjectReference", "GetInventoryItems", get_inventory_items),
    ("ObjectReference", "GetInventoryWeight", get_inventory_weight),
    ("ObjectReference", "GetAllMods", get_all_mods),
    ("ObjectReference", "GetMaterialSwap", get_material_swap),
    ("ObjectReference", "SetMaterialSwap", set_material_swap),
    ("ObjectReference", "ApplyMaterialSwap", apply_material_swap),
    ("ObjectReference", "GetConnectedObjects", get_connected_objects),
    ("ObjectReference", "GetConnectPoints", get_connect_points),
    ("ObjectReference", "AttachWire", attach_wire),
    ("ObjectReference", "CreateWire", attach_wire),
    ("ObjectReference", "TransmitConnectedPower", transmit_connected_power),
    ("ObjectReference", "Scrap", scrap),
    ("FavoritesManager", "GetFavorites", get_favorites),
    ("FavoritesManager", "SetFavorites", set_favorites),
    ("FavoritesManager", "GetTaggedForms", get_tagged_forms),
    ("FavoritesManager", "IsTaggedForm", is_tagged_form),
    ("FavoritesManager", "AddTaggedForms", |c| set_tagged(c, true)),
    ("FavoritesManager", "RemoveTaggedForms", |c| set_tagged(c, false)),
];

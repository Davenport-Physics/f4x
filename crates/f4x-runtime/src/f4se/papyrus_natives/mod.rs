//! F4SE Papyrus natives by family, bound by name: their types come from the signature
//! inventory (`f4x-pex/data/f4se-0.6.23-signatures.json`, extracted from F4SE's `.pex`).
//!
//! Behaviour reference: CommonLibF4's MIT `F4SEStub/runtime/src/Papyrus`, checked against
//! real F4SE with `tools/f4se_natives_check.py`.

use f4x_engine::papyrus::Handler;

mod math;
mod f4se;
mod utility;
mod forms;
mod instance_data;
mod items;
mod references;
mod ui;
pub(crate) mod events;

/// (script, function, handler).
pub(crate) type Entry = (&'static str, &'static str, Handler);

/// The signature inventory, parsed once.
pub(crate) fn signatures() -> &'static serde_json::Value {
    static DB: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    DB.get_or_init(|| serde_json::from_str(include_str!("../../../../f4x-pex/data/f4se-0.6.23-signatures.json")).unwrap_or_default())
}

/// The declared signature of `script.name`: (params as (type, name), return type, global).
pub(crate) fn signature(script: &str, name: &str) -> Option<(Vec<(String, String)>, String, bool)> {
    let scripts = signatures()["scripts"].as_array()?;
    let sc = scripts.iter().find(|s| s["script"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(script)))?;
    let f = sc["functions"].as_array()?.iter().find(|f| f["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))?;
    let params = f["params"]
        .as_array()?
        .iter()
        .filter_map(|p| Some((p[0].as_str()?.to_owned(), p[1].as_str()?.to_owned())))
        .collect();
    Some((params, f["returns"].as_str().unwrap_or("None").to_owned(), f["global"].as_bool().unwrap_or(false)))
}

/// The F4SE natives that F4SE 0.6.23 binds as latent (black-box: its native catalog,
/// `catalog.natives` under F4SE): the calling script waits while they run on the main
/// thread. f4x binds the same ones latent (`f4x_engine::papyrus::enable_latent`), run at the start
/// of the VM's frame update as F4SE does (`lifecycle::install_latent_runner`).
const LATENT: &[(&str, &str)] = &[
    ("ObjectReference", "AttachWire"),
    ("ObjectReference", "GetInventoryItems"),
    ("ObjectReference", "GetConnectPoints"),
    ("ObjectReference", "TransmitConnectedPower"),
    ("ObjectReference", "ApplyMaterialSwap"),
    ("ObjectReference", "Scrap"),
    ("UI", "Set"),
    ("UI", "Get"),
    ("UI", "Invoke"),
    ("UI", "Load"),
    ("FavoritesManager", "GetTaggedForms"),
    ("FavoritesManager", "AddTaggedForms"),
    ("FavoritesManager", "RemoveTaggedForms"),
    ("FavoritesManager", "IsTaggedForm"),
    ("FavoritesManager", "GetFavorites"),
    ("FavoritesManager", "SetFavorites"),
];

pub(crate) fn is_latent(script: &str, name: &str) -> bool {
    LATENT.iter().any(|(s, n)| s.eq_ignore_ascii_case(script) && n.eq_ignore_ascii_case(name))
}

/// Every implemented family.
pub(crate) fn all() -> Vec<Entry> {
    let mut v = Vec::new();
    v.extend_from_slice(math::NATIVES);
    v.extend_from_slice(f4se::NATIVES);
    v.extend_from_slice(utility::NATIVES);
    v.extend_from_slice(forms::NATIVES);
    v.extend_from_slice(instance_data::NATIVES);
    v.extend_from_slice(items::NATIVES);
    v.extend_from_slice(references::NATIVES);
    v.extend_from_slice(ui::NATIVES);
    v.extend_from_slice(events::NATIVES);
    v
}

/// Form-backed scripts F4SE adds, by the record type whose forms they wrap. The game
/// registers script names for most form types itself (e.g. `WATR` -> `WaterType`), but
/// not all: observed under f4x, `MSWP` forms had no script type while F4SE answers
/// `MatSwap` calls on them. Each is registered only where the VM has no name yet.
const FORM_SCRIPTS: &[(&str, &str)] =
    &[("ARMA", "ArmorAddon"), ("DFOB", "DefaultObject"), ("EQUP", "EquipSlot"), ("MSWP", "MatSwap"), ("WATR", "WaterType")];

/// Register the [`FORM_SCRIPTS`] the VM lacks.
pub(crate) fn register_form_scripts(vm: f4x_engine::papyrus::Vm) -> Vec<String> {
    let mut out = Vec::new();
    for (signature, script) in FORM_SCRIPTS {
        let Some(ty) = f4x_engine::form_type::FormType::from_signature(signature) else { continue };
        let id = u32::from(ty.0);
        match vm.script_for_type(id) {
            Some(existing) => out.push(format!("{signature}={existing} (game)")),
            None if vm.register_object_type(id, script) => out.push(format!("{signature}={script} (registered)")),
            None => out.push(format!("{signature}: registering {script} failed")),
        }
    }
    out
}

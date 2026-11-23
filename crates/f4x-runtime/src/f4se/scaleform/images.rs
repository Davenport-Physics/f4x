//! `root.f4se.MountImage(menuName, path, mountName)` and `UnmountImage(menuName, path)`:
//! `.dds` textures shown through `img://<mountName>` (e.g. in a `flash.display.Loader`).
//!
//! Black-box (F4SE 0.6.23, sessions 1795437790/1795438622/1795439564/1795440107, the
//! game's image loader map read by our probe; `provenance/scaleform-features.jsonl`):
//! - mounting loads the texture with the game's texture loader (`path` is relative to
//!   `Data\Textures`, as the loader adds `textures\` unless the path starts with it),
//!   renames it to `mountName` and registers it like the game's BookMenu does, so
//!   `img://mountName` (case-insensitive) loads it;
//! - known difference: for a missing file F4SE gets the engine's shared 1x1 placeholder
//!   texture and mounts that (so `img://` then shows 1x1, and every missing path is the
//!   same texture). The loader call BookMenu uses, which f4x makes, returns no texture
//!   for a missing file, so f4x mounts nothing (the call still returns true);
//! - each `menuName` (any string, case-sensitive; the menu need not exist) keeps its own
//!   set of mounted textures. Mounting a texture that set already holds does nothing and
//!   returns true; otherwise the result is the registration's (false for a texture the
//!   renderer cannot use, which stays in the set);
//! - `UnmountImage` loads `path` the same way and unregisters that texture from the
//!   menu's set: true if it was there. Since the loader caches textures, paths match
//!   case-insensitively (under F4SE an unknown path finds the placeholder texture);
//! - with an empty path nothing is mounted (true). F4SE crashes with fewer than three
//!   arguments (MountImage) or non-string ones; f4x returns false.

use std::collections::HashMap;
use std::sync::Mutex;

use f4x_engine::scaleform::{Call, Value};
use f4x_engine::scaleform_ext::{ExternalTexture, load_texture};

/// Menu name -> (texture, its registration).
static MOUNTS: Mutex<Option<HashMap<String, Vec<(usize, ExternalTexture)>>>> = Mutex::new(None);

fn strings<const N: usize>(call: &Call) -> Option<[String; N]> {
    let mut out: [String; N] = std::array::from_fn(|_| String::new());
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = call.args.get(i).and_then(Value::as_string)?;
    }
    Some(out)
}

pub(super) fn mount_image(call: &mut Call) {
    let Some([menu, path, name]) = strings::<3>(call) else {
        call.ret(Value::bool(false));
        return;
    };
    let ok = mount(&menu, &path, &name);
    log::debug!(target: "f4x::scaleform", "MountImage({menu:?}, {path:?}, {name:?}) -> {ok}");
    call.ret(Value::bool(ok));
}

fn mount(menu: &str, path: &str, name: &str) -> bool {
    let Some(texture) = load_texture(path) else { return true };
    let Ok(mut mounts) = MOUNTS.lock() else { return false };
    let set = mounts.get_or_insert_with(HashMap::new).entry(menu.to_owned()).or_default();
    let key = texture.as_ptr() as usize;
    if set.iter().any(|(t, _)| *t == key) {
        return true;
    }
    log::debug!(target: "f4x::scaleform", "MountImage: texture {key:#x} named {:?} -> {name:?}", texture.name());
    texture.set_name(name);
    let mut registration = ExternalTexture::default();
    let ok = registration.set(&texture);
    set.push((key, registration));
    ok
}

pub(super) fn unmount_image(call: &mut Call) {
    let Some([menu, path]) = strings::<2>(call) else {
        call.ret(Value::bool(false));
        return;
    };
    let ok = unmount(&menu, &path);
    log::debug!(target: "f4x::scaleform", "UnmountImage({menu:?}, {path:?}) -> {ok}");
    call.ret(Value::bool(ok));
}

fn unmount(menu: &str, path: &str) -> bool {
    let Some(texture) = load_texture(path) else { return false };
    let key = texture.as_ptr() as usize;
    let removed = MOUNTS.lock().ok().and_then(|mut mounts| {
        let set = mounts.as_mut()?.get_mut(menu)?;
        let i = set.iter().position(|(t, _)| *t == key)?;
        Some(set.remove(i))
    });
    drop(texture);
    match removed {
        Some((_, mut registration)) => {
            registration.release();
            true
        }
        None => false,
    }
}

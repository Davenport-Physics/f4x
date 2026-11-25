//! Scaleform translation injection: each active plugin's
//! `Interface\Translations\<plugin>_<language>.txt` is added to the game's translation
//! table (F4SE changelog 0.3.1), so `$KEY` strings in mods' menus resolve.
//!
//! Where: the BSScaleformManager constructor parses the game's own
//! `Interface\Translate_<language>.txt` into a new `BSScaleformTranslator` and only then
//! hands the translator to the GFx loader (our disassembly of 1.10.163, 0x2110430; see
//! `f4x_engine::translation`). f4x redirects that one parse call: after the game's file,
//! it adds the plugins' files (and enables the Scaleform log, see `gfx_log`). This is
//! before the data handler exists and before any movie loads.
//!
//! Black-box facts (F4SE 0.6.23, `provenance/compat-features.jsonl`):
//! - the plugins are the active (`*`) lines of `plugins.txt`, in file order, whether or
//!   not the plugin file exists; official masters count only when listed; files of
//!   plugins that are not listed or not active are never read;
//! - the file name is the plugin name without its extension, `_`, then
//!   `sLanguage:General` (`F4XTP1.esp` → `F4XTP1_fr.txt` with `sLanguage=fr`);
//! - the file is found like a game resource: loose, or inside an archive registered by
//!   then (a startup archive works; a plugin's own `- Main.ba2` is not registered yet);
//! - each file read logs `Reading translations from Interface\Translations\<file>...`;
//!   a file not starting with the UTF-16LE BOM logs `BOM Error, file must be encoded in
//!   UCS-2 LE.` and is skipped (UTF-8, with or without BOM, and BOM-less UTF-16);
//! - lines end at LF (a CR before it is dropped; a lone CR is text) or after 511 UTF-16
//!   units (the rest of a longer line is read as the next line); a line counts only if it
//!   starts with `$` and has a TAB: the key is everything before the last TAB, the value
//!   everything after (may be empty, trailing spaces kept);
//! - existing keys are never replaced: the game's own keys win, and of two mod files the
//!   one earlier in `plugins.txt` wins (within a file, the first line wins).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use f4x_engine::translation;
use f4x_engine_sys::addresses::SCALEFORM_TRANSLATOR_LOAD;
use f4x_pe::Image;

static LOAD: AtomicUsize = AtomicUsize::new(0);

type Load = unsafe extern "C" fn(usize, usize, usize, usize) -> usize;

/// UTF-16 units read per line (black-box: a 600-character value came back cut so that
/// key, TAB and value made 511 units).
const LINE_UNITS: usize = 511;

/// The game folder (the exe's directory).
pub(super) fn game_dir() -> Option<PathBuf> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    let mut buf = [0u16; 1024];
    // SAFETY: valid buffer; null = the game exe.
    let len =
        unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (len > 0)
        .then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])).parent().map(PathBuf::from))
        .flatten()
}

pub(super) fn install() {
    let Some(load) = f4x_engine::address::resolve(&SCALEFORM_TRANSLATOR_LOAD) else {
        log::warn!(target: "f4x::f4se", "translation loader not found; no translation injection");
        return;
    };
    LOAD.store(load, Relaxed);
    let game = f4x_engine::game();
    let Some(text) = game.image().section(".text").cloned() else { return };
    let sites = super::lifecycle::redirect_calls_in(
        game.base() + text.rva as usize,
        text.readable_size as usize,
        load,
        load_hook as Load as usize,
    );
    log::info!(target: "f4x::f4se", "translation loader hooked ({sites} site)");
}

unsafe extern "C" fn load_hook(translator: usize, reader: usize, c: usize, d: usize) -> usize {
    // SAFETY: the engine's own function with its own arguments.
    let result = unsafe { std::mem::transmute::<usize, Load>(LOAD.load(Relaxed))(translator, reader, c, d) };
    let _ = std::panic::catch_unwind(|| {
        if let Some(loader) = translation::loader() {
            super::gfx_log::on_scaleform_manager(loader);
        }
        inject(translator as *mut u8);
    });
    result
}

/// `%LOCALAPPDATA%\Fallout4\plugins.txt`.
fn plugins_txt() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};
    let mut path: *mut u16 = std::ptr::null_mut();
    // SAFETY: standard shell call; the buffer is freed below.
    unsafe {
        if SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut path) < 0
            || path.is_null()
        {
            return None;
        }
        let len = (0..).take_while(|&i| *path.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
        CoTaskMemFree(path.cast());
        Some(PathBuf::from(text).join("Fallout4").join("plugins.txt"))
    }
}

/// The active (`*`) entries of a `plugins.txt`, in order.
#[must_use]
pub(super) fn active_plugins(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix('*'))
        .map(|name| name.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect()
}

/// `Interface\Translations\<plugin without extension>_<language>.txt`.
#[must_use]
pub(super) fn translation_file(plugin: &str, language: &str) -> String {
    let stem = plugin.rsplit_once('.').map_or(plugin, |(stem, _)| stem);
    format!("Interface\\Translations\\{stem}_{language}.txt")
}

/// The `(key, value)` lines of a translation file without its BOM (see the module docs).
#[must_use]
pub(super) fn parse(units: &[u16]) -> Vec<(Vec<u16>, Vec<u16>)> {
    const LF: u16 = b'\n' as u16;
    const CR: u16 = b'\r' as u16;
    let mut out = Vec::new();
    let mut rest = units;
    while !rest.is_empty() {
        let window = &rest[..rest.len().min(LINE_UNITS)];
        let len = window.iter().position(|&u| u == LF).map_or(window.len(), |i| i + 1);
        let (mut line, next) = rest.split_at(len);
        rest = next;
        if let Some(stripped) = line.strip_suffix(&[LF]) {
            line = stripped.strip_suffix(&[CR]).unwrap_or(stripped);
        }
        if line.first() != Some(&u16::from(b'$')) {
            continue;
        }
        let Some(tab) = line.iter().rposition(|&u| u == u16::from(b'\t')) else { continue };
        out.push((line[..tab].to_vec(), line[tab + 1..].to_vec()));
    }
    out
}

fn inject(translator: *mut u8) {
    if !f4x_engine::game().is_instance_of(translator as usize, translation::TRANSLATOR_RTTI) {
        log::warn!(target: "f4x::f4se", "translation loader called on something else; skipped");
        return;
    }
    let Some(plugins) = plugins_txt().and_then(|p| std::fs::read(p).ok()) else {
        log::info!(target: "f4x::f4se", "translations: no plugins.txt, so no plugin translations");
        return;
    };
    let language = translation::ini_string("sLanguage:General").unwrap_or_else(|| "en".to_owned());
    let mut known: HashSet<String> = translation::entries(translator).into_iter().map(|(k, _)| k).collect();
    let before = known.len();
    for plugin in active_plugins(&String::from_utf8_lossy(&plugins)) {
        let file = translation_file(&plugin, &language);
        let Some(bytes) = translation::read_resource(&file) else { continue };
        log::info!(target: "f4x::f4se", "Reading translations from {file}...");
        let Some(body) = bytes.strip_prefix(&[0xFF, 0xFE]) else {
            log::warn!(target: "f4x::f4se", "BOM Error, file must be encoded in UCS-2 LE. ({file})");
            continue;
        };
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        for (key, value) in parse(&units) {
            let key = String::from_utf16_lossy(&key);
            if known.contains(&key) {
                continue;
            }
            if translation::insert(translator, &key, &String::from_utf16_lossy(&value)) {
                known.insert(key);
            }
        }
    }
    log::info!(target: "f4x::f4se", "translations ({language}): {before} -> {} keys", known.len());
}

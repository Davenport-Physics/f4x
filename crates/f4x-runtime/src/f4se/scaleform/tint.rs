//! The tint hook (F4SE 0.1.4: "hook global scaleform tint functions to allow interface
//! mods to selectively modify tint colors/intensities").
//!
//! Our RE (`provenance/scaleform-features.jsonl`): every tint of a menu element goes
//! through one game function, `BSGFxShaderFXTarget`'s apply-color (`(this, const NiColor&,
//! float brightness)`), reached from `SetToHUDColor`, `CreateAndSetFiltersToHUD`, both
//! `CreateAndSetFiltersToColor` overloads and the `ApplyColorUpdateEvent` sink (5 call
//! sites, no pointers to it). It stores the color and brightness and sets the element's
//! color transform to color x brightness. The element is the GFx value at +8 (also on NG
//! 1.11.240: generated NG tables, `BSGFxDisplayObject` has its `BSGFxObject` base, whose
//! first base is the `GFx::Value`, at +8).
//!
//! Black-box (F4SE 0.6.23 patches that function's entry; sessions 1795439564 and
//! 1795440107, probe wrapping its call sites and tracing GFx calls):
//! - before the original runs, the element is asked `HasMember("onApplyColorChange")`;
//!   if it has one, it is invoked (no type check) with four Numbers: red, green, blue,
//!   brightness;
//! - when the result is an array of at least four elements, its first three replace
//!   the color and the fourth the brightness, unclamped (`[0.5, 0.25, 0.125, 0.5]` gave
//!   multipliers 0.25, 0.125, 0.0625); anything else (undefined, numbers, booleans,
//!   strings, objects, shorter arrays) changes nothing;
//! - then the original applies the (possibly new) color.
//!
//! f4x redirects the 5 call sites to the same logic. Array elements that are not numbers
//! are untested under F4SE; f4x then ignores the result.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use f4x_engine::scaleform::{Value, kind};
use f4x_engine_sys::addresses::SHADER_FX_TARGET_APPLY_COLOR;

const CALLBACK: &str = "onApplyColorChange";

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

type ApplyColor = unsafe extern "C" fn(*mut u8, *const [f32; 3], f32);

pub(super) fn install() {
    let Some(target) = f4x_engine::address::resolve(&SHADER_FX_TARGET_APPLY_COLOR) else {
        log::warn!(target: "f4x::scaleform", "tint function not found; onApplyColorChange disabled");
        return;
    };
    ORIGINAL.store(target, Relaxed);
    let sites = super::super::lifecycle::redirect_calls_and_jumps(target, apply_color_hook as ApplyColor as usize);
    log::info!(target: "f4x::scaleform", "tint hook: {sites} call site(s)");
}

unsafe extern "C" fn apply_color_hook(this: *mut u8, color: *const [f32; 3], brightness: f32) {
    // SAFETY: the engine's NiColor argument.
    let mut rgb = unsafe { *color };
    let mut level = brightness;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: a BSGFxShaderFXTarget; its GFx::Value lives at +8 for the call.
        let element = unsafe { &*(this.add(8) as *const Value) };
        if let Some([r, g, b, l]) = ask(element, rgb, level) {
            rgb = [r, g, b];
            level = l;
        }
    }));
    // SAFETY: the original with the engine's target and our (possibly new) color.
    unsafe { std::mem::transmute::<usize, ApplyColor>(ORIGINAL.load(Relaxed))(this, &rgb, level) };
}

/// The element's `onApplyColorChange(r, g, b, brightness)` answer, when it gives one.
fn ask(element: &Value, rgb: [f32; 3], level: f32) -> Option<[f32; 4]> {
    if !element.is_object() || !element.has_member(CALLBACK) {
        return None;
    }
    let args = [rgb[0], rgb[1], rgb[2], level].map(|c| Value::number(f64::from(c)));
    let result = element.invoke(CALLBACK, &args)?;
    if result.kind() != kind::ARRAY || result.length()? < 4 {
        return None;
    }
    let mut out = [0f32; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = result.element(i)?.as_number()? as f32;
    }
    Some(out)
}

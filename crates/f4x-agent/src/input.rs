//! Engine-level input: observe the input queue and inject synthesized button events.
//!
//! Each frame the engine hands its input queue (a linked list of `InputEvent`s) to the
//! `BSInputEventReceiver`s: `MenuControls` routes events to menus, `PlayerControls` to
//! gameplay. f4x hooks `PerformInputProcessing` (vtable slot 0) on both. Events queued
//! with [`press`] are prepended to the list as real `ButtonEvent`s (the game's vtable,
//! CommonLibF4 layout), and the same objects go to both receivers within a frame, so a
//! menu that handles an event also marks it handled for gameplay, as with real input.
//! Observers (e.g. F4SE control registrations) see the list `MenuControls` gets.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use f4x_engine::layout::RE;
use f4x_engine::papyrus::FixedString;
use serde_json::{Value, json};

use crate::{clock, hooks};

type PerformInputProcessing = unsafe extern "C" fn(*mut u8, *const u8);

static REAL_MENU: AtomicUsize = AtomicUsize::new(0);
static REAL_PLAYER: AtomicUsize = AtomicUsize::new(0);
static BUTTON_VTABLE: AtomicUsize = AtomicUsize::new(0);

/// `INPUT_DEVICE` (same on NG 1.11.240: next-gen fork `INPUT_DEVICE.h`).
pub mod device {
    pub const KEYBOARD: i32 = 0;
    pub const MOUSE: i32 = 1;
    pub const GAMEPAD: i32 = 2;
}

/// `ButtonEvent` (0x40): `InputEvent` + `IDEvent` + value/heldDownSecs.
#[repr(C)]
struct ButtonEvent {
    vtbl: usize,
    device: i32,
    device_id: i32,
    event_type: i32,
    _pad: u32,
    next: *mut ButtonEvent,
    time_code: u32,
    handled: i32,
    user_event: *mut u8,
    id_code: u32,
    disabled: u32,
    value: f32,
    held: f32,
}

const _: () = {
    assert!(std::mem::size_of::<ButtonEvent>() == RE::ButtonEvent::SIZE);
    assert!(std::mem::offset_of!(ButtonEvent, next) == RE::InputEvent::next);
    assert!(std::mem::offset_of!(ButtonEvent, user_event) == RE::IDEvent::strUserEvent);
    assert!(std::mem::offset_of!(ButtonEvent, id_code) == RE::IDEvent::idCode);
    assert!(std::mem::offset_of!(ButtonEvent, value) == RE::ButtonEvent::value);
    assert!(std::mem::offset_of!(ButtonEvent, held) == RE::ButtonEvent::heldDownSecs);
};

struct Scheduled {
    frame: u64,
    user_event: String,
    id_code: u32,
    device: i32,
    value: f32,
    held: f32,
}

struct State {
    pending: Vec<Scheduled>,
    /// Events built for the current frame: (frame, chain head, chain tail).
    current: Option<(u64, usize, usize)>,
    /// Event objects, reused round-robin (long after the frame that used them).
    pool: Vec<Box<ButtonEvent>>,
    next_slot: usize,
    recent: VecDeque<Value>,
    observers: Vec<fn(*const u8)>,
}

// SAFETY: the raw pointers are engine objects used on the main thread only.
unsafe impl Send for State {}

static STATE: Mutex<State> =
    Mutex::new(State { pending: Vec::new(), current: None, pool: Vec::new(), next_slot: 0, recent: VecDeque::new(), observers: Vec::new() });

const POOL: usize = 64;

/// Queue a press of a user event (`Accept`, `Activate`, `WorkshopMode`, ...): down on the
/// next frame, up `hold_frames` frames later. `id_code` is the key (DirectInput scan code
/// for the keyboard, e.g. 0x1C Enter).
pub fn press(user_event: &str, id_code: u32, device: i32, hold_frames: u32) -> u64 {
    press_with(user_event, id_code, device, hold_frames, false)
}

/// [`press`], optionally with a held event on every frame in between (`repeat`: value 1,
/// `heldDownSecs` growing by a 60 fps frame), as a physical button sends while held.
pub fn press_with(user_event: &str, id_code: u32, device: i32, hold_frames: u32, repeat: bool) -> u64 {
    let start = clock::frames() + 1;
    let hold = hold_frames.max(1);
    if let Ok(mut state) = STATE.lock() {
        let base = Scheduled { frame: start, user_event: user_event.to_owned(), id_code, device, value: 1.0, held: 0.0 };
        state.pending.push(Scheduled { frame: start + u64::from(hold), value: 0.0, held: hold as f32 / 60.0, user_event: base.user_event.clone(), ..base });
        if repeat {
            for i in 1..hold {
                state.pending.push(Scheduled { frame: start + u64::from(i), held: i as f32 / 60.0, user_event: base.user_event.clone(), ..base });
            }
        }
        state.pending.push(base);
    }
    start
}

/// Call `f` with the head of each frame's input list (main thread).
pub fn add_observer(f: fn(*const u8)) {
    if let Ok(mut state) = STATE.lock() {
        state.observers.push(f);
    }
}

/// The most recent button events seen (user event, key, value, held).
pub fn recent() -> Vec<Value> {
    STATE.lock().map(|s| s.recent.iter().cloned().collect()).unwrap_or_default()
}

pub fn install() {
    let game = f4x_engine::game();
    BUTTON_VTABLE.store(game.vtable(RE::ButtonEvent::RTTI).unwrap_or(0), Relaxed);
    for (class, hook, real) in [
        (RE::MenuControls::RTTI, menu_hook as PerformInputProcessing, &REAL_MENU),
        (RE::PlayerControls::RTTI, player_hook as PerformInputProcessing, &REAL_PLAYER),
    ] {
        let Some(vtable) = game.vtable(class) else {
            log::warn!(target: "f4x::input", "{class}: vtable not found");
            continue;
        };
        let slot = (vtable + 8 * RE::BSInputEventReceiver::vtbl::PerformInputProcessing) as *mut usize;
        // SAFETY: slot 0 (PerformInputProcessing) of a BSInputEventReceiver's primary vtable.
        match unsafe { hooks::patch_pointer(slot, hook as usize) } {
            Ok(original) => real.store(original, Relaxed),
            Err(error) => log::warn!(target: "f4x::input", "{class}: {error}"),
        }
    }
}

/// This frame's list: our due events in front of the engine's `head`.
fn chain(head: *const u8, observe: bool) -> *const u8 {
    let Ok(mut state) = STATE.lock() else { return head };
    let frame = clock::frames();
    let first = match state.current {
        Some((f, first, last)) if f == frame => {
            // SAFETY: our pooled event; re-link to this receiver's head.
            unsafe { (*(last as *mut ButtonEvent)).next = head as *mut ButtonEvent };
            first as *const u8
        }
        _ => {
            let vtbl = BUTTON_VTABLE.load(Relaxed);
            let (due, rest): (Vec<Scheduled>, Vec<Scheduled>) = std::mem::take(&mut state.pending).into_iter().partition(|s| s.frame <= frame);
            state.pending = rest;
            if due.is_empty() || vtbl == 0 {
                state.current = None;
                head
            } else {
                let mut next = head as *mut ButtonEvent;
                let mut last = 0usize;
                for s in due.iter().rev() {
                    let event = take_slot(&mut state);
                    // SAFETY: a pooled event we own.
                    unsafe {
                        f4x_engine::papyrus::release_entry(&raw mut (*event).user_event);
                        event.write(ButtonEvent {
                            vtbl,
                            device: s.device,
                            device_id: 0,
                            event_type: 0, // kButton
                            _pad: 0,
                            next,
                            time_code: u32::MAX,
                            handled: 0,
                            user_event: FixedString::new(&s.user_event).map_or(std::ptr::null_mut(), FixedString::into_raw),
                            id_code: s.id_code,
                            disabled: 0,
                            value: s.value,
                            held: s.held,
                        });
                    }
                    if last == 0 {
                        last = event as usize;
                    }
                    next = event;
                }
                state.current = Some((frame, next as usize, last));
                log::debug!(target: "f4x::input", "injected {} event(s)", due.len());
                next as *const u8
            }
        }
    };
    if observe {
        record(&mut state, first);
        let observers = state.observers.clone();
        drop(state);
        for f in observers {
            let _ = std::panic::catch_unwind(|| f(first));
        }
    }
    first
}

fn take_slot(state: &mut State) -> *mut ButtonEvent {
    if state.pool.len() < POOL {
        // SAFETY: an all-zero ButtonEvent is a valid (null) value.
        state.pool.push(Box::new(unsafe { std::mem::zeroed() }));
        return &raw mut **state.pool.last_mut().unwrap();
    }
    let i = state.next_slot % POOL;
    state.next_slot = state.next_slot.wrapping_add(1);
    &raw mut *state.pool[i]
}

fn record(state: &mut State, head: *const u8) {
    let mut event = head;
    for _ in 0..64 {
        if event.is_null() || !f4x_engine::mem::readable(event as usize, RE::ButtonEvent::SIZE) {
            break;
        }
        // SAFETY: a live InputEvent, readable for a ButtonEvent's size.
        unsafe {
            if f4x_engine::mem::read::<i32>(event, RE::InputEvent::eventType) == 0 {
                let name = f4x_engine::mem::fixed_string(f4x_engine::mem::read(event, RE::IDEvent::strUserEvent)).unwrap_or_default();
                let value: f32 = f4x_engine::mem::read(event, RE::ButtonEvent::value);
                let held: f32 = f4x_engine::mem::read(event, RE::ButtonEvent::heldDownSecs);
                if value == 0.0 || held == 0.0 {
                    state.recent.push_back(json!({
                        "frame": clock::frames(),
                        "event": name,
                        "key": f4x_engine::mem::read::<u32>(event, RE::IDEvent::idCode),
                        "device": f4x_engine::mem::read::<i32>(event, RE::InputEvent::device),
                        "down": value != 0.0,
                        "held": held,
                    }));
                    while state.recent.len() > 64 {
                        state.recent.pop_front();
                    }
                }
            }
            event = f4x_engine::mem::read(event, RE::InputEvent::next);
        }
    }
}

unsafe extern "C" fn menu_hook(this: *mut u8, head: *const u8) {
    let head = std::panic::catch_unwind(|| chain(head, true)).unwrap_or(head);
    // SAFETY: the original slot value.
    unsafe { std::mem::transmute::<usize, PerformInputProcessing>(REAL_MENU.load(Relaxed))(this, head) };
}

unsafe extern "C" fn player_hook(this: *mut u8, head: *const u8) {
    let head = std::panic::catch_unwind(|| chain(head, false)).unwrap_or(head);
    // SAFETY: the original slot value.
    unsafe { std::mem::transmute::<usize, PerformInputProcessing>(REAL_PLAYER.load(Relaxed))(this, head) };
}

/// RPC: `input.press {event, key?, device?, hold_frames?, repeat?}`, `input.recent`.
pub fn register_rpc() {
    use f4x_proto::{RpcError, code};
    crate::server::register("input.press", |_, p| {
        let event = p
            .get("event")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "params.event: a user event, e.g. Accept, Activate, WorkshopMode"))?
            .to_owned();
        let key = p.get("key").and_then(Value::as_u64).unwrap_or(0) as u32;
        let device = match p.get("device").and_then(Value::as_str).unwrap_or("keyboard") {
            "keyboard" => device::KEYBOARD,
            "mouse" => device::MOUSE,
            "gamepad" => device::GAMEPAD,
            other => return Err(RpcError::new(code::INVALID_PARAMS, format!("params.device: keyboard|mouse|gamepad, not {other:?}"))),
        };
        let hold = p.get("hold_frames").and_then(Value::as_u64).unwrap_or(2) as u32;
        let repeat = p.get("repeat").and_then(Value::as_bool).unwrap_or(false);
        if BUTTON_VTABLE.load(Relaxed) == 0 || REAL_MENU.load(Relaxed) == 0 {
            return Err(RpcError::new(code::INTERNAL, "input injection unavailable (hooks not installed)"));
        }
        let frame = press_with(&event, key, device, hold, repeat);
        log::info!(target: "f4x::input", "press {event} (key {key:#x}) at frame {frame}");
        Ok(json!({"ok": true, "frame": frame}))
    });
    crate::server::register("input.recent", |_, _| Ok(json!(recent())));
    crate::server::register("input.state", |_, _| {
        Ok(json!({"text_entry_count": f4x_engine::controls::text_entry_count()}))
    });
}

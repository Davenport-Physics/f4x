//! UI: menus by name and their Scaleform movies.
//!
//! Behaviour reference: CommonLibF4 `F4SEStub/runtime/src/Papyrus/UI.h` (MIT) for the
//! menu functions; the movie functions (`Get`, `Set`, `Invoke`, `Load`,
//! `RegisterCustomMenu`) are unimplemented there and were black-boxed against F4SE 0.6.23
//! (`papyrus.call` + `ui.has`, sessions of 2026-11-22; `provenance/natives-ui.jsonl`):
//! - `IsMenuRegistered`: the UI's menu map has the name (open or not). `IsMenuOpen`: the
//!   menu exists and is on the menu stack. `OpenMenu`/`CloseMenu`: true when the name is
//!   registered (whether or not the menu is open), after queueing kShow/kHide;
//! - `Get`/`Set`/`Invoke` need the menu to exist (else None / false) and a non-empty path;
//!   paths are ActionScript paths (`root.f4se.version.major`), case-sensitive;
//! - Scaleform to Papyrus: bool, int/uint (Int), number (Float), string; arrays become
//!   `Var[]` (each element boxed); display objects None; anything else (objects,
//!   functions, null, undefined, a path that does not resolve) an empty `Var`. Objects
//!   made from forms (below) come back as the form;
//! - Papyrus to Scaleform: Bool, Int (int), Float (number), String, arrays (AS arrays),
//!   forms and script objects (`{__type__: script, __handleHigh__, __handleLow__}`). F4SE
//!   crashes on an empty `Var`; f4x sets nothing and returns false;
//! - `Invoke(menu, path, args)` calls the function at `path` through the movie root
//!   (`root.f4se.GetDirectoryListing`, `root.getChildAt`, `String`); class statics
//!   (`Math.max`) and array methods give an empty `Var`. F4SE crashes on
//!   `Object.prototype` methods by path (`root.hasOwnProperty`); f4x returns an empty
//!   `Var` for those names;
//! - `RegisterCustomMenu(name, swf, root, data)`: fails if the name is registered; the
//!   menu (`ui::CustomMenu`, a `GameMenuBase`) loads `Interface/<swf>.swf` when opened,
//!   with `root` as its code object, `menuFlags` as its flags, `depth` as its depth
//!   priority and `movieFlags` as the movie's scale mode (0 noScale, 1 showAll,
//!   2 exactFit, 3 noBorder; other values read showAll, as GFx maps them). A missing SWF
//!   still opens (an empty menu). `extendedFlags` showed no effect we can observe and is
//!   ignored. `RegisterBasicCustomMenu` uses kPausesGame | kUsesCursor and noBorder;
//! - `Load(menu, sourceVar, asset, receiver, callback)`: false when the menu has no movie
//!   or `sourceVar` ("" = root) does not resolve; else a `flash.display.Loader` loads
//!   `asset` (relative to the movie), is added to `sourceVar` on success, and
//!   `receiver.callback(Bool ok, String menu, String sourceVar, String loadedPath,
//!   String asset)` is called, `loadedPath` being the content's display path
//!   (`root1.instance83.instance82`), "" on failure.
//!
//! The callback goes to the receiver's own script object (the script that called
//! `Load`), as the argument carries it.

use f4x_engine::papyrus::{Arg, Vm};
use f4x_engine::papyrus_value::PValue;
use f4x_engine::scaleform::{Movie, Value, kind};
use f4x_engine::ui;

use super::Entry;

/// The value at a dotted path: the first segment through `GetVariable`, the rest member
/// by member (deep `GetVariable` paths can fault inside Scaleform); numeric segments of
/// arrays are elements (`root.scenes.0.name`).
fn resolve(movie: Movie, path: &str) -> Option<Value> {
    let mut parts = path.split('.');
    let mut value = movie.get_variable(parts.next()?)?;
    for part in parts {
        if value.kind() == kind::ARRAY
            && let Ok(i) = part.parse::<usize>()
        {
            if i >= value.length()? {
                return None;
            }
            value = value.element(i)?;
            continue;
        }
        if !value.is_object() || part.is_empty() || !value.has_member(part) {
            return None;
        }
        value = value.get_member(part)?;
    }
    Some(value)
}

/// `Object.prototype` methods: Scaleform faults when they are invoked by path (F4SE
/// crashes on `root.hasOwnProperty` and `<object>.toString`), so f4x does not call them.
const UNSAFE_METHODS: &[&str] =
    &["hasOwnProperty", "isPrototypeOf", "propertyIsEnumerable", "setPropertyIsEnumerable", "toString", "toLocaleString", "valueOf", "constructor"];

fn empty_var() -> PValue {
    PValue::Var(Box::new(PValue::None))
}

/// Members of an object made from a Papyrus object (black-box, F4SE 0.6.23).
const TYPE_MEMBER: &str = "__type__";
const HANDLE_HIGH: &str = "__handleHigh__";
const HANDLE_LOW: &str = "__handleLow__";

/// A Scaleform value as F4SE hands it to Papyrus (before boxing into the `Var` return).
fn to_pvalue(v: &Value, depth: u32) -> PValue {
    match v.kind() {
        kind::BOOL => PValue::Bool(v.as_bool().unwrap_or(false)),
        kind::INT | kind::UINT => PValue::Int(v.as_number().unwrap_or(0.0) as i64 as i32),
        kind::NUMBER => PValue::Float(v.as_number().unwrap_or(0.0) as f32),
        kind::STRING => PValue::String(v.as_string().unwrap_or_default()),
        kind::ARRAY if depth < 8 => {
            let n = v.length().unwrap_or(0).min(1 << 12);
            PValue::Array((0..n).map(|i| PValue::Var(Box::new(v.element(i).map_or_else(empty_var, |e| to_pvalue(&e, depth + 1))))).collect())
        }
        kind::DISPLAY_OBJECT => PValue::None,
        kind::OBJECT => object_to_pvalue(v).unwrap_or_else(empty_var),
        _ => empty_var(),
    }
}

/// A Papyrus object stored in Scaleform: `{__type__: script, __handleHigh__,
/// __handleLow__}` (the VM handle's two halves, uints). Objects without `__type__` are
/// not ones (None here). The object is looked up when the value is returned; one that
/// is gone makes the call return None, as under F4SE.
fn object_to_pvalue(v: &Value) -> Option<PValue> {
    if !v.has_member(TYPE_MEMBER) {
        return None;
    }
    let script = v.get_member(TYPE_MEMBER)?.as_string()?;
    let half = |name: &str| v.get_member(name).and_then(|h| h.as_number()).map_or(0, |n| n as u32);
    let handle = (u64::from(half(HANDLE_HIGH)) << 32) | u64::from(half(HANDLE_LOW));
    Some(PValue::Object { handle, script })
}

/// A Papyrus value as a Scaleform value of `movie`; None for an empty value.
fn from_pvalue(vm: Vm, movie: Movie, v: &PValue, depth: u32) -> Option<Value> {
    match v {
        PValue::None => None,
        PValue::Var(inner) => from_pvalue(vm, movie, inner, depth),
        PValue::Bool(b) => Some(Value::bool(*b)),
        PValue::Int(i) => Some(Value::int(*i)),
        PValue::Float(f) => Some(Value::number(f64::from(*f))),
        PValue::String(s) => movie.create_string(s),
        PValue::Array(items) if depth < 8 => {
            let mut array = movie.create_array()?;
            for item in items {
                let value = from_pvalue(vm, movie, item, depth + 1).unwrap_or_else(Value::null);
                array.push_back(&value);
            }
            Some(array)
        }
        PValue::Struct(fields) if depth < 8 => {
            let mut object = movie.create_object()?;
            for (name, field) in fields {
                let value = from_pvalue(vm, movie, field, depth + 1).unwrap_or_else(Value::null);
                object.set_member(name, &value);
            }
            Some(object)
        }
        PValue::Form(_) | PValue::Object { .. } => {
            let (handle, script) = receiver(vm, v)?;
            let mut object = movie.create_object()?;
            object.set_member(TYPE_MEMBER, &movie.create_string(&script)?);
            object.set_member(HANDLE_HIGH, &Value::uint((handle >> 32) as u32));
            object.set_member(HANDLE_LOW, &Value::uint(handle as u32));
            Some(object)
        }
        _ => None,
    }
}

/// Run `f` with the movie of the menu `name`, if the menu exists and has one.
///
/// Threads: natives run on Papyrus VM worker threads under both hosts (measured: f4x
/// natives on threads other than the main loop's; F4SE's crash stacks in `UI.Set` and
/// `UI.Invoke` are on VM threads too). F4SE uses the movie right there, synchronously,
/// and so does f4x: waiting for the menu's next `AdvanceMovie` instead would block a VM
/// thread on the frame, which can wait on the VM. The menu is referenced for the call.
fn with_movie<R>(name: &str, f: impl FnOnce(Movie) -> R) -> Option<R> {
    ui::with_menu(name, |menu| {
        let movie = menu.movie();
        (!movie.0.is_null()).then(|| f(movie))
    })
    .flatten()
}

fn menu_data_field(data: &PValue, name: &str) -> Option<i32> {
    let PValue::Struct(fields) = data else { return None };
    fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| match v {
        PValue::Int(i) => Some(*i),
        _ => None,
    })
}

fn register(name: &str, movie: String, root: String, menu_flags: i32, movie_flags: i32, depth: i32) -> bool {
    let def = ui::CustomMenu {
        movie,
        root,
        menu_flags: menu_flags as u32,
        depth: depth.clamp(0, 255) as u8,
        scale_mode: movie_flags,
        align: None,
        on_create: None,
    };
    ui::register_custom_menu(name, def)
}

// ---------------------------------------------------------------- Load

/// A `UI.Load` in flight, until its movie reports the outcome (inside its own advance).
struct PendingLoad {
    menu: String,
    source: String,
    asset: String,
    receiver: Option<(u64, String)>,
    callback: String,
    /// One reference to the `flash.display.Loader`, released by the outcome handler
    /// (never elsewhere: the movie may be gone by then).
    loader: std::mem::ManuallyDrop<Value>,
}

// SAFETY: the loader is only touched on its movie's thread (the event handlers).
unsafe impl Send for PendingLoad {}

static LOADS: std::sync::Mutex<Vec<(usize, PendingLoad)>> = std::sync::Mutex::new(Vec::new());
static NEXT_LOAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn load_handlers() -> (&'static f4x_engine::scaleform::Function, &'static f4x_engine::scaleform::Function) {
    use f4x_engine::scaleform::Function;
    static H: std::sync::OnceLock<(&'static Function, &'static Function)> = std::sync::OnceLock::new();
    *H.get_or_init(|| (Function::leak(|c| load_finished(c.movie, c.user, true)), Function::leak(|c| load_finished(c.movie, c.user, false))))
}

/// The display object at `source` ("" = the root).
fn load_target(movie: Movie, source: &str) -> Option<Value> {
    let target = if source.is_empty() { movie.get_variable("root") } else { resolve(movie, source) };
    target.filter(|t| t.kind() == kind::DISPLAY_OBJECT)
}

/// Start loading `asset` with a `flash.display.Loader`; false when the target does not
/// exist. The outcome arrives through the loader's `complete` / `ioError` events.
fn start_load(movie: Movie, p: PendingLoad) -> bool {
    if load_target(movie, &p.source).is_none() {
        return false;
    }
    let Some(loader) = movie.create_instance("flash.display.Loader", &[]) else { return false };
    let Some(info) = loader.get_member("contentLoaderInfo").filter(Value::is_object) else { return false };
    let id = NEXT_LOAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (done, failed) = load_handlers();
    for (event, handler) in [("complete", done), ("ioError", failed)] {
        let (Some(name), Some(f)) = (movie.create_string(event), movie.create_function(handler, id)) else { return false };
        if info.invoke("addEventListener", &[name, f]).is_none() {
            return false;
        }
    }
    let request = movie.create_string(&p.asset).and_then(|url| movie.create_instance("flash.net.URLRequest", &[url]));
    let started = request.is_some_and(|r| loader.invoke("load", &[r]).is_some());
    if let Ok(mut loads) = LOADS.lock() {
        loads.push((id, PendingLoad { loader: std::mem::ManuallyDrop::new(loader), ..p }));
    }
    if !started {
        load_finished(movie, id, false);
    }
    true
}

/// The dotted path of a display object from its root (`root1.instance83.instance82`).
fn display_path(object: &Value) -> String {
    let mut names = Vec::new();
    let mut current = object.clone_ref();
    for _ in 0..64 {
        names.push(current.get_member("name").and_then(|n| n.as_string()).unwrap_or_default());
        let Some(parent) = current.get_member("parent").filter(|p| p.kind() == kind::DISPLAY_OBJECT) else { break };
        // The root's parent is the stage, which has no parent.
        if !parent.get_member("parent").is_some_and(|g| g.kind() == kind::DISPLAY_OBJECT) {
            break;
        }
        current = parent;
    }
    names.reverse();
    names.join(".")
}

/// A load's outcome: on success the loader becomes a child of the target; then the
/// receiver's callback gets `(Bool ok, String menu, String sourceVar, String
/// loadedPath, String assetPath)`, `loadedPath` being the content's display path ("" on
/// failure).
fn load_finished(movie: Movie, id: usize, ok: bool) {
    let Some(p) = LOADS.lock().ok().and_then(|mut loads| loads.iter().position(|(i, _)| *i == id).map(|i| loads.remove(i).1)) else {
        return;
    };
    let loader = std::mem::ManuallyDrop::into_inner(p.loader);
    let mut path = String::new();
    if ok
        && let Some(target) = load_target(movie, &p.source)
        && target.invoke("addChild", &[loader.clone_ref()]).is_some()
    {
        path = loader.get_member("content").filter(|c| c.kind() == kind::DISPLAY_OBJECT).map(|c| display_path(&c)).unwrap_or_default();
    }
    drop(loader);
    let ok = !path.is_empty();
    log::debug!(target: "f4x::ui", "Load {:?} into {}:{:?} -> {ok} {path:?}", p.asset, p.menu, p.source);
    let (Some((handle, script)), Some(vm)) = (p.receiver.filter(|_| !p.callback.is_empty()), f4x_agent::observe::virtual_machine()) else {
        return;
    };
    let args = vec![Arg::Bool(ok), Arg::String(p.menu), Arg::String(p.source), Arg::String(path), Arg::String(p.asset)];
    Vm(vm as *mut _).dispatch_method_call(handle, &script, &p.callback, args);
}

/// A receiver argument as (handle, script).
fn receiver(vm: Vm, v: &PValue) -> Option<(u64, String)> {
    match v {
        PValue::Object { handle, script } => Some((*handle, script.clone())),
        PValue::Form(form) => {
            let f = f4x_engine::Form::checked(*form)?;
            let type_id = u32::from(f.form_type().0);
            Some((vm.handle_for(type_id, form.cast())?, vm.script_for_type(type_id)?))
        }
        _ => None,
    }
}

pub(crate) const NATIVES: &[Entry] = &[
    ("UI", "IsMenuOpen", |c| {
        let name = c.string(0);
        c.ret_bool(ui::is_open(&name));
        Ok(())
    }),
    ("UI", "IsMenuRegistered", |c| {
        let name = c.string(0);
        c.ret_bool(ui::is_registered(&name));
        Ok(())
    }),
    ("UI", "OpenMenu", |c| {
        let name = c.string(0);
        c.ret_bool(ui::is_registered(&name) && ui::add_message(&name, ui::Message::Show));
        Ok(())
    }),
    ("UI", "CloseMenu", |c| {
        let name = c.string(0);
        c.ret_bool(ui::is_registered(&name) && ui::add_message(&name, ui::Message::Hide));
        Ok(())
    }),
    ("UI", "get", |c| {
        let (menu, path) = (c.string(0), c.string(1));
        if path.is_empty() {
            return Ok(());
        }
        let Some(value) = with_movie(&menu, |movie| resolve(movie, &path).map_or_else(empty_var, |v| to_pvalue(&v, 0))) else {
            return Ok(());
        };
        c.ret_value(&value)
    }),
    ("UI", "set", |c| {
        let (menu, path, value, vm) = (c.string(0), c.string(1), c.value(2), c.vm);
        let ok = !path.is_empty()
            && with_movie(&menu, |movie| {
                let Some(v) = from_pvalue(vm, movie, &value, 0) else { return false };
                match path.rsplit_once('.') {
                    Some((parent, member)) => !member.is_empty() && resolve(movie, parent).is_some_and(|mut p| p.is_object() && p.set_member(member, &v)),
                    None => movie.set_variable(&path, &v),
                }
            })
            .unwrap_or(false);
        c.ret_bool(ok);
        Ok(())
    }),
    ("UI", "Invoke", |c| {
        let (menu, path, args, vm) = (c.string(0), c.string(1), c.value(2), c.vm);
        let args = match args {
            PValue::Array(items) => items,
            _ => Vec::new(),
        };
        let Some(value) = with_movie(&menu, |movie| {
            let values: Vec<Value> = args.iter().map(|a| from_pvalue(vm, movie, a, 0).unwrap_or_else(Value::undefined)).collect();
            // By path through the movie root, as F4SE does: a member function
            // (`root.f4se.GetDirectoryListing`), a display object's method
            // (`root.getChildAt`) or a global (`String`). Class statics and array methods
            // fail there (an empty Var). Calling through the resolved object instead
            // faults on array methods.
            let method = path.rsplit('.').next().unwrap_or_default();
            let result = if method.is_empty() || UNSAFE_METHODS.contains(&method) { None } else { movie.invoke(&path, &values) };
            result.map_or_else(empty_var, |v| to_pvalue(&v, 0))
        }) else {
            return Ok(());
        };
        c.ret_value(&value)
    }),
    ("UI", "Load", |c| {
        let (menu, source, asset, callback) = (c.string(0), c.string(1), c.string(2), c.string(4));
        // The receiver's own script object (e.g. a quest's script, not `Quest`), else
        // the form's base script type.
        let own = c.arg(3).and_then(|v| v.as_object()).and_then(|o| {
            Some((f4x_engine::papyrus::object_handle(o)?, f4x_engine::papyrus::object_script_name(o)?))
        });
        let receiver = own.or_else(|| receiver(c.vm, &c.value(3)));
        let ok = with_movie(&menu.clone(), |movie| {
                let loader = std::mem::ManuallyDrop::new(Value::undefined());
                start_load(movie, PendingLoad { menu, source, asset, receiver, callback, loader })
            })
            .unwrap_or(false);
        c.ret_bool(ok);
        Ok(())
    }),
    ("UI", "RegisterCustomMenu", |c| {
        let (name, path, root, data) = (c.string(0), c.string(1), c.string(2), c.value(3));
        let field = |n: &str| menu_data_field(&data, n).unwrap_or(0);
        let ok = register(&name, path, root, field("menuFlags"), field("movieFlags"), field("depth"));
        c.ret_bool(ok);
        Ok(())
    }),
    ("UI", "RegisterBasicCustomMenu", |c| {
        let (name, path, root) = (c.string(0), c.string(1), c.string(2));
        // Black-box: F4SE's basic menu shows the cursor, sets menu mode and scales
        // noBorder. Other flag bits, the depth and the extended flags are not observable
        // with our tools; the depth is IMenu's default.
        let flags = (ui::menu_flags::PAUSES_GAME | ui::menu_flags::USES_CURSOR) as i32;
        let ok = register(&name, path, root, flags, 3, i32::from(ui::DEPTH_STANDARD));
        c.ret_bool(ok);
        Ok(())
    }),
];

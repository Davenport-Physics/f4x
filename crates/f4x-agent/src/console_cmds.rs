//! Console commands registered by plugins.
//!
//! Each command takes a free slot of the engine's console command table: an entry whose
//! handler is a release-build no-op stub, checked at registration time so slots other
//! plugins already took are never reused. All f4x commands share one execute function,
//! which decodes the arguments with the engine's own `Script::ParseParameters` and calls
//! the plugin's handler with typed values.

use std::ffi::{CString, c_char, c_void};
use std::sync::Mutex;

use f4x_abi::service::{ConsoleArg, ConsoleCall, ConsoleCommandSpec, Str};
use f4x_engine::console::{self as engine_console, ScriptFunction, ScriptParameter, param};
use windows_sys::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};

const MAX_PARAMS: usize = 8;
const TEXT_LEN: usize = 512;

struct Command {
    plugin: String,
    name: String,
    params: &'static [ScriptParameter],
    kinds: Vec<u32>,
    handler: unsafe extern "C" fn(*const ConsoleCall, *mut c_void) -> bool,
    user: usize,
    slot: usize,
}

// SAFETY: the leaked parameter tables are immutable and only read by the engine and by
// `execute`; the handler/user pointers are owned by the registering plugin for the process.
unsafe impl Send for Command {}

static COMMANDS: Mutex<Vec<Command>> = Mutex::new(Vec::new());

fn leak(s: &str) -> *const c_char {
    Box::leak(CString::new(s.replace('\0', " ")).unwrap_or_default().into_boxed_c_str()).as_ptr()
}

/// Register a command. Returns an error text on failure.
///
/// # Safety
/// `spec` must be valid for the call; its handler and user pointer for the process.
pub unsafe fn register(plugin: &str, spec: &ConsoleCommandSpec) -> Result<usize, String> {
    // SAFETY: the plugin passes valid UTF-8 strings (contract).
    let (name, short, help) = unsafe { (spec.name.as_str().to_owned(), spec.short_name.as_str().to_owned(), spec.help.as_str().to_owned()) };
    if name.is_empty() {
        return Err("empty command name".into());
    }
    if spec.param_count as usize > MAX_PARAMS {
        return Err(format!("at most {MAX_PARAMS} parameters"));
    }
    let table = engine_console::console_functions().ok_or("console command table not found")?;
    let mut commands = COMMANDS.lock().map_err(|_| "poisoned")?;
    // Names must be unique among existing commands too.
    for i in 0..engine_console::CONSOLE_FUNCTION_COUNT {
        // SAFETY: the engine's SCRIPT_FUNCTION[522].
        let entry = unsafe { &*table.add(i) };
        for existing in [entry.name, entry.short_name] {
            // SAFETY: entry strings are static C strings or null.
            if !existing.is_null() && !name.is_empty() && unsafe { std::ffi::CStr::from_ptr(existing) }.to_string_lossy().eq_ignore_ascii_case(&name) {
                return Err(format!("a console command named {name:?} already exists"));
            }
        }
    }
    let slot = (0..engine_console::CONSOLE_FUNCTION_COUNT)
        .rev()
        // SAFETY: as above.
        .find(|&i| commands.iter().all(|c| c.slot != i) && engine_console::is_free_slot(unsafe { &*table.add(i) }))
        .ok_or("no free console command slot")?;

    let mut kinds = Vec::new();
    let mut params = Vec::new();
    for i in 0..spec.param_count as usize {
        // SAFETY: `params` has `param_count` entries.
        let p = unsafe { &*spec.params.add(i) };
        kinds.push(p.kind);
        // SAFETY: valid UTF-8 (contract).
        params.push(ScriptParameter { name: leak(unsafe { p.name.as_str() }), kind: p.kind as i32, optional: p.optional });
    }
    let params: &'static [ScriptParameter] = Box::leak(params.into_boxed_slice());

    // SAFETY: rewriting one table entry; made writable for the duration.
    unsafe {
        let entry = table.add(slot);
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        VirtualProtect(entry.cast(), size_of::<ScriptFunction>(), PAGE_READWRITE, &mut old);
        (*entry).name = leak(&name);
        (*entry).short_name = if short.is_empty() { leak("") } else { leak(&short) };
        (*entry).help = leak(&help);
        (*entry).reference_function = false;
        (*entry).param_count = params.len() as u16;
        (*entry).params = if params.is_empty() { std::ptr::null() } else { params.as_ptr() };
        (*entry).execute = Some(execute);
        (*entry).condition = std::ptr::null();
        (*entry).editor_filter = false;
        VirtualProtect(entry.cast(), size_of::<ScriptFunction>(), old, &mut old);
    }
    log::info!(target: "f4x::console", "{plugin} registered console command {name:?} (slot {slot})");
    commands.push(Command { plugin: plugin.to_owned(), name, params, kinds, handler: spec.handler, user: spec.user as usize, slot });
    Ok(slot)
}

/// The shared execute function of every f4x console command.
unsafe extern "C" fn execute(
    params: *const ScriptParameter,
    compiled: *const u8,
    reference: *mut c_void,
    container: *mut c_void,
    script: *mut c_void,
    locals: *mut c_void,
    _result: *mut f32,
    offset: *mut u32,
) -> bool {
    let found = COMMANDS.lock().ok().and_then(|c| {
        c.iter()
            .find(|c| (c.params.as_ptr() == params) || (c.params.is_empty() && params.is_null()))
            .map(|c| (c.plugin.clone(), c.name.clone(), c.params, c.kinds.clone(), c.handler, c.user))
    });
    let Some((plugin, name, table, kinds, handler, user)) = found else { return true };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Outputs per parameter (sentinels detect omitted optional arguments).
        let mut ints = [i32::MIN; MAX_PARAMS];
        let mut floats = [f32::NAN; MAX_PARAMS];
        let mut texts = [[0u8; TEXT_LEN]; MAX_PARAMS];
        let mut forms = [std::ptr::null_mut::<c_void>(); MAX_PARAMS];
        let mut dummy = [0u64; 64];
        let mut out: [*mut c_void; MAX_PARAMS] = [dummy.as_mut_ptr().cast(); MAX_PARAMS];
        for (i, &kind) in kinds.iter().enumerate() {
            out[i] = match kind {
                param::INT => (&raw mut ints[i]).cast(),
                param::FLOAT => (&raw mut floats[i]).cast(),
                param::STRING => texts[i].as_mut_ptr().cast(),
                _ => (&raw mut forms[i]).cast(),
            };
        }
        let parsed = match (table.is_empty(), engine_console::parse_parameters()) {
            (true, _) => true,
            // SAFETY: the engine's parser with one output pointer per declared parameter.
            (false, Some(parse)) => unsafe {
                parse(table.as_ptr(), compiled, offset, reference, container, script, locals, out[0], out[1], out[2], out[3], out[4], out[5], out[6], out[7])
            },
            (false, None) => false,
        };
        if !parsed {
            let _ = engine_console::print(&format!("{name}: could not read the arguments"));
            return;
        }
        let text_strs: Vec<String> = texts
            .iter()
            .map(|t| String::from_utf8_lossy(&t[..t.iter().position(|&b| b == 0).unwrap_or(TEXT_LEN)]).into_owned())
            .collect();
        let args: Vec<ConsoleArg> = kinds
            .iter()
            .enumerate()
            .map(|(i, &kind)| {
                let present = match kind {
                    param::INT => ints[i] != i32::MIN,
                    param::FLOAT => !floats[i].is_nan(),
                    param::STRING => !text_strs[i].is_empty(),
                    _ => !forms[i].is_null(),
                };
                ConsoleArg { kind, present, int: ints[i], float: floats[i], text: Str::new(&text_strs[i]), form: forms[i] }
            })
            .collect();
        let call = ConsoleCall { args: args.as_ptr(), arg_count: args.len() as u32, reference };
        log::debug!(target: "f4x::console", "{plugin}: {name} ({} args)", args.len());
        // SAFETY: the plugin's handler with a call valid for its duration.
        unsafe { handler(&call, user as *mut c_void) };
    }));
    true
}

//! Console commands with typed, engine-parsed arguments.
//!
//! ```ignore
//! use f4x_sdk::console::{Arg, Param};
//! ctx.console_command("heal", "", "heal <amount> [target]",
//!     &[Param::float("amount"), Param::reference("target").optional()],
//!     |call| {
//!         let amount = call.float(0).unwrap_or(100.0);
//!         Ok(format!("healing {amount}"))
//!     });
//! ```
//!
//! The engine's own console parser decodes the arguments (numbers, quoted or bare
//! strings, editor IDs and form IDs for forms and references), so a command gets real
//! values instead of a fixed amount.

use std::ffi::c_void;

use f4x_abi::service::{ConsoleArg, ConsoleCall, ConsoleCommandSpec, ConsoleParam, Service, Str, console_param as kind};

/// A declared parameter.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    pub kind: u32,
    pub optional: bool,
}

impl Param {
    #[must_use]
    pub const fn new(name: &'static str, kind: u32) -> Param {
        Param { name, kind, optional: false }
    }
    #[must_use]
    pub const fn int(name: &'static str) -> Param {
        Param::new(name, kind::INT)
    }
    #[must_use]
    pub const fn float(name: &'static str) -> Param {
        Param::new(name, kind::FLOAT)
    }
    #[must_use]
    pub const fn string(name: &'static str) -> Param {
        Param::new(name, kind::STRING)
    }
    /// A placed reference (`player`, a ref id, or an editor id).
    #[must_use]
    pub const fn reference(name: &'static str) -> Param {
        Param::new(name, kind::OBJECT_REF)
    }
    /// An actor reference.
    #[must_use]
    pub const fn actor(name: &'static str) -> Param {
        Param::new(name, kind::ACTOR)
    }
    /// Any form.
    #[must_use]
    pub const fn form(name: &'static str) -> Param {
        Param::new(name, kind::FORM)
    }
    /// An inventory item (base object).
    #[must_use]
    pub const fn item(name: &'static str) -> Param {
        Param::new(name, kind::INVENTORY_OBJECT)
    }
    #[must_use]
    pub const fn optional(mut self) -> Param {
        self.optional = true;
        self
    }
}

/// A decoded argument.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Int(i32),
    Float(f32),
    Text(String),
    /// A form or reference (`TESForm*`); inspect it with `f4x-engine`.
    Form(*mut c_void),
    /// An optional argument that was not given.
    Missing,
}

/// One invocation of a command.
pub struct Call {
    pub args: Vec<Arg>,
    /// The reference the command ran on (`player.mycommand`), if any.
    pub reference: Option<*mut c_void>,
}

impl Call {
    pub fn int(&self, i: usize) -> Option<i32> {
        match self.args.get(i) {
            Some(Arg::Int(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn float(&self, i: usize) -> Option<f32> {
        match self.args.get(i) {
            Some(Arg::Float(v)) => Some(*v),
            Some(Arg::Int(v)) => Some(*v as f32),
            _ => None,
        }
    }
    pub fn text(&self, i: usize) -> Option<&str> {
        match self.args.get(i) {
            Some(Arg::Text(v)) => Some(v),
            _ => None,
        }
    }
    pub fn form(&self, i: usize) -> Option<*mut c_void> {
        match self.args.get(i) {
            Some(Arg::Form(v)) => Some(*v),
            _ => None,
        }
    }
}

/// What a handler returns: `Ok(text)` is printed (if non-empty), `Err(text)` printed as
/// an error.
pub type Handler = Box<dyn Fn(&Call) -> Result<String, String> + Send + Sync>;

fn service_v2() -> Option<&'static Service> {
    let service = crate::service()?;
    let needed = std::mem::offset_of!(Service, console_execute) + std::mem::size_of::<usize>();
    (service.struct_size as usize >= needed).then_some(service)
}

pub(crate) fn register(name: &str, short_name: &str, help: &str, params: &[Param], handler: Handler) -> bool {
    let Some(service) = service_v2() else {
        log::warn!("console commands need a newer f4x agent");
        return false;
    };
    let names: Vec<String> = params.iter().map(|p| p.name.to_owned()).collect();
    let raw: Vec<ConsoleParam> = params
        .iter()
        .zip(&names)
        .map(|(p, n)| ConsoleParam { name: Str::new(Box::leak(n.clone().into_boxed_str())), kind: p.kind, optional: p.optional })
        .collect();
    let raw: &'static [ConsoleParam] = Box::leak(raw.into_boxed_slice());
    let user = Box::into_raw(Box::new(handler)).cast::<c_void>();
    let spec = ConsoleCommandSpec {
        name: Str::new(name),
        short_name: Str::new(short_name),
        help: Str::new(help),
        params: raw.as_ptr(),
        param_count: raw.len() as u32,
        handler: trampoline,
        user,
    };
    // SAFETY: valid spec; the handler box is leaked for the process.
    unsafe { (service.register_console_command)(Str::new(crate::plugin_name()), &spec) }
}

unsafe extern "C" fn trampoline(call: *const ConsoleCall, user: *mut c_void) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: the agent passes a valid call and our leaked handler.
        let (call, handler) = unsafe { (&*call, &*user.cast::<Handler>()) };
        // SAFETY: `args` has `arg_count` entries for the duration of the call.
        let raw = unsafe { std::slice::from_raw_parts(call.args, call.arg_count as usize) };
        let args = raw.iter().map(decode).collect();
        let call = Call { args, reference: (!call.reference.is_null()).then_some(call.reference) };
        match handler(&call) {
            Ok(text) if !text.is_empty() => print(&text),
            Ok(_) => {}
            Err(text) => print(&format!("error: {text}")),
        }
    })
    .is_ok()
}

fn decode(arg: &ConsoleArg) -> Arg {
    if !arg.present {
        return Arg::Missing;
    }
    match arg.kind {
        kind::INT => Arg::Int(arg.int),
        kind::FLOAT => Arg::Float(arg.float),
        // SAFETY: valid for the call.
        kind::STRING => Arg::Text(unsafe { arg.text.as_str() }.to_owned()),
        _ => Arg::Form(arg.form),
    }
}

/// Print a line to the console.
pub fn print(text: &str) {
    if let Some(service) = service_v2() {
        // SAFETY: valid string for the call.
        unsafe { (service.console_print)(Str::new(text)) };
    }
}

/// Run a console command as if typed (on the main thread; queued otherwise).
pub fn execute(command: &str) -> bool {
    // SAFETY: as above.
    service_v2().is_some_and(|service| unsafe { (service.console_execute)(Str::new(command)) })
}

//! Write f4x plugins and in-game tests in Rust.
//!
//! ```ignore
//! f4x_sdk::plugin! {
//!     name: "my_tests",
//!     load: |ctx| {
//!         ctx.test("frames_advance", || async {
//!             let start = f4x_sdk::frames();
//!             f4x_sdk::wait_frames(3).await;
//!             f4x_sdk::check(f4x_sdk::frames() >= start + 3, "frames did not advance")
//!         });
//!         Ok(())
//!     },
//! }
//! ```
//!
//! One DLL works under both hosts:
//! - **f4x**: the runtime calls the `f4x_plugin_load` export with the service table.
//! - **real F4SE**: F4SE calls `F4SEPlugin_Query`/`F4SEPlugin_Load`. The plugin then finds
//!   the agent (`f4x_agent.dll`, itself an F4SE plugin) and connects once it is loaded.
//!
//! Tests are `async` blocks polled once per main-loop iteration, on the game's main
//! thread, so engine calls are safe between `await`s. Helpers: [`next_pump`],
//! [`wait_pumps`], [`wait_frames`], [`wait_until`].

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::{Duration, Instant};

pub use f4x_abi;
use f4x_abi::service::{Service, Str, TestVtbl, test_status};
pub use log;

pub mod console;
#[doc(hidden)]
pub mod __private;

static SERVICE: OnceLock<&'static Service> = OnceLock::new();
static NAME: OnceLock<&'static str> = OnceLock::new();

/// The agent's service table, once connected.
pub fn service() -> Option<&'static Service> {
    SERVICE.get().copied()
}

/// This plugin's name (from `plugin!`).
pub fn plugin_name() -> &'static str {
    NAME.get().copied().unwrap_or("?")
}

/// `"f4x"` or `"f4se"`.
pub fn host() -> &'static str {
    // SAFETY: the host string in the service table is 'static.
    service().map_or("", |s| unsafe { s.host.as_str() })
}

/// Frames presented so far.
pub fn frames() -> u64 {
    // SAFETY: calling the agent's exported function.
    service().map_or(0, |s| unsafe { (s.frames)() })
}

/// Main-loop iterations so far.
pub fn pumps() -> u64 {
    // SAFETY: as above.
    service().map_or(0, |s| unsafe { (s.pumps)() })
}

/// What a test returns.
pub type TestResult = Result<(), String>;

/// `Ok(())` if `condition`, else `Err(message)`.
pub fn check(condition: bool, message: impl Into<String>) -> TestResult {
    if condition { Ok(()) } else { Err(message.into()) }
}

/// Registration context passed to the plugin's `load`.
pub struct Context {
    _private: (),
}

type Factory = Box<dyn Fn() -> Pin<Box<dyn Future<Output = TestResult>>> + Send + Sync>;

impl Context {
    /// Register an in-game test. `make` creates a fresh future for each run.
    pub fn test<F, Fut>(&self, name: &str, make: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TestResult> + 'static,
    {
        let Some(service) = service() else { return };
        let factory: Box<Factory> = Box::new(Box::new(move || Box::pin(make())));
        let user = Box::into_raw(factory).cast::<c_void>();
        // SAFETY: valid strings and vtable; `user` is leaked for the process lifetime.
        let ok = unsafe { (service.register_test)(Str::new(plugin_name()), Str::new(name), &TEST_VTBL, user) };
        if !ok {
            log::warn!("test {name} was not registered (duplicate?)");
        }
    }
}

impl Context {
    /// Add a console command with typed parameters, decoded by the engine's own console
    /// parser. Returns false if it could not be added (name taken, no free slot, old agent).
    pub fn console_command(
        &self,
        name: &str,
        short_name: &str,
        help: &str,
        params: &[console::Param],
        handler: impl Fn(&console::Call) -> Result<String, String> + Send + Sync + 'static,
    ) -> bool {
        console::register(name, short_name, help, params, Box::new(handler))
    }
}

// ---- Test driving (called by the agent through TEST_VTBL) ----

struct Instance {
    future: Pin<Box<dyn Future<Output = TestResult>>>,
    message: String,
}

static TEST_VTBL: TestVtbl = TestVtbl { start: test_start, poll: test_poll, message: test_message, drop: test_drop };

unsafe extern "C" fn test_start(user: *mut c_void) -> *mut c_void {
    // SAFETY: `user` is the Factory leaked in `Context::test`.
    let factory = unsafe { &*user.cast::<Factory>() };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(factory)) {
        Ok(future) => Box::into_raw(Box::new(Instance { future, message: String::new() })).cast(),
        Err(payload) => {
            let message = panic_message(&*payload);
            let failed: Pin<Box<dyn Future<Output = TestResult>>> = Box::pin(async move { Err(message) });
            Box::into_raw(Box::new(Instance { future: failed, message: String::new() })).cast()
        }
    }
}

unsafe extern "C" fn test_poll(instance: *mut c_void) -> u32 {
    // SAFETY: created by `test_start`, not yet dropped.
    let instance = unsafe { &mut *instance.cast::<Instance>() };
    let mut cx = TaskContext::from_waker(Waker::noop());
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.future.as_mut().poll(&mut cx))) {
        Ok(Poll::Pending) => test_status::PENDING,
        Ok(Poll::Ready(Ok(()))) => test_status::PASSED,
        Ok(Poll::Ready(Err(message))) => {
            instance.message = message;
            test_status::FAILED
        }
        Err(payload) => {
            instance.message = panic_message(&*payload);
            test_status::PANICKED
        }
    }
}

unsafe extern "C" fn test_message(instance: *mut c_void, out: *mut Str) {
    // SAFETY: as above; the message lives until `test_drop`.
    unsafe {
        let instance = &*instance.cast::<Instance>();
        *out = Str::new(&instance.message);
    }
}

unsafe extern "C" fn test_drop(instance: *mut c_void) {
    // SAFETY: created by `test_start`; dropped exactly once.
    drop(unsafe { Box::from_raw(instance.cast::<Instance>()) });
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panicked: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panicked: {s}")
    } else {
        "panicked".to_owned()
    }
}

// ---- Async helpers (each poll of a test = one main-loop iteration) ----

/// Resolves at the next main-loop iteration.
pub fn next_pump() -> impl Future<Output = ()> {
    let mut yielded = false;
    std::future::poll_fn(move |_| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            Poll::Pending
        }
    })
}

/// Resolves after `n` main-loop iterations.
pub async fn wait_pumps(n: u64) {
    for _ in 0..n {
        next_pump().await;
    }
}

/// Resolves once `n` more frames have been presented.
pub async fn wait_frames(n: u64) {
    let target = frames() + n;
    while frames() < target {
        next_pump().await;
    }
}

/// Polls `condition` once per main-loop iteration until it holds or `timeout` passes.
pub async fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> TestResult {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("condition not met within {timeout:?}"));
        }
        next_pump().await;
    }
}

/// Declare an f4x plugin. See the crate docs.
#[macro_export]
macro_rules! plugin {
    (name: $name:literal, load: $load:expr $(,)?) => {
        #[doc(hidden)]
        fn __f4x_plugin_load(ctx: &$crate::Context) -> ::std::result::Result<(), ::std::string::String> {
            let load: fn(&$crate::Context) -> ::std::result::Result<(), ::std::string::String> = $load;
            load(ctx)
        }

        /// f4x host entry point.
        ///
        /// # Safety
        /// Called by the f4x runtime with its service table.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn f4x_plugin_load(service: *const $crate::f4x_abi::service::Service) -> bool {
            // SAFETY: forwarded from the host.
            unsafe { $crate::__private::load(concat!($name, "\0"), service, __f4x_plugin_load) }
        }

        /// F4SE entry point (old-gen).
        ///
        /// # Safety
        /// Called by F4SE.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn F4SEPlugin_Query(
            f4se: *const $crate::f4x_abi::f4se::F4seInterface,
            info: *mut $crate::f4x_abi::f4se::PluginInfo,
        ) -> bool {
            // SAFETY: forwarded from F4SE.
            unsafe { $crate::__private::f4se_query(concat!($name, "\0"), f4se, info) }
        }

        /// F4SE entry point.
        ///
        /// # Safety
        /// Called by F4SE.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn F4SEPlugin_Load(f4se: *const $crate::f4x_abi::f4se::F4seInterface) -> bool {
            // SAFETY: forwarded from F4SE.
            unsafe { $crate::__private::f4se_load(concat!($name, "\0"), f4se, __f4x_plugin_load) }
        }
    };
}

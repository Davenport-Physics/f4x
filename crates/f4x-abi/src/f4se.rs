//! The F4SE-compatible plugin ABI.
//!
//! Source: CommonLibF4 `include/F4SE/Interfaces.h` (`detail::*` structs, the
//! `LoadInterface` / `MessagingInterface` enums, `PluginInfo`) and, for next-gen version
//! data, `origin/community` `PluginVersionData`. `F4SEAPI` is `__cdecl`, which on x64
//! is the platform C ABI, hence `extern "C"`.
//!
//! Plugins export `F4SEPlugin_Query(*const F4seInterface, *mut PluginInfo) -> bool` and
//! `F4SEPlugin_Load(*const F4seInterface) -> bool` (CommonLibF4 `ExampleProject`); next-gen
//! plugins also export a `F4SEPlugin_Version` [`PluginVersionData`].
#![allow(missing_docs)]

use core::ffi::{c_char, c_void};

use crate::version::PackedVersion;

pub type PluginHandle = u32;

/// `F4SEPlugin_Query`.
pub type QueryFn = unsafe extern "C" fn(*const F4seInterface, *mut PluginInfo) -> bool;
/// `F4SEPlugin_Load`.
pub type LoadFn = unsafe extern "C" fn(*const F4seInterface) -> bool;

pub const EXPORT_QUERY: &core::ffi::CStr = c"F4SEPlugin_Query";
pub const EXPORT_LOAD: &core::ffi::CStr = c"F4SEPlugin_Load";
pub const EXPORT_VERSION: &core::ffi::CStr = c"F4SEPlugin_Version";

/// The table passed to `F4SEPlugin_Query` and `F4SEPlugin_Load`.
#[repr(C)]
pub struct F4seInterface {
    pub f4se_version: PackedVersion,
    pub runtime_version: PackedVersion,
    pub editor_version: PackedVersion,
    pub is_editor: u32,
    pub query_interface: Option<unsafe extern "C" fn(id: u32) -> *mut c_void>,
    pub get_plugin_handle: Option<unsafe extern "C" fn() -> PluginHandle>,
    pub get_release_index: Option<unsafe extern "C" fn() -> u32>,
    /// Returns a [`PluginInfo`] for a loaded plugin by name (CommonLibF4 marks it 0.6.22+).
    pub get_plugin_info: Option<unsafe extern "C" fn(name: *const c_char) -> *const c_void>,
}

/// `QueryInterface` ids (CommonLibF4 `LoadInterface`).
pub mod interface_id {
    pub const INVALID: u32 = 0;
    pub const MESSAGING: u32 = 1;
    pub const SCALEFORM: u32 = 2;
    pub const PAPYRUS: u32 = 3;
    pub const SERIALIZATION: u32 = 4;
    pub const TASK: u32 = 5;
    pub const OBJECT: u32 = 6;
    pub const TRAMPOLINE: u32 = 7;
}

/// Filled in by old-gen plugins in `F4SEPlugin_Query`.
#[repr(C)]
pub struct PluginInfo {
    pub info_version: u32,
    pub name: *const c_char,
    pub version: u32,
}

impl PluginInfo {
    pub const VERSION: u32 = 1;
}

/// Messaging interface (CommonLibF4 `MessagingInterface::kVersion = 1`).
#[repr(C)]
pub struct MessagingInterface {
    pub interface_version: u32,
    pub register_listener:
        Option<unsafe extern "C" fn(handle: PluginHandle, sender: *const c_char, callback: *mut c_void) -> bool>,
    pub dispatch: Option<
        unsafe extern "C" fn(
            handle: PluginHandle,
            kind: u32,
            data: *mut c_void,
            data_len: u32,
            receiver: *const c_char,
        ) -> bool,
    >,
    pub get_event_dispatcher: Option<unsafe extern "C" fn(dispatcher_id: u32) -> *mut c_void>,
}

impl MessagingInterface {
    pub const VERSION: u32 = 1;
}

/// A message delivered to a listener (`void F4SEAPI(Message*)`).
#[repr(C)]
pub struct Message {
    pub sender: *const c_char,
    pub kind: u32,
    pub data_len: u32,
    pub data: *mut c_void,
}

/// Listener callback type.
pub type MessageCallback = unsafe extern "C" fn(*mut Message);

/// Message types sent by the host under sender `"F4SE"` (CommonLibF4 order).
pub mod message {
    pub const POST_LOAD: u32 = 0;
    pub const POST_POST_LOAD: u32 = 1;
    pub const PRE_LOAD_GAME: u32 = 2;
    pub const POST_LOAD_GAME: u32 = 3;
    pub const PRE_SAVE_GAME: u32 = 4;
    pub const POST_SAVE_GAME: u32 = 5;
    pub const DELETE_GAME: u32 = 6;
    pub const INPUT_LOADED: u32 = 7;
    pub const NEW_GAME: u32 = 8;
    pub const GAME_LOADED: u32 = 9;
    pub const GAME_DATA_READY: u32 = 10;

    /// The sender name plugins pass to `RegisterListener` for host messages
    /// (CommonLibF4 `RegisterListener(handler)` defaults to `"F4SE"`).
    pub const HOST_SENDER: &core::ffi::CStr = c"F4SE";

    /// Human-readable name, for logs.
    #[must_use]
    pub const fn name(kind: u32) -> &'static str {
        match kind {
            POST_LOAD => "PostLoad",
            POST_POST_LOAD => "PostPostLoad",
            PRE_LOAD_GAME => "PreLoadGame",
            POST_LOAD_GAME => "PostLoadGame",
            PRE_SAVE_GAME => "PreSaveGame",
            POST_SAVE_GAME => "PostSaveGame",
            DELETE_GAME => "DeleteGame",
            INPUT_LOADED => "InputLoaded",
            NEW_GAME => "NewGame",
            GAME_LOADED => "GameLoaded",
            GAME_DATA_READY => "GameDataReady",
            _ => "Unknown",
        }
    }
}

/// Scaleform interface (`kVersion = 1`). The callback is
/// `bool(GFx::Movie* view, GFx::Value* value)`.
#[repr(C)]
pub struct ScaleformInterface {
    pub interface_version: u32,
    pub register: Option<unsafe extern "C" fn(name: *const c_char, callback: *mut c_void) -> bool>,
}

impl ScaleformInterface {
    pub const VERSION: u32 = 1;
}

/// Scaleform registration callback.
pub type ScaleformCallback = unsafe extern "C" fn(movie: *mut c_void, value: *mut c_void) -> bool;

/// Serialization (co-save) interface (`kVersion = 1`).
#[repr(C)]
pub struct SerializationInterface {
    pub version: u32,
    pub set_unique_id: Option<unsafe extern "C" fn(handle: PluginHandle, uid: u32)>,
    pub set_revert_callback: Option<unsafe extern "C" fn(handle: PluginHandle, callback: *mut c_void)>,
    pub set_save_callback: Option<unsafe extern "C" fn(handle: PluginHandle, callback: *mut c_void)>,
    pub set_load_callback: Option<unsafe extern "C" fn(handle: PluginHandle, callback: *mut c_void)>,
    pub set_form_delete_callback: Option<unsafe extern "C" fn(handle: PluginHandle, callback: *mut c_void)>,
    pub write_record: Option<unsafe extern "C" fn(kind: u32, version: u32, buf: *const c_void, len: u32) -> bool>,
    pub open_record: Option<unsafe extern "C" fn(kind: u32, version: u32) -> bool>,
    pub write_record_data: Option<unsafe extern "C" fn(buf: *const c_void, len: u32) -> bool>,
    pub get_next_record_info: Option<unsafe extern "C" fn(kind: *mut u32, version: *mut u32, len: *mut u32) -> bool>,
    pub read_record_data: Option<unsafe extern "C" fn(buf: *mut c_void, len: u32) -> u32>,
    pub resolve_handle: Option<unsafe extern "C" fn(handle: u64, out: *mut u64) -> bool>,
    pub resolve_form_id: Option<unsafe extern "C" fn(form_id: u32, out: *mut u32) -> bool>,
}

impl SerializationInterface {
    pub const VERSION: u32 = 1;
}

/// Save/load/revert callback: `void(const SerializationInterface*)`.
pub type SerializationCallback = unsafe extern "C" fn(*const SerializationInterface);
/// Form-delete callback: `void(u64 handle)`.
pub type FormDeleteCallback = unsafe extern "C" fn(u64);

/// Papyrus interface (`kVersion = 2`). `register` takes `bool(IVirtualMachine*)`;
/// the registrant functor is `void(u64 handle, const char* script, const char* callback, void* data)`.
#[repr(C)]
pub struct PapyrusInterface {
    pub interface_version: u32,
    pub register: Option<unsafe extern "C" fn(callback: *mut c_void) -> bool>,
    pub get_external_event_registrations:
        Option<unsafe extern "C" fn(event: *const c_char, data: *mut c_void, functor: *mut c_void)>,
}

impl PapyrusInterface {
    pub const VERSION: u32 = 2;
}

pub type PapyrusRegisterCallback = unsafe extern "C" fn(vm: *mut c_void) -> bool;
pub type RegistrantFunctor =
    unsafe extern "C" fn(handle: u64, script: *const c_char, callback: *const c_char, data: *mut c_void);

/// Task interface (`kVersion = 2`). Tasks are `ITaskDelegate*` (see [`TaskDelegateVtbl`]).
#[repr(C)]
pub struct TaskInterface {
    pub interface_version: u32,
    pub add_task: Option<unsafe extern "C" fn(task: *mut TaskDelegate)>,
    pub add_ui_task: Option<unsafe extern "C" fn(task: *mut TaskDelegate)>,
}

impl TaskInterface {
    pub const VERSION: u32 = 2;
}

/// A C++ `ITaskDelegate` object: a vtable pointer followed by the object's own state.
#[repr(C)]
pub struct TaskDelegate {
    pub vtable: *const TaskDelegateVtbl,
}

/// `ITaskDelegate`'s MSVC vtable, in declaration order (CommonLibF4): the virtual
/// destructor first (MSVC emits a scalar deleting destructor taking a flags argument;
/// bit 0 = free the memory), then `Run`.
#[repr(C)]
pub struct TaskDelegateVtbl {
    pub destroy: unsafe extern "C" fn(this: *mut TaskDelegate, flags: u32) -> *mut c_void,
    pub run: unsafe extern "C" fn(this: *mut TaskDelegate),
}

/// Object interface (`kVersion = 1`). The three returned objects' layouts are not
/// documented in CommonLibF4; they stay opaque.
#[repr(C)]
pub struct ObjectInterface {
    pub interface_version: u32,
    pub get_delay_functor_manager: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub get_object_registry: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub get_persistent_object_storage: Option<unsafe extern "C" fn() -> *mut c_void>,
}

impl ObjectInterface {
    pub const VERSION: u32 = 1;
}

/// Trampoline interface (`kVersion = 1`): executable memory near the game image
/// ("branch pool") or near the calling plugin ("local pool").
#[repr(C)]
pub struct TrampolineInterface {
    pub interface_version: u32,
    pub allocate_from_branch_pool: Option<unsafe extern "C" fn(handle: PluginHandle, size: usize) -> *mut c_void>,
    pub allocate_from_local_pool: Option<unsafe extern "C" fn(handle: PluginHandle, size: usize) -> *mut c_void>,
}

impl TrampolineInterface {
    pub const VERSION: u32 = 1;
}

/// Next-gen plugin version data, exported as `F4SEPlugin_Version`
/// (CommonLibF4 `origin/community` `PluginVersionData`).
#[repr(C)]
pub struct PluginVersionData {
    pub data_version: u32,
    pub plugin_version: PackedVersion,
    pub plugin_name: [c_char; 256],
    pub author: [c_char; 256],
    pub address_independence: u32,
    pub structure_independence: u32,
    /// Zero-terminated list of compatible runtime versions.
    pub compatible_versions: [PackedVersion; 16],
    pub xse_minimum: PackedVersion,
    pub reserved_non_breaking: u32,
    pub reserved_breaking: u32,
    pub reserved: [u8; 512],
}

impl PluginVersionData {
    pub const VERSION: u32 = 1;
}

/// `PluginVersionData::address_independence` bits (CommonLibF4 community).
pub mod address_independence {
    /// The plugin finds its addresses by signature scanning.
    pub const SIGNATURES: u32 = 1 << 0;
    /// The plugin uses the Address Library for 1.10.980 and later.
    pub const ADDRESS_LIBRARY_1_10_980: u32 = 1 << 1;
}

/// `PluginVersionData::structure_independence` bits (CommonLibF4 community).
pub mod structure_independence {
    /// The plugin does not use game structures.
    pub const NO_STRUCTS: u32 = 1 << 0;
    /// The plugin depends on the 1.10.980+ structure layouts.
    pub const LAYOUT_1_10_980: u32 = 1 << 1;
}

// Layouts: MSVC x64, which these #[repr(C)] structs reproduce.
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<F4seInterface>() == 0x30);
    assert!(offset_of!(F4seInterface, query_interface) == 0x10);
    assert!(offset_of!(F4seInterface, get_plugin_info) == 0x28);
    assert!(size_of::<PluginInfo>() == 0x18);
    assert!(offset_of!(PluginInfo, name) == 0x08);
    assert!(offset_of!(PluginInfo, version) == 0x10);
    assert!(size_of::<Message>() == 0x18);
    assert!(offset_of!(Message, data) == 0x10);
    assert!(size_of::<MessagingInterface>() == 0x20);
    assert!(size_of::<ScaleformInterface>() == 0x10);
    assert!(size_of::<SerializationInterface>() == 0x68);
    assert!(offset_of!(SerializationInterface, resolve_form_id) == 0x60);
    assert!(size_of::<PapyrusInterface>() == 0x18);
    assert!(size_of::<TaskInterface>() == 0x18);
    assert!(size_of::<ObjectInterface>() == 0x20);
    assert!(size_of::<TrampolineInterface>() == 0x18);
    // community static_asserts: total 0x45C.
    assert!(size_of::<PluginVersionData>() == 0x45C);
    assert!(offset_of!(PluginVersionData, plugin_name) == 0x008);
    assert!(offset_of!(PluginVersionData, author) == 0x108);
    assert!(offset_of!(PluginVersionData, address_independence) == 0x208);
    assert!(offset_of!(PluginVersionData, compatible_versions) == 0x210);
    assert!(offset_of!(PluginVersionData, xse_minimum) == 0x250);
    assert!(offset_of!(PluginVersionData, reserved) == 0x25C);
};

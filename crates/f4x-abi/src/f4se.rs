//! The F4SE-compatible plugin ABI.
//!
//! Source: CommonLibF4 `include/F4SE/Interfaces.h` (`detail::*` structs, the
//! `LoadInterface` / `MessagingInterface` enums, `PluginInfo`) and, for next-gen, the
//! next-gen fork (libxse/commonlibf4 at 40a93f69, MIT) `include/F4SE/Interfaces.h`:
//! `PluginVersionData`, `PreLoadInterface`, `GetSaveFolderName`, `AddTaskPermanent`.
//! `F4SEAPI` is `__cdecl`, which on x64 is the platform C ABI, hence `extern "C"`.
//!
//! Old-gen plugins export `F4SEPlugin_Query(*const F4seInterface, *mut PluginInfo) -> bool`
//! and `F4SEPlugin_Load(*const F4seInterface) -> bool` (CommonLibF4 `ExampleProject`).
//! Next-gen plugins export a `F4SEPlugin_Version` [`PluginVersionData`] (data, not a
//! function), `F4SEPlugin_Load` and optionally `F4SEPlugin_Preload` (next-gen
//! `F4SE_PLUGIN_VERSION` / `F4SE_PLUGIN_LOAD` / `F4SE_PLUGIN_PRELOAD`). Both entry points
//! get the same table: the fork's `PreLoadInterface` and `LoadInterface` both read it as
//! `F4SEInterface`, which next-gen extends by `GetSaveFolderName` ([`F4seInterfaceNg`]).
#![allow(missing_docs)]

use core::ffi::{c_char, c_void};

use crate::version::PackedVersion;

pub type PluginHandle = u32;

/// `F4SEPlugin_Query`.
pub type QueryFn = unsafe extern "C" fn(*const F4seInterface, *mut PluginInfo) -> bool;
/// `F4SEPlugin_Load`.
pub type LoadFn = unsafe extern "C" fn(*const F4seInterface) -> bool;
/// `F4SEPlugin_Preload` (next-gen; `bool F4SEPlugin_Preload(const PreLoadInterface*)`).
pub type PreloadFn = unsafe extern "C" fn(*const F4seInterface) -> bool;

pub const EXPORT_QUERY: &core::ffi::CStr = c"F4SEPlugin_Query";
pub const EXPORT_LOAD: &core::ffi::CStr = c"F4SEPlugin_Load";
pub const EXPORT_VERSION: &core::ffi::CStr = c"F4SEPlugin_Version";
pub const EXPORT_PRELOAD: &core::ffi::CStr = c"F4SEPlugin_Preload";

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

/// The next-gen table: [`F4seInterface`] plus `GetSaveFolderName` (the fork marks it
/// 0.7.1+). The name is the `My Games` subfolder (the fork's log path is
/// `Documents/My Games/<name>/F4SE/<plugin>.log`); the fork keeps the returned pointer
/// as a `string_view` for the whole process, so it must stay valid.
#[repr(C)]
pub struct F4seInterfaceNg {
    pub base: F4seInterface,
    pub get_save_folder_name: Option<unsafe extern "C" fn() -> *const c_char>,
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
    /// The only id the fork's `PreLoadInterface` names (besides `INVALID`): during
    /// `F4SEPlugin_Preload` plugins ask for the trampoline interface alone.
    pub const PRELOAD_IDS: &[u32] = &[TRAMPOLINE];
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

/// The next-gen task table: [`TaskInterface`] plus `AddTaskPermanent` (the fork keeps
/// `kVersion = 2`).
#[repr(C)]
pub struct TaskInterfaceNg {
    pub interface_version: u32,
    pub add_task: Option<unsafe extern "C" fn(task: *mut TaskDelegate)>,
    pub add_ui_task: Option<unsafe extern "C" fn(task: *mut TaskDelegate)>,
    pub add_task_permanent: Option<unsafe extern "C" fn(task: *mut TaskDelegate)>,
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

/// Next-gen plugin version data, exported as `F4SEPlugin_Version` (the next-gen fork's
/// and `origin/community`'s `PluginVersionData`, same layout).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginVersionData {
    /// [`PluginVersionData::VERSION`].
    pub data_version: u32,
    /// The plugin's own version, packed like a runtime version (`REL::Version::pack`).
    pub plugin_version: PackedVersion,
    pub plugin_name: [c_char; 256],
    pub author: [c_char; 256],
    /// [`address_independence`] bits.
    pub address_independence: u32,
    /// [`structure_independence`] bits.
    pub structure_independence: u32,
    /// Zero-terminated list of compatible runtime versions.
    pub compatible_versions: [PackedVersion; 16],
    /// The oldest F4SE the plugin works with (0: any).
    pub xse_minimum: PackedVersion,
    pub reserved_non_breaking: u32,
    pub reserved_breaking: u32,
    pub reserved: [u8; 512],
}

impl PluginVersionData {
    pub const VERSION: u32 = 1;

    /// Version data for a plugin to export (`const`, so it can initialise the exported
    /// static). `name` and `author` are truncated to 255 bytes; at most 15 versions are
    /// kept (the list is zero-terminated).
    #[must_use]
    pub const fn declare(
        name: &str,
        author: &str,
        plugin_version: PackedVersion,
        address_independence: u32,
        structure_independence: u32,
        compatible_versions: &[PackedVersion],
        xse_minimum: PackedVersion,
    ) -> Self {
        let mut versions = [PackedVersion(0); 16];
        let mut i = 0;
        while i < compatible_versions.len() && i < 15 {
            versions[i] = compatible_versions[i];
            i += 1;
        }
        PluginVersionData {
            data_version: Self::VERSION,
            plugin_version,
            plugin_name: c_chars(name),
            author: c_chars(author),
            address_independence,
            structure_independence,
            compatible_versions: versions,
            xse_minimum,
            reserved_non_breaking: 0,
            reserved_breaking: 0,
            reserved: [0; 512],
        }
    }

    /// The plugin name, up to its NUL (or all 256 bytes).
    #[must_use]
    pub fn name_bytes(&self) -> &[u8] {
        bytes_to_nul(&self.plugin_name)
    }

    /// The author, up to its NUL.
    #[must_use]
    pub fn author_bytes(&self) -> &[u8] {
        bytes_to_nul(&self.author)
    }

    /// The listed runtime versions, up to the terminating zero.
    pub fn listed_versions(&self) -> impl Iterator<Item = PackedVersion> + '_ {
        self.compatible_versions.iter().copied().take_while(|v| v.0 != 0)
    }

    /// Whether a host reporting F4SE version `f4se` on runtime `runtime` loads the plugin.
    ///
    /// The checks and their order are F4SE 0.7.9's on 1.11.240, observed black-box with
    /// patched copies of `tests/plugins/ng-abi` (`provenance/ng-plugin-abi.jsonl`); the
    /// bit meanings are the next-gen fork's:
    /// 1. `data_version` 0 is rejected (any other value is accepted);
    /// 2. an empty name is rejected;
    /// 3. the Address Library bit for 1.11.137 rejects the plugin on 1.11.240 even when the
    ///    runtime is listed or signature scanning is also declared;
    /// 4. the runtime must be listed (exact packed match, `sub` included; the list ends at
    ///    the first zero or after 16 entries), or the plugin must be version-independent:
    ///    `reserved_breaking` zero, addresses by signature scanning, and no structure use or
    ///    the structure generation of this runtime;
    /// 5. `xse_minimum` must not be newer than `f4se` (packed compare).
    ///
    /// # Errors
    /// Why it should not.
    pub fn compatibility(&self, runtime: PackedVersion, f4se: PackedVersion) -> Result<Compatibility, Rejection> {
        if self.data_version == 0 {
            return Err(Rejection::BadVersionData);
        }
        if self.name_bytes().is_empty() {
            return Err(Rejection::NoName);
        }
        if address_independence::outdated(self.address_independence, runtime) {
            return Err(Rejection::AddressLibraryOutdated);
        }
        let compatibility = if self.listed_versions().any(|v| v == runtime) {
            Compatibility::Listed
        } else {
            let addresses = address_independence::covers(self.address_independence, runtime);
            let structures = structure_independence::covers(self.structure_independence, runtime);
            if !(addresses && structures && self.reserved_breaking == 0) {
                return Err(Rejection::Runtime { addresses, structures, breaking: self.reserved_breaking });
            }
            Compatibility::Independent
        };
        if self.xse_minimum > f4se {
            return Err(Rejection::NeedsNewerF4se(self.xse_minimum));
        }
        Ok(compatibility)
    }
}

/// Why a plugin's [`PluginVersionData`] accepts the running game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    /// The runtime is in `compatible_versions`.
    Listed,
    /// Not listed, but the plugin finds its addresses in a way that works on this runtime
    /// and its structure use fits this runtime.
    Independent,
}

/// Why a plugin's [`PluginVersionData`] rejects the running game or this host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// `data_version` is 0.
    BadVersionData,
    /// `plugin_name` is empty.
    NoName,
    /// It declares an Address Library generation that does not cover this runtime.
    AddressLibraryOutdated,
    /// The runtime is not listed and the plugin is not version-independent: whether its
    /// addresses and structures would fit, and its `reserved_breaking` value.
    Runtime { addresses: bool, structures: bool, breaking: u32 },
    /// `xse_minimum` is newer than the F4SE version this host reports.
    NeedsNewerF4se(PackedVersion),
}

impl core::fmt::Display for Rejection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Rejection::BadVersionData => write!(f, "invalid version data (dataVersion 0)"),
            Rejection::NoName => write!(f, "its version data has no plugin name"),
            Rejection::AddressLibraryOutdated => {
                write!(f, "it uses an Address Library generation that does not cover this game version; it needs an update")
            }
            Rejection::Runtime { addresses, structures, breaking } => write!(
                f,
                "this game version is not in its compatible versions and it is not version-independent \
                 (addresses: {}, structures: {}{})",
                if addresses { "signature scanning" } else { "version-specific" },
                if structures { "fit" } else { "version-specific" },
                if breaking != 0 { ", breaking extension set" } else { "" },
            ),
            Rejection::NeedsNewerF4se(v) => write!(f, "it needs F4SE {v} or newer"),
        }
    }
}

/// `PluginVersionData::address_independence` bits (next-gen fork; `origin/community`
/// names bit 1 alone, for 1.10.980 and later, before 1.11.137 existed).
pub mod address_independence {
    use crate::version::{PackedVersion, runtime};

    /// The plugin finds its addresses by signature scanning (`UsesSigScanning`).
    pub const SIGNATURES: u32 = 1 << 0;
    /// The plugin uses the Address Library for 1.10.980 (up to 1.11.137).
    pub const ADDRESS_LIBRARY_1_10_980: u32 = 1 << 1;
    /// The plugin uses the Address Library for 1.11.137 and later (`UsesAddressLibrary`).
    pub const ADDRESS_LIBRARY_1_11_137: u32 = 1 << 2;

    /// Whether `flags` let the plugin find its addresses on `runtime` without listing it.
    /// Only signature scanning does on 1.11.240 (black-box, F4SE 0.7.9: neither Address
    /// Library bit nor any of bits 3-7 does).
    #[must_use]
    pub fn covers(flags: u32, runtime: PackedVersion) -> bool {
        flags & SIGNATURES != 0
            || (runtime >= runtime::V1_11_137 && runtime < runtime::V1_11_240 && flags & ADDRESS_LIBRARY_1_11_137 != 0)
            || (runtime >= runtime::V1_10_980 && runtime < runtime::V1_11_137 && flags & ADDRESS_LIBRARY_1_10_980 != 0)
    }

    /// Whether `flags` name an Address Library that is out of date for `runtime`: F4SE
    /// 0.7.9 rejects bit 2 on 1.11.240 whatever else the plugin declares (black-box).
    #[must_use]
    pub fn outdated(flags: u32, runtime: PackedVersion) -> bool {
        runtime >= runtime::V1_11_240 && flags & ADDRESS_LIBRARY_1_11_137 != 0
    }
}

/// `PluginVersionData::structure_independence` bits (next-gen fork; `origin/community`
/// names bit 1 alone).
pub mod structure_independence {
    use crate::version::{PackedVersion, runtime};

    /// The plugin does not use game structures (`HasNoStructUse`).
    pub const NO_STRUCTS: u32 = 1 << 0;
    /// The plugin depends on the 1.10.980 structure layouts (up to 1.11.137).
    pub const LAYOUT_1_10_980: u32 = 1 << 1;
    /// The plugin depends on the 1.11.137+ structure layouts (`IsLayoutDependent`).
    pub const LAYOUT_1_11_137: u32 = 1 << 2;

    /// Whether `flags` make the plugin's structure use fit `runtime` (black-box, F4SE
    /// 0.7.9 on 1.11.240: bits 0 and 2 do; bit 1 and bits 3-7 do not).
    #[must_use]
    pub fn covers(flags: u32, runtime: PackedVersion) -> bool {
        flags & NO_STRUCTS != 0
            || (runtime >= runtime::V1_11_137 && flags & LAYOUT_1_11_137 != 0)
            || (runtime >= runtime::V1_10_980 && runtime < runtime::V1_11_137 && flags & LAYOUT_1_10_980 != 0)
    }
}

const fn c_chars(text: &str) -> [c_char; 256] {
    let bytes = text.as_bytes();
    let mut out = [0 as c_char; 256];
    let mut i = 0;
    while i < bytes.len() && i < 255 {
        out[i] = bytes[i] as c_char;
        i += 1;
    }
    out
}

fn bytes_to_nul(chars: &[c_char; 256]) -> &[u8] {
    // SAFETY: c_char and u8 have the same size and alignment.
    let bytes = unsafe { core::slice::from_raw_parts(chars.as_ptr().cast::<u8>(), chars.len()) };
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..end]
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
    assert!(size_of::<TaskInterfaceNg>() == 0x20);
    assert!(offset_of!(TaskInterfaceNg, add_task_permanent) == 0x18);
    // Next-gen fork: GetSaveFolderName follows GetPluginInfo.
    assert!(offset_of!(F4seInterfaceNg, get_save_folder_name) == 0x30);
    assert!(size_of::<F4seInterfaceNg>() == 0x38);
    assert!(size_of::<ObjectInterface>() == 0x20);
    assert!(size_of::<TrampolineInterface>() == 0x18);
    // community static_asserts: total 0x45C.
    assert!(size_of::<PluginVersionData>() == 0x45C);
    assert!(offset_of!(PluginVersionData, plugin_name) == 0x008);
    assert!(offset_of!(PluginVersionData, author) == 0x108);
    assert!(offset_of!(PluginVersionData, address_independence) == 0x208);
    assert!(offset_of!(PluginVersionData, structure_independence) == 0x20C);
    assert!(offset_of!(PluginVersionData, compatible_versions) == 0x210);
    assert!(offset_of!(PluginVersionData, xse_minimum) == 0x250);
    assert!(offset_of!(PluginVersionData, reserved_non_breaking) == 0x254);
    assert!(offset_of!(PluginVersionData, reserved_breaking) == 0x258);
    assert!(offset_of!(PluginVersionData, reserved) == 0x25C);
};

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;
    use crate::version::runtime;

    const F4SE_0_7_9: PackedVersion = PackedVersion::new(0, 7, 9, 0);

    /// What the next-gen fork's plugin template declares (`res/commonlibf4-plugin.cpp.in`).
    fn template() -> PluginVersionData {
        PluginVersionData::declare(
            "MyPlugin",
            "me",
            PackedVersion::new(1, 2, 3, 0),
            address_independence::ADDRESS_LIBRARY_1_11_137,
            structure_independence::LAYOUT_1_11_137,
            &[runtime::V1_11_221],
            PackedVersion(0),
        )
    }

    #[test]
    fn declare_fills_strings_and_list() {
        let d = template();
        assert_eq!(d.name_bytes(), b"MyPlugin");
        assert_eq!(d.author_bytes(), b"me");
        assert_eq!(d.listed_versions().collect::<Vec<_>>(), [runtime::V1_11_221]);
        assert_eq!(d.data_version, 1);
        let long = PluginVersionData::declare(&"x".repeat(400), "", PackedVersion(0), 0, 0, &[PackedVersion(1); 20], PackedVersion(0));
        assert_eq!(long.name_bytes().len(), 255);
        assert_eq!(long.listed_versions().count(), 15);
    }

    /// The copies of `tools/ng_abi.py` and what F4SE 0.7.9 did with them on 1.11.240
    /// (black-box, `provenance/ng-plugin-abi.jsonl`).
    #[test]
    fn matches_f4se_0_7_9_on_1_11_240() {
        use address_independence::{ADDRESS_LIBRARY_1_10_980 as AL980, ADDRESS_LIBRARY_1_11_137 as AL137, SIGNATURES as SIG};
        use structure_independence::{LAYOUT_1_10_980 as L980, LAYOUT_1_11_137 as L137, NO_STRUCTS as NOS};
        const R221: PackedVersion = runtime::V1_11_221;
        const R240: PackedVersion = runtime::V1_11_240;
        let v = |addr: u32, structs: u32, list: &[PackedVersion]| {
            PluginVersionData::declare("p", "", PackedVersion(0), addr, structs, list, PackedVersion(0))
        };
        let ind = || v(SIG, NOS, &[]);
        let with = |mut d: PluginVersionData, f: &dyn Fn(&mut PluginVersionData)| {
            f(&mut d);
            d
        };
        use Compatibility::*;
        use Rejection::*;
        let runtime = |a: bool, s: bool, b: u32| Err(Runtime { addresses: a, structures: s, breaking: b });
        let cases: &[(&str, PluginVersionData, Result<Compatibility, Rejection>)] = &[
            ("template", template(), Err(AddressLibraryOutdated)),
            ("listed_exact", v(0, 0, &[R240]), Ok(Listed)),
            ("listed_sub", v(0, 0, &[PackedVersion::new(1, 11, 240, 5)]), runtime(false, false, 0)),
            ("listed_second", v(0, 0, &[R221, R240]), Ok(Listed)),
            ("listed_after_zero", v(0, 0, &[R221, PackedVersion(0), R240]), runtime(false, false, 0)),
            ("unlisted_noflags", v(0, 0, &[R221]), runtime(false, false, 0)),
            ("al980", v(AL980, NOS, &[R221]), runtime(false, true, 0)),
            ("al137", v(AL137, NOS, &[R221]), Err(AddressLibraryOutdated)),
            ("addr_bit3", v(1 << 3, NOS, &[R221]), runtime(false, true, 0)),
            ("layout980", v(SIG, L980, &[R221]), runtime(true, false, 0)),
            ("layout137", v(SIG, L137, &[R221]), Ok(Independent)),
            ("struct_bit3", v(SIG, 1 << 3, &[R221]), runtime(true, false, 0)),
            ("sig_only", v(SIG, 0, &[R221]), runtime(true, false, 0)),
            ("nostruct_only", v(0, NOS, &[R221]), runtime(false, true, 0)),
            ("independent", ind(), Ok(Independent)),
            ("xse_newer", with(ind(), &|d| d.xse_minimum = PackedVersion::new(0, 7, 10, 0)), Err(NeedsNewerF4se(PackedVersion::new(0, 7, 10, 0)))),
            ("xse_equal", with(ind(), &|d| d.xse_minimum = F4SE_0_7_9), Ok(Independent)),
            ("xse_sub", with(ind(), &|d| d.xse_minimum = PackedVersion::new(0, 7, 9, 1)), Err(NeedsNewerF4se(PackedVersion::new(0, 7, 9, 1)))),
            ("xse_older", with(ind(), &|d| d.xse_minimum = PackedVersion::new(0, 7, 8, 0)), Ok(Independent)),
            ("dataver2", with(ind(), &|d| d.data_version = 2), Ok(Independent)),
            ("dataver99", with(ind(), &|d| d.data_version = 99), Ok(Independent)),
            ("dataver0", with(ind(), &|d| d.data_version = 0), Err(BadVersionData)),
            ("breaking", with(ind(), &|d| d.reserved_breaking = 1), runtime(true, true, 1)),
            ("nonbreaking", with(ind(), &|d| d.reserved_non_breaking = 1), Ok(Independent)),
            ("noname", with(ind(), &|d| d.plugin_name = [0; 256]), Err(NoName)),
            ("listed_and_xse_newer", with(v(0, 0, &[R240]), &|d| d.xse_minimum = PackedVersion::new(0, 7, 10, 0)), Err(NeedsNewerF4se(PackedVersion::new(0, 7, 10, 0)))),
            ("listed_flags_old", v(AL980, L980, &[R240]), Ok(Listed)),
            ("al137_listed", v(AL137, L137, &[R240]), Err(AddressLibraryOutdated)),
            ("al137_sig", v(SIG | AL137, NOS, &[]), Err(AddressLibraryOutdated)),
            ("breaking_listed", with(v(0, 0, &[R240]), &|d| d.reserved_breaking = 1), Ok(Listed)),
            ("breaking_xse_newer", with(ind(), &|d| { d.reserved_breaking = 1; d.xse_minimum = PackedVersion::new(0, 7, 10, 0) }), runtime(true, true, 1)),
            ("noname_dataver0", with(ind(), &|d| { d.plugin_name = [0; 256]; d.data_version = 0 }), Err(BadVersionData)),
            ("al137_noname", with(v(AL137, L137, &[R221]), &|d| d.plugin_name = [0; 256]), Err(NoName)),
            ("al980_xse_newer", with(v(AL980, L137, &[R221]), &|d| d.xse_minimum = PackedVersion::new(0, 7, 10, 0)), runtime(false, true, 0)),
        ];
        for (name, data, expected) in cases {
            assert_eq!(data.compatibility(R240, F4SE_0_7_9), *expected, "{name}");
        }
        let mut sixteen = v(0, 0, &[]);
        sixteen.compatible_versions = [R221; 16];
        sixteen.compatible_versions[15] = R240;
        assert_eq!(sixteen.compatibility(R240, F4SE_0_7_9), Ok(Listed), "listed_16th");
    }

    #[test]
    fn earlier_next_gen_runtimes_follow_the_fork_bits() {
        let d = template();
        assert_eq!(d.compatibility(runtime::V1_11_221, F4SE_0_7_9), Ok(Compatibility::Listed));
        assert_eq!(d.compatibility(PackedVersion::new(1, 11, 191, 0), F4SE_0_7_9), Ok(Compatibility::Independent));
        assert!(d.compatibility(runtime::V1_10_984, F4SE_0_7_9).is_err());
    }
}

//! The game's UI: menus by name, the message queue that opens and closes them, and
//! menus implemented in Rust ([`register_custom_menu`]).
//!
//! Layouts and IDs from CommonLibF4 (MIT; `RE/Bethesda/UI.h`, `UIMessageQueue.h`,
//! `UIMessage.h`, `IMenu.h`, `BSScaleformManager.h`, `BSLock.h`; community branch for
//! `GameMenuBase`'s constructor):
//! - `UI::menuMap` (+0x1A8) is a `BSTHashMap<BSFixedString, UIMenuEntry>`: capacity
//!   `u32` at +0x0C, entries at +0x28; an entry is the key, the `UIMenuEntry` (`menu`,
//!   `create`, `staticUpdate`) and the chain pointer (+0x20, null when free), 0x28 bytes.
//!   A registered menu's `menu` is set only while it exists (menus are created on open).
//!   A global `BSReadWriteLock` guards the map;
//! - `IMenu`: `uiMovie` +0x40, `menuName` +0x50, `menuFlags` +0x58 (`kOnStack` = 1 << 6
//!   while open), `depthPriority` +0x64; `GameMenuBase` is 0xE0 bytes;
//! - `UIMessageQueue::AddMessage(name, UI_MESSAGE_TYPE)` queues `kShow` (1) / `kHide`
//!   (3); the UI handles the queue on its own thread.
//!
//! Menus are engine objects: read them on the thread the caller is on only through the
//! guarded helpers here, and touch their movies on the thread that advances them.

use std::alloc::Layout;
use std::collections::HashMap;
use std::ffi::{CString, c_char, c_void};
use std::mem::transmute;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use f4x_engine_sys::og_1_10_163_addresses as addr;

use crate::address::resolve;
use crate::layout::RE;
use crate::mem;
use crate::papyrus::FixedString;
use crate::scaleform::Movie;

/// `UI_MENU_FLAGS` (CommonLibF4 `IMenu.h`).
pub mod menu_flags {
    pub const PAUSES_GAME: u32 = 1 << 0;
    pub const ALWAYS_OPEN: u32 = 1 << 1;
    pub const USES_CURSOR: u32 = 1 << 2;
    pub const USES_MENU_CONTEXT: u32 = 1 << 3;
    pub const MODAL: u32 = 1 << 4;
    pub const ON_STACK: u32 = 1 << 6;
    pub const CUSTOM_RENDERING: u32 = 1 << 15;
}

/// `UI_MESSAGE_TYPE` (CommonLibF4 `UIMessage.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Message {
    Update = 0,
    Show = 1,
    Reshow = 2,
    Hide = 3,
    ForceHide = 4,
}

/// `UI_DEPTH_PRIORITY::kStandard`.
pub const DEPTH_STANDARD: u8 = 6;
/// `UserEvents::INPUT_CONTEXT_ID::kNone` (after the 33 contexts and `kTotal`).
const INPUT_CONTEXT_NONE: i32 = 34;

/// The `UI` singleton.
#[must_use]
pub fn ui() -> Option<*mut u8> {
    let global = resolve(&addr::UI_SINGLETON)?;
    // SAFETY: the UI* global.
    let ui: *mut u8 = unsafe { mem::read(global as *const u8, 0) };
    (!ui.is_null() && mem::readable(ui as usize, RE::UI::SIZE)).then_some(ui)
}

/// The menu map's read lock (`BSReadWriteLock::lock_read`; unlocking is a decrement of
/// the lock word at +4, as CommonLibF4's inline `unlock_read`).
struct MapReadLock(*mut u8);

impl MapReadLock {
    fn acquire() -> Option<MapReadLock> {
        let lock = resolve(&addr::UI_MENU_MAP_LOCK)? as *mut u8;
        let lock_read = resolve(&addr::BS_READ_WRITE_LOCK_LOCK_READ)?;
        // SAFETY: BSReadWriteLock::lock_read(this) on the engine's global lock.
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8)>(lock_read)(lock) };
        Some(MapReadLock(lock))
    }
}

impl Drop for MapReadLock {
    fn drop(&mut self) {
        // SAFETY: the lock word of the lock we hold for reading.
        let word = unsafe { &*(self.0.add(4) as *const AtomicU32) };
        word.fetch_sub(1, Ordering::AcqRel);
    }
}

const ENTRY_SIZE: usize = 0x28;
const ENTRY_NEXT: usize = 0x20;
const ENTRY_VALUE: usize = 0x08;

/// Run `f` on the `UIMenuEntry` registered as `name` (case-insensitive, like the
/// engine's `BSFixedString` keys), under the map's read lock.
fn with_entry<R>(name: &str, f: impl FnOnce(*mut u8) -> R) -> Option<R> {
    let ui = ui()?;
    let key = FixedString::new(name)?;
    let _lock = MapReadLock::acquire()?;
    // SAFETY: UI::menuMap (layout in the module docs).
    unsafe {
        let map = ui.add(RE::UI::menuMap);
        let capacity: u32 = mem::read(map, 0x0C);
        let entries: *mut u8 = mem::read(map, 0x28);
        if entries.is_null() || capacity == 0 || capacity > 1 << 16 || !mem::readable(entries as usize, capacity as usize * ENTRY_SIZE) {
            return None;
        }
        for i in 0..capacity as usize {
            let entry = entries.add(i * ENTRY_SIZE);
            if mem::read::<usize>(entry, ENTRY_NEXT) != 0 && mem::read::<*mut u8>(entry, 0) == key.as_raw() {
                return Some(f(entry.add(ENTRY_VALUE)));
            }
        }
    }
    None
}

/// Every registered menu name.
#[must_use]
pub fn registered_menus() -> Vec<String> {
    let mut out = Vec::new();
    let Some(ui) = ui() else { return out };
    let Some(_lock) = MapReadLock::acquire() else { return out };
    // SAFETY: as in `with_entry`.
    unsafe {
        let map = ui.add(RE::UI::menuMap);
        let capacity: u32 = mem::read(map, 0x0C);
        let entries: *mut u8 = mem::read(map, 0x28);
        if entries.is_null() || capacity > 1 << 16 {
            return out;
        }
        for i in 0..capacity as usize {
            let entry = entries.add(i * ENTRY_SIZE);
            if mem::read::<usize>(entry, ENTRY_NEXT) != 0 {
                out.push(mem::fixed_string(mem::read(entry, 0)).unwrap_or_default());
            }
        }
    }
    out
}

/// Whether a menu called `name` is registered with the UI (open or not).
#[must_use]
pub fn is_registered(name: &str) -> bool {
    with_entry(name, |_| ()).is_some()
}

/// A live `IMenu`, found by name. It is not referenced: use it right away, on the thread
/// the caller is on (the UI may destroy a closed menu at any later frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Menu(*mut u8);

// SAFETY: a handle; see the type's docs.
unsafe impl Send for Menu {}

impl Menu {
    #[must_use]
    pub fn as_ptr(self) -> *mut u8 {
        self.0
    }

    /// `IMenu::menuFlags`.
    #[must_use]
    pub fn flags(self) -> u32 {
        // SAFETY: a live IMenu (checked readable when found).
        unsafe { mem::read(self.0, RE::IMenu::menuFlags) }
    }

    /// `IMenu::OnStack`: the menu is open.
    #[must_use]
    pub fn on_stack(self) -> bool {
        self.flags() & menu_flags::ON_STACK != 0
    }

    /// The menu's movie (`IMenu::uiMovie`), null when it has none.
    #[must_use]
    pub fn movie(self) -> Movie {
        // SAFETY: a live IMenu; uiMovie is a Scaleform::Ptr (a raw pointer).
        Movie(unsafe { mem::read::<*mut c_void>(self.0, RE::IMenu::uiMovie) })
    }

    /// `IMenu::menuName`.
    #[must_use]
    pub fn name(self) -> String {
        // SAFETY: a live IMenu; a BSFixedString field.
        unsafe { mem::fixed_string(mem::read(self.0, RE::IMenu::menuName)) }.unwrap_or_default()
    }

    /// Whether this is one of ours ([`register_custom_menu`]).
    #[must_use]
    pub fn is_custom(self) -> bool {
        let Some(v) = vtables() else { return false };
        // SAFETY: readable checked first; the vtable pointer is the first field.
        mem::readable(self.0 as usize, OBJECT_SIZE)
            && unsafe { mem::read::<usize>(self.0, 0) } == v.primary_ptr()
            && unsafe { mem::read::<u64>(self.0, MAGIC_OFFSET) } == MAGIC
    }
}

/// The menu registered as `name`, if it exists now (menus exist while open, and briefly
/// around opening and closing).
#[must_use]
pub fn menu(name: &str) -> Option<Menu> {
    with_entry(name, |entry| {
        // SAFETY: UIMenuEntry::menu (Scaleform::Ptr<IMenu>, a raw pointer).
        unsafe { mem::read::<*mut u8>(entry, RE::UIMenuEntry::menu) }
    })
    .filter(|m| !m.is_null() && mem::readable(*m as usize, RE::IMenu::SIZE))
    .map(Menu)
}

/// Run `f` with the menu registered as `name`, holding a reference to it for the call
/// (taken under the menu map's lock), so a menu closing meanwhile stays alive. The
/// reference is the `IMenu`'s Scaleform refcount (`FunctionHandler`, `i32` at +8);
/// dropping the last one destroys the menu (vtable slot 0, delete), as Scaleform's
/// `RefCountImpl::Release` does.
pub fn with_menu<R>(name: &str, f: impl FnOnce(Menu) -> R) -> Option<R> {
    let menu = with_entry(name, |entry| {
        // SAFETY: UIMenuEntry::menu; the map lock keeps the entry's reference valid while
        // we add ours.
        unsafe {
            let menu = mem::read::<*mut u8>(entry, RE::UIMenuEntry::menu);
            if menu.is_null() || !mem::readable(menu as usize, RE::IMenu::SIZE) {
                return None;
            }
            (*(menu.add(8) as *const AtomicI32)).fetch_add(1, Ordering::AcqRel);
            Some(Menu(menu))
        }
    })??;
    struct Release(Menu);
    impl Drop for Release {
        fn drop(&mut self) {
            let m = self.0.0;
            // SAFETY: the reference we took; the last one deletes the menu.
            unsafe {
                if (*(m.add(8) as *const AtomicI32)).fetch_sub(1, Ordering::AcqRel) == 1 {
                    let vtbl: *const usize = mem::read(m, 0);
                    transmute::<usize, unsafe extern "C" fn(*mut u8, u32) -> *mut u8>(*vtbl)(m, 1);
                }
            }
        }
    }
    let guard = Release(menu);
    Some(f(guard.0))
}

/// Whether the menu `name` exists and is on the menu stack (`IMenu::OnStack`).
#[must_use]
pub fn is_open(name: &str) -> bool {
    menu(name).is_some_and(Menu::on_stack)
}

/// Queue a message for the menu `name` (`UIMessageQueue::AddMessage`).
pub fn add_message(name: &str, message: Message) -> bool {
    let (Some(global), Some(add)) = (resolve(&addr::UI_MESSAGE_QUEUE_SINGLETON), resolve(&addr::UI_MESSAGE_QUEUE_ADD_MESSAGE)) else {
        return false;
    };
    // SAFETY: the UIMessageQueue* global.
    let queue: *mut u8 = unsafe { mem::read(global as *const u8, 0) };
    let Some(key) = FixedString::new(name) else { return false };
    if queue.is_null() {
        return false;
    }
    // SAFETY: UIMessageQueue::AddMessage(this, const BSFixedString&, UI_MESSAGE_TYPE).
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *const FixedString, i32)>(add)(queue, &key, message as i32) };
    true
}

/// `UIMenuEntry::Create_t`: `IMenu* (const UIMessage&)`.
pub type CreateMenu = unsafe extern "C" fn(*const u8) -> *mut u8;

/// `UI::RegisterMenu(name, create, staticUpdate = nullptr)`.
pub fn register_menu(name: &str, create: CreateMenu) -> bool {
    let (Some(ui), Some(register)) = (ui(), resolve(&addr::UI_REGISTER_MENU)) else { return false };
    let Ok(name) = CString::new(name) else { return false };
    type F = unsafe extern "C" fn(*mut u8, *const c_char, CreateMenu, usize);
    // SAFETY: UI::RegisterMenu(this, const char*, Create_t*, StaticUpdate_t*).
    unsafe { transmute::<usize, F>(register)(ui, name.as_ptr(), create, 0) };
    true
}

// ---------------------------------------------------------------- custom menus

/// A menu implemented here: a `GameMenuBase` that shows the SWF `movie`.
#[derive(Clone, Debug)]
pub struct CustomMenu {
    /// The movie name given to `BSScaleformManager::LoadMovie` (relative to `Interface`,
    /// without `.swf`).
    pub movie: String,
    /// Path of the menu's code object in the movie (`IMenu::menuObj`), "" for none.
    pub root: String,
    pub menu_flags: u32,
    pub depth: u8,
    /// `GFx::Movie::ScaleModeType` (F4SE's `MenuData.movieFlags`).
    pub scale_mode: i32,
    /// F4SE's `MenuData.extendedFlags`: bit 0 tints the menu with the HUD color.
    pub extended_flags: u32,
    /// `GFx::Movie::AlignType`, applied after loading when set.
    pub align: Option<i32>,
    /// Called on the UI thread right after the menu and its movie are created.
    pub on_create: Option<fn(*mut u8, &CustomMenu)>,
}

/// Lower-case name -> definition.
static CUSTOM: Mutex<Option<HashMap<String, CustomMenu>>> = Mutex::new(None);

/// Whether `name` was registered through [`register_custom_menu`].
#[must_use]
pub fn is_custom(name: &str) -> bool {
    CUSTOM.lock().ok().and_then(|c| c.as_ref().map(|m| m.contains_key(&name.to_ascii_lowercase()))).unwrap_or(false)
}

/// Register a menu called `name` that shows `menu.movie`. Fails if a menu of that name
/// is already registered with the UI.
pub fn register_custom_menu(name: &str, menu: CustomMenu) -> bool {
    if is_registered(name) || vtables().is_none() {
        return false;
    }
    if let Ok(mut custom) = CUSTOM.lock() {
        custom.get_or_insert_with(HashMap::new).insert(name.to_ascii_lowercase(), menu);
    } else {
        return false;
    }
    register_menu(name, create_custom) && is_registered(name)
}

/// Our object: a `GameMenuBase` followed by our own fields.
const BASE_SIZE: usize = RE::GameMenuBase::SIZE;
const MAGIC_OFFSET: usize = BASE_SIZE;
const MAGIC: u64 = u64::from_le_bytes(*b"f4xMenu\0");
const OBJECT_SIZE: usize = BASE_SIZE + 0x10;

fn object_layout() -> Layout {
    // SAFETY-free: constant, valid size and alignment.
    Layout::from_size_align(OBJECT_SIZE, 16).unwrap_or(Layout::new::<[u64; 2]>())
}

/// Each copy keeps the engine's complete-object-locator pointer in front (`[-1]`), so
/// RTTI on our menus reads as `GameMenuBase`.
struct Vtables {
    primary: Box<[usize]>,
    secondary: Box<[usize]>,
    /// The engine's `GameMenuBase` destructor (scalar deleting, `flags & 1` frees).
    base_dtor: usize,
    /// The engine's `IMenu::AdvanceMovie`.
    advance: usize,
}

impl Vtables {
    fn primary_ptr(&self) -> usize {
        self.primary.as_ptr() as usize + 8
    }

    fn secondary_ptr(&self) -> usize {
        self.secondary.as_ptr() as usize + 8
    }
}

/// Our vtables: copies of the engine's `GameMenuBase` ones with our destructor (both
/// subobjects) and `AdvanceMovie`. `IMenu::AdvanceMovie` comes from `IMenu`'s own
/// vtable (`GameMenuBase`'s slot may be wrapped by an observer).
fn vtables() -> Option<&'static Vtables> {
    static V: OnceLock<Option<Vtables>> = OnceLock::new();
    V.get_or_init(|| {
        let game = crate::game();
        let rtti = game.rtti();
        let base = game.base();
        let find = |class: &str, offset: u32| rtti.vtables_of(class).find(|v| v.offset == offset).map(|v| base + v.vtable_rva as usize);
        let primary = find(RE::GameMenuBase::RTTI, 0)?;
        let secondary = find(RE::GameMenuBase::RTTI, RE::IMenu::bases::BSInputEventUser as u32)?;
        let imenu = find(RE::IMenu::RTTI, 0)?;
        // SAFETY: vtables in the game's .rdata, with the CommonLibF4 slot counts.
        let copy = |vtable: usize, len: usize| -> Box<[usize]> { (0..=len).map(|i| unsafe { mem::read::<usize>((vtable - 8) as *const u8, 8 * i) }).collect() };
        let mut p = copy(primary, RE::GameMenuBase::vtbl::LEN);
        let mut s = copy(secondary, RE::BSInputEventUser::vtbl::LEN);
        let base_dtor = p[1 + RE::IMenu::vtbl::dtor];
        // SAFETY: as above.
        let advance = unsafe { mem::read::<usize>(imenu as *const u8, 8 * RE::IMenu::vtbl::AdvanceMovie) };
        p[1 + RE::IMenu::vtbl::dtor] = custom_dtor as *const () as usize;
        p[1 + RE::IMenu::vtbl::AdvanceMovie] = custom_advance as *const () as usize;
        s[1 + RE::BSInputEventUser::vtbl::dtor] = custom_dtor_input_user as *const () as usize;
        Some(Vtables { primary: p, secondary: s, base_dtor, advance })
    })
    .as_ref()
}

unsafe extern "C" fn create_custom(message: *const u8) -> *mut u8 {
    std::panic::catch_unwind(|| {
        // SAFETY: the engine's UIMessage; `menu` is a BSFixedString.
        let name = unsafe { mem::fixed_string(mem::read(message, RE::UIMessage::menu)) }.unwrap_or_default();
        let def = CUSTOM.lock().ok().and_then(|c| c.as_ref().and_then(|m| m.get(&name.to_ascii_lowercase()).cloned()));
        let (Some(def), Some(v)) = (def, vtables()) else { return std::ptr::null_mut() };
        // SAFETY: a fresh zeroed block, initialised as GameMenuBase's constructor does.
        unsafe { construct(&name, &def, v) }
    })
    .unwrap_or(std::ptr::null_mut())
}

unsafe fn construct(name: &str, def: &CustomMenu, v: &Vtables) -> *mut u8 {
    // SAFETY: non-zero size.
    let menu = unsafe { std::alloc::alloc_zeroed(object_layout()) };
    if menu.is_null() {
        return menu;
    }
    let put_string = |offset: usize, text: &str| {
        if let Some(s) = FixedString::new(text) {
            // SAFETY: a BSFixedString field of our object; it takes the reference.
            unsafe { menu.add(offset).cast::<*mut u8>().write(s.into_raw()) };
        }
    };
    // SAFETY: fields of our block (IMenu / GameMenuBase layout).
    unsafe {
        menu.cast::<usize>().write(v.primary_ptr());
        menu.add(8).cast::<i32>().write(1); // FunctionHandler refcount
        menu.add(RE::IMenu::bases::BSInputEventUser).cast::<usize>().write(v.secondary_ptr());
        menu.add(RE::IMenu::bases::BSInputEventUser + RE::BSInputEventUser::inputEventHandlingEnabled).write(1);
        put_string(RE::IMenu::customRendererName, "FlatScreenModel");
        put_string(RE::IMenu::menuName, name);
        menu.add(RE::IMenu::menuFlags).cast::<u32>().write(def.menu_flags);
        menu.add(RE::IMenu::passesTopMenuTest).write(1);
        menu.add(RE::IMenu::menuCanBeVisible).write(1);
        menu.add(RE::IMenu::depthPriority).write(def.depth);
        menu.add(RE::IMenu::inputContext).cast::<i32>().write(INPUT_CONTEXT_NONE);
        menu.add(MAGIC_OFFSET).cast::<u64>().write(MAGIC);
    }
    if !def.movie.is_empty() && !load_movie(menu, &def.movie, &def.root, def.scale_mode) {
        log::warn!(target: "f4x::ui", "{name}: could not load movie {:?}", def.movie);
    }
    let movie = Menu(menu).movie();
    if let Some(align) = def.align
        && !movie.0.is_null()
    {
        // SAFETY: Movie::SetViewAlignment (vtable slot 0x10).
        unsafe {
            let vtbl: *const usize = mem::read(movie.0 as *const u8, 0);
            transmute::<usize, unsafe extern "C" fn(*mut c_void, i32)>(*vtbl.add(0x10))(movie.0, align);
        }
    }
    // SAFETY: our menu, created with its movie on this (the UI) thread.
    unsafe { set_up_code_object(menu, def) };
    if let Some(f) = def.on_create {
        f(menu, def);
    }
    menu
}

/// What F4SE adds to its custom menus on creation (black-box, F4SE 0.6.23;
/// `provenance/scaleform-features.jsonl`), done once the movie is loaded:
/// - the code object (`IMenu::menuObj`, the value at the registered root path) gets
///   `menuFlags`, `movieFlags` and `extendedFlags` as uint members, in that order. F4SE
///   crashes when the path gives no object (`""`, `"root"`); f4x then skips all of this;
/// - `GameMenuBase::filterHolder` is a `BSGFxShaderFXTarget` on the code object, built by
///   the engine's constructor in game memory (the menu's destructor deletes it);
/// - `extendedFlags` bit 0: the holder is tinted with `CreateAndSetFiltersToHUD(
///   kGameplayHUDColor, 1.0)`;
/// - with `kCustomRendering` in the menu flags the holder is the one entry of
///   `shaderFXObjects` (capacity 10), so the renderer applies its tint.
///
/// # Safety
/// `menu` is our live custom menu, on the thread that owns its movie.
unsafe fn set_up_code_object(menu: *mut u8, def: &CustomMenu) {
    use crate::scaleform::Value;
    // SAFETY: IMenu::menuObj, a GFx::Value filled by LoadMovie.
    let code = unsafe { &mut *(menu.add(RE::IMenu::menuObj) as *mut Value) };
    if !code.is_object() {
        return;
    }
    code.set_member("menuFlags", &Value::uint(def.menu_flags));
    code.set_member("movieFlags", &Value::uint(def.scale_mode as u32));
    code.set_member("extendedFlags", &Value::uint(def.extended_flags));
    let (Some(ctor), Some(holder)) = (resolve(&addr::SHADER_FX_TARGET_CTOR_VALUE), game_alloc(SHADER_FX_TARGET_SIZE)) else {
        log::warn!(target: "f4x::ui", "custom menu: no filter holder (constructor or allocator not found)");
        return;
    };
    // SAFETY: BSGFxShaderFXTarget::BSGFxShaderFXTarget(this, const GFx::Value&) on fresh game
    // memory; the holder is owned by GameMenuBase::filterHolder from here on.
    unsafe {
        transmute::<usize, unsafe extern "C" fn(*mut u8, *const Value) -> *mut u8>(ctor)(holder, code);
        menu.add(RE::GameMenuBase::filterHolder).cast::<*mut u8>().write(holder);
    }
    if def.extended_flags & 1 != 0
        && let Some(to_hud) = resolve(&addr::SHADER_FX_TARGET_FILTERS_TO_HUD)
    {
        // SAFETY: CreateAndSetFiltersToHUD(this, HUDColorTypes, float scale).
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, u32, f32)>(to_hud)(holder, HUD_COLOR_GAMEPLAY, 1.0) };
    }
    if def.menu_flags & menu_flags::CUSTOM_RENDERING != 0
        && let Some(data) = game_alloc(SHADER_FX_OBJECTS_CAPACITY as usize * 8)
    {
        // SAFETY: GameMenuBase::shaderFXObjects, an empty BSTArray<BSGFxShaderFXTarget*>
        // (data, capacity u32 at +8, size u32 at +0x10); game memory, freed by its destructor.
        unsafe {
            data.cast::<*mut u8>().write(holder);
            let array = menu.add(RE::GameMenuBase::shaderFXObjects);
            array.cast::<*mut u8>().write(data);
            array.add(8).cast::<u32>().write(SHADER_FX_OBJECTS_CAPACITY);
            array.add(0x10).cast::<u32>().write(1);
        }
    }
}

/// `sizeof(BSGFxShaderFXTarget)` (CommonLibF4 community `IMenu.h`; the engine's
/// `make_unique` of it allocates 0xB0 bytes).
const SHADER_FX_TARGET_SIZE: usize = 0xB0;
/// `HUDColorTypes::kGameplayHUDColor` (CommonLibF4 community `IMenu.h`).
const HUD_COLOR_GAMEPLAY: u32 = 2;
/// The capacity F4SE's custom menus show for their one `shaderFXObjects` entry.
const SHADER_FX_OBJECTS_CAPACITY: u32 = 10;

/// `size` bytes from the game's heap (`MemoryManager::Allocate(size, 0, false)`, as the
/// engine's own `make_unique` does), zeroed.
fn game_alloc(size: usize) -> Option<*mut u8> {
    let (singleton, allocate) = (resolve(&addr::MEMORY_MANAGER_SINGLETON)?, resolve(&addr::MEMORY_MANAGER_ALLOCATE)?);
    // SAFETY: MemoryManager::GetSingleton(); Allocate(this, size, alignment, alignmentRequired).
    let p = unsafe {
        let manager = transmute::<usize, unsafe extern "C" fn() -> *mut c_void>(singleton)();
        transmute::<usize, unsafe extern "C" fn(*mut c_void, usize, u32, bool) -> *mut u8>(allocate)(manager, size, 0, false)
    };
    if p.is_null() {
        return None;
    }
    // SAFETY: a fresh allocation of `size` bytes.
    unsafe { std::ptr::write_bytes(p, 0, size) };
    Some(p)
}

/// `BSScaleformManager::LoadMovie(menu, menu.uiMovie, name, menuObjPath, scaleMode, 0)`.
fn load_movie(menu: *mut u8, movie: &str, root: &str, scale_mode: i32) -> bool {
    let (Some(global), Some(load)) = (resolve(&addr::SCALEFORM_MANAGER_SINGLETON), resolve(&addr::SCALEFORM_MANAGER_LOAD_MOVIE)) else {
        return false;
    };
    // SAFETY: the BSScaleformManager* global.
    let manager: *mut u8 = unsafe { mem::read(global as *const u8, 0) };
    let (Ok(movie), Ok(root)) = (CString::new(movie), CString::new(root)) else { return false };
    if manager.is_null() {
        return false;
    }
    type F = unsafe extern "C" fn(*mut u8, *mut u8, *mut *mut c_void, *const c_char, *const c_char, i32, f32) -> bool;
    // SAFETY: LoadMovie(this, IMenu&, Ptr<Movie>&, const char*, const char*, ScaleModeType, float).
    unsafe {
        let slot = menu.add(RE::IMenu::uiMovie).cast::<*mut c_void>();
        let root_ptr = if root.as_bytes().is_empty() { std::ptr::null() } else { root.as_ptr() };
        transmute::<usize, F>(load)(manager, menu, slot, movie.as_ptr(), root_ptr, scale_mode, 0.0)
    }
}

/// Scalar deleting destructor: the engine's `GameMenuBase` destructor without freeing,
/// then our own deallocation.
unsafe extern "C" fn custom_dtor(this: *mut u8, flags: u32) -> *mut u8 {
    let Some(v) = vtables() else { return this };
    // SAFETY: our live object; the engine destructor with flags 0 only destroys members.
    unsafe {
        transmute::<usize, unsafe extern "C" fn(*mut u8, u32) -> *mut u8>(v.base_dtor)(this, 0);
        if flags & 1 != 0 {
            std::alloc::dealloc(this, object_layout());
        }
    }
    this
}

/// The same, through the `BSInputEventUser` subobject.
unsafe extern "C" fn custom_dtor_input_user(this: *mut u8, flags: u32) -> *mut u8 {
    // SAFETY: `this` is our object's BSInputEventUser subobject.
    unsafe { custom_dtor(this.sub(RE::IMenu::bases::BSInputEventUser), flags) }
}

/// `IMenu::AdvanceMovie` (the copied `GameMenuBase` slot may be an observer's wrapper,
/// which only advances the vtables it knows).
unsafe extern "C" fn custom_advance(this: *mut u8, delta: f32, time: u64) {
    if let Some(v) = vtables() {
        // SAFETY: the engine's IMenu::AdvanceMovie on our IMenu.
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, f32, u64)>(v.advance)(this, delta, time) };
    }
}

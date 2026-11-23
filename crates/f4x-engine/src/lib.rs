//! Rust access to Fallout 4 engine objects, built only on engine internals (never on
//! Papyrus natives).
//!
//! Phase 2, first slice. Everything here works **without any per-build function
//! address**:
//! - layouts come from the generated tables (`f4x-engine-sys`, CommonLibF4 via clang);
//! - classes and vtables are found by RTTI name;
//! - singletons are found by scanning the game's writable data for a global that points
//!   at an object of the right class.
//!
//! Calls that need non-virtual engine functions (lookups by the engine's own index,
//! creating forms, …) arrive once recipes for those functions exist.
//!
//! **Threading:** engine objects belong to the game's main thread. Read them there, e.g.
//! from an in-game test or through the agent's `on_main`.

pub use f4x_engine_sys::og_1_10_163 as layout;

pub mod form_id;
pub mod form_type;

#[cfg(windows)]
pub mod address;
#[cfg(windows)]
pub mod console;
#[cfg(windows)]
pub mod control_map;
#[cfg(windows)]
pub mod controls;
#[cfg(windows)]
pub mod data;
#[cfg(windows)]
pub mod form;
#[cfg(windows)]
pub mod game;
#[cfg(windows)]
pub mod matswap;
#[cfg(windows)]
pub mod mem;
#[cfg(windows)]
pub mod objref;
#[cfg(windows)]
pub mod papyrus;
#[cfg(windows)]
pub mod papyrus_value;
#[cfg(windows)]
pub mod refr;
#[cfg(windows)]
pub mod scaleform;
#[cfg(windows)]
pub mod scaleform_ext;
#[cfg(windows)]
pub mod translation;
#[cfg(windows)]
pub mod ui;

#[cfg(windows)]
pub use data::{DataHandler, File};
#[cfg(windows)]
pub use form::Form;
#[cfg(windows)]
pub use game::{Game, game};
#[cfg(windows)]
pub use refr::{Point3, Reference, player};
pub use form_id::FormId;

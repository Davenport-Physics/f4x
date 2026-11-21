//! PE32+ images and MSVC RTTI, for a file on disk or the image mapped in memory.
//!
//! - [`Image`]: read-only access by RVA to an image's sections.
//! - [`PeFile`]: an image parsed from bytes (e.g. `Fallout4.exe` on disk).
//! - [`MappedImage`]: the image loaded in this process (Windows only).
//! - [`rtti`]: complete-object-locator decoding, demangling, and [`rtti::RttiIndex`], which
//!   finds every polymorphic class's vtables by name with no per-build addresses.
//!
//! Format references: Microsoft PE/COFF specification; MSVC x64 RTTI layout as
//! described publicly (e.g. "Reversing Microsoft Visual C++ Part II: Classes, Methods
//! and RTTI", openrce.org, 2006). Ported from the f4se-rs `pe.rs`/`rtti.rs` readers,
//! which contain no F4SE-derived material.

mod image;
mod pefile;
pub mod rtti;

pub use image::{Image, Section};
#[cfg(windows)]
pub use image::MappedImage;
pub use pefile::{Import, PeError, PeFile};

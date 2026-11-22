//! Lossless reader and writer for Fallout 4 compiled Papyrus scripts (`.pex`, version 3.9).
//!
//! [`Pex::parse`] builds a typed tree ([`ast`]) in which every string is a [`StringId`]
//! into the file's [`StringTable`]; [`Pex::to_bytes`] writes it back. For any vanilla
//! script `Pex::parse(b)?.to_bytes()? == b`.
//!
//! Layout notes (sources in `provenance/formats.jsonl`, `format/pex/*`):
//! - Base layout as documented by UESP for Skyrim, but little-endian.
//! - Fallout 4 additions: a const flag on objects, variables and struct members; structs
//!   in each object; property groups and struct member orders at the end of the debug
//!   info; opcodes 0x24..=0x2E.
//! - Object size fields are recomputed on write, in the convention the file used (with or
//!   without counting the field itself); parsing rejects sizes that fit neither.
//!
//! ```
//! # fn demo(bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
//! let pex = f4x_pex::Pex::parse(bytes)?;
//! for sig in pex.native_functions() {
//!     println!("{sig}");
//! }
//! assert_eq!(pex.to_bytes()?, bytes);
//! # Ok(()) }
//! ```

pub mod ast;
mod error;
mod opcode;
mod query;
mod read;
mod strings;
mod write;

use std::borrow::Cow;

pub use ast::*;
pub use error::{ParseError, ParseErrorKind, WriteError};
pub use opcode::Opcode;
pub use query::{FunctionKind, FunctionRef, FunctionSignature, Param};
pub use strings::{PexString, StringId, StringTable};

impl Pex {
    /// Parses a complete `.pex` file.
    pub fn parse(bytes: &[u8]) -> Result<Pex, ParseError> {
        read::parse(bytes)
    }

    /// Serializes the tree. Object size fields are recomputed.
    pub fn to_bytes(&self) -> Result<Vec<u8>, WriteError> {
        write::write(self)
    }

    /// The text of a string-table entry (see [`StringId::resolve`]).
    pub fn str(&self, id: StringId) -> Cow<'_, str> {
        id.resolve(&self.strings)
    }

    /// Builds a body-less function, interning its strings (case-insensitively, like the
    /// compiler). With [`Function::FLAG_NATIVE`] it is a native declaration; with no flags
    /// it is an empty function such as an event stub. `params` are `(type, name)` pairs;
    /// `return_type` is `"None"` for no return value.
    pub fn declare_function(&mut self, return_type: &str, params: &[(&str, &str)], flags: u8) -> Function {
        let mut intern = |text: &str| self.strings.intern_ignore_case(text);
        Function {
            return_type: intern(return_type),
            doc_string: intern(""),
            user_flags: 0,
            flags,
            params: params
                .iter()
                .map(|(ty, name)| TypedName { type_name: intern(ty), name: intern(name) })
                .collect(),
            locals: Vec::new(),
            instructions: Vec::new(),
        }
    }
}

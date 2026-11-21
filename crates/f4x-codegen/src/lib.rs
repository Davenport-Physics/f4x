//! f4x-codegen: machine-extracts CommonLibF4's engine type information with clang
//! (spike S2) and will grow into the Rust binding generator.
//!
//! - [`clang`]: toolchain discovery and clang invocations for `x86_64-pc-windows-msvc`.
//! - [`thirdparty`]: pinned, hash-checked header-only dependencies of CommonLibF4.
//! - [`records`]: parser for `-fdump-record-layouts` output.
//! - [`ast`]: streaming JSON AST reader, location tracking and analyses.
//! - [`dump`]: the `dump` subcommand that ties them together.
//! - [`tables`]: per-build offset tables for `f4x-engine-sys`.

pub mod ast;
pub mod clang;
pub mod dump;
pub mod tables;
pub mod records;
pub mod thirdparty;

use std::path::PathBuf;

/// The workspace root (two levels above this crate's manifest).
pub fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .expect("crates/f4x-codegen lives two levels below the workspace root")
        .to_path_buf()
}

/// Default output directory: `<workspace>/target/clangdump`.
pub fn default_out_dir() -> PathBuf {
    workspace_root().join("target/clangdump")
}

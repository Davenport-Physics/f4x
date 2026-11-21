//! `#[repr(C)]` definitions shared by the f4x runtime and plugins.
//!
//! - `f4se`: the F4SE-compatible plugin ABI. Derived from CommonLibF4
//!   `include/F4SE/Interfaces.h` (MIT); see `provenance/abi.jsonl`.
//! - `service`: the f4x service table (append-only, versioned by `struct_size`).
#![no_std]

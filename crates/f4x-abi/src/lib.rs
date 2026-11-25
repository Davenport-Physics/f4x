//! `#[repr(C)]` definitions shared by the f4x runtime and plugins.
//!
//! - [`version`]: packed version numbers used throughout the F4SE ABI.
//! - [`f4se`]: the F4SE-compatible plugin ABI (old-gen, plus the next-gen additions).
//!
//! Every layout here is derived from CommonLibF4 (MIT) and recorded in
//! `provenance/abi.jsonl` and `provenance/ng-plugin-abi.jsonl`. Members that F4SE added
//! beyond what CommonLibF4 documents are deliberately absent.
#![no_std]

pub mod f4se;
pub mod service;
pub mod version;

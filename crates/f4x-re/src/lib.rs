//! Our own analysis of `Fallout4.exe` builds (provenance tag `re`): a function index with
//! per-function features, OG->NG function matching, and disassembly/xref helpers for
//! checking results by hand. Host-only; reads the user's exe locally and never writes
//! any of its bytes into the repository.

pub mod index;
pub mod matcher;
pub mod compare;

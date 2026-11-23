//! `Utility`: `Var` <-> `Var[]` (CommonLibF4 `F4SEStub/runtime/src/Papyrus/Utility.h`).

use f4x_engine::papyrus_value::PValue;

use super::Entry;

// Observed under F4SE: an empty array packs to a `Var` holding None, and unpacking a
// `Var` that holds no array returns None (not an empty array).
pub(crate) const NATIVES: &[Entry] = &[
    ("Utility", "VarArrayToVar", |c| {
        let inner = match c.value(0) {
            PValue::Array(items) if !items.is_empty() => PValue::Array(items),
            _ => PValue::None,
        };
        c.ret_value(&PValue::Var(Box::new(inner)))
    }),
    ("Utility", "VarToVarArray", |c| {
        let items = match c.value(0) {
            PValue::Var(inner) => match *inner {
                PValue::Array(items) => items,
                _ => return c.ret_value(&PValue::None),
            },
            PValue::Array(items) => items,
            _ => return c.ret_value(&PValue::None),
        };
        // Elements come back as `Var`s.
        let items = items.into_iter().map(|v| if matches!(v, PValue::Var(_)) { v } else { PValue::Var(Box::new(v)) }).collect();
        c.ret_value(&PValue::Array(items))
    }),
];

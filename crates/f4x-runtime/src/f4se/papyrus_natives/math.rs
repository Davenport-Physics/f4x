//! `Math`: bit operations and exp/log (CommonLibF4 `F4SEStub/runtime/src/Papyrus/Math.h`).

use f4x_engine::papyrus::NativeCall;

use super::Entry;

fn int2(c: &mut NativeCall, f: fn(u32, u32) -> u32) -> Result<(), String> {
    let (a, b) = (c.int(0) as u32, c.int(1) as u32);
    c.ret_int(f(a, b) as i32);
    Ok(())
}

pub(crate) const NATIVES: &[Entry] = &[
    ("Math", "LeftShift", |c| int2(c, |a, b| a.checked_shl(b).unwrap_or(0))),
    ("Math", "RightShift", |c| int2(c, |a, b| a.checked_shr(b).unwrap_or(0))),
    ("Math", "LogicalAnd", |c| int2(c, |a, b| a & b)),
    ("Math", "LogicalOr", |c| int2(c, |a, b| a | b)),
    ("Math", "LogicalXor", |c| int2(c, |a, b| a ^ b)),
    ("Math", "LogicalNot", |c| {
        let a = c.int(0) as u32;
        c.ret_int(!a as i32);
        Ok(())
    }),
    ("Math", "Exp", |c| {
        let a = c.float(0);
        c.ret_float(a.exp());
        Ok(())
    }),
    ("Math", "Log", |c| {
        let a = c.float(0);
        c.ret_float(a.ln());
        Ok(())
    }),
];

//! Papyrus VM opcodes and their operand counts.
//!
//! 0x00..=0x23 are the opcodes shared with Skyrim (UESP "Compiled Script File Format").
//! 0x24..=0x2E were added in Fallout 4; their numbering and operand counts were confirmed
//! against the vanilla scripts (see `provenance/formats.jsonl`, `format/pex/opcodes`).
//! `nop`, `array_rfindelement`, `array_rfindstruct` and `array_removelast` never occur in
//! vanilla code, so their counts mirror their siblings and are unconfirmed by data.

use std::fmt;

macro_rules! opcodes {
    ($($variant:ident = $byte:literal, $mnemonic:literal, $fixed:literal, $varargs:literal;)*) => {
        /// One VM instruction kind.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Opcode {
            $($variant = $byte,)*
        }

        impl Opcode {
            /// Every opcode, in byte order.
            pub const ALL: &'static [Opcode] = &[$(Opcode::$variant,)*];

            pub fn from_byte(byte: u8) -> Option<Opcode> {
                match byte {
                    $($byte => Some(Opcode::$variant),)*
                    _ => None,
                }
            }

            /// Lower-case assembler mnemonic, e.g. `callmethod`.
            pub fn mnemonic(self) -> &'static str {
                match self {
                    $(Opcode::$variant => $mnemonic,)*
                }
            }

            /// Operands always present, before any variable-length tail.
            pub fn fixed_args(self) -> usize {
                match self {
                    $(Opcode::$variant => $fixed,)*
                }
            }

            /// True if the fixed operands are followed by an integer count and that many
            /// further operands (the call opcodes).
            pub fn has_varargs(self) -> bool {
                match self {
                    $(Opcode::$variant => $varargs,)*
                }
            }
        }
    };
}

opcodes! {
    Nop = 0x00, "nop", 0, false;
    IAdd = 0x01, "iadd", 3, false;
    FAdd = 0x02, "fadd", 3, false;
    ISub = 0x03, "isub", 3, false;
    FSub = 0x04, "fsub", 3, false;
    IMul = 0x05, "imul", 3, false;
    FMul = 0x06, "fmul", 3, false;
    IDiv = 0x07, "idiv", 3, false;
    FDiv = 0x08, "fdiv", 3, false;
    IMod = 0x09, "imod", 3, false;
    Not = 0x0A, "not", 2, false;
    INeg = 0x0B, "ineg", 2, false;
    FNeg = 0x0C, "fneg", 2, false;
    Assign = 0x0D, "assign", 2, false;
    Cast = 0x0E, "cast", 2, false;
    CmpEq = 0x0F, "cmp_eq", 3, false;
    CmpLt = 0x10, "cmp_lt", 3, false;
    CmpLe = 0x11, "cmp_le", 3, false;
    CmpGt = 0x12, "cmp_gt", 3, false;
    CmpGe = 0x13, "cmp_ge", 3, false;
    Jmp = 0x14, "jmp", 1, false;
    JmpT = 0x15, "jmpt", 2, false;
    JmpF = 0x16, "jmpf", 2, false;
    CallMethod = 0x17, "callmethod", 3, true;
    CallParent = 0x18, "callparent", 2, true;
    CallStatic = 0x19, "callstatic", 3, true;
    Return = 0x1A, "return", 1, false;
    StrCat = 0x1B, "strcat", 3, false;
    PropGet = 0x1C, "propget", 3, false;
    PropSet = 0x1D, "propset", 3, false;
    ArrayCreate = 0x1E, "array_create", 2, false;
    ArrayLength = 0x1F, "array_length", 2, false;
    ArrayGetElement = 0x20, "array_getelement", 3, false;
    ArraySetElement = 0x21, "array_setelement", 3, false;
    ArrayFindElement = 0x22, "array_findelement", 4, false;
    ArrayRFindElement = 0x23, "array_rfindelement", 4, false;
    Is = 0x24, "is", 3, false;
    StructCreate = 0x25, "struct_create", 1, false;
    StructGet = 0x26, "struct_get", 3, false;
    StructSet = 0x27, "struct_set", 3, false;
    ArrayFindStruct = 0x28, "array_findstruct", 5, false;
    ArrayRFindStruct = 0x29, "array_rfindstruct", 5, false;
    ArrayAdd = 0x2A, "array_add", 3, false;
    ArrayInsert = 0x2B, "array_insert", 3, false;
    ArrayRemoveLast = 0x2C, "array_removelast", 1, false;
    ArrayRemove = 0x2D, "array_remove", 3, false;
    ArrayClear = 0x2E, "array_clear", 1, false;
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mnemonic())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_dense_and_consistent() {
        for (i, op) in Opcode::ALL.iter().enumerate() {
            assert_eq!(*op as u8 as usize, i);
            assert_eq!(Opcode::from_byte(i as u8), Some(*op));
        }
        assert_eq!(Opcode::from_byte(0x2F), None);
        let varargs: Vec<_> = Opcode::ALL.iter().filter(|o| o.has_varargs()).map(|o| o.mnemonic()).collect();
        assert_eq!(varargs, ["callmethod", "callparent", "callstatic"]);
    }
}

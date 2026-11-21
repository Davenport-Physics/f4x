//! The typed tree a `.pex` parses into. Every field maps to stored bytes, in file order,
//! so [`crate::Pex::to_bytes`] can rebuild the input exactly.

use crate::opcode::Opcode;
use crate::strings::{PexString, StringId, StringTable};

/// `0xFA57C0DE`, stored little-endian in Fallout 4 files.
pub const MAGIC: u32 = 0xFA57_C0DE;
/// Version written by the Fallout 4 compiler.
pub const FO4_VERSION: (u8, u8) = (3, 9);
/// Game id in the header: 1 is Skyrim, 2 is Fallout 4.
pub const FO4_GAME_ID: u16 = 2;

/// A whole compiled script.
#[derive(Clone, Debug, PartialEq)]
pub struct Pex {
    pub header: Header,
    pub strings: StringTable,
    /// `None` when the has-debug-info byte is 0.
    pub debug_info: Option<DebugInfo>,
    /// Names of the user-flag bits used in this file (`hidden`, `conditional`, ...).
    pub user_flags: Vec<UserFlag>,
    /// Normally exactly one: the script itself.
    pub objects: Vec<Object>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub major_version: u8,
    pub minor_version: u8,
    pub game_id: u16,
    /// `time_t` of compilation.
    pub compilation_time: u64,
    /// Path of the `.psc` the compiler read.
    pub source_file_name: PexString,
    pub user_name: PexString,
    pub machine_name: PexString,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugInfo {
    /// `time_t` of the source file.
    pub modification_time: u64,
    pub functions: Vec<DebugFunction>,
    /// Fallout 4: `Group ... EndGroup` blocks, in source order.
    pub property_groups: Vec<PropertyGroup>,
    /// Fallout 4: source order of each struct's members.
    pub struct_orders: Vec<StructOrder>,
}

/// Line-number table of one function body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugFunction {
    pub object_name: StringId,
    pub state_name: StringId,
    pub function_name: StringId,
    pub function_type: DebugFunctionType,
    /// One source line per instruction.
    pub line_numbers: Vec<u16>,
}

/// What kind of body a [`DebugFunction`] describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DebugFunctionType {
    /// A state function or event.
    Method,
    /// A property's read handler (named after the property).
    Getter,
    /// A property's write handler (named after the property).
    Setter,
    /// Any other stored value, kept for round-tripping.
    Other(u8),
}

impl DebugFunctionType {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 => DebugFunctionType::Method,
            1 => DebugFunctionType::Getter,
            2 => DebugFunctionType::Setter,
            other => DebugFunctionType::Other(other),
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            DebugFunctionType::Method => 0,
            DebugFunctionType::Getter => 1,
            DebugFunctionType::Setter => 2,
            DebugFunctionType::Other(byte) => byte,
        }
    }
}

/// A named group of properties (Fallout 4 `Group` blocks), for the editor UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyGroup {
    pub object_name: StringId,
    /// Empty for the implicit ungrouped group.
    pub group_name: StringId,
    pub doc_string: StringId,
    pub user_flags: u32,
    /// Member properties in source order.
    pub properties: Vec<StringId>,
}

/// Source order of one struct's members (the struct record itself may list them in
/// another order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructOrder {
    pub object_name: StringId,
    pub struct_name: StringId,
    pub members: Vec<StringId>,
}

/// Maps a user-flag name to its bit in every `user_flags` word of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserFlag {
    pub name: StringId,
    pub bit: u8,
}

/// One script object (a script and its contents).
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub name: StringId,
    /// Empty string when the script has no parent.
    pub parent_class_name: StringId,
    pub doc_string: StringId,
    /// Fallout 4: script declared `Const`.
    pub is_const: bool,
    pub user_flags: u32,
    /// State the object starts in (empty: the default state).
    pub auto_state_name: StringId,
    /// Fallout 4.
    pub structs: Vec<Struct>,
    pub variables: Vec<Variable>,
    pub properties: Vec<Property>,
    pub states: Vec<State>,
}

/// Fallout 4 `Struct ... EndStruct`.
#[derive(Clone, Debug, PartialEq)]
pub struct Struct {
    pub name: StringId,
    pub members: Vec<StructMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructMember {
    pub name: StringId,
    pub type_name: StringId,
    pub user_flags: u32,
    pub default: Value,
    pub is_const: bool,
    pub doc_string: StringId,
}

/// A script variable (including the hidden backing variables of auto properties).
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub name: StringId,
    pub type_name: StringId,
    pub user_flags: u32,
    pub default: Value,
    /// Fallout 4.
    pub is_const: bool,
}

/// A property. Which optional parts are present is decided by [`Property::flags`].
#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub name: StringId,
    pub type_name: StringId,
    pub doc_string: StringId,
    pub user_flags: u32,
    /// Bit 0 readable, bit 1 writable, bit 2 auto (backed by [`Property::auto_var_name`]).
    pub flags: u8,
    /// Present iff `flags & 4`.
    pub auto_var_name: Option<StringId>,
    /// Present iff `flags & 5 == 1`.
    pub read_handler: Option<Function>,
    /// Present iff `flags & 6 == 2`.
    pub write_handler: Option<Function>,
}

impl Property {
    pub const FLAG_READ: u8 = 1;
    pub const FLAG_WRITE: u8 = 2;
    pub const FLAG_AUTO: u8 = 4;

    pub fn is_auto(&self) -> bool {
        self.flags & Self::FLAG_AUTO != 0
    }

    pub(crate) fn has_auto_var(flags: u8) -> bool {
        flags & Self::FLAG_AUTO != 0
    }

    pub(crate) fn has_read_handler(flags: u8) -> bool {
        flags & (Self::FLAG_READ | Self::FLAG_AUTO) == Self::FLAG_READ
    }

    pub(crate) fn has_write_handler(flags: u8) -> bool {
        flags & (Self::FLAG_WRITE | Self::FLAG_AUTO) == Self::FLAG_WRITE
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    /// Empty string for the default (empty) state.
    pub name: StringId,
    pub functions: Vec<NamedFunction>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NamedFunction {
    pub name: StringId,
    pub function: Function,
}

/// A function body or native declaration. Its name lives in the enclosing
/// [`NamedFunction`] (or is the property name for property handlers).
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// `None` for functions that return nothing.
    pub return_type: StringId,
    pub doc_string: StringId,
    pub user_flags: u32,
    /// Bit 0 global, bit 1 native.
    pub flags: u8,
    pub params: Vec<TypedName>,
    pub locals: Vec<TypedName>,
    pub instructions: Vec<Instruction>,
}

impl Function {
    pub const FLAG_GLOBAL: u8 = 1;
    pub const FLAG_NATIVE: u8 = 2;

    pub fn is_global(&self) -> bool {
        self.flags & Self::FLAG_GLOBAL != 0
    }

    pub fn is_native(&self) -> bool {
        self.flags & Self::FLAG_NATIVE != 0
    }
}

/// A parameter or local: name and type name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedName {
    pub name: StringId,
    pub type_name: StringId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Instruction {
    pub op: Opcode,
    /// Exactly [`Opcode::fixed_args`] operands.
    pub args: Vec<Value>,
    /// Extra operands of the call opcodes; stored after an integer count. Empty otherwise.
    pub varargs: Vec<Value>,
}

/// An operand or default value: a tag byte and its payload.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// Tag 0: `None`.
    None,
    /// Tag 1: a variable, parameter, property or type name.
    Identifier(StringId),
    /// Tag 2: a string literal.
    String(StringId),
    /// Tag 3.
    Integer(i32),
    /// Tag 4. Kept as an `f32`; its bits are written back unchanged.
    Float(f32),
    /// Tag 5.
    Bool(bool),
}

impl Value {
    pub fn tag(&self) -> u8 {
        match self {
            Value::None => 0,
            Value::Identifier(_) => 1,
            Value::String(_) => 2,
            Value::Integer(_) => 3,
            Value::Float(_) => 4,
            Value::Bool(_) => 5,
        }
    }
}

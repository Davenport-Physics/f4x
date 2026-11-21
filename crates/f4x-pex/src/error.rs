use std::fmt;

/// A parse failure at a byte offset of the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Offset of the field that could not be read.
    pub offset: usize,
    /// The structure being read.
    pub what: &'static str,
    pub kind: ParseErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    UnexpectedEof,
    /// The first four bytes, read little-endian, are not `0xFA57C0DE` (a big-endian Skyrim
    /// file reads as `0xDEC057FA`).
    BadMagic(u32),
    /// Only version 3.9 (Fallout 4) is supported.
    UnsupportedVersion {
        major: u8,
        minor: u8,
    },
    StringIndexOutOfRange {
        index: u16,
        count: usize,
    },
    BadValueTag(u8),
    /// A flag byte that should be 0 or 1.
    BadBool(u8),
    UnknownOpcode(u8),
    /// The variable-argument count is not a non-negative integer (carries its tag).
    BadVarargCount(u8),
    /// An object's stored size disagrees with the bytes its contents occupy.
    ObjectSizeMismatch {
        declared: usize,
        actual: usize,
    },
    TrailingBytes(usize),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at {:#x} ({}): ", self.offset, self.what)?;
        match &self.kind {
            ParseErrorKind::UnexpectedEof => write!(f, "unexpected end of file"),
            ParseErrorKind::BadMagic(m) => write!(f, "bad magic {m:#010x}, expected 0xfa57c0de"),
            ParseErrorKind::UnsupportedVersion { major, minor } => {
                write!(f, "unsupported version {major}.{minor}, expected 3.9")
            }
            ParseErrorKind::StringIndexOutOfRange { index, count } => {
                write!(f, "string index {index} out of range (table has {count})")
            }
            ParseErrorKind::BadValueTag(t) => write!(f, "unknown value tag {t}"),
            ParseErrorKind::BadBool(b) => write!(f, "flag byte {b} is not 0 or 1"),
            ParseErrorKind::UnknownOpcode(op) => write!(f, "unknown opcode {op:#04x}"),
            ParseErrorKind::BadVarargCount(tag) => {
                write!(f, "variable-argument count is a tag-{tag} value, expected a non-negative integer")
            }
            ParseErrorKind::ObjectSizeMismatch { declared, actual } => {
                write!(f, "object size field says {declared}, contents take {actual}")
            }
            ParseErrorKind::TrailingBytes(n) => write!(f, "{n} unread bytes after the last object"),
        }
    }
}

impl std::error::Error for ParseError {}

/// A tree that cannot be encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// A list longer than its uint16 count allows.
    TooMany { what: &'static str, len: usize },
    /// A string longer than its uint16 length prefix allows.
    StringTooLong(usize),
    /// A [`crate::StringId`] that is not in the string table.
    StringIndexOutOfRange { index: u16, count: usize },
    /// A property whose optional parts disagree with its flags.
    PropertyFlags { property: u16, flags: u8 },
    /// An instruction whose fixed operand count differs from its opcode's, or that has
    /// varargs on an opcode that takes none.
    Operands { op: &'static str, fixed: usize, varargs: usize },
    /// An object larger than its uint32 size field allows.
    ObjectTooLarge(usize),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::TooMany { what, len } => write!(f, "{len} {what} exceed the 65535 limit"),
            WriteError::StringTooLong(len) => write!(f, "string of {len} bytes exceeds 65535"),
            WriteError::StringIndexOutOfRange { index, count } => {
                write!(f, "string index {index} out of range (table has {count})")
            }
            WriteError::PropertyFlags { property, flags } => write!(
                f,
                "property (name string {property}): auto variable/handlers do not match flags {flags:#x}"
            ),
            WriteError::Operands { op, fixed, varargs } => {
                write!(f, "{op}: {fixed} fixed and {varargs} variable operands do not fit the opcode")
            }
            WriteError::ObjectTooLarge(len) => write!(f, "object of {len} bytes exceeds the size field"),
        }
    }
}

impl std::error::Error for WriteError {}

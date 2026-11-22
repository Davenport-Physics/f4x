//! Parsing: bytes to [`Pex`]. Fallout 4 files are little-endian throughout.

use crate::ast::*;
use crate::error::{ParseError, ParseErrorKind};
use crate::opcode::Opcode;
use crate::strings::{PexString, StringId, StringTable};

pub(crate) fn parse(bytes: &[u8]) -> Result<Pex, ParseError> {
    let mut r = Reader { buf: bytes, pos: 0, string_count: 0 };
    let header = r.header()?;
    let strings = r.string_table()?;
    r.string_count = strings.len();
    let debug_info = r.debug_info()?;
    let user_flags = r.list("user flags", |r| Ok(UserFlag { name: r.string_id()?, bit: r.u8()? }))?;
    let objects = r.list("objects", Reader::object)?;
    if r.pos != bytes.len() {
        return Err(r.error(ParseErrorKind::TrailingBytes(bytes.len() - r.pos), "end of file"));
    }
    Ok(Pex { header, strings, debug_info, user_flags, objects })
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Size of the string table once read; every [`StringId`] is checked against it.
    string_count: usize,
}

impl<'a> Reader<'a> {
    fn error(&self, kind: ParseErrorKind, what: &'static str) -> ParseError {
        ParseError { offset: self.pos, what, kind }
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], ParseError> {
        let end = self.pos.checked_add(n).filter(|&end| end <= self.buf.len());
        let Some(end) = end else {
            return Err(self.error(ParseErrorKind::UnexpectedEof, what));
        };
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn array<const N: usize>(&mut self, what: &'static str) -> Result<[u8; N], ParseError> {
        Ok(self.take(N, what)?.try_into().expect("take returned N bytes"))
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.array::<1>("u8")?[0])
    }

    fn u16(&mut self) -> Result<u16, ParseError> {
        Ok(u16::from_le_bytes(self.array("u16")?))
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        Ok(u32::from_le_bytes(self.array("u32")?))
    }

    fn u64(&mut self) -> Result<u64, ParseError> {
        Ok(u64::from_le_bytes(self.array("u64")?))
    }

    fn bool(&mut self, what: &'static str) -> Result<bool, ParseError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => {
                self.pos -= 1;
                Err(self.error(ParseErrorKind::BadBool(other), what))
            }
        }
    }

    /// uint16 length, then that many bytes.
    fn wstring(&mut self) -> Result<PexString, ParseError> {
        let len = usize::from(self.u16()?);
        Ok(PexString(self.take(len, "string")?.to_vec()))
    }

    fn string_id(&mut self) -> Result<StringId, ParseError> {
        let index = self.u16()?;
        if usize::from(index) >= self.string_count {
            self.pos -= 2;
            return Err(self.error(
                ParseErrorKind::StringIndexOutOfRange { index, count: self.string_count },
                "string index",
            ));
        }
        Ok(StringId(index))
    }

    /// uint16 count, then that many items.
    fn list<T>(
        &mut self,
        what: &'static str,
        mut item: impl FnMut(&mut Self) -> Result<T, ParseError>,
    ) -> Result<Vec<T>, ParseError> {
        let count = self.u16().map_err(|e| ParseError { what, ..e })?;
        (0..count).map(|_| item(self)).collect()
    }

    fn header(&mut self) -> Result<Header, ParseError> {
        let magic = u32::from_le_bytes(self.array("magic")?);
        if magic != MAGIC {
            self.pos = 0;
            return Err(self.error(ParseErrorKind::BadMagic(magic), "magic"));
        }
        let major_version = self.u8()?;
        let minor_version = self.u8()?;
        if (major_version, minor_version) != FO4_VERSION {
            self.pos -= 2;
            return Err(self.error(
                ParseErrorKind::UnsupportedVersion { major: major_version, minor: minor_version },
                "version",
            ));
        }
        Ok(Header {
            major_version,
            minor_version,
            game_id: self.u16()?,
            compilation_time: self.u64()?,
            source_file_name: self.wstring()?,
            user_name: self.wstring()?,
            machine_name: self.wstring()?,
        })
    }

    fn string_table(&mut self) -> Result<StringTable, ParseError> {
        Ok(StringTable { strings: self.list("string table", Reader::wstring)? })
    }

    fn debug_info(&mut self) -> Result<Option<DebugInfo>, ParseError> {
        if !self.bool("has debug info")? {
            return Ok(None);
        }
        let modification_time = self.u64()?;
        let functions = self.list("debug functions", |r| {
            Ok(DebugFunction {
                object_name: r.string_id()?,
                state_name: r.string_id()?,
                function_name: r.string_id()?,
                function_type: DebugFunctionType::from_byte(r.u8()?),
                line_numbers: r.list("line numbers", Reader::u16)?,
            })
        })?;
        let property_groups = self.list("property groups", |r| {
            Ok(PropertyGroup {
                object_name: r.string_id()?,
                group_name: r.string_id()?,
                doc_string: r.string_id()?,
                user_flags: r.u32()?,
                properties: r.list("group properties", Reader::string_id)?,
            })
        })?;
        let struct_orders = self.list("struct orders", |r| {
            Ok(StructOrder {
                object_name: r.string_id()?,
                struct_name: r.string_id()?,
                members: r.list("struct order members", Reader::string_id)?,
            })
        })?;
        Ok(Some(DebugInfo { modification_time, functions, property_groups, struct_orders }))
    }

    fn object(&mut self) -> Result<Object, ParseError> {
        let name = self.string_id()?;
        let size_at = self.pos;
        // The stored size counts itself (4 bytes) plus the object data.
        let size = self.u32()? as usize;
        let start = self.pos;
        let object = Object {
            name,
            parent_class_name: self.string_id()?,
            doc_string: self.string_id()?,
            is_const: self.bool("object const flag")?,
            user_flags: self.u32()?,
            auto_state_name: self.string_id()?,
            structs: self.list("structs", Reader::struct_)?,
            variables: self.list("variables", Reader::variable)?,
            properties: self.list("properties", Reader::property)?,
            states: self.list("states", Reader::state)?,
            size_excludes_field: false,
        };
        let mut object = object;
        let actual = self.pos - start + 4;
        if actual == size {
            object.size_excludes_field = false;
        } else if actual - 4 == size {
            object.size_excludes_field = true;
        } else {
            self.pos = size_at;
            return Err(
                self.error(ParseErrorKind::ObjectSizeMismatch { declared: size, actual }, "object size")
            );
        }
        Ok(object)
    }

    fn struct_(&mut self) -> Result<Struct, ParseError> {
        Ok(Struct {
            name: self.string_id()?,
            members: self.list("struct members", |r| {
                Ok(StructMember {
                    name: r.string_id()?,
                    type_name: r.string_id()?,
                    user_flags: r.u32()?,
                    default: r.value()?,
                    is_const: r.bool("struct member const flag")?,
                    doc_string: r.string_id()?,
                })
            })?,
        })
    }

    fn variable(&mut self) -> Result<Variable, ParseError> {
        Ok(Variable {
            name: self.string_id()?,
            type_name: self.string_id()?,
            user_flags: self.u32()?,
            default: self.value()?,
            is_const: self.bool("variable const flag")?,
        })
    }

    fn property(&mut self) -> Result<Property, ParseError> {
        let name = self.string_id()?;
        let type_name = self.string_id()?;
        let doc_string = self.string_id()?;
        let user_flags = self.u32()?;
        let flags = self.u8()?;
        let auto_var_name = if Property::has_auto_var(flags) { Some(self.string_id()?) } else { None };
        let read_handler = if Property::has_read_handler(flags) { Some(self.function()?) } else { None };
        let write_handler = if Property::has_write_handler(flags) { Some(self.function()?) } else { None };
        Ok(Property {
            name,
            type_name,
            doc_string,
            user_flags,
            flags,
            auto_var_name,
            read_handler,
            write_handler,
        })
    }

    fn state(&mut self) -> Result<State, ParseError> {
        Ok(State {
            name: self.string_id()?,
            functions: self.list("state functions", |r| {
                Ok(NamedFunction { name: r.string_id()?, function: r.function()? })
            })?,
        })
    }

    fn typed_name(&mut self) -> Result<TypedName, ParseError> {
        Ok(TypedName { name: self.string_id()?, type_name: self.string_id()? })
    }

    fn function(&mut self) -> Result<Function, ParseError> {
        Ok(Function {
            return_type: self.string_id()?,
            doc_string: self.string_id()?,
            user_flags: self.u32()?,
            flags: self.u8()?,
            params: self.list("parameters", Reader::typed_name)?,
            locals: self.list("locals", Reader::typed_name)?,
            instructions: self.list("instructions", Reader::instruction)?,
        })
    }

    fn instruction(&mut self) -> Result<Instruction, ParseError> {
        let byte = self.u8()?;
        let Some(op) = Opcode::from_byte(byte) else {
            self.pos -= 1;
            return Err(self.error(ParseErrorKind::UnknownOpcode(byte), "instruction"));
        };
        let args = (0..op.fixed_args()).map(|_| self.value()).collect::<Result<Vec<_>, _>>()?;
        let varargs = if op.has_varargs() {
            let count_at = self.pos;
            match self.value()? {
                Value::Integer(n) if n >= 0 => (0..n).map(|_| self.value()).collect::<Result<Vec<_>, _>>()?,
                other => {
                    self.pos = count_at;
                    return Err(self.error(ParseErrorKind::BadVarargCount(other.tag()), "vararg count"));
                }
            }
        } else {
            Vec::new()
        };
        Ok(Instruction { op, args, varargs })
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        Ok(match self.u8()? {
            0 => Value::None,
            1 => Value::Identifier(self.string_id()?),
            2 => Value::String(self.string_id()?),
            3 => Value::Integer(self.u32()? as i32),
            4 => Value::Float(f32::from_bits(self.u32()?)),
            5 => Value::Bool(self.bool("bool value")?),
            tag => {
                self.pos -= 1;
                return Err(self.error(ParseErrorKind::BadValueTag(tag), "value"));
            }
        })
    }
}

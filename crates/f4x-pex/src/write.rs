//! Serialization: [`Pex`] to bytes, the exact inverse of `read.rs`.

use crate::ast::*;
use crate::error::WriteError;
use crate::strings::{PexString, StringId};

pub(crate) fn write(pex: &Pex) -> Result<Vec<u8>, WriteError> {
    let mut w = Writer { out: Vec::new(), string_count: pex.strings.len() };
    w.header(&pex.header)?;
    w.count("strings", pex.strings.len())?;
    for s in &pex.strings.strings {
        w.wstring(s)?;
    }
    w.debug_info(pex.debug_info.as_ref())?;
    w.list("user flags", &pex.user_flags, |w, flag| {
        w.string_id(flag.name)?;
        w.u8(flag.bit);
        Ok(())
    })?;
    w.list("objects", &pex.objects, Writer::object)?;
    Ok(w.out)
}

struct Writer {
    out: Vec<u8>,
    string_count: usize,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }

    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    fn count(&mut self, what: &'static str, len: usize) -> Result<(), WriteError> {
        let n = u16::try_from(len).map_err(|_| WriteError::TooMany { what, len })?;
        self.u16(n);
        Ok(())
    }

    fn list<T>(
        &mut self,
        what: &'static str,
        items: &[T],
        mut item: impl FnMut(&mut Self, &T) -> Result<(), WriteError>,
    ) -> Result<(), WriteError> {
        self.count(what, items.len())?;
        items.iter().try_for_each(|x| item(self, x))
    }

    fn wstring(&mut self, s: &PexString) -> Result<(), WriteError> {
        let len = u16::try_from(s.0.len()).map_err(|_| WriteError::StringTooLong(s.0.len()))?;
        self.u16(len);
        self.out.extend_from_slice(&s.0);
        Ok(())
    }

    fn string_id(&mut self, id: StringId) -> Result<(), WriteError> {
        if usize::from(id.0) >= self.string_count {
            return Err(WriteError::StringIndexOutOfRange { index: id.0, count: self.string_count });
        }
        self.u16(id.0);
        Ok(())
    }

    fn string_ids(&mut self, what: &'static str, ids: &[StringId]) -> Result<(), WriteError> {
        self.list(what, ids, |w, &id| w.string_id(id))
    }

    fn header(&mut self, h: &Header) -> Result<(), WriteError> {
        self.u32(MAGIC);
        self.u8(h.major_version);
        self.u8(h.minor_version);
        self.u16(h.game_id);
        self.u64(h.compilation_time);
        self.wstring(&h.source_file_name)?;
        self.wstring(&h.user_name)?;
        self.wstring(&h.machine_name)
    }

    fn debug_info(&mut self, debug: Option<&DebugInfo>) -> Result<(), WriteError> {
        let Some(debug) = debug else {
            self.u8(0);
            return Ok(());
        };
        self.u8(1);
        self.u64(debug.modification_time);
        self.list("debug functions", &debug.functions, |w, f| {
            w.string_id(f.object_name)?;
            w.string_id(f.state_name)?;
            w.string_id(f.function_name)?;
            w.u8(f.function_type.to_byte());
            w.list("line numbers", &f.line_numbers, |w, &line| {
                w.u16(line);
                Ok(())
            })
        })?;
        self.list("property groups", &debug.property_groups, |w, g| {
            w.string_id(g.object_name)?;
            w.string_id(g.group_name)?;
            w.string_id(g.doc_string)?;
            w.u32(g.user_flags);
            w.string_ids("group properties", &g.properties)
        })?;
        self.list("struct orders", &debug.struct_orders, |w, o| {
            w.string_id(o.object_name)?;
            w.string_id(o.struct_name)?;
            w.string_ids("struct order members", &o.members)
        })
    }

    fn object(&mut self, o: &Object) -> Result<(), WriteError> {
        self.string_id(o.name)?;
        let size_at = self.out.len();
        self.u32(0); // patched below
        self.string_id(o.parent_class_name)?;
        self.string_id(o.doc_string)?;
        self.bool(o.is_const);
        self.u32(o.user_flags);
        self.string_id(o.auto_state_name)?;
        self.list("structs", &o.structs, Writer::struct_)?;
        self.list("variables", &o.variables, Writer::variable)?;
        self.list("properties", &o.properties, Writer::property)?;
        self.list("states", &o.states, Writer::state)?;
        let size = self.out.len() - size_at;
        let size32 = u32::try_from(size).map_err(|_| WriteError::ObjectTooLarge(size))?;
        self.out[size_at..size_at + 4].copy_from_slice(&size32.to_le_bytes());
        Ok(())
    }

    fn struct_(&mut self, s: &Struct) -> Result<(), WriteError> {
        self.string_id(s.name)?;
        self.list("struct members", &s.members, |w, m| {
            w.string_id(m.name)?;
            w.string_id(m.type_name)?;
            w.u32(m.user_flags);
            w.value(&m.default)?;
            w.bool(m.is_const);
            w.string_id(m.doc_string)
        })
    }

    fn variable(&mut self, v: &Variable) -> Result<(), WriteError> {
        self.string_id(v.name)?;
        self.string_id(v.type_name)?;
        self.u32(v.user_flags);
        self.value(&v.default)?;
        self.bool(v.is_const);
        Ok(())
    }

    fn property(&mut self, p: &Property) -> Result<(), WriteError> {
        let consistent = p.auto_var_name.is_some() == Property::has_auto_var(p.flags)
            && p.read_handler.is_some() == Property::has_read_handler(p.flags)
            && p.write_handler.is_some() == Property::has_write_handler(p.flags);
        if !consistent {
            return Err(WriteError::PropertyFlags { property: p.name.0, flags: p.flags });
        }
        self.string_id(p.name)?;
        self.string_id(p.type_name)?;
        self.string_id(p.doc_string)?;
        self.u32(p.user_flags);
        self.u8(p.flags);
        if let Some(var) = p.auto_var_name {
            self.string_id(var)?;
        }
        if let Some(f) = &p.read_handler {
            self.function(f)?;
        }
        if let Some(f) = &p.write_handler {
            self.function(f)?;
        }
        Ok(())
    }

    fn state(&mut self, s: &State) -> Result<(), WriteError> {
        self.string_id(s.name)?;
        self.list("state functions", &s.functions, |w, f| {
            w.string_id(f.name)?;
            w.function(&f.function)
        })
    }

    fn typed_names(&mut self, what: &'static str, names: &[TypedName]) -> Result<(), WriteError> {
        self.list(what, names, |w, t| {
            w.string_id(t.name)?;
            w.string_id(t.type_name)
        })
    }

    fn function(&mut self, f: &Function) -> Result<(), WriteError> {
        self.string_id(f.return_type)?;
        self.string_id(f.doc_string)?;
        self.u32(f.user_flags);
        self.u8(f.flags);
        self.typed_names("parameters", &f.params)?;
        self.typed_names("locals", &f.locals)?;
        self.list("instructions", &f.instructions, Writer::instruction)
    }

    fn instruction(&mut self, i: &Instruction) -> Result<(), WriteError> {
        let fits = i.args.len() == i.op.fixed_args() && (i.op.has_varargs() || i.varargs.is_empty());
        if !fits {
            return Err(WriteError::Operands {
                op: i.op.mnemonic(),
                fixed: i.args.len(),
                varargs: i.varargs.len(),
            });
        }
        self.u8(i.op as u8);
        for v in &i.args {
            self.value(v)?;
        }
        if i.op.has_varargs() {
            let n = i32::try_from(i.varargs.len())
                .map_err(|_| WriteError::TooMany { what: "variable operands", len: i.varargs.len() })?;
            self.value(&Value::Integer(n))?;
            for v in &i.varargs {
                self.value(v)?;
            }
        }
        Ok(())
    }

    fn value(&mut self, v: &Value) -> Result<(), WriteError> {
        self.u8(v.tag());
        match *v {
            Value::None => {}
            Value::Identifier(id) | Value::String(id) => self.string_id(id)?,
            Value::Integer(n) => self.u32(n as u32),
            Value::Float(x) => self.u32(x.to_bits()),
            Value::Bool(b) => self.bool(b),
        }
        Ok(())
    }
}

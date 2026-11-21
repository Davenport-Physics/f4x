//! Convenience queries over a parsed script: objects, functions and their signatures.

use std::borrow::Cow;
use std::fmt;

use crate::ast::{Function, Object, Pex};
use crate::strings::StringId;

/// Where a function body lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FunctionKind {
    /// A function or event in a state (the empty state is the default one).
    State,
    /// A property's read handler.
    Getter,
    /// A property's write handler.
    Setter,
}

/// A function found in the tree, with the names that locate it.
#[derive(Clone, Copy, Debug)]
pub struct FunctionRef<'a> {
    pub object: &'a Object,
    /// State name for [`FunctionKind::State`]; the property name for handlers.
    pub owner: StringId,
    /// Function name; the property name for handlers.
    pub name: StringId,
    pub kind: FunctionKind,
    pub function: &'a Function,
}

/// A parameter with resolved names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub type_name: String,
}

/// A function declaration with every string resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    pub object: String,
    /// State name (empty: default state), or the property name for handlers.
    pub owner: String,
    pub name: String,
    pub kind: FunctionKind,
    /// `None` when the function returns nothing (stored as the string `None`).
    pub return_type: Option<String>,
    pub params: Vec<Param>,
    pub is_global: bool,
    pub is_native: bool,
    pub user_flags: u32,
}

impl fmt::Display for FunctionSignature {
    /// Papyrus-style declaration: `Int Function GetFormID() Native`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ret) = &self.return_type {
            write!(f, "{ret} ")?;
        }
        let name = match self.kind {
            FunctionKind::State => self.name.as_str(),
            FunctionKind::Getter => "Get",
            FunctionKind::Setter => "Set",
        };
        write!(f, "Function {name}(")?;
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{} {}", p.type_name, p.name)?;
        }
        f.write_str(")")?;
        if self.is_global {
            f.write_str(" Global")?;
        }
        if self.is_native {
            f.write_str(" Native")?;
        }
        Ok(())
    }
}

impl Pex {
    /// The first object whose name matches, ignoring ASCII case (as Papyrus does).
    pub fn object(&self, name: &str) -> Option<&Object> {
        self.objects.iter().find(|o| self.strings.get(o.name).is_some_and(|s| s.eq_ignore_case(name)))
    }

    /// Names of all objects (normally one: the script name).
    pub fn object_names(&self) -> Vec<Cow<'_, str>> {
        self.objects.iter().map(|o| self.str(o.name)).collect()
    }

    /// Every function body or declaration: state functions first (by state), then
    /// property handlers, for each object in order.
    pub fn functions(&self) -> Vec<FunctionRef<'_>> {
        let mut out = Vec::new();
        for object in &self.objects {
            for state in &object.states {
                for f in &state.functions {
                    out.push(FunctionRef {
                        object,
                        owner: state.name,
                        name: f.name,
                        kind: FunctionKind::State,
                        function: &f.function,
                    });
                }
            }
            for p in &object.properties {
                let handlers =
                    [(FunctionKind::Getter, &p.read_handler), (FunctionKind::Setter, &p.write_handler)];
                for (kind, handler) in handlers {
                    if let Some(function) = handler {
                        out.push(FunctionRef { object, owner: p.name, name: p.name, kind, function });
                    }
                }
            }
        }
        out
    }

    /// Resolves a function's names and types.
    pub fn signature(&self, f: &FunctionRef<'_>) -> FunctionSignature {
        let s = |id: StringId| self.str(id).into_owned();
        let return_type = s(f.function.return_type);
        FunctionSignature {
            object: s(f.object.name),
            owner: s(f.owner),
            name: s(f.name),
            kind: f.kind,
            return_type: (!return_type.eq_ignore_ascii_case("none")).then_some(return_type),
            params: f
                .function
                .params
                .iter()
                .map(|p| Param { name: s(p.name), type_name: s(p.type_name) })
                .collect(),
            is_global: f.function.is_global(),
            is_native: f.function.is_native(),
            user_flags: f.function.user_flags,
        }
    }

    /// Signatures of every function.
    pub fn function_signatures(&self) -> Vec<FunctionSignature> {
        self.functions().iter().map(|f| self.signature(f)).collect()
    }

    /// Signatures of the functions declared `Native`.
    pub fn native_functions(&self) -> Vec<FunctionSignature> {
        self.functions().iter().filter(|f| f.function.is_native()).map(|f| self.signature(f)).collect()
    }

    /// Names of the user flags set in `flags`, using this file's flag table.
    pub fn user_flag_names(&self, flags: u32) -> Vec<Cow<'_, str>> {
        let mut out: Vec<(u8, Cow<'_, str>)> = self
            .user_flags
            .iter()
            .filter(|uf| uf.bit < 32 && flags & (1 << uf.bit) != 0)
            .map(|uf| (uf.bit, self.str(uf.name)))
            .collect();
        out.sort_by_key(|(bit, _)| *bit);
        out.into_iter().map(|(_, name)| name).collect()
    }
}

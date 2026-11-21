//! Form types (CommonLibF4 `ENUM_FORM_ID`, generated into `f4x-engine-sys`).

use crate::layout::form_type::TABLE;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FormType(pub u8);

impl FormType {
    pub const COUNT: usize = 159;

    /// The record signature, e.g. `WEAP`.
    #[must_use]
    pub fn signature(self) -> &'static str {
        TABLE.iter().find(|(v, _, _)| *v == self.0).map_or("????", |(_, s, _)| s)
    }

    /// The engine class, e.g. `TESObjectWEAP`.
    #[must_use]
    pub fn class(self) -> &'static str {
        TABLE.iter().find(|(v, _, _)| *v == self.0).map_or("?", |(_, _, c)| c)
    }

    #[must_use]
    pub fn from_signature(signature: &str) -> Option<FormType> {
        TABLE.iter().find(|(_, s, _)| s.eq_ignore_ascii_case(signature)).map(|(v, _, _)| FormType(*v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        assert_eq!(TABLE.len(), FormType::COUNT);
        let weap = FormType::from_signature("WEAP").unwrap();
        assert_eq!(weap.0, 0x2B);
        assert_eq!(weap.class(), "TESObjectWEAP");
        assert_eq!(FormType(0x2D).signature(), "NPC_");
    }
}

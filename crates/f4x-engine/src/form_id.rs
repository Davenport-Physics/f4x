//! Form IDs: `[load-order index:8][local id:24]`, with light (ESL) files in slot 0xFE
//! (`0xFE[light index:12][local id:12]`) and runtime-created forms in 0xFF.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct FormId(pub u32);

impl FormId {
    #[must_use]
    pub fn file_index(self) -> u8 {
        (self.0 >> 24) as u8
    }

    #[must_use]
    pub fn is_light(self) -> bool {
        self.file_index() == 0xFE
    }

    #[must_use]
    pub fn is_created(self) -> bool {
        self.file_index() == 0xFF
    }

    /// The id within its file.
    #[must_use]
    pub fn local(self) -> u32 {
        if self.is_light() { self.0 & 0xFFF } else { self.0 & 0x00FF_FFFF }
    }

    /// Light-file index (only meaningful for light forms).
    #[must_use]
    pub fn light_index(self) -> u16 {
        ((self.0 >> 12) & 0xFFF) as u16
    }

    /// Parse `0x00000014`, `00000014` or `14` (hex).
    #[must_use]
    pub fn parse(text: &str) -> Option<FormId> {
        let t = text.trim();
        let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
        u32::from_str_radix(t, 16).ok().map(FormId)
    }
}

impl fmt::Debug for FormId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FormId({:08X})", self.0)
    }
}

impl fmt::Display for FormId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08X}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts() {
        let regular = FormId(0x0402_2B3C);
        assert_eq!((regular.file_index(), regular.local()), (4, 0x02_2B3C));
        let light = FormId(0xFE01_2803);
        assert!(light.is_light());
        assert_eq!((light.light_index(), light.local()), (0x012, 0x803));
        assert!(FormId(0xFF00_0001).is_created());
        assert_eq!(FormId::parse("0x14"), Some(FormId(0x14)));
        assert_eq!(FormId::parse("00000014").unwrap().to_string(), "00000014");
    }
}

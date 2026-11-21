//! Strings: the string table, indices into it, and raw length-prefixed strings.
//!
//! Strings are kept as the exact stored bytes so that writing reproduces them, and are
//! decoded as Windows-1252 only when viewed.

use std::borrow::Cow;
use std::fmt;

/// Index into the file's [`StringTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StringId(pub u16);

impl StringId {
    /// The text this index names, or `"<bad string N>"` if it is out of range.
    pub fn resolve(self, table: &StringTable) -> Cow<'_, str> {
        match table.get(self) {
            Some(s) => s.to_str(),
            None => Cow::Owned(format!("<bad string {}>", self.0)),
        }
    }
}

/// A length-prefixed string exactly as stored (no terminator, Windows-1252 by convention).
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct PexString(pub Vec<u8>);

impl PexString {
    /// Encodes `text` as Windows-1252; characters with no mapping become `?`.
    pub fn new(text: &str) -> PexString {
        PexString(text.chars().map(encode_char).collect())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Decodes as Windows-1252, borrowing when the bytes are plain ASCII.
    pub fn to_str(&self) -> Cow<'_, str> {
        if self.0.is_ascii() {
            // ASCII is valid UTF-8.
            Cow::Borrowed(std::str::from_utf8(&self.0).unwrap_or_default())
        } else {
            Cow::Owned(self.0.iter().map(|&b| decode_byte(b)).collect())
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Case-insensitive (ASCII) comparison, matching how Papyrus treats identifiers.
    pub fn eq_ignore_case(&self, text: &str) -> bool {
        self.0.eq_ignore_ascii_case(PexString::new(text).as_bytes())
    }
}

impl fmt::Debug for PexString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.to_str(), f)
    }
}

impl fmt::Display for PexString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_str())
    }
}

impl From<&str> for PexString {
    fn from(text: &str) -> Self {
        PexString::new(text)
    }
}

/// The table every [`StringId`] indexes. Order is significant and preserved.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StringTable {
    pub strings: Vec<PexString>,
}

impl StringTable {
    pub fn get(&self, id: StringId) -> Option<&PexString> {
        self.strings.get(usize::from(id.0))
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// Finds an entry with exactly these bytes.
    pub fn find(&self, text: &str) -> Option<StringId> {
        let want = PexString::new(text);
        self.strings.iter().position(|s| *s == want).map(|i| StringId(i as u16))
    }

    /// Finds an entry equal to `text` ignoring ASCII case.
    pub fn find_ignore_case(&self, text: &str) -> Option<StringId> {
        self.strings.iter().position(|s| s.eq_ignore_case(text)).map(|i| StringId(i as u16))
    }

    /// Returns the index of exactly `text`, appending it if absent. Panics past 65,535
    /// entries, which the format cannot represent.
    pub fn intern(&mut self, text: &str) -> StringId {
        match self.find(text) {
            Some(id) => id,
            None => self.push(text),
        }
    }

    /// Like [`StringTable::intern`] but reuses an entry that differs only in ASCII case.
    /// Vanilla tables never hold two entries that differ only in case, so this keeps an
    /// edited table looking compiler-made.
    pub fn intern_ignore_case(&mut self, text: &str) -> StringId {
        match self.find_ignore_case(text) {
            Some(id) => id,
            None => self.push(text),
        }
    }

    fn push(&mut self, text: &str) -> StringId {
        let id = u16::try_from(self.strings.len()).expect("string table is limited to 65535 entries");
        self.strings.push(PexString::new(text));
        StringId(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (StringId, &PexString)> {
        self.strings.iter().enumerate().map(|(i, s)| (StringId(i as u16), s))
    }
}

/// Windows-1252 bytes 0x80..=0x9F (WHATWG mapping; undefined bytes map to the same C1
/// control, which keeps decode/encode lossless).
const HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

fn decode_byte(b: u8) -> char {
    match b {
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

fn encode_char(c: char) -> u8 {
    match u32::from(c) {
        n @ (0..=0x7F | 0xA0..=0xFF) => n as u8,
        _ => HIGH.iter().position(|&h| h == c).map_or(b'?', |i| 0x80 + i as u8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp1252_round_trips_every_byte() {
        let all = PexString((0..=255).collect());
        assert_eq!(PexString::new(&all.to_str()), all);
        assert_eq!(PexString::new("caf\u{e9} \u{20AC}").as_bytes(), b"caf\xe9 \x80");
        assert_eq!(PexString::new("\u{4e2d}").as_bytes(), b"?");
    }

    #[test]
    fn intern_reuses_and_appends() {
        let mut table = StringTable::default();
        let a = table.intern("Form");
        let b = table.intern("");
        assert_eq!(table.intern("Form"), a);
        assert_ne!(a, b);
        assert_eq!(table.len(), 2);
        assert_eq!(b.resolve(&table), "");
        assert_eq!(StringId(9).resolve(&table), "<bad string 9>");
        assert!(table.get(a).unwrap().eq_ignore_case("FORM"));
        assert_eq!(table.intern_ignore_case("FORM"), a);
        assert_ne!(table.intern("FORM"), a);
        assert_eq!(table.len(), 3);
    }
}

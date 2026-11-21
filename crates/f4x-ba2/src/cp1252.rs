//! Windows-1252 decoding for names stored in the archive.
//!
//! Bytes 0x80..=0x9F follow the WHATWG mapping; the five undefined bytes map to the C1
//! control with the same value, so decoding is lossless.

const HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

pub fn decode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => HIGH[usize::from(b - 0x80)],
            _ => char::from(b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn lossless_and_whatwg_high_range() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(super::decode(&all).chars().count(), 256);
        assert_eq!(super::decode(&[0x80, b'a', 0x9F]), "\u{20AC}a\u{0178}");
    }
}

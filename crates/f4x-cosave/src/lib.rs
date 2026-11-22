//! F4SE-format co-saves (`<save>.f4se`, written next to `<save>.fos`).
//!
//! Format learned black-box from files written by F4SE 0.6.23 (see
//! `docs/formats/cosave.md`, fixture `tests/fixtures/f4se-0.6.23-probe.f4se`);
//! all integers little-endian:
//!
//! ```text
//! "F4SE"  u32 format_version (1)  u32 f4se_version  u32 runtime_version  u32 plugin_count
//! plugin_count × { u32 uid  u32 chunk_count  u32 length   chunk_count × chunk }
//! chunk = { u32 type  u32 version  u32 length  [length bytes] }
//! ```
//!
//! `length` of a plugin block is the byte size of its chunks (headers included). Types
//! and uids are four-character codes stored as little-endian `u32`s (so `'TST1'` appears
//! as the bytes `1TST`). F4SE's own block has uid 0 and starts with `PLGN`, the load
//! order of the save. Unknown chunks are carried through untouched.

use std::fmt;

pub const MAGIC: &[u8; 4] = b"F4SE";
pub const FORMAT_VERSION: u32 = 1;
/// The block F4SE writes its own state into.
pub const HOST_UID: u32 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoSave {
    pub format_version: u32,
    pub f4se_version: u32,
    pub runtime_version: u32,
    pub plugins: Vec<PluginBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginBlock {
    pub uid: u32,
    pub chunks: Vec<Chunk>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Chunk {
    pub kind: u32,
    pub version: u32,
    pub data: Vec<u8>,
}

impl fmt::Debug for Chunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Chunk({} v{} {} bytes)", fourcc(self.kind), self.version, self.data.len())
    }
}

/// `'TST1'` (MSVC multi-character constant, big-endian character order) from text.
#[must_use]
pub fn tag(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// The four characters of a tag, e.g. `TST1`.
#[must_use]
pub fn fourcc(value: u32) -> String {
    value.to_be_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "co-save: {}", self.0)
    }
}

impl std::error::Error for Error {}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u32(&mut self, what: &str) -> Result<u32, Error> {
        let b = self.bytes.get(self.at..self.at + 4).ok_or_else(|| Error(format!("truncated at {} ({what})", self.at)))?;
        self.at += 4;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }

    fn take(&mut self, n: usize, what: &str) -> Result<&[u8], Error> {
        let b = self.bytes.get(self.at..self.at + n).ok_or_else(|| Error(format!("truncated at {} ({what})", self.at)))?;
        self.at += n;
        Ok(b)
    }
}

impl CoSave {
    /// An empty co-save for the given versions.
    #[must_use]
    pub fn new(f4se_version: u32, runtime_version: u32) -> CoSave {
        CoSave { format_version: FORMAT_VERSION, f4se_version, runtime_version, plugins: Vec::new() }
    }

    pub fn parse(bytes: &[u8]) -> Result<CoSave, Error> {
        if bytes.get(..4) != Some(MAGIC) {
            return Err(Error("not an F4SE co-save (bad magic)".into()));
        }
        let mut r = Reader { bytes, at: 4 };
        let format_version = r.u32("format version")?;
        let f4se_version = r.u32("f4se version")?;
        let runtime_version = r.u32("runtime version")?;
        let count = r.u32("plugin count")?;
        let mut plugins = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            let uid = r.u32("plugin uid")?;
            let chunk_count = r.u32("chunk count")?;
            let length = r.u32("plugin length")? as usize;
            let end = r.at + length;
            let mut chunks = Vec::with_capacity(chunk_count.min(4096) as usize);
            for _ in 0..chunk_count {
                let kind = r.u32("chunk type")?;
                let version = r.u32("chunk version")?;
                let len = r.u32("chunk length")? as usize;
                chunks.push(Chunk { kind, version, data: r.take(len, "chunk data")?.to_vec() });
            }
            if r.at != end {
                return Err(Error(format!("plugin {} block length {length} does not match its chunks", fourcc(uid))));
            }
            plugins.push(PluginBlock { uid, chunks });
        }
        Ok(CoSave { format_version, f4se_version, runtime_version, plugins })
    }

    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        for v in [self.format_version, self.f4se_version, self.runtime_version, self.plugins.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for plugin in &self.plugins {
            let length: usize = plugin.chunks.iter().map(|c| 12 + c.data.len()).sum();
            for v in [plugin.uid, plugin.chunks.len() as u32, length as u32] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for chunk in &plugin.chunks {
                for v in [chunk.kind, chunk.version, chunk.data.len() as u32] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                out.extend_from_slice(&chunk.data);
            }
        }
        out
    }

    pub fn plugin(&self, uid: u32) -> Option<&PluginBlock> {
        self.plugins.iter().find(|p| p.uid == uid)
    }
}

/// The `PLGN` chunk: the save's load order, full and light plugins interleaved in load
/// order. Format seen in F4SE 0.6.23 co-saves: `u16 count`, then per file `u8 index`;
/// for light files (`index == 0xFE`) a `u16 light index` follows; then `u16 name length`
/// and the name bytes.
pub mod plgn {
    pub const KIND: [u8; 4] = *b"PLGN";
    pub const LIGHT: u8 = 0xFE;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Entry {
        pub index: u8,
        /// For light files (`index == 0xFE`).
        pub light_index: Option<u16>,
        pub name: String,
    }

    impl Entry {
        #[must_use]
        pub fn full(index: u8, name: &str) -> Entry {
            Entry { index, light_index: None, name: name.to_owned() }
        }

        #[must_use]
        pub fn light(light_index: u16, name: &str) -> Entry {
            Entry { index: LIGHT, light_index: Some(light_index), name: name.to_owned() }
        }
    }

    pub fn parse(data: &[u8]) -> Option<Vec<Entry>> {
        let count = u16::from_le_bytes(data.get(..2)?.try_into().ok()?) as usize;
        let mut at = 2;
        let mut out = Vec::with_capacity(count);
        let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        for _ in 0..count {
            let index = *data.get(at)?;
            at += 1;
            let light_index = if index == LIGHT {
                at += 2;
                Some(u16_at(at - 2)?)
            } else {
                None
            };
            let len = u16_at(at)? as usize;
            let name = String::from_utf8_lossy(data.get(at + 2..at + 2 + len)?).into_owned();
            out.push(Entry { index, light_index, name });
            at += 2 + len;
        }
        Some(out)
    }

    pub fn write(entries: &[Entry]) -> Vec<u8> {
        let mut out = (entries.len() as u16).to_le_bytes().to_vec();
        for e in entries {
            out.push(e.index);
            if e.index == LIGHT {
                out.extend_from_slice(&e.light_index.unwrap_or(0).to_le_bytes());
            }
            out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            out.extend_from_slice(e.name.as_bytes());
        }
        out
    }
}

/// The host block F4SE 0.6.23 writes when nothing is registered (black-box): `PLGN`, then
/// empty registration tables (`KEYR`, `REGE`, `CTLR`, `REGE`, `EXEV`, `REGE`, `CAMR`,
/// `FRNR`, `OBMG`, `DFMG`, `FUNQ`, `WLST`) and the `____` end marker. Used when there is no
/// earlier host block to carry through.
#[must_use]
pub fn empty_host_chunks(load_order: &[plgn::Entry]) -> Vec<Chunk> {
    let chunk = |k: &[u8; 4], v: u32, data: Vec<u8>| Chunk { kind: tag(k), version: v, data };
    vec![
        chunk(&plgn::KIND, 0, plgn::write(load_order)),
        chunk(b"KEYR", 1, vec![]),
        chunk(b"REGE", 1, vec![]),
        chunk(b"CTLR", 1, vec![]),
        chunk(b"REGE", 1, vec![]),
        chunk(b"EXEV", 1, vec![]),
        chunk(b"REGE", 1, vec![]),
        chunk(b"CAMR", 1, vec![0; 4]),
        chunk(b"FRNR", 1, vec![0; 4]),
        chunk(b"OBMG", 1, vec![0; 8]),
        chunk(b"DFMG", 1, vec![]),
        chunk(b"FUNQ", 1, vec![0; 4]),
        chunk(b"WLST", 1, vec![0; 12]),
        chunk(b"____", 1, vec![]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/f4se-0.6.23-probe.f4se");

    #[test]
    fn parses_and_round_trips_an_f4se_cosave() {
        let save = CoSave::parse(FIXTURE).unwrap();
        assert_eq!((save.format_version, save.f4se_version, save.runtime_version), (1, 0x0006_0170, 0x010A_0A30));
        assert_eq!(save.plugins.len(), 2);
        let host = save.plugin(HOST_UID).unwrap();
        assert_eq!(host.chunks.len(), 14);
        let order = plgn::parse(&host.chunks[0].data).unwrap();
        assert_eq!(order.len(), 7);
        assert_eq!(order[0], plgn::Entry::full(0, "Fallout4.esm"));
        assert_eq!(order[6].name, "DLCNukaWorld.esm");

        let probe = save.plugin(tag(b"F4XP")).unwrap();
        let kinds: Vec<String> = probe.chunks.iter().map(|c| fourcc(c.kind)).collect();
        assert_eq!(kinds, ["TST1", "TST2", "TST3"]);
        assert_eq!(probe.chunks[0].data, b"hello");
        assert_eq!(probe.chunks[1].data, b"abcdefg\x00\x01\x02\xff");
        assert_eq!(probe.chunks[1].version, 2);

        assert_eq!(save.to_bytes(), FIXTURE);
    }

    #[test]
    fn empty_host_block_matches_f4se() {
        let save = CoSave::parse(FIXTURE).unwrap();
        let host = save.plugin(HOST_UID).unwrap();
        let order = plgn::parse(&host.chunks[0].data).unwrap();
        assert_eq!(empty_host_chunks(&order), host.chunks);
    }

    #[test]
    fn plgn_light_entries_round_trip() {
        // As F4SE 0.6.23 writes them for a load order with light plugins.
        let order = vec![
            plgn::Entry::full(0x0D, "StartMeUp.esp"),
            plgn::Entry::light(0, "CRP Radio Towers 2.5.esp"),
            plgn::Entry::light(1, "YAWA2_CRPMMRRN_Patch.esp"),
            plgn::Entry::full(0x0E, "FCOM.esp"),
        ];
        let bytes = plgn::write(&order);
        assert_eq!(&bytes[2..21], b"\x0d\x0d\x00StartMeUp.esp\xfe\x00\x00");
        assert_eq!(plgn::parse(&bytes).unwrap(), order);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(CoSave::parse(b"NOPE").is_err());
        assert!(CoSave::parse(&FIXTURE[..100]).is_err());
    }
}

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

/// F4SE's registration tables in its host block: a header chunk (`KEYR` keys, `CTLR`
/// controls, `EXEV` external events), one `REGS` chunk per registered name, then `REGE`.
/// Seen black-box (see `docs/formats/cosave.md`).
pub mod regs {
    use super::{Chunk, tag};

    pub const KEYS: [u8; 4] = *b"KEYR";
    pub const CONTROLS: [u8; 4] = *b"CTLR";
    pub const EXTERNAL_EVENTS: [u8; 4] = *b"EXEV";
    /// Camera-state registrations: one chunk holding a registrant list.
    pub const CAMERA: [u8; 4] = *b"CAMR";
    /// Furniture registrations: one chunk (see [`write_furniture`]).
    pub const FURNITURE: [u8; 4] = *b"FRNR";
    pub const ENTRY: [u8; 4] = *b"REGS";
    pub const END: [u8; 4] = *b"REGE";

    /// A script registered for something: its VM handle and script name.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Registrant {
        pub handle: u64,
        pub script: String,
    }

    fn put_str(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u16).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    /// A `REGS` entry keyed by name (controls): `u16` name, `u32` count, then per
    /// registrant a `u64` handle and a `u16`-prefixed script name.
    #[must_use]
    pub fn write_named(name: &str, registrants: &[Registrant]) -> Vec<u8> {
        let mut out = Vec::new();
        put_str(&mut out, name);
        out.extend_from_slice(&(registrants.len() as u32).to_le_bytes());
        for r in registrants {
            out.extend_from_slice(&r.handle.to_le_bytes());
            put_str(&mut out, &r.script);
        }
        out
    }

    #[must_use]
    pub fn parse_named(data: &[u8]) -> Option<(String, Vec<Registrant>)> {
        let mut at = 0;
        let str_at = |at: &mut usize| -> Option<String> {
            let len = u16::from_le_bytes(data.get(*at..*at + 2)?.try_into().ok()?) as usize;
            let s = String::from_utf8_lossy(data.get(*at + 2..*at + 2 + len)?).into_owned();
            *at += 2 + len;
            Some(s)
        };
        let name = str_at(&mut at)?;
        let count = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        let mut out = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let handle = u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?);
            at += 8;
            out.push(Registrant { handle, script: str_at(&mut at)? });
        }
        Some((name, out))
    }

    /// A registrant with a callback function (external events).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Callback {
        pub handle: u64,
        pub script: String,
        pub callback: String,
    }

    /// An external-event `REGS` entry: like [`write_named`], with a `u16`-prefixed callback
    /// name after each script name.
    #[must_use]
    pub fn write_callbacks(name: &str, registrants: &[Callback]) -> Vec<u8> {
        let mut out = Vec::new();
        put_str(&mut out, name);
        out.extend_from_slice(&(registrants.len() as u32).to_le_bytes());
        for r in registrants {
            out.extend_from_slice(&r.handle.to_le_bytes());
            put_str(&mut out, &r.script);
            put_str(&mut out, &r.callback);
        }
        out
    }

    #[must_use]
    pub fn parse_callbacks(data: &[u8]) -> Option<(String, Vec<Callback>)> {
        let mut at = 0;
        let str_at = |at: &mut usize| -> Option<String> {
            let len = u16::from_le_bytes(data.get(*at..*at + 2)?.try_into().ok()?) as usize;
            let s = String::from_utf8_lossy(data.get(*at + 2..*at + 2 + len)?).into_owned();
            *at += 2 + len;
            Some(s)
        };
        let name = str_at(&mut at)?;
        let count = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        let mut out = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let handle = u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?);
            at += 8;
            let script = str_at(&mut at)?;
            let callback = str_at(&mut at)?;
            out.push(Callback { handle, script, callback });
        }
        Some((name, out))
    }

    /// A list of registrants: `u32` count, then per registrant a `u64` handle and a
    /// `u16`-prefixed script name.
    #[must_use]
    pub fn write_registrants(registrants: &[Registrant]) -> Vec<u8> {
        let mut out = (registrants.len() as u32).to_le_bytes().to_vec();
        for r in registrants {
            out.extend_from_slice(&r.handle.to_le_bytes());
            put_str(&mut out, &r.script);
        }
        out
    }

    /// The registrant list at `at` (see [`write_registrants`]) and the offset after it.
    #[must_use]
    pub fn parse_registrants(data: &[u8], mut at: usize) -> Option<(Vec<Registrant>, usize)> {
        let count = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        let mut out = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let handle = u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?);
            let len = u16::from_le_bytes(data.get(at + 8..at + 10)?.try_into().ok()?) as usize;
            let script = String::from_utf8_lossy(data.get(at + 10..at + 10 + len)?).into_owned();
            at += 10 + len;
            out.push(Registrant { handle, script });
        }
        Some((out, at))
    }

    /// A key `REGS` entry (`KEYR` section): the `u32` key code, then the registrants
    /// ([`write_registrants`]).
    #[must_use]
    pub fn write_key(key: u32, registrants: &[Registrant]) -> Vec<u8> {
        let mut out = key.to_le_bytes().to_vec();
        out.extend_from_slice(&write_registrants(registrants));
        out
    }

    #[must_use]
    pub fn parse_key(data: &[u8]) -> Option<(u32, Vec<Registrant>)> {
        let key = u32::from_le_bytes(data.get(..4)?.try_into().ok()?);
        let (list, _) = parse_registrants(data, 4)?;
        Some((key, list))
    }

    /// The `FRNR` chunk: a `u32` count, then per registrant a `u64` handle, a
    /// `u16`-prefixed script name and a `u32` that F4SE 0.6.23 always writes as 0
    /// (black-box: whatever filter was passed to `RegisterForFurnitureEvent`). Read back
    /// as a count of `u32`s to skip.
    #[must_use]
    pub fn write_furniture(registrants: &[Registrant]) -> Vec<u8> {
        let mut out = (registrants.len() as u32).to_le_bytes().to_vec();
        for r in registrants {
            out.extend_from_slice(&r.handle.to_le_bytes());
            put_str(&mut out, &r.script);
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        out
    }

    #[must_use]
    pub fn parse_furniture(data: &[u8]) -> Option<Vec<Registrant>> {
        let count = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
        let mut at = 4;
        let mut out = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let handle = u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?);
            let len = u16::from_le_bytes(data.get(at + 8..at + 10)?.try_into().ok()?) as usize;
            let script = String::from_utf8_lossy(data.get(at + 10..at + 10 + len)?).into_owned();
            at += 10 + len;
            let extra = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
            at += 4 + 4 * extra.min(1 << 16);
            out.push(Registrant { handle, script });
        }
        Some(out)
    }

    /// The data of the first chunk of kind `kind`.
    #[must_use]
    pub fn chunk(chunks: &[Chunk], kind: [u8; 4]) -> Option<&[u8]> {
        chunks.iter().find(|c| c.kind == tag(&kind)).map(|c| c.data.as_slice())
    }

    /// Replace the data of the first chunk of kind `kind` (kept in place).
    pub fn replace_chunk(chunks: &mut [Chunk], kind: [u8; 4], data: Vec<u8>) {
        if let Some(c) = chunks.iter_mut().find(|c| c.kind == tag(&kind)) {
            c.data = data;
        }
    }

    fn bounds(chunks: &[Chunk], header: [u8; 4]) -> Option<(usize, usize)> {
        let start = chunks.iter().position(|c| c.kind == tag(&header))?;
        let end = chunks[start + 1..].iter().position(|c| c.kind == tag(&END)).map_or(chunks.len(), |i| start + 1 + i);
        Some((start, end))
    }

    /// The `REGS` chunks of the section that starts with `header`.
    #[must_use]
    pub fn section(chunks: &[Chunk], header: [u8; 4]) -> Vec<&Chunk> {
        bounds(chunks, header)
            .map(|(start, end)| chunks[start + 1..end].iter().filter(|c| c.kind == tag(&ENTRY)).collect())
            .unwrap_or_default()
    }

    /// Replace the entries of the section that starts with `header`.
    pub fn replace_section(chunks: &mut Vec<Chunk>, header: [u8; 4], entries: Vec<Vec<u8>>) {
        let Some((start, end)) = bounds(chunks, header) else { return };
        let new: Vec<Chunk> = entries.into_iter().map(|data| Chunk { kind: tag(&ENTRY), version: 1, data }).collect();
        chunks.splice(start + 1..end, new);
    }
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
    fn control_registrations_match_f4se() {
        // F4SE 0.6.23's REGS for Settle Anywhere's RegisterForControl("WorkshopMode").
        let f4se: &[u8] = b"\x0c\x00WorkshopMode\x01\x00\x00\x00\x1c\x00\x00\x0c\x01\x00\x00\x00\x13\x00SAPlayerAliasScript";
        let entry = regs::Registrant { handle: 0x0000_0001_0C00_001C, script: "SAPlayerAliasScript".into() };
        assert_eq!(regs::write_named("WorkshopMode", std::slice::from_ref(&entry)), f4se);
        assert_eq!(regs::parse_named(f4se).unwrap(), ("WorkshopMode".into(), vec![entry]));

        let mut chunks = empty_host_chunks(&[]);
        regs::replace_section(&mut chunks, regs::CONTROLS, vec![f4se.to_vec()]);
        let kinds: Vec<String> = chunks.iter().map(|c| fourcc(c.kind)).collect();
        assert_eq!(&kinds[1..7], ["KEYR", "REGE", "CTLR", "REGS", "REGE", "EXEV"]);
        assert_eq!(regs::section(&chunks, regs::CONTROLS).len(), 1);
        assert!(regs::section(&chunks, regs::KEYS).is_empty());
    }

    #[test]
    fn external_event_registrations_match_f4se() {
        // F4SE 0.6.23's REGS for RegisterForExternalEvent("f4xEvtA", "F4xProbeCallbackA")
        // on MQ101 (script mq101questscript).
        let f4se: &[u8] = b"\x07\x00f4xEvtA\x01\x00\x00\x00\x86\xed\x01\x00\xff\xff\x00\x00\x10\x00mq101questscript\x11\x00F4xProbeCallbackA";
        let entry = regs::Callback { handle: 0x0000_FFFF_0001_ED86, script: "mq101questscript".into(), callback: "F4xProbeCallbackA".into() };
        assert_eq!(regs::write_callbacks("f4xEvtA", std::slice::from_ref(&entry)), f4se);
        assert_eq!(regs::parse_callbacks(f4se).unwrap(), ("f4xEvtA".into(), vec![entry]));
    }

    /// F4SE 0.6.23's host block after `RegisterForKey` and `RegisterForCameraState` calls
    /// (black-box, `events-f4se` profile, 2026-11-22).
    const KEYS_CAMERA: &[u8] = include_bytes!("../tests/fixtures/f4se-0.6.23-keys-camera.f4se");

    fn reg(handle: u64, script: &str) -> regs::Registrant {
        regs::Registrant { handle, script: script.into() }
    }

    #[test]
    fn key_registrations_match_f4se() {
        // RegisterForKey(0x21) on TimeScale (GlobalVariable) and MQ101: registrants by handle.
        let f4se: &[u8] = b"\x21\x00\x00\x00\x02\x00\x00\x00\x3a\x00\x00\x00\xff\xff\x00\x00\x0e\x00GlobalVariable\x86\xed\x01\x00\xff\xff\x00\x00\x10\x00mq101questscript";
        let list = vec![reg(0x0000_FFFF_0000_003A, "GlobalVariable"), reg(0x0000_FFFF_0001_ED86, "mq101questscript")];
        assert_eq!(regs::write_key(0x21, &list), f4se);
        assert_eq!(regs::parse_key(f4se).unwrap(), (0x21, list));

        // The whole section: one REGS per key, in key order (as u32: -1 last).
        let save = CoSave::parse(KEYS_CAMERA).unwrap();
        let host = &save.plugin(HOST_UID).unwrap().chunks;
        let keys: Vec<u32> = regs::section(host, regs::KEYS).iter().map(|c| regs::parse_key(&c.data).unwrap().0).collect();
        assert_eq!(keys, [0x21, 0x30, 0x39, 0x100, 0x101, 0x114, 0x12C, 0xFFFF_FFFF]);
        let (_, k48) = regs::parse_key(&regs::section(host, regs::KEYS)[1].data).unwrap();
        let handles: Vec<u64> = k48.iter().map(|r| r.handle).collect();
        assert_eq!(handles, [0x0000_FFFF_0000_0014, 0x0000_FFFF_0000_0039, 0x0000_FFFF_0000_003A, 0x0000_FFFF_0002_29E5]);
        assert_eq!(k48[0].script, "ObjectReference");
        for c in regs::section(host, regs::KEYS) {
            let (key, list) = regs::parse_key(&c.data).unwrap();
            assert_eq!(regs::write_key(key, &list), c.data);
        }
    }

    #[test]
    fn camera_registrations_match_f4se() {
        let save = CoSave::parse(KEYS_CAMERA).unwrap();
        let host = &save.plugin(HOST_UID).unwrap().chunks;
        let data = regs::chunk(host, regs::CAMERA).unwrap();
        let (list, end) = regs::parse_registrants(data, 0).unwrap();
        assert_eq!(end, data.len());
        assert_eq!(
            list,
            [
                reg(0x0000_FFFF_0000_0014, "ObjectReference"),
                reg(0x0000_FFFF_0000_0039, "GlobalVariable"),
                reg(0x0000_FFFF_0001_ED86, "mq101questscript"),
                reg(0x0000_FFFF_0002_29E5, "ScriptObject"),
            ]
        );
        assert_eq!(regs::write_registrants(&list), data);
        // Nothing registered: a zero count.
        assert_eq!(regs::write_registrants(&[]), [0, 0, 0, 0]);
    }

    #[test]
    fn rebuilt_host_block_matches_f4se() {
        // Replacing every registration table with what it already holds changes nothing.
        let save = CoSave::parse(KEYS_CAMERA).unwrap();
        let host = save.plugin(HOST_UID).unwrap().chunks.clone();
        let keys: Vec<Vec<u8>> = regs::section(&host, regs::KEYS)
            .iter()
            .map(|c| {
                let (k, l) = regs::parse_key(&c.data).unwrap();
                regs::write_key(k, &l)
            })
            .collect();
        let (camera, _) = regs::parse_registrants(regs::chunk(&host, regs::CAMERA).unwrap(), 0).unwrap();
        let mut rebuilt = empty_host_chunks(&plgn::parse(&host[0].data).unwrap());
        regs::replace_section(&mut rebuilt, regs::KEYS, keys);
        regs::replace_chunk(&mut rebuilt, regs::CAMERA, regs::write_registrants(&camera));
        assert_eq!(rebuilt, host);
    }

    #[test]
    fn furniture_registrations_match_f4se() {
        // RegisterForFurnitureEvent on six forms, with forms, a FormList and an array as
        // filters (black-box, `events-f4se` profile, 2026-11-22): registrants by handle,
        // each followed by a zero `u32`.
        const FURNITURE: &[u8] = include_bytes!("../tests/fixtures/f4se-0.6.23-furniture.f4se");
        let save = CoSave::parse(FURNITURE).unwrap();
        let host = &save.plugin(HOST_UID).unwrap().chunks;
        let data = regs::chunk(host, regs::FURNITURE).unwrap();
        let list = regs::parse_furniture(data).unwrap();
        let handles: Vec<u64> = list.iter().map(|r| r.handle & 0xFFFF_FFFF).collect();
        assert_eq!(handles, [0x35, 0x36, 0x38, 0x39, 0x3A, 0x1ED86]);
        assert_eq!(list[0], reg(0x0000_FFFF_0000_0035, "ScriptObject"));
        assert_eq!(list[5], reg(0x0000_FFFF_0001_ED86, "mq101questscript"));
        assert_eq!(regs::write_furniture(&list), data);
        assert_eq!(regs::write_furniture(&[]), [0, 0, 0, 0]);
        let mut rebuilt = empty_host_chunks(&plgn::parse(&host[0].data).unwrap());
        let (camera, _) = regs::parse_registrants(regs::chunk(host, regs::CAMERA).unwrap(), 0).unwrap();
        regs::replace_chunk(&mut rebuilt, regs::CAMERA, regs::write_registrants(&camera));
        regs::replace_chunk(&mut rebuilt, regs::FURNITURE, regs::write_furniture(&list));
        assert_eq!(&rebuilt, host);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(CoSave::parse(b"NOPE").is_err());
        assert!(CoSave::parse(&FIXTURE[..100]).is_err());
    }
}

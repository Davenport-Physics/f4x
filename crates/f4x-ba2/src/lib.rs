//! Reader for Fallout 4 BA2 archives (magic `BTDX`).
//!
//! - **GNRL** (general) archives: entries are listed and extracted, zlib-compressed or stored.
//! - **DX10** (texture) archives: entries and their mip chunks are listed. Texture extraction
//!   (rebuilding a `.dds` header) is out of scope.
//! - Versions 1 (original release) and 7/8 (next-gen update) share the same 24-byte header
//!   and record layouts; Starfield's versions 2/3 extend the header and are rejected.
//!
//! Format sources are recorded in `provenance/formats.jsonl` (`format/ba2/*`).
//!
//! ```no_run
//! let archive = f4x_ba2::Archive::open("Fallout4 - Misc.ba2")?;
//! for entry in archive.entries() {
//!     println!("{} {} bytes", entry.name, entry.size());
//! }
//! let first = archive.extract(&archive.entries()[0])?;
//! # Ok::<(), f4x_ba2::Error>(())
//! ```

mod cp1252;
mod error;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

pub use error::Error;

/// `BTDX`, the first four bytes of every BA2.
pub const MAGIC: [u8; 4] = *b"BTDX";
/// Versions this reader accepts.
pub const SUPPORTED_VERSIONS: [u32; 3] = [1, 7, 8];
/// The `0xBAADF00D` value that ends every GNRL record and DX10 chunk record.
pub const RECORD_SENTINEL: u32 = 0xBAAD_F00D;

const HEADER_LEN: usize = 24;
const GNRL_RECORD_LEN: usize = 36;
const DX10_RECORD_LEN: usize = 24;
const DX10_CHUNK_LEN: usize = 24;

/// What an archive holds: the four-byte type tag after the version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    /// `GNRL`: any kind of file, one contiguous blob per entry.
    General,
    /// `DX10`: `.dds` textures split into mip chunks.
    Texture,
}

impl ArchiveKind {
    pub fn tag(self) -> [u8; 4] {
        match self {
            ArchiveKind::General => *b"GNRL",
            ArchiveKind::Texture => *b"DX10",
        }
    }
}

/// The fixed 24-byte archive header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub kind: ArchiveKind,
    pub file_count: u32,
    /// Absolute offset of the name table, 0 when the archive has none.
    pub name_table_offset: u64,
}

/// One file in the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Path from the name table, as stored (normally `\`-separated). Empty when the archive
    /// has no name table.
    pub name: String,
    /// Hash of the file stem, as stored.
    pub name_hash: u32,
    /// Extension without the dot, NUL-padded to four bytes (e.g. `pex\0`).
    pub extension: [u8; 4],
    /// Hash of the directory path, as stored.
    pub dir_hash: u32,
    pub data: EntryData,
}

/// The record that follows the three hashes; its shape depends on the archive kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryData {
    General(GeneralRecord),
    Texture(TextureRecord),
}

/// GNRL record tail: where the blob lives and how big it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeneralRecord {
    /// Opaque flags word (0x0010_0100 on every vanilla record seen).
    pub flags: u32,
    /// Absolute offset of the stored bytes.
    pub offset: u64,
    /// Stored (zlib) size; 0 means the entry is stored uncompressed.
    pub packed_size: u32,
    /// Size after decompression.
    pub unpacked_size: u32,
    /// Trailing word, `0xBAADF00D` in well-formed archives.
    pub sentinel: u32,
}

/// DX10 record tail: texture description and its chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureRecord {
    /// Byte after the hashes; opaque.
    pub unknown: u8,
    /// Size of one chunk record (24 in every archive seen).
    pub chunk_header_size: u16,
    pub height: u16,
    pub width: u16,
    pub mip_count: u8,
    /// DXGI_FORMAT value.
    pub dxgi_format: u8,
    /// Bit 0 set: cubemap.
    pub flags: u8,
    /// 8 on PC archives.
    pub tile_mode: u8,
    pub chunks: Vec<TextureChunk>,
}

/// One contiguous range of mips of a texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureChunk {
    pub offset: u64,
    /// 0 means stored uncompressed.
    pub packed_size: u32,
    pub unpacked_size: u32,
    pub start_mip: u16,
    pub end_mip: u16,
    pub sentinel: u32,
}

impl Entry {
    /// Decompressed size in bytes (sum of chunks for a texture, without a DDS header).
    pub fn size(&self) -> u64 {
        match &self.data {
            EntryData::General(g) => u64::from(g.unpacked_size),
            EntryData::Texture(t) => t.chunks.iter().map(|c| u64::from(c.unpacked_size)).sum(),
        }
    }

    /// Bytes occupied in the archive.
    pub fn stored_size(&self) -> u64 {
        let stored = |packed: u32, unpacked: u32| u64::from(if packed == 0 { unpacked } else { packed });
        match &self.data {
            EntryData::General(g) => stored(g.packed_size, g.unpacked_size),
            EntryData::Texture(t) => t.chunks.iter().map(|c| stored(c.packed_size, c.unpacked_size)).sum(),
        }
    }

    /// True if any part of the entry is zlib-compressed.
    pub fn is_compressed(&self) -> bool {
        match &self.data {
            EntryData::General(g) => g.packed_size != 0,
            EntryData::Texture(t) => t.chunks.iter().any(|c| c.packed_size != 0),
        }
    }

    /// The extension as text (`"pex"`), trailing NULs removed.
    pub fn extension_str(&self) -> String {
        let end = self.extension.iter().position(|&b| b == 0).unwrap_or(4);
        cp1252::decode(&self.extension[..end])
    }
}

enum Source {
    Bytes(Vec<u8>),
    File { file: Mutex<File>, len: u64 },
}

impl Source {
    fn len(&self) -> u64 {
        match self {
            Source::Bytes(bytes) => bytes.len() as u64,
            Source::File { len, .. } => *len,
        }
    }

    /// Reads exactly `len` bytes at `offset`, failing with `Truncated` past the end.
    fn read_at(&self, offset: u64, len: usize, what: &'static str) -> Result<Vec<u8>, Error> {
        let end = offset.checked_add(len as u64).ok_or(Error::Truncated { what, offset })?;
        if end > self.len() {
            return Err(Error::Truncated { what, offset });
        }
        match self {
            Source::Bytes(bytes) => Ok(bytes[offset as usize..end as usize].to_vec()),
            Source::File { file, .. } => {
                let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                file.seek(SeekFrom::Start(offset))?;
                let mut buf = vec![0; len];
                file.read_exact(&mut buf)?;
                Ok(buf)
            }
        }
    }
}

/// An opened archive: the parsed index plus a handle to the bytes.
pub struct Archive {
    header: Header,
    entries: Vec<Entry>,
    source: Source,
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive").field("header", &self.header).field("entries", &self.entries.len()).finish()
    }
}

impl Archive {
    /// Opens an archive on disk. Only the header, records and name table are read up front;
    /// entry data is read on [`Archive::extract`].
    pub fn open(path: impl AsRef<Path>) -> Result<Archive, Error> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Archive::from_source(Source::File { file: Mutex::new(file), len })
    }

    /// Parses an archive held in memory.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Archive, Error> {
        Archive::from_source(Source::Bytes(bytes))
    }

    fn from_source(source: Source) -> Result<Archive, Error> {
        let raw = source.read_at(0, HEADER_LEN, "header")?;
        let mut r = Le::new(&raw);
        let magic = r.array::<4>();
        if magic != MAGIC {
            return Err(Error::BadMagic(magic));
        }
        let version = r.u32();
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(Error::UnsupportedVersion(version));
        }
        let kind = match &r.array::<4>() {
            b"GNRL" => ArchiveKind::General,
            b"DX10" => ArchiveKind::Texture,
            other => return Err(Error::UnsupportedKind(*other)),
        };
        let file_count = r.u32();
        let name_table_offset = r.u64();
        let header = Header { version, kind, file_count, name_table_offset };

        let entries = match kind {
            ArchiveKind::General => read_general_records(&source, file_count)?,
            ArchiveKind::Texture => read_texture_records(&source, file_count)?,
        };
        let mut archive = Archive { header, entries, source };
        if name_table_offset != 0 {
            archive.read_names()?;
        }
        Ok(archive)
    }

    fn read_names(&mut self) -> Result<(), Error> {
        let start = self.header.name_table_offset;
        let to_eof = self
            .source
            .len()
            .checked_sub(start)
            .ok_or(Error::Truncated { what: "name table", offset: start })?;
        // Vanilla archives end with the name table; never read more than the longest
        // possible table, in case it sits before other data.
        let longest = self.entries.len() as u64 * (2 + u64::from(u16::MAX));
        let table = self.source.read_at(start, to_eof.min(longest) as usize, "name table")?;
        let mut pos = 0usize;
        for entry in &mut self.entries {
            let at = start + pos as u64;
            let Some(len_bytes) = table.get(pos..pos + 2) else {
                return Err(Error::Truncated { what: "name table", offset: at });
            };
            let n = usize::from(u16::from_le_bytes([len_bytes[0], len_bytes[1]]));
            let Some(name) = table.get(pos + 2..pos + 2 + n) else {
                return Err(Error::Truncated { what: "name table", offset: at });
            };
            entry.name = cp1252::decode(name);
            pos += 2 + n;
        }
        Ok(())
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Finds an entry by path, ignoring ASCII case and treating `/` and `\` alike.
    pub fn find(&self, path: &str) -> Option<&Entry> {
        let want = normalize(path);
        self.entries.iter().find(|e| normalize(&e.name) == want)
    }

    /// Returns the decompressed bytes of a GNRL entry.
    pub fn extract(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        let EntryData::General(record) = &entry.data else {
            return Err(Error::TextureExtraction(entry.name.clone()));
        };
        if record.packed_size == 0 {
            return self.source.read_at(record.offset, record.unpacked_size as usize, "entry data");
        }
        let packed = self.source.read_at(record.offset, record.packed_size as usize, "entry data")?;
        let expected = u64::from(record.unpacked_size);
        // Cap the up-front allocation and the inflated length (one byte past the record's
        // size is enough to detect a mismatch) so a bad record cannot exhaust memory.
        let mut out = Vec::with_capacity((record.unpacked_size as usize).min(1 << 24));
        flate2::read::ZlibDecoder::new(packed.as_slice())
            .take(expected + 1)
            .read_to_end(&mut out)
            .map_err(|source| Error::Decompress { name: entry.name.clone(), source })?;
        if out.len() != record.unpacked_size as usize {
            return Err(Error::SizeMismatch { name: entry.name.clone(), expected, actual: out.len() as u64 });
        }
        Ok(out)
    }
}

fn normalize(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

fn read_general_records(source: &Source, count: u32) -> Result<Vec<Entry>, Error> {
    let raw = source.read_at(HEADER_LEN as u64, count as usize * GNRL_RECORD_LEN, "file records")?;
    let mut r = Le::new(&raw);
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (name_hash, extension, dir_hash) = (r.u32(), r.array::<4>(), r.u32());
        let record = GeneralRecord {
            flags: r.u32(),
            offset: r.u64(),
            packed_size: r.u32(),
            unpacked_size: r.u32(),
            sentinel: r.u32(),
        };
        entries.push(Entry {
            name: String::new(),
            name_hash,
            extension,
            dir_hash,
            data: EntryData::General(record),
        });
    }
    Ok(entries)
}

fn read_texture_records(source: &Source, count: u32) -> Result<Vec<Entry>, Error> {
    // Records are variable-length (chunk count), so read the fixed part, then the chunks.
    let mut pos = HEADER_LEN as u64;
    let fit = (source.len() / DX10_RECORD_LEN as u64).min(u64::from(count));
    let mut entries = Vec::with_capacity(fit as usize);
    for _ in 0..count {
        let raw = source.read_at(pos, DX10_RECORD_LEN, "texture record")?;
        let mut r = Le::new(&raw);
        let (name_hash, extension, dir_hash) = (r.u32(), r.array::<4>(), r.u32());
        let unknown = r.u8();
        let chunk_count = r.u8();
        let chunk_header_size = r.u16();
        let (height, width) = (r.u16(), r.u16());
        let (mip_count, dxgi_format, flags, tile_mode) = (r.u8(), r.u8(), r.u8(), r.u8());
        pos += DX10_RECORD_LEN as u64;
        if usize::from(chunk_header_size) != DX10_CHUNK_LEN {
            return Err(Error::UnsupportedChunkHeader { offset: pos - 12, size: chunk_header_size });
        }
        let raw = source.read_at(pos, usize::from(chunk_count) * DX10_CHUNK_LEN, "texture chunks")?;
        let mut r = Le::new(&raw);
        let chunks = (0..chunk_count)
            .map(|_| TextureChunk {
                offset: r.u64(),
                packed_size: r.u32(),
                unpacked_size: r.u32(),
                start_mip: r.u16(),
                end_mip: r.u16(),
                sentinel: r.u32(),
            })
            .collect();
        pos += u64::from(chunk_count) * DX10_CHUNK_LEN as u64;
        let record = TextureRecord {
            unknown,
            chunk_header_size,
            height,
            width,
            mip_count,
            dxgi_format,
            flags,
            tile_mode,
            chunks,
        };
        entries.push(Entry {
            name: String::new(),
            name_hash,
            extension,
            dir_hash,
            data: EntryData::Texture(record),
        });
    }
    Ok(entries)
}

/// Little-endian cursor over a buffer that the caller has already length-checked.
struct Le<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Le<'a> {
    fn new(buf: &'a [u8]) -> Le<'a> {
        Le { buf, pos: 0 }
    }

    fn array<const N: usize>(&mut self) -> [u8; N] {
        let out: [u8; N] = self.buf[self.pos..self.pos + N].try_into().expect("length checked by caller");
        self.pos += N;
        out
    }

    fn u8(&mut self) -> u8 {
        self.array::<1>()[0]
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.array())
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.array())
    }

    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.array())
    }
}

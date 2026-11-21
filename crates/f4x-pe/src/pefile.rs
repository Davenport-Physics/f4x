use std::fmt;

use crate::image::{Image, Section};

/// Why bytes are not a usable PE32+ image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeError(pub String);

impl fmt::Display for PeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a usable PE32+ image: {}", self.0)
    }
}

impl std::error::Error for PeError {}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// Data directory indices.
const DIR_EXPORT: usize = 0;
const DIR_IMPORT: usize = 1;
const DIR_EXCEPTION: usize = 3;
const DIR_BASERELOC: usize = 5;

/// The parts of the headers shared by files and mapped images.
pub(crate) struct Headers {
    pub image_base: u64,
    pub size_of_image: u32,
    pub timestamp: u32,
    pub directories: Vec<(u32, u32)>,
    /// `readable_size` is the raw (file) size here.
    pub sections: Vec<Section>,
    pub raw_offsets: Vec<u32>,
}

impl Headers {
    pub fn parse(bytes: &[u8]) -> Result<Self, PeError> {
        let bad = |what: &str| PeError(what.to_owned());
        if bytes.get(..2) != Some(b"MZ") {
            return Err(bad("missing MZ header"));
        }
        let pe = u32_at(bytes, 0x3C).ok_or_else(|| bad("truncated"))? as usize;
        if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
            return Err(bad("missing PE signature"));
        }
        let timestamp = u32_at(bytes, pe + 8).ok_or_else(|| bad("truncated"))?;
        let section_count = u16_at(bytes, pe + 6).ok_or_else(|| bad("truncated"))? as usize;
        let optional_size = u16_at(bytes, pe + 20).ok_or_else(|| bad("truncated"))? as usize;
        let opt = pe + 24;
        if u16_at(bytes, opt) != Some(0x20B) {
            return Err(bad("not PE32+"));
        }
        let image_base = u64_at(bytes, opt + 24).ok_or_else(|| bad("truncated"))?;
        let size_of_image = u32_at(bytes, opt + 56).ok_or_else(|| bad("truncated"))?;
        let dir_count = u32_at(bytes, opt + 108).ok_or_else(|| bad("truncated"))?.min(16) as usize;
        let directories = (0..dir_count)
            .map(|i| {
                Some((u32_at(bytes, opt + 112 + 8 * i)?, u32_at(bytes, opt + 112 + 8 * i + 4)?))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad("truncated data directories"))?;

        let mut sections = Vec::with_capacity(section_count);
        let mut raw_offsets = Vec::with_capacity(section_count);
        for i in 0..section_count {
            let s = opt + optional_size + 40 * i;
            let name = bytes.get(s..s + 8).ok_or_else(|| bad("truncated section table"))?;
            let end = name.iter().position(|&c| c == 0).unwrap_or(8);
            let field = |off| u32_at(bytes, s + off).ok_or_else(|| bad("truncated section table"));
            sections.push(Section {
                name: String::from_utf8_lossy(&name[..end]).into_owned(),
                virtual_size: field(8)?,
                rva: field(12)?,
                readable_size: field(16)?,
                characteristics: field(36)?,
            });
            raw_offsets.push(field(20)?);
        }
        Ok(Headers { image_base, size_of_image, timestamp, directories, sections, raw_offsets })
    }
}

/// One imported function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// DLL name as written in the image, e.g. `USER32.dll`.
    pub dll: String,
    /// Function name, or `None` for an import by ordinal.
    pub name: Option<String>,
    pub ordinal: Option<u16>,
    /// RVA of the IAT slot the loader fills in.
    pub iat_rva: u32,
}

/// A PE32+ image parsed from file bytes.
pub struct PeFile<'a> {
    bytes: &'a [u8],
    headers: Headers,
}

impl<'a> PeFile<'a> {
    /// Parse a 64-bit PE image.
    ///
    /// # Errors
    /// Fails if `bytes` isn't a PE32+ image.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PeError> {
        Ok(PeFile { bytes, headers: Headers::parse(bytes)? })
    }

    /// The preferred load address.
    #[must_use]
    pub fn image_base(&self) -> u64 {
        self.headers.image_base
    }

    #[must_use]
    pub fn size_of_image(&self) -> u32 {
        self.headers.size_of_image
    }

    /// The COFF `TimeDateStamp` (part of a build's identity).
    #[must_use]
    pub fn timestamp(&self) -> u32 {
        self.headers.timestamp
    }

    fn directory(&self, index: usize) -> Option<(u32, u32)> {
        self.headers.directories.get(index).copied().filter(|&(rva, size)| rva != 0 && size != 0)
    }

    /// Start RVAs of every function in the exception table, sorted. Leaf functions
    /// that need no unwind data are not listed.
    #[must_use]
    pub fn function_starts(&self) -> Vec<u32> {
        let Some((rva, size)) = self.directory(DIR_EXCEPTION) else { return Vec::new() };
        let Some(table) = self.bytes(rva, size as usize) else { return Vec::new() };
        // RUNTIME_FUNCTION entries: { begin, end, unwind_info } RVAs.
        let mut starts: Vec<u32> =
            table.as_chunks::<12>().0.iter().map(|e| u32::from_le_bytes([e[0], e[1], e[2], e[3]])).collect();
        starts.sort_unstable();
        starts
    }

    /// `(begin, end)` RVA ranges of every function with unwind data, sorted by begin.
    #[must_use]
    pub fn function_ranges(&self) -> Vec<(u32, u32)> {
        let Some((rva, size)) = self.directory(DIR_EXCEPTION) else { return Vec::new() };
        let Some(table) = self.bytes(rva, size as usize) else { return Vec::new() };
        let mut ranges: Vec<(u32, u32)> = table
            .as_chunks::<12>()
            .0
            .iter()
            .map(|e| (u32::from_le_bytes([e[0], e[1], e[2], e[3]]), u32::from_le_bytes([e[4], e[5], e[6], e[7]])))
            .collect();
        ranges.sort_unstable();
        ranges
    }

    /// Whether `rva` plausibly starts a function: it begins an unwind-table entry, or it
    /// is a 16-byte-aligned address in executable code outside every unwind range (a
    /// leaf function, which MSVC emits without unwind data). `ranges` comes from
    /// [`function_ranges`](Self::function_ranges).
    #[must_use]
    pub fn is_function_start(&self, ranges: &[(u32, u32)], rva: u32) -> bool {
        let index = ranges.partition_point(|&(begin, _)| begin <= rva);
        if index > 0 {
            let (begin, end) = ranges[index - 1];
            if begin == rva {
                return true;
            }
            if rva < end {
                return false; // inside another function
            }
        }
        rva % 16 == 0 && self.section_at(rva).is_some_and(Section::is_executable)
    }

    /// Every imported function, in table order.
    #[must_use]
    pub fn imports(&self) -> Vec<Import> {
        let mut out = Vec::new();
        let Some((dir, _)) = self.directory(DIR_IMPORT) else { return out };
        for index in 0.. {
            let descriptor = dir + 20 * index;
            let (Some(lookup), Some(name_rva), Some(iat)) =
                (self.u32_at(descriptor), self.u32_at(descriptor + 12), self.u32_at(descriptor + 16))
            else {
                break;
            };
            if name_rva == 0 {
                break;
            }
            let Some(dll) = self.c_str_at(name_rva, 256) else { break };
            let lookup = if lookup != 0 { lookup } else { iat };
            for slot in 0.. {
                let Some(entry) = self.u64_at(lookup + 8 * slot) else { break };
                if entry == 0 {
                    break;
                }
                let (name, ordinal) = if entry & (1 << 63) != 0 {
                    (None, Some(entry as u16))
                } else {
                    (self.c_str_at(entry as u32 + 2, 512).map(str::to_owned), None)
                };
                out.push(Import { dll: dll.to_owned(), name, ordinal, iat_rva: iat + 8 * slot });
            }
        }
        out
    }

    /// RVAs of every 64-bit absolute pointer the loader relocates (`IMAGE_REL_BASED_DIR64`).
    #[must_use]
    pub fn relocations(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let Some((dir, size)) = self.directory(DIR_BASERELOC) else { return out };
        let mut block = dir;
        while block < dir + size {
            let (Some(page), Some(block_size)) = (self.u32_at(block), self.u32_at(block + 4)) else { break };
            if block_size < 8 {
                break;
            }
            let Some(entries) = self.bytes(block + 8, block_size as usize - 8) else { break };
            for entry in entries.as_chunks::<2>().0 {
                let entry = u16::from_le_bytes(*entry);
                if entry >> 12 == 10 {
                    out.push(page + u32::from(entry & 0xFFF));
                }
            }
            block += block_size;
        }
        out
    }

    /// Exported function names and RVAs (forwarders excluded).
    #[must_use]
    pub fn exports(&self) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        let Some((dir, size)) = self.directory(DIR_EXPORT) else { return out };
        let (Some(functions), Some(names), Some(ordinals), Some(count)) = (
            self.u32_at(dir + 28),
            self.u32_at(dir + 32),
            self.u32_at(dir + 36),
            self.u32_at(dir + 24),
        ) else {
            return out;
        };
        for i in 0..count {
            let (Some(name_rva), Some(ordinal)) =
                (self.u32_at(names + 4 * i), self.bytes(ordinals + 2 * i, 2).map(|b| u16::from_le_bytes([b[0], b[1]])))
            else {
                break;
            };
            let Some(rva) = self.u32_at(functions + 4 * u32::from(ordinal)) else { break };
            let forwarder = rva >= dir && rva < dir + size;
            if let Some(name) = self.c_str_at(name_rva, 512)
                && !forwarder
            {
                out.push((name.to_owned(), rva));
            }
        }
        out
    }
}

impl Image for PeFile<'_> {
    fn base(&self) -> u64 {
        self.headers.image_base
    }

    fn sections(&self) -> &[Section] {
        &self.headers.sections
    }

    fn bytes(&self, rva: u32, len: usize) -> Option<&[u8]> {
        let index = self.headers.sections.iter().position(|s| s.contains(rva))?;
        let section = &self.headers.sections[index];
        let within = (rva - section.rva) as usize;
        if within + len > section.readable_size as usize {
            return None;
        }
        let start = self.headers.raw_offsets[index] as usize + within;
        self.bytes.get(start..start + len)
    }
}

impl fmt::Debug for PeFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeFile")
            .field("image_base", &format_args!("{:#X}", self.headers.image_base))
            .field("sections", &self.headers.sections.iter().map(|s| s.name.as_str()).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

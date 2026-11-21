use std::fmt;

/// One section of an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Name, e.g. `.text`.
    pub name: String,
    /// Address relative to the image base.
    pub rva: u32,
    /// Size in memory.
    pub virtual_size: u32,
    /// Bytes backed by the file (0 for e.g. `.bss`-like data). For a mapped image this
    /// is the whole virtual size.
    pub readable_size: u32,
    /// `IMAGE_SCN_*` characteristics.
    pub characteristics: u32,
}

impl Section {
    pub const EXECUTE: u32 = 0x2000_0000;
    pub const WRITE: u32 = 0x8000_0000;

    /// True if `rva` falls inside this section in memory.
    #[must_use]
    pub fn contains(&self, rva: u32) -> bool {
        rva >= self.rva && rva - self.rva < self.virtual_size.max(self.readable_size)
    }

    #[must_use]
    pub fn is_executable(&self) -> bool {
        self.characteristics & Self::EXECUTE != 0
    }

    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.characteristics & Self::WRITE != 0
    }
}

/// Read-only access to an image by RVA.
pub trait Image {
    /// The address the image is (or would be) loaded at. Pointers stored inside the
    /// image are relative to this base.
    fn base(&self) -> u64;
    /// All sections.
    fn sections(&self) -> &[Section];
    /// `len` readable bytes at `rva`, or `None` if any of them is not readable.
    fn bytes(&self, rva: u32, len: usize) -> Option<&[u8]>;

    /// The section containing `rva`.
    fn section_at(&self, rva: u32) -> Option<&Section> {
        self.sections().iter().find(|s| s.contains(rva))
    }

    /// The section called `name`.
    fn section(&self, name: &str) -> Option<&Section> {
        self.sections().iter().find(|s| s.name == name)
    }

    /// All readable bytes of a section.
    fn section_bytes(&self, section: &Section) -> &[u8] {
        self.bytes(section.rva, section.readable_size as usize).unwrap_or_default()
    }

    fn u32_at(&self, rva: u32) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(rva, 4)?.try_into().ok()?))
    }

    fn i32_at(&self, rva: u32) -> Option<i32> {
        Some(i32::from_le_bytes(self.bytes(rva, 4)?.try_into().ok()?))
    }

    fn u64_at(&self, rva: u32) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(rva, 8)?.try_into().ok()?))
    }

    /// A NUL-terminated string at `rva` (at most `max` bytes).
    fn c_str_at(&self, rva: u32, max: usize) -> Option<&str> {
        let section = self.section_at(rva)?;
        let available = (section.rva + section.readable_size).checked_sub(rva)? as usize;
        let bytes = self.bytes(rva, available.min(max))?;
        let end = bytes.iter().position(|&b| b == 0)?;
        std::str::from_utf8(&bytes[..end]).ok()
    }

    /// Convert a pointer stored in the image to an RVA, if it points inside it.
    fn rva_of(&self, pointer: u64) -> Option<u32> {
        let rva = u32::try_from(pointer.checked_sub(self.base())?).ok()?;
        self.section_at(rva).map(|_| rva)
    }
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:#x}+{:#x}", self.name, self.rva, self.virtual_size)
    }
}

/// The PE image mapped into this process (by default the game's exe).
#[cfg(windows)]
pub struct MappedImage {
    base: usize,
    size: usize,
    sections: Vec<Section>,
}

#[cfg(windows)]
impl MappedImage {
    /// The image whose module handle (= base address) is `base`.
    ///
    /// # Safety
    /// `base` must be the base of a PE32+ image that stays mapped for `'static`.
    #[must_use]
    pub unsafe fn from_base(base: usize) -> Option<Self> {
        // SAFETY: the caller guarantees mapped PE headers at `base`.
        let header = unsafe { std::slice::from_raw_parts(base as *const u8, 0x1000) };
        let parsed = crate::pefile::Headers::parse(header).ok()?;
        let sections = parsed
            .sections
            .into_iter()
            .map(|mut s| {
                s.readable_size = s.virtual_size;
                s
            })
            .collect();
        Some(MappedImage { base, size: parsed.size_of_image as usize, sections })
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }
}

#[cfg(windows)]
impl Image for MappedImage {
    fn base(&self) -> u64 {
        self.base as u64
    }

    fn sections(&self) -> &[Section] {
        &self.sections
    }

    fn bytes(&self, rva: u32, len: usize) -> Option<&[u8]> {
        let section = self.section_at(rva)?;
        if (rva - section.rva) as usize + len > section.virtual_size as usize {
            return None;
        }
        // SAFETY: the whole section is mapped for the life of the process
        // (`from_base` contract); section pages are at least readable.
        Some(unsafe { std::slice::from_raw_parts((self.base + rva as usize) as *const u8, len) })
    }
}

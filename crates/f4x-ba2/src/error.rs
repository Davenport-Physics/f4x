use std::fmt;

/// Everything that can go wrong while reading an archive.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The first four bytes are not `BTDX`.
    BadMagic([u8; 4]),
    /// A version other than 1, 7 or 8 (Starfield's 2/3 use a longer header).
    UnsupportedVersion(u32),
    /// A type tag other than `GNRL` or `DX10` (e.g. console `GNMF`).
    UnsupportedKind([u8; 4]),
    /// A DX10 record declares a chunk record size other than 24.
    UnsupportedChunkHeader {
        offset: u64,
        size: u16,
    },
    /// A structure runs past the end of the archive.
    Truncated {
        what: &'static str,
        offset: u64,
    },
    /// Extracting DX10 entries (which needs a rebuilt DDS header) is not supported.
    TextureExtraction(String),
    Decompress {
        name: String,
        source: std::io::Error,
    },
    /// Decompressed length differs from the record's unpacked size.
    SizeMismatch {
        name: String,
        expected: u64,
        actual: u64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::BadMagic(m) => write!(f, "not a BA2 archive (magic {m:02x?})"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported BA2 version {v} (expected 1, 7 or 8)"),
            Error::UnsupportedKind(k) => {
                write!(f, "unsupported BA2 type {:?}", String::from_utf8_lossy(k))
            }
            Error::UnsupportedChunkHeader { offset, size } => {
                write!(f, "texture record at {offset:#x}: chunk record size {size}, expected 24")
            }
            Error::Truncated { what, offset } => {
                write!(f, "{what} at {offset:#x} runs past the end of the archive")
            }
            Error::TextureExtraction(name) => write!(f, "{name}: extracting DX10 textures is not supported"),
            Error::Decompress { name, source } => write!(f, "{name}: zlib error: {source}"),
            Error::SizeMismatch { name, expected, actual } => {
                write!(f, "{name}: decompressed to {actual} bytes, record says {expected}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) | Error::Decompress { source: e, .. } => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

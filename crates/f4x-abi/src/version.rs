//! Packed `major.minor.build.sub` versions.
//!
//! Layout (CommonLibF4 `REL::Version::pack` and `QueryInterface::MakeVersion`):
//! `major:8 | minor:8 | build:12 | sub:4`, most significant first.

use core::fmt;

/// A packed version as it appears in the ABI (`runtimeVersion`, `f4seVersion`, ...).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct PackedVersion(pub u32);

impl PackedVersion {
    /// Pack a version. Components are masked to their field widths.
    #[must_use]
    pub const fn new(major: u32, minor: u32, build: u32, sub: u32) -> Self {
        PackedVersion((major & 0xFF) << 24 | (minor & 0xFF) << 16 | (build & 0xFFF) << 4 | (sub & 0xF))
    }

    #[must_use]
    pub const fn major(self) -> u32 {
        self.0 >> 24 & 0xFF
    }

    #[must_use]
    pub const fn minor(self) -> u32 {
        self.0 >> 16 & 0xFF
    }

    #[must_use]
    pub const fn build(self) -> u32 {
        self.0 >> 4 & 0xFFF
    }

    #[must_use]
    pub const fn sub(self) -> u32 {
        self.0 & 0xF
    }

    /// Same major, minor and build; ignores `sub`.
    #[must_use]
    pub const fn same_build(self, other: PackedVersion) -> bool {
        self.0 >> 4 == other.0 >> 4
    }
}

impl fmt::Display for PackedVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.major(), self.minor(), self.build(), self.sub())
    }
}

impl fmt::Debug for PackedVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PackedVersion({self} = {:#010x})", self.0)
    }
}

/// Game builds CommonLibF4 names (`F4SE/Version.h`: master, community, next-gen fork),
/// plus 1.11.240 (our reading of the Steam exe's version resource).
pub mod runtime {
    use super::PackedVersion;

    /// The last old-gen build.
    pub const V1_10_163: PackedVersion = PackedVersion::new(1, 10, 163, 0);
    /// The first next-gen build.
    pub const V1_10_980: PackedVersion = PackedVersion::new(1, 10, 980, 0);
    pub const V1_10_984: PackedVersion = PackedVersion::new(1, 10, 984, 0);
    /// The first build of the Address Library / layout generation the next-gen fork's
    /// version-data bit 2 names.
    pub const V1_11_137: PackedVersion = PackedVersion::new(1, 11, 137, 0);
    /// The fork's `RUNTIME_LATEST`.
    pub const V1_11_221: PackedVersion = PackedVersion::new(1, 11, 221, 0);
    /// The Steam next-gen build f4x's next-gen tables are for.
    pub const V1_11_240: PackedVersion = PackedVersion::new(1, 11, 240, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_like_clf4() {
        let v = PackedVersion::new(1, 10, 163, 0);
        assert_eq!(v.0, 0x010A_0A30);
        assert_eq!((v.major(), v.minor(), v.build(), v.sub()), (1, 10, 163, 0));
        assert!(v.same_build(PackedVersion::new(1, 10, 163, 7)));
        assert!(!v.same_build(runtime::V1_10_980));
        assert!(v < runtime::V1_10_980);
        assert_eq!(runtime::V1_11_240.0, 0x010B_0F00);
        assert_eq!(PackedVersion::new(0, 7, 9, 0).0, 0x0007_0090);
    }

    #[test]
    fn masks_out_of_range_components() {
        let v = PackedVersion::new(0x1FF, 0, 0x1FFF, 0x1F);
        assert_eq!((v.major(), v.build(), v.sub()), (0xFF, 0xFFF, 0xF));
    }
}

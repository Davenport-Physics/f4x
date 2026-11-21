//! Raw memory access helpers.

use std::ffi::CStr;

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
};

/// # Safety
/// `base + offset` must point to a readable `T`.
#[inline]
pub unsafe fn read<T: Copy>(base: *const u8, offset: usize) -> T {
    // SAFETY: guaranteed by the caller.
    unsafe { base.add(offset).cast::<T>().read_unaligned() }
}

/// Whether `len` bytes at `p` are committed and readable (one `VirtualQuery`).
pub fn readable(p: usize, len: usize) -> bool {
    if p < 0x10000 {
        return false;
    }
    // SAFETY: VirtualQuery only inspects the address space.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let got = unsafe { VirtualQuery(p as *const _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    got != 0
        && info.State == MEM_COMMIT
        && readable_protection(info.Protect)
        && p.checked_add(len).is_some_and(|end| end <= info.BaseAddress as usize + info.RegionSize)
}

fn readable_protection(protect: u32) -> bool {
    const READABLE: u32 = 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80; // R, RW, WC, XR, XRW, XWC
    protect & (PAGE_NOACCESS | PAGE_GUARD) == 0 && protect & READABLE != 0
}

/// A snapshot of the process's readable regions, for checking many pointers quickly.
pub struct Regions(Vec<(usize, usize)>);

impl Regions {
    pub fn snapshot() -> Regions {
        let mut regions = Vec::new();
        let mut address = 0x10000usize;
        while address < 0x7FFF_FFFF_0000 {
            // SAFETY: as above.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            let got = unsafe { VirtualQuery(address as *const _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
            if got == 0 {
                break;
            }
            let start = info.BaseAddress as usize;
            let end = start + info.RegionSize;
            if info.State == MEM_COMMIT && readable_protection(info.Protect) {
                regions.push((start, end));
            }
            if end <= address {
                break;
            }
            address = end;
        }
        Regions(regions)
    }

    pub fn contains(&self, p: usize, len: usize) -> bool {
        let Some(end) = p.checked_add(len) else { return false };
        let i = self.0.partition_point(|&(start, _)| start <= p);
        i > 0 && end <= self.0[i - 1].1
    }
}

/// Read a `BSFixedString`/`BGSLocalizedString` (a pointer to a string-pool entry).
/// Layout (CommonLibF4 `BSStringPool::Entry`): flags `u16` at +8 (`1<<14` shallow,
/// `1<<15` wide), next entry at +0x10 when shallow, text after the 0x18-byte header.
///
/// # Safety
/// `entry` must be null or a live string-pool entry.
pub unsafe fn fixed_string(mut entry: *const u8) -> Option<String> {
    if entry.is_null() {
        return Some(String::new());
    }
    for _ in 0..16 {
        // SAFETY: live entry (caller), chained entries are live.
        let flags: u16 = unsafe { read(entry, 8) };
        if flags & (1 << 14) == 0 {
            if flags & (1 << 15) != 0 {
                return None;
            }
            // SAFETY: as above.
            return Some(unsafe { CStr::from_ptr(entry.add(0x18).cast()) }.to_string_lossy().into_owned());
        }
        // SAFETY: as above.
        entry = unsafe { read::<*const u8>(entry, 0x10) };
        if entry.is_null() {
            return None;
        }
    }
    None
}

/// A C string field.
///
/// # Safety
/// `p` must point to a NUL-terminated string (or be null).
pub unsafe fn c_str(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(p.cast()) }.to_string_lossy().into_owned()
}

/// A `BSTArray<T>` view (CommonLibF4 `BSTArray`: data `*T` at 0, capacity `u32` at 8,
/// size `u32` at 0x10).
#[derive(Clone, Copy)]
pub struct BSTArray<T: Copy> {
    data: *const T,
    len: usize,
}

impl<T: Copy> BSTArray<T> {
    /// # Safety
    /// `array` must point to a live `BSTArray<T>`.
    pub unsafe fn at(array: *const u8) -> Self {
        // SAFETY: guaranteed by the caller.
        unsafe { BSTArray { data: read(array, 0), len: read::<u32>(array, 0x10) as usize } }
    }

    pub fn len(&self) -> usize {
        if self.data.is_null() { 0 } else { self.len }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<T> {
        // SAFETY: bounds checked; the array is live for the caller's use.
        (i < self.len()).then(|| unsafe { self.data.add(i).read_unaligned() })
    }

    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }
}


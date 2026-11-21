//! Trampoline pools: executable memory near an image, for rel32 jumps/calls.
//! The branch pool sits within ±2 GB of the game exe; a local pool near the requesting
//! plugin's own module.

use std::ffi::c_void;
use std::sync::Mutex;

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_FREE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualQuery,
};

const CHUNK: usize = 1 << 20;
const GRANULARITY: usize = 1 << 16;
const REACH: usize = 0x7FF0_0000;

pub struct Pool {
    near: usize,
    chunks: Vec<(usize, usize, usize)>, // (base, size, used)
}

impl Pool {
    pub const fn new() -> Pool {
        Pool { near: 0, chunks: Vec::new() }
    }

    pub fn allocate(&mut self, near: usize, size: usize) -> *mut c_void {
        let size = size.max(1).next_multiple_of(16);
        if self.near == 0 {
            self.near = near;
        }
        for chunk in &mut self.chunks {
            if chunk.1 - chunk.2 >= size {
                let p = chunk.0 + chunk.2;
                chunk.2 += size;
                return p as *mut c_void;
            }
        }
        let chunk_size = size.next_multiple_of(CHUNK);
        let Some(base) = reserve_near(self.near, chunk_size) else { return std::ptr::null_mut() };
        self.chunks.push((base, chunk_size, size));
        base as *mut c_void
    }
}

/// Reserve + commit `size` RWX bytes within ±2 GB of `near`.
fn reserve_near(near: usize, size: usize) -> Option<usize> {
    let low = near.saturating_sub(REACH).next_multiple_of(GRANULARITY);
    let high = near.saturating_add(REACH);
    // Search downward from just below `near`, then upward.
    let mut candidate = (near & !(GRANULARITY - 1)).saturating_sub(GRANULARITY);
    while candidate >= low && candidate > 0x10000 {
        if let Some(p) = try_at(candidate, size) {
            return Some(p);
        }
        candidate -= GRANULARITY;
    }
    let mut candidate = (near + GRANULARITY) & !(GRANULARITY - 1);
    while candidate + size < high {
        if let Some(p) = try_at(candidate, size) {
            return Some(p);
        }
        candidate += GRANULARITY;
    }
    None
}

fn try_at(address: usize, size: usize) -> Option<usize> {
    // SAFETY: VirtualQuery inspects; VirtualAlloc only claims free address space.
    unsafe {
        let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        if VirtualQuery(address as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 {
            return None;
        }
        if info.State != MEM_FREE || info.BaseAddress as usize + info.RegionSize < address + size {
            return None;
        }
        let p = VirtualAlloc(address as *const c_void, size, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE);
        (!p.is_null()).then_some(p as usize)
    }
}

pub static BRANCH: Mutex<Pool> = Mutex::new(Pool::new());
pub static LOCAL: Mutex<Vec<(usize, Pool)>> = Mutex::new(Vec::new());

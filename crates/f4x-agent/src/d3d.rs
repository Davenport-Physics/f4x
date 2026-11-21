//! Frame hook and screenshots.
//!
//! `Fallout4.exe` creates its device and swap chain with `d3d11!D3D11CreateDeviceAndSwapChain`
//! (one import). Hooking that import gives us the game's own swap chain; replacing
//! slot 8 of its vtable (`IDXGISwapChain::Present`, per the public d3d/dxgi headers)
//! counts frames and lets us read the back buffer right before it is shown.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use f4x_proto::{RpcError, Screenshot, ScreenshotParams, code};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_FORMAT_R10G10B10A2_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::Interface;

use crate::{clock, hooks};

const PRESENT_SLOT: usize = 8;

static REAL_CREATE: AtomicUsize = AtomicUsize::new(0);
static REAL_PRESENT: AtomicUsize = AtomicUsize::new(0);

type CreateDeviceAndSwapChain = unsafe extern "system" fn(
    *mut c_void,
    i32,
    *mut c_void,
    u32,
    *const i32,
    u32,
    u32,
    *const c_void,
    *mut *mut c_void,
    *mut *mut c_void,
    *mut i32,
    *mut *mut c_void,
) -> i32;
type Present = unsafe extern "system" fn(*mut c_void, u32, u32) -> i32;

struct Request {
    name: String,
    reply: Sender<Result<Screenshot, String>>,
}

static REQUESTS: Mutex<Vec<Request>> = Mutex::new(Vec::new());

/// Hook the game's device creation. Must run before the game creates its device.
pub fn install(base: *mut u8) {
    // SAFETY: `base` is the mapped game image.
    match unsafe { hooks::iat_hook(base, "d3d11.dll", "D3D11CreateDeviceAndSwapChain", create_hook as CreateDeviceAndSwapChain as usize) } {
        Ok(real) => {
            REAL_CREATE.store(real, Relaxed);
            log::debug!(target: "f4x::frames", "hooked D3D11CreateDeviceAndSwapChain");
        }
        Err(error) => log::warn!(target: "f4x::frames", "no frame hook: {error}"),
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn create_hook(
    adapter: *mut c_void,
    driver: i32,
    software: *mut c_void,
    flags: u32,
    levels: *const i32,
    level_count: u32,
    sdk: u32,
    desc: *const c_void,
    swapchain: *mut *mut c_void,
    device: *mut *mut c_void,
    level: *mut i32,
    context: *mut *mut c_void,
) -> i32 {
    // SAFETY: forwarding the game's own call to the real function.
    let hr = unsafe {
        let real: CreateDeviceAndSwapChain = std::mem::transmute(REAL_CREATE.load(Relaxed));
        real(adapter, driver, software, flags, levels, level_count, sdk, desc, swapchain, device, level, context)
    };
    if hr >= 0 && !swapchain.is_null() {
        // SAFETY: on success *swapchain is a live IDXGISwapChain; its first field is the vtable.
        unsafe {
            let chain = *swapchain;
            if !chain.is_null() && REAL_PRESENT.load(Relaxed) == 0 {
                let vtable = *(chain as *mut *mut usize);
                match hooks::patch_pointer(vtable.add(PRESENT_SLOT), present_hook as Present as usize) {
                    Ok(real) => {
                        REAL_PRESENT.store(real, Relaxed);
                        log::info!(target: "f4x::frames", "hooked IDXGISwapChain::Present");
                    }
                    Err(error) => log::warn!(target: "f4x::frames", "Present hook failed: {error}"),
                }
            }
        }
    }
    hr
}

unsafe extern "system" fn present_hook(this: *mut c_void, sync: u32, flags: u32) -> i32 {
    let frame = clock::FRAMES.fetch_add(1, Relaxed) + 1;
    if frame == 1 {
        log::info!(target: "f4x::frames", "first frame presented");
    }
    let pending: Vec<Request> = REQUESTS.lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default();
    for request in pending {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: `this` is the live swap chain being presented.
            unsafe { capture(this, frame) }
        }))
        .unwrap_or_else(|_| Err("capture panicked".into()));
        match result {
            Ok((width, height, rgba)) => {
                let name = request.name;
                // Encode off the render thread.
                std::thread::spawn(move || {
                    let reply = encode_png(&name, width, height, &rgba).map(|()| Screenshot { width, height, file: name, frame });
                    let _ = request.reply.send(reply);
                });
            }
            Err(error) => {
                let _ = request.reply.send(Err(error));
            }
        }
    }
    // SAFETY: calling the real Present with the game's arguments.
    unsafe {
        let real: Present = std::mem::transmute(REAL_PRESENT.load(Relaxed));
        real(this, sync, flags)
    }
}

/// Copy the current back buffer to CPU memory as tightly packed RGBA8.
///
/// # Safety
/// `swapchain` must be a live `IDXGISwapChain`.
unsafe fn capture(swapchain: *mut c_void, _frame: u64) -> Result<(u32, u32, Vec<u8>), String> {
    // SAFETY: borrowed COM pointer, valid for this call.
    unsafe {
        let chain = IDXGISwapChain::from_raw_borrowed(&swapchain).ok_or("null swap chain")?;
        let back: ID3D11Texture2D = chain.GetBuffer(0).map_err(|e| format!("GetBuffer: {e}"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        back.GetDesc(&mut desc);
        if desc.SampleDesc.Count > 1 {
            return Err("multisampled back buffer is not supported yet".into());
        }
        let device: ID3D11Device = back.GetDevice().map_err(|e| format!("GetDevice: {e}"))?;
        let context = device.GetImmediateContext().map_err(|e| format!("GetImmediateContext: {e}"))?;
        let mut staging_desc = desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = 0;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        staging_desc.MiscFlags = 0;
        let mut staging: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).map_err(|e| format!("CreateTexture2D: {e}"))?;
        let staging = staging.ok_or("CreateTexture2D returned null")?;
        context.CopyResource(&staging, &back);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("Map: {e}"))?;
        let (width, height) = (desc.Width, desc.Height);
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        let format = desc.Format;
        for y in 0..height as usize {
            let row = std::slice::from_raw_parts(
                mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                width as usize * 4,
            );
            convert_row(format, row, &mut rgba[y * width as usize * 4..(y + 1) * width as usize * 4]);
        }
        context.Unmap(&staging, 0);
        Ok((width, height, rgba))
    }
}

fn convert_row(format: DXGI_FORMAT, src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        match format {
            f if f == DXGI_FORMAT_B8G8R8A8_UNORM || f == DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
            f if f == DXGI_FORMAT_R10G10B10A2_UNORM => {
                let v = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                let c = |shift: u32| (((v >> shift) & 0x3FF) >> 2) as u8;
                d.copy_from_slice(&[c(0), c(10), c(20), 255]);
            }
            f if f == DXGI_FORMAT_R8G8B8A8_UNORM || f == DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => {
                d.copy_from_slice(&[s[0], s[1], s[2], 255]);
            }
            _ => d.copy_from_slice(&[s[0], s[1], s[2], 255]),
        }
    }
}

fn encode_png(name: &str, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let session = crate::session::get().ok_or("no session directory")?;
    let mut bytes = Vec::with_capacity(rgba.len() / 2);
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    session.write_atomic(name, &bytes).map_err(|e| e.to_string())
}

/// Capture the next presented frame into the session directory.
pub fn screenshot(params: &ScreenshotParams, timeout: Duration) -> Result<Screenshot, RpcError> {
    if REAL_PRESENT.load(Relaxed) == 0 {
        return Err(RpcError::new(code::NOT_READY, "no frame hook (the swap chain has not been created yet)"));
    }
    let session = crate::session::get().ok_or_else(|| RpcError::new(code::INTERNAL, "no session directory"))?;
    let name = params.name.clone().unwrap_or_else(|| session.numbered(f4x_proto::files::SCREENSHOT_PREFIX, "png"));
    let (reply, rx) = mpsc::channel();
    REQUESTS.lock().map_err(|_| RpcError::new(code::INTERNAL, "poisoned"))?.push(Request { name, reply });
    rx.recv_timeout(timeout)
        .map_err(|_| RpcError::new(code::TIMEOUT, "no frame was presented in time"))?
        .map_err(|e| RpcError::new(code::ENGINE, e))
}

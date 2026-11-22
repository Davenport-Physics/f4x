//! `f4x_loader.exe`: start the game suspended, load the f4x runtime into it, resume it.
//!
//! The technique is the standard one: `CreateProcessW(CREATE_SUSPENDED)`, write the DLL
//! path into the child, and run `LoadLibraryW` on a remote thread. The first thread to
//! run in a fresh process performs loader initialisation, so the exe's static imports
//! are bound before the runtime's `DllMain` runs. CommonLibF4's `F4SEStub/loader` (MIT)
//! uses the same approach.
//!
//! The loader waits for the game to exit and returns its exit code, so a wrapper like
//! Proton/umu keeps its container alive for the whole session.

#[cfg(not(windows))]
fn main() {
    eprintln!("f4x_loader only runs on Windows (or under Wine/Proton)");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(match windows::run() {
        Ok(code) => code as i32,
        Err(message) => {
            log(&format!("error: {message}"));
            1
        }
    });
}

/// Print to stderr and, when the host gave us a session directory, append to
/// `loader.log` there (Proton does not always forward a child's stderr).
#[cfg(windows)]
fn log(message: &str) {
    use std::io::Write;
    eprintln!("f4x_loader: {message}");
    if let Some(dir) = std::env::var_os("F4X_SESSION_DIR") {
        let path = std::path::Path::new(&dir).join("loader.log");
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{message}");
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::{mem, ptr};

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_WRITE, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MapViewOfFile, PAGE_READONLY,
        PAGE_READWRITE, UnmapViewOfFile, VirtualAllocEx, VirtualFreeEx,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CreateEventW, CreateProcessW, CreateRemoteThread, GetExitCodeProcess, GetExitCodeThread, INFINITE,
        PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    };

    struct Options {
        game_dir: PathBuf,
        exe: String,
        dll: PathBuf,
        game_args: Vec<String>,
    }

    const USAGE: &str = "usage: f4x_loader [--game-dir DIR] [--exe Fallout4.exe] [--dll PATH] [-- game args...]";

    fn parse_options() -> Result<Options, String> {
        let mut args = std::env::args().skip(1);
        // The loader's own folder when it sits next to the game (normal play: launchers
        // may start it from anywhere), else the working directory.
        let mut game_dir = std::env::current_exe()
            .ok()
            .and_then(|me| me.parent().map(Path::to_path_buf))
            .filter(|dir| dir.join("Fallout4.exe").is_file())
            .map_or_else(|| std::env::current_dir().map_err(|e| e.to_string()), Ok)?;
        let mut exe = String::from("Fallout4.exe");
        let mut dll = None;
        let mut game_args = Vec::new();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value\n{USAGE}"));
            match arg.as_str() {
                "--game-dir" => game_dir = PathBuf::from(value("--game-dir")?),
                "--exe" => exe = value("--exe")?,
                "--dll" => dll = Some(PathBuf::from(value("--dll")?)),
                "--" => {
                    game_args.extend(args.by_ref());
                }
                "-h" | "--help" => return Err(USAGE.into()),
                other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
            }
        }
        let dll = match dll {
            Some(dll) => dll,
            None => {
                let me = std::env::current_exe().map_err(|e| e.to_string())?;
                me.with_file_name("f4x_runtime.dll")
            }
        };
        Ok(Options { game_dir, exe, dll, game_args })
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    fn quote(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            return arg.to_owned();
        }
        format!("\"{}\"", arg.replace('"', "\\\""))
    }

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own this handle.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub fn run() -> Result<u32, String> {
        crate::log(&format!("f4x_loader {} args {:?}", env!("CARGO_PKG_VERSION"), std::env::args().collect::<Vec<_>>()));
        let options = parse_options()?;
        let exe_path = options.game_dir.join(&options.exe);
        if !exe_path.is_file() {
            return Err(format!("{} not found", exe_path.display()));
        }
        let dll = std::path::absolute(&options.dll).map_err(|e| e.to_string())?;
        if !dll.is_file() {
            return Err(format!("runtime DLL {} not found", dll.display()));
        }

        let mut command_line = quote(&exe_path.to_string_lossy());
        for arg in &options.game_args {
            command_line.push(' ');
            command_line.push_str(&quote(arg));
        }
        // The runtime loads F4SE plugins on its own thread before the game runs; it signals
        // this event when done, and we only resume the game's main thread after that.
        let event_name = format!("Local\\f4x-init-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        // SAFETY: setting our own environment before creating the child (single-threaded).
        unsafe { std::env::set_var("F4X_INIT_EVENT", &event_name) };
        let event_wide = wide(OsStr::new(&event_name));
        // SAFETY: creating a named, manual-reset, initially unsignalled event.
        let init_event = Handle(unsafe { CreateEventW(ptr::null(), 1, 0, event_wide.as_ptr()) });
        let mut command_line = wide(OsStr::new(&command_line));
        let exe_wide = wide(exe_path.as_os_str());
        let dir_wide = wide(options.game_dir.as_os_str());

        // SAFETY: plain Win32 calls with valid, NUL-terminated buffers.
        unsafe {
            let mut startup: STARTUPINFOW = mem::zeroed();
            startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = mem::zeroed();
            if CreateProcessW(
                exe_wide.as_ptr(),
                command_line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_SUSPENDED,
                ptr::null(),
                dir_wide.as_ptr(),
                &startup,
                &mut info,
            ) == 0
            {
                return Err(format!("CreateProcessW({}) failed: error {}", exe_path.display(), GetLastError()));
            }
            let process = Handle(info.hProcess);
            let thread = Handle(info.hThread);
            crate::log(&format!("started {} (pid {}) suspended", exe_path.display(), info.dwProcessId));

            if let Err(message) = inject(process.0, &dll) {
                TerminateProcess(process.0, 1);
                return Err(message);
            }
            crate::log(&format!("injected {}", dll.display()));
            if !init_event.0.is_null() {
                let waited = WaitForSingleObject(init_event.0, 180_000);
                crate::log(if waited == WAIT_OBJECT_0 {
                    "runtime finished loading plugins"
                } else {
                    "timed out waiting for the runtime's init; resuming anyway"
                });
            }

            if ResumeThread(thread.0) == u32::MAX {
                return Err(format!("ResumeThread failed: error {}", GetLastError()));
            }
            drop(thread);

            WaitForSingleObject(process.0, INFINITE);
            let mut code = 0;
            GetExitCodeProcess(process.0, &mut code);
            crate::log(&format!("game exited with code {code:#x}"));
            Ok(code)
        }
    }

    /// Load `dll` into `process` by running `LoadLibraryW` on a remote thread.
    ///
    /// The DLL path reaches the child through a shared section mapped into it with
    /// `NtMapViewOfSection`, not `WriteProcessMemory`. Under Proton the prefix's
    /// wineserver may belong to another pressure-vessel container (e.g. Vortex is open),
    /// whose user namespace cannot write our child's memory directly. Mapping and
    /// remote-thread creation are carried out by the child itself, so they still work.
    ///
    /// # Safety
    /// `process` must be a process handle with VM operation and thread-creation access.
    unsafe fn inject(process: HANDLE, dll: &Path) -> Result<(), String> {
        let path = wide(dll.as_os_str());
        let bytes = path.len() * mem::size_of::<u16>();
        // SAFETY: the caller guarantees `process` is valid; buffers are sized correctly.
        unsafe {
            let remote = match map_into(process, &path, bytes) {
                Ok(remote) => remote,
                Err(error) => {
                    crate::log(&format!("shared-section path transfer failed ({error}); trying WriteProcessMemory"));
                    write_into(process, &path, bytes)?
                }
            };

            // kernel32 is mapped at the same address in every process of a session (on
            // Windows and under Wine), so our LoadLibraryW is valid in the child too.
            let kernel32 = GetModuleHandleW(wide(OsStr::new("kernel32.dll")).as_ptr());
            let Some(load_library) = GetProcAddress(kernel32, c"LoadLibraryW".as_ptr().cast()) else {
                return Err("LoadLibraryW not found".into());
            };
            let start: unsafe extern "system" fn(*mut c_void) -> u32 = mem::transmute(load_library);

            let thread = CreateRemoteThread(process, ptr::null(), 0, Some(start), remote, 0, ptr::null_mut());
            if thread.is_null() {
                return Err(format!("CreateRemoteThread failed: error {}", GetLastError()));
            }
            let thread = Handle(thread);
            let waited = WaitForSingleObject(thread.0, 60_000);
            let mut module_low = 0u32;
            GetExitCodeThread(thread.0, &mut module_low);
            if waited != WAIT_OBJECT_0 {
                return Err("timed out waiting for LoadLibraryW in the game process".into());
            }
            if module_low == 0 {
                return Err(format!("LoadLibraryW({}) failed inside the game process", dll.display()));
            }
            Ok(())
        }
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtMapViewOfSection(
            section: HANDLE,
            process: HANDLE,
            base: *mut *mut c_void,
            zero_bits: usize,
            commit_size: usize,
            offset: *mut i64,
            view_size: *mut usize,
            inherit: u32,
            allocation_type: u32,
            protect: u32,
        ) -> i32;
    }

    const VIEW_UNMAP: u32 = 2;

    /// Copy `data` into a pagefile-backed section and map a read-only view of it into
    /// `process`. Returns the child-side address.
    ///
    /// # Safety
    /// `process` must be a valid process handle; `data` must hold `bytes` bytes.
    unsafe fn map_into(process: HANDLE, data: &[u16], bytes: usize) -> Result<*mut c_void, String> {
        // SAFETY: standard section APIs on handles we own; sizes match `data`.
        unsafe {
            let section = CreateFileMappingW(INVALID_HANDLE_VALUE, ptr::null(), PAGE_READWRITE, 0, bytes as u32, ptr::null());
            if section.is_null() {
                return Err(format!("CreateFileMappingW: error {}", GetLastError()));
            }
            let section = Handle(section);
            let local = MapViewOfFile(section.0, FILE_MAP_WRITE, 0, 0, bytes);
            if local.Value.is_null() {
                return Err(format!("MapViewOfFile: error {}", GetLastError()));
            }
            ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), local.Value.cast::<u8>(), bytes);
            UnmapViewOfFile(local);

            let mut remote = ptr::null_mut();
            let mut size = 0usize;
            let status = NtMapViewOfSection(
                section.0,
                process,
                &mut remote,
                0,
                0,
                ptr::null_mut(),
                &mut size,
                VIEW_UNMAP,
                0,
                PAGE_READONLY,
            );
            if status < 0 || remote.is_null() {
                return Err(format!("NtMapViewOfSection: status {status:#x}"));
            }
            Ok(remote)
        }
    }

    /// Fallback: allocate in the child and write the data directly.
    ///
    /// # Safety
    /// `process` must be a valid process handle; `data` must hold `bytes` bytes.
    unsafe fn write_into(process: HANDLE, data: &[u16], bytes: usize) -> Result<*mut c_void, String> {
        // SAFETY: as documented.
        unsafe {
            let remote = VirtualAllocEx(process, ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if remote.is_null() {
                return Err(format!("VirtualAllocEx failed: error {}", GetLastError()));
            }
            if WriteProcessMemory(process, remote, data.as_ptr().cast(), bytes, ptr::null_mut()) == 0 {
                let error = GetLastError();
                VirtualFreeEx(process, remote, 0, MEM_RELEASE);
                return Err(format!("WriteProcessMemory failed: error {error}"));
            }
            Ok(remote)
        }
    }
}

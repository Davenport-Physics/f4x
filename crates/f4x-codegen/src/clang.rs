//! Finding clang and the MSVC CRT/SDK headers, building the command line, and
//! running one clang invocation ("pass").

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::time::Instant;

use serde::Serialize;

pub const TARGET: &str = "x86_64-pc-windows-msvc";

/// `_MSC_VER` to report. The xwin STL is 14.44 (`_MSVC_STL_UPDATE` 202503, VS 17.14),
/// so claim the matching compiler instead of clang's default of 19.33.
pub const MSVC_VERSION: &str = "19.44";

/// Preprocessor definitions, each with the reason it is needed.
pub const DEFINES: &[(&str, &str)] = &[
    (
        "SPDLOG_COMPILED_LIB",
        "required: without it spdlog's header-only mode includes <windows.h>, whose \
         MessageBox/GetModuleFileName/... macros break F4SE::WinAPI (7 errors)",
    ),
    (
        "SPDLOG_FMT_EXTERNAL",
        "fidelity: spdlog uses the external fmt like vcpkg's port, so only one fmt copy is parsed",
    ),
    (
        "BOOST_STL_INTERFACES_DISABLE_CONCEPTS",
        "fidelity: CommonLibF4's CMake sets it PUBLIC; selects the v1 iterator_interface it was written against",
    ),
];

#[derive(Debug, Clone, Serialize)]
pub struct Toolchain {
    pub clang: PathBuf,
    pub version: String,
    pub major: u32,
}

/// `explicit`, else `$F4X_CLANG`, else the first `clang++` on `PATH` that is not a
/// ccache wrapper.
pub fn find_clang(explicit: Option<&Path>) -> Result<Toolchain, String> {
    let clang = match explicit.map(Path::to_path_buf).or_else(|| env::var_os("F4X_CLANG").map(PathBuf::from))
    {
        Some(path) => path,
        None => {
            let path = env::var_os("PATH").unwrap_or_default();
            env::split_paths(&path)
                .map(|dir| dir.join("clang++"))
                .find(|candidate| {
                    candidate.is_file()
                        && fs::canonicalize(candidate).is_ok_and(|real| {
                            !real.file_name().is_some_and(|n| n.to_string_lossy().contains("ccache"))
                        })
                })
                .ok_or("no clang++ on PATH (pass --clang or set F4X_CLANG)")?
        }
    };
    let output = Command::new(&clang)
        .arg("--version")
        .output()
        .map_err(|e| format!("running {} --version: {e}", clang.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.lines().next().unwrap_or("").trim().to_string();
    let major = version
        .split("version ")
        .nth(1)
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("cannot read the version of {}: `{version}`", clang.display()))?;
    if major < 19 {
        return Err(format!("{version}: the MSVC STL needs clang 19 or newer"));
    }
    Ok(Toolchain { clang, version, major })
}

#[derive(Debug, Clone, Serialize)]
pub struct Xwin {
    pub root: PathBuf,
    pub msvc_stl_update: Option<String>,
    pub sdk_versions: Vec<String>,
}

impl Xwin {
    pub fn include_dirs(&self) -> [PathBuf; 4] {
        [
            self.root.join("crt/include"),
            self.root.join("sdk/include/ucrt"),
            self.root.join("sdk/include/um"),
            self.root.join("sdk/include/shared"),
        ]
    }
}

/// `explicit`, else `$XWIN_CACHE_DIR/xwin`, else `~/.cache/cargo-xwin/xwin`.
pub fn find_xwin(explicit: Option<&Path>) -> Result<Xwin, String> {
    let candidates: Vec<PathBuf> = match explicit {
        Some(p) => vec![p.to_path_buf()],
        None => env::var_os("XWIN_CACHE_DIR")
            .map(|d| PathBuf::from(d).join("xwin"))
            .into_iter()
            .chain(env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/cargo-xwin/xwin")))
            .collect(),
    };
    let root = candidates
        .iter()
        .find(|r| {
            r.join("crt/include/yvals_core.h").is_file() && r.join("sdk/include/ucrt/corecrt.h").is_file()
        })
        .ok_or_else(|| {
            format!(
                "no xwin CRT/SDK at {:?} (run `cargo win` once, or pass --xwin <dir with crt/ and sdk/>)",
                candidates
            )
        })?
        .clone();
    let msvc_stl_update = fs::read_to_string(root.join("crt/include/yvals_core.h")).ok().and_then(|text| {
        text.lines().find_map(|l| l.strip_prefix("#define _MSVC_STL_UPDATE").map(|v| v.trim().to_string()))
    });
    let mut sdk_versions: Vec<String> = fs::read_dir(root.join("sdk/include"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("10."))
                .collect()
        })
        .unwrap_or_default();
    sdk_versions.sort();
    Ok(Xwin { root, msvc_stl_update, sdk_versions })
}

/// Everything that is the same for all passes.
pub struct Config {
    pub toolchain: Toolchain,
    pub xwin: Xwin,
    pub third_party: Vec<PathBuf>,
    pub clf4_include: PathBuf,
    /// MSVC-style delayed parsing of template bodies (see `docs/spikes/S2-clang.md`).
    pub delayed_templates: bool,
}

impl Config {
    pub fn common_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "--no-default-config",
            &format!("--target={TARGET}"),
            "-fms-compatibility",
            "-fms-extensions",
            &format!("-fms-compatibility-version={MSVC_VERSION}"),
            "-std=c++20",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        if self.delayed_templates {
            args.push("-fdelayed-template-parsing".into());
            args.push("-Wno-delayed-template-parsing-in-cxx20".into());
        }
        args.push("-ferror-limit=0".into());
        args.push("-fno-color-diagnostics".into());
        for dir in self.xwin.include_dirs().iter().chain(&self.third_party) {
            args.push("-isystem".into());
            args.push(dir.into());
        }
        for (define, _why) in DEFINES {
            args.push(format!("-D{define}").into());
        }
        args.push("-I".into());
        args.push(self.clf4_include.as_os_str().into());
        args
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PassResult {
    pub name: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub errors: usize,
    pub warnings: usize,
    pub seconds: f64,
    pub diagnostics: PathBuf,
    pub stdout_bytes: u64,
}

impl PassResult {
    pub fn ok(&self) -> bool {
        self.exit_code == Some(0) && self.errors == 0
    }
}

/// Runs clang with `common_args + mode + tu`. stderr goes to `diag_path`; stdout is
/// handed to `sink`, which returns the number of bytes it consumed.
pub fn run_pass(
    cfg: &Config,
    name: &str,
    mode: &[&str],
    tu: &Path,
    diag_path: &Path,
    sink: impl FnOnce(ChildStdout) -> Result<u64, String>,
) -> Result<PassResult, String> {
    let mut args = cfg.common_args();
    args.extend(mode.iter().map(OsString::from));
    args.push(tu.as_os_str().into());
    let diag = fs::File::create(diag_path).map_err(|e| format!("creating {}: {e}", diag_path.display()))?;
    let start = Instant::now();
    let mut cmd = Command::new(&cfg.toolchain.clang);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(diag);
    // clang's MSVC driver also reads these; keep the search path to what we pass.
    for var in ["INCLUDE", "EXTERNAL_INCLUDE", "CPATH", "CPLUS_INCLUDE_PATH"] {
        cmd.env_remove(var);
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawning {}: {e}", cfg.toolchain.clang.display()))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let consumed = match sink(stdout) {
        Ok(n) => n,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{name} pass: {e}"));
        }
    };
    let status = child.wait().map_err(|e| format!("waiting for clang: {e}"))?;
    let seconds = start.elapsed().as_secs_f64();
    let text = fs::read_to_string(diag_path).unwrap_or_default();
    let (errors, warnings) = count_diagnostics(&text);
    let argv = std::iter::once(cfg.toolchain.clang.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    Ok(PassResult {
        name: name.to_string(),
        argv,
        exit_code: status.code(),
        errors,
        warnings,
        seconds,
        diagnostics: diag_path.to_path_buf(),
        stdout_bytes: consumed,
    })
}

/// A sink that copies stdout into `path`.
pub fn to_file(path: &Path) -> impl FnOnce(ChildStdout) -> Result<u64, String> + '_ {
    move |mut stdout| {
        let file = fs::File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
        let mut out = io::BufWriter::with_capacity(1 << 20, file);
        let n = io::copy(&mut stdout, &mut out).map_err(|e| format!("writing {}: {e}", path.display()))?;
        io::Write::flush(&mut out).map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok(n)
    }
}

/// Counts `error:` and `warning:` diagnostics. Lines that start with whitespace are
/// source snippets or carets and are skipped.
pub fn count_diagnostics(text: &str) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        if line.contains(": error: ")
            || line.contains(": fatal error: ")
            || line.starts_with("error: ")
            || line.starts_with("fatal error: ")
        {
            errors += 1;
        } else if line.contains(": warning: ") || line.starts_with("warning: ") {
            warnings += 1;
        }
    }
    (errors, warnings)
}

/// `file:line:` prefixes of error diagnostics located in `file`.
pub fn error_lines_in(text: &str, file: &Path) -> Vec<usize> {
    let prefix = format!("{}:", file.display());
    text.lines()
        .filter(|l| l.contains(": error: "))
        .filter_map(|l| l.strip_prefix(&prefix))
        .filter_map(|rest| rest.split(':').next()?.parse().ok())
        .collect()
}

/// Quotes `argv` for a POSIX shell.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_=./:+,@".contains(&b)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_diagnostics() {
        let text = "\
In file included from a.cpp:1:
/x/y z.h:3:4: warning: something [-Wfoo]
    3 | int a: error: not a diagnostic
/x/y z.h:5:1: error: bad thing
fatal error: too many errors
1 warning and 2 errors generated.
";
        assert_eq!(count_diagnostics(text), (2, 1));
    }

    #[test]
    fn finds_error_lines() {
        let text =
            "/t/p.cpp:12:5: error: unknown type name 'X'\n/t/p.cpp:13:1: warning: w\n/o.h:1:1: error: e\n";
        assert_eq!(error_lines_in(text, Path::new("/t/p.cpp")), [12]);
    }

    #[test]
    fn quotes_for_shell() {
        let argv = ["clang++".to_string(), "-I".into(), "/a/Fallout 4/x".into(), "it's".into()];
        assert_eq!(shell_join(&argv), r"clang++ -I '/a/Fallout 4/x' 'it'\''s'");
    }
}

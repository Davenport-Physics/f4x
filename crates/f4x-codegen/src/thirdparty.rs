//! Pinned header-only dependencies of CommonLibF4's public headers.
//!
//! CommonLibF4 gets these from vcpkg (`CommonLibF4/vcpkg.json`: boost-stl-interfaces,
//! fmt, rsm-mmio, spdlog). We fetch the upstream release tarballs into
//! `target/third_party/` (never committed), check each against its SHA-256, and
//! unpack each into its own directory. Only the headers are used, only on the host,
//! and nothing from them ends up in f4x.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Dep {
    pub name: &'static str,
    pub version: &'static str,
    pub license: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Include directory inside the unpacked archive.
    pub include: &'static str,
    /// Why CommonLibF4 needs it.
    pub why: &'static str,
}

/// Versions are the ones vcpkg shipped when CommonLibF4 67173f8d was committed
/// (Nov 2025). Boost.STLInterfaces needs only Boost.Config and Boost.Assert from the
/// rest of Boost in the configuration CommonLibF4 selects.
pub const DEPS: &[Dep] = &[
    Dep {
        name: "fmt",
        version: "12.1.0",
        license: "MIT",
        url: "https://github.com/fmtlib/fmt/archive/refs/tags/12.1.0.tar.gz",
        sha256: "ea7de4299689e12b6dddd392f9896f08fb0777ac7168897a244a6d6085043fea",
        include: "fmt-12.1.0/include",
        why: "F4SE/Impl/PCH.h, REL/Relocation.h, BSScript.h: fmt::format",
    },
    Dep {
        name: "spdlog",
        version: "1.16.0",
        license: "MIT",
        url: "https://github.com/gabime/spdlog/archive/refs/tags/v1.16.0.tar.gz",
        sha256: "8741753e488a78dd0d0024c980e1fb5b5c85888447e309d9cb9d949bdb52aa3e",
        include: "spdlog-1.16.0/include",
        why: "F4SE/Impl/PCH.h, F4SE/Logger.h: spdlog::log",
    },
    Dep {
        name: "rsm-mmio",
        version: "2.0.0",
        license: "MIT",
        url: "https://github.com/Ryan-rsm-McKenzie/mmio/archive/refs/tags/2.0.0.tar.gz",
        sha256: "360dddf74a97bd0a7eb41378cc59f2a69871dabfd36c55bf027429ac54930d5b",
        include: "mmio-2.0.0/include",
        why: "REL/Relocation.h: mmio::mapped_file_source (Address Library reader)",
    },
    Dep {
        name: "boost-stl_interfaces",
        version: "1.89.0",
        license: "BSL-1.0",
        url: "https://github.com/boostorg/stl_interfaces/archive/refs/tags/boost-1.89.0.tar.gz",
        sha256: "515b160567b9c684ad2ea6e9237887cd8481e6a47cf06083c689a449917800d8",
        include: "stl_interfaces-boost-1.89.0/include",
        why: "BSTArray/BSTHashMap/BSTList/... iterators: iterator_interface, sequence_container_interface",
    },
    Dep {
        name: "boost-config",
        version: "1.89.0",
        license: "BSL-1.0",
        url: "https://github.com/boostorg/config/archive/refs/tags/boost-1.89.0.tar.gz",
        sha256: "a16a53f8f0dde2a9ff485fb134a069e54aca738009d825837dac9a35cd4afe0a",
        include: "config-boost-1.89.0/include",
        why: "included by Boost.STLInterfaces",
    },
    Dep {
        name: "boost-assert",
        version: "1.89.0",
        license: "BSL-1.0",
        url: "https://github.com/boostorg/assert/archive/refs/tags/boost-1.89.0.tar.gz",
        sha256: "e25e42fbc5f77dcb203a3479eaf2e9303511f2a5bb8cbdf54e9c562fcbf29da1",
        include: "assert-boost-1.89.0/include",
        why: "included by Boost.STLInterfaces",
    },
];

const STAMP: &str = ".f4x-verified";

/// Makes sure every dependency is unpacked under `root` and returns their include
/// directories, in `DEPS` order. Downloads with `curl` unless `offline`.
pub fn ensure_all(root: &Path, offline: bool) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(root).map_err(|e| format!("creating {}: {e}", root.display()))?;
    DEPS.iter().map(|dep| ensure(root, dep, offline)).collect()
}

fn ensure(root: &Path, dep: &Dep, offline: bool) -> Result<PathBuf, String> {
    // <root>/<name>-<version>/{archive.tar.gz, src/, .f4x-verified}
    let dir = root.join(format!("{}-{}", dep.name, dep.version));
    let src = dir.join("src");
    let include = src.join(dep.include);
    let stamp = dir.join(STAMP);
    if fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == dep.sha256) && include.is_dir() {
        return Ok(include);
    }
    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;

    let archive = dir.join("archive.tar.gz");
    if !archive.is_file() || sha256_file(&archive)? != dep.sha256 {
        if offline {
            return Err(format!(
                "{} {} is not in {} and --offline was given",
                dep.name,
                dep.version,
                dir.display()
            ));
        }
        let part = dir.join("archive.tar.gz.part");
        eprintln!("fetching {} {} from {}", dep.name, dep.version, dep.url);
        let status = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location", "--retry", "3", "--output"])
            .arg(&part)
            .arg(dep.url)
            .status()
            .map_err(|e| format!("running curl: {e}"))?;
        if !status.success() {
            return Err(format!("curl failed for {} ({status})", dep.url));
        }
        let got = sha256_file(&part)?;
        if got != dep.sha256 {
            let _ = fs::remove_file(&part);
            return Err(format!("{}: SHA-256 mismatch: expected {}, got {got}", dep.url, dep.sha256));
        }
        fs::rename(&part, &archive).map_err(|e| format!("renaming {}: {e}", part.display()))?;
    }

    // Unpack into a fresh, empty directory, then move it into place.
    let tmp = dir.join("src.tmp");
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(|e| format!("removing {}: {e}", tmp.display()))?;
    }
    fs::create_dir(&tmp).map_err(|e| format!("creating {}: {e}", tmp.display()))?;
    let status = Command::new("tar")
        .arg("--no-same-owner")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&tmp)
        .status()
        .map_err(|e| format!("running tar: {e}"))?;
    if !status.success() {
        return Err(format!("tar failed for {} ({status})", archive.display()));
    }
    if src.exists() {
        fs::remove_dir_all(&src).map_err(|e| format!("removing {}: {e}", src.display()))?;
    }
    fs::rename(&tmp, &src).map_err(|e| format!("renaming {}: {e}", tmp.display()))?;
    if !include.is_dir() {
        return Err(format!("{} has no {}", archive.display(), dep.include));
    }
    fs::write(&stamp, dep.sha256).map_err(|e| format!("writing {}: {e}", stamp.display()))?;
    Ok(include)
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok(hex(&Sha256::digest(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn pins_are_well_formed() {
        for dep in DEPS {
            assert_eq!(dep.sha256.len(), 64, "{}", dep.name);
            assert!(dep.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
            assert!(dep.url.starts_with("https://") && dep.url.contains(dep.version), "{}", dep.name);
        }
    }
}

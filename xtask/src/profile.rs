//! `cargo xtask profile`: create or locate a test profile (see `f4x_host::profile`).
//!
//! `profile create NAME --host f4se|f4x [--plugin DLL]...` links each DLL into the
//! profile's `Data/F4SE/Plugins` through its link manifest, replacing the links of an
//! earlier `--plugin` list (or of `f4x launch --host f4se`). `--data install` mirrors
//! the user's deployed install (mods, loose files, F4SE plugins, plugins.txt) instead of
//! official data only.

use std::path::PathBuf;

use f4x_host::{GameEnv, Host, Profile};

pub fn cli(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            let name = args.get(1).ok_or("profile create <name> --host <f4se|f4x> [--data vanilla|install] [--plugin DLL]...")?;
            let host = args.iter().position(|a| a == "--host").and_then(|i| args.get(i + 1)).ok_or("--host <f4se|f4x>")?;
            let mut plugins = Vec::new();
            for (i, arg) in args.iter().enumerate() {
                if arg == "--plugin" {
                    let path = PathBuf::from(args.get(i + 1).ok_or("--plugin needs a DLL path")?);
                    plugins.push(path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?);
                }
            }
            let data = match args.iter().position(|a| a == "--data").and_then(|i| args.get(i + 1)) {
                Some(mode) => f4x_host::profile::DataMode::parse(mode)?,
                None => f4x_host::profile::DataMode::Vanilla,
            };
            let profile = Profile::create_with(name, Host::parse(host)?, data, &GameEnv::from_env())?;
            if !plugins.is_empty() {
                for link in profile.set_plugins(&plugins)? {
                    eprintln!("linked {}", link.display());
                }
            }
            Ok(())
        }
        Some("path") => {
            let name = args.get(1).ok_or("profile path <name>")?;
            println!("{}", Profile::load(name)?.root.display());
            Ok(())
        }
        _ => Err("usage: cargo xtask profile <create NAME --host f4se|f4x [--plugin DLL]... | path NAME>".into()),
    }
}

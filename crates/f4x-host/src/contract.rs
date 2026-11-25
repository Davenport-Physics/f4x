//! The host contract: how a session is launched for each host, so the in-game agent
//! and test plugins find what they need.
//!
//! Both hosts get the same environment:
//! - `F4X_CONTROL=1`: start the control server;
//! - `F4X_SESSION_DIR=<wine path>`: where `session.json` and every other session file go;
//! - `F4X_PLUGINS=<wine path>;<wine path>...`: the test plugin DLLs (only when given);
//! - `F4X_TEST_MODE=1`: test mode (only when asked; `f4x test` always asks).
//!
//! **Host `f4x`:** `target/x86_64-pc-windows-msvc/release/f4x_loader.exe --game-dir <wine
//! path> --dll <wine path to f4x_runtime.dll>`, in the real install with the default
//! test prefix, or in an f4x profile. The runtime loads `F4X_PLUGINS` itself.
//!
//! **Host `f4se`:** `<profile>/game/f4se_loader.exe` in an f4se profile (default
//! `f4se-baseline`). `f4x_agent.dll` (the agent as an F4SE plugin) and each test plugin
//! are symlinked into `<profile>/game/Data/F4SE/Plugins/` through the profile's link
//! manifest ([`Profile::set_plugins`]), replacing the links of the previous launch.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::env::{GameEnv, wine_path};
use crate::launch::{Launch, new_session};
use crate::profile::{Host, Profile};

/// The default profile for `--host f4se`.
pub const DEFAULT_F4SE_PROFILE: &str = "f4se-baseline";
/// How long the agent gets to publish `session.json` after the game starts.
pub const DEFAULT_STARTUP_TIMEOUT_S: u64 = 120;

/// What the user asked for.
#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub host: Host,
    pub profile: Option<String>,
    pub headless: bool,
    /// Test plugin DLLs (host paths).
    pub plugins: Vec<PathBuf>,
    pub test_mode: bool,
    /// Build the Windows crates first (`cargo xwin`).
    pub build: bool,
    /// Session directory label.
    pub label: String,
    pub startup_timeout_s: u64,
    /// The supervisor kills the game after this long (`None`: never).
    pub lifetime_s: Option<u64>,
    /// The workspace (build output and sessions live under its `target/`).
    pub workspace: PathBuf,
}

impl LaunchOptions {
    pub fn new(host: Host) -> Self {
        LaunchOptions {
            host,
            profile: None,
            headless: false,
            plugins: Vec::new(),
            test_mode: false,
            build: false,
            label: format!("launch-{host}"),
            startup_timeout_s: DEFAULT_STARTUP_TIMEOUT_S,
            lifetime_s: None,
            workspace: crate::workspace_root(),
        }
    }
}

/// A fully resolved launch, written to `<session>/launch.json` and run by the
/// supervisor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LaunchSpec {
    pub host: Host,
    pub profile: Option<String>,
    pub env: GameEnv,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub game_dir: PathBuf,
    pub session: PathBuf,
    pub headless: bool,
    pub extra_env: Vec<(String, String)>,
    pub workspace: PathBuf,
    pub share: Vec<PathBuf>,
    pub plugins: Vec<PathBuf>,
    pub test_mode: bool,
    pub startup_timeout_s: u64,
    pub lifetime_s: Option<u64>,
}

impl LaunchSpec {
    pub fn launch(&self) -> Launch<'_> {
        Launch {
            env: &self.env,
            program: self.program.clone(),
            args: self.args.clone(),
            game_dir: self.game_dir.clone(),
            session: self.session.clone(),
            headless: self.headless,
            extra_env: self.extra_env.clone(),
            workspace: self.workspace.clone(),
            share: self.share.clone(),
        }
    }

    pub fn write(&self) -> Result<(), String> {
        let path = self.session.join(crate::supervisor::LAUNCH_FILE);
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn read(session: &Path) -> Result<LaunchSpec, String> {
        let path = session.join(crate::supervisor::LAUNCH_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The f4x environment variables for a launch.
pub fn contract_env(plugins: &[PathBuf], test_mode: bool) -> Vec<(String, String)> {
    let mut env = vec![("F4X_CONTROL".to_owned(), "1".to_owned())];
    if !plugins.is_empty() {
        let list: Vec<String> = plugins.iter().map(|p| wine_path(p)).collect();
        env.push(("F4X_PLUGINS".into(), list.join(";")));
    }
    if test_mode {
        env.push(("F4X_TEST_MODE".into(), "1".into()));
    }
    env
}

/// Resolve options into a launch: check the tools and build output, prepare the
/// profile and prefix (muted), and create the session directory.
pub fn prepare(options: &LaunchOptions) -> Result<LaunchSpec, String> {
    let root = &options.workspace;
    let base = GameEnv::from_env();
    base.check()?;
    // The binaries built for the profile's game generation (`target/ng` for next-gen).
    let install = match &options.profile {
        Some(name) => Profile::load(name).ok().and_then(|p| p.install).unwrap_or_else(|| base.game_dir.clone()),
        None => base.game_dir.clone(),
    };
    let generation = crate::exe_generation(&install)?;
    let out = crate::windows_out_for(root, generation);

    let mut plugins = Vec::new();
    for plugin in &options.plugins {
        let path = plugin.canonicalize().map_err(|e| format!("plugin {}: {e}", plugin.display()))?;
        if !path.is_file() {
            return Err(format!("plugin {} is not a file", path.display()));
        }
        plugins.push(path);
    }

    if options.build {
        let packages: &[&str] = match options.host {
            Host::F4x => &["f4x-loader", "f4x-runtime"],
            Host::F4se => &["f4x-agent-f4se"],
        };
        crate::launch::build_windows_for(root, packages, generation)?;
    }

    let (env, program, args, game_dir, profile) = match options.host {
        Host::F4x => {
            let (env, game_dir) = match &options.profile {
                Some(name) => {
                    let profile = Profile::load(name)?;
                    if profile.host != Host::F4x {
                        return Err(format!("profile {name:?} is for host {}, not f4x", profile.host));
                    }
                    (profile.env(&base), profile.game_dir())
                }
                None => {
                    let game_dir = base.game_dir.clone();
                    (base, game_dir)
                }
            };
            // The loader has no per-generation code; it is built once (1.10.163 target dir).
            let loader = crate::windows_out(root).join("f4x_loader.exe");
            let runtime = out.join("f4x_runtime.dll");
            for file in [&loader, &runtime] {
                if !file.is_file() {
                    return Err(format!(
                        "{} is missing; build it with `cargo win -p f4x-loader -p f4x-runtime` (or pass --build)",
                        file.display()
                    ));
                }
            }
            let args = vec!["--game-dir".into(), wine_path(&game_dir), "--dll".into(), wine_path(&runtime)];
            (env, loader, args, game_dir, options.profile.clone())
        }
        Host::F4se => {
            let name = options.profile.clone().unwrap_or_else(|| DEFAULT_F4SE_PROFILE.into());
            let profile = Profile::load_or_create(&name, Host::F4se, &base)?;
            let agent = out.join("f4x_agent.dll");
            let mut links = Vec::new();
            if agent.is_file() {
                links.push(agent);
            } else {
                eprintln!(
                    "warning: {} is not built (`cargo win -p f4x-agent-f4se`); without it the session never publishes session.json",
                    agent.display()
                );
            }
            links.extend(plugins.iter().cloned());
            profile.set_plugins(&links)?;
            let loader = profile.game_dir().join("f4se_loader.exe");
            if !loader.is_file() {
                return Err(format!("{} is missing; recreate the profile", loader.display()));
            }
            (profile.env(&base), loader, Vec::new(), profile.game_dir(), Some(name))
        }
    };
    env.ensure_prefix()?;

    let session = new_session(root, &options.label);
    let spec = LaunchSpec {
        host: options.host,
        profile,
        env,
        program,
        args,
        game_dir,
        session,
        headless: options.headless,
        extra_env: contract_env(&plugins, options.test_mode),
        workspace: root.clone(),
        share: plugins.clone(),
        plugins,
        test_mode: options.test_mode,
        startup_timeout_s: options.startup_timeout_s,
        lifetime_s: options.lifetime_s.filter(|s| *s > 0),
    };
    spec.write()?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_environment() {
        let env = contract_env(&[PathBuf::from("/mnt/a/t1.dll"), PathBuf::from("/mnt/a/t2.dll")], true);
        assert_eq!(
            env,
            vec![
                ("F4X_CONTROL".to_owned(), "1".to_owned()),
                ("F4X_PLUGINS".to_owned(), "Z:\\mnt\\a\\t1.dll;Z:\\mnt\\a\\t2.dll".to_owned()),
                ("F4X_TEST_MODE".to_owned(), "1".to_owned()),
            ]
        );
        assert_eq!(contract_env(&[], false), vec![("F4X_CONTROL".to_owned(), "1".to_owned())]);
    }

    #[test]
    fn launch_spec_round_trips() {
        let dir = crate::test_dir("spec");
        let spec = LaunchSpec {
            host: Host::F4x,
            profile: None,
            env: GameEnv::from_env(),
            program: "/x/f4x_loader.exe".into(),
            args: vec!["--dll".into(), "Z:\\x".into()],
            game_dir: "/g".into(),
            session: dir.clone(),
            headless: true,
            extra_env: contract_env(&[], false),
            workspace: "/w".into(),
            share: Vec::new(),
            plugins: Vec::new(),
            test_mode: false,
            startup_timeout_s: 5,
            lifetime_s: Some(60),
        };
        spec.write().unwrap();
        assert_eq!(LaunchSpec::read(&dir).unwrap(), spec);
    }
}

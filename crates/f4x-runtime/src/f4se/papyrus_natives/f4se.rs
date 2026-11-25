//! `F4SE`: versions and plugin versions (CommonLibF4 `F4SEStub/runtime/src/Papyrus/F4SE.h`).

use super::Entry;

/// The version f4x reports (black-box: F4SE 0.6.23 returns 0/6/23/25 on 1.10.163, F4SE
/// 0.7.9 returns 0/7/9/33 on 1.11.240; `GetScriptVersionRelease` matches the release).
const VERSION: (i32, i32, i32, i32) = f4x_engine::pick!((0, 6, 23, 25), (0, 7, 9, 33));

pub(crate) const NATIVES: &[Entry] = &[
    ("F4SE", "GetVersion", |c| {
        c.ret_int(VERSION.0);
        Ok(())
    }),
    ("F4SE", "GetVersionMinor", |c| {
        c.ret_int(VERSION.1);
        Ok(())
    }),
    ("F4SE", "GetVersionBeta", |c| {
        c.ret_int(VERSION.2);
        Ok(())
    }),
    ("F4SE", "GetVersionRelease", |c| {
        c.ret_int(VERSION.3);
        Ok(())
    }),
    ("F4SE", "GetPluginVersion", |c| {
        let name = c.string(0);
        let version = crate::f4se::plugin_version(&name).map_or(-1, |v| v as i32);
        c.ret_int(version);
        Ok(())
    }),
];

//! The globals every module leans on: `api/Version.cpp`, `interface/Version.cpp`,
//! `expansion/Constants.cpp` and `expansion/Level.cpp`.

use mlua::Value;

use super::{is_number, to_number, Api};

/// The bootstrap chunks, in run order: `(chunk name, source)`.
pub(super) const BOOTSTRAPS: &[(&str, &str)] = &[];

/// `kCurrentExpansionLevel`: the client is vanilla.
const CURRENT_EXPANSION: i64 = 0;

const EXPANSIONS: &[(&str, i64)] = &[
    ("LE_EXPANSION_CLASSIC", 0),
    ("LE_EXPANSION_BURNING_CRUSADE", 1),
    ("LE_EXPANSION_WRATH_OF_THE_LICH_KING", 2),
    ("LE_EXPANSION_CATACLYSM", 3),
    ("LE_EXPANSION_MISTS_OF_PANDARIA", 4),
    ("LE_EXPANSION_WARLORDS_OF_DRAENOR", 5),
    ("LE_EXPANSION_LEGION", 6),
    ("LE_EXPANSION_BATTLE_FOR_AZEROTH", 7),
    ("LE_EXPANSION_SHADOWLANDS", 8),
    ("LE_EXPANSION_DRAGONFLIGHT", 9),
    ("LE_EXPANSION_WAR_WITHIN", 10),
    ("LE_EXPANSION_MIDNIGHT", 11),
    ("LE_EXPANSION_LEVEL_CURRENT", CURRENT_EXPANSION),
];

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.number("CLASSIC_API_VERSION", crate::VERSION_VALUE)?;
    // The engine's `FUN_ADDON_CLIENT_INTERFACE_VERSION`, the `## Interface` the client loads.
    api.number(
        "INTERFACE_VERSION",
        benilla_ui::script::addon_gate::CLIENT_INTERFACE,
    )?;
    for (name, value) in EXPANSIONS {
        api.number(name, *value)?;
    }
    api.global("GetClassicExpansionLevel", |_, ()| Ok(CURRENT_EXPANSION))?;
    api.global("ClassicExpansionAtLeast", |_, level: Value| {
        if !is_number(&level) {
            return Err(mlua::Error::runtime(
                "Usage: ClassicExpansionAtLeast(expansionLevel)",
            ));
        }
        Ok(CURRENT_EXPANSION >= to_number(&level) as i64)
    })?;
    api.global("ClassicExpansionAtMost", |_, level: Value| {
        if !is_number(&level) {
            return Err(mlua::Error::runtime(
                "Usage: ClassicExpansionAtMost(expansionLevel)",
            ));
        }
        Ok(CURRENT_EXPANSION <= to_number(&level) as i64)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;
    use crate::Ca;

    #[test]
    fn the_version_and_the_mirror_are_published() {
        let script = vm(&Ca::default());
        let v: u32 = script.eval("return CLASSIC_API_VERSION").unwrap();
        assert_eq!(v, crate::VERSION_VALUE);
        let same: bool = script
            .eval("return ClassicAPI.ClassicExpansionAtLeast == ClassicExpansionAtLeast")
            .unwrap();
        assert!(same);
        let (a, b): (bool, bool) = script
            .eval("return ClassicExpansionAtLeast(0), ClassicExpansionAtMost(-1)")
            .unwrap();
        assert!(a && !b);
    }
}

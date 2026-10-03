//! The plain unit readers: `ClassBase.cpp`, `RaceBase.cpp`, `CreatureFamily.cpp`,
//! `CreatureID.cpp`, `CreatureType.cpp`, `Combat.cpp` and `Health.cpp`.

use mlua::{IntoLuaMulti, Lua, Value};

use super::{token_arg, unit_guid};
use crate::lua::Api;
use crate::Ca;

/// `ChrClasses.dbc`'s `Filename` (`OFF_CHRCLASSES_FILENAME` 0x38 / 4).
const CHRCLASSES_FILENAME: usize = 0x38 / 4;
/// `ChrRaces.dbc`'s `ClientFileString` (`OFF_CHRRACES_FILENAME` 0x3C / 4).
const CHRRACES_FILENAME: usize = 0x3C / 4;

/// A `Chr*` table's id for a file token, the reverse of `DBC::ClassToken` / `RaceToken`.
fn id_by_file(ca: &Ca, table: &'static str, col: usize, file: &str) -> Option<u32> {
    ca.db
        .get(table)?
        .rows()
        .find(|r| r.str(col).eq_ignore_ascii_case(file))
        .map(|r| r.id())
}

/// The token's `(file, id)` for a `Chr*` table: the live descriptor's byte when the unit is held,
/// else the file token the stock verb answers from its record or the roster.
fn chr_base(
    lua: &Lua,
    ca: &Ca,
    token: &str,
    table: &'static str,
    col: usize,
    byte: fn(&crate::mirror::Fields) -> u8,
    file_of: fn(&benilla_ui::script::UnitState) -> Option<String>,
) -> mlua::Result<mlua::MultiValue> {
    let state = benilla_ui::script::ext_read::unit_state(lua, token)?;
    let guid = state.as_ref().map_or(0, |s| s.guid);
    let held = {
        let st = ca.lock();
        st.mirror.object(guid).map(byte).filter(|b| *b != 0)
    };
    let id = match held {
        Some(b) => Some(u32::from(b)),
        None => state
            .as_ref()
            .and_then(file_of)
            .and_then(|f| id_by_file(ca, table, col, &f)),
    };
    let file = id.and_then(|id| {
        ca.db
            .get(table)?
            .row(id)
            .map(|r| r.str(col).to_string())
            .filter(|s| !s.is_empty())
    });
    match (file, id) {
        (Some(file), Some(id)) => (file, id).into_lua_multi(lua),
        _ => (Value::Nil, Value::Nil).into_lua_multi(lua),
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.global("UnitClassBase", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitClassBase(\"unit\")")?;
        chr_base(
            lua,
            &c,
            &token,
            "ChrClasses",
            CHRCLASSES_FILENAME,
            crate::mirror::Fields::class,
            |s| s.class_file.clone(),
        )
    })?;
    let c = api.ca.clone();
    api.global("UnitRaceBase", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitRaceBase(\"unit\")")?;
        chr_base(
            lua,
            &c,
            &token,
            "ChrRaces",
            CHRRACES_FILENAME,
            crate::mirror::Fields::race,
            |s| s.race_file.clone(),
        )
    })?;

    // The family id the stock `UnitCreatureFamily` reads before its name lookup: the creature
    // cache row's family, nil for players, non-beasts and unqueried templates.
    let c = api.ca.clone();
    api.global("UnitCreatureFamilyID", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitCreatureFamilyID(\"unit\")")?;
        let guid = unit_guid(lua, &token)?.unwrap_or(0);
        let family = c
            .lock()
            .mirror
            .creatures
            .get(&guid)
            .map(|c| c.1)
            .filter(|f| *f > 0);
        Ok(family)
    })?;

    // The live template entry when the unit is held, else a creature guid's packed entry.
    let c = api.ca.clone();
    api.global("UnitCreatureID", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitCreatureID(\"unit\")")?;
        let guid = unit_guid(lua, &token)?.unwrap_or(0);
        Ok(creature_id(&c, guid))
    })?;

    // The type the stock `UnitCreatureType` names: its resolver's form, template or race type,
    // which benilla's snapshot carries by name.
    let c = api.ca.clone();
    api.global("UnitCreatureTypeID", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitCreatureTypeID(\"unit\")")?;
        let Some(state) = benilla_ui::script::ext_read::unit_state(lua, &token)? else {
            return Ok(None);
        };
        if let Some(t) = c.lock().mirror.creatures.get(&state.guid).map(|c| c.0) {
            if t > 0 && !state.is_player {
                return Ok(Some(t));
            }
        }
        let Some(name) = state.creature_type_name else {
            return Ok(None);
        };
        Ok(c.db
            .get("CreatureType")
            .and_then(|t| t.rows().find(|r| r.loc(1) == name).map(|r| r.id())))
    })?;

    // No secure frames in 1.12: never in lockdown.
    api.global("InCombatLockdown", |_, ()| Ok(false))?;

    api.global("UnitHealthMissing", |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitHealthMissing(\"unit\")")?;
        let state = benilla_ui::script::ext_read::unit_state(lua, &token)?;
        Ok(state.map_or(0, |s| s.max_health.saturating_sub(s.health)))
    })?;
    Ok(())
}

/// `Unit::CreatureID::ForGuid`.
pub(crate) fn creature_id(ca: &Ca, guid: u64) -> Option<u32> {
    let kind = crate::guid::classify(guid);
    if kind != crate::guid::Kind::Creature && kind != crate::guid::Kind::Pet {
        return None;
    }
    let held = ca
        .lock()
        .mirror
        .object(guid)
        .map(|f| f.entry())
        .filter(|e| *e != 0);
    held.or_else(|| Some(crate::guid::creature_entry(guid)))
        .filter(|e| *e != 0)
}

//! `player/Info.cpp`, `player/LocationInfo.cpp` and `guid/PlayerInfo.cpp`:
//! `GetPlayerInfoByGUID`, `UnitNameFromGUID` and `C_PlayerInfo`.
//!
//! Names come from benilla's name cache (the `SMSG_NAME_QUERY_RESPONSE` answers) and the streamed
//! units' names, class, race and sex from the unit's descriptor or the name query's traits.
//! Deviation: the DLL's friends-list and opt-in persistent-cache fallbacks are not ported, so a
//! name no query has answered this session resolves to nothing.

use mlua::{IntoLuaMulti, Lua, MultiValue, Value};

use crate::guid::{self, Kind};
use crate::lua::{to_str, Api};
use crate::mirror::Mirror;
use crate::Ca;

/// `ChrClasses.dbc` and `ChrRaces.dbc`: the localized name and the token columns.
const CLASS_NAME: usize = 0x14 / 4;
const CLASS_TOKEN: usize = 0x38 / 4;
const RACE_NAME: usize = 0x44 / 4;
const RACE_TOKEN: usize = 0x3C / 4;

/// A unit's name as `Script_UnitName` resolves it: the streamed object's, else the name cache's.
fn name_of(m: &Mirror, guid: u64) -> Option<String> {
    m.names
        .get(&guid)
        .or_else(|| m.player_names.get(&guid).map(|p| &p.0))
        .filter(|n| !n.is_empty() && n.as_str() != "UNKNOWNOBJECT" && n.as_str() != "Unknown Being")
        .cloned()
}

/// `(race, class, gender)` of a player: the streamed descriptor's, else the name query's.
fn traits(m: &Mirror, guid: u64) -> Option<(u8, u8, u8)> {
    match m.object(guid) {
        Some(f) if f.class() != 0 => Some((f.race(), f.class(), f.gender())),
        _ => m.player_names.get(&guid).and_then(|p| p.1),
    }
}

fn dbc_names(
    ca: &Ca,
    table: &'static str,
    id: u8,
    name_col: usize,
    token_col: usize,
) -> (String, String) {
    ca.db
        .get(table)
        .and_then(|t| {
            t.row(u32::from(id))
                .map(|r| (r.loc(name_col).to_string(), r.str(token_col).to_string()))
        })
        .unwrap_or_default()
}

/// A `PlayerLocation`'s guid: its `unit` token, else its `guid` string.
fn location_guid(lua: &Lua, loc: &Value) -> Option<u64> {
    let Value::Table(t) = loc else {
        return None;
    };
    let field =
        |k: &str| -> Option<String> { to_str(&t.get::<Value>(k).ok()?).filter(|s| !s.is_empty()) };
    if let Some(token) = field("unit") {
        return crate::lua::unit::unit_guid(lua, &token).ok().flatten();
    }
    field("guid")
        .and_then(|g| guid::parse(&g))
        .filter(|g| *g != 0)
}

fn guid_arg(v: &Value, usage: &str) -> mlua::Result<Option<u64>> {
    match v {
        Value::String(s) => Ok(guid::parse(&s.to_str()?).filter(|g| *g != 0)),
        _ => Err(mlua::Error::runtime(usage.to_string())),
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `(localizedClass, englishClass, localizedRace, englishRace, sex, name, realm)`, sex 2 male
    // and 3 female, the realm always "" in a single-realm 1.12.
    let c = api.ca.clone();
    api.global("GetPlayerInfoByGUID", move |lua, v: Value| {
        let Some(g) = guid_arg(&v, "Usage: GetPlayerInfoByGUID(\"0x...\")")? else {
            return Ok(MultiValue::new());
        };
        // The live caches, else the persistent name cache when it is on.
        let found = {
            let st = c.lock();
            name_of(&st.mirror, g)
                .zip(traits(&st.mirror, g))
                .or_else(|| {
                    st.playercache
                        .by_guid(g)
                        .map(|(name, e)| (name, (e.race as u8, e.class as u8, e.sex as u8)))
                })
        };
        let Some((name, (race, class, gender))) = found else {
            return Ok(MultiValue::new());
        };
        let (class_name, class_token) = dbc_names(&c, "ChrClasses", class, CLASS_NAME, CLASS_TOKEN);
        let (race_name, race_token) = dbc_names(&c, "ChrRaces", race, RACE_NAME, RACE_TOKEN);
        (
            class_name,
            class_token,
            race_name,
            race_token,
            i64::from(gender) + 2,
            name,
            "",
        )
            .into_lua_multi(lua)
    })?;

    let c = api.ca.clone();
    api.global("UnitNameFromGUID", move |lua, v: Value| {
        let Some(g) = guid_arg(&v, "Usage: UnitNameFromGUID(\"0x...\")")? else {
            return Ok(MultiValue::new());
        };
        // The live caches, then the friends list (online or not), then the persistent cache.
        let live = name_of(&c.lock().mirror, g);
        let name = live
            .or_else(|| {
                benilla_ui::script::ext_read::social(lua)?
                    .friends
                    .into_iter()
                    .find(|f| f.guid == g)
                    .map(|f| f.name)
                    .filter(|n| !n.is_empty())
            })
            .or_else(|| c.lock().playercache.by_guid(g).map(|p| p.0));
        match name {
            Some(name) => (name, "").into_lua_multi(lua),
            None => Ok(MultiValue::new()),
        }
    })?;

    for (name, kind) in [
        ("GUIDIsPlayer", Kind::Player),
        ("GUIDIsCreature", Kind::Creature),
        ("GUIDIsPet", Kind::Pet),
        ("GUIDIsGameObject", Kind::GameObject),
    ] {
        api.table("C_PlayerInfo", name, move |_, v: Value| {
            Ok(to_str(&v)
                .and_then(|s| guid::parse(&s))
                .is_some_and(|g| guid::classify(g) == kind))
        })?;
    }

    let c = api.ca.clone();
    api.table("C_PlayerInfo", "GetName", move |lua, loc: Value| {
        let Some(g) = location_guid(lua, &loc) else {
            return Ok(None);
        };
        let st = c.lock();
        Ok(st.mirror.object(g).and_then(|_| name_of(&st.mirror, g)))
    })?;
    let c = api.ca.clone();
    api.table("C_PlayerInfo", "GetClass", move |lua, loc: Value| {
        let class = location_guid(lua, &loc)
            .and_then(|g| c.lock().mirror.object(g).map(|f| f.class()))
            .filter(|c| *c > 0);
        let Some(class) = class else {
            return Ok(MultiValue::new());
        };
        let (name, token) = dbc_names(&c, "ChrClasses", class, CLASS_NAME, CLASS_TOKEN);
        if token.is_empty() {
            return Ok(MultiValue::new());
        }
        (name, token, i64::from(class)).into_lua_multi(lua)
    })?;
    let c = api.ca.clone();
    api.table("C_PlayerInfo", "GetRace", move |lua, loc: Value| {
        Ok(location_guid(lua, &loc)
            .and_then(|g| c.lock().mirror.object(g).map(|f| i64::from(f.race())))
            .filter(|r| *r > 0))
    })?;
    let c = api.ca.clone();
    api.table("C_PlayerInfo", "GetSex", move |lua, loc: Value| {
        Ok(location_guid(lua, &loc)
            .and_then(|g| c.lock().mirror.object(g).map(|f| i64::from(f.gender()) + 2)))
    })?;
    // Online when the unit is streamed; a groupmate out of range by the roster's own answer.
    let c = api.ca.clone();
    let connected: Option<mlua::Function> = api.lua.globals().get("UnitIsConnected").ok();
    api.table("C_PlayerInfo", "IsConnected", move |lua, loc: Value| {
        let g = match &loc {
            Value::Table(_) => location_guid(lua, &loc),
            _ => Some(c.lock().mirror.player).filter(|g| *g != 0),
        };
        let Some(g) = g else {
            return Ok(None);
        };
        let (streamed, grouped) = {
            let st = c.lock();
            (st.mirror.object(g).is_some(), st.mirror.group.contains(&g))
        };
        if streamed {
            return Ok(Some(true));
        }
        let token = grouped
            .then(|| crate::lua::unit::identity::token_from_guid(lua, &c, g))
            .flatten();
        let online = match (token, &connected) {
            (Some(t), Some(f)) => crate::lua::truthy(&f.call::<Value>(t)?),
            _ => false,
        };
        Ok(Some(online))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn guids_classify_and_unknown_ones_answer_nothing() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                return tostring(C_PlayerInfo.GUIDIsPlayer("0x0000000000000010"))
                  .. tostring(C_PlayerInfo.GUIDIsCreature("0xF130000C1F00A3B2"))
                  .. tostring(C_PlayerInfo.GUIDIsPet("0x0000000000000010"))
                  .. tostring((UnitNameFromGUID("0x0000000000000010")))
                  .. tostring((GetPlayerInfoByGUID("0x0000000000000010")))
                  .. tostring(pcall(UnitNameFromGUID))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "truetruefalsenilnilfalse");
    }
}

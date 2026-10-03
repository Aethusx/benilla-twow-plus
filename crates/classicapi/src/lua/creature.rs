//! `creature/Info.cpp`, `gameobject/Info.cpp` and `quest/Data.cpp`'s Lua surface:
//! `C_CreatureInfo`, `C_GameObjectInfo` and `C_QuestLog.RequestLoadQuestByID` /
//! `IsQuestDataCachedByID`, over [`crate::templates`] and the client databases.

use mlua::{Lua, Table, Value};

use crate::dbc::Row;
use crate::lua::{is_number, to_int, Api};
use crate::templates::Kind;
use crate::Ca;

/// `ChrClasses.dbc`, `ChrRaces.dbc`, `CreatureFamily.dbc`, `CreatureType.dbc`,
/// `FactionTemplate.dbc` and `FactionGroup.dbc` columns, `Offsets.h` byte offsets over 4.
const CLASS_NAME: usize = 0x14 / 4;
const CLASS_TOKEN: usize = 0x38 / 4;
const RACE_FACTION_TEMPLATE: usize = 0x08 / 4;
const RACE_TOKEN: usize = 0x3C / 4;
const RACE_NAME: usize = 0x44 / 4;
const FAMILY_NAME: usize = 0x20 / 4;
const FAMILY_ICON: usize = 0x44 / 4;
const TYPE_NAME: usize = 1; // +0x04
const TEMPLATE_GROUP_MASK: usize = 0x0C / 4;
const GROUP_BIT: usize = 1; // +0x04
const GROUP_ENGLISH: usize = 0x08 / 4;
const GROUP_NAME: usize = 0x0C / 4;

/// A positive numeric id argument.
fn id_arg(v: &Value) -> Option<u32> {
    is_number(v)
        .then(|| to_int(v))
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

fn row_table(
    lua: &Lua,
    ca: &Ca,
    table: &'static str,
    id: u32,
    f: impl FnOnce(&Lua, &Row) -> mlua::Result<Option<Table>>,
) -> mlua::Result<Option<Table>> {
    match ca.db.get(table).as_deref().and_then(|t| t.row(id)) {
        Some(r) => f(lua, &r),
        None => Ok(None),
    }
}

/// The ids of a table's rows with a localized name, ascending.
fn named_ids(ca: &Ca, table: &'static str, name_col: usize) -> Vec<u32> {
    let mut ids: Vec<u32> = ca
        .db
        .get(table)
        .map(|t| {
            t.rows()
                .filter(|r| !r.loc(name_col).is_empty())
                .map(|r| r.id())
                .collect()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids
}

fn request(ca: &Ca, kind: Kind, v: &Value) -> bool {
    let Some(id) = id_arg(v) else {
        return false;
    };
    ca.lock()
        .templates
        .request(kind, id, std::time::Instant::now());
    true
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.table(
        "C_CreatureInfo",
        "GetCreatureInfoByID",
        move |lua, v: Value| {
            let Some(id) = id_arg(&v) else {
                return Ok(None);
            };
            let Some(rec) = c.lock().templates.creatures.get(&id).cloned() else {
                return Ok(None);
            };
            let t = lua.create_table()?;
            t.set("creatureID", id)?;
            t.set("name", rec.name)?;
            t.set("subName", rec.subname)?;
            t.set("type", rec.creature_type)?;
            t.set("family", rec.family)?;
            t.set("rank", rec.rank)?;
            t.set("displayID", rec.display_id)?;
            Ok(Some(t))
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_CreatureInfo",
        "RequestLoadCreatureByID",
        move |_, v: Value| Ok(request(&c, Kind::Creature, &v)),
    )?;

    let c = api.ca.clone();
    api.table("C_CreatureInfo", "GetRaceInfo", move |lua, v: Value| {
        let Some(id) = id_arg(&v) else {
            return Ok(None);
        };
        row_table(lua, &c, "ChrRaces", id, |lua, r| {
            let file = r.str(RACE_TOKEN);
            if file.is_empty() {
                return Ok(None);
            }
            let t = lua.create_table()?;
            t.set("raceName", r.loc(RACE_NAME))?;
            t.set("clientFileString", file)?;
            t.set("raceID", id)?;
            Ok(Some(t))
        })
    })?;
    let c = api.ca.clone();
    api.table("C_CreatureInfo", "GetClassInfo", move |lua, v: Value| {
        let Some(id) = id_arg(&v) else {
            return Ok(None);
        };
        row_table(lua, &c, "ChrClasses", id, |lua, r| {
            let file = r.str(CLASS_TOKEN);
            if file.is_empty() {
                return Ok(None);
            }
            let t = lua.create_table()?;
            t.set("className", r.loc(CLASS_NAME))?;
            t.set("classFile", file)?;
            t.set("classID", id)?;
            Ok(Some(t))
        })
    })?;
    let c = api.ca.clone();
    api.table(
        "C_CreatureInfo",
        "GetCreatureFamilyInfo",
        move |lua, v: Value| {
            let Some(id) = id_arg(&v) else {
                return Ok(None);
            };
            row_table(lua, &c, "CreatureFamily", id, |lua, r| {
                let name = r.loc(FAMILY_NAME);
                if name.is_empty() {
                    return Ok(None);
                }
                let t = lua.create_table()?;
                t.set("id", id)?;
                t.set("name", name)?;
                t.set("iconFile", r.str(FAMILY_ICON))?;
                Ok(Some(t))
            })
        },
    )?;
    let c = api.ca.clone();
    api.table("C_CreatureInfo", "GetCreatureFamilyIDs", move |_, ()| {
        Ok(named_ids(&c, "CreatureFamily", FAMILY_NAME))
    })?;
    let c = api.ca.clone();
    api.table(
        "C_CreatureInfo",
        "GetCreatureTypeInfo",
        move |lua, v: Value| {
            let Some(id) = id_arg(&v) else {
                return Ok(None);
            };
            row_table(lua, &c, "CreatureType", id, |lua, r| {
                let name = r.loc(TYPE_NAME);
                if name.is_empty() {
                    return Ok(None);
                }
                let t = lua.create_table()?;
                t.set("id", id)?;
                t.set("name", name)?;
                Ok(Some(t))
            })
        },
    )?;
    let c = api.ca.clone();
    api.table("C_CreatureInfo", "GetCreatureTypeIDs", move |_, ()| {
        Ok(named_ids(&c, "CreatureType", TYPE_NAME))
    })?;
    // The first named `FactionGroup` in the race's faction template's group mask: Alliance or
    // Horde ("Player" and "Monster" have no localized name).
    let c = api.ca.clone();
    api.table("C_CreatureInfo", "GetFactionInfo", move |lua, v: Value| {
        let Some(race) = id_arg(&v) else {
            return Ok(None);
        };
        let template =
            c.db.get("ChrRaces")
                .and_then(|t| t.row(race).map(|r| r.u32(RACE_FACTION_TEMPLATE)));
        let mask = template.and_then(|ft| {
            c.db.get("FactionTemplate")
                .and_then(|t| t.row(ft).map(|r| r.u32(TEMPLATE_GROUP_MASK)))
        });
        let (Some(mask), Some(groups)) = (mask, c.db.get("FactionGroup")) else {
            return Ok(None);
        };
        for r in groups.rows() {
            if mask & (1 << (r.u32(GROUP_BIT) & 0x1f)) == 0 || r.loc(GROUP_NAME).is_empty() {
                continue;
            }
            let t = lua.create_table()?;
            t.set("name", r.loc(GROUP_NAME))?;
            t.set("groupTag", r.str(GROUP_ENGLISH))?;
            return Ok(Some(t));
        }
        Ok(None)
    })?;

    let c = api.ca.clone();
    api.table(
        "C_GameObjectInfo",
        "GetGameObjectInfoByID",
        move |lua, v: Value| {
            let Some(id) = id_arg(&v) else {
                return Ok(None);
            };
            let Some(rec) = c.lock().templates.gameobjects.get(&id).cloned() else {
                return Ok(None);
            };
            let t = lua.create_table()?;
            t.set("gameObjectID", id)?;
            t.set("name", rec.name)?;
            t.set("type", rec.type_id)?;
            t.set("displayID", rec.display_id)?;
            Ok(Some(t))
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_GameObjectInfo",
        "RequestLoadGameObjectByID",
        move |_, v: Value| Ok(request(&c, Kind::GameObject, &v)),
    )?;

    let c = api.ca.clone();
    api.table("C_QuestLog", "RequestLoadQuestByID", move |_, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_QuestLog.RequestLoadQuestByID(questID)",
            ));
        }
        request(&c, Kind::Quest, &v);
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table("C_QuestLog", "IsQuestDataCachedByID", move |_, v: Value| {
        Ok(id_arg(&v).is_some_and(|id| c.lock().templates.cached(Kind::Quest, id)))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn races_classes_and_factions_read_the_client_tables() {
        let _ = benilla_formats::wow_data_or_skip!();
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local r = C_CreatureInfo.GetRaceInfo(2)
                local f = C_CreatureInfo.GetFactionInfo(1)
                local c = C_CreatureInfo.GetClassInfo(8)
                local fam = C_CreatureInfo.GetCreatureFamilyIDs()
                local typ = C_CreatureInfo.GetCreatureTypeInfo(7)
                return r.clientFileString .. " " .. f.groupTag .. " " .. c.classFile .. " "
                  .. tostring(table.getn(fam) > 5) .. " " .. typ.name .. " "
                  .. tostring(C_CreatureInfo.GetCreatureInfoByID(69))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "Orc Alliance MAGE true Humanoid nil");
    }
}

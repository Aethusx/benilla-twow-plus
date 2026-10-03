//! `nameplate/Info.cpp`: `C_NamePlate`, over benilla's plate pool. A plate frame is the pool's
//! own Lua frame, the one plate addons already find under `WorldFrame`.

use mlua::{Lua, Value};

use super::{as_string, Api};
use crate::Ca;

/// `PushNamePlateForGUID`: the unit's live plate, else the one whose removal is being dispatched.
fn plate_for(lua: &Lua, ca: &Ca, guid: u64) -> Value {
    if guid == 0 {
        return Value::Nil;
    }
    let live = benilla_ui::script::ext_read::nameplates(lua);
    let frame = live
        .iter()
        .find(|(g, _)| *g == guid)
        .map(|(_, f)| *f)
        .or_else(|| {
            ca.lock()
                .plate_removing
                .filter(|(g, _)| *g == guid)
                .map(|(_, f)| f)
        });
    match frame {
        Some(f) => benilla_ui::script::ext_read::frame_value(lua, f),
        None => Value::Nil,
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.table("C_NamePlate", "GetNamePlates", |lua, ()| {
        let out = lua.create_table()?;
        for (i, (_, frame)) in benilla_ui::script::ext_read::nameplates(lua)
            .into_iter()
            .enumerate()
        {
            out.raw_set(i + 1, benilla_ui::script::ext_read::frame_value(lua, frame))?;
        }
        Ok(out)
    })?;
    api.table("C_NamePlate", "GetNamePlateGUIDs", |lua, ()| {
        let out = lua.create_table()?;
        for (i, (guid, _)) in benilla_ui::script::ext_read::nameplates(lua)
            .into_iter()
            .filter(|(g, _)| *g != 0)
            .enumerate()
        {
            out.raw_set(i + 1, crate::items::guid_string(guid))?;
        }
        Ok(out)
    })?;
    let c = api.ca.clone();
    api.table(
        "C_NamePlate",
        "GetNamePlateForUnit",
        move |lua, v: Value| {
            let Some(token) = as_string(&v) else {
                return Ok(Value::Nil);
            };
            let guid = benilla_ui::script::unit_token_guid_in(lua, &token).unwrap_or(0);
            Ok(plate_for(lua, &c, guid))
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_NamePlate",
        "GetNamePlateForGUID",
        move |lua, v: Value| {
            let guid = as_string(&v)
                .and_then(|s| crate::items::parse_guid(&s))
                .unwrap_or(0);
            Ok(plate_for(lua, &c, guid))
        },
    )?;
    Ok(())
}

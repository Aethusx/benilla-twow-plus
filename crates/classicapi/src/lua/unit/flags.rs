//! `unit/Flags.cpp`: AFK, DND, feign death, possession and guild membership.

use mlua::Value;

use super::token_arg;
use crate::lua::{to_str, Api};
use crate::mirror::field;

/// `PLAYER_FLAGS`' AFK and DND bits.
const PLAYER_FLAG_AFK: u32 = 0x02;
const PLAYER_FLAG_DND: u32 = 0x04;
/// `UNIT_FIELD_FLAGS`' possessed and feign-death bits.
const UNIT_FLAG_POSSESSED: u32 = 0x0100_0000;
const UNIT_FLAG_FEIGN_DEATH: u32 = 0x2000_0000;

/// `TestPlayerFlag`: a held player object's `PLAYER_FLAGS` bit; false for anything else.
fn player_flag(lua: &mlua::Lua, v: &Value, mask: u32) -> bool {
    let Some(token) = to_str(v) else {
        return false;
    };
    match benilla_ui::script::ext_read::unit_state(lua, &token) {
        Ok(Some(s)) => s.has_object && s.is_player && s.player_flags & mask != 0,
        _ => false,
    }
}

/// A held unit's `UNIT_FIELD_FLAGS` bit, after the usage check.
fn unit_flag(lua: &mlua::Lua, v: &Value, usage: &str, mask: u32) -> mlua::Result<bool> {
    let token = token_arg(v, usage)?;
    Ok(benilla_ui::script::ext_read::unit_state(lua, &token)?
        .is_some_and(|s| s.has_object && s.flags & mask != 0))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.global("UnitIsAFK", |lua, v: Value| {
        Ok(player_flag(lua, &v, PLAYER_FLAG_AFK))
    })?;
    api.global("UnitIsDND", |lua, v: Value| {
        Ok(player_flag(lua, &v, PLAYER_FLAG_DND))
    })?;
    api.global("UnitIsFeignDeath", |lua, v: Value| {
        unit_flag(
            lua,
            &v,
            "Usage: UnitIsFeignDeath(\"unit\")",
            UNIT_FLAG_FEIGN_DEATH,
        )
    })?;
    api.global("UnitIsPossessed", |lua, v: Value| {
        unit_flag(
            lua,
            &v,
            "Usage: UnitIsPossessed(\"unit\")",
            UNIT_FLAG_POSSESSED,
        )
    })?;

    // `UnitIsInMyGuild(unitOrName)`: 1 or nil, as 3.3.5 pushes it. A held player compares guild
    // ids (`PLAYER_GUILDID`); otherwise the name, a token's cached one or the literal, against
    // the whole roster.
    let c = api.ca.clone();
    api.global("UnitIsInMyGuild", move |lua, v: Value| {
        let input = token_arg(&v, "Usage: UnitIsInMyGuild(\"unitOrName\")")?;
        if input.is_empty() {
            return Ok(None);
        }
        let guid = benilla_ui::script::ext_read::unit_guid(lua, &input)
            .ok()
            .flatten()
            .unwrap_or(0);
        let want = {
            let st = c.lock();
            let m = &st.mirror;
            let mine = m.me().map_or(0, |f| f.u32(field::PLAYER_GUILDID));
            if mine == 0 {
                return Ok(None);
            }
            if guid == 0 {
                input
            } else {
                let theirs = m
                    .object(guid)
                    .filter(|f| f.is(crate::mirror::typemask::PLAYER))
                    .map_or(0, |f| f.u32(field::PLAYER_GUILDID));
                if theirs == mine {
                    return Ok(Some(1));
                }
                if theirs != 0 {
                    return Ok(None);
                }
                match m.player_names.get(&guid) {
                    Some((name, _)) => name.clone(),
                    None => return Ok(None),
                }
            }
        };
        Ok(benilla_ui::script::ext_read::guild_roster_names(lua)
            .contains(&want)
            .then_some(1))
    })?;
    Ok(())
}

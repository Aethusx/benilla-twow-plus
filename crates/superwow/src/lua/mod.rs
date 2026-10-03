//! SuperWoW's Lua API, installed on each in-game VM before the interface loads: the unit-token
//! extension, the natives, then the bootstrap chunk (`superwow.lua`), which declares the CVars and
//! wraps the stock verbs SuperWoW extends.
//!
//! The token extension locks the shared state, and a native may resolve a token: every native
//! resolves its tokens before it takes the lock, never while holding it.

use benilla_app::ext::UiScript;
use benilla_ui::script::{player_buff_spell_id, unit_aura_spell_id, unit_token_guid_in};
use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::tokens::{self, guid_string};
use crate::Sw;

/// The bootstrap chunk.
const BOOTSTRAP: &str = include_str!("superwow.lua");

/// Install the token extension, every native, then the bootstrap; a failure is reported to the
/// script error handler and leaves the stock interface as it was.
pub fn install(sw: &Sw, script: &mut UiScript) {
    let ext = sw.clone();
    script.set_unit_token_extension(Box::new(move |stock, token| {
        let st = ext.lock();
        tokens::resolve(stock, &st.marks, &*st, token)
    }));
    let result = (|| -> mlua::Result<()> {
        let lua = script.lua();
        let g = lua.globals();
        natives(lua, &g, sw)?;
        crate::files::install(lua, &g)?;
        Ok(())
    })();
    if let Err(e) = result {
        script.report_script_error(&format!("superwow: {e}"));
        return;
    }
    if let Err(e) = script.run_chunk_named(BOOTSTRAP.as_bytes(), "Interface\\superwow.lua") {
        script.report_script_error(&format!("superwow: {e}"));
    }
}

/// A number argument, as `lua_tonumber` reads one.
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        Value::String(s) => s.to_str().ok()?.trim().parse().ok(),
        _ => None,
    }
}

/// The unit a token argument names, through benilla's resolver and this crate's grammar.
fn unit(lua: &Lua, v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => unit_token_guid_in(lua, &s.to_str().ok()?),
        _ => None,
    }
}

/// 1 or nil, the 1.12 predicate shape.
fn flag(b: bool) -> Option<i64> {
    b.then_some(1)
}

/// A switch: an argument sets it (anything but nil, false and 0 is on), and the answer is its
/// state as 1 or 0.
fn switch(slot: &mut bool, v: &Value) -> i64 {
    match v {
        Value::Nil => {}
        Value::Boolean(b) => *slot = *b,
        other => *slot = number(other).is_none_or(|n| n != 0.0),
    }
    i64::from(*slot)
}

fn set<A, R, F>(lua: &Lua, g: &Table, name: &str, f: F) -> mlua::Result<()>
where
    A: mlua::FromLuaMulti,
    R: mlua::IntoLuaMulti,
    F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
{
    g.set(name, lua.create_function(f)?)
}

fn natives(lua: &Lua, g: &Table, sw: &Sw) -> mlua::Result<()> {
    // The guid string a token names, for the `UnitExists` wrapper.
    set(lua, g, "SW_Guid", |lua, token: Value| {
        Ok(unit(lua, &token).map(guid_string))
    })?;

    // The spell id of `UnitBuff`'s (helpful) or `UnitDebuff`'s `index`-th aura.
    set(
        lua,
        g,
        "SW_AuraId",
        |lua, (token, index, helpful): (Value, Value, Value)| {
            let (Value::String(t), Some(i)) = (&token, number(&index)) else {
                return Ok(None);
            };
            let helpful = !matches!(helpful, Value::Nil | Value::Boolean(false));
            Ok(unit_aura_spell_id(lua, &t.to_str()?, i as i64, helpful))
        },
    )?;

    // GetPlayerBuffID(buffIndex): the spell id at `GetPlayerBuff`'s cache position.
    set(lua, g, "GetPlayerBuffID", |lua, index: Value| {
        Ok(number(&index).and_then(|i| player_buff_spell_id(lua, i as i64)))
    })?;

    // SpellInfo(id): name, rank, texture, min range, max range.
    let s = sw.clone();
    set(lua, g, "SpellInfo", move |lua, id: Value| {
        match number(&id).and_then(|id| s.spells.info(id as u32)) {
            Some(info) => info.into_lua_multi(lua),
            None => Ok(MultiValue::new()),
        }
    })?;

    // UnitPosition(unit): x, y, z of a friendly unit, WoW's axes.
    let s = sw.clone();
    set(lua, g, "UnitPosition", move |lua, token: Value| {
        let Some(guid) = unit(lua, &token) else {
            return Ok(MultiValue::new());
        };
        let pos = s
            .lock()
            .units
            .get(&guid)
            .filter(|u| !u.hostile)
            .and_then(|u| u.pos);
        match pos {
            Some([x, y, z]) => (x, y, z).into_lua_multi(lua),
            None => Ok(MultiValue::new()),
        }
    })?;

    // CanLootUnit(unit): 1 while the unit has loot for us.
    let s = sw.clone();
    set(lua, g, "CanLootUnit", move |lua, token: Value| {
        let Some(guid) = unit(lua, &token) else {
            return Ok(None);
        };
        Ok(flag(s.lock().units.get(&guid).is_some_and(|u| u.lootable)))
    })?;

    let s = sw.clone();
    set(lua, g, "IsSwimming", move |_, ()| {
        Ok(flag(s.lock().move_flags & crate::MOVEFLAG_SWIMMING != 0))
    })?;

    let s = sw.clone();
    set(lua, g, "IsMounted", move |_, ()| {
        let st = s.lock();
        Ok(flag(st.units.get(&st.player).is_some_and(|u| u.mounted)))
    })?;

    // GetSpeed(): run and swim speed, yd/s.
    let s = sw.clone();
    set(lua, g, "GetSpeed", move |_, ()| Ok(s.lock().speeds))?;

    let s = sw.clone();
    set(lua, g, "SetAutoloot", move |_, v: Value| {
        Ok(switch(&mut s.lock().autoloot, &v))
    })?;

    let s = sw.clone();
    set(lua, g, "Clickthrough", move |_, v: Value| {
        Ok(switch(&mut s.lock().clickthrough, &v))
    })?;

    let s = sw.clone();
    set(lua, g, "TrackUnit", move |lua, token: Value| {
        let guid = unit(lua, &token);
        let mut st = s.lock();
        let Some(g) = guid.filter(|g| st.units.get(g).is_some_and(|u| !u.hostile)) else {
            return Ok(None);
        };
        st.tracked.insert(g);
        Ok(Some(1))
    })?;

    let s = sw.clone();
    set(lua, g, "UntrackUnit", move |lua, token: Value| {
        if matches!(&token, Value::String(t) if t.to_str().is_ok_and(|t| t.eq_ignore_ascii_case("all")))
        {
            s.lock().tracked.clear();
            return Ok(Some(1));
        }
        let guid = unit(lua, &token);
        Ok(flag(guid.is_some_and(|g| s.lock().tracked.remove(&g))))
    })?;

    // SetMouseoverUnit(unit): the hovered frame's unit; no argument clears it.
    let s = sw.clone();
    set(lua, g, "SetMouseoverUnit", move |lua, token: Value| {
        let guid = unit(lua, &token);
        s.lock().mouseover = guid;
        Ok(())
    })?;

    // The local raid mark: `index` 1-8 on the unit, 0 off it.
    let s = sw.clone();
    set(
        lua,
        g,
        "SW_SetLocalMark",
        move |lua, (token, index): (Value, Value)| {
            let Some(guid) = unit(lua, &token) else {
                return Ok(());
            };
            let index = number(&index).unwrap_or(0.0) as i64;
            let mut st = s.lock();
            if (1..=8).contains(&index) {
                st.set_local_mark(index as u8 - 1, guid);
            } else if let Some(i) = st.marks.iter().position(|m| *m == guid) {
                st.set_local_mark(i as u8, 0);
            }
            Ok(())
        },
    )?;

    // A book spell, by id or name, cast at the unit a token names: true when sent.
    let s = sw.clone();
    set(
        lua,
        g,
        "SW_CastAt",
        move |lua, (spell, token): (Value, Value)| {
            let Some(guid) = unit(lua, &token) else {
                return Ok(false);
            };
            let mut st = s.lock();
            let id = match &spell {
                Value::String(name) => s.spells.in_book(&st.book, &name.to_str()?),
                other => number(other).map(|n| n as u32),
            };
            let Some(id) = id else {
                return Ok(false);
            };
            st.cast(id, Some(guid));
            Ok(true)
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests;

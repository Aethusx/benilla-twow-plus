//! `AtCursor.cpp`, `AtUnit.cpp` and `CastNoToggle.cpp`: `C_Spell.CastAtCursor`,
//! `C_Spell.CastAtUnit` and `CastSpellNoToggle`.
//!
//! The DLL dispatches the cast and resolves its placement inside the call. benilla sends a
//! crate's cast through its ladder the next frame (an [`ExtCast`] with its [`ExtPlace`]), as
//! benilla's own script casts are applied after the script runs. Deviation: each return is
//! decided at the call from what is known then — the spell is in the book, the unit has a
//! position, the spell wants a ground point (`Targets & 0x60`, the word's location bits) — and
//! not from the outcome; a cursor off the terrain, or a unit the spell refuses, cancels the cursor
//! as in the DLL, but the call has already answered.

use benilla_app::ext::{ExtCast, ExtPlace};
use mlua::{Function, Lua, Value};

use super::{find_book_slot, name_to_spell_id};
use crate::lua::unit::unit_guid;
use crate::lua::{is_number, to_int, to_str, truthy, Api};
use crate::mirror::field;
use crate::spells::{self, col};
use crate::Ca;

/// `TARGET_FLAG_SOURCE_LOCATION | TARGET_FLAG_DEST_LOCATION`.
const TARGETS_LOCATION: u32 = 0x60;
/// `SPELL_ATTR_EX2_AUTOREPEAT_FLAG`.
const ATTR_EX2_AUTOREPEAT: u32 = 0x20;
const EFFECT_APPLY_AURA: u32 = 6;
/// `SPELL_AURA_MOD_SHAPESHIFT`.
const AURA_MOD_SHAPESHIFT: u32 = 36;

fn wants_location(ca: &Ca, spell: u32) -> bool {
    spells::table(&ca.db)
        .and_then(|t| {
            t.row(spell)
                .map(|r| r.u32(col::TARGETS) & TARGETS_LOCATION != 0)
        })
        .unwrap_or(false)
}

/// `DispatchSpellCast` / `DispatchSpellCastByName`: an exact id must be in the book; a name goes
/// through the engine's resolver (`(Rank N)`-aware, highest rank otherwise). 0 for neither.
fn book_spell(lua: &Lua, v: &Value) -> u32 {
    let id = if is_number(v) {
        let id = to_int(v);
        if find_book_slot(lua, id).is_none() {
            return 0;
        }
        id
    } else {
        match to_str(v) {
            Some(name) => name_to_spell_id(lua, &name),
            None => 0,
        }
    };
    u32::try_from(id).unwrap_or(0)
}

fn queue(ca: &Ca, spell_id: u32, target: Option<u64>, place: Option<ExtPlace>) {
    ca.lock().casts.push(ExtCast {
        spell_id,
        target,
        item: None,
        place,
    });
}

/// `AtUnit::CastCore`: the unit must resolve and have a position; a unit-target spell goes at
/// its guid, a ground spell is placed at its feet unless `place` is false.
fn cast_at_unit(lua: &Lua, ca: &Ca, spell: u32, token: &str, place: bool) -> mlua::Result<bool> {
    if spell == 0 {
        return Ok(false);
    }
    let Some(guid) = unit_guid(lua, token)? else {
        return Ok(false);
    };
    let Some(pos) = ca.lock().mirror.place(guid).map(|p| p.pos) else {
        return Ok(false);
    };
    let how = if place {
        ExtPlace::At(pos)
    } else {
        ExtPlace::KeepGround
    };
    queue(ca, spell, Some(guid), Some(how));
    Ok(true)
}

/// The third argument of `CastAtUnit` and `CastSpellNoToggle`: omitted or nil places a ground
/// spell, only an explicit false holds the reticle.
fn place_arg(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Nil) => true,
        Some(v) => truthy(v),
    }
}

/// `NameMatches`: case-insensitive, the input's `(Rank N)` and the spaces before it ignored.
fn name_matches(input: &str, name: &str) -> bool {
    let input = input.split('(').next().unwrap_or("").trim_end();
    !name.is_empty() && input.eq_ignore_ascii_case(name)
}

/// The dispatcher's toggle test (`FUN_SPELL_IS_TOGGLE_AURA_ACTIVE`), as benilla's `CastSpell`
/// dispatcher makes it: a spell with an active icon whose own aura is live and cancelable, or a
/// shapeshift spell into the form the player is in.
fn toggle_active(ca: &Ca, spell: u32) -> bool {
    let Some(table) = spells::table(&ca.db) else {
        return false;
    };
    let Some(rec) = table.row(spell) else {
        return false;
    };
    let st = ca.lock();
    let Some(me) = st.mirror.me() else {
        return false;
    };
    if rec.u32(col::ACTIVE_ICON_ID) != 0
        && (0..crate::aura::AURA_TOTAL).any(|slot| {
            me.u32(field::UNIT_AURA + slot) == spell
                && (me.u32(field::UNIT_AURA_FLAGS + slot / 8) >> ((slot % 8) * 4)) & 1 != 0
        })
    {
        return true;
    }
    let form = u32::from(me.byte(field::UNIT_BYTES_1, 2));
    form != 0
        && (0..crate::spells::EFFECTS).any(|i| {
            rec.u32(col::EFFECT + i) == EFFECT_APPLY_AURA
                && rec.u32(col::EFFECT_APPLY_AURA_NAME + i) == AURA_MOD_SHAPESHIFT
                && rec.u32(col::EFFECT_MISC_VALUE + i) == form
        })
}

fn spell_name(ca: &Ca, spell: u32) -> String {
    spells::table(&ca.db)
        .and_then(|t| t.row(spell).map(|r| r.loc(col::NAME).to_string()))
        .unwrap_or_default()
}

fn is_auto_repeat(ca: &Ca, spell: u32) -> bool {
    spells::table(&ca.db)
        .and_then(|t| {
            t.row(spell)
                .map(|r| r.u32(col::ATTRIBUTES_EX2) & ATTR_EX2_AUTOREPEAT != 0)
        })
        .unwrap_or(false)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let cast_by_name: Option<Function> = api.lua.globals().get("CastSpellByName").ok();

    let c = api.ca.clone();
    api.table("C_Spell", "CastAtCursor", move |lua, v: Value| {
        let spell = book_spell(lua, &v);
        if spell == 0 {
            return Ok(false);
        }
        queue(&c, spell, None, Some(ExtPlace::Cursor));
        Ok(wants_location(&c, spell))
    })?;

    let c = api.ca.clone();
    api.table(
        "C_Spell",
        "CastAtUnit",
        move |lua, args: mlua::MultiValue| {
            let a: Vec<Value> = args.into_iter().collect();
            let (Some(spell_v), Some(Value::String(token))) = (a.first(), a.get(1)) else {
                return Ok(false);
            };
            if !is_number(spell_v) && !matches!(spell_v, Value::String(_)) {
                return Ok(false);
            }
            let token = token.to_str()?.to_string();
            let spell = book_spell(lua, spell_v);
            cast_at_unit(lua, &c, spell, &token, place_arg(a.get(2)))
        },
    )?;

    let c = api.ca.clone();
    api.global("CastSpellNoToggle", move |lua, args: mlua::MultiValue| {
        let a: Vec<Value> = args.into_iter().collect();
        let first = a.first().cloned().unwrap_or(Value::Nil);
        let (requested, name) = match &first {
            Value::Integer(_) | Value::Number(_) => {
                let id = to_int(&first);
                let name = u32::try_from(id).map_or(String::new(), |id| spell_name(&c, id));
                if id <= 0 || name.is_empty() {
                    return Ok(false);
                }
                (id as u32, name)
            }
            Value::String(s) => {
                // The macro `!Name` prefix is what this call means; the name behind it is cast.
                let s = s.to_str()?.to_string();
                let t = s.trim_start_matches([' ', '\t']);
                let name = match t.strip_prefix('!') {
                    Some(rest) if !rest.trim_start().is_empty() => rest.trim_start().to_string(),
                    _ => s.clone(),
                };
                if name.is_empty() {
                    return Ok(false);
                }
                (0, name)
            }
            _ => return Err(mlua::Error::runtime(
                "Usage: CastSpellNoToggle(\"name\" | spellID [, \"unit\" [, placeGroundSpell]])",
            )),
        };
        let unit = match a.get(1) {
            Some(Value::String(u)) if !u.as_bytes().is_empty() => Some(u.to_str()?.to_string()),
            _ => None,
        };
        let place = place_arg(a.get(2));
        // An auto-repeat already running: the same one is spam-safe, another is left alone.
        let active = c.lock().mirror.auto_repeat;
        if active != 0 {
            let same = if requested > 0 {
                active == requested
            } else {
                name_matches(&name, &spell_name(&c, active))
            };
            return Ok(same);
        }
        let spell = u32::try_from(name_to_spell_id(lua, &name)).unwrap_or(0);
        if spell != 0 && toggle_active(&c, spell) {
            return Ok(true);
        }
        match unit {
            Some(token) => {
                cast_at_unit(lua, &c, spell, &token, place)?;
            }
            None => {
                if let Some(f) = &cast_by_name {
                    f.call::<()>(name)?;
                }
            }
        }
        // The post-cast state: an auto-repeat spell is running once sent; a form or aura shows
        // only when the server applies it, after this call.
        Ok(spell != 0 && is_auto_repeat(&c, spell))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rank_suffix_and_case_do_not_stop_a_name_match() {
        assert!(name_matches("shoot(Rank 1)", "Shoot"));
        assert!(name_matches("Auto Shot ", "Auto Shot"));
        assert!(!name_matches("Shoo", "Shoot"));
        assert!(place_arg(None));
        assert!(place_arg(Some(&Value::Nil)));
        assert!(!place_arg(Some(&Value::Boolean(false))));
    }
}

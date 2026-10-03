//! The unit's body and bearing: `ShapeshiftForm.cpp`, `Sheath.cpp`, `Speed.cpp`,
//! `SpellHaste.cpp`, `StandState.cpp`, `SubName.cpp` and `ClosestPosition.cpp`.

use mlua::{IntoLuaMulti, Lua, Value};

use super::token_arg;
use super::world::held;
use crate::lua::{is_number, to_int, Api};
use crate::mirror::{field, Mirror};
use crate::spells::{self, col};

/// `SPELL_AURA_MOD_SHAPESHIFT`.
const AURA_MOD_SHAPESHIFT: i32 = 36;

/// `ReadCurrentForm`: the player's `UNIT_FIELD_BYTES_1` byte 2; `None` before the descriptor.
pub(crate) fn current_form(m: &Mirror) -> Option<u8> {
    m.me().map(|f| f.byte(field::UNIT_BYTES_1, 2))
}

/// `Object::ClosestByEntry::Find`: the nearest held object of `kind` whose guid packs `entry`,
/// `(x, y, squared distance)` in WoW's axes.
pub(crate) fn closest_by_entry(
    m: &Mirror,
    kind: crate::guid::Kind,
    entry: u32,
) -> Option<(f32, f32, f32)> {
    if entry == 0 {
        return None;
    }
    let me = m.place(m.player)?.pos;
    m.places
        .iter()
        .filter(|(g, _)| {
            crate::guid::classify(**g) == kind && ((**g >> 24) & 0xFF_FFFF) as u32 == entry
        })
        .map(|(_, p)| {
            let d2: f32 = (0..3).map(|i| (p.pos[i] - me[i]).powi(2)).sum();
            (p.pos[0], p.pos[1], d2)
        })
        .reduce(|a, b| if b.2 < a.2 { b } else { a })
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `SpellShapeshiftForm.dbc` ids, 0 unshifted; 1.12's numbering, not retail's.
    let c = api.ca.clone();
    api.global("GetShapeshiftFormID", move |_, ()| {
        Ok(current_form(&c.lock().mirror).unwrap_or(0))
    })?;

    // The buff whose shapeshift aura names the current form, cancelled; warrior stances the
    // server refuses.
    let c = api.ca.clone();
    api.global("CancelShapeshiftForm", move |_, ()| {
        let Some(table) = spells::table(&c.db) else {
            return Ok(());
        };
        let mut st = c.lock();
        let Some(form) = current_form(&st.mirror).filter(|f| *f > 0) else {
            return Ok(());
        };
        let Some(me) = st.mirror.me() else {
            return Ok(());
        };
        let hit = (0..32)
            .map(|s| me.u32(field::UNIT_AURA + s))
            .filter(|id| *id != 0)
            .find(|id| {
                table.row(*id).is_some_and(|r| {
                    (0..spells::EFFECTS).any(|e| {
                        r.i32(col::EFFECT_APPLY_AURA_NAME + e) == AURA_MOD_SHAPESHIFT
                            && r.i32(col::EFFECT_MISC_VALUE + e) == i32::from(form)
                    })
                })
            });
        if let Some(id) = hit {
            st.cancel_aura(id);
        }
        Ok(())
    })?;

    // 1 sheathed, 2 melee, 3 ranged: `UNIT_FIELD_BYTES_2` byte 0, the sheath the client shows.
    let c = api.ca.clone();
    api.global("GetSheathState", move |_, ()| {
        let state = c
            .lock()
            .mirror
            .me()
            .map_or(0, |f| f.byte(field::UNIT_BYTES_2, 0));
        Ok(if state > 2 { 1 } else { state + 1 })
    })?;

    // `currentSpeed, runSpeed, flightSpeed, swimSpeed`: the speed this frame's step takes (0
    // standing), the forward run and swim speeds; zeros for a unit not held.
    let c = api.ca.clone();
    api.global("GetUnitSpeed", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: GetUnitSpeed(\"unit\")")?;
        let speeds = match held(lua, &c, &token)? {
            Some((guid, _)) => {
                let st = c.lock();
                let current = st
                    .mirror
                    .range_units
                    .get(&guid)
                    .map_or(0.0, |u| u.motion.speed);
                let (run, swim) = st
                    .mirror
                    .speeds
                    .get(&guid)
                    .map_or((0.0, 0.0), |s| (s.run, s.swim));
                (current, run, 0.0f32, swim)
            }
            None => (0.0, 0.0, 0.0, 0.0),
        };
        Ok(speeds)
    })?;

    // The haste percentage of `UNIT_MOD_CAST_SPEED`, `(1 / mult - 1) * 100`.
    let c = api.ca.clone();
    api.global("UnitSpellHaste", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitSpellHaste(\"unit\")")?;
        let Some((_, f)) = held(lua, &c, &token)? else {
            return Ok(0.0);
        };
        let mult = f64::from(f.f32(field::UNIT_MOD_CAST_SPEED));
        Ok(if mult > 0.0 {
            (1.0 / mult - 1.0) * 100.0
        } else {
            0.0
        })
    })?;

    // `UNIT_FIELD_BYTES_1` byte 0, 0 standing; 0 for a unit not held.
    let c = api.ca.clone();
    api.global("UnitStandState", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitStandState(\"unit\")")?;
        Ok(held(lua, &c, &token)?.map_or(0, |(_, f)| f.byte(field::UNIT_BYTES_1, 0)))
    })?;

    // The creature template's subname, the tooltip's second line.
    api.global("UnitSubName", |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitSubName(\"unit\")")?;
        Ok(benilla_ui::script::ext_read::unit_state(lua, &token)?
            .filter(|s| s.has_object)
            .and_then(|s| s.subtitle)
            .filter(|s| !s.is_empty()))
    })?;

    // The nearest visible creature of that template, `x, y, distance`; nothing for none.
    let c = api.ca.clone();
    api.global("ClosestUnitPosition", move |lua: &Lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: ClosestUnitPosition(creatureID)",
            ));
        }
        let hit = closest_by_entry(
            &c.lock().mirror,
            crate::guid::Kind::Creature,
            to_int(&v) as u32,
        );
        match hit {
            Some((x, y, d2)) => (x, y, f64::from(d2).sqrt()).into_lua_multi(lua),
            None => Ok(mlua::MultiValue::new()),
        }
    })?;
    Ok(())
}

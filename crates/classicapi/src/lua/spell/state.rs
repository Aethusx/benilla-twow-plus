//! The player's live spell state: `Cancel.cpp`, `IsCurrent.cpp`, `Cooldown.cpp` and
//! `CastCount.cpp`.

use std::time::Instant;

use mlua::Value;

use super::{bank_is_pet, book_slot_to_id, resolve_spell_id};
use crate::lua::{is_number, to_int, Api};
use crate::mirror::field;
use crate::spells::{self, col};
use crate::Ca;

/// `UNIT_AURA_BUFF_COUNT`, the helpful slots 0-31.
const BUFF_SLOTS: usize = 32;

/// `Item::Count::InInventory`: the plain stack count of `item_id` worn and in bags 0-4.
pub(crate) fn carried(ca: &Ca, item_id: u32) -> u32 {
    if item_id == 0 {
        return 0;
    }
    let st = ca.lock();
    let m = &st.mirror;
    let worn: u32 = crate::items::equipped(m)
        .iter()
        .filter(|(_, it)| it.entry == item_id)
        .map(|(_, it)| it.stack())
        .sum();
    let bagged: u32 = crate::items::bagged(m, 0..=4)
        .iter()
        .filter(|(_, _, it)| it.entry == item_id)
        .map(|(_, _, it)| it.stack())
        .sum();
    worn + bagged
}

/// `ForSpell`: the casts the carried reagents allow, the least over the reagent slots of
/// carried / required; 0 for an unknown spell or one with no reagents.
fn cast_count(ca: &Ca, spell_id: i64) -> u32 {
    let Some(table) = spells::table(&ca.db) else {
        return 0;
    };
    let Some(rec) = u32::try_from(spell_id).ok().and_then(|i| table.row(i)) else {
        return 0;
    };
    let mut casts: Option<u32> = None;
    for i in 0..8 {
        let item = rec.u32(col::REAGENT + i);
        if item == 0 {
            break;
        }
        let need = rec.i32(col::REAGENT_COUNT + i);
        if need <= 0 {
            continue;
        }
        let possible = carried(ca, item) / need as u32;
        casts = Some(casts.map_or(possible, |c| c.min(possible)));
    }
    casts.unwrap_or(0)
}

/// `Compute`: `(usable, noMana)`. A pet-book slot, or a spell the player does not know that sits
/// in the pet's book, takes the pet branch; a known spell the bar's own walk; anything else
/// neither.
fn usability(lua: &mlua::Lua, ca: &Ca, spell_id: i64, pet_book: bool) -> (bool, bool) {
    if pet_book {
        return pet_usability(lua, ca, spell_id);
    }
    if super::info::player_knows(ca, spell_id) {
        let mut st = ca.lock();
        st.mirror.usable_wanted_until = Some(Instant::now() + std::time::Duration::from_secs(10));
        return st
            .mirror
            .usable
            .get(&(spell_id as u32))
            .copied()
            .unwrap_or((false, false));
    }
    match super::find_book_slot(lua, spell_id) {
        Some((_, true)) => pet_usability(lua, ca, spell_id),
        _ => (false, false),
    }
}

/// `ComputePet`, the pet branch of `FUN_004E5050`: the pet can act, then its power of the spell's
/// type (health for -2) against the cost, a signed compare.
fn pet_usability(lua: &mlua::Lua, ca: &Ca, spell_id: i64) -> (bool, bool) {
    let Some(table) = spells::table(&ca.db) else {
        return (false, false);
    };
    let Some(rec) = u32::try_from(spell_id).ok().and_then(|i| table.row(i)) else {
        return (false, false);
    };
    if !benilla_ui::script::ext_read::pet_actions_usable(lua) {
        return (false, false);
    }
    let Some(pet) = benilla_ui::script::unit_token_guid_in(lua, "pet") else {
        return (false, false);
    };
    let st = ca.lock();
    let (Some(pet), Some(me)) = (st.mirror.object(pet), st.mirror.me()) else {
        return (false, false);
    };
    let power = match rec.i32(col::POWER_TYPE) {
        -2 => pet.i32(field::UNIT_HEALTH),
        t @ 0..=4 => pet.i32(field::UNIT_POWER1 + t as usize),
        _ => return (false, false),
    };
    let family = crate::spellmod::player_family(&ca.db, &st.mirror);
    let cost = spells::power_cost(&rec, me, &st.mods, family) as i32;
    if cost <= power {
        (true, false)
    } else {
        (false, true)
    }
}

/// `SPELL_AURA_TRACK_CREATURES`, `_RESOURCES` and `_STEALTHED`.
const TRACK_AURAS: [i32; 3] = [44, 45, 151];

/// `EnumerateTracking`: the player book's tracking spells in slot order, `(spell, slot)`.
fn tracking_spells(lua: &mlua::Lua, ca: &Ca) -> Vec<(u32, usize)> {
    let Some(table) = spells::table(&ca.db) else {
        return Vec::new();
    };
    let (book, _) = benilla_ui::script::ext_read::spellbook_ids(lua);
    book.into_iter()
        .enumerate()
        .filter(|(_, id)| {
            *id > 0
                && table.row(*id).is_some_and(|r| {
                    (0..spells::EFFECTS)
                        .any(|e| TRACK_AURAS.contains(&r.i32(col::EFFECT_APPLY_AURA_NAME + e)))
                })
        })
        .map(|(i, id)| (id, i + 1))
        .collect()
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let ca = api.ca.clone();

    let c = ca.clone();
    api.global("IsUsableSpell", move |lua, (a1, a2): (Value, Value)| {
        if !is_number(&a1) {
            return Err(mlua::Error::runtime(
                "Usage: IsUsableSpell(spellID) or IsUsableSpell(slot, bookType)",
            ));
        }
        let n = to_int(&a1);
        let (id, pet) = match &a2 {
            Value::String(s) => {
                let pet = s.to_string_lossy().eq_ignore_ascii_case("pet");
                (i64::from(book_slot_to_id(lua, n, pet)), pet)
            }
            _ => (n, false),
        };
        let (usable, oom) = usability(lua, &c, id, pet);
        let one = |b: bool| if b { Value::Integer(1) } else { Value::Nil };
        Ok((one(usable), one(!usable && oom)))
    })?;

    let c = ca.clone();
    api.table("C_Spell", "IsSpellUsable", move |lua, v: Value| {
        let (usable, oom) = usability(lua, &c, resolve_spell_id(lua, &v), false);
        Ok((usable, oom))
    })?;

    let c = ca.clone();
    api.global("GetNumTrackingTypes", move |lua, ()| {
        Ok(tracking_spells(lua, &c).len())
    })?;

    let c = ca.clone();
    api.global("GetTrackingInfo", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: GetTrackingInfo(index)"));
        }
        let index = to_int(&v);
        let list = tracking_spells(lua, &c);
        let Some(&(id, _)) = usize::try_from(index - 1).ok().and_then(|i| list.get(i)) else {
            return Ok(mlua::MultiValue::new());
        };
        let table = spells::table(&c.db);
        let rec = table.as_ref().and_then(|t| t.row(id));
        let name = rec.as_ref().map(|r| r.loc(col::NAME).to_string());
        let texture = rec
            .as_ref()
            .and_then(|r| spells::spell_icon(&c.db, r, false));
        let active = benilla_ui::script::ext_read::active_tracking_spell(lua) == Some(id);
        mlua::IntoLuaMulti::into_lua_multi((name, texture, active, "spell", id), lua)
    })?;

    let c = ca.clone();
    let cast_spell: Option<mlua::Function> = api.lua.globals().get("CastSpell").ok();
    api.global("SetTracking", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: SetTracking(index)"));
        }
        let index = to_int(&v);
        let list = tracking_spells(lua, &c);
        let Some(&(_, slot)) = usize::try_from(index - 1).ok().and_then(|i| list.get(i)) else {
            return Ok(mlua::MultiValue::new());
        };
        // Choosing a tracker is casting it: the engine's own `CastSpell(slot, "spell")`.
        match &cast_spell {
            Some(f) => f.call::<mlua::MultiValue>((slot, "spell")),
            None => Ok(mlua::MultiValue::new()),
        }
    })?;

    // The server is the authority on what cancels; a spell not in `Spell.dbc` sends nothing.
    let c = ca.clone();
    api.table("C_Spell", "CancelSpellByID", move |_, v: Value| {
        if !is_number(&v) {
            return Ok(());
        }
        let id = to_int(&v);
        if spells::table(&c.db)
            .is_some_and(|t| u32::try_from(id).ok().and_then(|i| t.row(i)).is_some())
        {
            c.lock().cancel_aura(id as u32);
        }
        Ok(())
    })?;

    let c = ca.clone();
    api.global("CancelSpellByName", move |_, v: Value| {
        let Value::String(s) = &v else {
            return Ok(());
        };
        let want = s.to_string_lossy();
        if want.is_empty() {
            return Ok(());
        }
        let Some(table) = spells::table(&c.db) else {
            return Ok(());
        };
        let mut st = c.lock();
        let Some(me) = st.mirror.me() else {
            return Ok(());
        };
        let hit = (0..BUFF_SLOTS)
            .map(|slot| me.u32(field::UNIT_AURA + slot))
            .filter(|id| *id != 0)
            .find(|id| {
                table
                    .row(*id)
                    .is_some_and(|r| r.loc(col::NAME).eq_ignore_ascii_case(&want))
            });
        if let Some(id) = hit {
            st.cancel_aura(id);
        }
        Ok(())
    })?;

    // The cast-bar spell, the queued one and the channel.
    let c = ca.clone();
    api.table("C_Spell", "IsCurrentSpell", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        if id <= 0 {
            return Ok(false);
        }
        let st = c.lock();
        let m = &st.mirror;
        let channel = m.me().map_or(0, |me| me.u32(field::UNIT_CHANNEL_SPELL));
        Ok([m.cast_in_flight, channel]
            .into_iter()
            .any(|s| i64::from(s) == id))
    })?;

    let c = ca.clone();
    api.table("C_Spell", "GetSpellCooldown", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        let Some(table) = spells::table(&c.db) else {
            return Ok(Value::Nil);
        };
        let Some(rec) = u32::try_from(id)
            .ok()
            .filter(|i| *i > 0)
            .and_then(|i| table.row(i))
        else {
            return Ok(Value::Nil);
        };
        let (start, duration, enabled) = {
            let st = c.lock();
            let read = crate::cooldown::query(&st.mirror, id as u32, 0, Some(&rec), Instant::now());
            read.triple(&st.mirror)
        };
        let t = lua.create_table()?;
        t.raw_set("startTime", start)?;
        t.raw_set("duration", duration)?;
        t.raw_set("isEnabled", enabled)?;
        t.raw_set("modRate", 1.0)?;
        Ok(Value::Table(t))
    })?;

    let c = ca.clone();
    api.table("C_Spell", "GetSpellCastCount", move |lua, v: Value| {
        Ok(cast_count(&c, resolve_spell_id(lua, &v)))
    })?;

    let c = ca.clone();
    api.table(
        "C_SpellBook",
        "GetSpellBookItemCastCount",
        move |lua, (slot, bank): (Value, Value)| {
            if !is_number(&slot) {
                return Ok(0);
            }
            let id = book_slot_to_id(lua, to_int(&slot), bank_is_pet(&bank));
            Ok(cast_count(&c, i64::from(id)))
        },
    )?;
    Ok(())
}

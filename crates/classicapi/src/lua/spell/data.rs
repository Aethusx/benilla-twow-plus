//! The `Spell.dbc` readers: `Exists.cpp`, `Subtext.cpp`, `School.cpp`, `SchoolString.cpp`,
//! `Mechanic.cpp`, `EffectMechanic.cpp`, `EffectInfo.cpp`, `Reagents.cpp`, `RecordFields.cpp`,
//! `HasRange.cpp`, `Radius.cpp`, `PowerCost.cpp` and `InRange.cpp` with the range core of
//! `Range.cpp`.

use mlua::{IntoLuaMulti, Lua, Value};

use super::{book_slot_to_id, book_type_is_pet, resolve_spell_id};
use crate::dbc::{Databases, Row};
use crate::lua::{is_number, none, to_int, to_str, Api};
use crate::spells::{self, col};
use crate::Ca;

/// The record of a resolved id, through `f`; `None` for an unknown or non-positive id.
fn with_rec<T>(db: &Databases, id: i64, f: impl FnOnce(&Row) -> T) -> Option<T> {
    let table = spells::table(db)?;
    let rec = table.row(u32::try_from(id).ok()?)?;
    Some(f(&rec))
}

/// `Spell.dbc` school names, locale-independent, by the 0-based school.
const SCHOOL_NAMES: [&str; 7] = [
    "Physical", "Holy", "Fire", "Nature", "Frost", "Shadow", "Arcane",
];

/// `SPELL_ATTR_ON_NEXT_SWING` and `_2`.
const ATTR_ON_NEXT_SWING: u32 = 0x04 | 0x400;
/// `SPELL_INTERRUPT_FLAG_AUTOATTACK`.
const INTERRUPT_AUTOATTACK: u32 = 0x08;
/// `SPELL_ATTR_EX2_NOT_RESET_AUTO_ACTIONS`.
const ATTR_EX2_NOT_RESET_AUTO_ACTIONS: u32 = 0x20000;

/// `Spell::Range::SpellIDHasRange`: the `SpellRange.dbc` row has a nonzero min or max.
pub(crate) fn has_range(db: &Databases, id: i64) -> bool {
    if id <= 0 {
        return false;
    }
    with_rec(db, id, |rec| {
        let idx = rec.u32(col::RANGE_INDEX);
        idx != 0
            && db
                .get("SpellRange")
                .and_then(|t| t.row(idx).map(|r| r.f32(1) > 0.0 || r.f32(2) > 0.0))
                .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// `Spell::Range::PlayerVsUnit`: `Some(true)` in range, `Some(false)` out, `None` when the check
/// does not apply (no spell, a rangeless one, no unit, or a position that cannot be read). The
/// engine core `IsTargetInRange 0x6e47b0` tests the squared 3D distance against
/// `GetMinMaxRange`'s bounds, the maximum taking `SPELLMOD_RANGE` unless the spell is on-next-swing.
pub(crate) fn player_vs_unit(ca: &Ca, spell_id: i64, guid: Option<u64>) -> Option<bool> {
    if spell_id <= 0 || !has_range(&ca.db, spell_id) {
        return None;
    }
    let guid = guid?;
    let catalog = spells::catalog(&ca.db)?;
    let ranges = spells::range_catalog(&ca.db)?;
    let d = catalog.get(spell_id as u32)?;
    let st = ca.lock();
    let m = &st.mirror;
    let me = m.place(m.player)?.pos;
    let them = m.place(guid)?.pos;
    let targets = benilla_formats::RangeTargets {
        target: m.range_units.get(&guid).copied(),
        attack_target: m.caster_attack_target,
    };
    let caster = m.caster?;
    let (min, mut max) =
        benilla_formats::min_max_range(d, ranges.get(d.range_index), caster, targets)?;
    if !d.on_next_swing() {
        let family = crate::spellmod::player_family(&ca.db, m);
        let table = spells::table(&ca.db)?;
        let rec = table.row(spell_id as u32)?;
        max = crate::spellmod::apply(&st.mods, family, &rec, crate::spellmod::OP_RANGE, max);
    }
    let d2: f32 = (0..3).map(|i| (me[i] - them[i]).powi(2)).sum();
    Some(d2 >= min * min && d2 <= max * max)
}

/// `MaxBaseRadius`: the largest `SpellRadius.dbc` base radius over the three effects.
fn max_base_radius(db: &Databases, rec: &Row) -> Option<f32> {
    let radii = db.get("SpellRadius")?;
    (0..spells::EFFECTS)
        .filter_map(|i| radii.row(rec.u32(col::EFFECT_RADIUS_INDEX + i)))
        .map(|r| r.f32(1))
        .reduce(f32::max)
}

/// `PushRadiusForSpellID`: the base radius and the player's modified one; nothing for no area.
fn radius(lua: &Lua, ca: &Ca, id: i64) -> mlua::Result<mlua::MultiValue> {
    let Some(table) = spells::table(&ca.db) else {
        return Ok(none());
    };
    let Some(rec) = u32::try_from(id).ok().and_then(|i| table.row(i)) else {
        return Ok(none());
    };
    let Some(base) = max_base_radius(&ca.db, &rec) else {
        return Ok(none());
    };
    let st = ca.lock();
    let family = crate::spellmod::player_family(&ca.db, &st.mirror);
    let modified = crate::spellmod::apply(&st.mods, family, &rec, crate::spellmod::OP_RADIUS, base);
    (base, modified).into_lua_multi(lua)
}

/// The `(slot, bookType)` globals' book: `"pet"` case-insensitively, anything else the player's.
fn slot_book_id(lua: &Lua, slot: &Value, book: &Value) -> Option<i64> {
    if !is_number(slot) {
        return None;
    }
    let pet = to_str(book).is_some_and(|b| book_type_is_pet(&b));
    Some(i64::from(book_slot_to_id(lua, to_int(slot), pet)))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let ca = api.ca.clone();

    let d = ca.db.clone();
    api.table("C_Spell", "DoesSpellExist", move |_, v: Value| {
        if !is_number(&v) {
            return Ok(false);
        }
        let id = to_int(&v);
        Ok(id > 0 && with_rec(&d, id, |_| ()).is_some())
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "GetSpellSubtext", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        if id <= 0 {
            return Ok(None);
        }
        // `DBC::LocalizedField`: the locale slot's pointer, nil only for a missing record.
        Ok(with_rec(&d, id, |r| r.loc(col::RANK).to_string()))
    })?;

    let d = ca.db.clone();
    api.global("GetSpellSchool", move |lua, v: Value| {
        if !is_number(&v) {
            return Ok(none());
        }
        let school = with_rec(&d, to_int(&v), |r| r.u32(col::SCHOOL));
        match school.filter(|s| (*s as usize) < SCHOOL_NAMES.len()) {
            Some(s) => (s + 1, SCHOOL_NAMES[s as usize]).into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    api.table("C_Spell", "GetSchoolString", |lua, v: Value| {
        if !is_number(&v) {
            return Ok("Unknown".to_string());
        }
        let mask = to_int(&v) as u32;
        if mask == 0 || mask & (mask - 1) != 0 {
            return Ok("Unknown".to_string());
        }
        let Some(idx) = (0..7).find(|i| mask == 1 << i) else {
            return Ok("Unknown".to_string());
        };
        // `PushLocalizedString`: the global when it is a string, else the fallback.
        match lua
            .globals()
            .get::<Value>(format!("SPELL_SCHOOL{idx}_CAP"))?
        {
            Value::String(s) => Ok(s.to_string_lossy()),
            _ => Ok("Unknown".to_string()),
        }
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "GetSpellMechanicByID", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        let Some(mechanic) = with_rec(&d, id, |r| r.u32(col::MECHANIC)) else {
            return Ok(none());
        };
        if mechanic == 0 {
            return (0, Value::Nil).into_lua_multi(lua);
        }
        // The enUS name, a stable token whatever the client's locale.
        let name = d
            .get("SpellMechanic")
            .and_then(|t| t.row(mechanic).map(|r| r.str(1).to_string()))
            .filter(|s| !s.is_empty());
        (mechanic, name).into_lua_multi(lua)
    })?;

    let d = ca.db.clone();
    api.table(
        "C_Spell",
        "GetSpellEffectMechanics",
        move |lua, v: Value| {
            let id = resolve_spell_id(lua, &v);
            let Some(m) = with_rec(&d, id, |r| {
                [0, 1, 2].map(|i| r.u32(col::EFFECT_MECHANIC + i))
            }) else {
                return Ok(Value::Nil);
            };
            let t = lua.create_table()?;
            for (i, x) in m.into_iter().enumerate() {
                t.raw_set(i + 1, x)?;
            }
            Ok(Value::Table(t))
        },
    )?;

    let d = ca.db.clone();
    api.table("C_Spell", "GetSpellEffectInfo", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        let table = spells::table(&d);
        let Some(rec) = table
            .as_ref()
            .and_then(|t| u32::try_from(id).ok().and_then(|i| t.row(i)))
        else {
            return Ok(Value::Nil);
        };
        let out = lua.create_table()?;
        for i in 0..spells::EFFECTS {
            let fx = lua.create_table()?;
            fx.raw_set("effect", rec.i32(col::EFFECT + i))?;
            fx.raw_set("auraName", rec.i32(col::EFFECT_APPLY_AURA_NAME + i))?;
            fx.raw_set("basePoints", rec.i32(col::EFFECT_BASE_POINTS + i))?;
            fx.raw_set("baseDice", rec.i32(col::EFFECT_BASE_DICE + i))?;
            fx.raw_set("dieSides", rec.i32(col::EFFECT_DIE_SIDES + i))?;
            fx.raw_set(
                "pointsPerLevel",
                rec.f32(col::EFFECT_REAL_POINTS_PER_LEVEL + i),
            )?;
            fx.raw_set("dicePerLevel", rec.f32(col::EFFECT_DICE_PER_LEVEL + i))?;
            fx.raw_set("miscValue", rec.i32(col::EFFECT_MISC_VALUE + i))?;
            out.raw_set(i + 1, fx)?;
        }
        Ok(Value::Table(out))
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "GetSpellReagents", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        if id <= 0 {
            return Ok(Value::Nil);
        }
        let Some(reagents) = with_rec(&d, id, |r| {
            (0..8)
                .map(|i| (r.i32(col::REAGENT + i), r.i32(col::REAGENT_COUNT + i)))
                .take_while(|(item, _)| *item != 0)
                .collect::<Vec<_>>()
        }) else {
            return Ok(Value::Nil);
        };
        let out = lua.create_table()?;
        for (i, (item, count)) in reagents.into_iter().enumerate() {
            let e = lua.create_table()?;
            e.raw_set("itemID", item)?;
            e.raw_set("count", count)?;
            out.raw_set(i + 1, e)?;
        }
        Ok(Value::Table(out))
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "GetSpellDispelType", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        Ok(with_rec(&d, id, |r| r.u32(col::DISPEL)).unwrap_or(0))
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "IsNextMeleeSpell", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        Ok(with_rec(&d, id, |r| r.u32(col::ATTRIBUTES) & ATTR_ON_NEXT_SWING != 0).unwrap_or(false))
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "ResetsMeleeSwing", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        Ok(with_rec(&d, id, |r| {
            r.u32(col::INTERRUPT_FLAGS) & INTERRUPT_AUTOATTACK != 0
                && r.u32(col::ATTRIBUTES_EX2) & ATTR_EX2_NOT_RESET_AUTO_ACTIONS == 0
        })
        .unwrap_or(false))
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "SpellHasRange", move |lua, v: Value| {
        Ok(has_range(&d, resolve_spell_id(lua, &v)))
    })?;
    let d = ca.db.clone();
    api.global("SpellHasRange", move |lua, (slot, book): (Value, Value)| {
        Ok(slot_book_id(lua, &slot, &book).is_some_and(|id| has_range(&d, id)))
    })?;

    let c = ca.clone();
    api.table("C_Spell", "GetSpellRadius", move |lua, v: Value| {
        radius(lua, &c, resolve_spell_id(lua, &v))
    })?;
    let c = ca.clone();
    api.global(
        "GetSpellRadius",
        move |lua, (slot, book): (Value, Value)| match slot_book_id(lua, &slot, &book) {
            Some(id) => radius(lua, &c, id),
            None => Ok(none()),
        },
    )?;

    let c = ca.clone();
    api.table("C_Spell", "GetSpellPowerCost", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        let Some(table) = spells::table(&c.db) else {
            return Ok(Value::Nil);
        };
        let Some(rec) = u32::try_from(id).ok().and_then(|i| table.row(i)) else {
            return Ok(Value::Nil);
        };
        let cost = {
            let st = c.lock();
            // No player context: the engine's cost helper answers 0xFFFFFFFF.
            let Some(me) = st.mirror.me() else {
                return Ok(Value::Nil);
            };
            let family = crate::spellmod::player_family(&c.db, &st.mirror);
            spells::power_cost(&rec, me, &st.mods, family)
        };
        let power_type = rec.i32(col::POWER_TYPE);
        let pct = rec.i32(col::MANA_COST_PERCENT);
        if cost == 0 && pct == 0 {
            return Ok(Value::Nil);
        }
        let e = lua.create_table()?;
        e.raw_set("type", power_type)?;
        e.raw_set("name", spells::power_token(i64::from(power_type)))?;
        e.raw_set("cost", cost)?;
        e.raw_set("minCost", cost)?;
        e.raw_set("costPercent", pct)?;
        e.raw_set("costPerSec", 0)?;
        e.raw_set("requiredAuraID", 0)?;
        e.raw_set("hasRequiredAura", false)?;
        let out = lua.create_table()?;
        out.raw_set(1, e)?;
        Ok(Value::Table(out))
    })?;

    let c = ca.clone();
    api.table(
        "C_Spell",
        "IsSpellInRange",
        move |lua, (v, unit): (Value, Value)| {
            let id = resolve_spell_id(lua, &v);
            let guid = to_str(&unit).and_then(|t| benilla_ui::script::unit_token_guid_in(lua, &t));
            Ok(player_vs_unit(&c, id, guid))
        },
    )?;
    Ok(())
}

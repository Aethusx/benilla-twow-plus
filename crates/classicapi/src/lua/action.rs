//! `action/Info.cpp`, `cursor/Info.cpp` and `spell/BonusDamage.cpp`: `GetActionInfo`,
//! `GetCursorInfo`, `GetSpellBonusDamage` and `GetSpellBonusHealing`.

use benilla_ui::script::ext_read::{self, CursorView};
use mlua::{IntoLuaMulti, Lua, Value};

use super::item::stats::Accum;
use crate::items;
use crate::lua::{is_number, none, to_int, Api};
use crate::mirror::field;
use crate::spells::{self, col, ATTR_PASSIVE};
use crate::Ca;

const KIND_SPELL: u8 = 0x00;
const KIND_MACRO: u8 = 0x40;
const KIND_ITEM: u8 = 0x80;
/// The action table's 120 slots.
const ACTION_SLOTS: i64 = 120;
/// `SPELL_AURA_MOD_HEALING_DONE`, `_MOD_HEALING_OF_STAT_PERCENT` (× Spirit), and Turtle's
/// `_MOD_HEALING_OF_ARMOR_PERCENT` (× Armor).
const AURA_HEALING_DONE: i32 = 135;
const AURA_HEALING_OF_SPIRIT: i32 = 175;
const AURA_HEALING_OF_ARMOR: i32 = 199;

/// One spell's healing auras, `AddSpellHealingAuras`.
fn spell_healing(
    ca: &Ca,
    spell: u32,
    spirit: i64,
    armor: i64,
    passive_only: bool,
    turtle: bool,
) -> i64 {
    let Some(t) = spells::table(&ca.db) else {
        return 0;
    };
    let Some(r) = t.row(spell) else {
        return 0;
    };
    if passive_only && r.u32(col::ATTRIBUTES) & ATTR_PASSIVE == 0 {
        return 0;
    }
    (0..spells::EFFECTS)
        .map(|i| {
            let amount = i64::from(r.i32(col::EFFECT_BASE_POINTS + i))
                + i64::from(r.i32(col::EFFECT_BASE_DICE + i));
            match r.i32(col::EFFECT_APPLY_AURA_NAME + i) {
                AURA_HEALING_DONE => amount,
                AURA_HEALING_OF_SPIRIT => spirit * amount / 100,
                AURA_HEALING_OF_ARMOR if turtle => armor * amount / 100,
                _ => 0,
            }
        })
        .sum()
}

/// `ComputeHealing`: the worn items' flat healing (template, roll, permanent and temporary
/// enchantments), the player's auras' healing, and the known passives'. Vanilla has no healing
/// field, so the DLL derives it the same way.
fn bonus_healing(lua: &Lua, ca: &Ca) -> i64 {
    let (worn, auras, known, spirit, armor, turtle) = {
        let st = ca.lock();
        let m = &st.mirror;
        let Some(me) = m.me() else {
            return 0;
        };
        let auras: Vec<u32> = (0..crate::aura::AURA_TOTAL)
            .map(|s| me.u32(field::UNIT_AURA + s))
            .filter(|id| *id != 0)
            .collect();
        (
            items::equipped(m),
            auras,
            st.known.clone(),
            i64::from(me.i32(field::UNIT_STAT0 + 4)),
            i64::from(me.i32(field::UNIT_RESISTANCES)),
            st.auras.turtle,
        )
    };
    let mut total = 0;
    for (_, it) in &worn {
        let mut acc = Accum::default();
        if let Some(r) = crate::itemdb::peek(lua, it.entry) {
            acc.record(&ca.db, &r, 1);
        }
        acc.suffix(&ca.db, i64::from(it.random_property()), 1);
        for slot in 0..2 {
            acc.enchant(&ca.db, it.fields.u32(field::ITEM_ENCHANTMENT + 3 * slot), 1);
        }
        total += acc.get("ITEM_MOD_SPELL_HEALING_DONE_SHORT");
    }
    for id in auras {
        total += spell_healing(ca, id, spirit, armor, false, turtle);
    }
    for id in known {
        total += spell_healing(ca, id, spirit, armor, true, turtle);
    }
    total
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `GetActionInfo(slot)` -> `"spell", id, "spell"`, `"macro", index` or `"item", id`.
    api.global("GetActionInfo", |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: GetActionInfo(slot)"));
        }
        let slot = to_int(&v);
        if !(1..=ACTION_SLOTS).contains(&slot) {
            return Value::Nil.into_lua_multi(lua);
        }
        match ext_read::action_slot(lua, slot as u32) {
            Some((KIND_SPELL, id)) if id != 0 => ("spell", id, "spell").into_lua_multi(lua),
            Some((KIND_MACRO, id)) => ("macro", id).into_lua_multi(lua),
            Some((KIND_ITEM, id)) => ("item", id).into_lua_multi(lua),
            _ => Value::Nil.into_lua_multi(lua),
        }
    })?;

    // `GetCursorInfo()`: `"item", id, link`; `"money", copper`; `"spell", slot, book, id`;
    // `"macro", index`; `"merchant", index`; nothing when empty.
    api.global("GetCursorInfo", |lua, ()| {
        let spell = |lua: &Lua, id: u32| -> mlua::Result<mlua::MultiValue> {
            let (slot, pet) =
                super::spell::find_book_slot(lua, i64::from(id)).unwrap_or((0, false));
            ("spell", slot, if pet { "pet" } else { "spell" }, id).into_lua_multi(lua)
        };
        let item = |lua: &Lua, id: u32, link: Option<String>| -> mlua::Result<mlua::MultiValue> {
            if id == 0 {
                return Ok(none());
            }
            let link = link.or_else(|| items::link(lua, id, 0));
            ("item", id, link).into_lua_multi(lua)
        };
        match ext_read::cursor(lua) {
            Some(CursorView::Item { item_id, link }) => item(lua, item_id, link),
            Some(CursorView::Money(c)) => ("money", c).into_lua_multi(lua),
            Some(CursorView::Spell { spell_id, .. }) if spell_id != 0 => spell(lua, spell_id),
            Some(CursorView::Macro(i)) if i != 0 => ("macro", i).into_lua_multi(lua),
            Some(CursorView::Merchant(row)) => ("merchant", row).into_lua_multi(lua),
            Some(CursorView::Action { kind, action }) => match kind {
                KIND_SPELL if action != 0 => spell(lua, action),
                KIND_MACRO => ("macro", action).into_lua_multi(lua),
                KIND_ITEM => item(lua, action, None),
                _ => Ok(none()),
            },
            _ => Ok(none()),
        }
    })?;

    // `GetSpellBonusDamage(school)`: `PLAYER_FIELD_MOD_DAMAGE_DONE_POS - _NEG` for school 1-7.
    let c = api.ca.clone();
    api.global("GetSpellBonusDamage", move |_, v: Value| {
        let school = if is_number(&v) { to_int(&v) } else { 0 };
        if !(1..=7).contains(&school) {
            return Err(mlua::Error::runtime("Usage: GetSpellBonusDamage(school)"));
        }
        let i = (school - 1) as usize;
        Ok(c.lock().mirror.me().map_or(0, |f| {
            i64::from(f.i32(field::PLAYER_MOD_DAMAGE_DONE_POS + i))
                - i64::from(f.i32(field::PLAYER_MOD_DAMAGE_DONE_NEG + i))
        }))
    })?;
    let c = api.ca.clone();
    api.global("GetSpellBonusHealing", move |lua, ()| {
        Ok(bonus_healing(lua, &c))
    })?;
    Ok(())
}

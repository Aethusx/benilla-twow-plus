//! The spellbook and level views: `Level.cpp`, `SkillLine.cpp`, `Tabs.cpp`, `IsSelfBuff.cpp`,
//! `AutoAttack.cpp`, and `talent/SpellSet.cpp`'s `IsClassTalentSpellBookItem`.

use std::collections::HashSet;

use mlua::{IntoLuaMulti, Lua, Value};

use super::{bank_is_pet, book_slot_to_id, book_type_is_pet};
use crate::dbc::{Databases, Row};
use crate::lua::{is_number, none, to_int, Api};
use crate::spells::{self, col};
use crate::talents::{self, sla_col, SKILL_LINE_NAME};

/// `SPELL_ATTR_HIDDEN_CLIENTSIDE`.
const ATTR_HIDDEN_CLIENTSIDE: u32 = 0x80;
/// `SPELL_EFFECT_APPLY_AURA` and `SPELL_EFFECT_APPLY_AREA_AURA_PARTY`.
const EFFECT_APPLY_AURA: u32 = 6;
const EFFECT_APPLY_AREA_AURA_PARTY: u32 = 35;
/// `SPELL_ATTR_EX2_AUTOREPEAT_FLAG`, on exactly Auto Shot and Shoot.
const ATTR_EX2_AUTOREPEAT: u32 = 0x20;
/// Auto Attack, the one spell that swings in melee.
const SPELL_AUTO_ATTACK: i64 = 6603;
/// Every valid 1.12 class bit: warrior to druid, without 6 and 10.
const VALID_CLASS_MASK: u32 = (1 << 0)
    | (1 << 1)
    | (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 6)
    | (1 << 7)
    | (1 << 8)
    | (1 << 10);

/// `IsPositiveRankTarget`: the friendly targets `SelectAuraRankForLevel` treats as positive.
fn is_positive_rank_target(t: u32) -> bool {
    matches!(t, 21 | 35 | 45 | 57 | 61 | 20 | 30 | 31 | 33 | 34 | 37 | 56)
}

/// `RequiredTargetLevel`: `spellLevel - 10` for a ranked, non-passive spell with a positive aura
/// effect; 0 for any other.
fn required_target_level(rec: &Row) -> i32 {
    let spell_level = rec.i32(col::SPELL_LEVEL);
    if spell_level <= 10
        || rec.u32(col::ATTRIBUTES) & spells::ATTR_PASSIVE != 0
        || rec.loc(col::RANK).is_empty()
    {
        return 0;
    }
    let positive = (0..spells::EFFECTS).any(|i| {
        let effect = rec.u32(col::EFFECT + i);
        (effect == EFFECT_APPLY_AURA
            && is_positive_rank_target(rec.u32(col::EFFECT_IMPLICIT_TARGET_A + i)))
            || effect == EFFECT_APPLY_AREA_AURA_PARTY
    });
    if positive {
        spell_level - 10
    } else {
        0
    }
}

/// `Spell::IsSelfBuff::IsSelfBuff`: every used effect targets none or self, and one is used.
pub(crate) fn is_self_buff(db: &Databases, spell_id: u32) -> bool {
    spells::table(db)
        .and_then(|t| t.row(spell_id).map(|r| spells::self_buff(&r)))
        .unwrap_or(false)
}

fn is_ranged_auto(db: &Databases, id: i64) -> bool {
    id > 0
        && spells::table(db)
            .and_then(|t| t.row(id as u32).map(|r| r.u32(col::ATTRIBUTES_EX2)))
            .is_some_and(|ex2| ex2 & ATTR_EX2_AUTOREPEAT != 0)
}

/// `ReadSpellBookSlotID`: the slot, with `"pet"` or a bank number picking the book.
fn auto_attack_slot_id(lua: &Lua, slot: &Value, book: &Value) -> i64 {
    if !is_number(slot) {
        return 0;
    }
    let pet = match book {
        Value::String(s) => book_type_is_pet(&s.to_string_lossy()),
        v if is_number(v) => to_int(v) == 1,
        _ => false,
    };
    i64::from(book_slot_to_id(lua, to_int(slot), pet))
}

/// `Spell::Tabs::IndexForSlot`: the 1-based tab holding a 1-based player slot.
fn tab_for_slot(lua: &Lua, slot: i64) -> usize {
    if slot < 1 {
        return 0;
    }
    let slot0 = (slot - 1) as u32;
    let mut offset = 0;
    for (i, tab) in benilla_ui::script::ext_read::spellbook_tabs(lua)
        .iter()
        .enumerate()
    {
        if slot0 < offset + tab.num_spells {
            return i + 1;
        }
        offset += tab.num_spells;
    }
    0
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let ca = api.ca.clone();

    let d = ca.db.clone();
    api.table("C_SpellBook", "GetSpellLevelLearned", move |_, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_SpellBook.GetSpellLevelLearned(spellID)",
            ));
        }
        let id = to_int(&v);
        if id <= 0 {
            return Ok(0);
        }
        Ok(spells::table(&d)
            .and_then(|t| t.row(id as u32).map(|r| r.i32(col::BASE_LEVEL)))
            .unwrap_or(0))
    })?;

    let c = ca.clone();
    api.table(
        "C_SpellBook",
        "GetCurrentLevelSpells",
        move |lua, v: Value| {
            let arg_level = if is_number(&v) { to_int(&v) } else { 0 };
            let out = lua.create_table()?;
            let (class, race, level) = {
                let st = c.lock();
                let Some(me) = st.mirror.me() else {
                    return Ok(out);
                };
                (me.class(), me.race(), i64::from(me.level()))
            };
            if class == 0 || race == 0 || level <= 0 {
                return Ok(out);
            }
            let query = if arg_level > 0 { arg_level } else { level };
            let class_bit = 1u32 << (class - 1);
            let race_bit = 1u32 << (race - 1);
            let (Some(sla), Some(table)) = (c.db.get("SkillLineAbility"), spells::table(&c.db))
            else {
                return Ok(out);
            };
            let mut seen = HashSet::new();
            let mut n = 0;
            for r in sla.rows() {
                if r.u32(sla_col::EXCLUDE_CLASS) & class_bit != 0
                    || r.u32(sla_col::EXCLUDE_RACE) & race_bit != 0
                {
                    continue;
                }
                let race_mask = r.u32(sla_col::RACE_MASK);
                if race_mask != 0 && race_mask & race_bit == 0 {
                    continue;
                }
                let class_mask = r.u32(sla_col::CLASS_MASK);
                let class_spell =
                    class_mask & !VALID_CLASS_MASK == 0 && class_mask & class_bit != 0;
                let racial = class_mask == 0 && race_mask != 0;
                if !class_spell && !racial {
                    continue;
                }
                let id = r.u32(sla_col::SPELL);
                if id == 0 {
                    continue;
                }
                let Some(rec) = table.row(id) else {
                    continue;
                };
                if i64::from(rec.i32(col::BASE_LEVEL)) != query
                    || talents::is_talent_rank(&c.db, id)
                    || rec.u32(col::ATTRIBUTES) & ATTR_HIDDEN_CLIENTSIDE != 0
                    || !seen.insert(id)
                {
                    continue;
                }
                n += 1;
                out.raw_set(n, id)?;
            }
            Ok(out)
        },
    )?;

    let d = ca.db.clone();
    let required = move |_: &Lua, v: Value| -> mlua::Result<Option<i32>> {
        let id = if is_number(&v) { to_int(&v) } else { 0 };
        let Some(table) = spells::table(&d) else {
            return Ok(None);
        };
        Ok(u32::try_from(id)
            .ok()
            .and_then(|i| table.row(i))
            .map(|r| required_target_level(&r)))
    };
    api.global("GetSpellRequiredTargetLevel", required.clone())?;
    api.table("C_Spell", "GetSpellRequiredTargetLevel", required)?;

    let d = ca.db.clone();
    api.table("C_Spell", "GetSpellLevelInfo", move |lua, v: Value| {
        let id = if is_number(&v) { to_int(&v) } else { 0 };
        let row = spells::table(&d).and_then(|t| {
            t.row(u32::try_from(id).ok()?).map(|r| {
                (
                    r.i32(col::SPELL_LEVEL),
                    r.i32(col::BASE_LEVEL),
                    r.i32(col::MAX_LEVEL),
                )
            })
        });
        match row {
            Some(levels) => levels.into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    let c = ca.clone();
    api.table("C_SpellBook", "GetSpellSkillLine", move |lua, v: Value| {
        let nils = || (Value::Nil, Value::Nil).into_lua_multi(lua);
        if !is_number(&v) {
            return nils();
        }
        let id = to_int(&v);
        let skill = {
            let st = c.lock();
            u32::try_from(id).map_or(0, |id| talents::skill_for_spell(&c.db, &st.mirror, id))
        };
        let name =
            c.db.get("SkillLine")
                .and_then(|t| t.row(skill).map(|r| r.loc(SKILL_LINE_NAME).to_string()));
        match (skill, name) {
            (s, Some(name)) if s != 0 => (name, s).into_lua_multi(lua),
            _ => nils(),
        }
    })?;

    let d = ca.db.clone();
    api.table("C_SpellBook", "GetSkillLineName", move |_, v: Value| {
        if !is_number(&v) {
            return Ok(None);
        }
        let id = to_int(&v);
        if id <= 0 {
            return Ok(None);
        }
        Ok(d.get("SkillLine")
            .and_then(|t| t.row(id as u32).map(|r| r.loc(SKILL_LINE_NAME).to_string())))
    })?;

    let c = ca.clone();
    api.table("C_SpellBook", "GetSkillLineRank", move |lua, v: Value| {
        if !is_number(&v) || to_int(&v) <= 0 {
            return Value::Nil.into_lua_multi(lua);
        }
        let rank = talents::skill_rank(&c.lock().mirror, to_int(&v) as u32);
        match rank {
            Some(r) => r.into_lua_multi(lua),
            None => Value::Nil.into_lua_multi(lua),
        }
    })?;

    api.table("C_SpellBook", "GetNumSpellBookSkillLines", |lua, ()| {
        Ok(benilla_ui::script::ext_read::spellbook_tabs(lua).len())
    })?;

    api.table(
        "C_SpellBook",
        "GetSpellBookSkillLineInfo",
        |lua, v: Value| {
            if !is_number(&v) {
                return Ok(Value::Nil);
            }
            let tabs = benilla_ui::script::ext_read::spellbook_tabs(lua);
            let index = to_int(&v);
            let Some(tab) = usize::try_from(index - 1).ok().and_then(|i| tabs.get(i)) else {
                return Ok(Value::Nil);
            };
            let t = lua.create_table()?;
            t.raw_set("name", tab.name.as_str())?;
            // The texture path, as `C_SpellBook.GetSpellBookItemInfo`'s `iconID`.
            t.raw_set("iconID", tab.texture.as_deref().unwrap_or(""))?;
            t.raw_set("itemIndexOffset", tab.offset)?;
            t.raw_set("numSpellBookItems", tab.num_spells)?;
            t.raw_set("isGuild", false)?;
            t.raw_set("shouldHide", false)?;
            Ok(Value::Table(t))
        },
    )?;

    api.table(
        "C_SpellBook",
        "GetSpellBookItemSkillLineIndex",
        |lua, (slot, bank): (Value, Value)| {
            if !is_number(&slot) || bank_is_pet(&bank) {
                return Ok(None);
            }
            let index = tab_for_slot(lua, to_int(&slot));
            Ok((index > 0).then_some(index))
        },
    )?;

    api.table("C_SpellBook", "GetSkillLineIndexByID", |lua, v: Value| {
        if !is_number(&v) {
            return Ok(None);
        }
        let id = to_int(&v);
        if id <= 0 {
            return Ok(None);
        }
        Ok(benilla_ui::script::ext_read::spellbook_tabs(lua)
            .iter()
            .position(|t| i64::from(t.skill_line) == id)
            .map(|i| i + 1))
    })?;

    let d = ca.db.clone();
    api.table("C_Spell", "IsSelfBuff", move |_, v: Value| {
        if !is_number(&v) {
            return Ok(false);
        }
        let id = to_int(&v);
        Ok(id > 0 && is_self_buff(&d, id as u32))
    })?;

    api.table("C_Spell", "IsAutoAttackSpell", |_, v: Value| {
        Ok(is_number(&v) && to_int(&v) == SPELL_AUTO_ATTACK)
    })?;
    let d = ca.db.clone();
    api.table("C_Spell", "IsRangedAutoAttackSpell", move |_, v: Value| {
        Ok(is_ranged_auto(
            &d,
            if is_number(&v) { to_int(&v) } else { 0 },
        ))
    })?;
    api.table(
        "C_SpellBook",
        "IsAutoAttackSpellBookItem",
        |lua, (slot, book): (Value, Value)| {
            Ok(auto_attack_slot_id(lua, &slot, &book) == SPELL_AUTO_ATTACK)
        },
    )?;
    let d = ca.db.clone();
    api.table(
        "C_SpellBook",
        "IsRangedAutoAttackSpellBookItem",
        move |lua, (slot, book): (Value, Value)| {
            Ok(is_ranged_auto(&d, auto_attack_slot_id(lua, &slot, &book)))
        },
    )?;

    let d = ca.db.clone();
    api.table(
        "C_SpellBook",
        "IsClassTalentSpellBookItem",
        move |lua, (slot, bank): (Value, Value)| {
            let pet = bank_is_pet(&bank);
            let id = if is_number(&slot) {
                book_slot_to_id(lua, to_int(&slot), pet)
            } else {
                0
            };
            Ok(!pet && id > 0 && talents::is_talent_line(&d, id))
        },
    )?;

    Ok(())
}

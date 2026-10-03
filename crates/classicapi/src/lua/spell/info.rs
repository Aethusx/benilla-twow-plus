//! `spell/Info.cpp`: `GetSpellInfo`, `GetSpellLink`, the spellbook lookups, the passive, harmful
//! and helpful tests, and `C_Spell`'s and `C_SpellBook`'s table forms of them.

use mlua::{IntoLuaMulti, Lua, Table, Value};

use super::{bank_is_pet, book_slot_to_id, book_type_is_pet, find_book_slot, resolve_spell_id};
use crate::dbc::{Databases, Row};
use crate::lua::{as_string, is_number, none, opt_str, to_int, truthy, Api};
use crate::spells::{self, col};

/// `IsHostileTarget`: the implicit targets that aim a spell at an enemy, CMaNGOS's
/// `IsPositiveSpell` set.
fn is_hostile_target(t: u32) -> bool {
    matches!(t, 6 | 15 | 16 | 25 | 28 | 36 | 47 | 53 | 54)
}

/// `IsFriendlyTarget`: self, pet, party, friend and chain-heal targets.
fn is_friendly_target(t: u32) -> bool {
    matches!(
        t,
        1 | 5 | 20 | 21 | 30 | 31 | 33 | 34 | 35 | 37 | 40 | 45 | 56 | 57 | 61 | 68 | 77 | 80
    )
}

/// Any of the three effects' implicit targets, A or B, matches.
fn any_effect_target(rec: &Row, pred: fn(u32) -> bool) -> bool {
    (0..spells::EFFECTS).any(|i| {
        pred(rec.u32(col::EFFECT_IMPLICIT_TARGET_A + i))
            || pred(rec.u32(col::EFFECT_IMPLICIT_TARGET_B + i))
    })
}

/// `ReadSpellInfo`'s fields.
struct SpellInfo {
    spell_id: i64,
    name: String,
    rank: String,
    icon: Option<String>,
    cost: i32,
    is_funnel: bool,
    power_type: i32,
    cast_time_ms: i32,
    min_range: f32,
    max_range: f32,
    passive: bool,
}

fn read_spell_info(db: &Databases, spell_id: i64) -> Option<SpellInfo> {
    let id = u32::try_from(spell_id).ok()?;
    let table = spells::table(db)?;
    let rec = table.row(id)?;
    let (min_range, max_range) = spells::range(db, &rec);
    Some(SpellInfo {
        spell_id,
        name: rec.loc(col::NAME).to_string(),
        rank: rec.loc(col::RANK).to_string(),
        icon: spells::spell_icon(db, &rec, false),
        cost: rec.i32(col::MANA_COST),
        is_funnel: rec.u32(col::ATTRIBUTES_EX2) & spells::ATTR_EX2_HEALTH_FUNNEL != 0,
        power_type: rec.i32(col::POWER_TYPE),
        cast_time_ms: spells::cast_time_ms(db, &rec),
        min_range,
        max_range,
        passive: rec.u32(col::ATTRIBUTES) & spells::ATTR_PASSIVE != 0,
    })
}

/// `SpellIDFromLink`: the id after `Hspell:`, 0 when the string is not a spell link.
fn spell_id_from_link(s: &str) -> i64 {
    let Some(at) = s.find("Hspell:") else {
        return 0;
    };
    let digits: String = s[at + 7..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(0)
}

/// `Spell::Lookup::SpellNameToID`: a name exactly as `Spell.dbc` spells it, the player's book
/// then the pet's. A trailing `(subtext)` pins that rank; a plain name is the last matching
/// slot, the highest rank, since ranks are stored ascending.
pub(crate) fn spell_name_to_id(lua: &Lua, db: &Databases, name: &str) -> i64 {
    if name.is_empty() {
        return 0;
    }
    let (base, want_rank) = match (name.rfind('('), name.rfind(')')) {
        (Some(open), Some(close)) if close > open + 1 => {
            (&name[..open], Some(&name[open + 1..close]))
        }
        _ => (name, None),
    };
    let base = base.trim_end_matches([' ', '\t']);
    if base.is_empty() || base.len() >= 256 {
        return 0;
    }
    let Some(table) = spells::table(db) else {
        return 0;
    };
    let (player, pet) = benilla_ui::script::ext_read::spellbook_ids(lua);
    for book in [player, pet] {
        let mut best = 0;
        for id in book {
            let Some(rec) = table.row(id) else {
                continue;
            };
            if rec.loc(col::NAME) != base {
                continue;
            }
            match want_rank {
                None => best = i64::from(id),
                Some(rank) if rec.loc(col::RANK) == rank => return i64::from(id),
                Some(_) => {}
            }
        }
        if want_rank.is_none() && best != 0 {
            return best;
        }
    }
    0
}

/// `ResolveLuaArgsToSpellID`, the globals' argument set: a name or link, a spell id, or a
/// spellbook slot with a book type.
fn resolve_args(lua: &Lua, db: &Databases, a1: &Value, a2: &Value) -> mlua::Result<i64> {
    if let Value::String(s) = a1 {
        let s = s.to_string_lossy();
        let from_link = spell_id_from_link(&s);
        return Ok(if from_link > 0 {
            from_link
        } else {
            spell_name_to_id(lua, db, &s)
        });
    }
    if !is_number(a1) {
        return Err(mlua::Error::runtime(
            "Usage: GetSpellInfo(spellID | \"name\" | link) or GetSpellInfo(slot, bookType)",
        ));
    }
    let n = to_int(a1);
    if let Some(book) = as_string(a2) {
        return Ok(i64::from(book_slot_to_id(lua, n, book_type_is_pet(&book))));
    }
    Ok(n)
}

/// `BuildSpellLink`: `|cff71d5ff|Hspell:ID:0|h[Name]|h|r`, `None` for an unknown or unnamed spell.
fn spell_link(db: &Databases, spell_id: i64) -> Option<String> {
    let table = spells::table(db)?;
    let rec = table.row(u32::try_from(spell_id).ok()?)?;
    let name = rec.loc(col::NAME);
    if name.is_empty() {
        return None;
    }
    Some(format!("|cff71d5ff|Hspell:{spell_id}:0|h[{name}]|h|r"))
}

/// `PlayerKnowsSpell`: the known-spell bitmap's bit, bounded by the table's record count.
pub(crate) fn player_knows(ca: &crate::Ca, spell_id: i64) -> bool {
    if spell_id < 1 {
        return false;
    }
    let max = spells::table(&ca.db).map_or(0, |t| i64::from(t.max_id()));
    spell_id <= max && ca.lock().known.contains(&(spell_id as u32))
}

/// The known spells with a `Spell.dbc` row, ascending: `ForEachKnownSpell`'s walk order.
pub(crate) fn known_spells(ca: &crate::Ca) -> Vec<u32> {
    let mut ids = ca.lock().known.clone();
    ids.sort_unstable();
    ids.dedup();
    let Some(table) = spells::table(&ca.db) else {
        return Vec::new();
    };
    ids.retain(|id| table.row(*id).is_some());
    ids
}

fn harmful(db: &Databases, spell_id: i64) -> bool {
    let Some(table) = spells::table(db) else {
        return false;
    };
    u32::try_from(spell_id)
        .ok()
        .and_then(|id| table.row(id))
        .is_some_and(|r| any_effect_target(&r, is_hostile_target))
}

fn helpful(db: &Databases, spell_id: i64) -> bool {
    let Some(table) = spells::table(db) else {
        return false;
    };
    u32::try_from(spell_id)
        .ok()
        .and_then(|id| table.row(id))
        .is_some_and(|r| any_effect_target(&r, is_friendly_target))
}

/// `PushIsPassive`: the passive bit, nil for an unknown spell.
fn is_passive(db: &Databases, spell_id: i64) -> Option<bool> {
    if spell_id <= 0 {
        return None;
    }
    let table = spells::table(db)?;
    let rec = table.row(spell_id as u32)?;
    Some(rec.u32(col::ATTRIBUTES) & spells::ATTR_PASSIVE != 0)
}

fn info_table(lua: &Lua, info: &SpellInfo) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("name", info.name.as_str())?;
    // `iconID` is the icon path: vanilla has no file ids.
    t.raw_set("iconID", opt_str(lua, info.icon.as_deref())?)?;
    t.raw_set("castTime", info.cast_time_ms)?;
    t.raw_set("minRange", info.min_range)?;
    t.raw_set("maxRange", info.max_range)?;
    t.raw_set("spellID", info.spell_id)?;
    t.raw_set("rank", info.rank.as_str())?;
    t.raw_set("cost", info.cost)?;
    t.raw_set("isFunnel", info.is_funnel)?;
    t.raw_set("powerType", info.power_type)?;
    Ok(t)
}

/// `Enum.SpellBookItemType`'s values the 1.12 books produce.
const ITEM_TYPE_SPELL: i64 = 1;
const ITEM_TYPE_PET_ACTION: i64 = 3;

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let ca = api.ca.clone();
    let db = ca.db.clone();

    let d = db.clone();
    api.global("GetSpellInfo", move |lua, (a1, a2): (Value, Value)| {
        let id = resolve_args(lua, &d, &a1, &a2)?;
        let Some(i) = (id > 0).then(|| read_spell_info(&d, id)).flatten() else {
            return Ok(none());
        };
        (
            i.name.as_str(),
            i.rank.as_str(),
            opt_str(lua, i.icon.as_deref())?,
            i.cost,
            i.is_funnel,
            i.power_type,
            i.cast_time_ms,
            i.min_range,
            i.max_range,
            i.spell_id,
        )
            .into_lua_multi(lua)
    })?;

    let d = db.clone();
    api.global("GetSpellLink", move |lua, (a1, a2): (Value, Value)| {
        let id = resolve_args(lua, &d, &a1, &a2)?;
        match (id > 0).then(|| spell_link(&d, id)).flatten() {
            Some(link) => (link, id).into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    api.global("FindSpellBookSlotByID", |lua, id: Value| {
        if !is_number(&id) {
            return Err(mlua::Error::runtime(
                "Usage: FindSpellBookSlotByID(spellID)",
            ));
        }
        match find_book_slot(lua, to_int(&id)) {
            Some((slot, pet)) => (slot, if pet { "pet" } else { "spell" }).into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    let d = db.clone();
    api.global("IsPassiveSpell", move |lua, (a1, a2): (Value, Value)| {
        Ok(is_passive(&d, resolve_args(lua, &d, &a1, &a2)?))
    })?;

    let c = ca.clone();
    api.global("IsPlayerSpell", move |_, id: Value| {
        if !is_number(&id) {
            return Err(mlua::Error::runtime("Usage: IsPlayerSpell(spellID)"));
        }
        Ok(player_knows(&c, to_int(&id)))
    })?;

    // Dual Wield (674) is the one spell with `SPELL_EFFECT_DUAL_WIELD`; knowing it is the
    // server's `m_canDualWield`.
    let c = ca.clone();
    api.global("CanDualWield", move |_, ()| Ok(player_knows(&c, 674)))?;

    api.global("IsSpellKnown", |lua, (id, is_pet): (Value, Value)| {
        if !is_number(&id) {
            return Err(mlua::Error::runtime(
                "Usage: IsSpellKnown(spellID, [isPet])",
            ));
        }
        let id = to_int(&id);
        if id < 1 {
            return Ok(false);
        }
        Ok(find_book_slot(lua, id).is_some_and(|(_, pet)| pet == truthy(&is_pet)))
    })?;

    let d = db.clone();
    api.global("IsHarmfulSpell", move |lua, (a1, a2): (Value, Value)| {
        Ok(harmful(&d, resolve_args(lua, &d, &a1, &a2)?))
    })?;
    let d = db.clone();
    api.global("IsHelpfulSpell", move |lua, (a1, a2): (Value, Value)| {
        Ok(helpful(&d, resolve_args(lua, &d, &a1, &a2)?))
    })?;

    let d = db.clone();
    api.table("C_Spell", "GetSpellLink", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        Ok((id > 0).then(|| spell_link(&d, id)).flatten())
    })?;

    let d = db.clone();
    api.table(
        "C_Spell",
        "GetSpellInfo",
        move |lua, v: Value| match read_spell_info(&d, resolve_spell_id(lua, &v)) {
            Some(i) => Ok(Value::Table(info_table(lua, &i)?)),
            None => Ok(Value::Nil),
        },
    )?;

    let d = db.clone();
    api.table("C_Spell", "GetSpellName", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        let Some(table) = spells::table(&d) else {
            return Ok(None);
        };
        let name = u32::try_from(id)
            .ok()
            .and_then(|id| table.row(id))
            .map(|r| r.loc(col::NAME).to_string());
        Ok(name.filter(|n| !n.is_empty()))
    })?;

    let d = db.clone();
    api.table("C_Spell", "GetSpellTexture", move |lua, v: Value| {
        let id = resolve_spell_id(lua, &v);
        let Some(table) = spells::table(&d) else {
            return Ok(None);
        };
        Ok(u32::try_from(id)
            .ok()
            .and_then(|id| table.row(id))
            .and_then(|r| spells::spell_icon(&d, &r, false)))
    })?;

    let d = db.clone();
    api.table("C_Spell", "IsSpellPassive", move |lua, v: Value| {
        Ok(is_passive(&d, resolve_spell_id(lua, &v)))
    })?;
    let d = db.clone();
    api.table("C_Spell", "IsSpellHarmful", move |lua, v: Value| {
        Ok(harmful(&d, resolve_spell_id(lua, &v)))
    })?;
    let d = db.clone();
    api.table("C_Spell", "IsSpellHelpful", move |lua, v: Value| {
        Ok(helpful(&d, resolve_spell_id(lua, &v)))
    })?;

    let d = db.clone();
    api.table(
        "C_SpellBook",
        "GetSpellBookItemInfo",
        move |lua, (slot, bank): (Value, Value)| {
            if !is_number(&slot) {
                return Ok(Value::Nil);
            }
            let pet = bank_is_pet(&bank);
            let id = book_slot_to_id(lua, to_int(&slot), pet);
            let Some(info) = (id > 0)
                .then(|| read_spell_info(&d, i64::from(id)))
                .flatten()
            else {
                return Ok(Value::Nil);
            };
            let t = lua.create_table()?;
            t.raw_set(
                "itemType",
                if pet {
                    ITEM_TYPE_PET_ACTION
                } else {
                    ITEM_TYPE_SPELL
                },
            )?;
            t.raw_set("actionID", id)?;
            t.raw_set("spellID", id)?;
            t.raw_set("name", info.name.as_str())?;
            t.raw_set("subName", info.rank.as_str())?;
            t.raw_set("iconID", opt_str(lua, info.icon.as_deref())?)?;
            t.raw_set("isPassive", info.passive)?;
            t.raw_set("isOffSpec", false)?;
            Ok(Value::Table(t))
        },
    )?;

    let c = ca.clone();
    api.table(
        "C_SpellBook",
        "GetPlayerSpellsByAura",
        move |lua, aura: Value| {
            if !is_number(&aura) {
                return Err(mlua::Error::runtime(
                    "Usage: C_SpellBook.GetPlayerSpellsByAura(auraName)",
                ));
            }
            let aura = to_int(&aura);
            let out = lua.create_table()?;
            if aura <= 0 {
                return Ok(out);
            }
            let Some(table) = spells::table(&c.db) else {
                return Ok(out);
            };
            let mut n = 0;
            for id in known_spells(&c) {
                let Some(rec) = table.row(id) else {
                    continue;
                };
                if (0..spells::EFFECTS)
                    .any(|e| i64::from(rec.i32(col::EFFECT_APPLY_AURA_NAME + e)) == aura)
                {
                    n += 1;
                    out.raw_set(n, id)?;
                }
            }
            Ok(out)
        },
    )?;

    let c = ca.clone();
    api.table("C_SpellBook", "ContainsAnyDisenchantSpell", move |_, ()| {
        let Some(table) = spells::table(&c.db) else {
            return Ok(false);
        };
        Ok(known_spells(&c).into_iter().any(|id| {
            table.row(id).is_some_and(|r| {
                (0..spells::EFFECTS).any(|e| r.u32(col::EFFECT + e) == spells::EFFECT_DISENCHANT)
            })
        }))
    })?;

    api.int_enum("Enum", "SpellBookSpellBank", &[("Player", 0), ("Pet", 1)])?;
    api.int_enum(
        "Enum",
        "SpellBookItemType",
        &[
            ("None", 0),
            ("Spell", 1),
            ("FutureSpell", 2),
            ("PetAction", 3),
            ("Flyout", 4),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbc::Dbc;
    use crate::lua::test_support::vm;
    use crate::Ca;

    /// A one-row `Spell.dbc` (Fireball, 133) and its icon.
    fn seeded() -> Ca {
        let ca = Ca::default();
        let mut strings = b"\0Fireball\0Rank 1\0".to_vec();
        let mut row = vec![0u32; 173];
        row[0] = 133;
        row[col::NAME] = 1;
        row[col::RANK] = 10;
        row[col::ICON_ID] = 7;
        row[col::MANA_COST] = 30;
        row[col::EFFECT_IMPLICIT_TARGET_A] = 6;
        ca.db.seed("Spell", Dbc::from_rows(&[row], &strings));
        strings = b"\0Interface\\Icons\\Spell_Fire_FlameBolt\0".to_vec();
        ca.db
            .seed("SpellIcon", Dbc::from_rows(&[vec![7, 1]], &strings));
        ca
    }

    #[test]
    fn c_spell_reads_spell_dbc() {
        let script = vm(&seeded());
        let (name, tex, harm, help): (String, String, bool, bool) = script
            .eval(
                "return C_Spell.GetSpellName(133), C_Spell.GetSpellTexture('133'), \
                 C_Spell.IsSpellHarmful(133), C_Spell.IsSpellHelpful(133)",
            )
            .unwrap();
        assert_eq!(name, "Fireball");
        assert_eq!(tex, "Interface\\Icons\\Spell_Fire_FlameBolt");
        assert!(harm && !help);
        let link: String = script.eval("return C_Spell.GetSpellLink(133)").unwrap();
        assert_eq!(link, "|cff71d5ff|Hspell:133:0|h[Fireball]|h|r");
        let unknown: Option<String> = script.eval("return C_Spell.GetSpellName(99)").unwrap();
        assert_eq!(unknown, None);
        let by_link: String = script
            .eval("return C_Spell.GetSpellName('|Hspell:133|h[x]|h')")
            .unwrap();
        assert_eq!(by_link, "Fireball");
    }

    #[test]
    fn get_spell_info_returns_ten_values() {
        let script = vm(&seeded());
        let n: i64 = script
            .eval("local t = {GetSpellInfo(133)} return table.getn(t)")
            .unwrap();
        assert_eq!(n, 10);
        let cost: i64 = script
            .eval("return C_Spell.GetSpellInfo(133).cost")
            .unwrap();
        assert_eq!(cost, 30);
        assert!(script.eval::<()>("GetSpellInfo({})").is_err());
    }
}

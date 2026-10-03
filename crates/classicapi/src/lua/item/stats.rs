//! `StatAccum.cpp` and `Stats.cpp`: an item's stats keyed by the modern `ITEM_MOD_*` names, from
//! its template, its equip spells' auras, its random suffix and enchantments.

use benilla_protocol::ItemInfo;
use mlua::{Lua, Table, Value};

use super::arg_id;
use crate::dbc::Databases;
use crate::lua::Api;
use crate::spells::{self, col};

/// Every key the accumulator carries, `kKeys`.
pub(crate) const KEYS: [&str; 30] = [
    "ITEM_MOD_MANA_SHORT",
    "ITEM_MOD_HEALTH_SHORT",
    "ITEM_MOD_AGILITY_SHORT",
    "ITEM_MOD_STRENGTH_SHORT",
    "ITEM_MOD_INTELLECT_SHORT",
    "ITEM_MOD_SPIRIT_SHORT",
    "ITEM_MOD_STAMINA_SHORT",
    "RESISTANCE0_NAME",
    "RESISTANCE1_NAME",
    "RESISTANCE2_NAME",
    "RESISTANCE3_NAME",
    "RESISTANCE4_NAME",
    "RESISTANCE5_NAME",
    "RESISTANCE6_NAME",
    "ITEM_MOD_ATTACK_POWER_SHORT",
    "ITEM_MOD_RANGED_ATTACK_POWER_SHORT",
    "ITEM_MOD_CRIT_MELEE_RATING",
    "ITEM_MOD_CRIT_RANGED_RATING",
    "ITEM_MOD_HIT_MELEE_RATING",
    "ITEM_MOD_HIT_RANGED_RATING",
    "ITEM_MOD_HIT_SPELL_RATING",
    "ITEM_MOD_CRIT_SPELL_RATING",
    "ITEM_MOD_SPELL_DAMAGE_DONE_SHORT",
    "ITEM_MOD_SPELL_HEALING_DONE_SHORT",
    "ITEM_MOD_MANA_REGENERATION",
    "ITEM_MOD_DEFENSE_SKILL_RATING",
    "ITEM_MOD_DODGE_RATING",
    "ITEM_MOD_PARRY_RATING",
    "ITEM_MOD_BLOCK_RATING",
    "ITEM_MOD_BLOCK_VALUE",
];

fn item_mod_key(stat: u32) -> Option<&'static str> {
    Some(match stat {
        0 => "ITEM_MOD_MANA_SHORT",
        1 => "ITEM_MOD_HEALTH_SHORT",
        3 => "ITEM_MOD_AGILITY_SHORT",
        4 => "ITEM_MOD_STRENGTH_SHORT",
        5 => "ITEM_MOD_INTELLECT_SHORT",
        6 => "ITEM_MOD_SPIRIT_SHORT",
        7 => "ITEM_MOD_STAMINA_SHORT",
        _ => return None,
    })
}

fn unit_stat_key(stat: i32) -> Option<&'static str> {
    Some(match stat {
        0 => "ITEM_MOD_STRENGTH_SHORT",
        1 => "ITEM_MOD_AGILITY_SHORT",
        2 => "ITEM_MOD_STAMINA_SHORT",
        3 => "ITEM_MOD_INTELLECT_SHORT",
        4 => "ITEM_MOD_SPIRIT_SHORT",
        _ => return None,
    })
}

fn resist_key(school: i32) -> Option<&'static str> {
    usize::try_from(school)
        .ok()
        .filter(|s| *s <= 6)
        .map(|s| KEYS[7 + s])
}

/// The accumulator, one signed sum per key.
#[derive(Clone, Debug, Default)]
pub(crate) struct Accum([i64; 30]);

impl Accum {
    pub fn add(&mut self, key: Option<&str>, delta: i64) {
        if let Some(i) = key.and_then(|k| KEYS.iter().position(|x| *x == k)) {
            self.0[i] += delta;
        }
    }

    /// `Value`: one key's sum.
    pub fn get(&self, key: &str) -> i64 {
        KEYS.iter().position(|x| *x == key).map_or(0, |i| self.0[i])
    }

    /// `AddSpellStatAuras`: the stat auras of one spell, `base + dice` each.
    pub fn spell_auras(&mut self, db: &Databases, spell: i32, sign: i64) {
        let Some(t) = spells::table(db) else {
            return;
        };
        let Some(sp) = u32::try_from(spell)
            .ok()
            .filter(|s| *s > 0)
            .and_then(|s| t.row(s))
        else {
            return;
        };
        let aura = |i| sp.i32(col::EFFECT_APPLY_AURA_NAME + i);
        let misc = |i| sp.i32(col::EFFECT_MISC_VALUE + i);
        let melee_ap = (0..spells::EFFECTS).any(|i| aura(i) == 99);
        for i in 0..spells::EFFECTS {
            let v = sign
                * (i64::from(sp.i32(col::EFFECT_BASE_POINTS + i))
                    + i64::from(sp.i32(col::EFFECT_BASE_DICE + i)));
            match aura(i) {
                29 => self.add(unit_stat_key(misc(i)), v),
                34 => self.add(Some("ITEM_MOD_HEALTH_SHORT"), v),
                35 if misc(i) == 0 => self.add(Some("ITEM_MOD_MANA_SHORT"), v),
                22 => {
                    for school in 0..=6 {
                        if misc(i) & (1 << school) != 0 {
                            self.add(resist_key(school), v);
                        }
                    }
                }
                99 => self.add(Some("ITEM_MOD_ATTACK_POWER_SHORT"), v),
                124 if !melee_ap => self.add(Some("ITEM_MOD_RANGED_ATTACK_POWER_SHORT"), v),
                52 => {
                    self.add(Some("ITEM_MOD_CRIT_MELEE_RATING"), v);
                    self.add(Some("ITEM_MOD_CRIT_RANGED_RATING"), v);
                }
                54 => {
                    self.add(Some("ITEM_MOD_HIT_MELEE_RATING"), v);
                    self.add(Some("ITEM_MOD_HIT_RANGED_RATING"), v);
                }
                55 => self.add(Some("ITEM_MOD_HIT_SPELL_RATING"), v),
                57 | 71 => self.add(Some("ITEM_MOD_CRIT_SPELL_RATING"), v),
                47 => self.add(Some("ITEM_MOD_PARRY_RATING"), v),
                49 => self.add(Some("ITEM_MOD_DODGE_RATING"), v),
                51 => self.add(Some("ITEM_MOD_BLOCK_RATING"), v),
                13 if misc(i) & 0x7E != 0 => self.add(Some("ITEM_MOD_SPELL_DAMAGE_DONE_SHORT"), v),
                135 => self.add(Some("ITEM_MOD_SPELL_HEALING_DONE_SHORT"), v),
                85 if misc(i) == 0 => self.add(Some("ITEM_MOD_MANA_REGENERATION"), v),
                30 if misc(i) == 95 => self.add(Some("ITEM_MOD_DEFENSE_SKILL_RATING"), v),
                _ => {}
            }
        }
    }

    /// `AccumulateRecord`: the template's stats, armour and resistances, block, equip spells.
    pub fn record(&mut self, db: &Databases, r: &ItemInfo, sign: i64) {
        for (ty, v) in &r.stats {
            self.add(item_mod_key(*ty), sign * i64::from(*v));
        }
        self.add(Some("RESISTANCE0_NAME"), sign * i64::from(r.armor));
        for (school, v) in r.resistances.iter().enumerate() {
            self.add(Some(KEYS[8 + school]), sign * i64::from(*v as u32));
        }
        self.add(Some("ITEM_MOD_BLOCK_VALUE"), sign * i64::from(r.block));
        for s in &r.spells {
            if s.spell_id != 0 && s.trigger == super::data::TRIGGER_ON_EQUIP {
                self.spell_auras(db, s.spell_id as i32, sign);
            }
        }
    }

    /// `ApplyEnchant`: equip-spell, resistance and stat effects.
    pub fn enchant(&mut self, db: &Databases, enchant: u32, sign: i64) {
        let Some(t) = db.get("SpellItemEnchantment") else {
            return;
        };
        let Some(r) = (enchant != 0).then(|| t.row(enchant)).flatten() else {
            return;
        };
        for i in 0..3 {
            let (ty, amount, arg) = (r.i32(1 + i), r.i32(0x10 / 4 + i), r.i32(0x28 / 4 + i));
            match ty {
                3 => self.spell_auras(db, arg, sign),
                4 => self.add(resist_key(arg), sign * i64::from(amount)),
                5 => self.add(item_mod_key(arg as u32), sign * i64::from(amount)),
                _ => {}
            }
        }
    }

    /// `ApplyRandomSuffix`: the `ItemRandomProperties.dbc` row's five enchantments.
    pub fn suffix(&mut self, db: &Databases, suffix: i64, sign: i64) {
        let Some(t) = db.get("ItemRandomProperties") else {
            return;
        };
        let Some(r) = u32::try_from(suffix)
            .ok()
            .filter(|s| *s > 0)
            .and_then(|s| t.row(s))
        else {
            return;
        };
        let enchants: Vec<u32> = (0..5).map(|i| r.u32(2 + i)).collect();
        for e in enchants {
            self.enchant(db, e, sign);
        }
    }
}

/// `ComputeDPS`: the summed average damage over the swing time.
pub(crate) fn dps(r: &ItemInfo) -> f64 {
    if r.delay_ms == 0 {
        return 0.0;
    }
    let avg: f64 = r
        .damages
        .iter()
        .map(|d| (f64::from(d.min) + f64::from(d.max)) * 0.5)
        .sum();
    if avg <= 0.0 {
        0.0
    } else {
        avg / (f64::from(r.delay_ms) / 1000.0)
    }
}

/// `ParseRandomSuffixFromLink`: a string's `item:` third field.
fn suffix_of(v: &Value) -> i64 {
    match v {
        Value::String(s) => crate::items::resolve_string(&s.to_string_lossy()).suffix,
        _ => 0,
    }
}

fn table(lua: &Lua, acc: &Accum, dps: f64) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (k, v) in KEYS.iter().zip(acc.0) {
        if v != 0 {
            t.set(*k, v)?;
        }
    }
    if dps.abs() > 1e-6 {
        t.set("ITEM_MOD_DAMAGE_PER_SECOND_SHORT", dps)?;
    }
    Ok(t)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let db = api.ca.db.clone();
    api.table("C_Item", "GetItemStats", move |lua, v: Value| {
        let id = arg_id(&v);
        if id <= 0 {
            return Err(mlua::Error::runtime("Usage: C_Item.GetItemStats(itemLink)"));
        }
        let Some(r) = crate::itemdb::peek(lua, id as u32) else {
            return Ok(Value::Nil);
        };
        let mut acc = Accum::default();
        acc.record(&db, &r, 1);
        acc.suffix(&db, suffix_of(&v), 1);
        Ok(Value::Table(table(lua, &acc, dps(&r))?))
    })?;

    let db = api.ca.db.clone();
    api.table(
        "C_Item",
        "GetItemStatDelta",
        move |lua, (a, b): (Value, Value)| {
            let (id1, id2) = (arg_id(&a), arg_id(&b));
            if id1 <= 0 || id2 <= 0 {
                return Err(mlua::Error::runtime(
                    "Usage: C_Item.GetItemStatDelta(itemLink1, itemLink2)",
                ));
            }
            let (Some(r1), Some(r2)) = (
                crate::itemdb::peek(lua, id1 as u32),
                crate::itemdb::peek(lua, id2 as u32),
            ) else {
                return Ok(Value::Nil);
            };
            let mut acc = Accum::default();
            acc.record(&db, &r2, 1);
            acc.suffix(&db, suffix_of(&b), 1);
            acc.record(&db, &r1, -1);
            acc.suffix(&db, suffix_of(&a), -1);
            Ok(Value::Table(table(lua, &acc, dps(&r2) - dps(&r1))?))
        },
    )?;
    Ok(())
}

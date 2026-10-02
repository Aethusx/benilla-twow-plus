//! Spell records and lookups: `GetSpellRec`, `GetSpellRecField`, the name and id lookups, the
//! icon, duration, range, modifier and spell power reads, and the usable and range checks.

use mlua::{Lua, Table, Value, Variadic};

use super::{flag, int, set, text, unit};
use crate::dbc::{spell_column, Kind, SpellRec, SPELL_COLUMNS};
use crate::mirror::field;
use crate::Np;

/// The locale cell `GetSpellRec` reads for a localized string: the first non-empty of the eight.
fn locale(rec: &SpellRec, at: usize) -> String {
    (0..8)
        .map(|i| rec.0.str(at + i))
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

fn cell(lua: &Lua, rec: &SpellRec, at: usize, kind: Kind) -> mlua::Result<Value> {
    Ok(match kind {
        Kind::U32 => Value::Number(f64::from(rec.0.u32(at))),
        Kind::I32 => Value::Number(f64::from(rec.0.i32(at))),
        Kind::F32 => Value::Number(f64::from(rec.0.f32(at))),
        Kind::U64 => {
            Value::Number((u64::from(rec.0.u32(at)) | (u64::from(rec.0.u32(at + 1)) << 32)) as f64)
        }
        Kind::Locale => Value::String(lua.create_string(locale(rec, at))?),
    })
}

/// One `SpellRec` field: a value, or a 1-based table for an array field.
fn field_value(
    lua: &Lua,
    rec: &SpellRec,
    at: usize,
    count: usize,
    kind: Kind,
) -> mlua::Result<Value> {
    if count == 1 || matches!(kind, Kind::Locale | Kind::U64) {
        return cell(lua, rec, at, kind);
    }
    let t = lua.create_table()?;
    for i in 0..count {
        t.set(i + 1, cell(lua, rec, at + i, kind)?)?;
    }
    Ok(Value::Table(t))
}

/// The computed fields the examples read beside the record's own.
fn computed(np: &Np, rec: &SpellRec, name: &str) -> Option<f64> {
    match name.to_ascii_lowercase().as_str() {
        "casttime" => np
            .db
            .cast_time_ms(rec.casting_time_index())
            .map(|ms| f64::from(ms.max(0))),
        "rangemin" => np.db.range(rec.range_index()).map(|r| f64::from(r.0)),
        "rangemax" => np.db.range(rec.range_index()).map(|r| f64::from(r.1)),
        _ => None,
    }
}

/// Split `"Name(Rank 3)"` into the name and the rank, both lower-cased.
pub(crate) fn split_rank(name: &str) -> (String, Option<String>) {
    let name = name.trim();
    match name.rfind('(') {
        Some(open) if name.ends_with(')') => (
            name[..open].trim().to_lowercase(),
            Some(name[open + 1..name.len() - 1].trim().to_lowercase()),
        ),
        _ => (name.to_lowercase(), None),
    }
}

fn rank_number(rank: &str) -> u32 {
    rank.split_whitespace()
        .last()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// The book spell a name (with an optional rank) names: the exact rank, else the highest.
pub(crate) fn book_spell(np: &Np, book: &[u32], name: &str) -> Option<u32> {
    let (want, rank) = split_rank(name);
    let mut best: Option<(u32, u32)> = None;
    for &id in book {
        let Some(rec) = np.db.spell(id) else {
            continue;
        };
        if rec.name().to_lowercase() != want {
            continue;
        }
        let r = rec.rank().to_lowercase();
        if let Some(rank) = &rank {
            if &r == rank {
                return Some(id);
            }
            continue;
        }
        let n = rank_number(&r);
        if best.is_none_or(|(_, b)| n >= b) {
            best = Some((id, n));
        }
    }
    best.map(|(id, _)| id)
}

/// A spell argument: an id, `"spellId:N"`, or a name in the book.
pub(crate) fn spell_arg(np: &Np, v: &Value) -> Option<u32> {
    if let Some(id) = match v {
        Value::Integer(_) | Value::Number(_) => int(v),
        _ => None,
    } {
        return u32::try_from(id).ok();
    }
    let name = text(v)?;
    if let Some(id) = name.strip_prefix("spellId:") {
        return id.trim().parse().ok();
    }
    let st = np.lock();
    book_spell(np, &st.mirror.book, &name).or_else(|| book_spell(np, &st.mirror.pet_book, &name))
}

pub fn install(lua: &Lua, g: &Table, np: &Np) -> mlua::Result<()> {
    set(lua, g, "GetNampowerVersion", |_, ()| {
        let (a, b, c) = crate::VERSION;
        Ok((a, b, c))
    })?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetSpellRec",
        move |lua, (id, _copy): (Value, Value)| {
            let Some(rec) = int(&id).and_then(|id| n.db.spell(id as u32)) else {
                return Ok(Value::Nil);
            };
            let t = lua.create_table()?;
            for &(name, at, count, kind) in SPELL_COLUMNS {
                t.set(name, field_value(lua, &rec, at, count, kind)?)?;
            }
            for extra in ["castTime", "rangeMin", "rangeMax"] {
                if let Some(v) = computed(&n, &rec, extra) {
                    t.set(extra, v)?;
                }
            }
            Ok(Value::Table(t))
        },
    )?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetSpellRecField",
        move |lua, (id, name, _copy): (Value, String, Value)| {
            let Some(rec) = int(&id).and_then(|id| n.db.spell(id as u32)) else {
                return Ok(Value::Nil);
            };
            if let Some((at, count, kind)) = spell_column(&name) {
                return field_value(lua, &rec, at, count, kind);
            }
            match computed(&n, &rec, &name) {
                Some(v) => Ok(Value::Number(v)),
                None => Err(mlua::Error::runtime(format!(
                    "GetSpellRecField: unknown field '{name}'"
                ))),
            }
        },
    )?;

    let n = np.clone();
    set(lua, g, "GetSpellIdForName", move |_, name: String| {
        let st = n.lock();
        Ok(book_spell(&n, &st.mirror.book, &name).unwrap_or(0))
    })?;

    // The spell an id, `"spellId:N"` or a book name names, for the bootstrap's wrappers.
    let n = np.clone();
    set(lua, g, "NP_SpellId", move |_, spell: Value| {
        Ok(spell_arg(&n, &spell).unwrap_or(0))
    })?;

    let n = np.clone();
    set(lua, g, "GetSpellNameAndRankForId", move |_, id: Value| {
        Ok(match int(&id).and_then(|id| n.db.spell(id as u32)) {
            Some(rec) => (Some(rec.name().to_string()), Some(rec.rank().to_string())),
            None => (None, None),
        })
    })?;

    let n = np.clone();
    set(lua, g, "GetSpellIconTexture", move |_, id: Value| {
        Ok(int(&id).and_then(|id| n.db.spell_icon(id as u32)))
    })?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetSpellDuration",
        move |_, (id, ignore): (Value, Value)| {
            let Some(rec) = int(&id).and_then(|id| n.db.spell(id as u32)) else {
                return Ok(None);
            };
            let base = n.db.duration_ms(rec.duration_index()).unwrap_or(0).max(0);
            if flag(&ignore) || base == 0 {
                return Ok(Some(i64::from(base)));
            }
            // Op 1, `SPELLMOD_DURATION`, applied as the client's integer applier does.
            let st = n.lock();
            Ok(Some(match st.mirror.mods.get(&(rec.id(), 1)) {
                Some(&(flat, pct)) => {
                    (i64::from(base) + i64::from(flat)) * i64::from(pct + 100) / 100
                }
                None => i64::from(base),
            }))
        },
    )?;

    let n = np.clone();
    set(lua, g, "GetSpellRangeData", move |_, index: Value| {
        Ok(match int(&index).and_then(|i| n.db.range(i as u32)) {
            Some((min, max, flags, name)) => (Some(min), Some(max), Some(flags), Some(name)),
            None => (None, None, None, None),
        })
    })?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetSpellModifiers",
        move |_, (id, op): (Value, Value)| {
            let (Some(id), Some(op)) = (int(&id), int(&op)) else {
                return Err(mlua::Error::runtime(
                    "Usage: GetSpellModifiers(spellId, modifierType)",
                ));
            };
            let st = n.lock();
            Ok(match st.mirror.mods.get(&(id as u32, op as u8)) {
                Some(&(flat, pct)) => (flat, pct, 1),
                None => (0, 0, 0),
            })
        },
    )?;

    let n = np.clone();
    set(lua, g, "GetSpellPower", move |_, mode: Option<String>| {
        let st = n.lock();
        let Some(f) = st.mirror.player_fields() else {
            return Ok(Variadic::new());
        };
        let mode = mode.unwrap_or_else(|| "net".into()).to_lowercase();
        Ok((0..7)
            .map(|school| {
                let pos = f.u32(field::PLAYER_MOD_DAMAGE_DONE_POS + school) as i32;
                let neg = f.u32(field::PLAYER_MOD_DAMAGE_DONE_NEG + school) as i32;
                f64::from(match mode.as_str() {
                    "positive" => pos,
                    "negative" => neg,
                    _ => pos + neg,
                })
            })
            .collect::<Variadic<f64>>())
    })?;

    let n = np.clone();
    set(lua, g, "IsAuraHidden", move |_, id: Value| {
        let Some(id) = int(&id) else {
            return Err(mlua::Error::runtime("Usage: IsAuraHidden(spellId)"));
        };
        Ok(n.db
            .spell(id as u32)
            .is_some_and(|r| r.aura_hidden())
            .then_some(1))
    })?;

    // `IsSpellUsable`: the caster aura state the reactive spells need, then the power cost.
    let n = np.clone();
    set(lua, g, "IsSpellUsable", move |_, spell: Value| {
        let Some(rec) = spell_arg(&n, &spell).and_then(|id| n.db.spell(id)) else {
            return Ok((0, 0));
        };
        let st = n.lock();
        let Some(f) = st.mirror.player_fields() else {
            return Ok((0, 0));
        };
        let aura_state = rec.0.u32(16);
        if aura_state != 0 && f.u32(125) & (1 << (aura_state - 1)) == 0 {
            return Ok((0, 0));
        }
        let power_type = rec.0.u32(31);
        let mut cost = i64::from(rec.0.u32(32));
        let pct = i64::from(rec.0.u32(156));
        if pct > 0 {
            cost += i64::from(f.u32(162)) * pct / 100;
        }
        let have = if power_type > 4 {
            i64::from(f.u32(field::UNIT_HEALTH))
        } else {
            i64::from(f.u32(field::UNIT_POWER1 + power_type as usize))
        };
        Ok(if have < cost { (0, 1) } else { (1, 0) })
    })?;

    // `IsSpellInRange`: single-unit spells only (-1 otherwise), against the range row with both
    // units' combat reach, as the client's range gate measures.
    let n = np.clone();
    set(
        lua,
        g,
        "IsSpellInRange",
        move |_, (spell, target): (Value, Value)| {
            let Some(rec) = spell_arg(&n, &spell).and_then(|id| n.db.spell(id)) else {
                return Ok(-1);
            };
            if !matches!(rec.implicit_target_a(0), 5 | 6 | 21 | 25) {
                return Ok(-1);
            }
            let Some((min, max, flags, _)) = n.db.range(rec.range_index()) else {
                return Ok(-1);
            };
            let st = n.lock();
            let target = match target {
                Value::Nil => st.mirror.token("target"),
                v => unit(&st, &v),
            };
            let (Some(target), Some(me)) = (target, st.mirror.token("player")) else {
                return Ok(0);
            };
            let (Some(a), Some(b)) = (
                st.mirror.positions.get(&me),
                st.mirror.positions.get(&target),
            ) else {
                return Ok(0);
            };
            let reach = |g: u64| st.mirror.objects.get(&g).map_or(1.5, |f| f.f32(130));
            let mut max = max + reach(me) + reach(target);
            if flags & 1 != 0 {
                max += 4.0 / 3.0;
            }
            let d = a.distance(*b);
            Ok(if d <= max && d >= min { 1 } else { 0 })
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rank_suffix_splits_off_the_name() {
        assert_eq!(
            split_rank("Frostbolt(Rank 1)"),
            ("frostbolt".into(), Some("rank 1".into()))
        );
        assert_eq!(split_rank(" Frostbolt "), ("frostbolt".into(), None));
        assert_eq!(rank_number("rank 11"), 11);
    }
}

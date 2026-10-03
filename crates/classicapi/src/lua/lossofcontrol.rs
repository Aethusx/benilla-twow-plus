//! `lossofcontrol/`: `C_LossOfControl`, the spell lockout cooldowns, and the
//! `LOSS_OF_CONTROL_ADDED` / `LOSS_OF_CONTROL_UPDATE` events.
//!
//! Two sources: a school lockout, inferred from an `SMSG_SPELL_COOLDOWN` for the player that
//! lists two or more spells at one duration (the interrupt's whole-school cooldown), and every
//! harmful aura on the player that imposes a crowd-control effect
//! ([`crate::spells::crowd_control`]), timed from the aura cache when it knows the aura.

use std::time::Instant;

use mlua::{IntoLuaMulti, Value};

use crate::lua::{is_number, none, to_int, Api};
use crate::spells::{self, col};
use crate::Ca;

/// `SPELL_SCHOOL_COUNT`.
pub(crate) const SCHOOLS: usize = 7;
/// `SPELL_PREVENTION_TYPE_SILENCE`, `_PACIFY`.
const PREVENT_SILENCE: i32 = 1;
const PREVENT_PACIFY: i32 = 2;

/// One effect: its type, spell (0 for a school interrupt), school mask, start and end on the aura
/// cache's clock (0 unknown).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Effect {
    pub kind: &'static str,
    pub spell: u32,
    pub school_mask: u32,
    pub start: u64,
    pub end: u64,
}

/// The school lockouts, `(start, end)` per school.
pub(crate) type Locks = [Option<(Instant, Instant)>; SCHOOLS];

/// `CooldownSub`: a player cooldown packet listing two or more spells at one duration locks the
/// first resolvable spell's school.
pub(crate) fn on_cooldowns(ca: &Ca, caster: u64, list: &[(u32, u32)]) {
    let table = spells::table(&ca.db);
    let mut st = ca.lock();
    if caster == 0 || caster != st.mirror.player || list.len() < 2 {
        return;
    }
    let first = list[0].1;
    if first == 0 || list.iter().any(|(_, d)| *d != first) {
        return;
    }
    let school = list
        .iter()
        .find_map(|(id, _)| {
            table
                .as_ref()?
                .row(*id)
                .map(|r| r.u32(col::SCHOOL) as usize)
        })
        .filter(|s| *s < SCHOOLS);
    if let Some(s) = school {
        let now = Instant::now();
        st.school_locks[s] = Some((
            now,
            now + std::time::Duration::from_millis(u64::from(first)),
        ));
    }
}

/// `BuildList`: school lockouts first, then the crowd-control auras.
pub(crate) fn effects(ca: &Ca) -> Vec<Effect> {
    let now = Instant::now();
    let (locks, to_ms) = {
        let st = ca.lock();
        let src = &st.auras;
        let epoch_now = src.now_ms();
        let to_ms = move |t: Instant| -> u64 {
            if t >= now {
                epoch_now + t.duration_since(now).as_millis() as u64
            } else {
                epoch_now.saturating_sub(now.duration_since(t).as_millis() as u64)
            }
        };
        (st.school_locks, to_ms)
    };
    let mut out: Vec<Effect> = locks
        .iter()
        .enumerate()
        .filter_map(|(s, l)| {
            let (start, end) = (*l)?;
            (end > now).then(|| Effect {
                kind: "SCHOOL_INTERRUPT",
                spell: 0,
                school_mask: 1 << s,
                start: to_ms(start),
                end: to_ms(end),
            })
        })
        .collect();
    let Some(table) = spells::table(&ca.db) else {
        return out;
    };
    for (spell, start, end) in super::aura::player_harmful(ca) {
        let Some(kind) = table.row(spell).and_then(|r| spells::crowd_control(&r)) else {
            continue;
        };
        out.push(Effect {
            kind,
            spell,
            school_mask: 0,
            start,
            end,
        });
        if out.len() >= 32 {
            break;
        }
    }
    out
}

/// `Blocks`: whether an effect stops a spell of `school` with `prevention`.
fn blocks(e: &Effect, school: u32, prevention: i32) -> bool {
    match e.kind {
        "SCHOOL_INTERRUPT" => e.school_mask & (1 << school) != 0,
        "STUN" | "FEAR" | "CONFUSE" | "CHARM" | "POSSESS" => true,
        "PACIFYSILENCE" => matches!(prevention, PREVENT_SILENCE | PREVENT_PACIFY),
        "SILENCE" => prevention == PREVENT_SILENCE,
        "PACIFY" => prevention == PREVENT_PACIFY,
        _ => false,
    }
}

/// `LockoutForSpell`: the longest timed effect that blocks the spell, `(start, end)`.
fn lockout_for(ca: &Ca, spell: u32) -> Option<(u64, u64)> {
    let table = spells::table(&ca.db)?;
    let rec = table.row(spell)?;
    let (school, prevention) = (rec.u32(col::SCHOOL), rec.i32(col::PREVENTION_TYPE));
    let now = ca.lock().auras.now_ms();
    effects(ca)
        .into_iter()
        .filter(|e| e.end > now && blocks(e, school, prevention))
        .max_by_key(|e| e.end)
        .map(|e| (if e.start == 0 { now } else { e.start }, e.end))
}

/// The frame's diff: `LOSS_OF_CONTROL_ADDED` with the 1-based index per new effect, then one
/// `LOSS_OF_CONTROL_UPDATE` for any change.
pub(crate) fn tick(ca: &Ca) -> Vec<(&'static str, Vec<benilla_app::ext::ScriptValue>)> {
    use benilla_app::ext::ScriptValue;
    let cur: Vec<(u32, u32)> = effects(ca)
        .iter()
        .map(|e| (e.spell, e.school_mask))
        .collect();
    let mut st = ca.lock();
    let prev = std::mem::replace(&mut st.loc_prev, cur.clone());
    let mut out = Vec::new();
    for (i, k) in cur.iter().enumerate() {
        if !prev.contains(k) {
            out.push((
                "LOSS_OF_CONTROL_ADDED",
                vec![ScriptValue::Number((i + 1) as f64)],
            ));
        }
    }
    if !out.is_empty() || prev.iter().any(|k| !cur.contains(k)) {
        out.push((
            "LOSS_OF_CONTROL_UPDATE",
            vec![ScriptValue::Str("player".into())],
        ));
    }
    out
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.table(
        "C_LossOfControl",
        "GetActiveLossOfControlDataCount",
        move |_, ()| Ok(effects(&c).len()),
    )?;

    let c = api.ca.clone();
    api.table(
        "C_LossOfControl",
        "GetActiveLossOfControlData",
        move |lua, v: Value| {
            if !is_number(&v) {
                return Err(mlua::Error::runtime(
                    "Usage: C_LossOfControl.GetActiveLossOfControlData(index)",
                ));
            }
            let list = effects(&c);
            let Some(e) = usize::try_from(to_int(&v) - 1)
                .ok()
                .and_then(|i| list.get(i))
            else {
                return Ok(none());
            };
            let (now, ui) = {
                let st = c.lock();
                let now = st.auras.now_ms();
                let at = |ms: u64| st.mirror.ui_time(st.auras.instant(ms));
                (now, (e.start != 0).then(|| at(e.start)))
            };
            let rec = spells::table(&c.db).and_then(|t| {
                t.row(e.spell).map(|r| {
                    (
                        r.loc(col::NAME).to_string(),
                        spells::spell_icon(&c.db, &r, false),
                    )
                })
            });
            let t = lua.create_table()?;
            t.set("locType", e.kind)?;
            t.set("spellID", e.spell)?;
            match &rec {
                Some((name, icon)) => {
                    t.set("displayText", name.as_str())?;
                    t.set("iconTexture", icon.clone().unwrap_or_default())?;
                }
                None => {
                    t.set("displayText", "Interrupted")?;
                    t.set("iconTexture", "")?;
                }
            }
            if e.end != 0 && e.end > now {
                t.set("timeRemaining", (e.end - now) as f64 * 0.001)?;
                if let Some(start) = ui {
                    t.set("startTime", start)?;
                    t.set("duration", e.end.saturating_sub(e.start) as f64 * 0.001)?;
                }
            }
            t.set("lockoutSchool", e.school_mask)?;
            t.set("priority", 0)?;
            t.set("displayType", 2)?;
            t.into_lua_multi(lua)
        },
    )?;

    // `GetSchoolLockout([mask])` -> the locked schools' mask, then the longest remaining seconds.
    let c = api.ca.clone();
    api.table(
        "C_LossOfControl",
        "GetSchoolLockout",
        move |lua, v: Value| {
            let filter = if is_number(&v) && to_int(&v) != 0 {
                to_int(&v) as u32
            } else {
                u32::MAX
            };
            let now = Instant::now();
            let locks = c.lock().school_locks;
            let mut locked = 0u32;
            let mut longest = 0.0f64;
            for (s, l) in locks.iter().enumerate() {
                let Some((_, end)) = l else {
                    continue;
                };
                if filter & (1 << s) == 0 || *end <= now {
                    continue;
                }
                locked |= 1 << s;
                longest = longest.max(end.duration_since(now).as_secs_f64());
            }
            if locked == 0 {
                0.into_lua_multi(lua)
            } else {
                (locked, longest).into_lua_multi(lua)
            }
        },
    )?;

    // The spell lockout cooldowns (`SpellCooldown.cpp`).
    let c = api.ca.clone();
    api.table(
        "C_Spell",
        "GetSpellLossOfControlCooldown",
        move |lua, v: Value| {
            let id = super::spell::resolve_spell_id(lua, &v);
            let Some(_) = spells::table(&c.db).and_then(|t| t.row(id.max(0) as u32).map(|_| ()))
            else {
                return Ok(none());
            };
            let (start, dur) = lockout(&c, id as u32);
            (start, dur).into_lua_multi(lua)
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_SpellBook",
        "GetSpellBookItemLossOfControlCooldownInfo",
        move |lua, (slot, bank): (Value, Value)| {
            let id = book_spell(lua, &slot, &bank);
            if id == 0 {
                return Ok(Value::Nil);
            }
            let (start, dur) = lockout(&c, id);
            let t = lua.create_table()?;
            t.set("startTime", start)?;
            t.set("duration", dur)?;
            t.set("modRate", 1.0)?;
            t.set("isActive", dur > 0.0)?;
            t.set("shouldReplaceNormalCooldown", outlasts_cooldown(&c, id))?;
            Ok(Value::Table(t))
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_SpellBook",
        "GetSpellBookItemLossOfControlCooldownDuration",
        move |lua, (slot, bank): (Value, Value)| {
            let id = book_spell(lua, &slot, &bank);
            if id == 0 {
                return Ok(none());
            }
            lockout(&c, id).1.into_lua_multi(lua)
        },
    )?;
    Ok(())
}

/// `SpellbookItemArgsToID`.
fn book_spell(lua: &mlua::Lua, slot: &Value, bank: &Value) -> u32 {
    if !is_number(slot) {
        return 0;
    }
    super::spell::book_slot_to_id(lua, to_int(slot), super::spell::bank_is_pet(bank))
}

/// A spell's lockout on the `GetTime()` clock, `(start, duration)`; zeros when none.
fn lockout(ca: &Ca, spell: u32) -> (f64, f64) {
    let Some((start, end)) = lockout_for(ca, spell) else {
        return (0.0, 0.0);
    };
    let st = ca.lock();
    (
        st.mirror.ui_time(st.auras.instant(start)),
        end.saturating_sub(start) as f64 * 0.001,
    )
}

/// `OutlastsNormalCooldown`: the lockout ends after the spell's own cooldown would.
fn outlasts_cooldown(ca: &Ca, spell: u32) -> bool {
    let Some((_, end)) = lockout_for(ca, spell) else {
        return false;
    };
    let table = spells::table(&ca.db);
    let rec = table.as_ref().and_then(|t| t.row(spell));
    let st = ca.lock();
    let read = crate::cooldown::query(&st.mirror, spell, 0, rec.as_ref(), Instant::now());
    if read.duration_ms == 0 {
        return true;
    }
    end.saturating_sub(st.auras.now_ms()) > u64::from(read.remaining_ms)
}

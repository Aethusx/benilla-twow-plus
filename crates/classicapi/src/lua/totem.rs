//! `totem/Tracker.cpp`: `GetTotemInfo`, `GetTotemTimeLeft`, `GetTotemDuration`, `TargetTotem`,
//! `GameTooltip:SetTotem` and `PLAYER_TOTEM_UPDATE(slot)`, the TBC totem bar 1.12 keeps nothing
//! of.
//!
//! - The slot is the summon effect's: 87-90 (`SUMMON_TOTEM_SLOT1-4`) are Fire, Earth, Water,
//!   Air; the effect's misc value is the totem's creature entry, and the duration is the spell's
//!   with the player's modifiers.
//! - Our `SMSG_SPELL_GO` of such a spell fills its slot; the slot empties at its duration, or once
//!   our totem of that entry (`UNIT_FIELD_CREATEDBY` us) has been seen and is gone (killed,
//!   recalled, destroyed). A change fires `PLAYER_TOTEM_UPDATE` the next frame.
//! - `haveTotem` is whether the slot's tool (the summon spell's `Totem[0]`, from the book) is
//!   carried, not whether a totem is down.

use std::time::{Duration, Instant};

use mlua::{IntoLuaMulti, Lua, Value};

use crate::aura::Env;
use crate::lua::{is_number, to_int, Api};
use crate::mirror::{field, Mirror};
use crate::spells::{col, EFFECTS};
use crate::Ca;

const SLOTS: usize = 4;
/// `SPELL_EFFECT_SUMMON_TOTEM_SLOT1`.
const SUMMON_TOTEM_SLOT1: u32 = 87;
const SCAN_EVERY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    active: bool,
    seen: bool,
    spell: u32,
    entry: u32,
    start: Option<Instant>,
    duration: Duration,
}

#[derive(Default)]
pub struct Totems {
    slots: [Slot; SLOTS],
    tools: [u32; SLOTS],
    dirty: u8,
    last_scan: Option<Instant>,
    /// A `TargetTotem` to select next frame.
    pub(crate) select: Option<u64>,
}

/// The slot a summon spell fills and its creature entry.
fn summon(rec: &crate::dbc::Row) -> Option<(usize, u32)> {
    (0..EFFECTS).find_map(|i| {
        let e = rec.u32(col::EFFECT + i);
        (SUMMON_TOTEM_SLOT1..SUMMON_TOTEM_SLOT1 + SLOTS as u32)
            .contains(&e)
            .then(|| {
                (
                    (e - SUMMON_TOTEM_SLOT1) as usize,
                    rec.u32(col::EFFECT_MISC_VALUE + i),
                )
            })
    })
}

/// The live guid of our totem of `entry`, if it is streamed.
fn our_totem(m: &Mirror, entry: u32) -> Option<u64> {
    m.objects.iter().find_map(|(guid, f)| {
        (crate::guid::classify(*guid) == crate::guid::Kind::Creature
            && crate::guid::creature_entry(*guid) == entry
            && f.guid(field::UNIT_CREATED_BY) == m.player)
            .then_some(*guid)
    })
}

impl Totems {
    /// `OnPlayerSpellGo`.
    pub fn on_player_spell_go(&mut self, env: &Env, spell: u32, now: Instant) {
        let Some(rec) = env.spells.and_then(|t| t.row(spell)) else {
            return;
        };
        let Some((i, entry)) = summon(&rec) else {
            return;
        };
        self.slots[i] = Slot {
            active: true,
            seen: false,
            spell,
            entry,
            start: Some(now),
            duration: Duration::from_millis(u64::from(env.duration_ms(&rec, false))),
        };
        self.dirty |= 1 << i;
        if self.tools[i] == 0 {
            self.tools[i] = rec.i32(col::TOTEM).max(0) as u32;
        }
    }

    /// Expiry, the seen-then-gone scan, and the slots whose `PLAYER_TOTEM_UPDATE` fires now.
    pub fn tick(&mut self, env: &Env, now: Instant) -> Vec<usize> {
        // The tools, from the book, until all four are known.
        if self.tools.contains(&0) {
            if let Some(spells) = env.spells {
                for &id in env.known {
                    if let Some(rec) = spells.row(id) {
                        if let Some((i, _)) = summon(&rec) {
                            if self.tools[i] == 0 {
                                self.tools[i] = rec.i32(col::TOTEM).max(0) as u32;
                            }
                        }
                    }
                }
            }
        }
        let mut any = false;
        for (i, t) in self.slots.iter_mut().enumerate() {
            if !t.active {
                continue;
            }
            if !t.duration.is_zero() && t.start.is_some_and(|s| now.duration_since(s) >= t.duration)
            {
                t.active = false;
                self.dirty |= 1 << i;
                continue;
            }
            any = true;
        }
        if any
            && env.mirror.player != 0
            && self
                .last_scan
                .is_none_or(|at| now.duration_since(at) >= SCAN_EVERY)
        {
            self.last_scan = Some(now);
            for (i, t) in self.slots.iter_mut().enumerate() {
                if !t.active {
                    continue;
                }
                if our_totem(env.mirror, t.entry).is_some() {
                    t.seen = true;
                } else if t.seen {
                    t.active = false;
                    self.dirty |= 1 << i;
                }
            }
        }
        let fired = (0..SLOTS).filter(|i| self.dirty & (1 << i) != 0).collect();
        self.dirty = 0;
        fired
    }
}

fn slot_arg(v: &Value, usage: &str) -> mlua::Result<i64> {
    if !is_number(v) {
        return Err(mlua::Error::runtime(usage.to_string()));
    }
    Ok(to_int(v))
}

/// The active slot `n` (1-4).
fn active(ca: &Ca, n: i64) -> Option<Slot> {
    let i = usize::try_from(n - 1).ok().filter(|i| *i < SLOTS)?;
    Some(ca.lock().totems.slots[i]).filter(|t| t.active)
}

fn left(t: &Slot, now: Instant) -> Duration {
    match t.start {
        Some(s) if !t.duration.is_zero() => t.duration.saturating_sub(now.duration_since(s)),
        _ => Duration::ZERO,
    }
}

fn name_icon(ca: &Ca, spell: u32) -> (String, Option<String>) {
    crate::spells::table(&ca.db)
        .and_then(|t| {
            t.row(spell).map(|r| {
                (
                    r.loc(col::NAME).to_string(),
                    crate::spells::spell_icon(&ca.db, &r, false),
                )
            })
        })
        .unwrap_or_default()
}

fn set_totem(lua: &Lua, ca: &Ca, tip: &mlua::Table, n: i64) -> mlua::Result<()> {
    use mlua::ObjectLike;
    let Some(t) = active(ca, n) else {
        return Ok(());
    };
    let (name, _) = name_icon(ca, t.spell);
    if name.is_empty() {
        return Ok(());
    }
    let color = |key: &str| -> (f64, f64, f64) {
        match lua.globals().get::<Value>(key) {
            Ok(Value::Table(c)) => (
                c.get("r").unwrap_or(1.0),
                c.get("g").unwrap_or(1.0),
                c.get("b").unwrap_or(1.0),
            ),
            _ => (1.0, 1.0, 1.0),
        }
    };
    tip.call_method::<()>("ClearLines", ())?;
    let (r, g, b) = color("NORMAL_FONT_COLOR");
    tip.call_method::<()>("AddLine", (name, r, g, b))?;
    let ms = left(&t, Instant::now()).as_millis() as i64;
    if ms > 0 {
        let (key, fallback, n) = if ms < 60_000 {
            ("SPELL_TIME_REMAINING_SEC", "%d Sec", ms / 1000)
        } else {
            ("SPELL_TIME_REMAINING_MIN", "%d Min", (ms + 59_999) / 60_000)
        };
        let template = benilla_ui::strings::global(lua, key).unwrap_or_else(|| fallback.into());
        let text = benilla_ui::strings::fill(&template, &[benilla_ui::strings::Arg::D(n)]);
        let (r, g, b) = color("HIGHLIGHT_FONT_COLOR");
        tip.call_method::<()>("AddLine", (text, r, g, b))?;
    }
    tip.call_method::<()>("Show", ())
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `haveTotem, totemName, startTime, duration, icon, modRate, spellID`.
    let c = api.ca.clone();
    api.global("GetTotemInfo", move |lua, v: Value| {
        let n = slot_arg(&v, "Usage: GetTotemInfo(slot)")?;
        let i = usize::try_from(n - 1).ok().filter(|i| *i < SLOTS);
        let (have, slot, start) = {
            let st = c.lock();
            let tool = i.map_or(0, |i| st.totems.tools[i]);
            let have = tool != 0
                && crate::items::bagged(&st.mirror, 0..=4)
                    .iter()
                    .any(|(_, _, it)| it.entry == tool);
            let slot = i.map(|i| st.totems.slots[i]).filter(|t| t.active);
            let start = slot.and_then(|t| t.start).map(|s| st.mirror.ui_time(s));
            (have, slot, start)
        };
        let Some(t) = slot else {
            return (have, "", 0, 0, Value::Nil, 1, 0).into_lua_multi(lua);
        };
        let (name, icon) = name_icon(&c, t.spell);
        (
            have,
            name,
            start.unwrap_or(0.0),
            t.duration.as_secs_f64(),
            icon,
            1,
            t.spell,
        )
            .into_lua_multi(lua)
    })?;
    let c = api.ca.clone();
    api.global("GetTotemTimeLeft", move |_, v: Value| {
        let n = slot_arg(&v, "Usage: GetTotemTimeLeft(slot)")?;
        Ok(active(&c, n).map_or(0.0, |t| left(&t, Instant::now()).as_secs_f64()))
    })?;
    let c = api.ca.clone();
    api.global("GetTotemDuration", move |_, v: Value| {
        let n = slot_arg(&v, "Usage: GetTotemDuration(slot)")?;
        Ok(active(&c, n).map_or(0.0, |t| t.duration.as_secs_f64()))
    })?;
    let c = api.ca.clone();
    api.global("TargetTotem", move |_, v: Value| {
        let n = slot_arg(&v, "Usage: TargetTotem(slot)")?;
        if let Some(t) = active(&c, n).filter(|t| t.entry != 0) {
            let mut st = c.lock();
            st.totems.select = our_totem(&st.mirror, t.entry);
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    let f = api
        .lua
        .create_function(move |lua, (tip, slot): (Value, Value)| {
            let (Value::Table(tip), true) = (&tip, is_number(&slot)) else {
                return Err(mlua::Error::runtime("Usage: GameTooltip:SetTotem(slot)"));
            };
            set_totem(lua, &c, tip, to_int(&slot))
        })?;
    // A VM without the tooltip methods (a bare test VM) has nothing to extend.
    let _ = benilla_ui::script::ext_read::replace_tooltip_method(api.lua, "SetTotem", f);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbc::Dbc;

    #[test]
    fn a_summon_fills_its_slot_and_expires() {
        let mut row = vec![0u32; 0x2B4 / 4];
        row[0] = 3599; // Searing Totem
        row[col::EFFECT] = 87;
        row[col::EFFECT_MISC_VALUE] = 2523;
        row[col::DURATION_INDEX] = 1;
        row[col::TOTEM] = 5176;
        let spells = Dbc::from_rows(&[row], b"\0");
        let durations = Dbc::from_rows(&[vec![1, 30000, 0, 30000]], b"\0");
        let mut m = Mirror::default();
        m.player = 0x10;
        let mods = crate::spellmod::Tables::default();
        let env = Env {
            spells: Some(&spells),
            durations: Some(&durations),
            mirror: &m,
            mods: &mods,
            family: 0,
            known: &[],
        };
        let mut t = Totems::default();
        let t0 = Instant::now();
        t.on_player_spell_go(&env, 3599, t0);
        assert_eq!(t.tick(&env, t0), [0]);
        assert!(t.slots[0].active && t.tools[0] == 5176);
        assert_eq!(t.slots[0].duration, Duration::from_secs(30));
        assert!(t.tick(&env, t0 + Duration::from_secs(10)).is_empty());
        assert_eq!(t.tick(&env, t0 + Duration::from_secs(30)), [0]);
        assert!(!t.slots[0].active);
    }
}

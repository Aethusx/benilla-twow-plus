//! `GetSpellIdCooldown`, `GetItemIdCooldown` and `GetTrinketCooldown`: the client's cooldown
//! list split into its three legs (the spell's own, its category's, the GCD), as the reference's
//! `GetCooldownInfo` walks it.

use std::time::{Duration, Instant};

use benilla_app::ext::CooldownRecord;
use mlua::{Lua, Table, Value};

use super::{int, set, ui_time};
use crate::lua::items::{entry, equipped};
use crate::{Np, State};

/// One leg's winning timer: start, duration and what is left.
#[derive(Clone, Copy, Default)]
struct Leg {
    start: Option<Instant>,
    duration: Duration,
    remaining: Duration,
}

impl Leg {
    fn consider(&mut self, (start, duration): (Instant, Duration), on_hold: bool, now: Instant) {
        if duration.is_zero() {
            return;
        }
        let remaining = if on_hold {
            duration
        } else {
            (start + duration).saturating_duration_since(now)
        };
        if remaining > self.remaining {
            *self = Leg {
                start: Some(start),
                duration,
                remaining,
            };
        }
    }
}

struct Legs {
    individual: Leg,
    category: (u32, Leg),
    gcd: (u32, Leg),
}

fn legs(
    records: &[CooldownRecord],
    spell_id: u32,
    item_id: u32,
    category: u32,
    gcd_category: u32,
    now: Instant,
) -> Legs {
    let mut out = Legs {
        individual: Leg::default(),
        category: (category, Leg::default()),
        gcd: (gcd_category, Leg::default()),
    };
    for r in records {
        if r.spell_id == spell_id && r.item_id == item_id {
            out.individual.consider(r.recovery, r.on_hold, now);
        }
        if (r.category != 0 && r.category == category) || r.category_wildcard {
            out.category.1.consider(r.category_recovery, r.on_hold, now);
        }
        if r.gcd_category == gcd_category && !r.gcd.1.is_zero() {
            out.gcd.1.consider(r.gcd, false, now);
        }
    }
    out
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

fn detail(lua: &Lua, legs: &Legs, item_id: u32, item_spell: u32) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let start = |leg: &Leg| leg.start.map_or(0.0, |s| ui_time(lua, s));
    let longest = legs
        .individual
        .remaining
        .max(legs.category.1.remaining)
        .max(legs.gcd.1.remaining);
    t.set("isOnCooldown", i32::from(!longest.is_zero()))?;
    t.set("cooldownRemainingMs", ms(longest))?;
    t.set("itemId", item_id)?;
    t.set("itemHasActiveSpell", i32::from(item_spell != 0))?;
    t.set("itemActiveSpellId", item_spell)?;
    let i = &legs.individual;
    t.set("individualStartS", start(i))?;
    t.set("individualDurationMs", ms(i.duration))?;
    t.set("individualRemainingMs", ms(i.remaining))?;
    t.set("isOnIndividualCooldown", i32::from(!i.remaining.is_zero()))?;
    let (category, c) = &legs.category;
    t.set(
        "categoryId",
        if c.duration.is_zero() { 0 } else { *category },
    )?;
    t.set("categoryStartS", start(c))?;
    t.set("categoryDurationMs", ms(c.duration))?;
    t.set("categoryRemainingMs", ms(c.remaining))?;
    t.set("isOnCategoryCooldown", i32::from(!c.remaining.is_zero()))?;
    let (gcd_category, gcd) = &legs.gcd;
    t.set("gcdCategoryId", *gcd_category)?;
    t.set("gcdCategoryStartS", start(gcd))?;
    t.set("gcdCategoryDurationMs", ms(gcd.duration))?;
    t.set("gcdCategoryRemainingMs", ms(gcd.remaining))?;
    t.set(
        "isOnGcdCategoryCooldown",
        i32::from(!gcd.remaining.is_zero()),
    )?;
    Ok(t)
}

/// The item's cooldown: the longest leg over its spells, keyed on the item's entry.
fn item_cooldown(np: &Np, st: &State, item_id: u32, now: Instant) -> (Legs, u32) {
    let records = &st.mirror.cooldowns;
    let use_spell = st
        .mirror
        .items
        .get(&item_id)
        .and_then(|i| i.use_spell)
        .map_or(0, |u| u.spell_id);
    let spells: Vec<u32> = st
        .mirror
        .items
        .get(&item_id)
        .map(|i| i.spells.iter().map(|s| s.spell_id).collect())
        .unwrap_or_default();
    let mut best: Option<Legs> = None;
    for spell in spells.iter().copied().chain(std::iter::once(use_spell)) {
        let (category, gcd_category) = np
            .db
            .spell(spell)
            .map_or((0, 0), |r| (r.category(), r.start_recovery_category()));
        let l = legs(records, spell, item_id, category, gcd_category, now);
        let total = |l: &Legs| l.individual.remaining.max(l.category.1.remaining);
        if best.as_ref().is_none_or(|b| total(&l) > total(b)) {
            best = Some(l);
        }
    }
    (
        best.unwrap_or_else(|| legs(records, 0, item_id, 0, 0, now)),
        use_spell,
    )
}

pub fn install(lua: &Lua, g: &Table, np: &Np) -> mlua::Result<()> {
    let n = np.clone();
    set(lua, g, "GetSpellIdCooldown", move |lua, id: Value| {
        let Some(spell) = int(&id).map(|i| i as u32) else {
            return Err(mlua::Error::runtime("Usage: GetSpellIdCooldown(spellId)"));
        };
        let (category, gcd_category) =
            n.db.spell(spell)
                .map_or((0, 0), |r| (r.category(), r.start_recovery_category()));
        let now = Instant::now();
        let l = {
            let st = n.lock();
            legs(&st.mirror.cooldowns, spell, 0, category, gcd_category, now)
        };
        detail(lua, &l, 0, 0)
    })?;

    let n = np.clone();
    set(lua, g, "GetItemIdCooldown", move |lua, id: Value| {
        let Some(item) = int(&id).map(|i| i as u32) else {
            return Err(mlua::Error::runtime("Usage: GetItemIdCooldown(itemId)"));
        };
        let (l, spell) = {
            let st = n.lock();
            item_cooldown(&n, &st, item, Instant::now())
        };
        detail(lua, &l, item, spell)
    })?;

    let n = np.clone();
    set(lua, g, "GetTrinketCooldown", move |lua, want: Value| {
        let slot: Option<i64> = g_trinket_slot(lua, &want)?;
        let Some(slot) = slot else {
            return Ok(Value::Integer(-1));
        };
        let (l, item, spell) = {
            let st = n.lock();
            let item = entry(&st, equipped(&st)[slot as usize - 1]);
            if item == 0 {
                return Ok(Value::Integer(-1));
            }
            let (l, spell) = item_cooldown(&n, &st, item, Instant::now());
            (l, item, spell)
        };
        Ok(Value::Table(detail(lua, &l, item, spell)?))
    })?;

    Ok(())
}

/// `NP_TrinketSlot`, through Lua so the lock is not held twice.
fn g_trinket_slot(lua: &Lua, want: &Value) -> mlua::Result<Option<i64>> {
    lua.globals()
        .get::<mlua::Function>("NP_TrinketSlot")?
        .call(want.clone())
}

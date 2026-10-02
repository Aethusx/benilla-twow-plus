//! Casting: the natives behind `QueueSpellByName` and the no-queue casts, `QueueScript`,
//! `ChannelStopCastingNextTick`, the cast info reads, and the CVar and `SpellTargetUnit` hooks
//! the bootstrap calls.

use mlua::{Lua, Table, Value};

use super::{flag, guid_value, int, set, text, ui_now, unit};
use crate::engine::Origin;
use crate::mirror::field;
use crate::Np;

pub fn install(lua: &Lua, g: &Table, np: &Np) -> mlua::Result<()> {
    // `NP_Cast(spellId, unit, forceQueue, noQueue)`: send a cast through the ladder at a unit (the
    // selection when nil), with nampower's queue overrides. Returns 1 when sent.
    let n = np.clone();
    set(
        lua,
        g,
        "NP_Cast",
        move |_, (spell, target, force, no_queue): (Value, Value, Value, Value)| {
            let Some(spell) = int(&spell).filter(|i| *i > 0).map(|i| i as u32) else {
                return Ok(0);
            };
            let mut st = n.lock();
            let target = match target {
                Value::Nil => None,
                v => match unit(&st, &v) {
                    Some(guid) => Some(guid),
                    None => return Ok(0),
                },
            };
            st.cast(
                spell,
                target,
                Origin::Direct {
                    force_queue: flag(&force),
                    no_queue: flag(&no_queue),
                },
            );
            Ok(1)
        },
    )?;

    // `QueueScript(script, priority)`: run now outside the queue window, else hold it.
    let n = np.clone();
    set(
        lua,
        g,
        "QueueScript",
        move |lua, (chunk, priority): (Value, Value)| {
            let Some(chunk) = text(&chunk).filter(|c| !c.is_empty()) else {
                return Err(mlua::Error::runtime(
                    "Usage: QueueScript(\"script\", (optional)priority)",
                ));
            };
            let priority = int(&priority).unwrap_or(1).clamp(1, 3) as u8;
            let queued = {
                let mut st = n.lock();
                let now = st.now_ms();
                st.engine.queue_script(chunk.clone(), priority, now)
            };
            if !queued {
                lua.load(chunk).set_name("QueueScript").exec()?;
            }
            Ok(())
        },
    )?;

    let n = np.clone();
    set(lua, g, "ChannelStopCastingNextTick", move |_, ()| {
        n.lock().engine.stop_channel_next_tick();
        Ok(())
    })?;

    // `GetCurrentCastingInfo()`: cast id, visual id, auto-repeat id, casting, channeling,
    // on-swing pending, auto-attacking.
    let n = np.clone();
    set(lua, g, "GetCurrentCastingInfo", move |_, ()| {
        let st = n.lock();
        let now = st.now_ms();
        let c = &st.engine.cast;
        let casting = c.cast_end_ms > now && c.cast_spell_id != 0;
        let channel = st
            .mirror
            .player_fields()
            .map_or(0, |f| f.u32(field::UNIT_CHANNEL_SPELL));
        let cast_id = if casting { c.cast_spell_id } else { 0 };
        let visual = if casting { c.cast_spell_id } else { channel };
        Ok((
            cast_id,
            visual,
            st.mirror.auto_repeat,
            i32::from(casting),
            i32::from(c.channeling || channel != 0),
            i32::from(c.pending_on_swing_cast),
            i32::from(st.mirror.attacking),
        ))
    })?;

    // `GetCastInfo()`: the newest cast record and the timers, or nil.
    let n = np.clone();
    set(lua, g, "GetCastInfo", move |lua, ()| {
        let ui = ui_now(lua);
        let st = n.lock();
        let now = st.now_ms();
        let Some(p) = st.engine.cast_info().copied() else {
            return Ok(Value::Nil);
        };
        let c = st.engine.cast;
        let (end, duration) = if c.channeling {
            (c.channel_end_ms, c.channel_duration_ms)
        } else {
            (c.cast_end_ms, p.cast_time_ms)
        };
        let at = |ms: u64| ui + (ms as f64 - now as f64) / 1000.0;
        let t = lua.create_table()?;
        t.set("castId", p.cast_id)?;
        t.set("spellId", p.spell_id)?;
        t.set("guid", guid_value(lua, p.target.unwrap_or(0))?)?;
        t.set("castType", p.cast_type as i32)?;
        t.set("castStartS", at(p.start_ms))?;
        t.set("castEndS", at(end))?;
        t.set("castRemainingMs", end.saturating_sub(now))?;
        t.set("castDurationMs", duration)?;
        t.set("gcdEndS", at(c.gcd_end_ms))?;
        t.set("gcdRemainingMs", c.gcd_end_ms.saturating_sub(now))?;
        Ok(Value::Table(t))
    })?;

    // The bootstrap's `SetCVar` wrapper: a written `NP_` value reaches the engine at once.
    let n = np.clone();
    set(
        lua,
        g,
        "NP_OnCVar",
        move |_, (name, value): (String, Value)| {
            if name.len() > 3 && name[..3].eq_ignore_ascii_case("np_") {
                let value = text(&value).unwrap_or_default();
                n.lock().engine.apply_setting(&name, &value);
            }
            Ok(())
        },
    )?;

    // The bootstrap's `SpellTargetUnit` wrapper: a mouseover macro retargets the queued casts.
    let n = np.clone();
    set(lua, g, "NP_Retarget", move |_, token: Value| {
        let mut st = n.lock();
        if let Some(guid) = unit(&st, &token) {
            st.engine.retarget_queued(guid);
        }
        Ok(())
    })?;

    Ok(())
}

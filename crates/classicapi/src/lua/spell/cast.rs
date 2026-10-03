//! `Cast.cpp`'s Lua surface: `C_Spell.UnitCastingInfo`, `CastingInfo`, `UnitChannelInfo`,
//! `ChannelInfo` (under `C_Spell` so an addon's own `UnitCastingInfo or <fallback>` keeps its
//! fallback) and the global `UnitSpellTargetName`, over [`crate::cast::Tracker`].

use mlua::{IntoLua, Lua, MultiValue, Value};

use crate::cast::{self, Tracked};
use crate::lua::unit::{token_arg, unit_guid};
use crate::lua::Api;
use crate::spells;
use crate::Ca;

/// One getter's answer, built under the lock and pushed after it.
struct Info {
    name: String,
    icon: String,
    /// `(start, end)` on `GetTime()`'s clock ×1000; `None` for a channel not seen to begin.
    times: Option<(f64, f64)>,
    tradeskill: bool,
    cast_id: Option<String>,
    not_interruptible: bool,
    spell: u32,
    delay: i64,
}

/// `PushCastInfo`'s 11 values, or nothing.
fn push_cast(lua: &Lua, info: Option<Info>) -> mlua::Result<MultiValue> {
    let Some(i) = info else {
        return Ok(MultiValue::new());
    };
    let (start, end) = i.times.unwrap_or_default();
    Ok(MultiValue::from_vec(vec![
        i.name.clone().into_lua(lua)?,
        i.name.into_lua(lua)?,
        i.icon.into_lua(lua)?,
        Value::Number(start),
        Value::Number(end),
        Value::Boolean(i.tradeskill),
        i.cast_id.into_lua(lua)?,
        Value::Boolean(i.not_interruptible),
        Value::Number(f64::from(i.spell)),
        Value::Nil,
        Value::Number(i.delay as f64),
    ]))
}

/// `PushChannelInfo`'s 8 values, or nothing.
fn push_channel(lua: &Lua, info: Option<Info>) -> mlua::Result<MultiValue> {
    let Some(i) = info else {
        return Ok(MultiValue::new());
    };
    let (start, end) = match i.times {
        Some((s, e)) => (Value::Number(s), Value::Number(e)),
        None => (Value::Nil, Value::Nil),
    };
    Ok(MultiValue::from_vec(vec![
        i.name.clone().into_lua(lua)?,
        i.name.into_lua(lua)?,
        i.icon.into_lua(lua)?,
        start,
        end,
        Value::Boolean(i.tradeskill),
        Value::Boolean(i.not_interruptible),
        Value::Number(f64::from(i.spell)),
    ]))
}

/// The answer for `caster`'s `t`, or `None` for an unknown spell.
fn info(ca: &Ca, caster: u64, t: Tracked, channel: bool, timed: bool) -> Option<Info> {
    ca.with_cast(|tracker, env| {
        let rec = env.spells?.row(t.spell)?;
        let (name, _) = cast::name_rank(&rec);
        let ms = |at: i64| env.mirror.ui_time(cast::instant(at)) * 1000.0;
        Some(Info {
            name,
            icon: spells::spell_icon(&ca.db, &rec, false).unwrap_or_default(),
            times: timed.then(|| (ms(t.start), ms(t.end))),
            tradeskill: cast::is_tradeskill(&rec),
            cast_id: if channel {
                None
            } else {
                tracker.current_cast_guid(env.mirror.player, caster, t.spell)
            },
            not_interruptible: tracker.not_interruptible(env, caster, &rec, channel),
            spell: t.spell,
            delay: t.delay,
        })
    })
}

/// The player's live cast or channel.
fn player_cast(ca: &Ca, channel: bool) -> Option<Info> {
    let now = cast::now_ms();
    let (player, t) = {
        let st = ca.lock();
        let t = if channel {
            st.cast.player_channel(now)
        } else {
            st.cast.player_cast(now)
        };
        (st.mirror.player, t?)
    };
    info(ca, player, t, channel, true)
}

fn unit_cast(lua: &Lua, ca: &Ca, token: &Value, usage: &str, channel: bool) -> Option<Info> {
    let token = token_arg(token, usage).ok()?;
    let guid = unit_guid(lua, &token).ok().flatten()?;
    let now = cast::now_ms();
    let (player, remote, chan) = {
        let st = ca.lock();
        let chan = st
            .mirror
            .object(guid)
            .map_or(0, |f| f.u32(crate::mirror::field::UNIT_CHANNEL_SPELL));
        (st.mirror.player, st.cast.remote.get(&guid).copied(), chan)
    };
    if guid == player {
        return player_cast(ca, channel);
    }
    let live = remote.filter(|r| now < r.end);
    if !channel {
        let r = live.filter(|r| !r.channel)?;
        let t = Tracked {
            spell: r.spell,
            start: r.start,
            end: r.end,
            delay: 0,
        };
        return info(ca, guid, t, false, true);
    }
    // The live `UNIT_CHANNEL_SPELL` says whether it channels; the cache adds times when the
    // channel was seen to begin.
    if chan == 0 {
        return None;
    }
    match live.filter(|r| r.channel && r.spell == chan) {
        Some(r) => {
            let t = Tracked {
                spell: chan,
                start: r.start,
                end: r.end,
                delay: 0,
            };
            info(ca, guid, t, true, true)
        }
        None => {
            let t = Tracked {
                spell: chan,
                ..Default::default()
            };
            info(ca, guid, t, true, false)
        }
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const CAST_USAGE: &str = "Usage: C_Spell.UnitCastingInfo(\"unit\")";
    const CHANNEL_USAGE: &str = "Usage: C_Spell.UnitChannelInfo(\"unit\")";
    let c = api.ca.clone();
    api.table("C_Spell", "UnitCastingInfo", move |lua, v: Value| {
        token_arg(&v, CAST_USAGE)?;
        push_cast(lua, unit_cast(lua, &c, &v, CAST_USAGE, false))
    })?;
    let c = api.ca.clone();
    api.table("C_Spell", "CastingInfo", move |lua, ()| {
        push_cast(lua, player_cast(&c, false))
    })?;
    let c = api.ca.clone();
    api.table("C_Spell", "UnitChannelInfo", move |lua, v: Value| {
        token_arg(&v, CHANNEL_USAGE)?;
        push_channel(lua, unit_cast(lua, &c, &v, CHANNEL_USAGE, true))
    })?;
    let c = api.ca.clone();
    api.table("C_Spell", "ChannelInfo", move |lua, ()| {
        push_channel(lua, player_cast(&c, true))
    })?;

    // Whom the unit's live cast or channel aims at, by name; nil for none or a nameless unit.
    let c = api.ca.clone();
    api.global("UnitSpellTargetName", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitSpellTargetName(\"unit\")")?;
        let caster = unit_guid(lua, &token)?.unwrap_or(0);
        let st = c.lock();
        let t = st.cast.target_of(st.mirror.player, caster, cast::now_ms());
        if t == 0 {
            return Ok(None);
        }
        Ok(st
            .mirror
            .names
            .get(&t)
            .cloned()
            .or_else(|| st.mirror.player_names.get(&t).map(|p| p.0.clone())))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn the_getters_answer_nothing_with_no_cast() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                return tostring((C_Spell.CastingInfo())) .. " " .. tostring((C_Spell.ChannelInfo()))
                  .. " " .. tostring((C_Spell.UnitCastingInfo("player")))
                  .. " " .. tostring(UnitSpellTargetName("target"))
                  .. " " .. tostring(pcall(C_Spell.UnitCastingInfo))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "nil nil nil nil false");
    }
}

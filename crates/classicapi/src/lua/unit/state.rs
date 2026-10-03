//! The player's own state: `unit/State.cpp`, `unit/LineOfSight.cpp`, `unit/MirrorTimer.cpp` and
//! `unit/Stat.cpp`.

use std::time::{Duration, Instant};

use mlua::{IntoLuaMulti, Lua, Value};

use super::token_arg;
use super::world::held;
use crate::lua::{as_string, is_number, to_int, Api};
use crate::mirror::{field, typemask};
use crate::spells::{self, col};
use crate::Ca;

/// `SPELL_AURA_MOUNTED`.
const AURA_MOUNTED: i32 = 78;
/// `UNIT_FIELD_BYTES_1` byte 3's stealth bit, the semi-transparent draw.
const BYTES_1_STEALTH: u32 = 0x0200_0000;
/// `MOVEFLAG_FALLING`, `MOVEFLAG_FALLING_FAR`, `MOVEFLAG_SWIMMING`.
const MOVEFLAG_FALLING: u32 = 0x2000;
const MOVEFLAG_FALLING_FAR: u32 = 0x4000;
const MOVEFLAG_SWIMMING: u32 = 0x0020_0000;
/// How long a `UnitInLineOfSight` keeps the sight lines traced.
const SIGHT_INTEREST: Duration = Duration::from_secs(10);

/// The engine's mirror-timer names by type (`FUN_MIRROR_TIMER_TYPE_NAME`).
pub(crate) fn timer_name(kind: u32) -> &'static str {
    match kind {
        0 => "EXHAUSTION",
        1 => "BREATH",
        2 => "FEIGNDEATH",
        _ => "UNKNOWN",
    }
}

/// `FUN_MIRROR_TIMER_LABEL`: the owning spell's name, else the `<NAME>_LABEL` global string, the
/// empty string when there is none.
fn timer_label(lua: &Lua, ca: &Ca, kind: u32, spell_id: u32) -> String {
    if spell_id != 0 {
        if let Some(name) = spells::table(&ca.db)
            .and_then(|t| t.row(spell_id).map(|r| r.loc(col::NAME).to_string()))
            .filter(|n| !n.is_empty())
        {
            return name;
        }
    }
    lua.globals()
        .get::<String>(format!("{}_LABEL", timer_name(kind)))
        .unwrap_or_default()
}

/// `1` or nil, the 1.12 predicate shape these few keep.
fn one(b: bool) -> Option<i64> {
    b.then_some(1)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `player/LoginStatus.cpp`: logged in once the player's object exists.
    let c = api.ca.clone();
    api.global("IsLoggedIn", move |_, ()| Ok(c.lock().mirror.player != 0))?;

    // Rendering a mount: `UNIT_FIELD_MOUNTDISPLAYID` nonzero.
    let c = api.ca.clone();
    api.global("IsMounted", move |_, ()| {
        Ok(c.lock()
            .mirror
            .me()
            .is_some_and(|f| f.u32(field::UNIT_MOUNT_DISPLAY_ID) != 0))
    })?;

    // The buff whose spell applies `SPELL_AURA_MOUNTED`, cancelled.
    let c = api.ca.clone();
    api.global("Dismount", move |_, ()| {
        let Some(table) = spells::table(&c.db) else {
            return Ok(());
        };
        let mut st = c.lock();
        let Some(me) = st.mirror.me() else {
            return Ok(());
        };
        if me.u32(field::UNIT_MOUNT_DISPLAY_ID) == 0 {
            return Ok(());
        }
        let hit = (0..32)
            .map(|s| me.u32(field::UNIT_AURA + s))
            .filter(|id| *id != 0)
            .find(|id| {
                table.row(*id).is_some_and(|r| {
                    (0..spells::EFFECTS)
                        .any(|e| r.i32(col::EFFECT_APPLY_AURA_NAME + e) == AURA_MOUNTED)
                })
            });
        if let Some(id) = hit {
            st.cancel_aura(id);
        }
        Ok(())
    })?;

    let c = api.ca.clone();
    api.global("IsStealthed", move |_, ()| {
        Ok(c.lock()
            .mirror
            .me()
            .is_some_and(|f| f.u32(field::UNIT_BYTES_1) & BYTES_1_STEALTH != 0))
    })?;

    let c = api.ca.clone();
    api.global("IsFalling", move |_, ()| {
        Ok(c.lock().mirror.move_flags & (MOVEFLAG_FALLING | MOVEFLAG_FALLING_FAR) != 0)
    })?;

    let c = api.ca.clone();
    api.global("IsSwimming", move |_, ()| {
        Ok(c.lock().mirror.move_flags & MOVEFLAG_SWIMMING != 0)
    })?;

    // Clicking a summoning portal: a channel spell on our descriptor whose channel object is a
    // GameObject someone else made. Deviation: the reference reads the CGPlayer's cast-object
    // pointer (`+0xB4`), which benilla does not keep; the portal's creator tells the clicker
    // from the warlock instead.
    let c = api.ca.clone();
    api.global("IsAssistingRitual", move |_, ()| {
        let st = c.lock();
        let m = &st.mirror;
        let Some(me) = m.me() else {
            return Ok(false);
        };
        if me.u32(field::UNIT_CHANNEL_SPELL) == 0 {
            return Ok(false);
        }
        let obj = me.guid(field::UNIT_CHANNEL_OBJECT);
        Ok(m.object(obj).is_some_and(|f| {
            f.is(typemask::GAMEOBJECT) && f.guid(field::GAMEOBJECT_CREATED_BY) != m.player
        }))
    })?;

    // Under a WMO roof: 1 or nil, nil before the player exists.
    let c = api.ca.clone();
    api.global("IsIndoors", move |_, ()| {
        Ok(one(c.lock().mirror.indoors == Some(true)))
    })?;
    let c = api.ca.clone();
    api.global("IsOutdoors", move |_, ()| {
        Ok(one(c.lock().mirror.indoors == Some(false)))
    })?;

    // True clear, false blocked, nil when the check cannot apply. The first ask arms the trace,
    // which answers from the next frame on.
    let c = api.ca.clone();
    api.global("UnitInLineOfSight", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitInLineOfSight(\"unit\")")?;
        let Some((guid, _)) = held(lua, &c, &token)? else {
            return Ok(None);
        };
        let mut st = c.lock();
        st.mirror.sight_wanted_until = Some(Instant::now() + SIGHT_INTEREST);
        if guid == st.mirror.player {
            return Ok(Some(true));
        }
        Ok(st.mirror.sight.get(&guid).copied())
    })?;

    // `timer, value, maxValue, scale, paused, label` for slot 1-3; the value is the last
    // packet's, as retail documents.
    let c = api.ca.clone();
    api.global("GetMirrorTimerInfo", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: GetMirrorTimerInfo(index)"));
        }
        let i = to_int(&v) - 1;
        let t = usize::try_from(i)
            .ok()
            .and_then(|i| c.lock().mirror_timers.get(i).copied().flatten());
        let Some(t) = t else {
            return Ok(mlua::MultiValue::new());
        };
        let label = timer_label(lua, &c, t.kind, t.spell_id);
        (timer_name(t.kind), t.value, t.max, t.scale, t.paused, label).into_lua_multi(lua)
    })?;

    // The live value of the named timer, ms; 0 when none runs.
    let c = api.ca.clone();
    api.global("GetMirrorTimerProgress", move |_, v: Value| {
        let Some(name) = as_string(&v) else {
            return Err(mlua::Error::runtime(
                "Usage: GetMirrorTimerProgress(\"timer\")",
            ));
        };
        let now = Instant::now();
        Ok(c.lock()
            .mirror_timers
            .iter()
            .flatten()
            .find(|t| timer_name(t.kind) == name)
            .map_or(0, |t| t.live(now)))
    })?;

    for (name, value) in [
        ("LE_UNIT_STAT_STRENGTH", 1),
        ("LE_UNIT_STAT_AGILITY", 2),
        ("LE_UNIT_STAT_STAMINA", 3),
        ("LE_UNIT_STAT_INTELLECT", 4),
        ("LE_UNIT_STAT_SPIRIT", 5),
    ] {
        api.number(name, value)?;
    }
    Ok(())
}

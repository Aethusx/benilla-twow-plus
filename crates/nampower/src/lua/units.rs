//! Unit reads: `GetUnitData`, `GetUnitField`, `GetUnitGUID`, the raid-target helpers, the
//! player's aura timers and cancels, movement state and quest ids.

use mlua::{Lua, Table, Value};

use super::{flag, guid_value, int, set, text, unit};
use crate::mirror::{field, Fields};
use crate::Np;

/// How a unit field reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    U32,
    F32,
    Guid,
    /// One byte per entry, four to a cell.
    Byte,
}

/// `UnitFields`, by name: first index, count and kind, as nampower's `unit_fields.cpp` lists them.
const UNIT_FIELDS: &[(&str, usize, usize, Kind)] = &[
    ("charm", 6, 1, Kind::Guid),
    ("summon", 8, 1, Kind::Guid),
    ("charmedBy", 10, 1, Kind::Guid),
    ("summonedBy", 12, 1, Kind::Guid),
    ("createdBy", 14, 1, Kind::Guid),
    ("target", 16, 1, Kind::Guid),
    ("persuaded", 18, 1, Kind::Guid),
    ("channelObject", 20, 1, Kind::Guid),
    ("health", 22, 1, Kind::U32),
    ("power1", 23, 1, Kind::U32),
    ("power2", 24, 1, Kind::U32),
    ("power3", 25, 1, Kind::U32),
    ("power4", 26, 1, Kind::U32),
    ("power5", 27, 1, Kind::U32),
    ("maxHealth", 28, 1, Kind::U32),
    ("maxPower1", 29, 1, Kind::U32),
    ("maxPower2", 30, 1, Kind::U32),
    ("maxPower3", 31, 1, Kind::U32),
    ("maxPower4", 32, 1, Kind::U32),
    ("maxPower5", 33, 1, Kind::U32),
    ("level", 34, 1, Kind::U32),
    ("factionTemplate", 35, 1, Kind::U32),
    ("bytes0", 36, 1, Kind::U32),
    ("virtualItemDisplay", 37, 3, Kind::U32),
    ("virtualItemInfo", 40, 6, Kind::U32),
    ("flags", 46, 1, Kind::U32),
    ("aura", 47, 48, Kind::U32),
    ("auraFlags", 95, 6, Kind::U32),
    ("auraLevels", 101, 48, Kind::Byte),
    ("auraApplications", 113, 48, Kind::Byte),
    ("auraState", 125, 1, Kind::U32),
    ("baseAttackTime", 126, 1, Kind::U32),
    ("offhandAttackTime", 127, 1, Kind::U32),
    ("rangedAttackTime", 128, 1, Kind::U32),
    ("boundingRadius", 129, 1, Kind::F32),
    ("combatReach", 130, 1, Kind::F32),
    ("displayId", 131, 1, Kind::U32),
    ("nativeDisplayId", 132, 1, Kind::U32),
    ("mountDisplayId", 133, 1, Kind::U32),
    ("minDamage", 134, 1, Kind::F32),
    ("maxDamage", 135, 1, Kind::F32),
    ("minOffhandDamage", 136, 1, Kind::F32),
    ("maxOffhandDamage", 137, 1, Kind::F32),
    ("bytes1", 138, 1, Kind::U32),
    ("petNumber", 139, 1, Kind::U32),
    ("petNameTimestamp", 140, 1, Kind::U32),
    ("petExperience", 141, 1, Kind::U32),
    ("petNextLevelExp", 142, 1, Kind::U32),
    ("dynamicFlags", 143, 1, Kind::U32),
    ("channelSpell", 144, 1, Kind::U32),
    ("modCastSpeed", 145, 1, Kind::F32),
    ("createdBySpell", 146, 1, Kind::U32),
    ("npcFlags", 147, 1, Kind::U32),
    ("npcEmoteState", 148, 1, Kind::U32),
    ("trainingPoints", 149, 1, Kind::U32),
    ("stat0", 150, 1, Kind::U32),
    ("stat1", 151, 1, Kind::U32),
    ("stat2", 152, 1, Kind::U32),
    ("stat3", 153, 1, Kind::U32),
    ("stat4", 154, 1, Kind::U32),
    ("resistances", 155, 7, Kind::U32),
    ("baseMana", 162, 1, Kind::U32),
    ("baseHealth", 163, 1, Kind::U32),
    ("bytes2", 164, 1, Kind::U32),
    ("attackPower", 165, 1, Kind::U32),
    ("attackPowerMods", 166, 1, Kind::U32),
    ("attackPowerMultiplier", 167, 1, Kind::F32),
    ("rangedAttackPower", 168, 1, Kind::U32),
    ("rangedAttackPowerMods", 169, 1, Kind::U32),
    ("rangedAttackPowerMultiplier", 170, 1, Kind::F32),
    ("minRangedDamage", 171, 1, Kind::F32),
    ("maxRangedDamage", 172, 1, Kind::F32),
    ("powerCostModifier", 173, 7, Kind::U32),
    ("powerCostMultiplier", 180, 7, Kind::F32),
];

fn one(lua: &Lua, f: &Fields, at: usize, i: usize, kind: Kind) -> mlua::Result<Value> {
    Ok(match kind {
        Kind::U32 => Value::Number(f64::from(f.u32(at + i))),
        Kind::F32 => Value::Number(f64::from(f.f32(at + i))),
        Kind::Guid => guid_value(lua, f.guid(at + 2 * i))?,
        Kind::Byte => Value::Number(f64::from(f.byte(at, i))),
    })
}

fn value(lua: &Lua, f: &Fields, at: usize, count: usize, kind: Kind) -> mlua::Result<Value> {
    if count == 1 {
        return one(lua, f, at, 0, kind);
    }
    let t = lua.create_table()?;
    for i in 0..count {
        t.set(i + 1, one(lua, f, at, i, kind)?)?;
    }
    Ok(Value::Table(t))
}

/// `MOVEFLAG_*` bits the player-state reads test.
const MOVING: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x40 | 0x80 | 0x2000 | 0x4000 | 0x0400_0000;
const ROOTED: u32 = 0x1000;
const SWIMMING: u32 = 0x0020_0000;

pub fn install(lua: &Lua, g: &Table, np: &Np) -> mlua::Result<()> {
    let n = np.clone();
    set(
        lua,
        g,
        "GetUnitData",
        move |lua, (token, _copy): (Value, Value)| {
            let st = n.lock();
            let Some(f) = unit(&st, &token).and_then(|g| st.mirror.objects.get(&g)) else {
                return Ok(Value::Nil);
            };
            let t = lua.create_table()?;
            for &(name, at, count, kind) in UNIT_FIELDS {
                t.set(name, value(lua, f, at, count, kind)?)?;
            }
            Ok(Value::Table(t))
        },
    )?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetUnitField",
        move |lua, (token, name, _copy): (Value, String, Value)| {
            let Some(&(_, at, count, kind)) = UNIT_FIELDS
                .iter()
                .find(|(n, ..)| n.eq_ignore_ascii_case(&name))
            else {
                return Err(mlua::Error::runtime(format!(
                    "GetUnitField: unknown field '{name}'"
                )));
            };
            let st = n.lock();
            match unit(&st, &token).and_then(|g| st.mirror.objects.get(&g)) {
                Some(f) => value(lua, f, at, count, kind),
                None => Ok(Value::Nil),
            }
        },
    )?;

    let n = np.clone();
    set(lua, g, "GetUnitGUID", move |lua, token: Value| {
        let st = n.lock();
        match unit(&st, &token) {
            Some(guid) => guid_value(lua, guid),
            None => Ok(Value::Nil),
        }
    })?;

    let n = np.clone();
    set(lua, g, "GetRaidTargets", move |lua, ()| {
        let st = n.lock();
        let t = lua.create_table()?;
        for (i, guid) in st.mirror.marks.iter().enumerate() {
            if *guid != 0 {
                t.set(i + 1, guid_value(lua, *guid)?)?;
            }
        }
        Ok(t)
    })?;

    // `SetLocalRaidTargetIndex(unit, index)`: index 1-8 marks the unit (moving the mark off its
    // last wearer), 0 clears the unit's mark; the client's table only, no packet.
    let n = np.clone();
    set(
        lua,
        g,
        "SetLocalRaidTargetIndex",
        move |_, (token, index): (Value, Value)| {
            let Some(index) = int(&index).filter(|i| (0..=8).contains(i)) else {
                return Err(mlua::Error::runtime(
                    "Usage: SetLocalRaidTargetIndex(unitToken, raidTargetIndex 0-8)",
                ));
            };
            let mut st = n.lock();
            let Some(guid) = unit(&st, &token) else {
                return Ok(0);
            };
            let marks = st.mirror.marks;
            for (icon, wearer) in marks.iter().enumerate() {
                if *wearer == guid {
                    st.set_local_mark(icon as u8, 0);
                    st.mirror.marks[icon] = 0;
                }
            }
            if index > 0 {
                let icon = (index - 1) as usize;
                st.set_local_mark(icon as u8, guid);
                st.mirror.marks[icon] = guid;
            }
            Ok(1)
        },
    )?;

    // `GetPlayerAuraDuration(slot)`: the raw slot's spell, its time left and its expiry on the
    // ms clock `GetWowTimeMs` would read.
    let n = np.clone();
    set(lua, g, "GetPlayerAuraDuration", move |_, slot: Value| {
        let st = n.lock();
        let Some(slot) = int(&slot)
            .filter(|s| (0..48).contains(s))
            .map(|s| s as usize)
        else {
            return Ok((None, None, None));
        };
        let Some(f) = st.mirror.player_fields() else {
            return Ok((None, None, None));
        };
        let now = st.now_ms();
        let expiry = st.mirror.aura_expiry[slot];
        let remaining = expiry.saturating_sub(now);
        Ok((
            Some(f.aura(slot)),
            Some(remaining),
            Some(if expiry > now { expiry } else { 0 }),
        ))
    })?;

    let n = np.clone();
    set(lua, g, "CancelPlayerAuraSlot", move |_, slot: Value| {
        let mut st = n.lock();
        let spell = int(&slot)
            .filter(|s| (0..48).contains(s))
            .and_then(|s| st.mirror.player_fields().map(|f| f.aura(s as usize)))
            .filter(|id| *id != 0);
        Ok(match spell {
            Some(id) => {
                st.cancel_aura(id);
                1
            }
            None => 0,
        })
    })?;

    let n = np.clone();
    set(
        lua,
        g,
        "CancelPlayerAuraSpellId",
        move |_, (spell, ignore_missing): (Value, Value)| {
            let mut st = n.lock();
            let Some(id) = int(&spell).filter(|i| *i > 0).map(|i| i as u32) else {
                return Ok(0);
            };
            let present = st
                .mirror
                .player_fields()
                .is_some_and(|f| (0..48).any(|s| f.aura(s) == id));
            if !present && !flag(&ignore_missing) {
                return Ok(0);
            }
            st.cancel_aura(id);
            Ok(1)
        },
    )?;

    for (name, mask) in [
        ("PlayerIsMoving", MOVING),
        ("PlayerIsRooted", ROOTED),
        ("PlayerIsSwimming", SWIMMING),
    ] {
        let n = np.clone();
        set(lua, g, name, move |_, ()| {
            Ok((n.lock().mirror.move_flags & mask != 0).then_some(1))
        })?;
    }

    // The quest log's quest ids, from the player's `PLAYER_QUEST_LOG_*` triples.
    let n = np.clone();
    set(lua, g, "GetQuestLogQuestIds", move |lua, _copy: Value| {
        let st = n.lock();
        let t = lua.create_table()?;
        if let Some(f) = st.mirror.player_fields() {
            let ids = (0..20)
                .map(|i| f.u32(field::PLAYER_QUEST_LOG_1_1 + 3 * i))
                .filter(|id| *id != 0);
            for (i, id) in ids.enumerate() {
                t.set(i + 1, id)?;
            }
        }
        Ok(t)
    })?;

    let n = np.clone();
    set(lua, g, "GetQuestDialogQuestId", move |_, ()| {
        Ok(n.lock().mirror.quest_dialog)
    })?;

    // The unit token a name resolves to, for the bootstrap's unit-taking wrappers.
    let n = np.clone();
    set(lua, g, "NP_ResolveUnit", move |lua, token: Value| {
        let st = n.lock();
        match text(&token).and_then(|_| unit(&st, &token)) {
            Some(guid) => guid_value(lua, guid),
            None => Ok(Value::Nil),
        }
    })?;

    Ok(())
}

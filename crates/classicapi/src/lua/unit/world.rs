//! `unit/Pet.cpp`, `unit/Position.cpp`, `unit/Power.cpp` and `unit/Range.cpp`: ownership, world
//! position, the modern power API and player-to-unit range.

use mlua::{IntoLuaMulti, Lua, Value};

use super::{token_arg, unit_guid};
use crate::lua::{is_number, to_int, truthy, Api};
use crate::mirror::{field, Fields};
use crate::Ca;

/// `UNIT_FLAG_PLAYER_CONTROLLED`.
const UNIT_FLAG_PLAYER_CONTROLLED: u32 = 0x08;
/// `VAR_UNIT_POWER_DIVISOR_TABLE`: rage stores 0..1000 and shows 0..100, happiness by 1000.
const POWER_DIVISOR: [u32; 5] = [1, 10, 1, 1, 1000];
/// `UnitInRange`'s healing range.
const IN_RANGE_YARDS: f32 = 40.0;

/// `Game::ResolveUnitToken`: the held unit a token names, its guid and descriptor; raises
/// `Unknown unit name` for a token no grammar knows, `None` for nobody or an object not held.
pub(crate) fn held(lua: &Lua, ca: &Ca, token: &str) -> mlua::Result<Option<(u64, Fields)>> {
    let Some(guid) = unit_guid(lua, token)? else {
        return Ok(None);
    };
    Ok(ca
        .lock()
        .mirror
        .object(guid)
        .filter(|f| f.is(crate::mirror::typemask::UNIT))
        .cloned()
        .map(|f| (guid, f)))
}

/// A tolerant predicate's unit: a string token's held unit, or nothing, never an error.
fn held_quiet(lua: &Lua, ca: &Ca, v: &Value) -> Option<(u64, Fields)> {
    let Value::String(s) = v else {
        return None;
    };
    held(lua, ca, &s.to_string_lossy()).ok().flatten()
}

/// `OwnerGuid`: `UNIT_FIELD_CHARMEDBY`, else `UNIT_FIELD_CREATEDBY`.
fn owner(f: &Fields) -> u64 {
    [field::UNIT_CHARMED_BY, field::UNIT_CREATED_BY]
        .into_iter()
        .map(|i| f.guid(i))
        .find(|g| *g != 0)
        .unwrap_or(0)
}

fn is_player_guid(g: u64) -> bool {
    crate::guid::classify(g) == crate::guid::Kind::Player
}

/// The `(position, bounding radius)` of a token's held unit; `None` when it has no place.
fn pos_and_reach(lua: &Lua, ca: &Ca, token: &str) -> mlua::Result<Option<([f32; 3], f32)>> {
    let Some(guid) = unit_guid(lua, token)? else {
        return Ok(None);
    };
    let st = ca.lock();
    let Some(place) = st.mirror.place(guid) else {
        return Ok(None);
    };
    let reach = st
        .mirror
        .object(guid)
        .map_or(0.0, |f| f.f32(field::UNIT_BOUNDING_RADIUS));
    Ok(Some((place.pos, reach)))
}

fn dist_sq(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// `ResolveArgs`: the unit's descriptor and the power type, the unit's own when the argument is
/// absent or outside 0..=4.
fn power_args(lua: &Lua, ca: &Ca, token: &str, t: &Value) -> mlua::Result<Option<(Fields, usize)>> {
    let Some((_, f)) = held(lua, ca, token)? else {
        return Ok(None);
    };
    let mut kind = if is_number(t) { to_int(t) } else { -1 };
    if !(0..=4).contains(&kind) {
        kind = i64::from(f.power_type());
    }
    if !(0..=4).contains(&kind) {
        return Ok(None);
    }
    Ok(Some((f, kind as usize)))
}

#[derive(Clone, Copy)]
enum PowerRead {
    Current,
    Max,
    Missing,
}

/// The three power reads; an out-of-range groupmate answers off the roster's primary power, 0
/// for any other type.
fn power(
    lua: &Lua,
    ca: &Ca,
    token: &str,
    t: &Value,
    raw: &Value,
    read: PowerRead,
) -> mlua::Result<u32> {
    let (cur, max, kind) = match power_args(lua, ca, token, t)? {
        Some((f, k)) => (
            f.u32(field::UNIT_POWER1 + k),
            f.u32(field::UNIT_MAX_POWER1 + k),
            k,
        ),
        None => {
            let Some(s) = benilla_ui::script::ext_read::unit_state(lua, token)? else {
                return Ok(0);
            };
            let want = if is_number(t) { to_int(t) } else { -1 };
            if (0..=4).contains(&want) && want != i64::from(s.power_type) {
                return Ok(0);
            }
            if s.power_type > 4 {
                return Ok(0);
            }
            (s.power, s.max_power, usize::from(s.power_type))
        }
    };
    let divisor = if truthy(raw) { 1 } else { POWER_DIVISOR[kind] };
    let (cur, max) = (cur / divisor, max / divisor);
    Ok(match read {
        PowerRead::Current => cur,
        PowerRead::Max => max,
        PowerRead::Missing => max.saturating_sub(cur),
    })
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.global("UnitIsMinion", move |lua, v: Value| {
        let Some((guid, f)) = held_quiet(lua, &c, &v) else {
            return Ok(false);
        };
        let minion = is_player_guid(owner(&f))
            || (f.u32(field::UNIT_FLAGS) & UNIT_FLAG_PLAYER_CONTROLLED != 0
                && !is_player_guid(guid));
        Ok(minion)
    })?;

    // A player's pet, not a guardian: owned by a player and a Beast, the engine's own
    // "X's Pet" against "X's Minion" test (`(FUN_00605570(unit) != 1) + 1`).
    let c = api.ca.clone();
    api.global("UnitIsPet", move |lua, v: Value| {
        let Some((guid, f)) = held_quiet(lua, &c, &v) else {
            return Ok(false);
        };
        if !is_player_guid(owner(&f)) {
            return Ok(false);
        }
        let beast = c.lock().mirror.creatures.get(&guid).map(|c| c.0) == Some(1);
        Ok(beast)
    })?;

    let c = api.ca.clone();
    api.global("UnitIsOtherPlayersPet", move |lua, v: Value| {
        let Some((_, f)) = held_quiet(lua, &c, &v) else {
            return Ok(false);
        };
        let o = owner(&f);
        let me = c.lock().mirror.player;
        Ok(is_player_guid(o) && o != me)
    })?;

    let c = api.ca.clone();
    api.global("UnitOwnerGUID", move |lua, v: Value| {
        Ok(held_quiet(lua, &c, &v)
            .map(|(_, f)| owner(&f))
            .filter(|g| *g != 0)
            .map(crate::guid::format))
    })?;

    let c = api.ca.clone();
    api.global("UnitCreatedBySpell", move |lua, v: Value| {
        Ok(held_quiet(lua, &c, &v)
            .map(|(_, f)| f.u32(field::UNIT_CREATED_BY_SPELL))
            .filter(|s| *s != 0))
    })?;

    // `UnitPosition(unit) -> posY, posX, posZ, instanceID`: retail's order (west, north, up),
    // any visible unit, the loaded map as the instance.
    let c = api.ca.clone();
    let position = move |lua: &Lua, v: Value| -> mlua::Result<mlua::MultiValue> {
        let token = token_arg(&v, "Usage: UnitPosition(\"unit\")")?;
        match pos_and_reach(lua, &c, &token)? {
            Some((p, _)) => {
                let map = c.lock().mirror.map_id;
                (p[1], p[0], p[2], map).into_lua_multi(lua)
            }
            None => Value::Nil.into_lua_multi(lua),
        }
    };
    api.global("UnitPosition", position.clone())?;
    api.table("C_PlayerInfo", "UnitPosition", position)?;

    let c = api.ca.clone();
    api.global(
        "UnitPower",
        move |lua, (v, t, raw): (Value, Value, Value)| {
            let token = token_arg(&v, "Usage: UnitPower(\"unit\" [, type [, unmodified]])")?;
            power(lua, &c, &token, &t, &raw, PowerRead::Current)
        },
    )?;
    let c = api.ca.clone();
    api.global(
        "UnitPowerMax",
        move |lua, (v, t, raw): (Value, Value, Value)| {
            let token = token_arg(&v, "Usage: UnitPowerMax(\"unit\" [, type [, unmodified]])")?;
            power(lua, &c, &token, &t, &raw, PowerRead::Max)
        },
    )?;
    let c = api.ca.clone();
    api.global(
        "UnitPowerMissing",
        move |lua, (v, t, raw): (Value, Value, Value)| {
            let token = token_arg(
                &v,
                "Usage: UnitPowerMissing(\"unit\" [, powerType [, unmodified]])",
            )?;
            power(lua, &c, &token, &t, &raw, PowerRead::Missing)
        },
    )?;

    // `UnitPowerType(unit)`: the stock verb's answer, then the modern token.
    let stock: Option<mlua::Function> = api.lua.globals().get("UnitPowerType").ok();
    api.global("UnitPowerType", move |lua, args: mlua::MultiValue| {
        let Some(stock) = &stock else {
            return Ok(mlua::MultiValue::new());
        };
        let out: mlua::MultiValue = stock.call(args)?;
        let Some(first) = out.front().cloned() else {
            return Ok(out);
        };
        let kind = to_int(&first);
        (first, crate::spells::power_token(kind)).into_lua_multi(lua)
    })?;
    api.int_enum(
        "Enum",
        "PowerType",
        &[
            ("HealthCost", -2),
            ("None", -1),
            ("Mana", 0),
            ("Rage", 1),
            ("Focus", 2),
            ("Energy", 3),
            ("Happiness", 4),
        ],
    )?;

    // Reach-aware, as the engine's spell range is: 40 yards plus both units' bounding radii.
    // `UnitInRange("player")` is false, false, as retail answers it.
    let c = api.ca.clone();
    api.global("UnitInRange", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitInRange(\"unit\")")?;
        if token == "player" {
            return Ok((false, false));
        }
        let (Some((u, ur)), Some((p, pr))) = (
            pos_and_reach(lua, &c, &token)?,
            pos_and_reach(lua, &c, "player")?,
        ) else {
            return Ok((false, false));
        };
        let range = IN_RANGE_YARDS + pr + ur;
        Ok((dist_sq(u, p) <= range * range, true))
    })?;

    // Centre to centre, squared; `(0, false)` when either position is unknown.
    let c = api.ca.clone();
    api.global("UnitDistanceSquared", move |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitDistanceSquared(\"unit\")")?;
        match (
            pos_and_reach(lua, &c, &token)?,
            pos_and_reach(lua, &c, "player")?,
        ) {
            (Some((u, _)), Some((p, _))) => Ok((dist_sq(u, p), true)),
            _ => Ok((0.0, false)),
        }
    })?;
    Ok(())
}

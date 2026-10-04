//! `map/Waypoint.cpp`: the one player-placed map pin, a `uiMapID`, a 0..1 position on it and an
//! optional `z`, held for the session (a UI reload keeps it), with `USER_WAYPOINT_UPDATED` on
//! every change. Another map sees it re-projected through world coordinates.

use mlua::{Lua, Table, Value};

use super::area::Maps;
use super::{read_vec2, vec2};
use crate::lua::{as_string, is_number, to_int, to_number, Api};

/// The pin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Waypoint {
    ui_map_id: i64,
    x: f64,
    y: f64,
    z: Option<f64>,
}

/// `MapSupportsWaypoint`: a map with a real rect (the World row's is all zeroes).
fn supports(maps: Option<&Maps>, ui: i64) -> bool {
    maps.and_then(|m| m.for_ui(ui))
        .is_some_and(|w| w.percent(0.0, 0.0).is_some())
}

fn point_table(lua: &Lua, ui: i64, x: f64, y: f64, z: Option<f64>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("uiMapID", ui)?;
    t.set("position", vec2(lua, x, y)?)?;
    if let Some(z) = z {
        t.set("z", z)?;
    }
    Ok(t)
}

/// `ReadUiMapPoint`.
fn read_point(v: &Value) -> mlua::Result<Option<Waypoint>> {
    let Value::Table(t) = v else {
        return Ok(None);
    };
    let id: Value = t.get("uiMapID")?;
    if !is_number(&id) {
        return Ok(None);
    }
    let Some((x, y)) = read_vec2(&t.get("position")?)? else {
        return Ok(None);
    };
    let z: Value = t.get("z")?;
    Ok(Some(Waypoint {
        ui_map_id: to_int(&id),
        x,
        y,
        z: is_number(&z).then(|| to_number(&z)),
    }))
}

/// `strtol` over a decimal field: its value and the rest, or `None` with no digits.
fn strtol(s: &str) -> Option<(i64, &str)> {
    let t = s.trim_start();
    let digits = t
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')))
        .count();
    let n = t[..digits].parse().ok()?;
    Some((n, &t[digits..]))
}

/// `ParseWorldmapLink`: `worldmap:<uiMapID>:<x>:<y>` anywhere in the string, coordinates × 10000.
fn parse_link(s: &str) -> Option<(i64, f64, f64)> {
    let p = &s[s.find("worldmap:")? + 9..];
    let (id, p) = strtol(p)?;
    let (xi, p) = strtol(p.strip_prefix(':')?)?;
    let (yi, _) = strtol(p.strip_prefix(':')?)?;
    Some((id, xi as f64 / 10000.0, yi as f64 / 10000.0))
}

fn fire(lua: &Lua) {
    benilla_ui::script::ext_read::fire_event(lua, "USER_WAYPOINT_UPDATED", vec![]);
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_Map";
    let c = api.ca.clone();
    api.table(NS, "CanSetUserWaypointOnMap", move |_, v: Value| {
        let maps = Maps::load(&c.db);
        Ok(is_number(&v) && supports(maps.as_deref(), to_int(&v)))
    })?;
    let c = api.ca.clone();
    api.table(NS, "SetUserWaypoint", move |lua, v: Value| {
        let maps = Maps::load(&c.db);
        let Some(wp) = read_point(&v)?.filter(|w| supports(maps.as_deref(), w.ui_map_id)) else {
            return Ok(false);
        };
        c.lock().waypoint = Some(wp);
        fire(lua);
        Ok(true)
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetUserWaypoint", move |lua, ()| {
        let wp = c.lock().waypoint;
        match wp {
            Some(w) => Ok(Value::Table(point_table(lua, w.ui_map_id, w.x, w.y, w.z)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let c = api.ca.clone();
    api.table(NS, "HasUserWaypoint", move |_, ()| {
        Ok(c.lock().waypoint.is_some())
    })?;
    let c = api.ca.clone();
    api.table(NS, "ClearUserWaypoint", move |lua, ()| {
        let had = c.lock().waypoint.take().is_some();
        if had {
            fire(lua);
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetUserWaypointPositionForMap", move |lua, v: Value| {
        let wp = c.lock().waypoint;
        let (Some(w), true) = (wp, is_number(&v)) else {
            return Ok(Value::Nil);
        };
        let want = to_int(&v);
        let at = if want == w.ui_map_id {
            Some((w.x, w.y))
        } else {
            Maps::load(&c.db).and_then(|m| {
                let (wx, wy) = m.for_ui(w.ui_map_id)?.world(w.x, w.y)?;
                m.for_ui(want)?
                    .percent(wx as f32, wy as f32)
                    .filter(|&(px, py)| (0.0..=1.0).contains(&px) && (0.0..=1.0).contains(&py))
            })
        };
        match at {
            Some((x, y)) => vec2(lua, x, y),
            None => Ok(Value::Nil),
        }
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetUserWaypointHyperlink", move |_, ()| {
        let wp = c.lock().waypoint;
        Ok(wp.map(|w| {
            let xi = (w.x * 10000.0 + 0.5) as i64;
            let yi = (w.y * 10000.0 + 0.5) as i64;
            format!(
                "|cffffff00|Hworldmap:{}:{xi}:{yi}|h[Map Pin Location]|h|r",
                w.ui_map_id
            )
        }))
    })?;
    api.table(
        NS,
        "GetUserWaypointFromHyperlink",
        |lua, v: Value| match as_string(&v).as_deref().and_then(parse_link) {
            Some((id, x, y)) => Ok(Value::Table(point_table(lua, id, x, y, None)?)),
            None => Ok(Value::Nil),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worldmap_link_parses_bare_or_wrapped() {
        assert_eq!(
            parse_link("|cffffff00|Hworldmap:14:2500:7500|h[Map Pin Location]|h|r"),
            Some((14, 0.25, 0.75))
        );
        assert_eq!(parse_link("worldmap:-13:1:2"), Some((-13, 0.0001, 0.0002)));
        assert_eq!(parse_link("worldmap:14:x:1"), None);
        assert_eq!(parse_link("nothing"), None);
    }
}

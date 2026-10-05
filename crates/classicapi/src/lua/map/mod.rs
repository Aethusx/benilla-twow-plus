//! `map/`: `C_Map` and `C_MapExplorationInfo` over `WorldMapArea.dbc`, `WorldMapOverlay.dbc`,
//! `AreaTable.dbc`, `AreaTrigger.dbc` and `Map.dbc`.
//!
//! - `Info.cpp`: `GetBestMapForUnit`, the unit's zone as an `AreaTable` id (1.12 has no
//!   `UiMap.db2`): the player's from the area under its feet raised to its zone, a group member's
//!   from the stats packets; anyone else nil.
//! - `WorldSize.cpp`, `Areas.cpp`, `AreaTriggers.cpp`: a zone's yard size, the area names, the
//!   art-folder → area map, and the trigger volumes with the zone each falls in.
//! - `Coordinates.cpp`: unit positions on a map, world↔map, and a zone's rect on another map.
//! - `MapInfo.cpp`: the World → continent → zone tree, `Enum.UIMapType` 1 World, 2 Continent,
//!   3 Zone, 4 Dungeon, and the background art (one 1002×668 layer of 256 tiles).
//! - `Overlays.cpp`, `MapExploration.cpp` ([`overlays`]), `Waypoint.cpp` ([`waypoint`]).
//!
//! With no id, the zone readers take the world map's displayed sheet.

mod area;
mod overlays;
mod taxi;
mod waypoint;

use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use self::area::{Maps, Wma, CANVAS_H, CANVAS_W, TILE};
use self::overlays::Filter;
use crate::lua::{is_number, none, to_int, to_number, to_str, Api};
use crate::Ca;

pub(crate) use self::waypoint::Waypoint;

/// The background tiles a map draws (`NUM_WORLDMAP_DETAIL_TILES`).
const DETAIL_TILES: u32 = 12;

/// `AreaTable.dbc`: the parent zone, and the localized name (enUS slot).
const AREA_PARENT: usize = 2;
const AREA_NAME: usize = 11;
/// `Map.dbc`: the instance type (0 world, 1 dungeon, 2 raid, 3 battleground) and the localized
/// name.
const MAP_INSTANCE_TYPE: usize = 2;
const MAP_NAME: usize = 4;

/// `PushVector2D`: a `Vector2DMixin` through the `CreateVector2D` global, else a plain `{x, y}`.
pub(super) fn vec2(lua: &Lua, x: f64, y: f64) -> mlua::Result<Value> {
    if let Ok(f) = lua.globals().get::<mlua::Function>("CreateVector2D") {
        if let Ok(v) = f.call::<Value>((x, y)) {
            return Ok(v);
        }
    }
    let t = lua.create_table()?;
    t.set("x", x)?;
    t.set("y", y)?;
    Ok(Value::Table(t))
}

/// `ReadVector2D`: a table's `x` and `y`, 0 for a non-number; `None` for a non-table.
pub(super) fn read_vec2(v: &Value) -> mlua::Result<Option<(f64, f64)>> {
    let Value::Table(t) = v else {
        return Ok(None);
    };
    Ok(Some((
        to_number(&t.get::<Value>("x")?),
        to_number(&t.get::<Value>("y")?),
    )))
}

/// `DBC::AreaName(id, resolveToParent = false)`: the localized name, `None` when empty.
fn area_name(ca: &Ca, id: u32) -> Option<String> {
    let name = ca.db.get("AreaTable")?.row(id)?.loc(AREA_NAME).to_string();
    (!name.is_empty()).then_some(name)
}

fn map_name(ca: &Ca, map_id: i32) -> Option<String> {
    let name = ca
        .db
        .get("Map")?
        .row_or_zero(u32::try_from(map_id).ok()?)?
        .loc(MAP_NAME)
        .to_string();
    (!name.is_empty()).then_some(name)
}

fn map_instance_type(ca: &Ca, map_id: i32) -> Option<u32> {
    Some(
        ca.db
            .get("Map")?
            .row_or_zero(u32::try_from(map_id).ok()?)?
            .u32(MAP_INSTANCE_TYPE),
    )
}

/// An area raised to its top-level zone through `AreaTable`'s parent column, the id the engine
/// keeps at `VAR_PLAYER_AREA_ID`.
fn top_zone(ca: &Ca, mut area: u32) -> u32 {
    let Some(table) = ca.db.get("AreaTable") else {
        return area;
    };
    for _ in 0..8 {
        match table.row(area).map(|r| r.u32(AREA_PARENT)) {
            Some(p) if p != 0 => area = p,
            _ => break,
        }
    }
    area
}

/// `CurrentViewRow`: the WorldMapArea row of the world map's displayed sheet.
fn view_row(lua: &Lua, maps: &Maps) -> Option<u32> {
    use benilla_ui::script::ext_read::WorldMapSheet as S;
    Some(match benilla_ui::script::ext_read::world_map_sheet(lua)? {
        S::World => maps.world()?.row,
        S::Continent(name) => {
            maps.areas
                .iter()
                .find(|w| w.area_id == 0 && w.name.eq_ignore_ascii_case(&name))?
                .row
        }
        S::Zone(area) => maps.for_area(area)?.row,
        S::Direct(row) => row,
    })
}

/// A zone reader's row: an `AreaTable` id argument, else the displayed sheet.
fn zone_row(lua: &Lua, maps: &Maps, v: &Value) -> Option<u32> {
    if is_number(v) {
        maps.for_area(to_int(v) as u32).map(|w| w.row)
    } else {
        view_row(lua, maps)
    }
}

fn number_or(v: &Value) -> Option<i64> {
    is_number(v).then(|| to_int(v))
}

/// `PushMapInfo`: the `UiMapDetails` table for a `uiMapID`, `None` when it names no map.
fn map_info(lua: &Lua, ca: &Ca, maps: &Maps, ui: i64) -> mlua::Result<Option<Table>> {
    let Some(w) = maps.for_ui(ui) else {
        return Ok(None);
    };
    let is_world = |w: &Wma| w.area_id == 0 && w.name == "World";
    let (name, ty, parent) = if ui > 0 {
        let parent = maps.continent(w.map_id).map_or(0, |c| -i64::from(c.row));
        (area_name(ca, ui as u32), 3, parent)
    } else if is_world(w) {
        (Some(w.name.clone()), 1, 0)
    } else {
        let name = map_name(ca, w.map_id).or_else(|| Some(w.name.clone()));
        if map_instance_type(ca, w.map_id) == Some(0) {
            (name, 2, maps.world().map_or(0, |r| -i64::from(r.row)))
        } else {
            (name, 4, 0)
        }
    };
    let t = lua.create_table()?;
    t.set("mapID", ui)?;
    t.set("name", name)?;
    t.set("mapType", ty)?;
    t.set("parentMapID", parent)?;
    t.set("flags", 0)?;
    Ok(Some(t))
}

/// `Interface\WorldMap\<dir>\<dir><n>`, the form `SetTexture` takes.
fn tile_path(dir: &str, n: u32) -> String {
    format!("Interface\\WorldMap\\{dir}\\{dir}{n}")
}

/// `DirHasArt`: the folder's first background tile ships.
fn has_art(ca: &Ca, dir: Option<&str>) -> bool {
    dir.filter(|d| !d.is_empty())
        .is_some_and(|d| ca.db.exists(&format!("{}.blp", tile_path(d, 1))))
}

/// The unit's zone for `GetBestMapForUnit`, `None` for nil.
fn best_map(lua: &Lua, ca: &Ca, token: &str) -> Option<u32> {
    let guid = benilla_ui::script::ext_read::unit_guid(lua, token)
        .ok()
        .flatten()?;
    let st = ca.lock();
    let area = if guid == st.mirror.player {
        let leaf = st.mirror.area?;
        drop(st);
        top_zone(ca, leaf)
    } else {
        *st.mirror.member_zones.get(&guid)?
    };
    (area != 0).then_some(area)
}

/// One `AreaTrigger.dbc` row's info table.
fn trigger_info(lua: &Lua, maps: Option<&Maps>, r: crate::dbc::Row<'_>) -> mlua::Result<Table> {
    let map_id = r.i32(1);
    let (x, y, z) = (r.f32(2), r.f32(3), r.f32(4));
    let (len, wid, hgt) = (r.f32(6), r.f32(7), r.f32(8));
    let t = lua.create_table()?;
    t.set("id", r.id())?;
    t.set("mapID", map_id)?;
    t.set("x", x)?;
    t.set("y", y)?;
    t.set("z", z)?;
    t.set("radius", r.f32(5))?;
    t.set("isBox", len != 0.0 || wid != 0.0 || hgt != 0.0)?;
    t.set("boxLength", len)?;
    t.set("boxWidth", wid)?;
    t.set("boxHeight", hgt)?;
    t.set("boxYaw", r.f32(9))?;
    if let Some((area, mx, my)) = maps.and_then(|m| m.zone_percent(map_id, x, y)) {
        t.set("areaID", area)?;
        t.set("mapX", mx)?;
        t.set("mapY", my)?;
    }
    Ok(t)
}

fn on_map(px: f64, py: f64) -> bool {
    (0.0..=1.0).contains(&px) && (0.0..=1.0).contains(&py)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_Map";
    let c = api.ca.clone();
    api.table(NS, "GetBestMapForUnit", move |lua, v: Value| {
        let Value::String(_) = v else {
            return Ok(None);
        };
        Ok(to_str(&v).and_then(|t| best_map(lua, &c, &t)))
    })?;

    let c = api.ca.clone();
    api.table(NS, "GetMapWorldSize", move |lua, v: Value| {
        let size = Maps::load(&c.db).and_then(|m| {
            let w = m.row(zone_row(lua, &m, &v)?)?;
            Some((w.left - w.right, w.top - w.bottom))
        });
        Ok(match size {
            Some(s) => s.into_lua_multi(lua)?,
            None => none(),
        })
    })?;

    let c = api.ca.clone();
    api.table(NS, "GetAreaInfo", move |_, v: Value| {
        let id = number_or(&v).filter(|&i| i > 0);
        Ok(id.and_then(|i| area_name(&c, i as u32)))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetAreas", move |lua, ()| {
        let out = lua.create_table()?;
        if let Some(t) = c.db.get("AreaTable") {
            for r in t.rows() {
                let name = r.loc(AREA_NAME);
                if !name.is_empty() {
                    out.raw_set(r.id(), name)?;
                }
            }
        }
        Ok(out)
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetMapAreaIDs", move |lua, ()| {
        let out = lua.create_table()?;
        if let Some(m) = Maps::load(&c.db) {
            for w in m
                .areas
                .iter()
                .filter(|w| w.area_id != 0 && !w.name.is_empty())
            {
                out.set(w.name.as_str(), w.area_id)?;
            }
        }
        Ok(out)
    })?;

    let c = api.ca.clone();
    api.table(NS, "GetAreaTriggerInfo", move |lua, v: Value| {
        let maps = Maps::load(&c.db);
        let (Some(id), Some(t)) = (number_or(&v), c.db.get("AreaTrigger")) else {
            return Ok(Value::Nil);
        };
        match u32::try_from(id).ok().and_then(|id| t.row(id)) {
            Some(r) => Ok(Value::Table(trigger_info(lua, maps.as_deref(), r)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetAreaTriggers", move |lua, v: Value| {
        let want = number_or(&v);
        let maps = Maps::load(&c.db);
        let out = lua.create_table()?;
        if let Some(t) = c.db.get("AreaTrigger") {
            let mut n = 0;
            for r in t
                .rows()
                .filter(|r| want.is_none_or(|m| i64::from(r.i32(1)) == m))
            {
                n += 1;
                out.raw_set(n, trigger_info(lua, maps.as_deref(), r)?)?;
            }
        }
        Ok(out)
    })?;

    // `GetPlayerMapPosition(uiMapID, unit)`: the unit's 0..1 position on the map, nil off it; a
    // token no grammar knows raises as unit functions do.
    let c = api.ca.clone();
    api.table(
        NS,
        "GetPlayerMapPosition",
        move |lua, (id, unit): (Value, Value)| {
            let (Some(ui), Some(token)) = (number_or(&id), to_str(&unit)) else {
                return Ok(Value::Nil);
            };
            let Some(guid) = benilla_ui::script::ext_read::unit_guid(lua, &token)? else {
                return Ok(Value::Nil);
            };
            let pos = c.lock().mirror.place(guid).map(|p| p.pos);
            let at = pos.and_then(|p| {
                Maps::load(&c.db)?
                    .for_ui(ui)?
                    .percent(p[0], p[1])
                    .filter(|&(x, y)| on_map(x, y))
            });
            match at {
                Some((x, y)) => vec2(lua, x, y),
                None => Ok(Value::Nil),
            }
        },
    )?;

    let c = api.ca.clone();
    api.table(
        NS,
        "GetWorldPosFromMapPos",
        move |lua, (id, pos): (Value, Value)| {
            let Some(ui) = number_or(&id) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let Some((mx, my)) = read_vec2(&pos)? else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let at = Maps::load(&c.db).and_then(|m| {
                let w = m.for_ui(ui)?;
                Some((w.map_id, w.world(mx, my)?))
            });
            match at {
                Some((map, (x, y))) => (map, vec2(lua, x, y)?).into_lua_multi(lua),
                None => Value::Nil.into_lua_multi(lua),
            }
        },
    )?;

    // `GetMapPosFromWorldPos(continentID, worldPos [, overrideUiMapID])`: into the override map
    // when given (nil off it), else into the zone the point falls in.
    let c = api.ca.clone();
    api.table(
        NS,
        "GetMapPosFromWorldPos",
        move |lua, (cont, pos, over): (Value, Value, Value)| {
            let Some(continent) = number_or(&cont) else {
                return Value::Nil.into_lua_multi(lua);
            };
            let Some((wx, wy)) = read_vec2(&pos)? else {
                return Value::Nil.into_lua_multi(lua);
            };
            let (wx, wy) = (wx as f32, wy as f32);
            let at = Maps::load(&c.db).and_then(|m| match number_or(&over) {
                Some(o) => m
                    .for_ui(o)?
                    .percent(wx, wy)
                    .filter(|&(x, y)| on_map(x, y))
                    .map(|(x, y)| (o, x, y)),
                None => m
                    .zone_percent(continent as i32, wx, wy)
                    .map(|(a, x, y)| (i64::from(a), x / 100.0, y / 100.0)),
            });
            match at {
                Some((ui, x, y)) => (ui, vec2(lua, x, y)?).into_lua_multi(lua),
                None => Value::Nil.into_lua_multi(lua),
            }
        },
    )?;

    // `GetMapRectOnMap(uiMapID, topUiMapID)`: the zone's rect on the top map (its own
    // continent's when the top names none) as `minX, maxX, minY, maxY`.
    let c = api.ca.clone();
    api.table(
        NS,
        "GetMapRectOnMap",
        move |lua, (id, top): (Value, Value)| {
            let rect = Maps::load(&c.db).and_then(|m| {
                let w = m.for_ui(number_or(&id).unwrap_or(0))?;
                let top = number_or(&top)
                    .filter(|&t| t != 0)
                    .and_then(|t| m.for_ui(t))
                    .or_else(|| m.continent(w.map_id))?;
                let (ax, ay) = top.percent(w.top as f32, w.left as f32)?;
                let (bx, by) = top.percent(w.bottom as f32, w.right as f32)?;
                Some((ax.min(bx), ax.max(bx), ay.min(by), ay.max(by)))
            });
            match rect {
                Some(r) => r.into_lua_multi(lua),
                None => Ok(none()),
            }
        },
    )?;

    let c = api.ca.clone();
    api.table(NS, "GetMapInfo", move |lua, v: Value| {
        let (Some(ui), Some(m)) = (number_or(&v), Maps::load(&c.db)) else {
            return Ok(Value::Nil);
        };
        Ok(map_info(lua, &c, &m, ui)?.map_or(Value::Nil, Value::Table))
    })?;
    // `GetMapChildrenInfo(uiMapID)`: the world's continents, or a continent's or instance map's
    // zones; a zone has none.
    let c = api.ca.clone();
    api.table(NS, "GetMapChildrenInfo", move |lua, v: Value| {
        let out = lua.create_table()?;
        let (Some(ui), Some(m)) = (number_or(&v), Maps::load(&c.db)) else {
            return Ok(out);
        };
        let Some(w) = m.for_ui(ui) else {
            return Ok(out);
        };
        let world = w.area_id == 0 && w.name == "World";
        if !world && ui > 0 {
            return Ok(out);
        }
        let parent_map = w.map_id;
        let parent_row = w.row;
        let children: Vec<i64> = m
            .areas
            .iter()
            .filter_map(|r| {
                if world {
                    let continent = r.area_id == 0
                        && r.row != parent_row
                        && map_instance_type(&c, r.map_id) == Some(0)
                        && m.continent(r.map_id).map(|x| x.row) == Some(r.row);
                    continent.then(|| -i64::from(r.row))
                } else {
                    (r.area_id != 0 && r.map_id == parent_map).then(|| i64::from(r.area_id))
                }
            })
            .collect();
        let mut n = 0;
        for child in children {
            if let Some(t) = map_info(lua, &c, &m, child)? {
                n += 1;
                out.raw_set(n, t)?;
            }
        }
        Ok(out)
    })?;
    let c = api.ca.clone();
    api.table(
        NS,
        "GetMapInfoAtPosition",
        move |lua, (id, x, y): (Value, Value, Value)| {
            if !(is_number(&id) && is_number(&x) && is_number(&y)) {
                return Ok(Value::Nil);
            }
            let Some(m) = Maps::load(&c.db) else {
                return Ok(Value::Nil);
            };
            let zone = m.for_ui(to_int(&id)).and_then(|w| {
                let (wx, wy) = w.world(to_number(&x), to_number(&y))?;
                m.zone_percent(w.map_id, wx as f32, wy as f32)
            });
            match zone {
                Some((area, ..)) => {
                    Ok(map_info(lua, &c, &m, i64::from(area))?.map_or(Value::Nil, Value::Table))
                }
                None => Ok(Value::Nil),
            }
        },
    )?;
    let c = api.ca.clone();
    api.table(NS, "GetFallbackWorldMapID", move |_, ()| {
        Ok(Maps::load(&c.db)
            .and_then(|m| m.world().map(|w| -i64::from(w.row)))
            .unwrap_or(0))
    })?;

    // The background art: the WorldMapArea row's folder, probed by its first tile.
    let dir = |c: &Ca, v: &Value| -> Option<String> {
        let m = Maps::load(&c.db)?;
        Some(m.for_ui(number_or(v)?)?.name.clone())
    };
    let c = api.ca.clone();
    api.table(NS, "MapHasArt", move |_, v: Value| {
        Ok(has_art(&c, dir(&c, &v).as_deref()))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetMapArtLayers", move |lua, v: Value| {
        let out = lua.create_table()?;
        if has_art(&c, dir(&c, &v).as_deref()) {
            let l = lua.create_table()?;
            l.set("layerWidth", CANVAS_W)?;
            l.set("layerHeight", CANVAS_H)?;
            l.set("tileWidth", TILE)?;
            l.set("tileHeight", TILE)?;
            l.set("minScale", 1)?;
            l.set("maxScale", 1)?;
            l.set("additionalZoomSteps", 0)?;
            out.raw_set(1, l)?;
        }
        Ok(out)
    })?;
    let c = api.ca.clone();
    api.table(
        NS,
        "GetMapArtLayerTextures",
        move |lua, (id, layer): (Value, Value)| {
            let out = lua.create_table()?;
            if !is_number(&layer) || to_int(&layer) != 1 {
                return Ok(out);
            }
            let Some(d) = dir(&c, &id).filter(|d| !d.is_empty()) else {
                return Ok(out);
            };
            let mut n = 0;
            for i in 1..=DETAIL_TILES {
                let path = tile_path(&d, i);
                if c.db.exists(&format!("{path}.blp")) {
                    n += 1;
                    out.raw_set(n, path)?;
                }
            }
            Ok(out)
        },
    )?;

    for (ns, name, filter) in [
        (NS, "GetMapOverlays", Filter::All),
        (
            "C_MapExplorationInfo",
            "GetExploredMapTextures",
            Filter::Explored,
        ),
        (
            "C_MapExplorationInfo",
            "GetUnexploredMapTextures",
            Filter::Unexplored,
        ),
    ] {
        let c = api.ca.clone();
        api.table(ns, name, move |lua, v: Value| {
            let maps = Maps::load(&c.db);
            let row = maps.as_deref().and_then(|m| zone_row(lua, m, &v));
            overlays::zone_overlays(lua, &c, maps.as_deref(), row, filter)
        })?;
    }

    taxi::install(api)?;
    waypoint::install(api)
}

#[cfg(test)]
mod tests {
    use super::area::fixture;
    use crate::lua::test_support::vm;

    #[test]
    fn c_map_reads_the_tables() {
        let ca = crate::Ca::default();
        fixture::seed(&ca.db);
        let script = vm(&ca);
        let out: String = script
            .lua()
            .load(
                r#"
                local M = C_Map
                local w, h = M.GetMapWorldSize(14)
                local info = M.GetMapInfo(14)
                local kids = M.GetMapChildrenInfo(-13)
                local cid, wp = M.GetWorldPosFromMapPos(14, { x = 0.25, y = 0.75 })
                local ui, mp = M.GetMapPosFromWorldPos(1, { x = 250, y = 750 })
                local a, b, c, d = M.GetMapRectOnMap(14)
                local ov = M.GetMapOverlays(14)[1]
                local trig = M.GetAreaTriggerInfo(1)
                return w .. "x" .. h
                  .. " " .. info.name .. info.mapType .. info.parentMapID
                  .. " " .. table.getn(kids) .. kids[1].name
                  .. " " .. cid .. ":" .. wp.x .. "," .. wp.y
                  .. " " .. ui .. ":" .. mp.x .. "," .. mp.y
                  .. " " .. a .. "," .. b .. "," .. c .. "," .. d
                  .. " " .. ov.textureName .. ov.areaID .. ov.hitRectRight .. table.getn(ov.tiles)
                  .. " " .. M.GetAreaInfo(362) .. tostring(M.GetAreaInfo(0))
                  .. " " .. M.GetMapAreaIDs().Durotar .. M.GetAreas()[14]
                  .. " " .. M.GetFallbackWorldMapID()
                  .. " " .. trig.areaID .. trig.mapX .. tostring(trig.isBox) .. tostring(M.GetBestMapForUnit(5))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            "1000x1000 Durotar3-13 1Durotar 1:250,750 14:0.25,0.75 \
             0.45,0.5,0.45,0.5 DUROTAR_OV1410020 Kalimdornil 14Durotar -1 1425falsenil"
        );
    }

    #[test]
    fn the_waypoint_round_trips_and_fires() {
        let ca = crate::Ca::default();
        fixture::seed(&ca.db);
        let script = vm(&ca);
        let out: String = script
            .lua()
            .load(
                r#"
                local M, fired = C_Map, 0
                local f = CreateFrame("Frame")
                f:RegisterEvent("USER_WAYPOINT_UPDATED")
                f:SetScript("OnEvent", function() fired = fired + 1 end)
                local bad = M.SetUserWaypoint({ uiMapID = -1, position = { x = 0.5, y = 0.5 } })
                local ok = M.SetUserWaypoint({ uiMapID = 14, position = { x = 0.25, y = 0.75 } })
                local link = M.GetUserWaypointHyperlink()
                local back = M.GetUserWaypointFromHyperlink(link)
                local on = M.GetUserWaypointPositionForMap(-13)
                M.ClearUserWaypoint()
                M.ClearUserWaypoint()
                return tostring(bad) .. tostring(ok) .. " " .. link
                  .. " " .. back.uiMapID .. ":" .. back.position.x
                  .. " " .. on.x .. "," .. on.y
                  .. " " .. tostring(M.HasUserWaypoint()) .. fired
                  .. tostring(M.CanSetUserWaypointOnMap(-13)) .. tostring(M.CanSetUserWaypointOnMap(-1))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            "falsetrue |cffffff00|Hworldmap:14:2500:7500|h[Map Pin Location]|h|r \
             14:0.25 0.4625,0.4875 false2truefalse"
        );
    }
    #[test]
    fn the_install_tree_and_art_hold() {
        let _ = benilla_formats::wow_data_or_skip!();
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local M = C_Map
                local d = M.GetMapInfo(14)
                local k = M.GetMapInfo(d.parentMapID)
                local w = M.GetMapInfo(k.parentMapID)
                local ui = M.GetMapPosFromWorldPos(1, { x = 315, y = -4724 })
                local ov = M.GetMapOverlays(14)[1]
                local kids = 0
                for _, c in ipairs(M.GetMapChildrenInfo(M.GetFallbackWorldMapID())) do
                  if c.mapType == 2 then kids = kids + 1 end
                end
                return d.mapType .. k.name .. k.mapType .. w.mapType .. " " .. ui
                  .. " " .. tostring(M.MapHasArt(14)) .. table.getn(M.GetMapArtLayerTextures(14, 1))
                  .. " " .. tostring(table.getn(ov.tiles) > 0) .. " " .. kids
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "3Kalimdor21 14 true12 true 2");
    }
}

//! `taxi/Map.cpp` and `taxi/Paths.cpp`: `C_TaxiMap` over `TaxiNodes.dbc`, `TaxiPath.dbc`,
//! `TaxiPathNode.dbc` and the open flight master's map (`ext_read::taxi`), with
//! `Enum.FlightPathFaction` and `Enum.FlightPathState`.
//!
//! - `GetTaxiNodesForMap([mapID])`: every flight master, discovered or not: a node with a flight
//!   mount that is some path's end, Blizzard's "Generic" zeppelin marker excepted; faction by its
//!   mounts, `reachable` when a path leads to it, its continent position, and its zone from its
//!   own name ("Location, Zone": the location's map when it holds the point, else the zone's),
//!   else the landmass resolver.
//! - `GetAllTaxiNodes`, `GetTaxiRoute(slot)`: the open map's nodes with their state and the
//!   `TakeTaxiNode` slot, and the node chain a slot's flight would take.
//! - `GetTaxiPaths`, `GetTaxiPathWaypoints(pathID)`: the raw edge list and a path's polyline.

use mlua::{Lua, Table, Value};

use super::area::Maps;
use crate::lua::{is_number, to_int, Api};
use crate::Ca;

/// `TaxiNodes.dbc`: map, position, localized name, the two mounts.
const NODE_MAP: usize = 1;
const NODE_X: usize = 2;
const NODE_NAME: usize = 5;
const NODE_MOUNT_HORDE: usize = 14;
const NODE_MOUNT_ALLIANCE: usize = 15;
/// `TaxiPath.dbc` and `TaxiPathNode.dbc`.
const PATH_FROM: usize = 1;
const PATH_TO: usize = 2;
const PATH_COST: usize = 3;
const WAY_PATH: usize = 1;
const WAY_INDEX: usize = 2;
const WAY_MAP: usize = 3;
const WAY_X: usize = 4;
/// `AreaTable.dbc`'s localized name.
const AREA_NAME: usize = 11;

/// `ResolveZoneByName`: an exact name, else the first word-prefix match ("Redridge" for
/// "Redridge Mountains").
fn zone_by_name(ca: &Ca, zone: &str) -> Option<u32> {
    if zone.is_empty() {
        return None;
    }
    let t = ca.db.get("AreaTable")?;
    let mut prefix = None;
    for r in t.rows() {
        let name = r.loc(AREA_NAME);
        if name.eq_ignore_ascii_case(zone) {
            return Some(r.id());
        }
        let word = name.len() > zone.len()
            && name.as_bytes()[zone.len()] == b' '
            && name[..zone.len()].eq_ignore_ascii_case(zone);
        if prefix.is_none() && word {
            prefix = Some(r.id());
        }
    }
    prefix
}

/// The node's zone and 0..100 position: its location's map, its zone suffix's, or the landmass.
fn node_zone(
    ca: &Ca,
    maps: &Maps,
    name: &str,
    map: i32,
    x: f32,
    y: f32,
) -> Option<(u32, f64, f64)> {
    let (location, suffix) = match (name.find(", "), name.rfind(", ")) {
        (Some(first), Some(last)) => (&name[..first], &name[last + 2..]),
        _ => ("", ""),
    };
    for part in [location, suffix] {
        if let Some(area) = zone_by_name(ca, part) {
            if let Some((mx, my)) = maps.percent_in_zone(area, x, y) {
                return Some((area, mx, my));
            }
        }
    }
    maps.zone_percent(map, x, y)
}

fn position(lua: &Lua, x: f64, y: f64) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("x", x)?;
    t.set("y", y)?;
    Ok(t)
}

fn nodes_for_map(lua: &Lua, ca: &Ca, want: Option<i64>) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    let (Some(nodes), Some(paths)) = (ca.db.get("TaxiNodes"), ca.db.get("TaxiPath")) else {
        return Ok(out);
    };
    let maps = Maps::load(&ca.db);
    let mut endpoint = std::collections::HashSet::new();
    let mut dest = std::collections::HashSet::new();
    for p in paths.rows() {
        endpoint.insert(p.u32(PATH_FROM));
        endpoint.insert(p.u32(PATH_TO));
        dest.insert(p.u32(PATH_TO));
    }
    let mut n = 0;
    for r in nodes.rows() {
        let map = r.i32(NODE_MAP);
        if want.is_some_and(|w| w != i64::from(map)) {
            continue;
        }
        let (horde, alliance) = (r.u32(NODE_MOUNT_HORDE), r.u32(NODE_MOUNT_ALLIANCE));
        if (horde == 0 && alliance == 0) || !endpoint.contains(&r.id()) {
            continue;
        }
        let name = r.loc(NODE_NAME).to_string();
        if name.starts_with("Generic") {
            continue;
        }
        let faction = match (horde != 0, alliance != 0) {
            (true, true) => 0,
            (false, true) => 2,
            _ => 1,
        };
        let (x, y, z) = (r.f32(NODE_X), r.f32(NODE_X + 1), r.f32(NODE_X + 2));
        let t = lua.create_table()?;
        t.set("nodeID", r.id())?;
        t.set("name", name.as_str())?;
        t.set("faction", faction)?;
        t.set("reachable", dest.contains(&r.id()))?;
        t.set("mapID", map)?;
        t.set("x", x)?;
        t.set("y", y)?;
        t.set("z", z)?;
        let (px, py) = maps
            .as_deref()
            .and_then(|m| m.continent(map)?.percent(x, y))
            .unwrap_or((0.0, 0.0));
        t.set("position", position(lua, px, py)?)?;
        if let Some((area, mx, my)) = maps
            .as_deref()
            .and_then(|m| node_zone(ca, m, &name, map, x, y))
        {
            t.set("areaID", area)?;
            t.set("mapX", mx)?;
            t.set("mapY", my)?;
        }
        n += 1;
        out.raw_set(n, t)?;
    }
    Ok(out)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.int_enum(
        "Enum",
        "FlightPathFaction",
        &[("Neutral", 0), ("Horde", 1), ("Alliance", 2)],
    )?;
    api.int_enum(
        "Enum",
        "FlightPathState",
        &[("Current", 0), ("Reachable", 1), ("Unreachable", 2)],
    )?;
    const NS: &str = "C_TaxiMap";
    let c = api.ca.clone();
    api.table(NS, "GetTaxiNodesForMap", move |lua, v: Value| {
        nodes_for_map(lua, &c, is_number(&v).then(|| to_int(&v)))
    })?;
    api.table(NS, "GetAllTaxiNodes", |lua, _: Value| {
        let out = lua.create_table()?;
        let Some(map) = benilla_ui::script::ext_read::taxi(lua) else {
            return Ok(out);
        };
        for (i, node) in map.nodes.into_iter().enumerate() {
            use benilla_ui::script::TaxiNodeType as T;
            let t = lua.create_table()?;
            t.set("slotIndex", i + 1)?;
            t.set("name", node.name)?;
            t.set("nodeID", node.node_id)?;
            t.set(
                "state",
                match node.node_type {
                    T::Current => 0,
                    T::Reachable => 1,
                    T::Distant => 2,
                },
            )?;
            t.set(
                "position",
                position(lua, f64::from(node.pos.0), f64::from(node.pos.1))?,
            )?;
            out.raw_set(i + 1, t)?;
        }
        Ok(out)
    })?;
    api.table(NS, "GetTaxiRoute", |lua, v: Value| {
        if !is_number(&v) {
            return Ok(Value::Nil);
        }
        let chain = benilla_ui::script::ext_read::taxi(lua).and_then(|m| {
            let node = m
                .nodes
                .into_iter()
                .nth(usize::try_from(to_int(&v) - 1).ok()?)?;
            (node.chain.len() >= 2).then_some(node.chain)
        });
        match chain {
            Some(c) => Ok(Value::Table(lua.create_sequence_from(c)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetTaxiPaths", move |lua, ()| {
        let out = lua.create_table()?;
        if let Some(paths) = c.db.get("TaxiPath") {
            for (i, r) in paths.rows().enumerate() {
                let t = lua.create_table()?;
                t.set("pathID", r.id())?;
                t.set("fromNodeID", r.u32(PATH_FROM))?;
                t.set("toNodeID", r.u32(PATH_TO))?;
                t.set("cost", r.u32(PATH_COST))?;
                out.raw_set(i + 1, t)?;
            }
        }
        Ok(out)
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetTaxiPathWaypoints", move |lua, v: Value| {
        let want = to_int(&v);
        if !is_number(&v) || want <= 0 {
            return Ok(Value::Nil);
        }
        let Some(table) = c.db.get("TaxiPathNode") else {
            return Ok(Value::Nil);
        };
        let mut points: Vec<(u32, f32, f32, f32, u32)> = table
            .rows()
            .filter(|r| i64::from(r.u32(WAY_PATH)) == want)
            .map(|r| {
                (
                    r.u32(WAY_INDEX),
                    r.f32(WAY_X),
                    r.f32(WAY_X + 1),
                    r.f32(WAY_X + 2),
                    r.u32(WAY_MAP),
                )
            })
            .collect();
        if points.is_empty() {
            return Ok(Value::Nil);
        }
        points.sort_by_key(|p| p.0);
        let out = lua.create_table()?;
        for (i, (_, x, y, z, map)) in points.into_iter().enumerate() {
            let t = lua.create_table()?;
            t.set("x", x)?;
            t.set("y", y)?;
            t.set("z", z)?;
            t.set("mapID", map)?;
            out.raw_set(i + 1, t)?;
        }
        Ok(Value::Table(out))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use benilla_ui::script::{TaxiNodeType, TaxiUiNode, TaxiUiState};

    #[test]
    fn the_open_map_reads_its_slots_and_routes() {
        let ca = crate::Ca::default();
        let mut script = crate::lua::test_support::vm(&ca);
        script.set_taxi(Some(TaxiUiState {
            art: String::new(),
            nodes: vec![
                TaxiUiNode {
                    name: "Stormwind, Elwynn".into(),
                    node_type: TaxiNodeType::Current,
                    pos: (0.5, 0.25),
                    node_id: 2,
                    chain: vec![2],
                    ..Default::default()
                },
                TaxiUiNode {
                    name: "Lakeshire, Redridge".into(),
                    node_id: 5,
                    chain: vec![2, 4, 5],
                    ..Default::default()
                },
            ],
        }));
        let out: String = script
            .lua()
            .load(
                r#"
                local T = C_TaxiMap
                local n = T.GetAllTaxiNodes()
                local r = T.GetTaxiRoute(2)
                return n[1].slotIndex .. n[1].nodeID .. n[1].state .. n[1].position.x
                  .. " " .. n[2].state .. " " .. table.concat(r, ",")
                  .. " " .. tostring(T.GetTaxiRoute(1)) .. Enum.FlightPathFaction.Alliance
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "1200.5 1 2,4,5 nil2");
    }

    #[test]
    fn the_install_lists_flight_masters_with_their_zones() {
        let _ = benilla_formats::wow_data_or_skip!();
        let script = crate::lua::test_support::vm(&crate::Ca::default());
        // Kalimdor's flight masters each sit in a zone, and every one leads somewhere or is led to.
        let out: String = script
            .lua()
            .load(
                r#"
                local nodes = C_TaxiMap.GetTaxiNodesForMap(1)
                local zoned, paths = 0, table.getn(C_TaxiMap.GetTaxiPaths())
                for _, n in ipairs(nodes) do
                  if n.areaID and n.mapX >= 0 and n.mapX <= 100 then zoned = zoned + 1 end
                end
                local way = C_TaxiMap.GetTaxiPathWaypoints(C_TaxiMap.GetTaxiPaths()[1].pathID)
                return tostring(table.getn(nodes) > 10) .. tostring(zoned == table.getn(nodes))
                  .. tostring(paths > 50) .. tostring(table.getn(way) > 2)
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "truetruetruetrue");
    }
}

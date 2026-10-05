//! `map/Area.cpp`: `WorldMapArea.dbc` row resolution and the world↔map projection every `C_Map`
//! reader shares.
//!
//! A `uiMapID` names a zone by its positive `AreaTable.dbc` id, or any other map (world,
//! continent, instance) by `-(WorldMapArea row)`, the engine's two-namespaces-in-one-int idiom.
//! World axes are WoW's, `x` north and `y` west; a map's horizontal axis runs off world `y`, its
//! vertical off world `x`.

use std::collections::HashMap;

use crate::dbc::Databases;

/// The world map's detail canvas (`WorldMapDetailFrame`, 1002×668 in `WorldMapFrame.xml`) and
/// one background tile; overlay placement and hit rects are authored in this space, so these are
/// the authored constants, not a live frame read.
pub(super) const CANVAS_W: f64 = 1002.0;
pub(super) const CANVAS_H: f64 = 668.0;
pub(super) const TILE: u32 = 256;

/// One `WorldMapArea.dbc` row.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Wma {
    pub row: u32,
    pub map_id: i32,
    pub area_id: u32,
    /// The `Interface\WorldMap\<name>\` art folder.
    pub name: String,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

impl Wma {
    /// `(spanX, spanY)`: the world `x` (vertical) and `y` (horizontal) extents.
    fn spans(&self) -> (f64, f64) {
        (self.top - self.bottom, self.left - self.right)
    }

    fn degenerate(&self) -> bool {
        let (sx, sy) = self.spans();
        sx <= 0.0 || sy <= 0.0
    }

    /// `PercentInRow`: world `(x, y)` as 0..1 on this row's rect, no containment gate; `None`
    /// for a degenerate rect.
    pub fn percent(&self, x: f32, y: f32) -> Option<(f64, f64)> {
        if self.degenerate() {
            return None;
        }
        let (sx, sy) = self.spans();
        Some((
            (self.left - f64::from(y)) / sy,
            (self.top - f64::from(x)) / sx,
        ))
    }

    /// `WorldFromRow`: a 0..1 position back to world `(x, y)`.
    pub fn world(&self, px: f64, py: f64) -> Option<(f64, f64)> {
        if self.degenerate() {
            return None;
        }
        let (sx, sy) = self.spans();
        Some((self.top - py * sx, self.left - px * sy))
    }
}

/// One `WorldMapOverlay.dbc` row.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Overlay {
    pub id: u32,
    pub wma: u32,
    pub areas: [i32; 4],
    pub map_point: (i32, i32),
    pub texture: String,
    pub size: (i32, i32),
    pub offset: (i32, i32),
    /// `(top, left, bottom, right)` in canvas pixels.
    pub hit: (i32, i32, i32, i32),
}

/// The two tables, read once.
pub(super) struct Maps {
    pub areas: Vec<Wma>,
    pub overlays: Vec<Overlay>,
    by_wma: HashMap<u32, Vec<usize>>,
}

impl Maps {
    pub fn load(db: &Databases) -> Option<std::sync::Arc<Self>> {
        db.derived(|db| {
            let wma = db.get("WorldMapArea")?;
            let areas = wma
                .rows()
                .map(|r| Wma {
                    row: r.id(),
                    map_id: r.i32(1),
                    area_id: r.u32(2),
                    name: r.str(3).to_string(),
                    left: f64::from(r.f32(4)),
                    right: f64::from(r.f32(5)),
                    top: f64::from(r.f32(6)),
                    bottom: f64::from(r.f32(7)),
                })
                .collect();
            let overlays: Vec<Overlay> = db
                .get("WorldMapOverlay")
                .map(|t| {
                    t.rows()
                        .map(|r| Overlay {
                            id: r.id(),
                            wma: r.u32(1),
                            areas: [r.i32(2), r.i32(3), r.i32(4), r.i32(5)],
                            map_point: (r.i32(6), r.i32(7)),
                            texture: r.str(8).to_string(),
                            size: (r.i32(9), r.i32(10)),
                            offset: (r.i32(11), r.i32(12)),
                            hit: (r.i32(13), r.i32(14), r.i32(15), r.i32(16)),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut by_wma: HashMap<u32, Vec<usize>> = HashMap::new();
            for (i, o) in overlays.iter().enumerate() {
                by_wma.entry(o.wma).or_default().push(i);
            }
            Some(Self {
                areas,
                overlays,
                by_wma,
            })
        })
    }

    pub fn row(&self, row: u32) -> Option<&Wma> {
        self.areas.iter().find(|w| w.row == row)
    }

    /// `RowForAreaID`: the first row carrying this `AreaTable` id.
    pub fn for_area(&self, area: u32) -> Option<&Wma> {
        self.areas.iter().find(|w| w.area_id == area)
    }

    /// `RowForUiMapID`.
    pub fn for_ui(&self, ui: i64) -> Option<&Wma> {
        match ui {
            0 => None,
            u if u > 0 => self.for_area(u32::try_from(u).ok()?),
            u => self.row(u32::try_from(-u).ok()?),
        }
    }

    /// `PercentInZone`: world `(x, y)` as 0..100 on the zone's first row with a real rect, `None`
    /// when the zone has none or the point lies outside it.
    pub fn percent_in_zone(&self, area: u32, x: f32, y: f32) -> Option<(f64, f64)> {
        let w = self
            .areas
            .iter()
            .find(|w| w.area_id == area && !w.degenerate())?;
        let (fx, fy) = (f64::from(x), f64::from(y));
        if fx < w.bottom || fx > w.top || fy < w.right || fy > w.left {
            return None;
        }
        w.percent(x, y).map(|(px, py)| (px * 100.0, py * 100.0))
    }

    /// `ContinentRowForMapID`: the map's `areaID` 0 row with a real rect, so the zero-rect
    /// "World" row never wins.
    pub fn continent(&self, map_id: i32) -> Option<&Wma> {
        self.areas
            .iter()
            .find(|w| w.map_id == map_id && w.area_id == 0 && !w.degenerate())
    }

    /// `WorldRow`: the `areaID` 0 row named "World".
    pub fn world(&self) -> Option<&Wma> {
        self.areas
            .iter()
            .find(|w| w.area_id == 0 && w.name == "World")
    }

    pub fn overlays_of(&self, wma: u32) -> impl Iterator<Item = &Overlay> {
        self.by_wma
            .get(&wma)
            .into_iter()
            .flatten()
            .map(|&i| &self.overlays[i])
    }

    /// `OverlayMargin`: how deep a 0..100 zone point sits inside the deepest of the zone's overlay
    /// hit rects that holds it, 0 at an edge to 0.5 at the centre; `None` on none of them.
    fn overlay_margin(&self, wma: u32, map_x: f64, map_y: f64) -> Option<f64> {
        let cx = map_x / 100.0 * CANVAS_W;
        let cy = map_y / 100.0 * CANVAS_H;
        let mut best: Option<f64> = None;
        for o in self.overlays_of(wma) {
            let (t, l, b, r) = (
                f64::from(o.hit.0),
                f64::from(o.hit.1),
                f64::from(o.hit.2),
                f64::from(o.hit.3),
            );
            let (sw, sh) = (r - l, b - t);
            if sw <= 0.0 || sh <= 0.0 || cx < l || cx > r || cy < t || cy > b {
                continue;
            }
            let m = ((cx - l).min(r - cx) / sw).min((cy - t).min(b - cy) / sh);
            if best.is_none_or(|b| m > b) {
                best = Some(m);
            }
        }
        best
    }

    /// `ZonePercent`: the zone a world point on `map_id` falls in, with its 0..100 position.
    /// Zone rects overlap, so the zone whose drawn landmass (an overlay hit rect) holds the point
    /// deepest wins; off every landmass, the rect the point is most interior to.
    pub fn zone_percent(&self, map_id: i32, x: f32, y: f32) -> Option<(u32, f64, f64)> {
        let (fx, fy) = (f64::from(x), f64::from(y));
        let mut land: Option<(f64, u32, f64, f64)> = None;
        let mut boxed: Option<(f64, u32, f64, f64)> = None;
        for w in &self.areas {
            if w.map_id != map_id || w.area_id == 0 || w.degenerate() {
                continue;
            }
            if fx < w.bottom || fx > w.top || fy < w.right || fy > w.left {
                continue;
            }
            let (sx, sy) = w.spans();
            let map_x = (w.left - fy) / sy * 100.0;
            let map_y = (w.top - fx) / sx * 100.0;
            let margin =
                ((fx - w.bottom).min(w.top - fx) / sx).min((fy - w.right).min(w.left - fy) / sy);
            if boxed.is_none_or(|b| margin > b.0) {
                boxed = Some((margin, w.area_id, map_x, map_y));
            }
            if let Some(m) = self.overlay_margin(w.row, map_x, map_y) {
                if land.is_none_or(|l| m > l.0) {
                    land = Some((m, w.area_id, map_x, map_y));
                }
            }
        }
        land.or(boxed).map(|(_, a, mx, my)| (a, mx, my))
    }
}

#[cfg(test)]
pub(super) mod fixture {
    use crate::dbc::Dbc;

    fn f(v: f32) -> u32 {
        v.to_bits()
    }

    /// Two WorldMapArea rows on map 1 (a continent and one zone inside it), a zero-rect World row,
    /// one overlay on the zone, and a two-row AreaTable.
    pub fn seed(db: &crate::dbc::Databases) {
        // strings: 1 "Kalimdor", 10 "Durotar", 18 "World", 24 "DUROTAR_OV"
        let strings = b"\0Kalimdor\0Durotar\0World\0DUROTAR_OV\0";
        let wma = vec![
            vec![
                13,
                1,
                0,
                1,
                f(10000.0),
                f(-10000.0),
                f(10000.0),
                f(-10000.0),
            ],
            vec![4, 1, 14, 10, f(1000.0), f(0.0), f(1000.0), f(0.0)],
            vec![1, 0, 0, 18, 0, 0, 0, 0],
        ];
        db.seed("WorldMapArea", Dbc::from_rows(&wma, strings));
        let ov = vec![vec![
            7, 4, 14, 0, 0, 0, 0, 0, 24, 300, 200, 100, 50, 0, 0, 668, 1002,
        ]];
        db.seed("WorldMapOverlay", Dbc::from_rows(&ov, strings));
        // AreaTable: id, map, parent zone, explore bit, flags, ..., gate (10), name (11, enUS)
        let mut zone = vec![0u32; 29];
        zone[0] = 14;
        zone[1] = 1;
        zone[3] = 5;
        zone[11] = 10;
        let mut sub = vec![0u32; 29];
        sub[0] = 362;
        sub[1] = 1;
        sub[2] = 14;
        sub[3] = 6;
        sub[11] = 1;
        db.seed("AreaTable", Dbc::from_rows(&[zone, sub], strings));
        // AreaTrigger: id, map, x, y, z, radius, box length, width, height, yaw.
        let trigger = vec![1, 1, f(250.0), f(750.0), 0, f(5.0), 0, 0, 0, 0];
        db.seed("AreaTrigger", Dbc::from_rows(&[trigger], b" "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trips_and_picks_the_zone() {
        let db = Databases::default();
        fixture::seed(&db);
        let maps = Maps::load(&db).expect("maps");
        let zone = maps.for_ui(14).expect("zone");
        let (px, py) = zone.percent(250.0, 750.0).expect("percent");
        assert_eq!((px, py), (0.25, 0.75));
        assert_eq!(zone.world(px, py), Some((250.0, 750.0)));
        assert_eq!(maps.for_ui(-13).map(|w| w.row), Some(13));
        assert_eq!(maps.continent(1).map(|w| w.row), Some(13));
        assert_eq!(maps.world().map(|w| w.row), Some(1));
        assert!(maps.world().unwrap().percent(0.0, 0.0).is_none());
        assert_eq!(maps.zone_percent(1, 250.0, 750.0), Some((14, 25.0, 75.0)));
        assert_eq!(maps.zone_percent(1, 5000.0, 750.0), None);
    }
}

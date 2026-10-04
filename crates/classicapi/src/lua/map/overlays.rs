//! `map/Overlays.cpp` and `map/MapExploration.cpp`: every `WorldMapOverlay.dbc` entry of a zone,
//! explored or not, with its tile grid resolved, and the exploration-filtered views.
//!
//! An overlay's art is `<base>1.blp, 2, …`, row-major on a 256 grid. The DBC's implied grid is
//! wrong on some custom data, so the grid comes from the tiles' BLP headers: the first
//! partial-width tile ends a row (searched one past the DBC's columns for a sliver it rounds
//! away), the first partial-height tile marks the last row, tiles past the grid are foreign art
//! and dropped, and a whole-multiple rect with no partial edge and extra tiles is an upscaled
//! re-export, its grid recovered from the aspect ratio. The full-cell size is the largest tile
//! dimension, so an HD patch's uniformly upscaled tiles resolve the same as stock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mlua::{Lua, Table};

use super::area::{Maps, Overlay, TILE};
use crate::Ca;

const MAX_TILES: usize = 64;

/// Which overlays of a zone to list, by the player's exploration.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Filter {
    All,
    Explored,
    Unexplored,
}

/// One overlay's resolved grid.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Resolved {
    pub cols: usize,
    pub rows: usize,
    /// Tiles to draw, the foreign tail dropped.
    pub count: usize,
    pub upscaled: bool,
    /// The full-cell file size: 256 on stock data, 256 × scale on an HD patch.
    pub cell: u32,
    /// Each tile's BLP `(width, height)`.
    pub dims: Vec<(u32, u32)>,
}

/// `ResolveTiles` over the tiles' file dimensions and the DBC rect.
pub(super) fn resolve(dims: Vec<(u32, u32)>, dbc_w: i32, dbc_h: i32) -> Resolved {
    let n = dims.len();
    let dbc_cols = ((dbc_w + 255) / 256).max(1) as usize;
    let dbc_rows = ((dbc_h + 255) / 256).max(1) as usize;
    let cell = dims.iter().flat_map(|&(w, h)| [w, h]).fold(TILE, u32::max);
    let edge_w = dims.iter().take(dbc_cols + 1).position(|&(w, _)| w < cell);
    let mut cols = edge_w.map_or(dbc_cols, |i| i + 1);
    let edge_h = dims.iter().position(|&(_, h)| h < cell);
    let mut rows = edge_h.map_or(dbc_rows, |i| i / cols + 1);
    let mut limit = cols * rows;
    let mut upscaled = false;
    if edge_w.is_none() && edge_h.is_none() && dbc_w % 256 == 0 && dbc_h % 256 == 0 && n > limit {
        upscaled = true;
        let aspect = (n as f64 * f64::from(dbc_w) / f64::from(dbc_h)).sqrt();
        cols = ((aspect + 0.5).floor() as usize).clamp(1, n);
        limit = n;
        rows = n.div_ceil(cols);
    }
    Resolved {
        cols: cols.max(1),
        rows: rows.max(1),
        count: limit.min(n),
        upscaled,
        cell,
        dims,
    }
}

/// A BLP's header `(width, height)`, at +0x0C and +0x10 in BLP1 and BLP2 alike.
fn blp_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 0x14 || &bytes[..3] != b"BLP" {
        return None;
    }
    let w = u32::from_le_bytes(bytes[0x0c..0x10].try_into().ok()?);
    let h = u32::from_le_bytes(bytes[0x10..0x14].try_into().ok()?);
    ((1..=4096).contains(&w) && (1..=4096).contains(&h)).then_some((w, h))
}

/// The resolved grids, once per overlay per session (the chain does not change under a run).
#[derive(Default)]
struct TileCache(Mutex<HashMap<u32, Arc<Resolved>>>);

fn resolved(ca: &Ca, o: &Overlay, base: &str) -> Arc<Resolved> {
    let cache = ca
        .db
        .derived(|_| Some(TileCache::default()))
        .expect("tile cache");
    if let Some(hit) = cache.0.lock().unwrap_or_else(|e| e.into_inner()).get(&o.id) {
        return hit.clone();
    }
    let dims = (1..=MAX_TILES)
        .map_while(|n| {
            ca.db
                .file(&format!("{base}{n}.blp"))
                .and_then(|b| blp_dims(&b))
        })
        .collect();
    let r = Arc::new(resolve(dims, o.size.0, o.size.1));
    cache
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(o.id, r.clone());
    r
}

/// The smallest power of two at or above `v`, at least 16: a tile's content fills
/// `content / next_pot` of its file at any upscale.
fn next_pot(v: i32) -> i32 {
    let mut p = 16;
    while p < v {
        p <<= 1;
    }
    p
}

/// One tile's `(width, height, texCoordX, texCoordY, offsetX, offsetY)` on the canvas.
pub(super) fn tile_geometry(
    r: &Resolved,
    i: usize,
    size: (i32, i32),
    offset: (i32, i32),
) -> (f64, f64, f64, f64, f64, f64) {
    let (col, row) = (i % r.cols, i / r.cols);
    let (off_x, off_y) = (f64::from(offset.0), f64::from(offset.1));
    if r.upscaled {
        let w = f64::from(size.0) / r.cols as f64;
        let h = f64::from(size.1) / r.rows as f64;
        return (
            w,
            h,
            1.0,
            1.0,
            off_x + col as f64 * w,
            off_y + row as f64 * h,
        );
    }
    let (file_w, file_h) = r.dims[i];
    let scale = f64::from(r.cell) / 256.0;
    let edge = |last: bool, dbc: i32, n: usize, file: u32| -> (f64, f64) {
        if !last {
            return (256.0, 1.0);
        }
        let rem = dbc - 256 * (n as i32 - 1);
        if rem <= 0 {
            (f64::from(file) / scale, 1.0)
        } else {
            (f64::from(rem), f64::from(rem) / f64::from(next_pot(rem)))
        }
    };
    let (w, tx) = edge(col == r.cols - 1, size.0, r.cols, file_w);
    let (h, ty) = edge(row == r.rows - 1, size.1, r.rows, file_h);
    (
        w,
        h,
        tx,
        ty,
        off_x + (col * 256) as f64,
        off_y + (row * 256) as f64,
    )
}

/// `IsOverlayExplored`: any of the overlay's areas needing no exploration (gate below 0), past
/// the bitfield, or with its explore bit set. `None` bits (no player) is unexplored.
fn explored(o: &Overlay, bits: Option<&[u32]>, ca: &Ca) -> bool {
    let Some(bits) = bits else {
        return false;
    };
    let Some(table) = ca.db.get("AreaTable") else {
        return false;
    };
    o.areas.iter().filter(|&&a| a > 0).any(|&a| {
        let Some(row) = table.row(a as u32) else {
            return false;
        };
        if row.i32(10) < 0 {
            return true;
        }
        let bit = row.i32(3);
        if bit < 0 {
            return false;
        }
        if bit >> 3 >= 0x100 {
            return true;
        }
        bits.get((bit / 32) as usize)
            .is_some_and(|w| w & (1 << (bit % 32)) != 0)
    })
}

/// `PushZoneOverlays`: the overlay tables of WorldMapArea `row`, filtered by exploration.
pub(super) fn zone_overlays(
    lua: &Lua,
    ca: &Ca,
    maps: Option<&Maps>,
    row: Option<u32>,
    filter: Filter,
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    let (Some(maps), Some(row)) = (maps, row) else {
        return Ok(out);
    };
    let dir = maps.row(row).map(|w| w.name.as_str()).unwrap_or_default();
    let bits = (filter != Filter::All && ca.lock().mirror.player != 0)
        .then(|| benilla_ui::script::ext_read::explored_zones(lua));
    let mut n = 0;
    for o in maps.overlays_of(row) {
        if filter != Filter::All {
            let e = explored(o, bits.as_deref(), ca);
            if (filter == Filter::Explored) != e {
                continue;
            }
        }
        let has_tex = !o.texture.is_empty();
        let (base, size, r) = if has_tex {
            let folder = if dir.is_empty() { "World" } else { dir };
            let base = format!("Interface\\WorldMap\\{folder}\\{}", o.texture);
            let r = resolved(ca, o, &base);
            (base, o.size, Some(r))
        } else {
            (String::new(), (0, 0), None)
        };
        let t = lua.create_table()?;
        t.set("textureName", o.texture.as_str())?;
        t.set("texturePath", base.as_str())?;
        t.set("textureWidth", size.0)?;
        t.set("textureHeight", size.1)?;
        t.set("offsetX", o.offset.0)?;
        t.set("offsetY", o.offset.1)?;
        t.set("mapPointX", o.map_point.0)?;
        t.set("mapPointY", o.map_point.1)?;
        t.set("hitRectTop", o.hit.0)?;
        t.set("hitRectLeft", o.hit.1)?;
        t.set("hitRectBottom", o.hit.2)?;
        t.set("hitRectRight", o.hit.3)?;
        let ids: Vec<i32> = o.areas.iter().copied().filter(|&a| a != 0).collect();
        t.set("areaID", ids.first().copied().unwrap_or(0))?;
        t.set("areaIDs", lua.create_sequence_from(ids)?)?;
        let tiles = lua.create_table()?;
        let files = lua.create_table()?;
        if let Some(r) = &r {
            t.set("tileCols", r.cols)?;
            t.set("tileRows", r.rows)?;
            t.set("upscaled", r.upscaled)?;
            for i in 0..r.count {
                let file = format!("{base}{}", i + 1);
                let (w, h, tx, ty, x, y) = tile_geometry(r, i, size, o.offset);
                let tile = lua.create_table()?;
                tile.set("file", file.as_str())?;
                tile.set("width", w)?;
                tile.set("height", h)?;
                tile.set("texCoordX", tx)?;
                tile.set("texCoordY", ty)?;
                tile.set("offsetX", x)?;
                tile.set("offsetY", y)?;
                tiles.raw_set(i + 1, tile)?;
                files.raw_set(i + 1, file)?;
            }
        }
        t.set("tiles", tiles)?;
        t.set("fileDataIDs", files)?;
        n += 1;
        out.raw_set(n, t)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_follows_the_art() {
        // Stock: a 300×200 rect is 2×1, the right tile partial.
        let r = resolve(vec![(256, 256), (64, 256)], 300, 200);
        assert_eq!((r.cols, r.rows, r.count, r.upscaled), (2, 1, 2, false));
        assert_eq!(
            tile_geometry(&r, 1, (300, 200), (10, 20)),
            (44.0, 200.0, 44.0 / 64.0, 200.0 / 256.0, 266.0, 20.0)
        );
        // A sliver column the DBC width rounds away: three columns, not two.
        let r = resolve(vec![(256, 256), (256, 256), (8, 256)], 512, 256);
        assert_eq!((r.cols, r.count), (3, 3));
        // Foreign tiles past the grid are dropped.
        let r = resolve(vec![(256, 128), (256, 128)], 256, 128);
        assert_eq!((r.cols, r.rows, r.count), (1, 1, 1));
        // An upscaled re-export: four full tiles for a one-tile rect, kept as 2×2.
        let r = resolve(vec![(256, 256); 4], 256, 256);
        assert_eq!((r.cols, r.rows, r.count, r.upscaled), (2, 2, 4, true));
        // An HD patch: every tile doubled reads like stock.
        let r = resolve(vec![(512, 512), (128, 512)], 300, 200);
        assert_eq!((r.cols, r.cell), (2, 512));
        assert_eq!(tile_geometry(&r, 1, (300, 200), (0, 0)).2, 44.0 / 64.0);
    }

    #[test]
    fn a_blp_header_reads_its_size() {
        let mut b = b"BLP2".to_vec();
        b.resize(0x14, 0);
        b[0x0c..0x10].copy_from_slice(&64u32.to_le_bytes());
        b[0x10..0x14].copy_from_slice(&128u32.to_le_bytes());
        assert_eq!(blp_dims(&b), Some((64, 128)));
        assert_eq!(blp_dims(b"PNG"), None);
    }
}

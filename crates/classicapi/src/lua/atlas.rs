//! `texture/Atlas.cpp` and `AtlasData.h`: the atlas registry behind `C_Texture.GetAtlasInfo`,
//! `GetAtlasExists`, `GetAtlasID`, `GetAtlasElementID`, `GetAtlasElements` and `RegisterAtlas`,
//! and the texture methods `SetAtlas`, `GetAtlas` and `ResetTexCoord`.
//!
//! - An atlas is a texture and a sub-rect: `SetAtlas` sets both, and the size with
//!   `useAtlasSize`. The built-in bindings are the DLL's six, each a 1.12 file carrying the art of
//!   a Classic Era atlas; `RegisterAtlas` adds an addon's own, with negative synthetic ids.
//! - With an atlas on, texture coordinates address the sprite: `SetTexCoord` takes them in sprite
//!   space and draws them composed into the atlas rect, `GetTexCoord` hands them back, and
//!   `ResetTexCoord` returns to the whole sprite. A later `SetTexture` to another file drops the
//!   atlas.
//! - Deviation, as in the DLL: an unknown name leaves the texture as it was, where retail clears it,
//!   so a ported frame stays visible.

use std::collections::BTreeMap;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::lua::{as_string, num, to_number, truthy, Api};

/// One atlas.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Info {
    name: String,
    file: String,
    width: f64,
    height: f64,
    /// `(left, right, top, bottom)` within the file.
    rect: (f64, f64, f64, f64),
    atlas_id: i64,
    element_id: i64,
    tiles: (bool, bool),
}

/// The registry, keyed by lowercased name.
pub(crate) struct Atlases {
    by_name: BTreeMap<String, Info>,
    next_synthetic: i64,
}

impl Default for Atlases {
    fn default() -> Self {
        let mut by_name = BTreeMap::new();
        // `AtlasData::kAtlases`: one widget match and five name matches, sizes from the 1.12 files.
        for (name, file, atlas_id, element_id) in [
            (
                "chatframe-button-highlightalert",
                r"Interface\ChatFrame\UI-ChatIcon-BlinkHilight",
                924,
                10841,
            ),
            ("MinimapArrow", r"Interface\Minimap\MinimapArrow", 647, 9441),
            ("Repair", r"Interface\CURSOR\Repair", 647, 9509),
            (
                "Rotating-MinimapArrow",
                r"Interface\Minimap\ROTATING-MINIMAPARROW",
                647,
                9442,
            ),
            (
                "Rotating-MinimapGroupArrow",
                r"Interface\Minimap\Rotating-MinimapGroupArrow",
                647,
                9443,
            ),
            (
                "Rotating-MinimapGuideArrow",
                r"Interface\Minimap\ROTATING-MINIMAPGUIDEARROW",
                647,
                9444,
            ),
        ] {
            by_name.insert(
                name.to_ascii_lowercase(),
                Info {
                    name: name.into(),
                    file: file.into(),
                    width: 32.0,
                    height: 32.0,
                    rect: (0.0, 1.0, 0.0, 1.0),
                    atlas_id,
                    element_id,
                    tiles: (false, false),
                },
            );
        }
        Self {
            by_name,
            next_synthetic: -1,
        }
    }
}

impl Atlases {
    pub(crate) fn find(&self, name: &str) -> Option<&Info> {
        self.by_name.get(&name.to_ascii_lowercase())
    }
}

/// The registry slots: each atlas'd texture's state, keyed weakly by the texture, and the
/// original `SetTexCoord` / `GetTexCoord`.
const REG_APPLIED: &str = "__classicapi_atlas_applied";
const REG_SET_TEXCOORD: &str = "__classicapi_atlas_set_texcoord";
const REG_GET_TEXCOORD: &str = "__classicapi_atlas_get_texcoord";

const IDENTITY: [f64; 8] = [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0];

/// `Compose`: sprite-space corners onto the atlas rect.
fn compose(rect: (f64, f64, f64, f64), user: &[f64; 8]) -> [f64; 8] {
    let (l, r, t, b) = rect;
    let mut out = [0.0; 8];
    for i in 0..4 {
        out[i * 2] = l + user[i * 2] * (r - l);
        out[i * 2 + 1] = t + user[i * 2 + 1] * (b - t);
    }
    out
}

/// A path as compared: back slashes, lower case, no `.blp`.
fn path_key(p: &str) -> String {
    let p = p.replace('/', "\\").to_ascii_lowercase();
    p.strip_suffix(".blp").map(str::to_string).unwrap_or(p)
}

fn applied(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value(REG_APPLIED)
}

/// The atlas live on `tex`: its entry while the texture still shows the atlas's file, dropping a
/// stale one.
fn live(lua: &Lua, tex: &Table) -> mlua::Result<Option<Table>> {
    let map = applied(lua)?;
    let Value::Table(entry) = map.raw_get::<Value>(tex.clone())? else {
        return Ok(None);
    };
    let current: Value = mlua::ObjectLike::call_method(tex, "GetTexture", ())?;
    let file: String = entry.get("file")?;
    if as_string(&current).is_some_and(|c| path_key(&c) == path_key(&file)) {
        return Ok(Some(entry));
    }
    map.raw_set(tex.clone(), Value::Nil)?;
    Ok(None)
}

fn write_corners(lua: &Lua, tex: &Table, corners: [f64; 8]) -> mlua::Result<()> {
    let set: Function = lua.named_registry_value(REG_SET_TEXCOORD)?;
    let mut args = vec![Value::Table(tex.clone())];
    args.extend(corners.into_iter().map(Value::Number));
    set.call::<()>(MultiValue::from_vec(args))
}

fn entry_rect(entry: &Table) -> mlua::Result<(f64, f64, f64, f64)> {
    Ok((
        entry.get("l")?,
        entry.get("r")?,
        entry.get("t")?,
        entry.get("b")?,
    ))
}

/// `ReadTexCoordArgs`: the 4-argument or 8-argument form in corner order.
fn read_coords(args: &[Value]) -> Option<[f64; 8]> {
    let n: Vec<f64> = args.iter().map(num).collect::<Option<_>>()?;
    match n[..] {
        [l, r, t, b] => Some([l, t, l, b, r, t, r, b]),
        [a, b, c, d, e, f, g, h] => Some([a, b, c, d, e, f, g, h]),
        _ => None,
    }
}

fn info_table(lua: &Lua, info: &Info) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("elementName", info.name.as_str())?;
    t.set("width", info.width)?;
    t.set("height", info.height)?;
    t.set("leftTexCoord", info.rect.0)?;
    t.set("rightTexCoord", info.rect.1)?;
    t.set("topTexCoord", info.rect.2)?;
    t.set("bottomTexCoord", info.rect.3)?;
    t.set("tilesHorizontally", info.tiles.0)?;
    t.set("tilesVertically", info.tiles.1)?;
    t.set("filename", info.file.as_str())?;
    Ok(t)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let lua = api.lua;
    let weak = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set("__mode", "k")?;
    weak.set_metatable(Some(meta))?;
    lua.set_named_registry_value(REG_APPLIED, weak)?;

    const NS: &str = "C_Texture";
    let c = api.ca.clone();
    api.table(NS, "GetAtlasInfo", move |lua, v: Value| {
        let info = as_string(&v).and_then(|n| c.lock().atlases.find(&n).cloned());
        match info {
            Some(i) => Ok(Value::Table(info_table(lua, &i)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetAtlasExists", move |_, v: Value| {
        Ok(as_string(&v).is_some_and(|n| c.lock().atlases.find(&n).is_some()))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetAtlasID", move |_, v: Value| {
        Ok(as_string(&v).and_then(|n| c.lock().atlases.find(&n).map(|i| i.atlas_id)))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetAtlasElementID", move |_, v: Value| {
        Ok(as_string(&v).and_then(|n| c.lock().atlases.find(&n).map(|i| i.element_id)))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetAtlasElements", move |lua, ()| {
        let names: Vec<String> = c
            .lock()
            .atlases
            .by_name
            .values()
            .map(|i| i.name.clone())
            .collect();
        lua.create_sequence_from(names)
    })?;
    let c = api.ca.clone();
    api.table(NS, "RegisterAtlas", move |_, args: MultiValue| {
        let a: Vec<Value> = args.into_iter().collect();
        let get = |i: usize| a.get(i).cloned().unwrap_or(Value::Nil);
        let (Some(name), Some(file)) = (as_string(&get(0)), as_string(&get(1))) else {
            return Err(mlua::Error::runtime(
                "Usage: C_Texture.RegisterAtlas(name, file, width, height, left, right, top, bottom [, tilesH, tilesV])",
            ));
        };
        let or = |i: usize, d: f64| num(&get(i)).unwrap_or(d);
        let mut st = c.lock();
        let reg = &mut st.atlases;
        let key = name.to_ascii_lowercase();
        let (atlas_id, element_id) = match reg.by_name.get(&key) {
            Some(old) => (old.atlas_id, old.element_id),
            None => {
                let id = reg.next_synthetic;
                reg.next_synthetic -= 1;
                (id, id)
            }
        };
        reg.by_name.insert(
            key,
            Info {
                name,
                file,
                width: to_number(&get(2)),
                height: to_number(&get(3)),
                rect: (or(4, 0.0), or(5, 1.0), or(6, 0.0), or(7, 1.0)),
                atlas_id,
                element_id,
                tiles: (truthy(&get(8)), truthy(&get(9))),
            },
        );
        Ok(true)
    })?;

    // The texture methods. The originals go first, so the composed writes bypass the wrapper.
    let set_tc = lua.create_function(|lua, (tex, args): (Table, MultiValue)| {
        let a: Vec<Value> = args.into_iter().collect();
        if let (Some(entry), Some(user)) = (live(lua, &tex)?, read_coords(&a)) {
            entry.set("user", lua.create_sequence_from(user)?)?;
            return write_corners(lua, &tex, compose(entry_rect(&entry)?, &user));
        }
        let orig: Function = lua.named_registry_value(REG_SET_TEXCOORD)?;
        let mut call = vec![Value::Table(tex)];
        call.extend(a);
        orig.call::<()>(MultiValue::from_vec(call))
    })?;
    if let Some(old) =
        benilla_ui::script::ext_read::replace_texture_method(lua, "SetTexCoord", set_tc)?
    {
        lua.set_named_registry_value(REG_SET_TEXCOORD, old)?;
    }
    let get_tc = lua.create_function(|lua, tex: Table| -> mlua::Result<MultiValue> {
        if let Some(entry) = live(lua, &tex)? {
            let user: Vec<f64> = entry.get("user")?;
            return Ok(MultiValue::from_vec(
                user.into_iter().map(Value::Number).collect(),
            ));
        }
        let orig: Function = lua.named_registry_value(REG_GET_TEXCOORD)?;
        orig.call(tex)
    })?;
    if let Some(old) =
        benilla_ui::script::ext_read::replace_texture_method(lua, "GetTexCoord", get_tc)?
    {
        lua.set_named_registry_value(REG_GET_TEXCOORD, old)?;
    }
    let c = api.ca.clone();
    let set_atlas =
        lua.create_function(move |lua, (tex, name, use_size): (Table, Value, Value)| {
            let map = applied(lua)?;
            let Some(name) = as_string(&name) else {
                map.raw_set(tex, Value::Nil)?;
                return Ok(());
            };
            let Some(info) = c.lock().atlases.find(&name).cloned() else {
                map.raw_set(tex, Value::Nil)?;
                return Ok(());
            };
            mlua::ObjectLike::call_method::<()>(&tex, "SetTexture", info.file.as_str())?;
            write_corners(lua, &tex, compose(info.rect, &IDENTITY))?;
            if truthy(&use_size) && info.width > 0.0 && info.height > 0.0 {
                mlua::ObjectLike::call_method::<()>(&tex, "SetWidth", info.width)?;
                mlua::ObjectLike::call_method::<()>(&tex, "SetHeight", info.height)?;
            }
            let entry = lua.create_table()?;
            entry.set("name", info.name.as_str())?;
            entry.set("file", info.file.as_str())?;
            entry.set("l", info.rect.0)?;
            entry.set("r", info.rect.1)?;
            entry.set("t", info.rect.2)?;
            entry.set("b", info.rect.3)?;
            entry.set("user", lua.create_sequence_from(IDENTITY)?)?;
            map.raw_set(tex, entry)?;
            Ok(())
        })?;
    benilla_ui::script::ext_read::replace_texture_method(lua, "SetAtlas", set_atlas)?;
    let get_atlas = lua.create_function(|lua, tex: Table| {
        Ok(match live(lua, &tex)? {
            Some(e) => Some(e.get::<String>("name")?),
            None => None,
        })
    })?;
    benilla_ui::script::ext_read::replace_texture_method(lua, "GetAtlas", get_atlas)?;
    let reset = lua.create_function(|lua, tex: Table| match live(lua, &tex)? {
        Some(entry) => {
            entry.set("user", lua.create_sequence_from(IDENTITY)?)?;
            write_corners(lua, &tex, compose(entry_rect(&entry)?, &IDENTITY))
        }
        None => write_corners(lua, &tex, IDENTITY),
    })?;
    benilla_ui::script::ext_read::replace_texture_method(lua, "ResetTexCoord", reset)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_compose_into_the_rect() {
        let out = compose(
            (0.5, 1.0, 0.0, 0.5),
            &[0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
        );
        assert_eq!(out, [0.5, 0.0, 0.5, 0.5, 1.0, 0.0, 1.0, 0.5]);
        assert_eq!(
            read_coords(&[0.25, 0.75, 0.0, 1.0].map(Value::Number)),
            Some([0.25, 0.0, 0.25, 1.0, 0.75, 0.0, 0.75, 1.0])
        );
    }

    #[test]
    fn an_atlas_sets_the_texture_and_addresses_the_sprite() {
        let ca = crate::Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        let out: String = script
            .lua()
            .load(
                r#"
                C_Texture.RegisterAtlas("probe-sprite", "Interface\\Probe\\Sheet", 16, 8, 0.5, 1, 0, 0.5)
                local f = CreateFrame("Frame")
                local t = f:CreateTexture()
                t:SetAtlas("probe-sprite", true)
                local a = { t:GetTexCoord() }
                t:SetTexCoord(0, 0.5, 0, 1)
                local b = { t:GetTexCoord() }
                local name = t:GetAtlas()
                t:SetAtlas("no-such-atlas")
                local info = C_Texture.GetAtlasInfo("PROBE-SPRITE")
                t:SetAtlas("probe-sprite")
                t:SetTexture("Interface\\Other")
                return a[5] .. a[8] .. " " .. b[5] .. " " .. name .. " " .. tostring(t:GetAtlas())
                  .. " " .. t:GetWidth() .. "x" .. t:GetHeight()
                  .. " " .. info.leftTexCoord .. info.filename .. C_Texture.GetAtlasID("probe-sprite")
                  .. tostring(C_Texture.GetAtlasExists("Repair")) .. C_Texture.GetAtlasElementID("Repair")
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            r"11 0.5 probe-sprite nil 16x8 0.5Interface\Probe\Sheet-1true9509"
        );
    }
}

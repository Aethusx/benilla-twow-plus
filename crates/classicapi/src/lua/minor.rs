//! The small DLL folders, one or two natives each:
//!
//! - `classes/Color.cpp`: `C_ClassColor.GetClassColor`, the stock `RAID_CLASS_COLORS` entry.
//! - `instance/Info.cpp`: `GetInstanceInfo`'s nine values from `Map.dbc` (difficulty always
//!   Normal, the type's canonical player cap).
//! - `guid/CreatureInfo.cpp`: `C_CreatureInfo.GetCreatureID`, a unit's live entry while in view,
//!   else a creature guid's packed entry.
//! - `talent/Info.cpp`: `GetTalentSpellID`, `GetTalentIDByIndex`, from `Talent.dbc` and
//!   `TalentTab.dbc` in the engine's order (a class's tabs in file order by both masks, a tab's
//!   talents in file order); the player's tree, or any class's with `classID`.
//! - `glue/Session.cpp`: `C_Glue.IsOnGlueScreen`, false in the world.
//! - `frame/MouseFoci.cpp`, `frame/ClickEvents.cpp`, `input/GlobalMouse.cpp`: `GetMouseFoci`,
//!   `GetMouseButtonClicked` with the `PreClick`/`PostClick` bracket, `IsMouseButtonDown` and
//!   `GLOBAL_MOUSE_DOWN`/`GLOBAL_MOUSE_UP` (fired by the frame from the mouse state).
//! - `clipboard/Copy.cpp`: `CopyToClipboard`, optionally markup-stripped, onto the OS pasteboard.
//! - `chat/CurrentGUID.cpp`, `event/Registered.cpp`: `GetCurrentChatGUID`,
//!   `GetFramesRegisteredForEvent`.
//! - `spell/scrape/CraftID.cpp`: `GetCraftSpellID`, the spell `GetCraftItemLink`'s link names.
//! - `player/CanUseItem.cpp`: `C_PlayerInfo.CanUseItem`, the client's item-usable predicate.
//! - `gameobject/ClosestPosition.cpp`: `ClosestGameObjectPosition`.
//! - `chatbubble/Info.cpp`: `C_ChatBubbles.GetAllChatBubbles`. Deviation: benilla draws bubbles as
//!   an overlay, not frames, so the list is always empty.

use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::lua::{as_string, is_number, to_int, to_str, truthy, Api};
use crate::mirror::typemask;
use crate::talents::talent_col;
use crate::Ca;

/// `TalentTab.dbc`'s race and class masks.
const TAB_RACE_MASK: usize = 11;
const TAB_CLASS_MASK: usize = 12;
/// The mouse buttons `IsMouseButtonDown` numbers 1..5.
pub(crate) const MOUSE_BUTTONS: [&str; 5] = [
    "LeftButton",
    "RightButton",
    "MiddleButton",
    "Button4",
    "Button5",
];

/// `CapForType`: the canonical player cap of a `Map.dbc` instance type.
fn cap_for_type(ty: u32) -> u32 {
    match ty {
        1 | 4 => 5,
        2 | 3 => 40,
        _ => 0,
    }
}

/// The Talent.dbc row for a class's `tab`-th tab's `talent`-th talent; `race` 0 tests the class
/// mask alone (`NthClassTalentTabID`), else both (the player's own tree, `0x4f2e50`).
fn talent_row(ca: &Ca, class: u32, race: u32, tab: i64, talent: i64) -> Option<u32> {
    if !(1..=31).contains(&class) || tab < 1 || talent < 1 {
        return None;
    }
    let fits = |mask: u32, id: u32| id == 0 || mask == 0 || mask & (1 << ((id - 1) & 31)) != 0;
    let tabs = ca.db.get("TalentTab")?;
    let tab_id = tabs
        .rows()
        .filter(|r| fits(r.u32(TAB_CLASS_MASK), class) && fits(r.u32(TAB_RACE_MASK), race))
        .nth(tab as usize - 1)?
        .id();
    let talents = ca.db.get("Talent")?;
    let id = talents
        .rows()
        .filter(|r| r.u32(talent_col::TAB) == tab_id)
        .nth(talent as usize - 1)?
        .id();
    Some(id)
}

/// The player's `(class, race)`, 0s before the player exists.
fn player_class_race(ca: &Ca) -> (u32, u32) {
    ca.lock()
        .mirror
        .me()
        .map_or((0, 0), |f| (u32::from(f.class()), u32::from(f.race())))
}

/// `StripMarkup`: colour, hyperlink, texture and atlas escapes out, `|n` a newline, `||` a pipe;
/// an unknown `|x` keeps its pipe.
pub(crate) fn strip_markup(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let skip_to = |mut i: usize, closer: u8| {
        while i < b.len() && !(b[i] == b'|' && b.get(i + 1) == Some(&closer)) {
            i += 1;
        }
        i
    };
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'|' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1).copied() {
            Some(b'|') => {
                out.push(b'|');
                i += 2;
            }
            Some(b'n') => {
                out.push(b'\n');
                i += 2;
            }
            Some(b'r' | b'h' | b't' | b'a') => i += 2,
            Some(b'c') => {
                i += 2;
                let mut k = 0;
                while k < 8 && i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                    k += 1;
                }
            }
            Some(b'H') => i = skip_to(i + 2, b'h'),
            Some(b'T') => i = skip_to(i + 2, b't'),
            Some(b'A') => i = skip_to(i + 2, b'a'),
            _ => {
                out.push(b'|');
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn frames(lua: &Lua, ids: Vec<u32>) -> Vec<Value> {
    ids.into_iter()
        .map(|id| benilla_ui::script::ext_read::frame_value(lua, id))
        .filter(|v| !v.is_nil())
        .collect()
}

/// The spell id an `|Hspell:<id>|h` link carries.
fn spell_in_link(link: &str) -> Option<u32> {
    let rest = &link[link.find("spell:")? + 6..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|id| *id > 0)
}

/// The mouse-button state the frame keeps, and the clipboard writes it hands to benilla.
#[derive(Default)]
pub(crate) struct Input {
    /// Bit `n` set: `MOUSE_BUTTONS[n]` held.
    pub(crate) mouse_mask: u8,
    pub(crate) clipboard: Vec<String>,
}

impl Input {
    /// Diff the held buttons against last frame: `(button, down)` for each that moved.
    pub(crate) fn mouse(&mut self, mask: u8) -> Vec<(&'static str, bool)> {
        let moved = self.mouse_mask ^ mask;
        self.mouse_mask = mask;
        (0..5)
            .filter(|b| moved & (1 << b) != 0)
            .map(|b| (MOUSE_BUTTONS[b], mask & (1 << b) != 0))
            .collect()
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.table("C_ClassColor", "GetClassColor", |lua, v: Value| {
        if as_string(&v).is_none() {
            return Ok(Value::Nil);
        }
        match lua.globals().get::<Value>("RAID_CLASS_COLORS")? {
            Value::Table(t) => t.get::<Value>(v),
            _ => Ok(Value::Nil),
        }
    })?;

    let c = api.ca.clone();
    api.global("GetInstanceInfo", move |lua, ()| {
        let map = c.lock().mirror.map_id;
        let row = c.db.get("Map").and_then(|t| {
            let r = t.row_or_zero(map)?;
            Some((r.loc(4).to_string(), r.u32(2)))
        });
        let (name, ty) = row.unwrap_or_default();
        let type_name = ["none", "party", "raid", "pvp", "arena"]
            .get(ty as usize)
            .copied()
            .unwrap_or("none");
        let cap = cap_for_type(ty);
        (name, type_name, 1, "Normal", cap, 0, false, map, cap).into_lua_multi(lua)
    })?;

    let c = api.ca.clone();
    api.table("C_CreatureInfo", "GetCreatureID", move |_, v: Value| {
        let Some(guid) = as_string(&v).and_then(|s| crate::guid::parse(&s)) else {
            return Ok(None);
        };
        let live = {
            let st = c.lock();
            st.mirror
                .object(guid)
                .filter(|f| f.is(typemask::UNIT))
                .map(|f| f.entry())
        };
        let entry = match crate::guid::classify(guid) {
            crate::guid::Kind::Creature => live.unwrap_or(crate::guid::creature_entry(guid)),
            crate::guid::Kind::Pet => live.unwrap_or(0),
            _ => 0,
        };
        Ok((entry != 0).then_some(entry))
    })?;

    // `GetTalentSpellID(tab, talent [, rank [, classID]])`: the rank's spell, the player's current
    // rank (at least 1) by default; another class's tree with `classID`, rank 1 by default.
    let c = api.ca.clone();
    api.global(
        "GetTalentSpellID",
        move |lua, (tab, talent, rank, class): (Value, Value, Value, Value)| {
            if !is_number(&tab) || !is_number(&talent) {
                return Err(mlua::Error::runtime(
                    "Usage: GetTalentSpellID(tabIndex, talentIndex[, rank[, classID]])",
                ));
            }
            let (tab, talent) = (to_int(&tab), to_int(&talent));
            let explicit = is_number(&rank).then(|| to_int(&rank));
            if explicit.is_some_and(|r| !(1..=talent_col::MAX_RANKS as i64).contains(&r)) {
                return Ok(None);
            }
            let (row, rank) = if is_number(&class) {
                let row = talent_row(&c, to_int(&class) as u32, 0, tab, talent);
                (row, explicit.unwrap_or(1))
            } else {
                let (class, race) = player_class_race(&c);
                let row = talent_row(&c, class, race, tab, talent);
                let rank = match explicit {
                    Some(r) => r,
                    None if row.is_some() => {
                        // The stock `GetTalentInfo`'s fifth return, the current rank.
                        let info: Function = lua.globals().get("GetTalentInfo")?;
                        let rets: MultiValue = info.call((tab, talent)).unwrap_or_default();
                        rets.get(4).map_or(0, to_int).max(1)
                    }
                    None => 1,
                };
                (row, rank)
            };
            let spell = row.and_then(|id| {
                let s =
                    c.db.get("Talent")?
                        .row(id)?
                        .u32(talent_col::SPELL_RANK + rank as usize - 1);
                (s != 0).then_some(s)
            });
            Ok(spell)
        },
    )?;
    let c = api.ca.clone();
    api.global(
        "GetTalentIDByIndex",
        move |_, (tab, talent, class): (Value, Value, Value)| {
            if !is_number(&tab) || !is_number(&talent) {
                return Err(mlua::Error::runtime(
                    "Usage: GetTalentIDByIndex(tabIndex, talentIndex[, classID])",
                ));
            }
            let (class, race) = if is_number(&class) {
                (to_int(&class) as u32, 0)
            } else {
                player_class_race(&c)
            };
            Ok(talent_row(&c, class, race, to_int(&tab), to_int(&talent)))
        },
    )?;

    api.table("C_Glue", "IsOnGlueScreen", |_, ()| Ok(false))?;

    api.global("GetMouseFoci", |lua, ()| -> mlua::Result<Table> {
        let ids = benilla_ui::script::ext_read::mouse_foci(lua);
        lua.create_sequence_from(frames(lua, ids))
    })?;
    benilla_ui::script::ext_read::enable_click_bracket(api.lua);
    api.global("GetMouseButtonClicked", |lua, ()| {
        Ok(benilla_ui::script::ext_read::mouse_button_clicked(lua))
    })?;
    let c = api.ca.clone();
    api.global("IsMouseButtonDown", move |_, args: MultiValue| {
        let mask = c.lock().input.mouse_mask;
        let Some(v) = args.front() else {
            return Ok(mask & 0x1f != 0);
        };
        let bit = if is_number(v) {
            usize::try_from(to_int(v) - 1).ok().filter(|b| *b < 5)
        } else {
            as_string(v).and_then(|s| MOUSE_BUTTONS.iter().position(|b| *b == s))
        };
        Ok(bit.is_some_and(|b| mask & (1 << b) != 0))
    })?;

    let c = api.ca.clone();
    api.global(
        "CopyToClipboard",
        move |_, (text, strip): (Value, Value)| {
            let Some(text) = to_str(&text) else {
                return Ok(0);
            };
            let text = if truthy(&strip) {
                strip_markup(&text)
            } else {
                text
            };
            let len = text.len();
            c.lock().input.clipboard.push(text);
            Ok(len)
        },
    )?;

    api.global("GetCurrentChatGUID", |lua, ()| {
        Ok(benilla_ui::script::ext_read::current_chat_guid(lua).map(crate::guid::format))
    })?;
    api.global(
        "GetFramesRegisteredForEvent",
        |lua, v: Value| -> mlua::Result<MultiValue> {
            let Some(event) = to_str(&v) else {
                return Ok(MultiValue::new());
            };
            let ids = benilla_ui::script::ext_read::frames_registered_for_event(lua, &event);
            Ok(MultiValue::from_vec(frames(lua, ids)))
        },
    )?;

    api.global("GetCraftSpellID", |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: GetCraftSpellID(craftIndex)"));
        }
        let link: Value = match lua.globals().get::<Function>("GetCraftItemLink") {
            Ok(f) => f.call(v).unwrap_or(Value::Nil),
            Err(_) => Value::Nil,
        };
        Ok(as_string(&link).as_deref().and_then(spell_in_link))
    })?;

    api.table("C_PlayerInfo", "CanUseItem", |lua, v: Value| {
        let id = crate::lua::item::arg_id(&v);
        Ok(u32::try_from(id)
            .ok()
            .filter(|i| *i > 0)
            .and_then(|i| benilla_ui::script::ext_read::item_usable(lua, i))
            .unwrap_or(false))
    })?;

    let c = api.ca.clone();
    api.global("ClosestGameObjectPosition", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: ClosestGameObjectPosition(gameObjectID)",
            ));
        }
        let hit = crate::lua::unit::body::closest_by_entry(
            &c.lock().mirror,
            crate::guid::Kind::GameObject,
            to_int(&v) as u32,
        );
        match hit {
            Some((x, y, d2)) => (x, y, f64::from(d2).sqrt()).into_lua_multi(lua),
            None => Ok(MultiValue::new()),
        }
    })?;

    api.table("C_ChatBubbles", "GetAllChatBubbles", |lua, _: Value| {
        lua.create_table()
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_strips_to_plain_text() {
        assert_eq!(
            strip_markup("|cff1eff00|Hitem:2589:0:0:0|h[Linen Cloth]|h|r x5||n|nend|q"),
            "[Linen Cloth] x5|n\nend|q"
        );
        assert_eq!(strip_markup("a|TInterface\\Icons\\X:0|tb"), "ab");
    }

    #[test]
    fn a_spell_link_reads_its_id_and_the_mouse_diff_fires_each_edge() {
        assert_eq!(
            spell_in_link("|cffffd000|Hspell:7418|h[Enchant Bracer]|h|r"),
            Some(7418)
        );
        assert_eq!(spell_in_link("|Hitem:12|h"), None);
        let mut input = Input::default();
        assert_eq!(input.mouse(0b001), vec![("LeftButton", true)]);
        assert_eq!(
            input.mouse(0b010),
            vec![("LeftButton", false), ("RightButton", true)]
        );
        assert!(input.mouse(0b010).is_empty());
    }

    #[test]
    fn the_natives_answer_in_retail_shape() {
        let ca = crate::Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        ca.lock().input.mouse_mask = 0b100;
        let out: String = script
            .lua()
            .load(
                r#"
                RAID_CLASS_COLORS = { MAGE = { r = 0.41 } }
                local n = CopyToClipboard("|cffff0000red|r", true)
                local name, ty, diff, dname, cap, _, dyn, map = GetInstanceInfo()
                return C_ClassColor.GetClassColor("MAGE").r .. tostring(C_ClassColor.GetClassColor(1))
                  .. " " .. n .. ty .. diff .. dname .. cap .. tostring(dyn) .. map
                  .. " " .. tostring(IsMouseButtonDown()) .. tostring(IsMouseButtonDown(3))
                  .. tostring(IsMouseButtonDown("MiddleButton")) .. tostring(IsMouseButtonDown("LeftButton"))
                  .. " " .. C_CreatureInfo.GetCreatureID("0xF13000064E00ABCD")
                  .. tostring(C_CreatureInfo.GetCreatureID("0x0000000000000001"))
                  .. " " .. tostring(GetCurrentChatGUID()) .. table.getn(C_ChatBubbles.GetAllChatBubbles())
                  .. tostring(C_Glue.IsOnGlueScreen())
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            "0.41nil 3none1Normal0false0 truetruetruefalse 1614nil nil0false"
        );
        assert_eq!(ca.lock().input.clipboard, vec!["red".to_string()]);
    }
    #[test]
    fn a_classes_talents_resolve_from_the_install() {
        let _ = benilla_formats::wow_data_or_skip!();
        let ca = crate::Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        // Mage (8): every tab's first talent has an id and a rank-1 spell; tab 4 has none.
        let out: String = script
            .lua()
            .load(
                r#"
                local s = ""
                for tab = 1, 4 do
                  local id = GetTalentIDByIndex(tab, 1, 8)
                  local spell = GetTalentSpellID(tab, 1, 1, 8)
                  s = s .. tostring(id ~= nil) .. tostring(spell ~= nil) .. " "
                end
                return s .. tostring(GetTalentSpellID(1, 1, 9, 8))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "truetrue truetrue truetrue falsefalse nil");
    }
}

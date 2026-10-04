//! `gossip/Info.cpp`: `C_GossipInfo`, retail-shaped over 1.12's gossip menu and, as the DLL does,
//! the questgiver greeting panel when that is the live session instead.
//!
//! - A gossip menu open answers from it; else an open greeting panel answers the text and quest
//!   lists; else nothing ("" and empty tables). Options are the gossip menu's alone.
//! - Option rows: `gossipOptionID` (the wire index), `name`, `icon` (the wire's 0-10 byte, not a
//!   file id), `flags` (1 for a password option), `orderIndex` (1-based display order).
//! - Quest rows: `questID`, `title`, `questLevel`, and `isComplete` (wire status 4) for active
//!   ones. 1.12's server sends no rewards, spell, status or icon override.
//! - The selectors find the row and select it through the stock 1.12 verbs.

use mlua::{Function, Lua, Table, Value};

use crate::lua::{is_number, to_int, to_str, Api};

/// One quest row: `(questID, title, level, complete)`.
type Row = (u32, String, u32, bool);

/// The live session's quest rows, `active` or available.
fn quests(lua: &Lua, active: bool) -> Option<(bool, Vec<Row>)> {
    if let Some(menu) = benilla_ui::script::ext_read::gossip_menu(lua) {
        let rows = menu
            .quests
            .into_iter()
            .filter(|q| q.active == active)
            .map(|q| (q.quest_id, q.title, q.level, q.complete))
            .collect();
        return Some((true, rows));
    }
    let (_, act, avail) = benilla_ui::script::ext_read::quest_greeting(lua)?;
    let rows = if active { act } else { avail };
    Some((
        false,
        rows.into_iter()
            .map(|(t, q)| (q.quest_id, t, q.level, q.complete))
            .collect(),
    ))
}

fn quest_table(lua: &Lua, active: bool) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (i, (id, title, level, complete)) in quests(lua, active)
        .map(|q| q.1)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let row = lua.create_table()?;
        row.set("questID", id)?;
        row.set("title", title)?;
        row.set("questLevel", level)?;
        if active {
            row.set("isComplete", complete)?;
        }
        out.raw_set(i + 1, row)?;
    }
    Ok(out)
}

fn call(lua: &Lua, verb: &str, args: impl mlua::IntoLuaMulti) -> mlua::Result<()> {
    match lua.globals().get::<Function>(verb) {
        Ok(f) => f.call::<()>(args),
        Err(_) => Ok(()),
    }
}

fn number_arg(v: &Value, usage: &str) -> mlua::Result<i64> {
    if !is_number(v) {
        return Err(mlua::Error::runtime(usage.to_string()));
    }
    Ok(to_int(v))
}

/// `SelectQuestByID`: the row's 1-based place among its side, through the session's verb.
fn select_quest(lua: &Lua, active: bool, quest: i64) -> mlua::Result<()> {
    let Some((gossip, rows)) = quests(lua, active) else {
        return Ok(());
    };
    let Some(i) = rows.iter().position(|r| i64::from(r.0) == quest) else {
        return Ok(());
    };
    let verb = match (gossip, active) {
        (true, true) => "SelectGossipActiveQuest",
        (true, false) => "SelectGossipAvailableQuest",
        (false, true) => "SelectActiveQuest",
        (false, false) => "SelectAvailableQuest",
    };
    call(lua, verb, i + 1)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_GossipInfo";
    api.table(NS, "GetText", |lua, ()| {
        Ok(benilla_ui::script::ext_read::gossip_menu(lua)
            .map(|m| m.greeting)
            .or_else(|| benilla_ui::script::ext_read::quest_greeting(lua).map(|g| g.0))
            .unwrap_or_default())
    })?;
    api.table(NS, "GetOptions", |lua, ()| {
        let out = lua.create_table()?;
        let options = benilla_ui::script::ext_read::gossip_menu(lua)
            .map(|m| m.options)
            .unwrap_or_default();
        for (i, o) in options.into_iter().enumerate() {
            let row = lua.create_table()?;
            row.set("gossipOptionID", o.index)?;
            row.set("name", o.label)?;
            row.set("icon", o.icon)?;
            row.set("flags", u8::from(o.coded))?;
            row.set("orderIndex", i + 1)?;
            out.raw_set(i + 1, row)?;
        }
        Ok(out)
    })?;
    api.table(NS, "GetNumOptions", |lua, ()| {
        Ok(benilla_ui::script::ext_read::gossip_menu(lua).map_or(0, |m| m.options.len()))
    })?;
    api.table(NS, "GetAvailableQuests", |lua, ()| quest_table(lua, false))?;
    api.table(NS, "GetActiveQuests", |lua, ()| quest_table(lua, true))?;
    api.table(NS, "GetNumAvailableQuests", |lua, ()| {
        Ok(quests(lua, false).map_or(0, |q| q.1.len()))
    })?;
    api.table(NS, "GetNumActiveQuests", |lua, ()| {
        Ok(quests(lua, true).map_or(0, |q| q.1.len()))
    })?;
    api.table(NS, "SelectOption", |lua, (id, text): (Value, Value)| {
        let id = number_arg(
            &id,
            "Usage: C_GossipInfo.SelectOption(gossipOptionID [, text])",
        )?;
        let Some(menu) = benilla_ui::script::ext_read::gossip_menu(lua) else {
            return Ok(());
        };
        if let Some(i) = menu.options.iter().position(|o| i64::from(o.index) == id) {
            call(lua, "SelectGossipOption", (i + 1, to_str(&text)))?;
        }
        Ok(())
    })?;
    api.table(NS, "SelectOptionByIndex", |lua, v: Value| {
        let i = number_arg(&v, "Usage: C_GossipInfo.SelectOptionByIndex(orderIndex)")?;
        let open = benilla_ui::script::ext_read::gossip_menu(lua)
            .is_some_and(|m| i >= 1 && (i as usize) <= m.options.len());
        if open {
            call(lua, "SelectGossipOption", i)?;
        }
        Ok(())
    })?;
    api.table(NS, "SelectAvailableQuest", |lua, v: Value| {
        let q = number_arg(&v, "Usage: C_GossipInfo.SelectAvailableQuest(questID)")?;
        select_quest(lua, false, q)
    })?;
    api.table(NS, "SelectActiveQuest", |lua, v: Value| {
        let q = number_arg(&v, "Usage: C_GossipInfo.SelectActiveQuest(questID)")?;
        select_quest(lua, true, q)
    })?;
    api.table(NS, "CloseGossip", |lua, ()| call(lua, "CloseGossip", ()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use benilla_ui::script::{GossipMenu, GossipOptionView, GossipQuestRow};

    #[test]
    fn the_menu_reads_in_retail_shape_and_selects_by_id() {
        let mut script = crate::lua::test_support::vm(&crate::Ca::default());
        script.set_gossip(Some(GossipMenu {
            greeting: "Hail".into(),
            quests: vec![
                GossipQuestRow {
                    title: "A".into(),
                    level: 10,
                    active: false,
                    quest_id: 101,
                    complete: false,
                },
                GossipQuestRow {
                    title: "B".into(),
                    level: 12,
                    active: true,
                    quest_id: 202,
                    complete: true,
                },
            ],
            options: vec![GossipOptionView {
                label: "Train me".into(),
                icon_type: "trainer".into(),
                index: 7,
                icon: 3,
                coded: true,
            }],
        }));
        let out: String = script
            .lua()
            .load(
                r#"
                local G = C_GossipInfo
                local picked
                SelectGossipActiveQuest = function(i) picked = "active" .. i end
                SelectGossipOption = function(i, t) picked = (picked or "") .. " opt" .. i .. tostring(t) end
                local o = G.GetOptions()[1]
                local a = G.GetActiveQuests()[1]
                G.SelectActiveQuest(202)
                G.SelectOption(7, "pw")
                G.SelectOption(99)
                return G.GetText() .. " " .. o.gossipOptionID .. o.name .. o.icon .. o.flags .. o.orderIndex
                  .. " " .. a.questID .. a.title .. a.questLevel .. tostring(a.isComplete)
                  .. " " .. G.GetNumAvailableQuests() .. G.GetNumActiveQuests() .. G.GetNumOptions()
                  .. " " .. picked
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "Hail 7Train me311 202B12true 111 active1 opt1pw");
    }
}

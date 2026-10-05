//! `quest/`: `C_QuestLog`'s log lookups, title and details from the quest template cache
//! ([`crate::templates`]), and `GetQuestLogLeaderBoardID`.
//!
//! - Log indexes are 1-based rows of the log as the interface shows it, headers included, the
//!   numbering `GetQuestLogTitle` uses. A header row's quest id is 0.
//! - `IsOnQuest` also counts a quest folded under a collapsed header, which is in the log though
//!   not in its rows; the DLL walks the rows alone.
//! - `IsUnitOnQuest` reads the unit's `PLAYER_QUEST_LOG` descriptor slots, for a player whose
//!   descriptor we hold.
//! - The template readers answer nil until the quest is cached (`RequestLoadQuestByID`).

use benilla_protocol::messages::QuestTemplate;
use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::lua::{is_number, none, to_int, to_str, Api};
use crate::mirror::typemask;
use crate::Ca;

/// `PLAYER_QUEST_LOG_1_1`'s slots: three fields each, 20 of them.
const PLAYER_QUEST_LOG: usize = crate::mirror::field::PLAYER_QUEST_LOG_1_1;
const QUEST_LOG_SLOTS: usize = 20;
const QUEST_LOG_STRIDE: usize = 3;
/// `QuestInfo.dbc`'s localized name.
const QUEST_INFO_NAME: usize = 1;
/// `QUEST_FLAGS_SHARABLE`.
const SHARABLE: u32 = 0x08;

/// The log's rows as `(quest id, header)`.
fn rows(lua: &Lua) -> Vec<(u32, bool)> {
    benilla_ui::script::ext_read::quest_log(lua)
        .map(|l| {
            l.entries
                .iter()
                .map(|e| (if e.is_header { 0 } else { e.quest_id }, e.is_header))
                .collect()
        })
        .unwrap_or_default()
}

/// `IndexForQuestID`: the 0-based row holding `quest`.
fn index_for(rows: &[(u32, bool)], quest: i64) -> Option<usize> {
    if quest <= 0 {
        return None;
    }
    rows.iter()
        .position(|&(id, header)| !header && i64::from(id) == quest)
}

fn number(v: &Value, usage: &str) -> mlua::Result<i64> {
    if !is_number(v) {
        return Err(mlua::Error::runtime(usage.to_string()));
    }
    Ok(to_int(v))
}

fn template(ca: &Ca, quest: i64) -> Option<Box<QuestTemplate>> {
    let id = u32::try_from(quest).ok().filter(|i| *i > 0)?;
    ca.lock().templates.quests.get(&id).cloned()
}

/// The objectives in the engine's order: creatures and objects first, then items, skipping
/// empty slots. Each is `(kind, id, count, slot)`, `slot` the template's objective index.
fn objectives(q: &QuestTemplate) -> Vec<(&'static str, u32, u32, usize)> {
    let mut out = Vec::new();
    for (i, o) in q.objectives.iter().enumerate() {
        // Raw on the wire: a creature entry, or a negative gameobject entry.
        let v = o.creature_or_go as i32;
        if v != 0 {
            let kind = if v < 0 { "object" } else { "monster" };
            out.push((kind, v.unsigned_abs(), o.required_count, i));
        }
    }
    for (i, o) in q.objectives.iter().enumerate() {
        if o.item_id != 0 {
            out.push(("item", o.item_id, o.item_count, i));
        }
    }
    out
}

fn item_list(lua: &Lua, items: &[(u32, u32)]) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (i, &(id, count)) in items.iter().filter(|(id, _)| *id != 0).enumerate() {
        let t = lua.create_table()?;
        t.set("id", id)?;
        t.set("count", count)?;
        out.raw_set(i + 1, t)?;
    }
    Ok(out)
}

/// `GetQuestDetails`' table.
fn details(lua: &Lua, ca: &Ca, q: &QuestTemplate) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("questID", q.quest_id)?;
    t.set("title", q.title.as_str())?;
    t.set("level", q.level as i32)?;
    let tag = ca
        .db
        .get("QuestInfo")
        .and_then(|d| {
            d.row(q.quest_type)
                .map(|r| r.loc(QUEST_INFO_NAME).to_string())
        })
        .filter(|s| !s.is_empty());
    if let Some(tag) = tag {
        t.set("questType", tag)?;
    }
    t.set("rewardMoney", q.money.max(0))?;
    t.set("requiredMoney", (-q.money).max(0))?;
    t.set("rewardMoneyAtMaxLevel", q.money_max_level)?;
    t.set("rewardSpellID", q.reward_spell)?;
    t.set("srcItemID", q.src_item_id)?;
    t.set("questFlags", q.flags)?;
    t.set("isSharable", q.flags & SHARABLE != 0)?;
    t.set("description", q.details.as_str())?;
    t.set("objectives", q.objectives_text.as_str())?;
    t.set("completionText", q.end_text.as_str())?;
    if q.point_map_id != 0 {
        let poi = lua.create_table()?;
        poi.set("mapID", q.point_map_id)?;
        poi.set("x", q.point_x)?;
        poi.set("y", q.point_y)?;
        poi.set("opt", q.point_opt)?;
        t.set("poi", poi)?;
    }
    t.set("rewardItems", item_list(lua, &q.rewards)?)?;
    t.set("choiceItems", item_list(lua, &q.choices)?)?;
    let reqs = lua.create_table()?;
    for (i, (kind, id, count, slot)) in objectives(q).into_iter().enumerate() {
        let r = lua.create_table()?;
        r.set("kind", kind)?;
        r.set("id", id)?;
        r.set("count", count)?;
        // The per-objective override text, by the declaring slot.
        r.set("text", q.objectives[slot].text.as_str())?;
        reqs.raw_set(i + 1, r)?;
    }
    t.set("requirements", reqs)?;
    Ok(t)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_QuestLog";
    api.table(NS, "GetQuestIDForLogIndex", |lua, v: Value| {
        let i = number(&v, "Usage: GetQuestIDForLogIndex(index)")?;
        let rows = rows(lua);
        Ok(usize::try_from(i - 1)
            .ok()
            .and_then(|i| rows.get(i))
            .map(|r| r.0))
    })?;
    api.table(NS, "GetLogIndexForQuestID", |lua, v: Value| {
        let q = number(&v, "Usage: GetLogIndexForQuestID(questID)")?;
        Ok(index_for(&rows(lua), q).map(|i| i + 1))
    })?;
    api.table(NS, "GetHeaderIndexForQuest", |lua, v: Value| {
        let q = number(&v, "Usage: GetHeaderIndexForQuest(questID)")?;
        let rows = rows(lua);
        Ok(index_for(&rows, q).and_then(|i| (0..i).rev().find(|&k| rows[k].1).map(|k| k + 1)))
    })?;
    api.table(NS, "IsOnQuest", |lua, v: Value| {
        if !is_number(&v) {
            return Ok(false);
        }
        let q = to_int(&v);
        let folded = benilla_ui::script::ext_read::quest_log(lua)
            .is_some_and(|l| l.hidden_quest_ids.iter().any(|id| i64::from(*id) == q));
        Ok(q > 0 && (index_for(&rows(lua), q).is_some() || folded))
    })?;
    let c = api.ca.clone();
    api.table(
        NS,
        "IsUnitOnQuest",
        move |lua, (unit, quest): (Value, Value)| {
            let (Some(token), true) = (to_str(&unit), is_number(&quest)) else {
                return Ok(false);
            };
            let q = to_int(&quest);
            if q <= 0 {
                return Ok(false);
            }
            let Some(guid) = benilla_ui::script::ext_read::unit_guid(lua, &token)
                .ok()
                .flatten()
            else {
                return Ok(false);
            };
            let st = c.lock();
            let Some(f) = st.mirror.object(guid).filter(|f| f.is(typemask::PLAYER)) else {
                return Ok(false);
            };
            Ok((0..QUEST_LOG_SLOTS)
                .any(|i| i64::from(f.u32(PLAYER_QUEST_LOG + i * QUEST_LOG_STRIDE)) == q))
        },
    )?;
    let c = api.ca.clone();
    api.table(NS, "GetTitleForQuestID", move |_, v: Value| {
        let q = number(&v, "Usage: C_QuestLog.GetTitleForQuestID(questID)")?;
        Ok(template(&c, q).map(|t| t.title).filter(|t| !t.is_empty()))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetNumQuestObjectives", move |_, v: Value| {
        let q = number(&v, "Usage: C_QuestLog.GetNumQuestObjectives(questID)")?;
        Ok(template(&c, q).map(|t| objectives(&t).len()))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetQuestDetails", move |lua, v: Value| {
        let q = number(&v, "Usage: C_QuestLog.GetQuestDetails(questID)")?;
        match template(&c, q) {
            Some(t) => Ok(Value::Table(details(lua, &c, &t)?)),
            None => Ok(Value::Nil),
        }
    })?;

    // `GetQuestLogLeaderBoardID(objectiveIndex [, questIndex])`: the id and kind behind the
    // objective `GetQuestLogLeaderBoard` shows at the same indices; the selected quest without a
    // quest index.
    let c = api.ca.clone();
    api.global(
        "GetQuestLogLeaderBoardID",
        move |lua, (obj, row): (Value, Value)| -> mlua::Result<MultiValue> {
            let obj = number(
                &obj,
                "Usage: GetQuestLogLeaderBoardID(objectiveIndex [, questIndex])",
            )?;
            if obj < 1 {
                return Ok(none());
            }
            let rows = rows(lua);
            let index = if is_number(&row) {
                to_int(&row)
            } else {
                i64::from(benilla_ui::script::ext_read::quest_log_selection(lua))
            };
            let Some(&(quest, false)) = usize::try_from(index - 1).ok().and_then(|i| rows.get(i))
            else {
                return Ok(none());
            };
            let Some(t) = template(&c, i64::from(quest)) else {
                return Ok(none());
            };
            match objectives(&t).get(obj as usize - 1) {
                Some(&(kind, id, ..)) => (id, kind).into_lua_multi(lua),
                None => Ok(none()),
            }
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use benilla_protocol::messages::{QuestObjective, QuestTemplate};
    use benilla_ui::script::{QuestLogEntryView, QuestLogState};

    fn quest() -> QuestTemplate {
        let o =
            |creature_or_go: i32, required_count, item_id, item_count, text: &str| QuestObjective {
                creature_or_go: creature_or_go as u32,
                required_count,
                item_id,
                item_count,
                text: text.into(),
            };
        let objectives = [
            o(0, 0, 2589, 5, ""),
            o(-1617, 1, 0, 0, "Search the cart"),
            o(299, 8, 0, 0, ""),
            o(0, 0, 0, 0, ""),
        ];
        QuestTemplate {
            quest_id: 33,
            method: 2,
            level: 5,
            zone_or_sort: 12,
            quest_type: 0,
            rep_objective_faction: 0,
            rep_objective_value: 0,
            next_quest_in_chain: 0,
            money: 75,
            money_max_level: 120,
            reward_spell: 0,
            src_item_id: 0,
            flags: 0x08,
            rewards: [(0, 0); 4],
            choices: [(0, 0); 6],
            point_map_id: 0,
            point_x: 0.0,
            point_y: 0.0,
            point_opt: 0,
            title: "Wolves Across the Border".into(),
            objectives_text: String::new(),
            details: String::new(),
            end_text: String::new(),
            objectives,
        }
    }

    #[test]
    fn the_log_and_the_template_read_in_retail_shape() {
        let ca = crate::Ca::default();
        ca.lock().templates.on_quest(Box::new(quest()));
        let mut script = crate::lua::test_support::vm(&ca);
        let row = |quest_id, header| QuestLogEntryView {
            quest_id,
            is_header: header,
            ..Default::default()
        };
        script.set_quest_log(QuestLogState {
            entries: vec![row(0, true), row(33, false), row(0, true), row(40, false)],
            num_quests: 3,
            hidden_quest_ids: vec![77],
        });
        let out: String = script
            .lua()
            .load(
                r#"
                local Q = C_QuestLog
                local d = Q.GetQuestDetails(33)
                local id, kind = GetQuestLogLeaderBoardID(2, 2)
                local item, ikind = GetQuestLogLeaderBoardID(3, 2)
                return Q.GetQuestIDForLogIndex(1) .. Q.GetQuestIDForLogIndex(2)
                  .. " " .. Q.GetLogIndexForQuestID(40) .. Q.GetHeaderIndexForQuest(40)
                  .. " " .. tostring(Q.IsOnQuest(33)) .. tostring(Q.IsOnQuest(77)) .. tostring(Q.IsOnQuest(5))
                  .. " " .. Q.GetTitleForQuestID(33) .. Q.GetNumQuestObjectives(33)
                  .. " " .. d.requirements[1].kind .. d.requirements[1].id .. d.requirements[1].text
                  .. d.requirements[3].kind .. d.requirements[3].count
                  .. " " .. d.rewardMoney .. d.requiredMoney .. tostring(d.isSharable)
                  .. " " .. id .. kind .. item .. ikind
                  .. " " .. tostring(Q.GetQuestDetails(9))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            "033 43 truetruefalse Wolves Across the Border3 object1617Search the cartitem5 750true 299monster2589item nil"
        );
    }
}

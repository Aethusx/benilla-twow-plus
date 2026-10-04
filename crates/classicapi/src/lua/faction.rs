//! `faction/`: `C_Reputation`, `GetFactionIDByIndex`, `GetFactionInfoByID`, `GetFactionParentID`,
//! `FACTION_STANDING_CHANGED(factionID, newStanding, repGained)` and the `UNIT_FACTION("player")`
//! polyfill, over benilla's reputation model.
//!
//! - A faction the player has a slot for reads the model: name, description, rank (1-8), the
//!   rank's bounds, the standing, at war, may toggle at war, header, folded, watched. One without a
//!   slot reads `Faction.dbc`: Neutral at 0, as the engine reports a faction never met.
//! - The by-ID setters run the stock verbs' bodies for the faction's slot, so every rule (no peace
//!   below -3000, the peace-forced flag) stays the stock one's.
//! - `FACTION_STANDING_CHANGED` and `GetLastStandingChange` come from the frame's diff of the
//!   model: a standing that moved after the first snapshot, never the login sync. A change of any
//!   faction's at-war, inactive or visible flag, and the first snapshot, fire `UNIT_FACTION`.
//!   Deviation: the DLL hooks the engine's notify; the diff sees the same changes a frame later.

use std::collections::HashMap;

use benilla_ui::script::{FactionEntry, ScriptValue, VisibleRow};
use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::lua::{is_number, to_int, Api};
use crate::Ca;

/// `Faction.dbc`: the parent, the localized name and description.
const PARENT: usize = 0x48 / 4;
const NAME: usize = 0x4C / 4;
const DESCRIPTION: usize = 0x70 / 4;
/// The rank bounds by rank 1-8 (`.rdata 0x80928c`).
const BOUNDS: [(i32, i32); 8] = [
    (-42000, -6000),
    (-6000, -3000),
    (-3000, 0),
    (0, 3000),
    (3000, 9000),
    (9000, 21000),
    (21000, 42000),
    (42000, 42999),
];

/// One faction as the readers report it.
struct Data {
    id: u32,
    slot: Option<u32>,
    name: String,
    description: String,
    reaction: u8,
    min: i32,
    max: i32,
    standing: i32,
    at_war: bool,
    can_toggle: bool,
    header: bool,
    collapsed: bool,
    watched: bool,
}

fn from_entry(lua: &Lua, e: &FactionEntry, watched: Option<u32>) -> Data {
    Data {
        id: e.faction_id,
        slot: Some(e.rep_list_id),
        name: e.name.clone(),
        description: e.description.clone(),
        reaction: e.standing_id,
        min: e.bar_min,
        max: e.bar_max,
        standing: e.standing,
        at_war: e.at_war,
        can_toggle: e.can_toggle_at_war,
        header: e.is_header,
        collapsed: benilla_ui::script::ext_read::reputation_header_collapsed(lua, e.faction_id)
            .unwrap_or(false),
        watched: watched == Some(e.rep_list_id),
    }
}

/// `ReadFactionData`: the model's entry, else the record's, else nothing.
fn read(lua: &Lua, ca: &Ca, id: u32) -> Option<Data> {
    if id == 0 {
        return None;
    }
    let rep = benilla_ui::script::ext_read::reputation(lua).unwrap_or_default();
    if let Some(e) = rep.entries.iter().find(|e| e.faction_id == id) {
        return Some(from_entry(lua, e, rep.watched));
    }
    let table = ca.db.get("Faction")?;
    let r = table.row(id)?;
    let (min, max) = BOUNDS[3];
    Some(Data {
        id,
        slot: None,
        name: r.loc(NAME).to_string(),
        description: r.loc(DESCRIPTION).to_string(),
        reaction: 4,
        min,
        max,
        standing: 0,
        at_war: false,
        can_toggle: false,
        header: benilla_ui::script::ext_read::reputation_header_collapsed(lua, id).is_some(),
        collapsed: benilla_ui::script::ext_read::reputation_header_collapsed(lua, id)
            .unwrap_or(false),
        watched: false,
    })
}

fn table(lua: &Lua, d: &Data) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("factionID", d.id)?;
    t.set("name", d.name.as_str())?;
    t.set("description", d.description.as_str())?;
    t.set("reaction", d.reaction)?;
    t.set("currentReactionThreshold", d.min)?;
    t.set("nextReactionThreshold", d.max)?;
    t.set("currentStanding", d.standing)?;
    t.set("atWarWith", d.at_war)?;
    t.set("canToggleAtWar", d.can_toggle)?;
    t.set("isHeader", d.header)?;
    t.set("isHeaderWithRep", false)?;
    t.set("isCollapsed", d.collapsed)?;
    t.set("isWatched", d.watched)?;
    t.set("canSetInactive", !d.header && d.slot.is_some())?;
    t.set("isChild", false)?;
    t.set("hasBonusRepGain", false)?;
    t.set("isAccountWide", false)?;
    Ok(t)
}

/// 1 or nil, as `GetFactionInfo` answers its flags.
fn flag(b: bool) -> Value {
    if b {
        Value::Integer(1)
    } else {
        Value::Nil
    }
}

fn id_arg(v: &Value, usage: &str) -> mlua::Result<i64> {
    if !is_number(v) {
        return Err(mlua::Error::runtime(usage.to_string()));
    }
    Ok(to_int(v))
}

/// The slot of a faction the player has one for.
fn slot_of(lua: &Lua, id: i64) -> Option<FactionEntry> {
    let id = u32::try_from(id).ok().filter(|i| *i > 0)?;
    benilla_ui::script::ext_read::reputation(lua)?
        .entries
        .into_iter()
        .find(|e| e.faction_id == id)
}

/// The frame's standing and flag diff.
#[derive(Default)]
pub struct Watch {
    seen: Option<HashMap<u32, (i32, bool, bool, bool)>>,
    pub(crate) last: Option<(u32, i32, i32)>,
}

impl Watch {
    /// The events this frame's reputation snapshot fires.
    pub fn tick(&mut self, lua: &Lua) -> Vec<(&'static str, Vec<ScriptValue>)> {
        let rep = benilla_ui::script::ext_read::reputation(lua).unwrap_or_default();
        let now: HashMap<u32, (i32, bool, bool, bool)> = rep
            .entries
            .iter()
            .map(|e| (e.faction_id, (e.standing, e.at_war, e.inactive, e.visible)))
            .collect();
        let mut out = Vec::new();
        match &self.seen {
            None if !now.is_empty() => {
                out.push(("UNIT_FACTION", vec![ScriptValue::Str("player".into())]));
            }
            None => return out,
            Some(before) => {
                let mut flags_moved = false;
                for (id, cur) in &now {
                    let Some(old) = before.get(id) else {
                        continue;
                    };
                    if old.0 != cur.0 {
                        let delta = cur.0 - old.0;
                        self.last = Some((*id, cur.0, delta));
                        out.push((
                            "FACTION_STANDING_CHANGED",
                            vec![
                                ScriptValue::Number(f64::from(*id)),
                                ScriptValue::Number(f64::from(cur.0)),
                                ScriptValue::Number(f64::from(delta)),
                            ],
                        ));
                    }
                    flags_moved |= (old.1, old.2, old.3) != (cur.1, cur.2, cur.3);
                }
                if flags_moved {
                    out.push(("UNIT_FACTION", vec![ScriptValue::Str("player".into())]));
                }
            }
        }
        self.seen = Some(now);
        out
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_Reputation";

    // 0 for a header row, nil past the end.
    api.global("GetFactionIDByIndex", |lua, v: Value| {
        let i = id_arg(&v, "Usage: GetFactionIDByIndex(factionIndex)")?;
        let row = usize::try_from(i)
            .ok()
            .and_then(|i| benilla_ui::script::ext_read::reputation_row(lua, i));
        Ok(row.map(|r| match r {
            VisibleRow::Header { .. } => 0,
            VisibleRow::Entry(id) => id,
        }))
    })?;
    let c = api.ca.clone();
    api.global("GetFactionInfoByID", move |lua, v: Value| {
        let id = id_arg(&v, "Usage: GetFactionInfoByID(factionID)")?;
        let Some(d) = u32::try_from(id).ok().and_then(|id| read(lua, &c, id)) else {
            return Ok(MultiValue::new());
        };
        if d.name.is_empty() {
            return Ok(MultiValue::new());
        }
        (
            d.name,
            d.description,
            d.reaction,
            d.min,
            d.max,
            d.standing,
            flag(d.at_war),
            flag(d.can_toggle),
            flag(d.header),
            flag(d.collapsed),
            flag(d.watched),
        )
            .into_lua_multi(lua)
    })?;
    let c = api.ca.clone();
    api.global("GetFactionParentID", move |_, v: Value| {
        let id = id_arg(&v, "Usage: GetFactionParentID(factionID)")?;
        Ok(u32::try_from(id)
            .ok()
            .and_then(|id| Some(c.db.get("Faction")?.row(id)?.u32(PARENT))))
    })?;

    api.table(NS, "GetFactionStandings", |lua, ()| {
        let t = lua.create_table()?;
        for e in benilla_ui::script::ext_read::reputation(lua)
            .unwrap_or_default()
            .entries
        {
            t.set(e.faction_id, e.standing)?;
        }
        Ok(t)
    })?;
    api.table(NS, "GetWatchedFactionData", |lua, ()| {
        let rep = benilla_ui::script::ext_read::reputation(lua).unwrap_or_default();
        let Some(e) = rep
            .watched
            .and_then(|w| rep.entries.iter().find(|e| e.rep_list_id == w))
        else {
            return Ok(None);
        };
        let mut d = from_entry(lua, e, rep.watched);
        d.watched = true;
        table(lua, &d).map(Some)
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetFactionDataByID", move |lua, v: Value| {
        let id = id_arg(&v, "Usage: C_Reputation.GetFactionDataByID(factionID)")?;
        match u32::try_from(id).ok().and_then(|id| read(lua, &c, id)) {
            Some(d) => table(lua, &d).map(Some),
            None => Ok(None),
        }
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetFactionDataByIndex", move |lua, v: Value| {
        let i = id_arg(
            &v,
            "Usage: C_Reputation.GetFactionDataByIndex(factionSortIndex)",
        )?;
        let id = match usize::try_from(i)
            .ok()
            .and_then(|i| benilla_ui::script::ext_read::reputation_row(lua, i))
        {
            Some(VisibleRow::Entry(id)) | Some(VisibleRow::Header { faction_id: id, .. }) => id,
            None => return Ok(None),
        };
        match read(lua, &c, id) {
            Some(d) => table(lua, &d).map(Some),
            None => Ok(None),
        }
    })?;
    api.table(NS, "ToggleFactionAtWarByID", |lua, v: Value| {
        let id = id_arg(&v, "Usage: C_Reputation.ToggleFactionAtWarByID(factionID)")?;
        if let Some(e) = slot_of(lua, id) {
            benilla_ui::script::ext_read::toggle_faction_at_war(lua, e.rep_list_id);
        }
        Ok(())
    })?;
    api.table(NS, "IsFactionActive", |lua, v: Value| {
        let i = id_arg(&v, "Usage: C_Reputation.IsFactionActive(factionSortIndex)")?;
        let id = match usize::try_from(i)
            .ok()
            .and_then(|i| benilla_ui::script::ext_read::reputation_row(lua, i))
        {
            Some(VisibleRow::Entry(id)) => i64::from(id),
            _ => return Ok(false),
        };
        Ok(slot_of(lua, id).is_some_and(|e| !e.inactive))
    })?;
    api.table(NS, "IsFactionActiveByID", |lua, v: Value| {
        let id = id_arg(&v, "Usage: C_Reputation.IsFactionActiveByID(factionID)")?;
        Ok(slot_of(lua, id).is_some_and(|e| !e.inactive))
    })?;
    for (name, inactive) in [
        ("SetFactionInactiveByID", true),
        ("SetFactionActiveByID", false),
    ] {
        let usage = format!("Usage: C_Reputation.{name}(factionID)");
        api.table(NS, name, move |lua, v: Value| {
            let id = id_arg(&v, &usage)?;
            if let Some(e) = slot_of(lua, id) {
                benilla_ui::script::ext_read::set_faction_inactive(lua, e.rep_list_id, inactive);
            }
            Ok(())
        })?;
    }
    // 0 clears; a faction with no slot is nothing to select or watch.
    api.table(NS, "SetSelectedFactionByID", |lua, v: Value| {
        let id = id_arg(&v, "Usage: C_Reputation.SetSelectedFactionByID(factionID)")?;
        match id {
            0 => benilla_ui::script::ext_read::select_faction(lua, None),
            id if id > 0 => {
                if let Some(e) = slot_of(lua, id) {
                    benilla_ui::script::ext_read::select_faction(lua, Some(e.rep_list_id));
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    api.table(NS, "SetWatchedFactionByID", |lua, v: Value| {
        let id = id_arg(&v, "Usage: C_Reputation.SetWatchedFactionByID(factionID)")?;
        match id {
            0 => benilla_ui::script::ext_read::watch_faction(lua, None),
            id if id > 0 => {
                if let Some(e) = slot_of(lua, id) {
                    benilla_ui::script::ext_read::watch_faction(lua, Some(e.rep_list_id));
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    // The stock by-index verbs, mirrored into the namespace.
    let g = api.lua.globals();
    for name in [
        "IsFactionInactive",
        "SetFactionInactive",
        "SetFactionActive",
        "SetSelectedFaction",
    ] {
        if let Ok(f) = g.get::<mlua::Function>(name) {
            api.function(Some(NS), name, f)?;
        }
    }
    if let Ok(f) = g.get::<mlua::Function>("FactionToggleAtWar") {
        api.function(Some(NS), "ToggleFactionAtWar", f)?;
    }
    let c = api.ca.clone();
    api.table(NS, "GetLastStandingChange", move |lua, ()| {
        match c.lock().faction_watch.last {
            Some(last) => last.into_lua_multi(lua),
            None => Ok(MultiValue::new()),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_ui::script::ReputationState;

    fn entry(id: u32, slot: u32, standing: i32) -> FactionEntry {
        FactionEntry {
            faction_id: id,
            rep_list_id: slot,
            parent_id: 469,
            name: format!("F{id}"),
            description: String::new(),
            standing,
            standing_id: 5,
            bar_min: 3000,
            bar_max: 9000,
            visible: true,
            is_header: false,
            at_war: false,
            can_toggle_at_war: true,
            inactive: false,
        }
    }

    #[test]
    fn faction_data_reads_by_id_and_changes_fire() {
        let mut script = crate::lua::test_support::vm(&Ca::default());
        script.set_reputation(ReputationState {
            entries: vec![entry(72, 3, 4000), entry(47, 4, 5000)],
            watched: Some(4),
        });
        let mut watch = Watch::default();
        let first = watch.tick(script.lua());
        assert_eq!(first.len(), 1, "the first snapshot is UNIT_FACTION only");
        let out: String = script
            .lua()
            .load(
                r#"
                local d = C_Reputation.GetFactionDataByID(72)
                local w = C_Reputation.GetWatchedFactionData()
                C_Reputation.ToggleFactionAtWarByID(72)
                local s = C_Reputation.GetFactionStandings()
                return d.name .. d.currentStanding .. tostring(d.isWatched) .. " " .. w.factionID
                  .. tostring(w.isWatched) .. " " .. s[47] .. " " .. tostring(C_Reputation.IsFactionActiveByID(72))
                  .. tostring(C_Reputation.IsFactionActiveByID(999))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "F724000false 47true 5000 truefalse");
        // The toggle flipped the flag; a standing change fires its event.
        script.set_reputation(ReputationState {
            entries: vec![entry(72, 3, 4250), entry(47, 4, 5000)],
            watched: Some(4),
        });
        let ev = watch.tick(script.lua());
        assert_eq!(ev[0].0, "FACTION_STANDING_CHANGED");
        assert_eq!(watch.last, Some((72, 4250, 250)));
        assert!(script.take_reputation_sends().iter().any(|s| matches!(
            s,
            benilla_ui::script::ReputationSend::AtWar {
                rep_list_id: 3,
                at_war: true
            }
        )));
    }
}

//! `friendlist/`: `C_FriendList`, over benilla's social snapshot and the stock `/who` verbs.
//!
//! - `GetNumFriends`, `GetNumOnlineFriends`, `GetFriendInfo(name)`, `GetFriendInfoByIndex(i)`:
//!   the modern FriendInfo table (`name`, `connected`, `level`, `className`, `classFilename`,
//!   `area`, `guid`, `notes`, `afk`, `dnd`, and `mobile`, `referAFriend`, `rafLinkType` with 1.12's
//!   nothing); class and area are absent for an offline friend.
//! - `IsFriend(guid | name)`, `IsIgnored(name | guid)`, `IsIgnoredByGuid(guid)`.
//! - `SetFriendNotes(name, notes)`, `SetFriendNotesByIndex(i, notes)`: 1.12 keeps no notes, so,
//!   as in the DLL, a per-character file holds them (`0xGUID<tab>name<tab>note`, 128 characters,
//!   no tabs or newlines), here under `benilla-config/saved/`; a change fires `FRIENDLIST_UPDATE`.
//! - `GetNumWhoResults`, `GetWhoInfo(i)` (the modern table), `SendWhoQueryByName(name)` and
//!   `IsWhoQueryPending()`: the query is `n-<name>`, sent with the answer routed to the Who list
//!   (`SetWhoToUI(1)`), the previous routing restored once every query of ours has its answer; one
//!   query per 5 s, the server's cooldown; a query unanswered for 10 s is given up.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use benilla_ui::script::FriendInfo;
use mlua::{Function, Lua, Table, Value};

use crate::lua::{is_number, to_int, to_str, Api};
use crate::Ca;

const MAX_NOTE: usize = 128;
const WHO_COOLDOWN: Duration = Duration::from_secs(5);
const WHO_GIVE_UP: Duration = Duration::from_secs(10);

/// `C_FriendList`'s own state: the notes and the `/who` queries in flight.
#[derive(Default)]
pub struct Friends {
    notes_path: Option<PathBuf>,
    /// By friend guid: `(name, note)`.
    notes: BTreeMap<u64, (String, String)>,
    last_send: Option<Instant>,
    in_flight: u32,
    /// The routing value our first query replaced.
    saved_routing: Option<bool>,
    /// `/who` answers seen, applied the frame after (once their `WHO_LIST_UPDATE` has run).
    answers: u32,
}

impl Friends {
    fn load(&mut self, path: Option<PathBuf>) {
        if path == self.notes_path {
            return;
        }
        self.notes.clear();
        if let Some(text) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                let mut parts = line.splitn(3, '\t');
                let (Some(g), Some(name), Some(note)) = (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                let guid = u64::from_str_radix(g.trim_start_matches("0x"), 16).unwrap_or(0);
                if guid != 0 && !note.is_empty() {
                    let note: String = note.chars().take(MAX_NOTE).collect();
                    self.notes.insert(guid, (name.to_string(), note));
                }
            }
        }
        self.notes_path = path;
    }

    fn save(&self) {
        let Some(path) = &self.notes_path else {
            return;
        };
        let mut out = String::from(
            "# ClassicAPI Friend Notes v1\n# <0xGUID>\\t<name>\\t<note>  (per-character, GUID-keyed)\n",
        );
        for (guid, (name, note)) in &self.notes {
            out.push_str(&format!("0x{guid:016X}\t{name}\t{note}\n"));
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("txt.tmp");
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// A `/who` answer arrived (`SMSG_WHO`).
    pub fn on_who_answer(&mut self) {
        if self.in_flight > 0 {
            self.answers += 1;
        }
    }

    /// The frame's upkeep: answers seen last frame retire their queries, a stuck one is given up,
    /// and the routing goes back once none of ours is left. `Some` routing to restore.
    pub fn tick(&mut self, now: Instant) -> Option<bool> {
        let answered = std::mem::take(&mut self.answers);
        self.in_flight = self.in_flight.saturating_sub(answered);
        if self.in_flight > 0
            && self
                .last_send
                .is_some_and(|at| now.duration_since(at) >= WHO_GIVE_UP)
        {
            self.in_flight = 0;
        }
        if self.in_flight == 0 {
            return self.saved_routing.take();
        }
        None
    }
}

/// A note as stored: no tabs or newlines, trailing blanks trimmed, at most 128 characters.
fn clean_note(note: &str) -> String {
    let s: String = note
        .chars()
        .map(|c| {
            if matches!(c, '\t' | '\r' | '\n') {
                ' '
            } else {
                c
            }
        })
        .collect();
    s.trim_end().chars().take(MAX_NOTE).collect()
}

fn social(lua: &Lua) -> benilla_ui::script::SocialState {
    benilla_ui::script::ext_read::social(lua).unwrap_or_default()
}

fn notes_of(lua: &Lua, ca: &Ca, guid: u64) -> Option<String> {
    let path = super::equipmentset::character_file(lua, "ClassicAPI_FriendNotes.txt");
    let mut st = ca.lock();
    st.friends.load(path);
    st.friends.notes.get(&guid).map(|n| n.1.clone())
}

fn friend_table(lua: &Lua, ca: &Ca, f: &FriendInfo) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("name", f.name.as_str())?;
    t.set("connected", f.connected)?;
    t.set("level", f.level)?;
    if !f.class.is_empty() {
        t.set("className", f.class.as_str())?;
        t.set("classFilename", f.class_file.as_str())?;
    }
    if !f.area.is_empty() {
        t.set("area", f.area.as_str())?;
    }
    t.set("guid", crate::guid::format(f.guid))?;
    if let Some(note) = notes_of(lua, ca, f.guid) {
        t.set("notes", note)?;
    }
    let flag = |key: &str, fallback: &str| -> bool {
        let tag = benilla_ui::strings::global(lua, key).unwrap_or_else(|| fallback.into());
        !f.status.is_empty() && f.status == tag
    };
    t.set("afk", flag("CHAT_FLAG_AFK", "<AFK>"))?;
    t.set("dnd", flag("CHAT_FLAG_DND", "<DND>"))?;
    t.set("mobile", false)?;
    t.set("referAFriend", false)?;
    t.set("rafLinkType", 0)?;
    Ok(t)
}

/// A token as a guid when it parses as one, else a name.
fn guid_or_name(s: &str) -> (Option<u64>, &str) {
    let guid = s
        .starts_with("0x")
        .then(|| crate::guid::parse(s))
        .flatten()
        .filter(|g| *g != 0);
    (guid, s)
}

fn by_name<'a>(friends: &'a [FriendInfo], name: &str) -> Option<&'a FriendInfo> {
    friends.iter().find(|f| f.name.eq_ignore_ascii_case(name))
}

fn set_note(lua: &Lua, ca: &Ca, f: &FriendInfo, note: &str) {
    let path = super::equipmentset::character_file(lua, "ClassicAPI_FriendNotes.txt");
    let note = clean_note(note);
    let changed = {
        let mut st = ca.lock();
        st.friends.load(path);
        let before = st.friends.notes.get(&f.guid).map(|n| n.1.clone());
        if note.is_empty() {
            st.friends.notes.remove(&f.guid);
        } else {
            st.friends
                .notes
                .insert(f.guid, (f.name.clone(), note.clone()));
        }
        let after = st.friends.notes.get(&f.guid).map(|n| n.1.clone());
        if before != after {
            st.friends.save();
        }
        before != after
    };
    if changed {
        benilla_ui::script::ext_read::fire_event(lua, "FRIENDLIST_UPDATE", vec![]);
    }
}

/// A class's token from its localized name, through `ChrClasses.dbc`.
fn class_token(ca: &Ca, localized: &str) -> Option<String> {
    let t = ca.db.get("ChrClasses")?;
    let found = t
        .rows()
        .find(|r| r.loc(0x14 / 4).eq_ignore_ascii_case(localized))
        .map(|r| r.str(0x38 / 4).to_string());
    found
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_FriendList";
    api.table(NS, "GetNumFriends", |lua, ()| Ok(social(lua).friends.len()))?;
    api.table(NS, "GetNumOnlineFriends", |lua, ()| {
        Ok(social(lua).friends.iter().filter(|f| f.connected).count())
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetFriendInfoByIndex", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_FriendList.GetFriendInfoByIndex(index)",
            ));
        }
        let friends = social(lua).friends;
        match usize::try_from(to_int(&v) - 1)
            .ok()
            .and_then(|i| friends.get(i))
        {
            Some(f) => friend_table(lua, &c, f).map(Some),
            None => Ok(None),
        }
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetFriendInfo", move |lua, v: Value| {
        let Some(name) = to_str(&v) else {
            return Err(mlua::Error::runtime(
                "Usage: C_FriendList.GetFriendInfo(name)",
            ));
        };
        let friends = social(lua).friends;
        match by_name(&friends, &name) {
            Some(f) => friend_table(lua, &c, f).map(Some),
            None => Ok(None),
        }
    })?;
    api.table(NS, "IsFriend", |lua, v: Value| {
        let Some(s) = to_str(&v) else {
            return Ok(false);
        };
        let friends = social(lua).friends;
        Ok(match guid_or_name(&s) {
            (Some(g), _) => friends.iter().any(|f| f.guid == g),
            (None, name) => by_name(&friends, name).is_some(),
        })
    })?;
    api.table(NS, "IsIgnored", |lua, v: Value| {
        let Some(s) = to_str(&v) else {
            return Ok(false);
        };
        let soc = social(lua);
        Ok(match guid_or_name(&s) {
            (Some(g), _) => soc.ignore_guids.contains(&g),
            (None, name) => soc.ignores.iter().any(|n| n.eq_ignore_ascii_case(name)),
        })
    })?;
    api.table(NS, "IsIgnoredByGuid", |lua, v: Value| {
        let guid = to_str(&v).and_then(|s| crate::guid::parse(&s));
        Ok(guid.is_some_and(|g| g != 0 && social(lua).ignore_guids.contains(&g)))
    })?;
    let c = api.ca.clone();
    api.table(
        NS,
        "SetFriendNotes",
        move |lua, (name, note): (Value, Value)| {
            let Some(name) = to_str(&name) else {
                return Err(mlua::Error::runtime(
                    "Usage: C_FriendList.SetFriendNotes(name, notes)",
                ));
            };
            let friends = social(lua).friends;
            if let Some(f) = by_name(&friends, &name) {
                set_note(lua, &c, f, &to_str(&note).unwrap_or_default());
            }
            Ok(())
        },
    )?;
    let c = api.ca.clone();
    api.table(
        NS,
        "SetFriendNotesByIndex",
        move |lua, (i, note): (Value, Value)| {
            if !is_number(&i) {
                return Err(mlua::Error::runtime(
                    "Usage: C_FriendList.SetFriendNotesByIndex(index, notes)",
                ));
            }
            let friends = social(lua).friends;
            if let Some(f) = usize::try_from(to_int(&i) - 1)
                .ok()
                .and_then(|i| friends.get(i))
            {
                set_note(lua, &c, f, &to_str(&note).unwrap_or_default());
            }
            Ok(())
        },
    )?;

    api.table(NS, "GetNumWhoResults", |lua, ()| {
        let s = social(lua);
        Ok((s.who.len(), s.who_total))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetWhoInfo", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_FriendList.GetWhoInfo(index)",
            ));
        }
        let who = social(lua).who;
        let Some(w) = usize::try_from(to_int(&v) - 1)
            .ok()
            .and_then(|i| who.get(i))
        else {
            return Ok(None);
        };
        let t = lua.create_table()?;
        t.set("fullName", w.name.as_str())?;
        t.set("fullGuildName", w.guild.as_str())?;
        t.set("level", w.level)?;
        t.set("raceStr", w.race.as_str())?;
        t.set("classStr", w.class.as_str())?;
        if let Some(token) = class_token(&c, &w.class) {
            t.set("filename", token)?;
        }
        t.set("area", w.zone.as_str())?;
        Ok(Some(t))
    })?;
    let c = api.ca.clone();
    let send_who: Option<Function> = api.lua.globals().get("SendWho").ok();
    let set_routing: Option<Function> = api.lua.globals().get("SetWhoToUI").ok();
    api.table(NS, "SendWhoQueryByName", move |lua, v: Value| {
        let Some(name) = to_str(&v) else {
            return Err(mlua::Error::runtime(
                "Usage: C_FriendList.SendWhoQueryByName(name)",
            ));
        };
        let (Some(send), Some(route)) = (&send_who, &set_routing) else {
            return Ok(false);
        };
        let now = Instant::now();
        {
            let mut st = c.lock();
            let f = &mut st.friends;
            if name.is_empty()
                || f.last_send
                    .is_some_and(|at| now.duration_since(at) < WHO_COOLDOWN)
            {
                return Ok(false);
            }
            if f.saved_routing.is_none() {
                f.saved_routing = Some(benilla_ui::script::ext_read::who_to_ui(lua));
            }
            f.last_send = Some(now);
            f.in_flight += 1;
        }
        route.call::<()>(1)?;
        send.call::<()>(format!("n-{name}"))?;
        Ok(true)
    })?;
    let c = api.ca.clone();
    api.table(NS, "IsWhoQueryPending", move |_, ()| {
        Ok(c.lock().friends.in_flight > 0)
    })?;
    Ok(())
}

/// Put the routing back once our queries are answered.
pub(crate) fn tick(lua: &Lua, ca: &Ca, now: Instant) {
    let restore = ca.lock().friends.tick(now);
    if let Some(on) = restore {
        if let Ok(f) = lua.globals().get::<Function>("SetWhoToUI") {
            let _ = f.call::<()>(i32::from(on));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_ui::script::{SocialState, WhoInfo};

    #[test]
    fn friends_read_by_index_name_and_guid_and_who_routes() {
        let mut script = crate::lua::test_support::vm(&Ca::default());
        script.set_social(SocialState {
            friends: vec![
                FriendInfo {
                    name: "Ann".into(),
                    level: 60,
                    class: "Mage".into(),
                    area: "Ironforge".into(),
                    connected: true,
                    status: "<AFK>".into(),
                    guid: 0x11,
                    class_file: "MAGE".into(),
                },
                FriendInfo {
                    name: "Bob".into(),
                    guid: 0x12,
                    ..Default::default()
                },
            ],
            ignores: vec!["Eve".into()],
            ignore_guids: vec![0x99],
            who: vec![WhoInfo {
                name: "Cid".into(),
                level: 30,
                ..Default::default()
            }],
            who_total: 3,
            ..Default::default()
        });
        let out: String = script
            .lua()
            .load(
                r#"
                local F = C_FriendList
                local a = F.GetFriendInfoByIndex(1)
                local b = F.GetFriendInfo("bob")
                local w = F.GetWhoInfo(1)
                local sent = F.SendWhoQueryByName("Cid")
                local again = F.SendWhoQueryByName("Cid")
                return a.name .. a.classFilename .. tostring(a.afk) .. a.guid .. " "
                  .. tostring(b.connected) .. tostring(b.className) .. " "
                  .. F.GetNumFriends() .. F.GetNumOnlineFriends() .. " "
                  .. tostring(F.IsFriend("0x0000000000000012")) .. tostring(F.IsFriend("Zed"))
                  .. tostring(F.IsIgnored("eve")) .. tostring(F.IsIgnoredByGuid("0x0000000000000099"))
                  .. " " .. w.fullName .. w.level .. " " .. tostring(sent) .. tostring(again)
                  .. tostring(F.IsWhoQueryPending())
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            "AnnMAGEtrue0x0000000000000011 falsenil 21 truefalsetruetrue Cid30 truefalsetrue"
        );
        assert_eq!(clean_note("a\tb\n  "), "a b");
    }
}

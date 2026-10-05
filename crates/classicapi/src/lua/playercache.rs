//! `player/NameCache.cpp`: the persistent name cache behind `C_PlayerCache`, and the fallback
//! `GetPlayerInfoByGUID` and `UnitNameFromGUID` read when the live caches miss.
//!
//! - Opt-in (`SetEnabled`), as is the visible-player sweep (`SetScanEnabled`); both persist in
//!   `benilla-config/saved/ClassicAPI.txt`, the DLL's account-level settings file.
//! - The cache is name-keyed, one entry per name: a new guid claiming a known name means the old
//!   character was deleted and the name reused, so the old guid is dropped. It is filled from the
//!   client's player-name cache (every name query answer) every two seconds, from the visible
//!   players every ten with the sweep on, and from `RememberPlayer`; a class, race or sex of 0
//!   leaves the stored one alone.
//! - The file is `benilla-config/saved/<Realm>/ClassicAPI_NameCache.txt`, `name guid class race
//!   sex` tab-separated, written at most every 30 seconds while dirty. Deviation: the DLL keys
//!   the folder by the realmlist address as well, which benilla does not hand a crate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use benilla_ui::script::UiScript;
use mlua::{IntoLuaMulti, Lua, MultiValue, Value};

use crate::lua::{as_string, is_number, to_number, to_str, truthy, Api};
use crate::mirror::{typemask, Mirror};
use crate::Ca;

const INGEST_EVERY: Duration = Duration::from_secs(2);
const SCAN_EVERY: Duration = Duration::from_secs(10);
const SAVE_EVERY: Duration = Duration::from_secs(30);
/// `ChrClasses.dbc` and `ChrRaces.dbc`'s token columns.
const CLASS_TOKEN: usize = 0x38 / 4;
const RACE_TOKEN: usize = 0x3C / 4;

/// One cached player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    pub guid: u64,
    pub class: u32,
    pub race: u32,
    pub sex: u32,
}

/// The cache and its two switches.
#[derive(Default)]
pub(crate) struct PlayerCache {
    settings_loaded: bool,
    enabled: bool,
    scan: bool,
    /// The realm the entries were loaded for.
    realm: Option<String>,
    by_name: HashMap<String, Entry>,
    by_guid: HashMap<u64, String>,
    dirty: bool,
    last_save: Option<Instant>,
    last_ingest: Option<Instant>,
    last_scan: Option<Instant>,
}

impl PlayerCache {
    /// `Remember`: insert or update by name, dropping a stale guid for the name and a stale name
    /// for the guid; a zero class, race or sex keeps the stored one.
    pub(crate) fn remember(&mut self, name: &str, e: Entry) {
        if name.is_empty() || e.guid == 0 {
            return;
        }
        if let Some(old) = self.by_guid.get(&e.guid).cloned() {
            if old != name {
                self.by_name.remove(&old);
            }
        }
        let prev = self.by_name.get(name).copied();
        if let Some(p) = prev {
            if p.guid != e.guid {
                self.by_guid.remove(&p.guid);
            }
        }
        let p = prev.filter(|p| p.guid == e.guid).unwrap_or_default();
        let merged = Entry {
            guid: e.guid,
            class: if e.class != 0 { e.class } else { p.class },
            race: if e.race != 0 { e.race } else { p.race },
            sex: if e.sex != 0 { e.sex } else { p.sex },
        };
        if prev != Some(merged) {
            self.by_name.insert(name.to_string(), merged);
            self.by_guid.insert(e.guid, name.to_string());
            self.dirty = true;
        }
    }

    /// The name and entry a guid has, when the cache is on.
    pub(crate) fn by_guid(&self, guid: u64) -> Option<(String, Entry)> {
        if !self.enabled {
            return None;
        }
        let name = self.by_guid.get(&guid)?;
        Some((name.clone(), *self.by_name.get(name)?))
    }

    fn parse(&mut self, text: &str) {
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            let [name, guid, class, race, sex] = f[..] else {
                continue;
            };
            let Some(guid) = crate::guid::parse(guid) else {
                continue;
            };
            let n = |s: &str| s.parse::<u32>().unwrap_or(0);
            self.remember(
                name,
                Entry {
                    guid,
                    class: n(class),
                    race: n(race),
                    sex: n(sex),
                },
            );
        }
    }

    fn serialize(&self) -> String {
        let mut names: Vec<&String> = self.by_name.keys().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let e = self.by_name[n];
                format!(
                    "{n}\t{}\t{}\t{}\t{}\n",
                    crate::guid::format(e.guid),
                    e.class,
                    e.race,
                    e.sex
                )
            })
            .collect()
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(
        benilla_app::ext::local_state_dir()?
            .join("saved")
            .join("ClassicAPI.txt"),
    )
}

fn cache_path(realm: &str) -> Option<PathBuf> {
    let token: String = realm
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(
        benilla_app::ext::local_state_dir()?
            .join("saved")
            .join(token)
            .join("ClassicAPI_NameCache.txt"),
    )
}

fn realm_name(lua: &Lua) -> Option<String> {
    let f: mlua::Function = lua.globals().get("GetRealmName").ok()?;
    f.call::<String>(()).ok().filter(|r| !r.is_empty())
}

/// `Settings::Account::EnsureLoaded`.
fn ensure_settings(pc: &mut PlayerCache) {
    if pc.settings_loaded {
        return;
    }
    pc.settings_loaded = true;
    let Some(text) = settings_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return;
    };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let on = v.trim() == "1" || v.trim() == "true";
        match k.trim() {
            "PersistentNameCacheEnabled" => pc.enabled = on,
            "NameCacheScanEnabled" => pc.scan = on,
            _ => {}
        }
    }
}

/// Write the switches, keeping the file's other lines.
fn save_settings(pc: &PlayerCache) {
    let Some(path) = settings_path() else {
        return;
    };
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            let k = l.split('=').next().unwrap_or("").trim();
            k != "PersistentNameCacheEnabled" && k != "NameCacheScanEnabled"
        })
        .map(str::to_string)
        .collect();
    lines.push(format!(
        "PersistentNameCacheEnabled={}",
        u8::from(pc.enabled)
    ));
    lines.push(format!("NameCacheScanEnabled={}", u8::from(pc.scan)));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

/// `LoadCacheIfNeeded`: the realm's file once the realm is known.
fn ensure_loaded(pc: &mut PlayerCache, realm: Option<String>) {
    let Some(realm) = realm else { return };
    if pc.realm.as_deref() == Some(realm.as_str()) {
        return;
    }
    pc.by_name.clear();
    pc.by_guid.clear();
    if let Some(text) = cache_path(&realm).and_then(|p| std::fs::read_to_string(p).ok()) {
        pc.parse(&text);
    }
    pc.dirty = false;
    pc.realm = Some(realm);
}

fn save(pc: &mut PlayerCache) {
    let Some(path) = pc.realm.as_deref().and_then(cache_path) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&path, pc.serialize()).is_ok() {
        pc.dirty = false;
    }
}

/// Every player the mirror names, with what it knows of them.
fn ingest(pc: &mut PlayerCache, m: &Mirror, scan: bool) {
    for (guid, (name, traits)) in &m.player_names {
        let (race, class, sex) = traits.unwrap_or_default();
        pc.remember(
            name,
            Entry {
                guid: *guid,
                class: u32::from(class),
                race: u32::from(race),
                sex: u32::from(sex),
            },
        );
    }
    if scan {
        for (guid, f) in &m.objects {
            if !f.is(typemask::PLAYER) || f.class() == 0 {
                continue;
            }
            let Some(name) = m.names.get(guid) else {
                continue;
            };
            pc.remember(
                name,
                Entry {
                    guid: *guid,
                    class: u32::from(f.class()),
                    race: u32::from(f.race()),
                    sex: u32::from(f.gender()),
                },
            );
        }
    }
}

/// The frame's turn: load, fill and save while the cache is on.
pub(crate) fn tick(ca: &Ca, script: &mut UiScript, now: Instant) {
    let realm = {
        let st = ca.lock();
        let pc = &st.playercache;
        (pc.enabled && pc.realm.is_none()).then_some(())
    }
    .and_then(|()| realm_name(script.lua()));
    let mut st = ca.lock();
    let crate::State {
        playercache: pc,
        mirror,
        ..
    } = &mut *st;
    if !pc.enabled {
        return;
    }
    ensure_loaded(pc, realm);
    if pc.realm.is_none() {
        return;
    }
    if pc
        .last_ingest
        .is_none_or(|t| now.duration_since(t) >= INGEST_EVERY)
    {
        pc.last_ingest = Some(now);
        let scan = pc.scan
            && pc
                .last_scan
                .is_none_or(|t| now.duration_since(t) >= SCAN_EVERY);
        if scan {
            pc.last_scan = Some(now);
        }
        ingest(pc, mirror, scan);
    }
    if pc.dirty
        && pc
            .last_save
            .is_none_or(|t| now.duration_since(t) >= SAVE_EVERY)
    {
        pc.last_save = Some(now);
        save(pc);
    }
}

fn token_id(ca: &Ca, table: &'static str, col: usize, token: &str) -> u32 {
    ca.db
        .get(table)
        .and_then(|t| {
            t.rows()
                .find(|r| r.str(col).eq_ignore_ascii_case(token))
                .map(|r| r.id())
        })
        .unwrap_or(0)
}

fn token_of(ca: &Ca, table: &'static str, col: usize, id: u32) -> String {
    ca.db
        .get(table)
        .and_then(|t| t.row(id).map(|r| r.str(col).to_string()))
        .unwrap_or_default()
}

fn flag(v: &Value) -> bool {
    if is_number(v) {
        to_number(v) != 0.0
    } else {
        truthy(v)
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_PlayerCache";
    let c = api.ca.clone();
    api.table(NS, "IsEnabled", move |_, ()| {
        let mut st = c.lock();
        ensure_settings(&mut st.playercache);
        Ok(st.playercache.enabled)
    })?;
    let c = api.ca.clone();
    api.table(NS, "SetEnabled", move |_, v: Value| {
        let mut st = c.lock();
        let pc = &mut st.playercache;
        ensure_settings(pc);
        let on = flag(&v);
        if pc.enabled != on {
            pc.enabled = on;
            save_settings(pc);
            if !on && pc.dirty {
                save(pc);
            }
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table(NS, "IsScanEnabled", move |_, ()| {
        let mut st = c.lock();
        ensure_settings(&mut st.playercache);
        Ok(st.playercache.scan)
    })?;
    let c = api.ca.clone();
    api.table(NS, "SetScanEnabled", move |_, v: Value| {
        let mut st = c.lock();
        let pc = &mut st.playercache;
        ensure_settings(pc);
        let on = flag(&v);
        if pc.scan != on {
            pc.scan = on;
            save_settings(pc);
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table(
        NS,
        "RememberPlayer",
        move |_, (g, n, class, race, sex): (Value, Value, Value, Value, Value)| {
            {
                let mut st = c.lock();
                ensure_settings(&mut st.playercache);
                if !st.playercache.enabled {
                    return Ok(false);
                }
            }
            let (Some(g), Some(name), Some(class)) = (to_str(&g), to_str(&n), to_str(&class))
            else {
                return Err(mlua::Error::runtime(
                    "Usage: C_PlayerCache.RememberPlayer(guid, name, classToken [, raceToken [, sex]])",
                ));
            };
            let Some(guid) = crate::guid::parse(&g).filter(|g| *g != 0) else {
                return Ok(false);
            };
            if name.is_empty() {
                return Ok(false);
            }
            let race = as_string(&race).map_or(0, |r| token_id(&c, "ChrRaces", RACE_TOKEN, &r));
            let sex = if is_number(&sex) && (0.0..=1.0).contains(&to_number(&sex)) {
                to_number(&sex) as u32
            } else {
                0
            };
            let class = token_id(&c, "ChrClasses", CLASS_TOKEN, &class);
            c.lock().playercache.remember(
                &name,
                Entry {
                    guid,
                    class,
                    race,
                    sex,
                },
            );
            Ok(true)
        },
    )?;
    let c = api.ca.clone();
    api.table(NS, "GetPlayerInfoByName", move |lua, v: Value| {
        let Some(name) = as_string(&v) else {
            return Ok(MultiValue::new());
        };
        let e = {
            let st = c.lock();
            let pc = &st.playercache;
            pc.enabled.then(|| pc.by_name.get(&name).copied()).flatten()
        };
        let Some(e) = e else {
            return Ok(MultiValue::new());
        };
        let class_name =
            c.db.get("ChrClasses")
                .and_then(|t| t.row(e.class).map(|r| r.loc(0x14 / 4).to_string()))
                .unwrap_or_default();
        let race_name =
            c.db.get("ChrRaces")
                .and_then(|t| t.row(e.race).map(|r| r.loc(0x44 / 4).to_string()))
                .unwrap_or_default();
        (
            class_name,
            token_of(&c, "ChrClasses", CLASS_TOKEN, e.class),
            race_name,
            token_of(&c, "ChrRaces", RACE_TOKEN, e.race),
            e.sex + 2,
            name,
            "",
            crate::guid::format(e.guid),
        )
            .into_lua_multi(lua)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_keeps_one_guid_and_zero_fields_keep_the_stored_ones() {
        let mut pc = PlayerCache {
            enabled: true,
            ..Default::default()
        };
        let e = |guid, class, race, sex| Entry {
            guid,
            class,
            race,
            sex,
        };
        pc.remember("Aeth", e(1, 8, 1, 1));
        pc.remember("Aeth", e(1, 0, 0, 0));
        assert_eq!(pc.by_guid(1), Some(("Aeth".into(), e(1, 8, 1, 1))));
        // The name reused by a new character: the old guid goes.
        pc.remember("Aeth", e(2, 1, 2, 0));
        assert_eq!(pc.by_guid(1), None);
        assert_eq!(pc.by_guid(2).map(|p| p.1.class), Some(1));
        // A renamed guid drops its old name.
        pc.remember("Other", e(2, 0, 0, 0));
        assert!(!pc.by_name.contains_key("Aeth"));
        let text = pc.serialize();
        let mut back = PlayerCache::default();
        back.parse(&text);
        assert_eq!(back.by_name, pc.by_name);
    }
}

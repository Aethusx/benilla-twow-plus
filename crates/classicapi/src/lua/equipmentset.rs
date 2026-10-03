//! `equipmentset/`: `C_EquipmentSet`, gear sets saved per character and worn in one call.
//!
//! A set is the item guid each paperdoll slot held when it was saved (0 empty, 1 ignored). The
//! DLL keeps them in a per-character `ClassicAPI_EquipmentSets.txt`; benilla writes nothing into
//! the install, so the file lives in the character's local state folder,
//! `benilla-config/saved/<Realm>-<Character>/`, in the DLL's format. Deviation: sets as action-bar
//! buttons (the 3.3.5 action type `0x20000000 | setID`) are not built; benilla's action bar knows
//! no such action.

use std::path::PathBuf;

use mlua::{IntoLuaMulti, Lua, Value};

use super::item::actions::Verbs;
use super::item::inventory::{
    pack_bag, pack_bank_bag, pack_equipped, pack_main_bank, LOC_BAGS, LOC_BANK,
};
use crate::items;
use crate::lua::{as_string, is_number, none, to_int, to_number, Api};
use crate::mirror::Mirror;
use crate::Ca;

pub(crate) const SLOT_COUNT: usize = 19;
const GUID_EMPTY: u64 = 0;
const GUID_IGNORED: u64 = 1;
const MAX_NAME_LEN: usize = 31;
const DEFAULT_ICON: &str = "INV_Misc_QuestionMark";
/// `GetItemLocations`' sentinels: missing, ignored.
const LOC_MISSING: i64 = -1;
const LOC_IGNORED: i64 = 1;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Set {
    pub id: u32,
    pub name: String,
    pub icon: String,
    pub items: [u64; SLOT_COUNT],
    pub item_ids: [u32; SLOT_COUNT],
    pub action_slots: Vec<i64>,
}

/// The loaded sets, the file they came from and the slots ignored for the next save.
#[derive(Default)]
pub(crate) struct Store {
    path: Option<PathBuf>,
    sets: Vec<Set>,
    ignored: [bool; SLOT_COUNT],
}

/// benilla's `file_token`: the realm and character in a file name.
fn file_token(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    if t.is_empty() {
        "unknown".into()
    } else {
        t
    }
}

/// `Storage::ResolveFilePath`: none before the realm and character are known.
fn file_path(lua: &Lua) -> Option<PathBuf> {
    let g = lua.globals();
    let realm: String = g
        .get::<mlua::Function>("GetRealmName")
        .ok()?
        .call(())
        .ok()?;
    let name: String = g
        .get::<mlua::Function>("UnitName")
        .ok()?
        .call("player")
        .ok()?;
    if realm.is_empty() || name.is_empty() {
        return None;
    }
    let key = format!("{}-{}", file_token(&realm), file_token(&name));
    Some(
        benilla_app::ext::local_state_dir()?
            .join("saved")
            .join(key)
            .join("ClassicAPI_EquipmentSets.txt"),
    )
}

/// `Storage::Load`, the DLL's text format.
pub(crate) fn parse(text: &str) -> Vec<Set> {
    let mut out: Vec<Set> = Vec::new();
    let field_value = |line: &str, key: &str| -> Option<String> {
        let (k, v) = line.split_once('=')?;
        (k.trim_end() == key).then(|| v.trim_start().to_string())
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("set ") {
            out.push(Set {
                id: crate::lua::atoi(rest).max(0) as u32,
                icon: DEFAULT_ICON.to_string(),
                ..Set::default()
            });
            continue;
        }
        let Some(cur) = out.last_mut() else {
            continue;
        };
        if let Some(mut v) = field_value(line, "name") {
            v.truncate(MAX_NAME_LEN);
            cur.name = v;
        } else if let Some(v) = field_value(line, "icon") {
            cur.icon = v;
        } else if let Some(rest) = line.strip_prefix("action ") {
            let slot = crate::lua::atoi(rest);
            if (1..=120).contains(&slot) {
                cur.action_slots.push(slot - 1);
            }
        } else if let Some(rest) = line.strip_prefix("slot ") {
            let slot = crate::lua::atoi(rest);
            if !(1..=SLOT_COUNT as i64).contains(&slot) {
                continue;
            }
            let i = slot as usize - 1;
            let after = rest
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start();
            if after.starts_with("ignored") {
                cur.items[i] = GUID_IGNORED;
                continue;
            }
            if let Some(p) = after.find("guid=") {
                let v = &after[p + 5..];
                let v = v.split_whitespace().next().unwrap_or("");
                cur.items[i] = v
                    .strip_prefix("0x")
                    .or_else(|| v.strip_prefix("0X"))
                    .map_or_else(|| v.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
                    .unwrap_or(0);
            }
            if let Some(p) = after.find("item=") {
                cur.item_ids[i] = crate::lua::atoi(&after[p + 5..]).max(0) as u32;
            }
        }
    }
    out
}

/// `Storage::Save`.
pub(crate) fn render(sets: &[Set]) -> String {
    let mut out = String::from(
        "# ClassicAPI Equipment Sets v1\n# Written by ClassicAPI. Hand-edits survive but lose comments.\n",
    );
    for s in sets {
        out += &format!("set {}\n  name={}\n  icon={}\n", s.id, s.name, s.icon);
        for a in &s.action_slots {
            out += &format!("  action {}\n", a + 1);
        }
        for (i, g) in s.items.iter().enumerate() {
            match *g {
                GUID_EMPTY => {}
                GUID_IGNORED => out += &format!("  slot {} ignored\n", i + 1),
                g => {
                    out += &format!("  slot {} guid=0x{g:016X}", i + 1);
                    if s.item_ids[i] != 0 {
                        out += &format!(" item={}", s.item_ids[i]);
                    }
                    out += "\n";
                }
            }
        }
    }
    out
}

impl Store {
    /// `EnsureLoaded`: (re)load when the character's file changes; ignored slots reset.
    fn ensure(&mut self, path: Option<PathBuf>) {
        let Some(path) = path else {
            return;
        };
        if self.path.as_ref() == Some(&path) {
            return;
        }
        self.sets = std::fs::read_to_string(&path)
            .map(|t| parse(&t))
            .unwrap_or_default();
        self.ignored = [false; SLOT_COUNT];
        self.path = Some(path);
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("txt.tmp");
        if std::fs::write(&tmp, render(&self.sets)).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    fn by_id(&self, id: u32) -> Option<&Set> {
        self.sets.iter().find(|s| s.id == id)
    }

    fn by_name(&self, name: &str) -> Option<&Set> {
        self.sets.iter().find(|s| s.name == name)
    }

    /// `PopulateFromEquipped`: the worn guids and ids, ignored slots marked.
    fn populate(&self, set: &mut Set, m: &Mirror) {
        for i in 0..SLOT_COUNT {
            if self.ignored[i] {
                set.items[i] = GUID_IGNORED;
                set.item_ids[i] = 0;
                continue;
            }
            set.items[i] = items::linear_guid(m, i);
            set.item_ids[i] = items::item_id(m, set.items[i]);
        }
    }
}

/// `Locations::FindGUID`: where a set's item is now, packed; 0 when it is not owned.
fn find_guid(m: &Mirror, guid: u64) -> i64 {
    if guid == GUID_EMPTY || guid == GUID_IGNORED {
        return 0;
    }
    if let Some(s) =
        (1..=SLOT_COUNT as i64).find(|s| items::linear_guid(m, *s as usize - 1) == guid)
    {
        return pack_equipped(s);
    }
    let in_bag = |bag: i64| {
        (1..=items::bag_slot_count(m, bag) as i64)
            .find(|s| items::bag_slot(m, bag, *s) == Some(guid))
    };
    if let Some(s) = in_bag(0) {
        return pack_bag(0, s);
    }
    if let Some(s) = in_bag(items::BANK_CONTAINER) {
        return pack_main_bank(s);
    }
    for bag in 1..=4 {
        if let Some(s) = in_bag(bag) {
            return pack_bag(bag, s);
        }
    }
    for bag in 5..=10 {
        if let Some(s) = in_bag(bag) {
            return pack_bank_bag(bag, s);
        }
    }
    0
}

/// The names of every set holding the item (`Data::SetsContainingItem`).
pub(crate) fn sets_containing(ca: &Ca, guid: u64) -> Vec<String> {
    if guid == GUID_EMPTY || guid == GUID_IGNORED {
        return Vec::new();
    }
    ca.lock()
        .equipment_sets
        .sets
        .iter()
        .filter(|s| s.items.contains(&guid))
        .map(|s| s.name.clone())
        .collect()
}

/// `ArgSetID`: a number at least 1, else 0.
fn set_id(v: &Value) -> u32 {
    if !is_number(v) {
        return 0;
    }
    let d = to_number(v);
    if d < 1.0 {
        0
    } else {
        d as u32
    }
}

/// Load the character's sets, then run `f` over the store and the mirror.
fn with<R>(lua: &Lua, ca: &Ca, f: impl FnOnce(&mut Store, &Mirror) -> R) -> R {
    let path = file_path(lua);
    let mut st = ca.lock();
    let crate::State {
        equipment_sets,
        mirror,
        ..
    } = &mut *st;
    equipment_sets.ensure(path);
    f(equipment_sets, mirror)
}

/// `EQUIPMENT_SETS_CHANGED`, at once as the engine fires its own.
fn changed(lua: &Lua) {
    benilla_ui::script::ext_read::fire_event(lua, "EQUIPMENT_SETS_CHANGED", Vec::new());
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.table("C_EquipmentSet", "CanUseEquipmentSets", |_, ()| Ok(true))?;

    let c = api.ca.clone();
    api.table("C_EquipmentSet", "GetNumEquipmentSets", move |lua, ()| {
        Ok(with(lua, &c, |s, _| s.sets.len()))
    })?;
    let c = api.ca.clone();
    api.table("C_EquipmentSet", "GetEquipmentSetIDs", move |lua, ()| {
        let ids: Vec<u32> = with(lua, &c, |s, _| s.sets.iter().map(|s| s.id).collect());
        lua.create_sequence_from(ids)
    })?;
    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "GetEquipmentSetID",
        move |lua, v: Value| {
            let Some(name) = as_string(&v) else {
                return Ok(none());
            };
            match with(lua, &c, |s, _| s.by_name(&name).map(|s| s.id)) {
                Some(id) => id.into_lua_multi(lua),
                None => Ok(none()),
            }
        },
    )?;

    // `GetEquipmentSetInfo(setID)`: name, icon, id, isEquipped, and the item, worn, carried,
    // missing and ignored counts.
    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "GetEquipmentSetInfo",
        move |lua, v: Value| {
            let id = set_id(&v);
            let info = with(lua, &c, |s, m| {
                let set = s.by_id(id)?.clone();
                let (mut n, mut worn, mut carried, mut missing, mut ignored) = (0, 0, 0, 0, 0);
                for (i, g) in set.items.iter().enumerate() {
                    match *g {
                        GUID_IGNORED => ignored += 1,
                        GUID_EMPTY => {}
                        g => {
                            n += 1;
                            let loc = find_guid(m, g);
                            if loc == 0 {
                                missing += 1;
                            } else if loc & (LOC_BAGS | LOC_BANK) == 0 && loc & 0xFF == i as i64 + 1
                            {
                                worn += 1;
                            } else {
                                carried += 1;
                            }
                        }
                    }
                }
                Some((set, n, worn, carried, missing, ignored))
            });
            let Some((set, n, worn, carried, missing, ignored)) = info else {
                return Ok(none());
            };
            let equipped = n > 0 && worn + missing == n && carried == 0;
            (
                set.name, set.icon, set.id, equipped, n, worn, carried, missing, ignored,
            )
                .into_lua_multi(lua)
        },
    )?;

    let c = api.ca.clone();
    api.table("C_EquipmentSet", "GetIgnoredSlots", move |lua, v: Value| {
        let id = set_id(&v);
        let slots = with(lua, &c, |s, _| {
            s.by_id(id).map(|set| {
                (0..SLOT_COUNT)
                    .filter(|i| set.items[*i] == GUID_IGNORED)
                    .map(|i| i + 1)
                    .collect::<Vec<_>>()
            })
        });
        match slots {
            Some(v) => lua.create_sequence_from(v)?.into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    // `GetItemIDs(setID)`: slot -> the owned item's id.
    let c = api.ca.clone();
    api.table("C_EquipmentSet", "GetItemIDs", move |lua, v: Value| {
        let id = set_id(&v);
        let ids = with(lua, &c, |s, m| {
            s.by_id(id).map(|set| {
                (0..SLOT_COUNT)
                    .filter(|i| !matches!(set.items[*i], GUID_EMPTY | GUID_IGNORED))
                    .map(|i| (i + 1, items::item_id(m, set.items[i])))
                    .filter(|(_, id)| *id != 0)
                    .collect::<Vec<_>>()
            })
        });
        let Some(ids) = ids else {
            return Ok(none());
        };
        let t = lua.create_table()?;
        for (slot, id) in ids {
            t.set(slot, id)?;
        }
        t.into_lua_multi(lua)
    })?;

    // `GetItemLocations(setID)`: slot -> packed location, 1 ignored, -1 missing.
    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "GetItemLocations",
        move |lua, v: Value| {
            let id = set_id(&v);
            let locs = with(lua, &c, |s, m| {
                s.by_id(id).map(|set| {
                    (0..SLOT_COUNT)
                        .filter(|i| set.items[*i] != GUID_EMPTY)
                        .map(|i| {
                            let loc = match set.items[i] {
                                GUID_IGNORED => LOC_IGNORED,
                                g => match find_guid(m, g) {
                                    0 => LOC_MISSING,
                                    l => l,
                                },
                            };
                            (i + 1, loc)
                        })
                        .collect::<Vec<_>>()
                })
            });
            let Some(locs) = locs else {
                return Ok(none());
            };
            let t = lua.create_table()?;
            for (slot, loc) in locs {
                t.set(slot, loc)?;
            }
            t.into_lua_multi(lua)
        },
    )?;

    // `CreateEquipmentSet(name [, icon])`: the worn gear under a new id; nothing for a taken name.
    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "CreateEquipmentSet",
        move |lua, (n, i): (Value, Value)| {
            let Some(name) = as_string(&n).filter(|s| !s.is_empty()) else {
                return Err(mlua::Error::runtime(
                    "Usage: C_EquipmentSet.CreateEquipmentSet(name [, icon])",
                ));
            };
            let icon = as_string(&i).filter(|s| !s.is_empty());
            let id = with(lua, &c, |s, m| {
                if s.path.is_none() || s.by_name(&name).is_some() {
                    return None;
                }
                let mut set = Set {
                    id: s.sets.iter().map(|s| s.id).max().unwrap_or(0) + 1,
                    name: name.chars().take(MAX_NAME_LEN).collect(),
                    icon: icon.unwrap_or_else(|| DEFAULT_ICON.to_string()),
                    ..Set::default()
                };
                s.populate(&mut set, m);
                let id = set.id;
                s.sets.push(set);
                s.persist();
                Some(id)
            });
            match id {
                Some(id) => {
                    changed(lua);
                    id.into_lua_multi(lua)
                }
                None => Ok(none()),
            }
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "SaveEquipmentSet",
        move |lua, (v, i): (Value, Value)| {
            let id = set_id(&v);
            if id == 0 {
                return Err(mlua::Error::runtime(
                    "Usage: C_EquipmentSet.SaveEquipmentSet(setID [, icon])",
                ));
            }
            let icon = as_string(&i).filter(|s| !s.is_empty());
            let saved = with(lua, &c, |s, m| {
                let Some(idx) = s.sets.iter().position(|x| x.id == id) else {
                    return false;
                };
                let mut set = s.sets[idx].clone();
                if let Some(icon) = icon {
                    set.icon = icon;
                }
                s.populate(&mut set, m);
                s.sets[idx] = set;
                s.persist();
                true
            });
            if saved {
                changed(lua);
            }
            Ok(())
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "ModifyEquipmentSet",
        move |lua, (v, n): (Value, Value)| {
            let id = set_id(&v);
            let name = as_string(&n).filter(|s| !s.is_empty());
            let (true, Some(name)) = (id != 0, name) else {
                return Err(mlua::Error::runtime(
                    "Usage: C_EquipmentSet.ModifyEquipmentSet(setID, newName)",
                ));
            };
            let renamed = with(lua, &c, |s, _| {
                if s.by_name(&name).is_some_and(|x| x.id != id) {
                    return false;
                }
                let Some(set) = s.sets.iter_mut().find(|x| x.id == id) else {
                    return false;
                };
                set.name = name.chars().take(MAX_NAME_LEN).collect();
                s.persist();
                true
            });
            if renamed {
                changed(lua);
            }
            Ok(())
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "DeleteEquipmentSet",
        move |lua, v: Value| {
            let id = set_id(&v);
            if id == 0 {
                return Err(mlua::Error::runtime(
                    "Usage: C_EquipmentSet.DeleteEquipmentSet(setID)",
                ));
            }
            let gone = with(lua, &c, |s, _| {
                let before = s.sets.len();
                s.sets.retain(|x| x.id != id);
                let gone = s.sets.len() != before;
                if gone {
                    s.persist();
                }
                gone
            });
            if gone {
                changed(lua);
            }
            Ok(())
        },
    )?;

    for (name, ignore) in [("IgnoreSlotForSave", true), ("UnignoreSlotForSave", false)] {
        let c = api.ca.clone();
        api.table("C_EquipmentSet", name, move |lua, v: Value| {
            let slot = if is_number(&v) { to_int(&v) } else { 0 };
            if (1..=SLOT_COUNT as i64).contains(&slot) {
                c.lock().equipment_sets.ignored[slot as usize - 1] = ignore;
                changed(lua);
            }
            Ok(())
        })?;
    }
    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "IsSlotIgnoredForSave",
        move |_, v: Value| {
            let slot = if is_number(&v) { to_int(&v) } else { 0 };
            Ok((1..=SLOT_COUNT as i64).contains(&slot)
                && c.lock().equipment_sets.ignored[slot as usize - 1])
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "ClearIgnoredSlotsForSave",
        move |lua, ()| {
            c.lock().equipment_sets.ignored = [false; SLOT_COUNT];
            changed(lua);
            Ok(())
        },
    )?;

    // `EquipmentSetContainsLockedItems(setID)`: an owned item of the set is client-locked.
    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "EquipmentSetContainsLockedItems",
        move |lua, v: Value| {
            let id = set_id(&v);
            let locs = with(lua, &c, |s, m| {
                s.by_id(id).map(|set| {
                    set.items
                        .iter()
                        .map(|g| find_guid(m, *g))
                        .filter(|l| *l != 0)
                        .collect::<Vec<_>>()
                })
            });
            let Some(locs) = locs else {
                return Ok(none());
            };
            let g = lua.globals();
            let mut locked = false;
            for loc in locs {
                let v: Value = if loc & (LOC_BAGS | LOC_BANK) == 0 {
                    g.get::<mlua::Function>("IsInventoryItemLocked")?
                        .call(loc & 0xFF)?
                } else if loc & LOC_BANK == 0 {
                    let out: mlua::MultiValue = g
                        .get::<mlua::Function>("GetContainerItemInfo")?
                        .call(((loc >> 8) & 0xFF, loc & 0xFF))?;
                    out.into_iter().nth(2).unwrap_or(Value::Nil)
                } else {
                    Value::Nil
                };
                locked |= crate::lua::truthy(&v);
            }
            locked.into_lua_multi(lua)
        },
    )?;

    // `UseEquipmentSet(setID)`: worn items between paperdoll slots first, then bag items onto
    // the paperdoll, then each slot the set leaves empty into a free bag slot. Bank items are
    // left, as the server refuses to equip from the bank.
    let (c, verbs) = (api.ca.clone(), Verbs::capture(&api.lua.globals()));
    api.table("C_EquipmentSet", "UseEquipmentSet", move |lua, v: Value| {
        let id = set_id(&v);
        let plan = with(lua, &c, |s, m| {
            s.by_id(id).map(|set| {
                let sources: Vec<i64> = set.items.iter().map(|g| find_guid(m, *g)).collect();
                let paperdoll: Vec<u64> =
                    (0..SLOT_COUNT).map(|i| items::linear_guid(m, i)).collect();
                let free: Vec<(i64, i64)> = (0..=4)
                    .flat_map(|bag| {
                        (1..=items::bag_slot_count(m, bag) as i64)
                            .filter(move |s| items::bag_slot(m, bag, *s).is_none())
                            .map(move |s| (bag, s))
                    })
                    .take(64)
                    .collect();
                (set.items, sources, paperdoll, free)
            })
        });
        let fire = |name: &str, args: Vec<benilla_app::ext::ScriptValue>| {
            benilla_ui::script::ext_read::fire_event(lua, name, args);
        };
        let Some((guids, sources, mut doll, free)) = plan else {
            fire(
                "EQUIPMENT_SWAP_FINISHED",
                vec![
                    benilla_app::ext::ScriptValue::Bool(false),
                    benilla_app::ext::ScriptValue::Number(f64::from(id)),
                ],
            );
            return Ok(false);
        };
        fire(
            "EQUIPMENT_SWAP_PENDING",
            vec![benilla_app::ext::ScriptValue::Number(f64::from(id))],
        );
        verbs.clear_cursor()?;
        for i in 0..SLOT_COUNT {
            let (g, loc, target) = (guids[i], sources[i], i + 1);
            if matches!(g, GUID_EMPTY | GUID_IGNORED)
                || loc == 0
                || loc & (LOC_BANK | LOC_BAGS) != 0
            {
                continue;
            }
            let Some(src) = doll.iter().position(|x| *x == g).map(|p| p + 1) else {
                continue;
            };
            if src == target {
                continue;
            }
            verbs.paperdoll_to_paperdoll(src as i64, target as i64)?;
            let displaced = doll[target - 1];
            doll[target - 1] = g;
            doll[src - 1] = displaced;
        }
        for i in 0..SLOT_COUNT {
            let (g, loc, target) = (guids[i], sources[i], i + 1);
            if matches!(g, GUID_EMPTY | GUID_IGNORED)
                || doll[i] == g
                || loc == 0
                || loc & LOC_BANK != 0
                || loc & LOC_BAGS == 0
            {
                continue;
            }
            verbs.bag_to_paperdoll(((loc >> 8) & 0xFF, loc & 0xFF), target as i64)?;
            doll[i] = g;
        }
        let mut free = free.into_iter();
        for i in 0..SLOT_COUNT {
            if guids[i] != GUID_EMPTY || doll[i] == 0 {
                continue;
            }
            let Some(dst) = free.next() else {
                continue;
            };
            verbs.paperdoll_to_bag(i as i64 + 1, dst)?;
            doll[i] = 0;
        }
        fire(
            "EQUIPMENT_SWAP_FINISHED",
            vec![
                benilla_app::ext::ScriptValue::Bool(true),
                benilla_app::ext::ScriptValue::Number(f64::from(id)),
            ],
        );
        Ok(true)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_set_file_round_trips() {
        let mut s = Set {
            id: 3,
            name: "Tank".into(),
            icon: "INV_Shield_06".into(),
            action_slots: vec![11],
            ..Set::default()
        };
        s.items[0] = 0x4000_0000_0000_0123;
        s.item_ids[0] = 12640;
        s.items[3] = GUID_IGNORED;
        let back = parse(&render(&[s.clone()]));
        assert_eq!(back, vec![s]);
    }
}

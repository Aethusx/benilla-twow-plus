//! `loot/`: `C_Loot` and `C_LootHistory`, over benilla's loot verbs and quiet loot session
//! ([`benilla_app::ext::ExtLootSend`], [`benilla_app::ext::ExtLoot`]) and the group-roll packets.
//!
//! - `Unit.cpp`, `UnitItem.cpp`: open a unit's loot with no walk-to; take one item by id, from
//!   the open window at once or once the window for that unit arrives (3 s at most).
//! - `Nearby.cpp`: the lootable units (`UNIT_DYNFLAG_LOOTABLE`, which the server sets only for a
//!   unit we may loot) within the engine's interact range, both bounding radii plus 4/3 yd, at
//!   least 5 yd.
//! - `Scan.cpp`: walk those units one at a time in a quiet session, so the loot frame never
//!   opens: record each window, take everything in loot mode (items, then coin, then the
//!   release), and fire `LOOT_SCAN_COMPLETED` at the end. A unit that does not answer within 3 s
//!   is skipped.
//! - `History.cpp`: `C_LootHistory` rebuilt from `SMSG_LOOT_START_ROLL`, `SMSG_LOOT_ROLL`,
//!   `SMSG_LOOT_ROLL_WON` and `SMSG_LOOT_ALL_PASSED`, the last 128 rolled items with each
//!   roller's name and class from the name cache at the packet; `LOOT_HISTORY_FULL_UPDATE`,
//!   `LOOT_HISTORY_ROLL_CHANGED` and `LOOT_HISTORY_ROLL_COMPLETE`.

use std::time::{Duration, Instant};

use benilla_app::ext::{ExtLoot, ExtLootSend, ExtLootWindow};
use benilla_protocol::messages::{LootAllPassed, LootRoll, LootRollWon, LootStartRoll};
use benilla_ui::script::ScriptValue;
use mlua::{IntoLuaMulti, MultiValue, Value};

use crate::lua::{as_string, is_number, none, to_int, to_number, Api};
use crate::mirror::{field, typemask, Mirror};

/// `kStepTimeoutTicks` (180 world ticks, 3-6 s), as wall time.
const STEP_TIMEOUT: Duration = Duration::from_secs(3);
/// `UNIT_DYNFLAG_LOOTABLE`.
const DYNFLAG_LOOTABLE: u32 = 0x1;
/// The engine's interact constants (`DAT_0080B058`, `DAT_0080A1E8`).
const INTERACT_REACH: f32 = 4.0 / 3.0;
const MIN_INTERACT_RANGE: f32 = 5.0;
/// `ChrClasses.dbc`'s file token column.
const CLASS_TOKEN: usize = 0x38 / 4;

/// One recorded window.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    guid: u64,
    coin: u32,
    /// `(item id, count, random property)`.
    items: Vec<(u32, u32, u32)>,
}

/// The walk `ScanNearbyLoot` and `LootAllCorpses` share.
#[derive(Debug)]
struct Walk {
    loot: bool,
    queue: Vec<u64>,
    /// The unit asked, the response count when asked, and when.
    current: Option<(u64, u64, Instant)>,
}

/// The `C_Loot` state the natives and the frame share.
#[derive(Default)]
pub(crate) struct Loot {
    /// Sends for benilla, written out by the frame.
    sends: Vec<ExtLootSend>,
    /// benilla's last loot window and response count, copied each frame.
    window: Option<ExtLootWindow>,
    responses: u64,
    /// `LootUnitItem`'s pending take: the unit, the item, and when it was asked.
    unit_item: Option<(u64, u32, Instant)>,
    walk: Option<Walk>,
    results: Vec<Entry>,
    pub(crate) history: History,
}

/// The wire slot of the first row holding `item` in `window`.
fn slot_of(window: &ExtLootWindow, item: u32) -> Option<u8> {
    window
        .items
        .iter()
        .find(|r| r.item_id == item)
        .map(|r| r.slot)
}

impl Loot {
    fn window_for(&self, guid: u64) -> Option<&ExtLootWindow> {
        self.window.as_ref().filter(|w| w.guid == guid)
    }

    /// The frame's exchange: benilla's window in, the walk and the pending take advanced, the sends
    /// out. Returns whether a walk completed.
    pub(crate) fn tick(
        &mut self,
        ext: &ExtLoot,
        m: &Mirror,
        now: Instant,
    ) -> (Vec<ExtLootSend>, bool) {
        self.window = ext.window.clone();
        let fresh = ext.responses > self.responses;
        self.responses = ext.responses;
        if let Some((guid, item, at)) = self.unit_item {
            if fresh && self.window_for(guid).is_some() {
                if let Some(slot) = self.window_for(guid).and_then(|w| slot_of(w, item)) {
                    self.sends.push(ExtLootSend::Item(slot));
                }
                self.unit_item = None;
            } else if now.duration_since(at) >= STEP_TIMEOUT {
                self.unit_item = None;
            }
        }
        let completed = self.advance(m, now);
        (std::mem::take(&mut self.sends), completed)
    }

    /// One step of the walk: take the answered window, skip a silent unit, open the next.
    fn advance(&mut self, m: &Mirror, now: Instant) -> bool {
        let Some(walk) = self.walk.as_mut() else {
            return false;
        };
        if let Some((guid, asked, at)) = walk.current {
            let answered =
                self.responses > asked && self.window.as_ref().is_some_and(|w| w.guid == guid);
            if answered {
                let w = self.window.as_ref().expect("answered");
                self.results.push(Entry {
                    guid,
                    coin: w.gold,
                    items: w
                        .items
                        .iter()
                        .map(|r| (r.item_id, r.count, r.random_property))
                        .collect(),
                });
                if walk.loot {
                    for r in &w.items {
                        self.sends.push(ExtLootSend::Item(r.slot));
                    }
                    if w.gold != 0 {
                        self.sends.push(ExtLootSend::Money);
                    }
                }
                self.sends.push(ExtLootSend::Release(guid));
                walk.current = None;
            } else if now.duration_since(at) >= STEP_TIMEOUT {
                walk.current = None;
            } else {
                return false;
            }
        }
        while let Some(guid) = walk.queue.pop() {
            if m.object(guid).is_some_and(|f| f.is(typemask::UNIT)) {
                self.sends.push(ExtLootSend::Open { guid, quiet: true });
                walk.current = Some((guid, self.responses, now));
                return false;
            }
        }
        self.walk = None;
        true
    }

    /// `BeginWalk`; `false` when one runs or there is no player. The frame opens the first unit,
    /// or completes an empty walk, firing `LOOT_SCAN_COMPLETED`.
    fn begin(&mut self, m: &Mirror, loot: bool, max: usize) -> bool {
        if self.walk.is_some() || m.player == 0 {
            return false;
        }
        self.results.clear();
        let mut queue = nearby_lootable(m);
        if max != 0 {
            queue.truncate(max);
        }
        self.walk = Some(Walk {
            loot,
            queue,
            current: None,
        });
        true
    }
}

/// The lootable units in interact range, in the mirror's order.
fn nearby_lootable(m: &Mirror) -> Vec<u64> {
    let Some(me) = m.place(m.player) else {
        return Vec::new();
    };
    let my_reach = m
        .object(m.player)
        .map_or(0.0, |f| f.f32(field::UNIT_BOUNDING_RADIUS));
    let mut out: Vec<(u64, f32)> = m
        .objects
        .iter()
        .filter(|(g, f)| {
            **g != m.player
                && f.is(typemask::UNIT)
                && f.u32(field::UNIT_DYNAMIC_FLAGS) & DYNFLAG_LOOTABLE != 0
        })
        .filter_map(|(g, f)| {
            let p = m.place(*g)?;
            let d2: f32 = (0..3).map(|i| (p.pos[i] - me.pos[i]).powi(2)).sum();
            let range = (my_reach + f.f32(field::UNIT_BOUNDING_RADIUS) + INTERACT_REACH)
                .max(MIN_INTERACT_RANGE);
            (d2 <= range * range).then_some((*g, d2))
        })
        .collect();
    // The engine walks its visible-object table; a hash map has no order, so nearest last, the
    // first the walk pops.
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)));
    out.into_iter().map(|(g, _)| g).collect()
}

/// `Guid::Parse` on a native's guid argument, raising its usage error.
fn guid_arg(v: &Value, usage: &str) -> mlua::Result<u64> {
    let s = crate::lua::to_str(v).ok_or_else(|| mlua::Error::runtime(usage.to_string()))?;
    crate::guid::parse(&s)
        .filter(|g| *g != 0)
        .ok_or_else(|| mlua::Error::runtime(format!("{usage} — unparseable GUID")))
}

// ---- History ---------------------------------------------------------------

/// `rollType`s, the modern convention.
const ROLL_PASS: i64 = 0;
const ROLL_NEED: i64 = 1;
const ROLL_GREED: i64 = 2;
/// The ring's size.
const MAX_ITEMS: usize = 128;
const MAX_PLAYERS: usize = 40;

#[derive(Clone, Debug, PartialEq)]
struct PlayerRoll {
    guid: u64,
    name: String,
    class: u8,
    roll_type: i64,
    roll: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct HistItem {
    roll_id: u32,
    source: u64,
    slot: u32,
    item: u32,
    random_property: u32,
    winner: Option<u64>,
    done: bool,
    created: f64,
    players: Vec<PlayerRoll>,
}

/// The rolled items, oldest first.
#[derive(Default)]
pub(crate) struct History {
    items: std::collections::VecDeque<HistItem>,
    next_id: u32,
}

/// What a recorded packet fires: a structural rebuild, then the row event.
type Fires = Vec<(&'static str, Vec<ScriptValue>)>;

impl History {
    /// `FindOrCreate`: the item's 1-based index and whether the set changed shape.
    fn find_or_create(&mut self, source: u64, slot: u32, item: u32, now: f64) -> (usize, bool) {
        if let Some(i) = self
            .items
            .iter()
            .position(|e| e.source == source && e.slot == slot)
        {
            if item != 0 {
                self.items[i].item = item;
            }
            return (i + 1, false);
        }
        if self.items.len() == MAX_ITEMS {
            self.items.pop_front();
        }
        self.next_id += 1;
        self.items.push_back(HistItem {
            roll_id: self.next_id,
            source,
            slot,
            item,
            random_property: 0,
            winner: None,
            done: false,
            created: now,
            players: Vec::new(),
        });
        (self.items.len(), true)
    }

    /// `FindOrAddPlayer`, the name and class from the name cache at the packet.
    fn player(e: &mut HistItem, m: &Mirror, guid: u64) -> Option<usize> {
        if let Some(i) = e.players.iter().position(|p| p.guid == guid) {
            return Some(i);
        }
        if e.players.len() >= MAX_PLAYERS {
            return None;
        }
        let (name, class) = m
            .player_names
            .get(&guid)
            .map(|(n, rcg)| (n.clone(), rcg.map_or(0, |(_, c, _)| c)))
            .unwrap_or_default();
        e.players.push(PlayerRoll {
            guid,
            name,
            class,
            roll_type: ROLL_PASS,
            roll: 0,
        });
        Some(e.players.len() - 1)
    }

    fn full(changed: bool, fires: &mut Fires) {
        if changed {
            fires.push(("LOOT_HISTORY_FULL_UPDATE", vec![]));
        }
    }

    pub(crate) fn start(&mut self, p: &LootStartRoll, now: f64) -> Fires {
        let (i, changed) = self.find_or_create(p.looted_target, p.item_slot, p.item_id, now);
        self.items[i - 1].random_property = p.random_property_id;
        let mut fires = Fires::new();
        Self::full(changed, &mut fires);
        fires
    }

    /// `RecordRoll`: the roll byte's high bit is a pass; else type 2 is greed, anything else need.
    pub(crate) fn roll(&mut self, p: &LootRoll, m: &Mirror, now: f64) -> Fires {
        let (i, changed) = self.find_or_create(p.looted_target, p.item_slot, p.item_id, now);
        let mut player_idx = 0;
        if p.roller != 0 {
            let e = &mut self.items[i - 1];
            if let Some(k) = Self::player(e, m, p.roller) {
                let rolled = p.roll_number & 0x80 == 0;
                e.players[k].roll = if rolled {
                    i64::from(p.roll_number & 0x7f)
                } else {
                    0
                };
                e.players[k].roll_type = match (rolled, p.roll_type) {
                    (false, _) => ROLL_PASS,
                    (true, 2) => ROLL_GREED,
                    (true, _) => ROLL_NEED,
                };
                player_idx = k + 1;
            }
        }
        let mut fires = Fires::new();
        Self::full(changed, &mut fires);
        fires.push((
            "LOOT_HISTORY_ROLL_CHANGED",
            vec![
                ScriptValue::Int(i as i64),
                ScriptValue::Int(player_idx as i64),
            ],
        ));
        fires
    }

    pub(crate) fn won(&mut self, p: &LootRollWon, m: &Mirror, now: f64) -> Fires {
        let (i, changed) = self.find_or_create(p.looted_target, p.item_slot, p.item_id, now);
        let e = &mut self.items[i - 1];
        e.winner = Some(p.winner);
        e.done = true;
        if p.winner != 0 {
            Self::player(e, m, p.winner);
        }
        let mut fires = Fires::new();
        Self::full(changed, &mut fires);
        fires.push((
            "LOOT_HISTORY_ROLL_COMPLETE",
            vec![ScriptValue::Int(i as i64)],
        ));
        fires
    }

    pub(crate) fn all_passed(&mut self, p: &LootAllPassed, now: f64) -> Fires {
        let (i, changed) = self.find_or_create(p.looted_target, p.item_slot, p.item_id, now);
        self.items[i - 1].done = true;
        let mut fires = Fires::new();
        Self::full(changed, &mut fires);
        fires.push((
            "LOOT_HISTORY_ROLL_COMPLETE",
            vec![ScriptValue::Int(i as i64)],
        ));
        fires
    }

    fn get(&self, index: i64) -> Option<&HistItem> {
        self.items.get(usize::try_from(index).ok()?.checked_sub(1)?)
    }
}

/// `WinnerIndex`: the winner's 1-based player index.
fn winner_index(e: &HistItem) -> Option<usize> {
    let w = e.winner.filter(|w| *w != 0)?;
    e.players.iter().position(|p| p.guid == w).map(|i| i + 1)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_Loot";
    let c = api.ca.clone();
    api.table(NS, "LootUnit", move |_, v: Value| {
        const USAGE: &str = "Usage: C_Loot.LootUnit(guid)";
        if as_string(&v).is_none() && !is_number(&v) {
            return Err(mlua::Error::runtime(USAGE));
        }
        let guid = guid_arg(&v, USAGE)?;
        let mut st = c.lock();
        let held =
            st.mirror.player != 0 && st.mirror.object(guid).is_some_and(|f| f.is(typemask::UNIT));
        if held {
            st.loot.sends.push(ExtLootSend::Open { guid, quiet: false });
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table(NS, "LootUnitItem", move |_, (g, item): (Value, Value)| {
        const USAGE: &str = "Usage: C_Loot.LootUnitItem(guid, itemID)";
        if (as_string(&g).is_none() && !is_number(&g)) || !is_number(&item) {
            return Err(mlua::Error::runtime(USAGE));
        }
        let guid = guid_arg(&g, USAGE)?;
        let item = to_number(&item) as u32;
        let mut st = c.lock();
        if st.loot.unit_item.is_some() || st.mirror.player == 0 {
            return Ok(false);
        }
        if let Some(w) = st.loot.window_for(guid) {
            let slot = slot_of(w, item);
            if let Some(slot) = slot {
                st.loot.sends.push(ExtLootSend::Item(slot));
            }
            return Ok(slot.is_some());
        }
        if !st.mirror.object(guid).is_some_and(|f| f.is(typemask::UNIT)) {
            return Ok(false);
        }
        st.loot.sends.push(ExtLootSend::Open { guid, quiet: false });
        st.loot.unit_item = Some((guid, item, Instant::now()));
        Ok(true)
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetNearbyLootableUnits", move |lua, ()| {
        let out = lua.create_table()?;
        let guids = nearby_lootable(&c.lock().mirror);
        for (i, g) in guids.into_iter().rev().enumerate() {
            let t = lua.create_table()?;
            t.set("guid", crate::guid::format(g))?;
            out.raw_set(i + 1, t)?;
        }
        Ok(out)
    })?;
    let c = api.ca.clone();
    api.table(NS, "ScanNearbyLoot", move |_, ()| {
        let mut st = c.lock();
        let crate::State { loot, mirror, .. } = &mut *st;
        Ok(loot.begin(mirror, false, 0))
    })?;
    let c = api.ca.clone();
    api.table(NS, "LootAllCorpses", move |_, v: Value| {
        let max = if is_number(&v) {
            to_int(&v).max(0) as usize
        } else {
            0
        };
        let mut st = c.lock();
        let crate::State { loot, mirror, .. } = &mut *st;
        Ok(loot.begin(mirror, true, max))
    })?;
    let c = api.ca.clone();
    api.table(NS, "IsScanInProgress", move |_, ()| {
        Ok(c.lock().loot.walk.is_some())
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetLastScanResults", move |lua, ()| {
        let results = c.lock().loot.results.clone();
        let out = lua.create_table()?;
        for (i, e) in results.into_iter().enumerate() {
            let t = lua.create_table()?;
            t.set("guid", crate::guid::format(e.guid))?;
            t.set("coin", e.coin)?;
            let items = lua.create_table()?;
            for (k, (id, count, prop)) in e.items.into_iter().enumerate() {
                let it = lua.create_table()?;
                it.set("itemID", id)?;
                it.set("count", count)?;
                if let Some(link) = crate::items::link(lua, id, prop as i32) {
                    it.set("link", link)?;
                }
                items.raw_set(k + 1, it)?;
            }
            t.set("items", items)?;
            out.raw_set(i + 1, t)?;
        }
        Ok(out)
    })?;

    const H: &str = "C_LootHistory";
    let c = api.ca.clone();
    api.table(H, "GetNumItems", move |_, ()| {
        Ok(c.lock().loot.history.items.len())
    })?;
    let c = api.ca.clone();
    api.table(
        H,
        "GetItem",
        move |lua, v: Value| -> mlua::Result<MultiValue> {
            if !is_number(&v) {
                return Err(mlua::Error::runtime("Usage: GetItem(itemIndex)"));
            }
            let e = c.lock().loot.history.get(to_int(&v)).cloned();
            let Some(e) = e else {
                return Ok(none());
            };
            let link = format!("item:{}:0:{}:0", e.item, e.random_property);
            (
                e.roll_id,
                link,
                e.players.len(),
                e.done,
                winner_index(&e),
                e.created,
            )
                .into_lua_multi(lua)
        },
    )?;
    let c = api.ca.clone();
    api.table(
        H,
        "GetPlayerInfo",
        move |lua, (i, p): (Value, Value)| -> mlua::Result<MultiValue> {
            if !is_number(&i) || !is_number(&p) {
                return Err(mlua::Error::runtime(
                    "Usage: GetPlayerInfo(itemIndex, playerIndex)",
                ));
            }
            let (e, me) = {
                let st = c.lock();
                (st.loot.history.get(to_int(&i)).cloned(), st.mirror.player)
            };
            let Some(e) = e else {
                return Ok(none());
            };
            let Some(r) = usize::try_from(to_int(&p) - 1)
                .ok()
                .and_then(|k| e.players.get(k))
            else {
                return Ok(none());
            };
            let token =
                c.db.get("ChrClasses")
                    .and_then(|t| {
                        t.row(u32::from(r.class))
                            .map(|row| row.str(CLASS_TOKEN).to_string())
                    })
                    .filter(|s| !s.is_empty());
            (
                (!r.name.is_empty()).then(|| r.name.clone()),
                token,
                r.roll_type,
                r.roll,
                e.winner == Some(r.guid),
                r.guid == me,
            )
                .into_lua_multi(lua)
        },
    )?;
    let c = api.ca.clone();
    api.table(H, "Clear", move |lua, ()| {
        c.lock().loot.history.items.clear();
        benilla_ui::script::ext_read::fire_event(lua, "LOOT_HISTORY_FULL_UPDATE", vec![]);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_app::ext::ExtLootRow;

    fn window(guid: u64, gold: u32, rows: &[(u8, u32)]) -> ExtLootWindow {
        ExtLootWindow {
            guid,
            gold,
            items: rows
                .iter()
                .map(|&(slot, item_id)| ExtLootRow {
                    slot,
                    item_id,
                    count: 1,
                    random_property: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn a_loot_walk_opens_quietly_takes_everything_and_releases() {
        let now = Instant::now();
        let mut m = Mirror::default();
        m.player = 1;
        for g in [10u64, 20] {
            let mut cells = vec![0u32; 160];
            cells[field::OBJECT_TYPE] = 0x1 | typemask::UNIT;
            m.objects.insert(g, crate::mirror::Fields::from_vec(cells));
        }
        let mut loot = Loot::default();
        loot.walk = Some(Walk {
            loot: true,
            queue: vec![20, 10],
            current: None,
        });
        let mut ext = ExtLoot::default();
        let (sends, done) = loot.tick(&ext, &m, now);
        assert_eq!(
            sends,
            vec![ExtLootSend::Open {
                guid: 10,
                quiet: true
            }]
        );
        assert!(!done);
        // No answer yet: nothing moves.
        assert_eq!(loot.tick(&ext, &m, now), (vec![], false));
        ext.responses = 1;
        ext.window = Some(window(10, 25, &[(0, 2589), (1, 4306)]));
        let (sends, _) = loot.tick(&ext, &m, now);
        assert_eq!(
            sends,
            vec![
                ExtLootSend::Item(0),
                ExtLootSend::Item(1),
                ExtLootSend::Money,
                ExtLootSend::Release(10),
                ExtLootSend::Open {
                    guid: 20,
                    quiet: true
                },
            ]
        );
        // The second unit never answers: skipped after the timeout, and the walk completes.
        let (sends, done) = loot.tick(&ext, &m, now + STEP_TIMEOUT);
        assert!(sends.is_empty());
        assert!(done);
        assert_eq!(loot.results.len(), 1);
        assert_eq!(loot.results[0].coin, 25);
    }

    #[test]
    fn the_roll_history_follows_the_packets() {
        let mut m = Mirror::default();
        m.player = 7;
        m.player_names.insert(7, ("Me".into(), Some((1, 1, 0))));
        let mut h = History::default();
        let start = LootStartRoll {
            looted_target: 99,
            item_slot: 2,
            item_id: 1234,
            random_property_id: 5,
            countdown_ms: 60_000,
        };
        assert_eq!(h.start(&start, 1.0)[0].0, "LOOT_HISTORY_FULL_UPDATE");
        let roll = LootRoll {
            looted_target: 99,
            item_slot: 2,
            roller: 7,
            item_id: 1234,
            random_property_id: 5,
            roll_number: 77,
            roll_type: 2,
        };
        let fires = h.roll(&roll, &m, 2.0);
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].1, vec![ScriptValue::Int(1), ScriptValue::Int(1)]);
        let pass = LootRoll {
            roller: 8,
            roll_number: 128,
            roll_type: 128,
            ..roll
        };
        h.roll(&pass, &m, 2.0);
        let won = LootRollWon {
            looted_target: 99,
            item_slot: 2,
            item_id: 1234,
            random_property_id: 5,
            winner: 7,
            roll_number: 77,
            roll_type: 2,
        };
        assert_eq!(h.won(&won, &m, 3.0)[0].0, "LOOT_HISTORY_ROLL_COMPLETE");
        let e = h.get(1).unwrap();
        assert_eq!(
            (e.random_property, e.done, winner_index(e)),
            (5, true, Some(1))
        );
        assert_eq!(
            (
                e.players[0].name.as_str(),
                e.players[0].class,
                e.players[0].roll_type,
                e.players[0].roll
            ),
            ("Me", 1, ROLL_GREED, 77)
        );
        assert_eq!((e.players[1].roll_type, e.players[1].roll), (ROLL_PASS, 0));
    }
}

//! `container/SortBags.cpp`: `C_Container.SortBags` and `SortBankBags`, the fill direction and
//! the backpack and bank opt-outs, over benilla's direct item moves
//! ([`benilla_app::ext::ExtItemMove`]).
//!
//! Two phases, every move a server round trip. Phase one tops up partial stacks, fullest first,
//! poured from the back into the front, a whole-stack swap or a split per transfer; an item whose
//! data has not arrived is offered whole, the server merging what fits. Phase two runs once the
//! merges show in the bags (or after a second), plans the whole layout and fires every swap in one
//! frame, tracking each item's live position as the swaps land in the plan.
//!
//! The order is the DLL's own, not Blizzard's: hearthstone, gear, consumables, reagents, trade
//! goods, quest items, the rest by quality, junk last; within a category by class, gear's slot
//! rank, subclass, gear's quality best first, name, fuller stacks, id. A specialty bag takes its
//! own family first and overflows into the general cells; junk fills the general cells from the
//! far end. Two stacks of one item never swap with each other: the server would merge them. An
//! item whose data has not arrived stays where it is, its slot withheld.
//!
//! The three settings are the DLL's CVars, `sortBagsRightToLeft`, `backpackAutosortDisabled` and
//! `bankAutosortDisabled`, registered as addon CVars so they persist.

use std::sync::Arc;
use std::time::{Duration, Instant};

use benilla_app::ext::ExtItemMove;
use benilla_protocol::ItemInfo;
use mlua::{Function, Lua, Value};

use crate::items;
use crate::lua::{truthy, Api};
use crate::mirror::{field, Mirror};
use crate::Ca;

const INVENTORY_BAGS: [i64; 5] = [0, 1, 2, 3, 4];
const BANK_BAGS: [i64; 7] = [-1, 5, 6, 7, 8, 9, 10];
const HEARTHSTONE: u32 = 6948;
/// How long phase two waits for the merges to show before planning anyway.
const MERGE_WAIT: Duration = Duration::from_secs(1);

const CVAR_RTL: &str = "sortBagsRightToLeft";
const CVAR_BACKPACK: &str = "backpackAutosortDisabled";
const CVAR_BANK: &str = "bankAutosortDisabled";

/// Gear's slot key: `INVTYPE_*` in the order like pieces sit (Baganator's).
const SLOT_ORDER: [u32; 29] = [
    17, 13, 21, 14, 23, 26, 22, 15, 25, 24, 27, 28, 1, 3, 16, 5, 20, 9, 10, 6, 7, 8, 2, 11, 12, 4,
    19, 18, 0,
];

fn slot_rank(inventory_type: u32) -> u8 {
    SLOT_ORDER
        .iter()
        .position(|t| *t == inventory_type)
        .unwrap_or(SLOT_ORDER.len()) as u8
}

/// `CategoryFor`.
fn category(id: u32, class: u32, quality: u32) -> u8 {
    match (id, quality, class) {
        (HEARTHSTONE, _, _) => 0,
        (_, 0, _) => 10,
        (_, _, 2 | 4) => 1,
        (_, _, 0) => 2,
        (_, _, 5) => 3,
        (_, _, 7) => 4,
        (_, _, 12) => 5,
        (_, q, _) if q >= 4 => 6,
        (_, 3, _) => 7,
        (_, 2, _) => 8,
        _ => 9,
    }
}

const JUNK: u8 = 10;
const GEAR: u8 = 1;

/// One bag slot as the sort sees it.
#[derive(Clone, Debug)]
struct Cell {
    bag: i64,
    slot: u32,
    /// The bag's family mask, 0 for a general bag.
    family: u32,
    item: Option<Held>,
}

#[derive(Clone, Debug)]
struct Held {
    id: u32,
    count: u32,
    info: Option<Arc<ItemInfo>>,
}

/// The cells of `bags` in bag then slot order.
fn snapshot(
    m: &Mirror,
    bags: &[i64],
    records: &dyn Fn(u32) -> Option<Arc<ItemInfo>>,
    turtle: bool,
) -> Vec<Cell> {
    let mut out = Vec::new();
    for &bag in bags {
        let n = items::bag_slot_count(m, bag);
        if n == 0 {
            continue;
        }
        let family = items::bag_object(m, bag)
            .and_then(|f| records(f.entry()))
            .map_or(0, |r| super::item::data::bag_family_mask(&r, turtle));
        for slot in 1..=n as u32 {
            let item = items::bag_slot(m, bag, i64::from(slot)).map(|guid| {
                let id = items::item_id(m, guid);
                let count = m
                    .object(guid)
                    .map_or(1, |f| f.u32(field::ITEM_STACK_COUNT).max(1));
                Held {
                    id,
                    count,
                    info: records(id),
                }
            });
            out.push(Cell {
                bag,
                slot,
                family,
                item,
            });
        }
    }
    out
}

fn swap(src: (i64, u32), dst: (i64, u32)) -> ExtItemMove {
    ExtItemMove {
        src_bag: src.0,
        src_slot: src.1,
        dst_bag: dst.0,
        dst_slot: dst.1,
        count: None,
    }
}

/// Phase one: the merge moves and the sources each whole-stack move empties.
fn consolidate(cells: &[Cell]) -> (Vec<ExtItemMove>, Vec<(i64, u32)>) {
    // `(item id, stack cap, stacks as (bag, slot, count))`.
    type Group = (u32, u32, Vec<(i64, u32, u32)>);
    let mut groups: Vec<Group> = Vec::new();
    for c in cells {
        let Some(h) = &c.item else { continue };
        if h.id == 0 {
            continue;
        }
        // An uncached item is offered whole: its cap is unknown, so its space reads as unbounded.
        let cap = match &h.info {
            Some(i) if i.stackable <= 1 => continue,
            Some(i) => i.stackable,
            None => u32::MAX,
        };
        match groups.iter_mut().find(|g| g.0 == h.id) {
            Some(g) => g.2.push((c.bag, c.slot, h.count)),
            None => groups.push((h.id, cap, vec![(c.bag, c.slot, h.count)])),
        }
    }
    let mut moves = Vec::new();
    let mut sources = Vec::new();
    for (_, cap, mut stacks) in groups {
        if stacks.len() < 2 {
            continue;
        }
        stacks.sort_by_key(|s| std::cmp::Reverse(s.2));
        let (mut lo, mut hi) = (0, stacks.len() - 1);
        while lo < hi {
            let space = cap.saturating_sub(stacks[lo].2);
            if space == 0 {
                lo += 1;
                continue;
            }
            let whole = space >= stacks[hi].2;
            let moved = if whole { stacks[hi].2 } else { space };
            let (src, dst) = ((stacks[hi].0, stacks[hi].1), (stacks[lo].0, stacks[lo].1));
            moves.push(ExtItemMove {
                count: (!whole).then_some(moved),
                ..swap(src, dst)
            });
            if whole {
                sources.push(src);
            }
            stacks[lo].2 += moved;
            stacks[hi].2 -= moved;
            if stacks[hi].2 == 0 {
                hi -= 1;
            } else {
                lo += 1;
            }
        }
    }
    (moves, sources)
}

/// One item to place.
#[derive(Clone, Debug)]
struct Entry {
    cur: (i64, u32),
    dest: Option<(i64, u32)>,
    id: u32,
    family: u32,
    class: u32,
    subclass: u32,
    quality: u32,
    count: u32,
    name: String,
    category: u8,
    slot_rank: u8,
}

/// `Precedes`.
fn precedes(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let gear = a.category == GEAR;
    a.category
        .cmp(&b.category)
        .then(a.class.cmp(&b.class))
        .then(if gear {
            a.slot_rank.cmp(&b.slot_rank)
        } else {
            Ordering::Equal
        })
        .then(a.subclass.cmp(&b.subclass))
        .then(if gear {
            b.quality.cmp(&a.quality)
        } else {
            Ordering::Equal
        })
        .then(
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase()),
        )
        .then(b.count.cmp(&a.count))
        .then(a.id.cmp(&b.id))
}

/// Phase two: the swaps that lay the bags out.
fn place(cells: &[Cell], right_to_left: bool, turtle: bool) -> Vec<ExtItemMove> {
    let mut entries = Vec::new();
    let mut general: Vec<(i64, u32)> = Vec::new();
    // Per family: its cells and the next free one.
    type Pool = (u32, Vec<(i64, u32)>, usize);
    let mut specialty: Vec<Pool> = Vec::new();
    for c in cells {
        let info = match &c.item {
            Some(h) => match &h.info {
                Some(i) => Some((h, i)),
                None => continue, // pinned: unrankable, its slot withheld
            },
            None => None,
        };
        if c.family == 0 {
            general.push((c.bag, c.slot));
        } else {
            match specialty.iter_mut().find(|p| p.0 == c.family) {
                Some(p) => p.1.push((c.bag, c.slot)),
                None => specialty.push((c.family, vec![(c.bag, c.slot)], 0)),
            }
        }
        let Some((h, i)) = info else { continue };
        entries.push(Entry {
            cur: (c.bag, c.slot),
            dest: None,
            id: h.id,
            family: super::item::data::bag_family_mask(i, turtle),
            class: i.class,
            subclass: i.subclass,
            quality: i.quality,
            count: h.count,
            name: i.name.clone(),
            category: category(h.id, i.class, i.quality),
            slot_rank: slot_rank(i.inventory_type),
        });
    }
    if entries.is_empty() {
        return Vec::new();
    }
    entries.sort_by(precedes);
    if right_to_left {
        general.reverse();
        for p in &mut specialty {
            p.1.reverse();
        }
    }
    let (mut front, mut back) = (0, general.len());
    for e in entries.iter_mut().filter(|e| e.category != JUNK) {
        let mut cell = None;
        if e.family != 0 {
            if let Some(p) = specialty
                .iter_mut()
                .find(|p| p.0 == e.family && p.2 < p.1.len())
            {
                cell = Some(p.1[p.2]);
                p.2 += 1;
            }
        }
        if cell.is_none() && front < back {
            cell = Some(general[front]);
            front += 1;
        }
        e.dest = cell;
    }
    for e in entries.iter_mut().rev().filter(|e| e.category == JUNK) {
        if back <= front {
            break;
        }
        back -= 1;
        e.dest = Some(general[back]);
    }
    // Same-item runs trade destinations so no two stacks of one item swap with each other.
    let mut i = 0;
    while i < entries.len() {
        let mut j = i;
        while j < entries.len() && entries[j].id == entries[i].id {
            j += 1;
        }
        for m in i..j {
            for n in i..j {
                if n != m && entries[n].dest == Some(entries[m].cur) {
                    let (dm, dn) = (entries[m].dest, entries[n].dest);
                    entries[m].dest = dn;
                    entries[n].dest = dm;
                    break;
                }
            }
        }
        i = j;
    }
    let mut moves = Vec::new();
    for k in 0..entries.len() {
        let Some(dest) = entries[k].dest else {
            continue;
        };
        let from = entries[k].cur;
        if dest == from {
            continue;
        }
        moves.push(swap(from, dest));
        if let Some(o) = entries.iter().position(|o| o.cur == dest) {
            entries[o].cur = from;
        }
        entries[k].cur = dest;
    }
    moves
}

/// A sort waiting on its merges: its bags, the merged-away sources, the fill direction, and when
/// phase one went out.
type Awaiting = (Vec<i64>, Vec<(i64, u32)>, bool, Instant);

/// The sort in flight.
#[derive(Default)]
pub(crate) struct Sorter {
    /// The bags of a sort waiting on its merges, the merged-away sources, the fill direction, and
    /// when phase one went out.
    awaiting: Option<Awaiting>,
    /// Moves for benilla, written out by the frame.
    moves: Vec<ExtItemMove>,
}

impl Sorter {
    /// `StartSort`: phase one, else straight to placement.
    fn start(
        &mut self,
        m: &Mirror,
        bags: Vec<i64>,
        rtl: bool,
        records: &dyn Fn(u32) -> Option<Arc<ItemInfo>>,
        turtle: bool,
    ) {
        if self.awaiting.is_some() {
            return;
        }
        let cells = snapshot(m, &bags, records, turtle);
        let (merges, sources) = consolidate(&cells);
        if merges.is_empty() {
            self.moves.extend(place(&cells, rtl, turtle));
        } else {
            self.moves.extend(merges);
            self.awaiting = Some((bags, sources, rtl, Instant::now()));
        }
    }

    /// The frame: phase two once every merged-away source is empty or the wait ran out, then the
    /// moves out.
    pub(crate) fn tick(
        &mut self,
        m: &Mirror,
        records: &dyn Fn(u32) -> Option<Arc<ItemInfo>>,
        turtle: bool,
        now: Instant,
    ) -> Vec<ExtItemMove> {
        if let Some((bags, sources, rtl, at)) = &self.awaiting {
            let settled = sources
                .iter()
                .all(|(b, s)| items::bag_slot(m, *b, i64::from(*s)).is_none());
            if settled || now.duration_since(*at) >= MERGE_WAIT {
                let cells = snapshot(m, bags, records, turtle);
                let moves = place(&cells, *rtl, turtle);
                self.moves.extend(moves);
                self.awaiting = None;
            }
        }
        std::mem::take(&mut self.moves)
    }
}

fn cvar_on(lua: &Lua, name: &str) -> bool {
    lua.globals()
        .get::<Function>("GetCVar")
        .and_then(|f| f.call::<Value>(name))
        .is_ok_and(
            |v| matches!(crate::lua::to_str(&v).as_deref(), Some(s) if s != "0" && !s.is_empty()),
        )
}

fn set_cvar(lua: &Lua, name: &str, on: bool) -> mlua::Result<()> {
    let f: Function = lua.globals().get("SetCVar")?;
    f.call::<()>((name, if on { "1" } else { "0" }))
}

/// `StartSortExcluding`.
fn sort(lua: &Lua, ca: &Ca, bags: &[i64], excluded: i64, cvar: &str) {
    let skip = cvar_on(lua, cvar);
    let rtl = cvar_on(lua, CVAR_RTL);
    let kept: Vec<i64> = bags
        .iter()
        .copied()
        .filter(|b| !(skip && *b == excluded))
        .collect();
    if kept.is_empty() {
        return;
    }
    let db = ca.items.clone();
    let records = |id: u32| db.lock().peek(id);
    let mut st = ca.lock();
    let turtle = st.auras.turtle;
    let crate::State { sorter, mirror, .. } = &mut *st;
    sorter.start(mirror, kept, rtl, &records, turtle);
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    if let Ok(register) = api.lua.globals().get::<Function>("RegisterCVar") {
        for name in [CVAR_RTL, CVAR_BACKPACK, CVAR_BANK] {
            register.call::<()>((name, "0"))?;
        }
    }
    const NS: &str = "C_Container";
    let c = api.ca.clone();
    api.table(NS, "SortBags", move |lua, ()| {
        sort(lua, &c, &INVENTORY_BAGS, 0, CVAR_BACKPACK);
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table(NS, "SortBankBags", move |lua, ()| {
        sort(lua, &c, &BANK_BAGS, -1, CVAR_BANK);
        Ok(())
    })?;
    for (get, set, cvar) in [
        ("GetSortBagsRightToLeft", "SetSortBagsRightToLeft", CVAR_RTL),
        (
            "GetBackpackAutosortDisabled",
            "SetBackpackAutosortDisabled",
            CVAR_BACKPACK,
        ),
        (
            "GetBankAutosortDisabled",
            "SetBankAutosortDisabled",
            CVAR_BANK,
        ),
    ] {
        api.table(NS, get, move |lua, ()| Ok(cvar_on(lua, cvar)))?;
        api.table(NS, set, move |lua, v: Value| {
            set_cvar(lua, cvar, truthy(&v))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, class: u32, quality: u32, stackable: u32) -> Arc<ItemInfo> {
        let mut i = crate::lua::item::data::tests::test_item();
        i.name = name.into();
        i.class = class;
        i.quality = quality;
        i.stackable = stackable;
        Arc::new(i)
    }

    fn cell(bag: i64, slot: u32, item: Option<(u32, u32, &Arc<ItemInfo>)>) -> Cell {
        Cell {
            bag,
            slot,
            family: 0,
            item: item.map(|(id, count, i)| Held {
                id,
                count,
                info: Some(i.clone()),
            }),
        }
    }

    #[test]
    fn phase_one_tops_up_stacks_fullest_first() {
        let cloth = info("Linen Cloth", 7, 1, 20);
        let cells = vec![
            cell(0, 1, Some((2589, 5, &cloth))),
            cell(0, 2, Some((2589, 18, &cloth))),
            cell(1, 1, Some((2589, 9, &cloth))),
        ];
        let (moves, sources) = consolidate(&cells);
        // 18 takes 2 of the 5 (a split), then the 9 takes the remaining 3 whole.
        assert_eq!(
            moves,
            vec![
                ExtItemMove {
                    count: Some(2),
                    ..swap((0, 1), (0, 2))
                },
                swap((0, 1), (1, 1)),
            ]
        );
        assert_eq!(sources, vec![(0, 1)]);
    }

    #[test]
    fn phase_two_orders_by_category_and_sends_junk_to_the_far_end() {
        let hearth = info("Hearthstone", 15, 1, 1);
        let potion = info("Minor Healing Potion", 0, 1, 5);
        let junk = info("Ruined Pelt", 15, 0, 1);
        let cells = vec![
            cell(0, 1, Some((4865, 1, &junk))),
            cell(0, 2, Some((118, 3, &potion))),
            cell(0, 3, None),
            cell(0, 4, Some((6948, 1, &hearth))),
        ];
        let moves = place(&cells, false, false);
        // Replayed, the swaps leave the hearthstone first and the junk at the far end.
        let mut layout: Vec<(u32, u32)> = vec![(1, 4865), (2, 118), (4, 6948)];
        for m in &moves {
            let a = layout.iter().position(|(s, _)| *s == m.src_slot);
            let b = layout.iter().position(|(s, _)| *s == m.dst_slot);
            if let Some(a) = a {
                layout[a].0 = m.dst_slot;
            }
            if let Some(b) = b {
                if Some(b) != a {
                    layout[b].0 = m.src_slot;
                }
            }
        }
        layout.sort();
        assert_eq!(layout, vec![(1, 6948), (2, 118), (4, 4865)]);
    }
    #[test]
    fn the_settings_round_trip_through_their_cvars() {
        let script = crate::lua::test_support::vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local C = C_Container
                local before = tostring(C.GetSortBagsRightToLeft())
                C.SetSortBagsRightToLeft(true)
                C.SetBankAutosortDisabled(1)
                return before .. tostring(C.GetSortBagsRightToLeft())
                  .. tostring(C.GetBankAutosortDisabled()) .. tostring(C.GetBackpackAutosortDisabled())
                  .. GetCVar("sortBagsRightToLeft")
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "falsetruetruefalse1");
    }
}

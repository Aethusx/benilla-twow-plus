//! `NewItems.cpp`: `C_NewItems`, the bag items that arrived since login, and
//! `BAG_NEW_ITEMS_UPDATED`.
//!
//! A baseline of everything owned is taken 1.5 s after the player resolves; afterwards any bag
//! item whose guid was not owned is new. An owned guid survives four bag changes unseen, so an
//! item moved through the bank or the paperdoll is not reborn new.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mlua::Value;

use crate::items;
use crate::lua::{is_number, to_int, Api};
use crate::mirror::Mirror;

const SETTLE: Duration = Duration::from_millis(1500);
const MAX_MISS: u8 = 4;

#[derive(Default)]
pub(crate) struct NewItems {
    player: u64,
    login: Option<Instant>,
    seeded: bool,
    /// Every owned guid and its unseen streak.
    seen: HashMap<u64, u8>,
    new: Vec<u64>,
    /// The bag contents at the last frame, the bag-change edge.
    last_bags: Vec<u64>,
    /// `BAG_NEW_ITEMS_UPDATED` is due.
    pub fire: bool,
}

/// Bag guids (backpack and bags 1-4) in walk order.
fn bag_guids(m: &Mirror) -> Vec<u64> {
    items::bagged(m, 0..=4)
        .into_iter()
        .map(|(_, _, it)| it.guid)
        .collect()
}

/// Everything else owned: worn and the bag containers, the bank and its bags.
fn other_owned(m: &Mirror) -> Vec<u64> {
    let mut out: Vec<u64> = (1..=23)
        .filter_map(|s| items::equipment_slot(m, s))
        .collect();
    out.extend(
        items::bagged(m, std::iter::once(items::BANK_CONTAINER).chain(5..=10))
            .into_iter()
            .map(|(_, _, it)| it.guid),
    );
    out
}

impl NewItems {
    fn reconcile(&mut self, owned: &[u64]) {
        for miss in self.seen.values_mut() {
            *miss += 1;
        }
        for g in owned.iter().filter(|g| **g != 0) {
            self.seen.insert(*g, 0);
        }
        self.seen.retain(|_, miss| *miss < MAX_MISS);
    }

    /// The frame: re-arm on a new player, seed after the settle, then on each bag change flag
    /// the bag items never owned and drop the flags of items no longer owned.
    pub fn frame(&mut self, m: &Mirror, now: Instant) {
        if m.player == 0 {
            return;
        }
        if m.player != self.player {
            *self = Self {
                player: m.player,
                login: Some(now),
                ..Self::default()
            };
        }
        let bags = bag_guids(m);
        if !self.seeded {
            if self.login.is_some_and(|t| now.duration_since(t) < SETTLE) {
                return;
            }
            let mut owned = bags.clone();
            owned.extend(other_owned(m));
            self.seen.clear();
            self.reconcile(&owned);
            self.new.clear();
            self.last_bags = bags;
            self.seeded = true;
            return;
        }
        if bags == self.last_bags {
            return;
        }
        let mut changed = false;
        for g in &bags {
            if !self.seen.contains_key(g) && !self.new.contains(g) {
                self.new.push(*g);
                changed = true;
            }
        }
        let mut owned = bags.clone();
        owned.extend(other_owned(m));
        self.reconcile(&owned);
        let before = self.new.len();
        let seen = &self.seen;
        self.new.retain(|g| seen.contains_key(g));
        changed |= self.new.len() != before;
        self.last_bags = bags;
        self.fire |= changed;
    }
}

/// The item guid at `(bag, slot)`.
fn slot_guid(m: &Mirror, bag: &Value, slot: &Value) -> u64 {
    if !is_number(bag) || !is_number(slot) {
        return 0;
    }
    items::bag_slot(m, to_int(bag), to_int(slot)).unwrap_or(0)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.table(
        "C_NewItems",
        "IsNewItem",
        move |_, (b, s): (Value, Value)| {
            let st = c.lock();
            let g = slot_guid(&st.mirror, &b, &s);
            Ok(g != 0 && st.new_items.new.contains(&g))
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_NewItems",
        "RemoveNewItem",
        move |_, (b, s): (Value, Value)| {
            let mut st = c.lock();
            let g = slot_guid(&st.mirror, &b, &s);
            let n = &mut st.new_items;
            if let Some(i) = n.new.iter().position(|x| *x == g && g != 0) {
                n.new.swap_remove(i);
                n.fire = true;
            }
            Ok(())
        },
    )?;
    let c = api.ca.clone();
    api.table("C_NewItems", "ClearAll", move |_, ()| {
        let mut st = c.lock();
        if !st.new_items.new.is_empty() {
            st.new_items.new.clear();
            st.new_items.fire = true;
        }
        Ok(())
    })?;
    Ok(())
}

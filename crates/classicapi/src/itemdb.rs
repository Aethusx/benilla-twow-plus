//! `Item::PeekRecord` and `item/Data.cpp`: the item-stats records the natives read, and the load
//! tracking behind `GET_ITEM_INFO_RECEIVED` and `ITEM_DATA_LOAD_RESULT`.
//!
//! The DLL reads the engine's item-stats cache, whose record is the
//! `SMSG_ITEM_QUERY_SINGLE_RESPONSE` body; here each response is kept as benilla decodes it. A
//! miss asks benilla to query the template, as an uncached `GetItemInfo` does, and is tracked
//! until the answer (or a "no such item" reply) fires the event.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use benilla_protocol::ItemInfo;
use mlua::Lua;

/// A tracked miss gives up after this many frames (`kMaxWaitTicks`).
const MAX_WAIT_FRAMES: u32 = 1200;
const MAX_PENDING: usize = 512;

/// One template: the record, or the server's "no such item".
#[derive(Clone)]
pub enum Record {
    Found(Arc<ItemInfo>),
    Missing,
}

struct Pending {
    item: u32,
    frames: u32,
    /// `GET_ITEM_INFO_RECEIVED` (a warm-up) rather than `ITEM_DATA_LOAD_RESULT` (an explicit
    /// request).
    implicit: bool,
    asked: bool,
}

#[derive(Default)]
pub struct Db {
    records: HashMap<u32, Record>,
    pending: Vec<Pending>,
    /// Events for the frame to fire: `(event, item, success)`.
    fired: Vec<(&'static str, u32, bool)>,
    /// Item ids the frame should ask benilla for.
    asks: Vec<u32>,
}

/// The shared cache; also the Lua VM's app data, so a helper with only `&Lua` can read it.
#[derive(Clone, Default)]
pub struct ItemDb(Arc<Mutex<Db>>);

impl ItemDb {
    pub fn lock(&self) -> MutexGuard<'_, Db> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cache a native's VM carries.
    pub fn of(lua: &Lua) -> Option<Self> {
        lua.app_data_ref::<ItemDb>().map(|d| d.clone())
    }
}

fn event(implicit: bool) -> &'static str {
    if implicit {
        "GET_ITEM_INFO_RECEIVED"
    } else {
        "ITEM_DATA_LOAD_RESULT"
    }
}

impl Db {
    /// `PeekRecord`: the cached record, `None` on a miss or a "no such item".
    pub fn peek(&self, item: u32) -> Option<Arc<ItemInfo>> {
        match self.records.get(&item) {
            Some(Record::Found(r)) => Some(r.clone()),
            _ => None,
        }
    }

    pub fn cached(&self, item: u32) -> bool {
        self.peek(item).is_some()
    }

    /// `Track`: a cached explicit request answers at once; a miss is queued once, an explicit
    /// request upgrading a warm-up. False when the queue is full.
    pub fn track(&mut self, item: u32, implicit: bool) -> bool {
        if item == 0 {
            return false;
        }
        if self.cached(item) {
            if !implicit {
                self.fired.push((event(false), item, true));
            }
            return true;
        }
        if let Some(p) = self.pending.iter_mut().find(|p| p.item == item) {
            if !implicit {
                p.implicit = false;
            }
            return true;
        }
        if self.pending.len() >= MAX_PENDING {
            return false;
        }
        self.pending.push(Pending {
            item,
            frames: 0,
            implicit,
            asked: false,
        });
        true
    }

    /// `WarmCache`.
    pub fn warm(&mut self, item: u32) {
        self.track(item, true);
    }

    /// A template response: store it, and complete its pending entry.
    pub fn answer(&mut self, item: u32, info: Option<ItemInfo>) {
        let found = info.is_some();
        self.records.insert(
            item,
            match info {
                Some(i) => Record::Found(Arc::new(i)),
                None => Record::Missing,
            },
        );
        if let Some(i) = self.pending.iter().position(|p| p.item == item) {
            let p = self.pending.swap_remove(i);
            self.fired.push((event(p.implicit), item, found));
        }
    }

    /// The frame: ask for every unasked miss, time out the stale ones, hand back the events
    /// and the asks.
    pub fn tick(&mut self) -> (Vec<(&'static str, u32, bool)>, Vec<u32>) {
        let mut asks = std::mem::take(&mut self.asks);
        let mut fired = std::mem::take(&mut self.fired);
        self.pending.retain_mut(|p| {
            if !p.asked {
                p.asked = true;
                asks.push(p.item);
            }
            p.frames += 1;
            if p.frames >= MAX_WAIT_FRAMES {
                fired.push((event(p.implicit), p.item, false));
                return false;
            }
            true
        });
        (fired, asks)
    }
}

/// The record a helper with only the VM reads, warming the cache on a miss.
pub fn record(lua: &Lua, item: u32) -> Option<Arc<ItemInfo>> {
    let db = ItemDb::of(lua)?;
    let mut d = db.lock();
    let r = d.peek(item);
    if r.is_none() && item != 0 {
        d.warm(item);
    }
    r
}

/// The record without warming (`PeekRecord` alone).
pub fn peek(lua: &Lua, item: u32) -> Option<Arc<ItemInfo>> {
    ItemDb::of(lua)?.lock().peek(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_miss_is_asked_once_and_answered_by_its_event() {
        let mut d = Db::default();
        d.warm(6948);
        d.track(6948, false);
        let (fired, asks) = d.tick();
        assert!(fired.is_empty());
        assert_eq!(asks, vec![6948]);
        assert!(d.tick().1.is_empty());
        d.answer(6948, None);
        let (fired, _) = d.tick();
        assert_eq!(fired, vec![("ITEM_DATA_LOAD_RESULT", 6948, false)]);
    }
}

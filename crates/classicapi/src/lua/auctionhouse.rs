//! `auctionhouse/PostItem.cpp`: `C_AuctionHouse.PostItem(itemLocation, duration, quantity,
//! numStacks, bid, buyout)`, `numStacks` auctions of `quantity` each from one source stack, with
//! `AUCTION_MULTISELL_START`, `_UPDATE(done, total)` and `_FAILURE`.
//!
//! 1.12's sell packet has no count: the server posts a whole item object. So each partial stack is
//! split off into a free general bag slot first, then posted once the split shows; the final
//! exact stack posts the source itself. A step at a time across frames, through the stock verbs
//! captured at install (`SplitContainerItem`, `PickupContainerItem`, `ClickAuctionSellItemButton`,
//! `StartAuction`), each transition keyed on the bags: a post is confirmed when its item leaves
//! the slot. A closed auction house, a changed source or a step over ten seconds fails the job.

use std::time::{Duration, Instant};

use benilla_ui::script::UiScript;
use mlua::{Function, Lua, MultiValue, Value};

use crate::items;
use crate::lua::{is_number, to_int, Api};
use crate::mirror::field;
use crate::Ca;

const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// The auction durations in minutes, `duration` 1-3.
const DURATIONS: [i64; 3] = [120, 480, 1440];
const REG_VERBS: &str = "__classicapi_auction_verbs";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    SplitWait,
    PostWait,
}

/// One multi-post job.
#[derive(Clone, Debug)]
pub(crate) struct Job {
    src: (i64, i64),
    item: u32,
    quantity: u32,
    total: u32,
    posted: u32,
    bid: i64,
    buyout: i64,
    run_time: i64,
    phase: Phase,
    work: (i64, i64),
    post_guid: u64,
    since: Instant,
}

/// The job in flight, if any.
#[derive(Default)]
pub(crate) struct AuctionPost {
    job: Option<Job>,
}

/// `MapDuration`: 1-3, or the minutes themselves.
fn run_time(d: i64) -> Option<i64> {
    match d {
        1..=3 => Some(DURATIONS[d as usize - 1]),
        d if DURATIONS.contains(&d) => Some(d),
        _ => None,
    }
}

/// A bag slot's `(item, count, guid)`.
fn slot(ca: &Ca, (bag, s): (i64, i64)) -> Option<(u32, u32, u64)> {
    let st = ca.lock();
    let guid = items::bag_slot(&st.mirror, bag, s)?;
    let count = st
        .mirror
        .object(guid)
        .map_or(1, |f| f.u32(field::ITEM_STACK_COUNT).max(1));
    Some((items::item_id(&st.mirror, guid), count, guid))
}

/// `FindFreeGeneralSlot`: an empty slot in the backpack or a general bag.
fn free_general_slot(ca: &Ca) -> Option<(i64, i64)> {
    let general = |bag: i64| -> bool {
        if bag == 0 {
            return true;
        }
        let entry = {
            let st = ca.lock();
            items::bag_object(&st.mirror, bag).map(|f| f.entry())
        };
        entry
            .and_then(|e| ca.items.lock().peek(e))
            .is_some_and(|r| r.bag_family == 0)
    };
    (0..=4).filter(|b| general(*b)).find_map(|bag| {
        let n = items::bag_slot_count(&ca.lock().mirror, bag) as i64;
        (1..=n)
            .find(|s| slot(ca, (bag, *s)).is_none())
            .map(|s| (bag, s))
    })
}

fn verb(lua: &Lua, name: &str) -> Option<Function> {
    let t: mlua::Table = lua.named_registry_value(REG_VERBS).ok()?;
    t.get(name).ok()
}

/// `PostStack`: the item onto the sell slot, then `StartAuction`.
fn post(lua: &Lua, job: &Job, at: (i64, i64)) -> mlua::Result<()> {
    for (name, args) in [
        (
            "PickupContainerItem",
            MultiValue::from_vec(vec![Value::Integer(at.0), Value::Integer(at.1)]),
        ),
        ("ClickAuctionSellItemButton", MultiValue::new()),
        (
            "StartAuction",
            MultiValue::from_vec(vec![
                Value::Integer(job.bid),
                Value::Integer(job.buyout),
                Value::Integer(job.run_time),
            ]),
        ),
    ] {
        if let Some(f) = verb(lua, name) {
            f.call::<()>(args)?;
        }
    }
    Ok(())
}

/// The frame's step of the job; the events it fires.
pub(crate) fn tick(ca: &Ca, script: &mut UiScript, now: Instant) {
    let Some(mut job) = ca.lock().auctionpost.job.clone() else {
        return;
    };
    let lua = script.lua();
    let fire = |name: &str, args| benilla_ui::script::ext_read::fire_event(lua, name, args);
    let fail = |ca: &Ca| {
        ca.lock().auctionpost.job = None;
        fire("AUCTION_MULTISELL_FAILURE", vec![]);
    };
    if !benilla_ui::script::ext_read::auction_open(lua) {
        return fail(ca);
    }
    match job.phase {
        Phase::Idle => {
            if job.posted >= job.total {
                ca.lock().auctionpost.job = None;
                return;
            }
            match slot(ca, job.src) {
                Some((item, count, guid)) if item == job.item && count >= job.quantity => {
                    if count == job.quantity {
                        job.work = job.src;
                        job.post_guid = guid;
                        if post(lua, &job, job.src).is_err() {
                            return fail(ca);
                        }
                        job.phase = Phase::PostWait;
                    } else {
                        let Some(free) = free_general_slot(ca) else {
                            return fail(ca);
                        };
                        let split = verb(lua, "SplitContainerItem")
                            .map(|f| f.call::<()>((job.src.0, job.src.1, job.quantity)));
                        let place = verb(lua, "PickupContainerItem")
                            .map(|f| f.call::<()>((free.0, free.1)));
                        if !matches!((split, place), (Some(Ok(())), Some(Ok(())))) {
                            return fail(ca);
                        }
                        job.work = free;
                        job.phase = Phase::SplitWait;
                    }
                    job.since = now;
                }
                _ => return fail(ca),
            }
        }
        Phase::SplitWait => match slot(ca, job.work) {
            Some((item, count, guid)) if item == job.item && count == job.quantity => {
                job.post_guid = guid;
                if post(lua, &job, job.work).is_err() {
                    return fail(ca);
                }
                job.phase = Phase::PostWait;
                job.since = now;
            }
            _ if now.duration_since(job.since) > STEP_TIMEOUT => return fail(ca),
            _ => {}
        },
        Phase::PostWait => {
            let still = slot(ca, job.work).is_some_and(|(_, _, g)| g == job.post_guid);
            if !still {
                job.posted += 1;
                job.phase = Phase::Idle;
                job.since = now;
                fire(
                    "AUCTION_MULTISELL_UPDATE",
                    vec![
                        benilla_ui::script::ScriptValue::Int(i64::from(job.posted)),
                        benilla_ui::script::ScriptValue::Int(i64::from(job.total)),
                    ],
                );
            } else if now.duration_since(job.since) > STEP_TIMEOUT {
                return fail(ca);
            }
        }
    }
    let mut st = ca.lock();
    if st.auctionpost.job.is_some() {
        st.auctionpost.job = Some(job);
    }
}

fn reject(lua: &Lua, reason: &str) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_vec(vec![
        Value::Boolean(false),
        Value::String(lua.create_string(reason)?),
    ]))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let verbs = api.lua.create_table()?;
    for name in [
        "SplitContainerItem",
        "PickupContainerItem",
        "ClickAuctionSellItemButton",
        "StartAuction",
    ] {
        if let Ok(f) = api.lua.globals().get::<Function>(name) {
            verbs.set(name, f)?;
        }
    }
    api.lua.set_named_registry_value(REG_VERBS, verbs)?;
    let c = api.ca.clone();
    api.table(
        "C_AuctionHouse",
        "PostItem",
        move |lua, (loc, d, q, n, bid, buyout): (Value, Value, Value, Value, Value, Value)| {
            const USAGE: &str =
                "Usage: C_AuctionHouse.PostItem(itemLocation, duration, quantity, numStacks, bid, buyout)";
            let Value::Table(loc) = loc else {
                return Err(mlua::Error::runtime(USAGE));
            };
            if ![&d, &q, &n, &bid].iter().all(|v| is_number(v)) {
                return Err(mlua::Error::runtime(USAGE));
            }
            let (bag, s): (Value, Value) = (loc.get("bagID")?, loc.get("slotIndex")?);
            if !is_number(&bag) || !is_number(&s) {
                return reject(lua, "itemLocation must be a { bagID, slotIndex } table");
            }
            let src = (to_int(&bag), to_int(&s));
            let (quantity, stacks, bid) = (to_int(&q), to_int(&n), to_int(&bid));
            let buyout = if is_number(&buyout) { to_int(&buyout) } else { 0 };
            let Some(run_time) = run_time(to_int(&d)) else {
                return reject(lua, "invalid duration (use 1/2/3 or 120/480/1440)");
            };
            if !(1..=255).contains(&quantity) {
                return reject(lua, "quantity must be 1..255");
            }
            if stacks < 1 {
                return reject(lua, "numStacks must be >= 1");
            }
            if bid < 1 {
                return reject(lua, "bid must be >= 1");
            }
            if !benilla_ui::script::ext_read::auction_open(lua) {
                return reject(lua, "auction house is not open");
            }
            if c.lock().auctionpost.job.is_some() {
                return reject(lua, "a multi-post is already in progress");
            }
            let Some((item, count, _)) = slot(&c, src) else {
                return reject(lua, "source slot is empty");
            };
            if i64::from(count) < quantity * stacks {
                return reject(lua, "not enough items for numStacks x quantity");
            }
            if i64::from(count) > quantity && free_general_slot(&c).is_none() {
                return reject(lua, "need at least one free general bag slot to split");
            }
            c.lock().auctionpost.job = Some(Job {
                src,
                item,
                quantity: quantity as u32,
                total: stacks as u32,
                posted: 0,
                bid,
                buyout,
                run_time,
                phase: Phase::Idle,
                work: src,
                post_guid: 0,
                since: Instant::now(),
            });
            benilla_ui::script::ext_read::fire_event(lua, "AUCTION_MULTISELL_START", vec![]);
            Ok(MultiValue::from_vec(vec![Value::Boolean(true)]))
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations_map_and_bad_calls_reject() {
        assert_eq!(super::run_time(2), Some(480));
        assert_eq!(super::run_time(1440), Some(1440));
        assert_eq!(super::run_time(7), None);
        let script = crate::lua::test_support::vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local loc = { bagID = 0, slotIndex = 1 }
                local a, why = C_AuctionHouse.PostItem(loc, 9, 5, 1, 100)
                local b, closed = C_AuctionHouse.PostItem(loc, 1, 5, 1, 100)
                local ok = pcall(C_AuctionHouse.PostItem, 5, 1, 1, 1, 1)
                return tostring(a) .. ":" .. why .. " " .. tostring(b) .. ":" .. closed .. " " .. tostring(ok)
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            "false:invalid duration (use 1/2/3 or 120/480/1440) false:auction house is not open false"
        );
    }
}

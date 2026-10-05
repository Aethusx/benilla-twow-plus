//! `merchant/Frame.cpp` and `item/Lock.cpp`: `C_MerchantFrame` over the open vendor
//! (`ext_read::merchant`), and the client-side item lock (`C_Item.LockItem`, `LockItemByGUID`,
//! `UnlockItem`, `UnlockAllItems`) over benilla's [`benilla_app::ext::ExtItemLock`].
//!
//! - `SellAllJunkItems` queues every grey item in bags 0-4 and sells one a frame through the
//!   stock `UseContainerItem` captured at install (an addon's hook on it does not run), as a burst
//!   loses sells to the server's flood throttle; a closed or changed vendor drops the rest.
//! - A lock is a UI hint the server knows nothing of: it holds until unlocked or the slot
//!   changes. Deviation: benilla's lock table is keyed by bag slot, so an item outside the bags
//!   (equipped, in the trade window or the mail) cannot be locked.

use std::collections::VecDeque;

use benilla_app::ext::ExtItemLock;
use benilla_ui::script::UiScript;
use mlua::{Function, Lua, Value};

use crate::items;
use crate::lua::{as_string, is_number, to_int, Api};
use crate::mirror::field;
use crate::Ca;

/// The registry slot of the stock `UseContainerItem`, captured at install.
const REG_USE_CONTAINER: &str = "__classicapi_use_container_item";

/// The sells and locks the natives queued.
#[derive(Default)]
pub(crate) struct Merchant {
    /// `(vendor guid, bag, slot, item guid)` still to sell.
    sells: VecDeque<(u64, i64, u32, u64)>,
    pub(crate) locks: Vec<ExtItemLock>,
}

fn vendor_guid(lua: &Lua) -> u64 {
    benilla_ui::script::ext_read::unit_guid(lua, "npc")
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// The grey items in bags 0-4 as `(bag, slot, guid)`.
fn junk(ca: &Ca) -> Vec<(i64, u32, u64)> {
    let db = ca.items.clone();
    let st = ca.lock();
    let m = &st.mirror;
    let mut out = Vec::new();
    for bag in 0..=4 {
        for slot in 1..=items::bag_slot_count(m, bag) as u32 {
            let Some(guid) = items::bag_slot(m, bag, i64::from(slot)) else {
                continue;
            };
            let id = items::item_id(m, guid);
            if db.lock().peek(id).is_some_and(|r| r.quality == 0) {
                out.push((bag, slot, guid));
            }
        }
    }
    out
}

/// One queued sell a frame: the next item, while the same vendor is open and the item still sits
/// where it was queued.
pub(crate) fn drain(ca: &Ca, script: &mut UiScript) {
    let lua = script.lua();
    let next = ca.lock().merchant.sells.pop_front();
    let Some((vendor, bag, slot, guid)) = next else {
        return;
    };
    let open = benilla_ui::script::ext_read::merchant(lua).is_some();
    if !open || vendor_guid(lua) != vendor {
        ca.lock().merchant.sells.clear();
        return;
    }
    let still = items::bag_slot(&ca.lock().mirror, bag, i64::from(slot)) == Some(guid);
    if still {
        if let Ok(f) = lua.named_registry_value::<Function>(REG_USE_CONTAINER) {
            let _ = f.call::<()>((bag, slot));
        }
    }
}

/// The bag slot, guid and stack count an item location names, bags only.
fn bag_item(ca: &Ca, v: &Value) -> Option<(i64, u32, u64, u32)> {
    let st = ca.lock();
    let found = items::resolve_location(&st.mirror, v)?;
    if found.equipment != 0 {
        return None;
    }
    let count = found.item.fields.u32(field::ITEM_STACK_COUNT).max(1);
    Some((found.bag, found.slot as u32, found.item.guid, count))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    if let Ok(f) = api.lua.globals().get::<Function>("UseContainerItem") {
        api.lua.set_named_registry_value(REG_USE_CONTAINER, f)?;
    }
    const NS: &str = "C_MerchantFrame";
    api.table(NS, "GetBuybackItemID", |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_MerchantFrame.GetBuybackItemID(slot)",
            ));
        }
        let m = benilla_ui::script::ext_read::merchant(lua);
        Ok(usize::try_from(to_int(&v) - 1)
            .ok()
            .and_then(|i| m?.buyback.get(i).map(|r| r.item_id))
            .filter(|id| *id != 0))
    })?;
    api.table(NS, "GetItemInfo", |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_MerchantFrame.GetItemInfo(slot)",
            ));
        }
        let m = benilla_ui::script::ext_read::merchant(lua);
        let Some(row) = usize::try_from(to_int(&v) - 1)
            .ok()
            .and_then(|i| m?.items.get(i).cloned())
            .filter(|r| r.item_id != 0)
        else {
            return Ok(Value::Nil);
        };
        let t = lua.create_table()?;
        t.set("itemID", row.item_id)?;
        t.set("price", row.price)?;
        t.set("stackCount", row.quantity)?;
        t.set("numAvailable", row.num_available)?;
        t.set("isPurchasable", true)?;
        t.set("isThrottled", false)?;
        t.set("hasExtendedCost", false)?;
        Ok(Value::Table(t))
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetNumJunkItems", move |lua, ()| {
        if benilla_ui::script::ext_read::merchant(lua).is_none() {
            return Ok(0);
        }
        Ok(junk(&c).len())
    })?;
    api.table(NS, "IsMerchantItemRefundable", |_, _: Value| Ok(false))?;
    api.table(NS, "IsSellAllJunkEnabled", |_, ()| Ok(true))?;
    let c = api.ca.clone();
    api.table(NS, "SellAllJunkItems", move |lua, ()| {
        if benilla_ui::script::ext_read::merchant(lua).is_none() {
            return Ok(());
        }
        let vendor = vendor_guid(lua);
        let junk = junk(&c);
        let mut st = c.lock();
        st.merchant.sells.clear();
        st.merchant
            .sells
            .extend(junk.into_iter().map(|(b, s, g)| (vendor, b, s, g)));
        Ok(())
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "LockItem", move |_, v: Value| {
        crate::lua::item::location_arg(&v, "Usage: C_Item.LockItem(itemLocation)")?;
        if let Some((bag, slot, guid, count)) = bag_item(&c, &v) {
            c.lock().merchant.locks.push(ExtItemLock::Lock {
                bag,
                slot,
                guid,
                count,
            });
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table("C_Item", "LockItemByGUID", move |_, v: Value| {
        let Some(s) = as_string(&v) else {
            return Err(mlua::Error::runtime(
                "Usage: C_Item.LockItemByGUID(itemGUID)",
            ));
        };
        let Some(guid) = crate::guid::parse(&s).filter(|g| *g != 0) else {
            return Ok(());
        };
        let found = {
            let st = c.lock();
            items::Carried::of(&st.mirror)
                .find_guid(guid)
                .filter(|f| f.equipment == 0)
                .map(|f| {
                    let count = f.item.fields.u32(field::ITEM_STACK_COUNT).max(1);
                    (f.bag, f.slot as u32, count)
                })
        };
        if let Some((bag, slot, count)) = found {
            c.lock().merchant.locks.push(ExtItemLock::Lock {
                bag,
                slot,
                guid,
                count,
            });
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table("C_Item", "UnlockItem", move |_, v: Value| {
        crate::lua::item::location_arg(&v, "Usage: C_Item.UnlockItem(itemLocation)")?;
        let guid = items::resolve_location(&c.lock().mirror, &v).map(|f| f.item.guid);
        if let Some(guid) = guid {
            c.lock().merchant.locks.push(ExtItemLock::Unlock(guid));
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.table("C_Item", "UnlockAllItems", move |_, ()| {
        c.lock().merchant.locks.push(ExtItemLock::UnlockAll);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use benilla_ui::script::{MerchantItem, MerchantState};

    #[test]
    fn the_vendor_reads_in_retail_shape_and_locks_queue() {
        let ca = crate::Ca::default();
        let mut script = crate::lua::test_support::vm(&ca);
        script.set_merchant(Some(MerchantState {
            items: vec![MerchantItem {
                item_id: 159,
                price: 25,
                quantity: 5,
                num_available: -1,
                ..Default::default()
            }],
            buyback: vec![MerchantItem {
                item_id: 4865,
                ..Default::default()
            }],
            can_repair: false,
        }));
        let out: String = script
            .lua()
            .load(
                r#"
                local M = C_MerchantFrame
                local i = M.GetItemInfo(1)
                C_Item.UnlockAllItems()
                return i.itemID .. i.price .. i.stackCount .. i.numAvailable .. tostring(i.isPurchasable)
                  .. " " .. M.GetBuybackItemID(1) .. tostring(M.GetBuybackItemID(2))
                  .. " " .. M.GetNumJunkItems() .. tostring(M.IsSellAllJunkEnabled())
                  .. tostring(M.GetItemInfo(2))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "159255-1true 4865nil 0truenil");
        assert_eq!(
            ca.lock().merchant.locks,
            vec![benilla_app::ext::ExtItemLock::UnlockAll]
        );
    }
}

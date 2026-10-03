//! `container/`: `C_Container`, the modern bag-slot API over the carried items' descriptors and
//! the template records. The moves go out through the stock verbs ([`Verbs`]).

use mlua::{IntoLuaMulti, Lua, Value};

use super::item::actions::Verbs;
use super::item::{self, arg_id};
use crate::items::{self, ItemSnap, BANK_CONTAINER};
use crate::lua::{is_number, none, to_int, truthy, Api};
use crate::mirror::{field, Mirror};
use crate::Ca;

/// The hearthstone: the item, or any item whose use spell is the hearth.
const HEARTHSTONE_ITEM: u32 = 6948;
const HEARTHSTONE_SPELL: u32 = 8690;
const ITEM_CLASS_QUEST: u32 = 12;
const ITEM_FLAG_SOULBOUND: u32 = 0x01;
const TEMPLATE_FLAG_LOOTABLE: u32 = 0x10;
/// `PLAYER_QUEST_LOG_1_1`'s 20 entries, 3 fields each.
const QUEST_LOG_SLOTS: usize = 20;

/// Two numbers, else the usage error.
fn bag_slot_args(b: &Value, s: &Value, usage: &str) -> mlua::Result<(i64, i64)> {
    if is_number(b) && is_number(s) {
        Ok((to_int(b), to_int(s)))
    } else {
        Err(mlua::Error::runtime(usage.to_string()))
    }
}

fn at(ca: &Ca, bag: i64, slot: i64) -> Option<ItemSnap> {
    let st = ca.lock();
    items::bag_slot(&st.mirror, bag, slot).and_then(|g| ItemSnap::of(&st.mirror, g))
}

/// `Quest::Log::IsOnQuest`: the quest is in the player's log.
fn on_quest(m: &Mirror, quest: u32) -> bool {
    m.me().is_some_and(|f| {
        (0..QUEST_LOG_SLOTS).any(|i| f.u32(field::PLAYER_QUEST_LOG_1_1 + 3 * i) == quest)
    })
}

/// `ResolveBagInfo`: a bag's slot count and family, from the equipped bag's template.
fn bag_info(lua: &Lua, ca: &Ca, bag: i64) -> (usize, u32) {
    if bag == 0 {
        return (items::BACKPACK_NUM_SLOTS, 0);
    }
    if !(1..=4).contains(&bag) {
        return (0, 0);
    }
    let entry = {
        let st = ca.lock();
        items::equipment_slot(&st.mirror, items::INVSLOT_BAG1 as i64 + bag - 1)
            .map_or(0, |g| items::item_id(&st.mirror, g))
    };
    let turtle = ca.lock().auras.turtle;
    match crate::itemdb::peek(lua, entry).filter(|_| entry != 0) {
        Some(r) => (
            r.container_slots as usize,
            item::data::bag_family_mask(&r, turtle),
        ),
        None => (0, 0),
    }
}

fn free_slots(ca: &Ca, bag: i64, count: usize) -> Vec<i64> {
    let st = ca.lock();
    (1..=count as i64)
        .filter(|s| items::bag_slot(&st.mirror, bag, *s).is_none())
        .collect()
}

/// `FindHearthstone`: the first bagged hearthstone and its id.
fn hearthstone(lua: &Lua, ca: &Ca) -> Option<items::Found> {
    let bags = items::bagged(&ca.lock().mirror, 0..=4);
    bags.into_iter()
        .find(|(_, _, it)| {
            it.entry == HEARTHSTONE_ITEM
                || crate::itemdb::peek(lua, it.entry)
                    .is_some_and(|r| item::on_use_spell(&r) == HEARTHSTONE_SPELL)
        })
        .map(|(bag, slot, item)| items::Found {
            equipment: 0,
            bag,
            slot,
            item,
        })
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let verbs = Verbs::capture(&api.lua.globals());

    // `GetContainerItemInfo(bag, slot)`: the modern table; nil for an empty slot.
    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerItemInfo",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.GetContainerItemInfo(containerIndex, slotIndex)",
            )?;
            let Some(it) = at(&c, bag, slot) else {
                return Ok(Value::Nil);
            };
            let r = crate::itemdb::peek(lua, it.entry);
            let locked = {
                let out: mlua::MultiValue = lua
                    .globals()
                    .get::<mlua::Function>("GetContainerItemInfo")?
                    .call((bag, slot))?;
                truthy(&out.into_iter().nth(2).unwrap_or(Value::Nil))
            };
            let t = lua.create_table()?;
            if let Some(icon) = r
                .as_ref()
                .and_then(|r| item::icon_for_display(&c.db, r.display_info_id))
            {
                t.set("iconFileID", icon)?;
            }
            t.set("stackCount", it.stack())?;
            t.set("isLocked", locked)?;
            if let Some(r) = &r {
                t.set("quality", r.quality)?;
            }
            let readable = r.as_ref().is_some_and(|r| r.page_text != 0)
                || it.fields.u32(field::ITEM_TEXT_ID) != 0;
            t.set("isReadable", readable)?;
            t.set(
                "hasLoot",
                r.as_ref()
                    .is_some_and(|r| r.flags & TEMPLATE_FLAG_LOOTABLE != 0),
            )?;
            if let Some(link) = items::item_link(lua, &it) {
                t.set("hyperlink", link)?;
            }
            t.set("isFiltered", false)?;
            t.set("hasNoValue", r.as_ref().is_some_and(|r| r.sell_price == 0))?;
            t.set("itemID", it.entry)?;
            t.set(
                "isBound",
                it.fields.u32(field::ITEM_FLAGS) & ITEM_FLAG_SOULBOUND != 0,
            )?;
            let name = items::display_name(lua, it.entry, it.random_property())
                .or_else(|| r.as_ref().map(|r| r.name.clone()).filter(|n| !n.is_empty()));
            if let Some(n) = name {
                t.set("itemName", n)?;
            }
            Ok(Value::Table(t))
        },
    )?;

    // Free slots: `GetContainerNumFreeSlots(bag)` -> count, family; `GetContainerFreeSlots(bag)`
    // -> the slot list; the total over the plain (family 0) bags.
    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerNumFreeSlots",
        move |lua, b: Value| {
            if !is_number(&b) {
                return Err(mlua::Error::runtime(
                    "Usage: C_Container.GetContainerNumFreeSlots(bagID)",
                ));
            }
            let bag = to_int(&b);
            let (count, family) = bag_info(lua, &c, bag);
            Ok((free_slots(&c, bag, count).len(), family))
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerFreeSlots",
        move |lua, b: Value| {
            if !is_number(&b) {
                return Err(mlua::Error::runtime(
                    "Usage: C_Container.GetContainerFreeSlots(bagID)",
                ));
            }
            let bag = to_int(&b);
            let (count, _) = bag_info(lua, &c, bag);
            if count == 0 {
                return Ok(none());
            }
            let t = lua.create_sequence_from(free_slots(&c, bag, count))?;
            t.into_lua_multi(lua)
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_Container",
        "CalculateTotalNumberOfFreeBagSlots",
        move |lua, ()| {
            Ok((0..=4)
                .map(|bag| (bag, bag_info(lua, &c, bag)))
                .filter(|(_, (count, family))| *count > 0 && *family == 0)
                .map(|(bag, (count, _))| free_slots(&c, bag, count).len())
                .sum::<usize>())
        },
    )?;

    // `GetContainerItemQuestInfo(bag, slot)` -> `{isQuestItem, questID, isActive}`.
    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerItemQuestInfo",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.GetContainerItemQuestInfo(containerIndex, slotIndex)",
            )?;
            let entry = at(&c, bag, slot).map_or(0, |i| i.entry);
            let r = (entry != 0)
                .then(|| crate::itemdb::record(lua, entry))
                .flatten();
            let t = lua.create_table()?;
            t.set(
                "isQuestItem",
                r.as_ref().is_some_and(|r| r.class == ITEM_CLASS_QUEST),
            )?;
            let quest = r.as_ref().map_or(0, |r| r.start_quest);
            if quest > 0 {
                t.set("questID", quest)?;
                t.set("isActive", on_quest(&c.lock().mirror, quest))?;
            } else {
                t.set("isActive", false)?;
            }
            Ok(t)
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_Container",
        "PlayerHasHearthstone",
        move |lua, ()| match hearthstone(lua, &c) {
            Some(f) => f.item.entry.into_lua_multi(lua),
            None => Ok(none()),
        },
    )?;
    let (c, v) = (api.ca.clone(), verbs.clone());
    api.table("C_Container", "UseHearthstone", move |lua, ()| {
        let Some(f) = hearthstone(lua, &c) else {
            return Ok(false);
        };
        v.use_item(&f, None)?;
        Ok(true)
    })?;

    // `GetContainerItemEquipmentSetInfo(bag, slot)` -> inSet, the set names joined.
    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerItemEquipmentSetInfo",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.GetContainerItemEquipmentSetInfo(containerIndex, slotIndex)",
            )?;
            let guid = at(&c, bag, slot).map_or(0, |i| i.guid);
            let names = super::equipmentset::sets_containing(&c, guid);
            if names.is_empty() {
                (false, Value::Nil).into_lua_multi(lua)
            } else {
                (true, names.join(", ")).into_lua_multi(lua)
            }
        },
    )?;

    // `AutoStoreItem(srcBag, srcSlot [, dstBag])`: into the backpack or one bag; a bank source
    // to the bags and a bag source to the bank (`dstBag` -1) through the open bank.
    let (c, v) = (api.ca.clone(), verbs.clone());
    api.table(
        "C_Container",
        "AutoStoreItem",
        move |lua, (b, s, d): (Value, Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.AutoStoreItem(srcBag, srcSlot [, dstBag])",
            )?;
            let dst = if is_number(&d) { to_int(&d) } else { 0 };
            if at(&c, bag, slot).is_none() || v.cursor_busy()? {
                return Ok(false);
            }
            let src_bank = bag == BANK_CONTAINER || (5..=10).contains(&bag);
            if src_bank || dst == BANK_CONTAINER {
                if src_bank && dst != 0 {
                    return Ok(false);
                }
                if !bank_open(lua) {
                    return Ok(false);
                }
                v.use_container((bag, slot))?;
                return Ok(true);
            }
            if !(0..=4).contains(&dst) {
                return Ok(false);
            }
            v.auto_store((bag, slot), dst)?;
            Ok(true)
        },
    )?;

    // `GetContainerItemCharges(bag, slot)`: stack times the uses per item.
    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerItemCharges",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.GetContainerItemCharges(containerIndex, slotIndex)",
            )?;
            match at(&c, bag, slot) {
                Some(it) => {
                    let charges = it.fields.i32(field::ITEM_SPELL_CHARGES);
                    let per = if charges < 0 {
                        charges.unsigned_abs()
                    } else {
                        1
                    };
                    (it.stack() * per).into_lua_multi(lua)
                }
                None => Ok(none()),
            }
        },
    )?;

    let item_cooldown: mlua::Function = api.lua.globals().get("GetItemCooldown")?;
    api.table("C_Container", "GetItemCooldown", move |_, v: Value| {
        item_cooldown.call::<mlua::MultiValue>(arg_id(&v))
    })?;

    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerItemDurability",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.GetContainerItemDurability(containerIndex, slotIndex)",
            )?;
            match at(&c, bag, slot) {
                Some(it) if it.fields.u32(field::ITEM_MAX_DURABILITY) > 0 => (
                    it.fields.u32(field::ITEM_DURABILITY),
                    it.fields.u32(field::ITEM_MAX_DURABILITY),
                )
                    .into_lua_multi(lua),
                _ => Ok(none()),
            }
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_Container",
        "HasContainerItem",
        move |_, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.HasContainerItem(bagIndex, slotIndex)",
            )?;
            Ok(at(&c, bag, slot).is_some())
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerItemID",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.GetContainerItemID(bagIndex, slotIndex)",
            )?;
            match at(&c, bag, slot).map(|i| i.entry).filter(|e| *e != 0) {
                Some(e) => e.into_lua_multi(lua),
                None => Ok(none()),
            }
        },
    )?;

    // `MoveItem(srcBag, srcSlot, dstBag, dstSlot, count)`: a whole stack swaps, part of one splits.
    let (c, v) = (api.ca.clone(), verbs.clone());
    api.table(
        "C_Container",
        "MoveItem",
        move |_, args: mlua::Variadic<Value>| {
            let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Nil);
            if !(0..5).all(|i| is_number(&arg(i))) {
                return Err(mlua::Error::runtime(
                    "Usage: C_Container.MoveItem(srcBag, srcSlot, dstBag, dstSlot, count)",
                ));
            }
            let [sb, ss, db, ds, count] = [0, 1, 2, 3, 4].map(|i| to_int(&arg(i)));
            let Some(it) = at(&c, sb, ss) else {
                return Ok(false);
            };
            if !(1..=255).contains(&count)
                || count > i64::from(it.stack())
                || !valid_slot(&c, db, ds)
            {
                return Ok(false);
            }
            if v.cursor_busy()? {
                return Ok(false);
            }
            if count == i64::from(it.stack()) {
                v.swap_containers((sb, ss), (db, ds))?;
            } else {
                v.split((sb, ss), (db, ds), count)?;
            }
            Ok(true)
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_Container",
        "IsContainerItemOpenable",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.IsContainerItemOpenable(containerIndex, slotIndex)",
            )?;
            let Some(it) = at(&c, bag, slot) else {
                return Ok(none());
            };
            item::inventory::openable(lua, &it)
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_Container",
        "GetContainerItemRepairCost",
        move |lua, (b, s): (Value, Value)| {
            let (bag, slot) = bag_slot_args(
                &b,
                &s,
                "Usage: C_Container.GetContainerItemRepairCost(containerIndex, slotIndex)",
            )?;
            Ok(at(&c, bag, slot).map_or(0, |it| item::inventory::repair_cost(lua, &c, &it)))
        },
    )?;

    let (c, v) = (api.ca.clone(), verbs);
    api.table(
        "C_Container",
        "SwapItems",
        move |_, args: mlua::Variadic<Value>| {
            let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Nil);
            if !(0..4).all(|i| is_number(&arg(i))) {
                return Err(mlua::Error::runtime(
                    "Usage: C_Container.SwapItems(srcBag, srcSlot, dstBag, dstSlot)",
                ));
            }
            let [sb, ss, db, ds] = [0, 1, 2, 3].map(|i| to_int(&arg(i)));
            if at(&c, sb, ss).is_none() || !valid_slot(&c, db, ds) || v.cursor_busy()? {
                return Ok(false);
            }
            v.swap_containers((sb, ss), (db, ds))?;
            Ok(true)
        },
    )?;
    Ok(())
}

/// `EncodeBagSlot`'s range test: the slot exists in that container.
fn valid_slot(ca: &Ca, bag: i64, slot: i64) -> bool {
    slot >= 1 && slot as usize <= items::bag_slot_count(&ca.lock().mirror, bag)
}

/// The bank window is open: the stock frame shows.
fn bank_open(lua: &Lua) -> bool {
    lua.globals()
        .get::<mlua::Table>("BankFrame")
        .ok()
        .and_then(|f| f.get::<mlua::Function>("IsVisible").ok().map(|v| (f, v)))
        .and_then(|(f, v)| v.call::<Value>(f).ok())
        .is_some_and(|v| truthy(&v))
}

//! The carried items: `Count.cpp`, `Equipment.cpp`, `GUID.cpp`, `GetItemLocation.cpp`,
//! `Durability.cpp`, `RepairCost.cpp`, `InventoryID.cpp`, `InventoryItemsForSlot.cpp`,
//! `AverageLevel.cpp`, `Bound.cpp`, `Openable.cpp`, `Lock.cpp`, `StackCount.cpp`, `Cooldown.cpp`,
//! `WeaponEnchant.cpp`, `Usable.cpp` and `InRange.cpp`.

use std::time::{Duration, Instant};

use mlua::{IntoLuaMulti, Lua, MultiValue, Value};

use super::{arg_id, located, location_arg, record};
use crate::items::{self, ItemSnap};
use crate::lua::{is_number, none, to_int, to_str, truthy, Api};
use crate::mirror::{field, typemask, Mirror};
use crate::spells;
use crate::Ca;

/// `ITEM_FIELD_FLAGS`: soulbound, unlocked.
const ITEM_FLAG_SOULBOUND: u32 = 0x01;
const ITEM_FLAG_UNLOCKED: u32 = 0x04;
/// The template's openable flag.
const TEMPLATE_FLAG_OPENABLE: u32 = 0x4;
/// `ITEM_FIELD_ENCHANTMENT`'s temporary slot: id, duration, charges.
const TEMP_ENCHANT: usize = field::ITEM_ENCHANTMENT + 3;
const INVSLOT_MAINHAND: i64 = 16;
const INVSLOT_OFFHAND: i64 = 17;
const INVSLOT_RANGED: i64 = 18;
/// `PLAYER_VISIBLE_ITEM_*`: 12 fields a slot, the entry at +2.
const VISIBLE_ITEM_STRIDE: usize = 12;
/// How long an `IsUsableItem` keeps its spell's verdict refreshed.
const USABLE_INTEREST: Duration = Duration::from_secs(10);

/// The equipment slots each inventory type fits, bit `slot - 1` (`kSlotMaskByInvType`).
pub(crate) const SLOT_MASK_BY_INV_TYPE: [u32; 29] = [
    0x0000_0000,
    0x0000_0001,
    0x0000_0002,
    0x0000_0004,
    0x0000_0008,
    0x0000_0010,
    0x0000_0020,
    0x0000_0040,
    0x0000_0080,
    0x0000_0100,
    0x0000_0200,
    0x0000_0C00,
    0x0000_3000,
    0x0001_8000,
    0x0001_0000,
    0x0002_0000,
    0x0000_4000,
    0x0000_8000,
    0x0078_0000,
    0x0004_0000,
    0x0000_0010,
    0x0000_8000,
    0x0001_0000,
    0x0001_0000,
    0x0000_0000,
    0x0002_0000,
    0x0002_0000,
    0x0000_0000,
    0x0002_0000,
];

pub(crate) fn slot_mask(inv_type: u32) -> u32 {
    SLOT_MASK_BY_INV_TYPE
        .get(inv_type as usize)
        .copied()
        .unwrap_or(0)
}

/// `GetItemContribution`: the stack, or stack times uses for a charged item when asked.
fn contribution(item: &ItemSnap, uses: bool) -> u32 {
    let stack = item.stack();
    if !uses {
        return stack;
    }
    let charges = item.fields.i32(field::ITEM_SPELL_CHARGES);
    stack
        * if charges < 0 {
            charges.unsigned_abs()
        } else {
            1
        }
}

/// The bank: the main slots, then each bank bag.
fn banked(m: &Mirror) -> Vec<(i64, i64, ItemSnap)> {
    items::bagged(m, std::iter::once(items::BANK_CONTAINER).chain(5..=10))
}

/// FrameXML's `ITEM_INVENTORY_LOCATION_*` bits: player, in a bag, bank.
pub(crate) const LOC_PLAYER: i64 = 0x0010_0000;
pub(crate) const LOC_BAGS: i64 = 0x0020_0000;
pub(crate) const LOC_BANK: i64 = 0x0040_0000;

/// `EquipmentSet::PackEquipped`: `PLAYER | slot`.
pub(crate) fn pack_equipped(slot: i64) -> i64 {
    LOC_PLAYER | (slot & 0xFF)
}

/// `PackBag`: `PLAYER | BAGS | bag << 8 | slot`, bags 0-4.
pub(crate) fn pack_bag(bag: i64, slot: i64) -> i64 {
    LOC_PLAYER | LOC_BAGS | ((bag & 0xFF) << 8) | (slot & 0xFF)
}

/// `PackMainBank`: `BANK | slot`.
pub(crate) fn pack_main_bank(slot: i64) -> i64 {
    LOC_BANK | (slot & 0xFF)
}

/// `PackBankBag`: `BANK | BAGS | (bag - 4) << 8 | slot`, bags 5-10.
pub(crate) fn pack_bank_bag(bag: i64, slot: i64) -> i64 {
    LOC_BANK | LOC_BAGS | (((bag - 4) & 0xFF) << 8) | (slot & 0xFF)
}

/// The player's proficiency in an item's subclass; uncovered classes and an unannounced table pass.
fn proficient(lua: &Lua, class: u32, sub: u32) -> bool {
    benilla_ui::script::ext_read::proficiency_mask(lua, class)
        .is_none_or(|mask| mask == 0 || mask & (1 << sub) != 0)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `GetItemCount(item [, includeBank [, includeUses]])`: worn and bagged, the bank when
    // asked; a charged item's uses when asked.
    let c = api.ca.clone();
    api.table(
        "C_Item",
        "GetItemCount",
        move |lua, (v, bank, uses): (Value, Value, Value)| {
            let arg = items::resolve_arg(&v);
            if arg.item_id <= 0 && arg.name.as_deref().is_none_or(str::is_empty) {
                return Ok(0);
            }
            let (bank, uses) = (truthy(&bank), truthy(&uses));
            let (worn, bags, banked) = {
                let st = c.lock();
                let m = &st.mirror;
                (
                    items::equipped(m),
                    items::bagged(m, 0..=4),
                    if bank { banked(m) } else { Vec::new() },
                )
            };
            let total: u32 = worn
                .iter()
                .map(|(_, it)| it)
                .chain(bags.iter().map(|(_, _, it)| it))
                .chain(banked.iter().map(|(_, _, it)| it))
                .filter(|it| items::matches(lua, it, &arg))
                .map(|it| contribution(it, uses))
                .sum();
            Ok(total)
        },
    )?;

    let c = api.ca.clone();
    api.table("C_Item", "IsEquippedItem", move |lua, v: Value| {
        let arg = items::resolve_arg(&v);
        if arg.is_empty() {
            return Ok(false);
        }
        let worn = items::equipped(&c.lock().mirror);
        Ok(worn.iter().any(|(_, it)| items::matches(lua, it, &arg)))
    })?;

    // `OffhandHasWeapon()`: a one-hander or off-hand weapon in slot 17.
    let c = api.ca.clone();
    api.global("OffhandHasWeapon", move |lua, ()| {
        let entry = {
            let st = c.lock();
            items::equipment_slot(&st.mirror, INVSLOT_OFFHAND)
                .map_or(0, |g| items::item_id(&st.mirror, g))
        };
        Ok(entry != 0
            && crate::itemdb::peek(lua, entry).is_some_and(|r| matches!(r.inventory_type, 13 | 22)))
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "GetItemGUID", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.GetItemGUID(itemLocation)")?;
        match located(&c, &v).map(|i| i.guid).filter(|g| *g != 0) {
            Some(g) => crate::guid::format(g).into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "IsItemGUIDInInventory", move |_, v: Value| {
        let Value::String(s) = &v else {
            if is_number(&v) {
                return Ok(false);
            }
            return Err(mlua::Error::runtime(
                "Usage: C_Item.IsItemGUIDInInventory(\"itemGUID\")",
            ));
        };
        let Some(target) = crate::guid::parse(&s.to_string_lossy()).filter(|g| *g != 0) else {
            return Ok(false);
        };
        Ok(items::Carried::of(&c.lock().mirror)
            .find_guid(target)
            .is_some())
    })?;

    // `GetItemLocation(guid)`: `{equipmentSlotIndex}` or `{bagID, slotIndex}`.
    let c = api.ca.clone();
    api.table("C_Item", "GetItemLocation", move |lua, v: Value| {
        let Value::String(s) = &v else {
            return Err(mlua::Error::runtime(
                "Usage: C_Item.GetItemLocation(itemGUID)",
            ));
        };
        let Some(guid) = crate::guid::parse(&s.to_string_lossy()) else {
            return Ok(none());
        };
        let Some(found) = items::Carried::of(&c.lock().mirror).find_guid(guid) else {
            return Ok(none());
        };
        let t = lua.create_table()?;
        if found.equipment != 0 {
            t.set("equipmentSlotIndex", found.equipment)?;
        } else {
            t.set("bagID", found.bag)?;
            t.set("slotIndex", found.slot)?;
        }
        t.into_lua_multi(lua)
    })?;

    // `GetInventoryItemDurability(slot)`: current, max; nothing for an item without durability.
    let c = api.ca.clone();
    api.global("GetInventoryItemDurability", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: GetInventoryItemDurability(invSlot)",
            ));
        }
        let item = {
            let st = c.lock();
            items::equipment_slot(&st.mirror, to_int(&v)).and_then(|g| ItemSnap::of(&st.mirror, g))
        };
        match item {
            Some(i) if i.fields.u32(field::ITEM_MAX_DURABILITY) > 0 => (
                i.fields.u32(field::ITEM_DURABILITY),
                i.fields.u32(field::ITEM_MAX_DURABILITY),
            )
                .into_lua_multi(lua),
            _ => Ok(none()),
        }
    })?;

    // `GetInventoryItemRepairCost(slot)`: the engine's repair arithmetic (`0x4faf30`) at no
    // merchant discount.
    let c = api.ca.clone();
    api.global("GetInventoryItemRepairCost", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: GetInventoryItemRepairCost(invSlot)",
            ));
        }
        let item = {
            let st = c.lock();
            items::equipment_slot(&st.mirror, to_int(&v)).and_then(|g| ItemSnap::of(&st.mirror, g))
        };
        let Some(item) = item else {
            return Ok(0);
        };
        Ok(repair_cost(lua, &c, &item))
    })?;

    // `GetInventoryItemID(unit, slot)`: the player's own slot, or another player's visible item.
    let c = api.ca.clone();
    api.global("GetInventoryItemID", move |lua, (u, s): (Value, Value)| {
        let (Some(token), true) = (
            to_str(&u)
                .filter(|_| matches!(u, Value::String(_) | Value::Integer(_) | Value::Number(_))),
            is_number(&s),
        ) else {
            return Err(mlua::Error::runtime(
                "Usage: GetInventoryItemID(unit, slot)",
            ));
        };
        let slot = to_int(&s);
        if slot < 1 {
            return Ok(none());
        }
        let Some(guid) = super::super::unit::unit_guid(lua, &token)? else {
            return Ok(none());
        };
        let st = c.lock();
        let m = &st.mirror;
        let id = if guid == m.player {
            items::equipment_slot(m, slot).map_or(0, |g| items::item_id(m, g))
        } else if let Some(f) = m.object(guid).filter(|f| f.is(typemask::PLAYER)) {
            f.u32(field::PLAYER_VISIBLE_ITEM_1 + 2 + (slot as usize - 1) * VISIBLE_ITEM_STRIDE)
        } else {
            0
        };
        drop(st);
        if id == 0 {
            Ok(none())
        } else {
            id.into_lua_multi(lua)
        }
    })?;

    // `GetInventoryItemsForSlot(slot, returnTable)`: every worn or bagged item the slot takes
    // that the player is proficient with, keyed by packed location, valued by link.
    let c = api.ca.clone();
    api.global(
        "GetInventoryItemsForSlot",
        move |lua, (s, t, _): (Value, Value, Value)| {
            let (true, Value::Table(out)) = (is_number(&s), t) else {
                return Err(mlua::Error::runtime(
                    "Usage: GetInventoryItemsForSlot(slot, returnTable [, transmog])",
                ));
            };
            let slot = to_int(&s);
            if !(items::EQUIPMENT_FIRST..=items::EQUIPMENT_LAST).contains(&slot) {
                return Ok(out);
            }
            let carried = items::Carried::of(&c.lock().mirror);
            let candidates = carried
                .equipped
                .iter()
                .map(|(s, it)| (pack_equipped(*s), it))
                .chain(carried.bags.iter().map(|(b, s, it)| (pack_bag(*b, *s), it)));
            for (loc, it) in candidates {
                let Some(r) = crate::itemdb::peek(lua, it.entry) else {
                    continue;
                };
                if slot_mask(r.inventory_type) & (1 << (slot - 1)) == 0
                    || !proficient(lua, r.class, r.subclass)
                {
                    continue;
                }
                if let Some(link) = items::item_link(lua, it) {
                    out.set(loc, link)?;
                }
            }
            Ok(out)
        },
    )?;

    // `GetAverageItemLevel()`: overall (the best carried or banked item per slot, greedily by
    // item level) and equipped, each the better of with and without the off-hand.
    let c = api.ca.clone();
    api.global("GetAverageItemLevel", move |lua, ()| {
        let (worn, mut carried) = {
            let st = c.lock();
            let m = &st.mirror;
            let mut carried = items::bagged(m, 0..=4);
            carried.extend(banked(m));
            (items::equipped(m), carried)
        };
        Ok(average_item_level(lua, &worn, &mut carried))
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "IsBound", move |_, v: Value| {
        location_arg(&v, "Usage: C_Item.IsBound(itemLocation)")?;
        Ok(located(&c, &v)
            .is_some_and(|i| i.fields.u32(field::ITEM_FLAGS) & ITEM_FLAG_SOULBOUND != 0))
    })?;

    // `IsItemOpenable(location)`: openable, and openable now (no lock, or unlocked).
    let c = api.ca.clone();
    api.table("C_Item", "IsItemOpenable", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.IsItemOpenable(itemLocation)")?;
        match located(&c, &v) {
            Some(item) => openable(lua, &item),
            None => Ok(none()),
        }
    })?;

    // `IsLocked(location)`: the client-side lock the stock slot getters report.
    let c = api.ca.clone();
    api.table("C_Item", "IsLocked", move |lua, v: Value| {
        if !items::is_location_arg(&v) {
            return Ok(false);
        }
        let Some(found) = items::resolve_location(&c.lock().mirror, &v) else {
            return Ok(false);
        };
        let g = lua.globals();
        let locked: Value = if found.equipment != 0 {
            g.get::<mlua::Function>("IsInventoryItemLocked")?
                .call(found.equipment)?
        } else {
            let out: MultiValue = g
                .get::<mlua::Function>("GetContainerItemInfo")?
                .call((found.bag, found.slot))?;
            out.into_iter().nth(2).unwrap_or(Value::Nil)
        };
        Ok(truthy(&locked))
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "GetStackCount", move |_, v: Value| {
        if !items::is_location_arg(&v) {
            return Ok(0);
        }
        Ok(located(&c, &v).map_or(0, |i| i.stack()))
    })?;

    // `GetItemCooldown(item)`: the use spell's cooldown, `(0, 0, 1)` when cold.
    let c = api.ca.clone();
    api.global("GetItemCooldown", move |lua, v: Value| {
        let id = arg_id(&v);
        let spell = record(lua, id).map_or(0, |r| super::on_use_spell(&r));
        let rec_table = spells::table(&c.db);
        let rec = rec_table.as_ref().and_then(|t| t.row(spell));
        let st = c.lock();
        let read = crate::cooldown::query(
            &st.mirror,
            spell,
            id.max(0) as u32,
            rec.as_ref(),
            Instant::now(),
        );
        let (start, duration, enabled) = read.triple(&st.mirror);
        Ok((start, duration, u8::from(enabled)))
    })?;

    // `GetWeaponEnchantInfo()`: the temporary enchantment on main hand, off hand and ranged:
    // `has, expiration ms, charges, enchant id` each.
    let c = api.ca.clone();
    api.table("C_Item", "GetWeaponEnchantInfo", move |lua, ()| {
        let st = c.lock();
        let m = &st.mirror;
        let mut out = Vec::new();
        for slot in [INVSLOT_MAINHAND, INVSLOT_OFFHAND, INVSLOT_RANGED] {
            let e = items::equipment_slot(m, slot)
                .and_then(|g| m.object(g))
                .map(temp_enchant);
            let (has, exp, charges, id) = e.unwrap_or((false, 0, 0, 0));
            out.extend([
                Value::Boolean(has),
                Value::Integer(exp.into()),
                Value::Integer(charges.into()),
                Value::Integer(id.into()),
            ]);
        }
        drop(st);
        let _ = lua;
        Ok(MultiValue::from_vec(out))
    })?;
    let c = api.ca.clone();
    api.table("C_Item", "GetItemTempEnchantInfo", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.GetItemTempEnchantInfo(itemLocation)")?;
        let (has, exp, charges, id) =
            located(&c, &v).map_or((false, 0, 0, 0), |i| temp_enchant(&i.fields));
        (has, exp, charges, id).into_lua_multi(lua)
    })?;

    // `IsUsableItem(item)`: the use spell's usable test after the level, class and race gates;
    // `1, nil`, `nil, 1` (no mana) or `nil, nil`. The first ask arms the verdict.
    let c = api.ca.clone();
    let usable = move |lua: &Lua, v: &Value| -> (bool, bool) {
        let Some(r) = record(lua, arg_id(v)).filter(|_| arg_id(v) > 0) else {
            return (false, false);
        };
        let spell = super::on_use_spell(&r);
        if spell == 0 {
            return (false, false);
        }
        let mut st = c.lock();
        let m = &mut st.mirror;
        let Some(me) = m.me() else {
            return (false, false);
        };
        let restricted = |mask: i32, index: u8| {
            let mask = mask as u32;
            mask != 0 && mask != u32::MAX && index != 0 && mask & (1 << (index - 1)) == 0
        };
        if r.required_level > me.level()
            || restricted(r.allowable_class, me.class())
            || restricted(r.allowable_race, me.race())
        {
            return (false, false);
        }
        if !m.usable_extra.contains(&spell) {
            m.usable_extra.push(spell);
        }
        m.usable_wanted_until = Some(Instant::now() + USABLE_INTEREST);
        m.usable.get(&spell).copied().unwrap_or((false, false))
    };
    let u = usable.clone();
    api.global("IsUsableItem", move |lua, v: Value| {
        let (ok, oom) = u(lua, &v);
        Ok(if ok {
            (Some(1), None)
        } else if oom {
            (None, Some(1))
        } else {
            (None, None)
        })
    })?;
    api.table("C_Item", "IsUsableItem", move |lua, v: Value| {
        Ok(usable(lua, &v))
    })?;

    // `IsItemInRange(item [, unit])`: the use spell's range from the player; nil when it has none.
    let c = api.ca.clone();
    api.table(
        "C_Item",
        "IsItemInRange",
        move |lua, (v, u): (Value, Value)| {
            let spell = record(lua, arg_id(&v)).map_or(0, |r| super::on_use_spell(&r));
            let token = to_str(&u).filter(|_| matches!(u, Value::String(_)));
            let guid = token.and_then(|t| benilla_ui::script::unit_token_guid_in(lua, &t));
            Ok(crate::lua::spell::data::player_vs_unit(
                &c,
                i64::from(spell),
                guid,
            ))
        },
    )?;
    Ok(())
}

/// The temporary enchantment slot: `(has, duration, charges, id)`.
fn temp_enchant(f: &crate::mirror::Fields) -> (bool, u32, u32, u32) {
    let id = f.u32(TEMP_ENCHANT);
    let exp = f.u32(TEMP_ENCHANT + 1);
    (id != 0 && exp > 0, exp, f.u32(TEMP_ENCHANT + 2), id)
}

/// `PushIsItemOpenable`: openable, and openable now (no lock, or unlocked); nothing uncached.
pub(crate) fn openable(lua: &Lua, item: &ItemSnap) -> mlua::Result<MultiValue> {
    let Some(r) = record(lua, i64::from(item.entry)) else {
        return Ok(none());
    };
    let openable = r.flags & TEMPLATE_FLAG_OPENABLE != 0;
    let unlocked = item.fields.u32(field::ITEM_FLAGS) & ITEM_FLAG_UNLOCKED != 0;
    (openable, openable && (r.lock_id == 0 || unlocked)).into_lua_multi(lua)
}

/// `0x4faf30` for one item, discount 0.
pub(crate) fn repair_cost(lua: &Lua, ca: &Ca, item: &ItemSnap) -> u32 {
    let cur = item.fields.u32(field::ITEM_DURABILITY);
    let max = item.fields.u32(field::ITEM_MAX_DURABILITY);
    if max == 0 || cur >= max {
        return 0;
    }
    let Some(r) = crate::itemdb::peek(lua, item.entry) else {
        return 0;
    };
    let Some(tables) = ca.db.catalog(benilla_formats::load_durability_tables) else {
        return 0;
    };
    tables.repair_cost(max - cur, r.item_level, r.quality, r.class, r.subclass, 0.0)
}

/// `Script_GetAverageItemLevel`'s two averages over slots 1-19 less the shirt and tabard.
fn average_item_level(
    lua: &Lua,
    worn: &[(i64, ItemSnap)],
    carried: &mut [(i64, i64, ItemSnap)],
) -> (f64, f64) {
    let excluded = |s: i64| s == 4 || s == 19;
    let info = |it: &ItemSnap| {
        crate::itemdb::record(lua, it.entry).map(|r| (r.item_level, r.inventory_type))
    };
    let mut best = [0u32; 20];
    let mut equipped = [0u32; 20];
    for (slot, it) in worn {
        if excluded(*slot) || !(1..=19).contains(slot) {
            continue;
        }
        if let Some((ilvl, _)) = info(it).filter(|i| i.0 > 0) {
            equipped[*slot as usize] = ilvl;
            best[*slot as usize] = ilvl;
        }
    }
    let mut candidates: Vec<(u32, u32)> = carried
        .iter()
        .filter_map(|(_, _, it)| info(it))
        .filter(|(ilvl, _)| *ilvl > 0)
        .filter_map(|(ilvl, inv)| {
            let mut mask = slot_mask(inv);
            for s in [4, 19] {
                mask &= !(1 << (s - 1));
            }
            (mask != 0).then_some((ilvl, mask))
        })
        .collect();
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    for (ilvl, mask) in candidates {
        let target = (1..=19usize)
            .filter(|s| mask & (1 << (s - 1)) != 0)
            .min_by_key(|s| best[*s]);
        if let Some(s) = target {
            if ilvl > best[s] {
                best[s] = ilvl;
            }
        }
    }
    let slots: Vec<usize> = (1..=19).filter(|s| !excluded(*s as i64)).collect();
    let n = slots.len() as f64;
    let avg = |vals: &[u32; 20]| {
        let sum: u64 = slots.iter().map(|s| u64::from(vals[*s])).sum();
        let all = sum as f64 / n;
        let no_oh = (sum - u64::from(vals[17])) as f64 / (n - 1.0);
        all.max(no_oh)
    };
    (avg(&best), avg(&equipped))
}

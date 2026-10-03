//! `Equipment.cpp`'s `EquipItemByName`, `Pickup.cpp`, `Use.cpp` and `Cursor.cpp`. The DLL drives
//! the engine's inventory and use calls directly; here the same packets go out through the stock
//! verbs benilla builds (`PickupContainerItem`, `AutoEquipCursorItem`, `UseContainerItem`, …),
//! captured at install so an addon's hook on them is not run, as the engine's call runs none.

use mlua::{Function, Lua, Table, Value};

use crate::items::{self, Found};
use crate::lua::{to_int, to_str, truthy, Api};
use crate::Ca;

/// The stock verbs the actions call.
#[derive(Clone)]
pub(crate) struct Verbs {
    clear_cursor: Option<Function>,
    cursor_has_item: Option<Function>,
    pickup_container: Option<Function>,
    pickup_inventory: Option<Function>,
    auto_equip: Option<Function>,
    equip_cursor: Option<Function>,
    use_container: Option<Function>,
    use_inventory: Option<Function>,
    spell_is_targeting: Option<Function>,
    spell_target_unit: Option<Function>,
    split_container: Option<Function>,
    put_in_backpack: Option<Function>,
    put_in_bag: Option<Function>,
}

impl Verbs {
    pub(crate) fn capture(g: &Table) -> Self {
        let f = |name: &str| g.get::<Function>(name).ok();
        Self {
            clear_cursor: f("ClearCursor"),
            cursor_has_item: f("CursorHasItem"),
            pickup_container: f("PickupContainerItem"),
            pickup_inventory: f("PickupInventoryItem"),
            auto_equip: f("AutoEquipCursorItem"),
            equip_cursor: f("EquipCursorItem"),
            use_container: f("UseContainerItem"),
            use_inventory: f("UseInventoryItem"),
            spell_is_targeting: f("SpellIsTargeting"),
            spell_target_unit: f("SpellTargetUnit"),
            split_container: f("SplitContainerItem"),
            put_in_backpack: f("PutItemInBackpack"),
            put_in_bag: f("PutItemInBag"),
        }
    }

    fn call(f: &Option<Function>, args: impl mlua::IntoLuaMulti) -> mlua::Result<Value> {
        match f {
            Some(f) => f.call(args),
            None => Ok(Value::Nil),
        }
    }

    pub(crate) fn cursor_busy(&self) -> mlua::Result<bool> {
        Ok(truthy(&Self::call(&self.cursor_has_item, ())?))
    }

    /// `Item::Cursor::PickupBagItem` / `PickupEquipmentSlot`.
    fn pickup(&self, f: &Found) -> mlua::Result<()> {
        if f.equipment != 0 {
            Self::call(&self.pickup_inventory, f.equipment)?;
        } else {
            Self::call(&self.pickup_container, (f.bag, f.slot))?;
        }
        Ok(())
    }

    /// `Item::Swap::ContainersFrom`: one bag slot's item swapped with another's.
    pub(crate) fn swap_containers(&self, src: (i64, i64), dst: (i64, i64)) -> mlua::Result<()> {
        Self::call(&self.pickup_container, src)?;
        Self::call(&self.pickup_container, dst)?;
        Ok(())
    }

    /// `Item::Swap::MoveCountFrom`'s split: `count` off a stack onto another slot.
    pub(crate) fn split(&self, src: (i64, i64), dst: (i64, i64), count: i64) -> mlua::Result<()> {
        Self::call(&self.split_container, (src.0, src.1, count))?;
        Self::call(&self.pickup_container, dst)?;
        Ok(())
    }

    /// `Item::Swap::AutoStoreFrom` within the bags: the item into the backpack or bag `dst`'s
    /// first fit.
    pub(crate) fn auto_store(&self, src: (i64, i64), dst: i64) -> mlua::Result<()> {
        Self::call(&self.pickup_container, src)?;
        if dst == 0 {
            Self::call(&self.put_in_backpack, ())?;
        } else {
            Self::call(&self.put_in_bag, items::INVSLOT_BAG1 as i64 - 1 + dst)?;
        }
        Ok(())
    }

    /// `Item::Swap::FromPaperdoll`: one worn item into another paperdoll slot.
    pub(crate) fn paperdoll_to_paperdoll(&self, src: i64, dst: i64) -> mlua::Result<()> {
        Self::call(&self.pickup_inventory, src)?;
        Self::call(&self.pickup_inventory, dst)?;
        Ok(())
    }

    /// `Item::Swap::FromBag`: a bag item into a paperdoll slot.
    pub(crate) fn bag_to_paperdoll(&self, src: (i64, i64), dst: i64) -> mlua::Result<()> {
        Self::call(&self.pickup_container, src)?;
        Self::call(&self.pickup_inventory, dst)?;
        Ok(())
    }

    /// `Item::Swap::ToBag`: a worn item into a bag slot.
    pub(crate) fn paperdoll_to_bag(&self, src: i64, dst: (i64, i64)) -> mlua::Result<()> {
        Self::call(&self.pickup_inventory, src)?;
        Self::call(&self.pickup_container, dst)?;
        Ok(())
    }

    pub(crate) fn clear_cursor(&self) -> mlua::Result<()> {
        Self::call(&self.clear_cursor, ())?;
        Ok(())
    }

    /// The bank's own transfer: a bag item's use while the bank is open.
    pub(crate) fn use_container(&self, src: (i64, i64)) -> mlua::Result<()> {
        Self::call(&self.use_container, src)?;
        Ok(())
    }

    /// `Item::Use`: the item's use, then the named unit when the spell waits for a target.
    pub(crate) fn use_item(&self, f: &Found, target: Option<&str>) -> mlua::Result<()> {
        if f.equipment != 0 {
            Self::call(&self.use_inventory, f.equipment)?;
        } else {
            Self::call(&self.use_container, (f.bag, f.slot))?;
        }
        if let Some(t) = target {
            if truthy(&Self::call(&self.spell_is_targeting, ())?) {
                Self::call(&self.spell_target_unit, t)?;
            }
        }
        Ok(())
    }
}

/// `FindItemArgOrLocation`.
fn find(lua: &Lua, ca: &Ca, v: &Value) -> Option<Found> {
    items::resolve_item_or_location(lua, ca, v)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let verbs = Verbs::capture(&api.lua.globals());

    // `EquipItemByName(item [, slot])`: the cursor cleared; to the named slot, else the engine's
    // pick for a bag item (an equipped one has no slot to pick).
    let (c, v2) = (api.ca.clone(), verbs.clone());
    api.table(
        "C_Item",
        "EquipItemByName",
        move |lua, (item, dst): (Value, Value)| {
            Verbs::call(&v2.clear_cursor, ())?;
            let Some(found) = find(lua, &c, &item) else {
                return Ok(());
            };
            if crate::lua::is_number(&dst) {
                let dst = to_int(&dst);
                if !(1..=19).contains(&dst) {
                    return Ok(());
                }
                v2.pickup(&found)?;
                Verbs::call(&v2.equip_cursor, dst)?;
                return Ok(());
            }
            if found.equipment != 0 || v2.cursor_busy()? {
                return Ok(());
            }
            v2.pickup(&found)?;
            Verbs::call(&v2.auto_equip, ())?;
            Ok(())
        },
    )?;

    // `PickupItem(item)`: the first worn, then bagged, match onto the cursor.
    let (c, v2) = (api.ca.clone(), verbs.clone());
    api.table("C_Item", "PickupItem", move |lua, item: Value| {
        let arg = items::resolve_arg(&item);
        if arg.is_empty() {
            return Ok(());
        }
        let carried = items::Carried::of(&c.lock().mirror);
        let Some(found) = carried.find(lua, &arg) else {
            return Ok(());
        };
        if found.equipment == 0 && v2.cursor_busy()? {
            return Ok(());
        }
        v2.pickup(&found)
    })?;

    // `UseItemByName(item [, unit])`.
    let (c, v2) = (api.ca.clone(), verbs);
    api.table(
        "C_Item",
        "UseItemByName",
        move |lua, (item, unit): (Value, Value)| {
            let target = match &unit {
                Value::String(_) => to_str(&unit),
                _ => None,
            };
            let target =
                target.filter(|t| benilla_ui::script::unit_token_guid_in(lua, t).is_some());
            let Some(found) = find(lua, &c, &item) else {
                return Ok(());
            };
            v2.use_item(&found, target.as_deref())
        },
    )?;
    Ok(())
}

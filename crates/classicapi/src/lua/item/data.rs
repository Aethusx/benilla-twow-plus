//! The template reads: `Info.cpp`, `Name.cpp`, `Icon.cpp`, `Quality.cpp`, `InventoryType.cpp`,
//! `MaxStackSize.cpp`, `SellPrice.cpp`, `Level.cpp`, `Link.cpp`, `ID.cpp`, `Data.cpp`,
//! `GetData.cpp`, `TypeInfo.cpp`, `Set.cpp`, `EnchantInfo.cpp`, `Spell.cpp`, `Uniqueness.cpp`,
//! `Consumable.cpp`, `Exists.cpp` and `BagFamily.cpp`.

use benilla_protocol::ItemInfo;
use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use super::{arg_id, located, location_arg, record};
use crate::dbc::Databases;
use crate::items;
use crate::lua::{is_number, none, to_int, Api};
use crate::spells::{self, col};
use crate::Ca;

/// `ITEM_SPELLTRIGGER_ON_USE`, `_ON_EQUIP`.
pub(crate) const TRIGGER_ON_USE: u32 = 0;
pub(crate) const TRIGGER_ON_EQUIP: u32 = 1;
const ITEM_CLASS_CONTAINER: u32 = 1;
const ITEM_CLASS_QUIVER: u32 = 11;
/// Template flags: conjured, lootable, openable, wrapper.
const FLAG_CONJURED: u32 = 0x2;
const FLAG_OPENABLE: u32 = 0x4;
const FLAG_LOOTABLE: u32 = 0x10;
const FLAG_WRAPPER: u32 = 0x200;
/// `ItemSubClass.dbc` columns: class, subclass, flags, the short and verbose names.
const SUBCLASS_FLAGS: usize = 4;
const SUBCLASS_NAME: usize = 0x28 / 4;
const SUBCLASS_VERBOSE: usize = 0x4C / 4;
const SUBCLASS_USES_INVTYPE: u32 = 0x200;

/// The engine's `INVTYPE_*` token table (`VAR_INVTYPE_STRING_TABLE`), 0 the empty string.
pub(crate) fn inv_type_token(t: u32) -> &'static str {
    const TOKENS: [&str; 29] = [
        "",
        "INVTYPE_HEAD",
        "INVTYPE_NECK",
        "INVTYPE_SHOULDER",
        "INVTYPE_BODY",
        "INVTYPE_CHEST",
        "INVTYPE_WAIST",
        "INVTYPE_LEGS",
        "INVTYPE_FEET",
        "INVTYPE_WRIST",
        "INVTYPE_HAND",
        "INVTYPE_FINGER",
        "INVTYPE_TRINKET",
        "INVTYPE_WEAPON",
        "INVTYPE_SHIELD",
        "INVTYPE_RANGED",
        "INVTYPE_CLOAK",
        "INVTYPE_2HWEAPON",
        "INVTYPE_BAG",
        "INVTYPE_TABARD",
        "INVTYPE_ROBE",
        "INVTYPE_WEAPONMAINHAND",
        "INVTYPE_WEAPONOFFHAND",
        "INVTYPE_HOLDABLE",
        "INVTYPE_AMMO",
        "INVTYPE_THROWN",
        "INVTYPE_RANGEDRIGHT",
        "INVTYPE_QUIVER",
        "INVTYPE_RELIC",
    ];
    TOKENS.get(t as usize).copied().unwrap_or("")
}

/// `ItemClass.dbc`'s localized name (column 3), `""` for none.
fn class_name(db: &Databases, class: u32) -> String {
    db.get("ItemClass")
        .and_then(|t| t.row(class).map(|r| r.loc(3).to_string()))
        .unwrap_or_default()
}

/// The `ItemSubClass.dbc` row for `(class, subclass)`: its name, the verbose one first, and flags.
fn subclass(db: &Databases, class: u32, sub: u32) -> Option<(String, u32)> {
    let t = db.get("ItemSubClass")?;
    let r = t.rows().find(|r| r.u32(0) == class && r.u32(1) == sub)?;
    let verbose = r.loc(SUBCLASS_VERBOSE);
    let name = if verbose.is_empty() {
        r.loc(SUBCLASS_NAME)
    } else {
        verbose
    };
    Some((name.to_string(), r.u32(SUBCLASS_FLAGS)))
}

fn subclass_name(db: &Databases, class: u32, sub: u32) -> String {
    subclass(db, class, sub).map(|s| s.0).unwrap_or_default()
}

/// `PathForDisplayInfoID`: `Interface\Icons\` plus `ItemDisplayInfo.dbc`'s inventory icon.
pub(crate) fn icon_for_display(db: &Databases, display: u32) -> Option<String> {
    let name = db
        .get("ItemDisplayInfo")
        .and_then(|t| t.row(display).map(|r| r.str(0x14 / 4).to_string()))?;
    (!name.is_empty()).then(|| format!("Interface\\Icons\\{name}"))
}

/// `FindOnUseSpellIDInRecord`: the first on-use spell.
pub(crate) fn on_use_spell(r: &ItemInfo) -> u32 {
    r.spells
        .iter()
        .find(|s| s.spell_id != 0 && s.trigger == TRIGGER_ON_USE)
        .map_or(0, |s| s.spell_id)
}

/// `BagFamily::BitmaskForRecord`: the template's family, else the container subclass's, as a
/// bit (`1 << (id - 1)`); Turtle's extra bags resolve only on Turtle.
pub(crate) fn bag_family_mask(r: &ItemInfo, turtle: bool) -> u32 {
    let id = if r.bag_family != 0 {
        r.bag_family
    } else {
        match (r.class, r.subclass) {
            (ITEM_CLASS_CONTAINER, 1) => 3,
            (ITEM_CLASS_CONTAINER, 2) => 6,
            (ITEM_CLASS_CONTAINER, 3) => 7,
            (ITEM_CLASS_CONTAINER, 4) => 8,
            (ITEM_CLASS_QUIVER, 2) => 1,
            (ITEM_CLASS_QUIVER, 3) => 2,
            (ITEM_CLASS_CONTAINER, 6) if turtle => 13,
            (ITEM_CLASS_CONTAINER, 7) if turtle => 12,
            (ITEM_CLASS_CONTAINER, 8) if turtle => 10,
            (ITEM_CLASS_CONTAINER, 9) if turtle => 11,
            _ => 0,
        }
    };
    if id == 0 || id > 32 {
        0
    } else {
        1 << (id - 1)
    }
}

/// `NameFromIDSuffix`: the name with the roll's suffix.
fn name_with_suffix(lua: &Lua, id: u32, suffix: i64) -> Option<String> {
    items::display_name(lua, id, suffix as i32)
}

fn spell_name(db: &Databases, id: u32) -> Option<String> {
    spells::table(db)
        .and_then(|t| t.row(id).map(|r| r.loc(col::NAME).to_string()))
        .filter(|s| !s.is_empty())
}

/// One record field by id, warming on a miss: the read behind every `*ByID` getter.
fn by_id<R: mlua::IntoLuaMulti>(
    lua: &Lua,
    v: &Value,
    f: impl FnOnce(&ItemInfo) -> R,
) -> mlua::Result<MultiValue> {
    match record(lua, arg_id(v)) {
        Some(r) => f(&r).into_lua_multi(lua),
        None => Ok(none()),
    }
}

/// The same read for a location's item.
fn by_location<R: mlua::IntoLuaMulti>(
    lua: &Lua,
    ca: &Ca,
    v: &Value,
    usage: &str,
    f: impl FnOnce(&ItemInfo) -> R,
) -> mlua::Result<MultiValue> {
    location_arg(v, usage)?;
    let id = located(ca, v).map_or(0, |i| i64::from(i.entry));
    match record(lua, id) {
        Some(r) => f(&r).into_lua_multi(lua),
        None => Ok(none()),
    }
}

/// `GetItemDataByID`'s table.
fn data_table(
    lua: &Lua,
    db: &Databases,
    id: u32,
    r: &ItemInfo,
    turtle: bool,
) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("itemID", id)?;
    t.set("name", r.name.as_str())?;
    if !r.description.is_empty() {
        t.set("description", r.description.as_str())?;
    }
    t.set("displayInfoID", r.display_info_id)?;
    if let Some(icon) = icon_for_display(db, r.display_info_id) {
        t.set("icon", icon)?;
    }
    t.set("quality", r.quality)?;
    t.set("classID", r.class)?;
    t.set("subclassID", r.subclass)?;
    t.set("className", class_name(db, r.class))?;
    t.set("subclassName", subclass_name(db, r.class, r.subclass))?;
    t.set("inventoryType", r.inventory_type)?;
    t.set("equipLoc", inv_type_token(r.inventory_type))?;
    t.set("bindType", r.bonding)?;
    t.set("flags", r.flags)?;
    t.set("isConjured", r.flags & FLAG_CONJURED != 0)?;
    t.set("isOpenable", r.flags & FLAG_OPENABLE != 0)?;
    t.set("isLootable", r.flags & FLAG_LOOTABLE != 0)?;
    t.set("isWrapper", r.flags & FLAG_WRAPPER != 0)?;
    t.set("maxStackSize", r.stackable as i32)?;
    t.set("maxCount", r.max_count as i32)?;
    t.set("containerSlots", r.container_slots)?;
    t.set("bagFamily", bag_family_mask(r, turtle))?;
    for (k, v) in [
        ("buyPrice", r.buy_price),
        ("sellPrice", r.sell_price),
        ("itemLevel", r.item_level),
        ("requiredLevel", r.required_level),
        ("requiredSkill", r.required_skill),
        ("requiredSkillRank", r.required_skill_rank),
        ("requiredSpell", r.required_spell),
        ("requiredHonorRank", r.required_honor_rank),
        ("requiredCityRank", r.required_city_rank),
        ("requiredFaction", r.required_rep_faction),
        ("requiredFactionRank", r.required_rep_rank),
        ("allowableClass", r.allowable_class as u32),
        ("allowableRace", r.allowable_race as u32),
        ("armor", r.armor),
        ("block", r.block),
        ("maxDurability", r.max_durability),
    ] {
        t.set(k, v)?;
    }
    let stats = lua.create_table()?;
    for (ty, v) in &r.stats {
        stats.set(*ty, *v)?;
    }
    t.set("stats", stats)?;
    let res = [
        "resistanceHoly",
        "resistanceFire",
        "resistanceNature",
        "resistanceFrost",
        "resistanceShadow",
        "resistanceArcane",
    ];
    for (k, v) in res.iter().zip(r.resistances) {
        t.set(*k, v as u32)?;
    }
    let (mins, maxs, types) = (
        lua.create_table()?,
        lua.create_table()?,
        lua.create_table()?,
    );
    for i in 0..5 {
        let d = r.damages.get(i);
        mins.set(i + 1, d.map_or(0.0, |d| f64::from(d.min)))?;
        maxs.set(i + 1, d.map_or(0.0, |d| f64::from(d.max)))?;
        types.set(i + 1, d.map_or(0, |d| d.school))?;
    }
    t.set("damageMin", mins)?;
    t.set("damageMax", maxs)?;
    t.set("damageType", types)?;
    t.set("delay", r.delay_ms)?;
    t.set("ammoType", r.ammo_type)?;
    t.set("rangedModRange", f64::from(r.ranged_mod_range))?;
    let sp = lua.create_table()?;
    for (n, s) in r.spells.iter().filter(|s| s.spell_id != 0).enumerate() {
        let e = lua.create_table()?;
        e.set("id", s.spell_id)?;
        e.set("trigger", s.trigger)?;
        e.set("charges", s.charges)?;
        e.set("cooldown", s.cooldown_ms as u32)?;
        e.set("category", s.category)?;
        e.set("categoryCooldown", s.category_cooldown_ms as u32)?;
        sp.set(n + 1, e)?;
    }
    t.set("spells", sp)?;
    let use_spell = on_use_spell(r);
    if use_spell > 0 {
        t.set("useSpellID", use_spell)?;
    }
    for (k, v) in [
        ("lockID", r.lock_id),
        ("itemSet", r.item_set),
        ("pageText", r.page_text),
        ("pageMaterial", r.page_material),
        ("languageID", r.language_id),
        ("startQuest", r.start_quest),
        ("sheath", r.sheath),
        ("area", r.area),
        ("map", r.map),
    ] {
        t.set(k, v)?;
    }
    t.set("material", r.material as i32)?;
    t.set("randomProperty", r.random_property as i32)?;
    Ok(t)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `GetItemInfo(item)`: 18 values; nil this call on a miss, `GET_ITEM_INFO_RECEIVED` when in.
    let db = api.ca.db.clone();
    api.table("C_Item", "GetItemInfo", move |lua, v: Value| {
        let arg = items::resolve_arg(&v);
        let Some(r) = record(lua, arg.item_id) else {
            return Ok(none());
        };
        let id = arg.item_id as u32;
        let name = name_with_suffix(lua, id, arg.suffix).unwrap_or_else(|| r.name.clone());
        let link = items::link(lua, id, arg.suffix as i32);
        let icon = icon_for_display(&db, r.display_info_id).unwrap_or_default();
        let set = (r.item_set != 0).then_some(r.item_set);
        let out = vec![
            Value::String(lua.create_string(&name)?),
            match link {
                Some(l) => Value::String(lua.create_string(&l)?),
                None => Value::Nil,
            },
            Value::Integer(r.quality.into()),
            Value::Integer(r.item_level.into()),
            Value::Integer(r.required_level.into()),
            Value::String(lua.create_string(class_name(&db, r.class))?),
            Value::String(lua.create_string(subclass_name(&db, r.class, r.subclass))?),
            Value::Integer(r.stackable.into()),
            Value::String(lua.create_string(inv_type_token(r.inventory_type))?),
            Value::String(lua.create_string(&icon)?),
            Value::Integer(r.sell_price.into()),
            Value::Integer(r.class.into()),
            Value::Integer(r.subclass.into()),
            Value::Integer(r.bonding.into()),
            Value::Integer(0),
            set.map_or(Value::Nil, |s| Value::Integer(s.into())),
            Value::Boolean(false),
            Value::String(lua.create_string(&r.description)?),
        ];
        Ok(MultiValue::from_vec(out))
    })?;

    // `GetItemInfoInstant(item)`: the id, then six nils on a miss; no warm-up.
    let db = api.ca.db.clone();
    api.table("C_Item", "GetItemInfoInstant", move |lua, v: Value| {
        let id = arg_id(&v);
        if id <= 0 {
            return Ok(none());
        }
        let Some(r) = crate::itemdb::peek(lua, id as u32) else {
            return (
                id,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                Value::Nil,
            )
                .into_lua_multi(lua);
        };
        (
            id,
            class_name(&db, r.class),
            subclass_name(&db, r.class, r.subclass),
            inv_type_token(r.inventory_type),
            icon_for_display(&db, r.display_info_id).unwrap_or_default(),
            r.class,
            r.subclass,
        )
            .into_lua_multi(lua)
    })?;

    let db = api.ca.db.clone();
    api.global("GetItemIcon", move |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: GetItemIcon(itemID)"));
        }
        by_id(lua, &v, |r| icon_for_display(&db, r.display_info_id))
    })?;
    let db = api.ca.db.clone();
    api.table("C_Item", "GetItemIconByID", move |lua, v: Value| {
        by_id(lua, &v, |r| icon_for_display(&db, r.display_info_id))
    })?;
    let (db, c) = (api.ca.db.clone(), api.ca.clone());
    api.table("C_Item", "GetItemIcon", move |lua, v: Value| {
        by_location(
            lua,
            &c,
            &v,
            "Usage: C_Item.GetItemIcon(itemLocation)",
            |r| icon_for_display(&db, r.display_info_id),
        )
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "GetItemFamily", move |lua, v: Value| {
        let turtle = c.lock().auras.turtle;
        by_id(lua, &v, |r| bag_family_mask(r, turtle))
    })?;

    // `GetItemName(location)`: the instance's name with its roll, else the template's.
    let c = api.ca.clone();
    api.table("C_Item", "GetItemName", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.GetItemName(itemLocation)")?;
        let Some(item) = located(&c, &v) else {
            return Ok(none());
        };
        if let Some(n) = items::display_name(lua, item.entry, item.random_property()) {
            return n.into_lua_multi(lua);
        }
        by_id(lua, &Value::Integer(item.entry.into()), |r| {
            (!r.name.is_empty()).then(|| r.name.clone())
        })
    })?;
    api.table("C_Item", "GetItemNameByID", |lua, v: Value| {
        let arg = items::resolve_arg(&v);
        if arg.item_id <= 0 {
            return Ok(none());
        }
        match name_with_suffix(lua, arg.item_id as u32, arg.suffix) {
            Some(n) => n.into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    for (name, value) in [
        ("LE_ITEM_QUALITY_POOR", 0),
        ("LE_ITEM_QUALITY_COMMON", 1),
        ("LE_ITEM_QUALITY_UNCOMMON", 2),
        ("LE_ITEM_QUALITY_RARE", 3),
        ("LE_ITEM_QUALITY_EPIC", 4),
        ("LE_ITEM_QUALITY_LEGENDARY", 5),
        ("LE_ITEM_QUALITY_ARTIFACT", 6),
        ("LE_ITEM_QUALITY_HEIRLOOM", 7),
        ("LE_ITEM_QUALITY_WOWTOKEN", 8),
    ] {
        api.number(name, value)?;
    }

    // The by-location / by-id pairs over one record field.
    type Field = fn(&ItemInfo) -> u32;
    let pairs: [(&str, &str, Field); 6] = [
        ("GetItemQuality", "GetItemQualityByID", |r| r.quality),
        ("GetItemInventoryType", "GetItemInventoryTypeByID", |r| {
            r.inventory_type
        }),
        ("GetItemMaxStackSize", "GetItemMaxStackSizeByID", |r| {
            r.stackable
        }),
        ("GetItemSellPrice", "GetItemSellPriceByID", |r| r.sell_price),
        ("GetCurrentItemLevel", "GetDetailedItemLevelInfo", |r| {
            r.item_level
        }),
        ("GetItemSetID", "GetItemSetIDByID", |r| r.item_set),
    ];
    for (loc_name, id_name, f) in pairs {
        let set = loc_name == "GetItemSetID";
        let c = api.ca.clone();
        let usage = format!("Usage: C_Item.{loc_name}(itemLocation)");
        api.table("C_Item", loc_name, move |lua, v: Value| {
            by_location(lua, &c, &v, &usage, |r| {
                let x = f(r);
                (!set || x != 0).then_some(x)
            })
        })?;
        api.table("C_Item", id_name, move |lua, v: Value| {
            by_id(lua, &v, |r| {
                let x = f(r);
                (!set || x != 0).then_some(x)
            })
        })?;
    }

    // `GetItemLink(location)`: the instance's link.
    let c = api.ca.clone();
    api.table("C_Item", "GetItemLink", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.GetItemLink(itemLocation)")?;
        match located(&c, &v).and_then(|i| items::item_link(lua, &i)) {
            Some(l) => l.into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "GetItemID", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.GetItemID(itemLocation)")?;
        match located(&c, &v).map(|i| i.entry).filter(|e| *e != 0) {
            Some(e) => e.into_lua_multi(lua),
            None => Value::Nil.into_lua_multi(lua),
        }
    })?;

    api.table("C_Item", "IsItemDataCachedByID", |lua, v: Value| {
        let id = arg_id(&v);
        Ok(id > 0 && crate::itemdb::peek(lua, id as u32).is_some())
    })?;
    let c = api.ca.clone();
    api.table("C_Item", "IsItemDataCached", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.IsItemDataCached(itemLocation)")?;
        let id = located(&c, &v).map_or(0, |i| i.entry);
        Ok(id > 0 && crate::itemdb::peek(lua, id).is_some())
    })?;
    // `RequestLoadItemData*`: queue the load; `ITEM_DATA_LOAD_RESULT` reports it.
    let c = api.ca.clone();
    api.table("C_Item", "RequestLoadItemDataByID", move |_, v: Value| {
        let id = arg_id(&v);
        Ok(id > 0 && c.items.lock().track(id as u32, false))
    })?;
    let c = api.ca.clone();
    api.table("C_Item", "RequestLoadItemData", move |_, v: Value| {
        location_arg(&v, "Usage: C_Item.RequestLoadItemData(itemLocation)")?;
        let id = located(&c, &v).map_or(0, |i| i.entry);
        Ok(id > 0 && c.items.lock().track(id, false))
    })?;

    // `GetItemDataByID` / `GetItemData`: every record field as a table; nil on a miss, no
    // warm-up.
    let c = api.ca.clone();
    api.table("C_Item", "GetItemDataByID", move |lua, v: Value| {
        let id = arg_id(&v);
        let r = (id > 0)
            .then(|| crate::itemdb::peek(lua, id as u32))
            .flatten();
        match r {
            Some(r) => {
                let turtle = c.lock().auras.turtle;
                Ok(Value::Table(data_table(lua, &c.db, id as u32, &r, turtle)?))
            }
            None => Ok(Value::Nil),
        }
    })?;
    let c = api.ca.clone();
    api.table("C_Item", "GetItemData", move |lua, v: Value| {
        location_arg(&v, "Usage: C_Item.GetItemData(itemLocation)")?;
        let id = located(&c, &v).map_or(0, |i| i.entry);
        match crate::itemdb::peek(lua, id) {
            Some(r) if id > 0 => {
                let turtle = c.lock().auras.turtle;
                Ok(Value::Table(data_table(lua, &c.db, id, &r, turtle)?))
            }
            _ => Ok(Value::Nil),
        }
    })?;

    // `ItemClass.dbc` / `ItemSubClass.dbc` names; the inventory-slot tokens and their strings.
    for ns in [None, Some("C_Item")] {
        let db = api.ca.db.clone();
        let f = move |_: &Lua, v: Value| {
            if !is_number(&v) {
                return Err(mlua::Error::runtime("Usage: GetItemClassInfo(classID)"));
            }
            let n = class_name(&db, to_int(&v) as u32);
            Ok((!n.is_empty()).then_some(n))
        };
        match ns {
            None => api.global("GetItemClassInfo", f)?,
            Some(ns) => api.table(ns, "GetItemClassInfo", f)?,
        }
        let db = api.ca.db.clone();
        let f = move |lua: &Lua, (c, s): (Value, Value)| {
            if !is_number(&c) || !is_number(&s) {
                return Err(mlua::Error::runtime(
                    "Usage: C_Item.GetItemSubClassInfo(classID, subClassID)",
                ));
            }
            match subclass(&db, to_int(&c) as u32, to_int(&s) as u32) {
                Some((name, flags)) => {
                    (name, flags & SUBCLASS_USES_INVTYPE != 0).into_lua_multi(lua)
                }
                None => Value::Nil.into_lua_multi(lua),
            }
        };
        match ns {
            None => api.global("GetItemSubClassInfo", f)?,
            Some(ns) => api.table(ns, "GetItemSubClassInfo", f)?,
        }
    }
    api.table("C_Item", "GetItemInventorySlotKey", |_, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_Item.GetItemInventorySlotKey(inventorySlot)",
            ));
        }
        let k = inv_type_token(to_int(&v).max(-1) as u32);
        Ok((!k.is_empty()).then_some(k))
    })?;
    api.table("C_Item", "GetItemInventorySlotInfo", |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_Item.GetItemInventorySlotInfo(inventorySlot)",
            ));
        }
        let k = inv_type_token(to_int(&v).max(-1) as u32);
        if k.is_empty() {
            return Ok(Value::Nil);
        }
        // `PushLocalizedString`: the global string, else the key.
        match lua.globals().get::<Value>(k)? {
            s @ Value::String(_) => Ok(s),
            _ => Ok(Value::String(lua.create_string(k)?)),
        }
    })?;
    let inventory_types: Vec<(&str, i64)> = [
        "NonEquip",
        "Head",
        "Neck",
        "Shoulder",
        "Body",
        "Chest",
        "Waist",
        "Legs",
        "Feet",
        "Wrist",
        "Hand",
        "Finger",
        "Trinket",
        "Weapon",
        "Shield",
        "Ranged",
        "Cloak",
        "2Hweapon",
        "Bag",
        "Tabard",
        "Robe",
        "Weaponmainhand",
        "Weaponoffhand",
        "Holdable",
        "Ammo",
        "Thrown",
        "Rangedright",
        "Quiver",
        "Relic",
        "ProfessionTool",
        "ProfessionGear",
        "EquipablespellOffensive",
        "EquipablespellUtility",
        "EquipablespellDefensive",
        "EquipablespellWeapon",
    ]
    .iter()
    .enumerate()
    .map(|(i, n)| (*n, i as i64))
    .collect();
    let names: Vec<String> = inventory_types
        .iter()
        .map(|(n, _)| format!("Index{n}Type"))
        .collect();
    let entries: Vec<(&str, i64)> = names
        .iter()
        .zip(&inventory_types)
        .map(|(n, (_, v))| (n.as_str(), *v))
        .collect();
    api.int_enum("Enum", "InventoryType", &entries)?;
    api.int_enum(
        "Enum",
        "ItemClass",
        &[
            ("Consumable", 0),
            ("Container", 1),
            ("Weapon", 2),
            ("Gem", 3),
            ("Armor", 4),
            ("Reagent", 5),
            ("Projectile", 6),
            ("Tradegoods", 7),
            ("ItemEnhancement", 8),
            ("Recipe", 9),
            ("CurrencyTokenObsolete", 10),
            ("Quiver", 11),
            ("Questitem", 12),
            ("Key", 13),
            ("PermanentObsolete", 14),
            ("Miscellaneous", 15),
            ("Glyph", 16),
            ("Battlepet", 17),
            ("WoWToken", 18),
            ("Profession", 19),
        ],
    )?;
    api.int_enum(
        "Enum",
        "ItemQuality",
        &[
            ("Poor", 0),
            ("Common", 1),
            ("Uncommon", 2),
            ("Rare", 3),
            ("Epic", 4),
            ("Legendary", 5),
            ("Artifact", 6),
        ],
    )?;

    // `GetItemSetInfo(setID)`: `ItemSet.dbc` as a table.
    let db = api.ca.db.clone();
    api.table("C_Item", "GetItemSetInfo", move |lua, v: Value| {
        if !is_number(&v) {
            return Ok(none());
        }
        let id = to_int(&v);
        let Some(t) = db.get("ItemSet") else {
            return Ok(none());
        };
        let Some(r) = u32::try_from(id)
            .ok()
            .filter(|i| *i > 0)
            .and_then(|i| t.row(i))
        else {
            return Ok(none());
        };
        let out = lua.create_table()?;
        out.set("setID", id)?;
        out.set("name", r.loc(1))?;
        out.set("requiredSkill", r.u32(0xAC / 4))?;
        out.set("requiredSkillRank", r.u32(0xB0 / 4))?;
        let items = lua.create_table()?;
        for (n, item) in (0..17)
            .map(|i| r.u32(0x28 / 4 + i))
            .filter(|i| *i != 0)
            .enumerate()
        {
            items.set(n + 1, item)?;
        }
        out.set("items", items)?;
        let bonuses = lua.create_table()?;
        let mut n = 0;
        for i in 0..8 {
            let spell = r.u32(0x6C / 4 + i);
            if spell == 0 {
                continue;
            }
            n += 1;
            let b = lua.create_table()?;
            b.set("spellID", spell)?;
            b.set("threshold", r.u32(0x8C / 4 + i))?;
            bonuses.set(n, b)?;
        }
        out.set("bonuses", bonuses)?;
        out.into_lua_multi(lua)
    })?;

    // `GetEnchantInfo(enchantID)`: `SpellItemEnchantment.dbc` as a table; the first spell-type
    // effect's argument is its spell.
    let db = api.ca.db.clone();
    api.table("C_Item", "GetEnchantInfo", move |lua, v: Value| {
        if !is_number(&v) {
            return Ok(none());
        }
        let id = to_int(&v);
        let Some(t) = db.get("SpellItemEnchantment") else {
            return Ok(none());
        };
        let Some(r) = u32::try_from(id)
            .ok()
            .filter(|i| *i > 0)
            .and_then(|i| t.row(i))
        else {
            return Ok(none());
        };
        let name = r.loc(0x34 / 4);
        if name.is_empty() {
            return Ok(none());
        }
        let out = lua.create_table()?;
        out.set("enchantID", id)?;
        out.set("name", name)?;
        let effects = lua.create_table()?;
        let mut spell = 0;
        let mut n = 0;
        for i in 0..3 {
            let ty = r.i32(1 + i);
            if ty == 0 {
                continue;
            }
            let arg = r.i32(0x28 / 4 + i);
            if spell == 0 && matches!(ty, 1 | 3 | 7) && arg != 0 {
                spell = arg;
            }
            n += 1;
            let e = lua.create_table()?;
            e.set("type", ty)?;
            e.set("amount", r.i32(0x10 / 4 + i))?;
            e.set("arg", arg)?;
            effects.set(n, e)?;
        }
        out.set("effects", effects)?;
        if spell != 0 {
            out.set("spellID", spell)?;
        }
        out.into_lua_multi(lua)
    })?;

    // `GetItemSpell(item)`: the on-use spell's name and id.
    let db = api.ca.db.clone();
    api.table("C_Item", "GetItemSpell", move |lua, v: Value| {
        let Some(r) = record(lua, arg_id(&v)) else {
            return Ok(none());
        };
        let id = on_use_spell(&r);
        match (id > 0).then(|| spell_name(&db, id)).flatten() {
            Some(name) => (name, id).into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    // `GetItemUniqueness(location)` -> 0, maxCount; `GetItemUniquenessByID(id)` -> limited,
    // nil, count or nil, nil (vanilla has no limit categories).
    let c = api.ca.clone();
    api.table("C_Item", "GetItemUniqueness", move |lua, v: Value| {
        if !crate::items::is_location_arg(&v) {
            return Ok(none());
        }
        let id = located(&c, &v).map_or(0, |i| i64::from(i.entry));
        match record(lua, id) {
            Some(r) => (0, r.max_count as i32).into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;
    api.table("C_Item", "GetItemUniquenessByID", |lua, v: Value| {
        if !is_number(&v) {
            return Ok(none());
        }
        match record(lua, to_int(&v)) {
            Some(r) => {
                let n = r.max_count as i32;
                (n > 0, Value::Nil, (n > 0).then_some(n), Value::Nil).into_lua_multi(lua)
            }
            None => Ok(none()),
        }
    })?;

    // Consumable: class 0, ammo or thrown; uncached is false.
    let consumable = |lua: &Lua, v: Value| -> mlua::Result<bool> {
        let id = arg_id(&v);
        Ok(id > 0
            && crate::itemdb::peek(lua, id as u32)
                .is_some_and(|r| r.class == 0 || matches!(r.inventory_type, 24 | 25)))
    };
    api.table("C_Item", "IsConsumableItem", consumable)?;
    api.global("IsConsumableItem", consumable)?;

    api.table("C_Item", "IsEquippableItem", |lua, v: Value| {
        let id = arg_id(&v);
        Ok(id > 0 && crate::itemdb::peek(lua, id as u32).is_some_and(|r| r.inventory_type > 0))
    })?;

    let c = api.ca.clone();
    api.table("C_Item", "DoesItemExist", move |_, v: Value| {
        Ok(crate::items::is_location_arg(&v) && located(&c, &v).is_some())
    })?;
    api.table("C_Item", "DoesItemExistByID", |_, v: Value| {
        Ok(arg_id(&v) > 0)
    })?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn bag_families_map_to_bits() {
        let mut r = ItemInfo {
            class: ITEM_CLASS_QUIVER,
            subclass: 2,
            ..test_item()
        };
        assert_eq!(bag_family_mask(&r, false), 1);
        r.class = ITEM_CLASS_CONTAINER;
        r.subclass = 6;
        assert_eq!(bag_family_mask(&r, false), 0);
        assert_eq!(bag_family_mask(&r, true), 1 << 12);
        assert_eq!(inv_type_token(26), "INVTYPE_RANGEDRIGHT");
    }

    pub(crate) fn test_item() -> ItemInfo {
        ItemInfo {
            class: 0,
            subclass: 0,
            name: String::new(),
            display_info_id: 0,
            quality: 0,
            flags: 0,
            buy_price: 0,
            sell_price: 0,
            inventory_type: 0,
            allowable_class: -1,
            allowable_race: -1,
            item_level: 0,
            required_level: 0,
            required_skill: 0,
            required_skill_rank: 0,
            required_spell: 0,
            required_honor_rank: 0,
            required_city_rank: 0,
            required_rep_faction: 0,
            required_rep_rank: 0,
            max_count: 0,
            stackable: 1,
            container_slots: 0,
            stats: Vec::new(),
            damages: Vec::new(),
            dmg_min: 0.0,
            dmg_max: 0.0,
            dmg_type: 0,
            armor: 0,
            resistances: [0; 6],
            delay_ms: 0,
            ammo_type: 0,
            ranged_mod_range: 0.0,
            spells: Vec::new(),
            spell_charges_0: 0,
            use_spell: None,
            bonding: 0,
            description: String::new(),
            page_text: 0,
            language_id: 0,
            page_material: 0,
            start_quest: 0,
            lock_id: 0,
            material: 0,
            sheath: 0,
            random_property: 0,
            block: 0,
            item_set: 0,
            max_durability: 0,
            area: 0,
            map: 0,
            bag_family: 0,
        }
    }
}

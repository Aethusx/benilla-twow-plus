//! Item reads: the `ItemStats` record (`GetItemStats`, `GetItemStatsField`, `GetItemLevel`), the
//! icon, and the inventory walks (`FindPlayerItemSlot`, `GetEquippedItems`, `GetBagItems`,
//! `GetAmmo`, `GetTrinkets`), read off the mirrored player and item descriptors.

use benilla_app::ext::ItemInfo;
use mlua::{Lua, Table, Value};

use super::{int, set, text, unit};
use crate::mirror::{field, Fields};
use crate::{Np, State};

/// The item guids of a bag's slots, 0 for an empty slot, in Lua's bag numbering: 0 the
/// backpack, 1-4 the bags, -1 the bank, 5-10 the bank bags, -2 the keyring.
pub(crate) fn bag_slots(st: &State, bag: i64) -> Option<Vec<u64>> {
    let me = st.mirror.player_fields()?;
    let run = |base: usize, n: usize| (0..n).map(|i| me.guid(base + 2 * i)).collect();
    let container = |guid: u64| -> Option<Vec<u64>> {
        let c = st.mirror.objects.get(&guid)?;
        let n = c.u32(field::CONTAINER_NUM_SLOTS) as usize;
        Some(
            (0..n.min(36))
                .map(|i| c.guid(field::CONTAINER_SLOT_1 + 2 * i))
                .collect(),
        )
    };
    match bag {
        0 => Some(run(field::PLAYER_PACK_SLOT_1, 16)),
        1..=4 => container(me.guid(field::PLAYER_INV_SLOT_HEAD + 2 * (18 + bag as usize))),
        -1 => Some(run(field::PLAYER_BANK_SLOT_1, 24)),
        5..=10 => container(me.guid(field::PLAYER_BANK_BAG_SLOT_1 + 2 * (bag as usize - 5))),
        -2 => Some(run(field::PLAYER_KEYRING_SLOT_1, 32)),
        _ => None,
    }
}

/// Our equipment, slots 0-18.
pub(crate) fn equipped(st: &State) -> Vec<u64> {
    st.mirror.player_fields().map_or_else(Vec::new, |me| {
        (0..19)
            .map(|i| me.guid(field::PLAYER_INV_SLOT_HEAD + 2 * i))
            .collect()
    })
}

pub(crate) fn entry(st: &State, guid: u64) -> u32 {
    st.mirror
        .objects
        .get(&guid)
        .map_or(0, |f| f.u32(field::OBJECT_ENTRY))
}

/// An item instance's fields, as nampower lists them for our own items.
fn own_item(lua: &Lua, f: &Fields) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("itemId", f.u32(field::OBJECT_ENTRY))?;
    t.set("stackCount", f.u32(field::ITEM_STACK_COUNT))?;
    t.set("duration", f.u32(field::ITEM_DURATION))?;
    let charges = (0..5)
        .map(|i| f.u32(field::ITEM_SPELL_CHARGES + i) as i32)
        .max_by_key(|c| c.unsigned_abs())
        .unwrap_or(0);
    t.set("spellChargesRemaining", charges)?;
    t.set("flags", f.u32(field::ITEM_FLAGS))?;
    t.set(
        "randomPropertiesId",
        f.u32(field::ITEM_RANDOM_PROPERTIES_ID),
    )?;
    t.set("permanentEnchantId", f.u32(field::ITEM_ENCHANTMENT))?;
    t.set("tempEnchantId", f.u32(field::ITEM_ENCHANTMENT + 3))?;
    t.set(
        "tempEnchantmentTimeLeftMs",
        f.u32(field::ITEM_ENCHANTMENT + 4),
    )?;
    t.set("tempEnchantmentCharges", f.u32(field::ITEM_ENCHANTMENT + 5))?;
    t.set("durability", f.u32(field::ITEM_DURABILITY))?;
    t.set("maxDurability", f.u32(field::ITEM_MAX_DURABILITY))?;
    Ok(t)
}

/// Another player's visible item: what the inspect fields broadcast.
fn visible_item(lua: &Lua, f: &Fields, slot: usize) -> mlua::Result<Option<Table>> {
    let base = field::PLAYER_VISIBLE_ITEM_1 + 12 * slot;
    let item_id = f.u32(base + 2);
    if item_id == 0 {
        return Ok(None);
    }
    let t = lua.create_table()?;
    t.set("itemId", item_id)?;
    t.set("randomPropertiesId", f.u32(base + 10) & 0xffff)?;
    t.set("permanentEnchantId", f.u32(base + 3))?;
    t.set("tempEnchantId", f.u32(base + 4))?;
    Ok(Some(t))
}

/// An item id, or a name matched case-insensitively against the cached templates.
fn matches(st: &State, want: &Value) -> impl Fn(u32) -> bool {
    let id = match want {
        Value::Integer(_) | Value::Number(_) => int(want).map(|i| i as u32),
        _ => None,
    };
    let name = text(want).map(|s| s.to_lowercase());
    let named: Vec<u32> = match (&id, &name) {
        (None, Some(name)) => st
            .mirror
            .items
            .iter()
            .filter(|(_, i)| i.name.to_lowercase() == *name)
            .map(|(e, _)| *e)
            .collect(),
        _ => Vec::new(),
    };
    move |entry| entry != 0 && (Some(entry) == id || named.contains(&entry))
}

fn stats_value(lua: &Lua, info: &ItemInfo, name: &str) -> mlua::Result<Option<Value>> {
    let num = |v: f64| Value::Number(v);
    let list = |vals: Vec<f64>| -> mlua::Result<Value> {
        let t = lua.create_table()?;
        for (i, v) in vals.into_iter().enumerate() {
            t.set(i + 1, v)?;
        }
        Ok(Value::Table(t))
    };
    let pad = |mut v: Vec<f64>, n: usize| {
        v.resize(n, 0.0);
        v
    };
    let spells = |pick: &dyn Fn(&benilla_protocol::messages::ItemSpellEntry) -> f64| {
        let mut out = vec![0.0; 5];
        for s in &info.spells {
            if let Some(slot) = out.get_mut(usize::from(s.index)) {
                *slot = pick(s);
            }
        }
        out
    };
    let u = |v: u32| num(f64::from(v));
    Ok(Some(match name.to_ascii_lowercase().as_str() {
        "displayname" | "name" => Value::String(lua.create_string(&info.name)?),
        "description" => Value::String(lua.create_string(&info.description)?),
        "allowableclass" => num(f64::from(info.allowable_class)),
        "allowablerace" => num(f64::from(info.allowable_race)),
        "ammotype" => u(info.ammo_type),
        "area" => u(info.area),
        "bagfamily" => u(info.bag_family),
        "block" => u(info.block),
        "bonding" => u(info.bonding),
        "buyprice" => u(info.buy_price),
        "class" => u(info.class),
        "containerslots" => u(info.container_slots),
        "delay" => u(info.delay_ms),
        "displayinfoid" => u(info.display_info_id),
        "flags" => u(info.flags),
        "inventorytype" => u(info.inventory_type),
        "itemlevel" => u(info.item_level),
        "itemset" => u(info.item_set),
        "languageid" => u(info.language_id),
        "lockid" => u(info.lock_id),
        "map" => u(info.map),
        "material" => u(info.material),
        "maxcount" => u(info.max_count),
        "maxdurability" => u(info.max_durability),
        "pagematerial" => u(info.page_material),
        "pagetext" => u(info.page_text),
        "quality" => u(info.quality),
        "randomproperty" => u(info.random_property),
        "rangedmodrange" => num(f64::from(info.ranged_mod_range)),
        "requiredcityrank" => u(info.required_city_rank),
        "requiredhonorrank" => u(info.required_honor_rank),
        "requiredlevel" => u(info.required_level),
        "requiredrep" => u(info.required_rep_faction),
        "requiredreprank" => u(info.required_rep_rank),
        "requiredskill" => u(info.required_skill),
        "requiredskillrank" => u(info.required_skill_rank),
        "requiredspell" => u(info.required_spell),
        "sellprice" => u(info.sell_price),
        "sheathetype" => u(info.sheath),
        "stackable" => u(info.stackable),
        "startquestid" => u(info.start_quest),
        "subclass" => u(info.subclass),
        "bonusstat" => list(pad(
            info.stats.iter().map(|(t, _)| f64::from(*t)).collect(),
            10,
        ))?,
        "bonusamount" => list(pad(
            info.stats.iter().map(|(_, v)| f64::from(*v)).collect(),
            10,
        ))?,
        "mindamage" => list(pad(
            info.damages.iter().map(|d| f64::from(d.min)).collect(),
            5,
        ))?,
        "maxdamage" => list(pad(
            info.damages.iter().map(|d| f64::from(d.max)).collect(),
            5,
        ))?,
        "damagetype" => list(pad(
            info.damages.iter().map(|d| f64::from(d.school)).collect(),
            5,
        ))?,
        "resistances" => list(
            std::iter::once(f64::from(info.armor))
                .chain(info.resistances.iter().map(|r| f64::from(*r)))
                .collect(),
        )?,
        "spellid" => list(spells(&|s| f64::from(s.spell_id)))?,
        "spelltrigger" => list(spells(&|s| f64::from(s.trigger)))?,
        "spellcharges" => list(spells(&|s| f64::from(s.charges)))?,
        "spellcooldown" => list(spells(&|s| f64::from(s.cooldown_ms)))?,
        "spellcategory" => list(spells(&|s| f64::from(s.category)))?,
        "spellcategorycooldown" => list(spells(&|s| f64::from(s.category_cooldown_ms)))?,
        _ => return Ok(None),
    }))
}

/// Every `GetItemStats` field name.
const STATS_FIELDS: &[&str] = &[
    "displayName",
    "description",
    "allowableClass",
    "allowableRace",
    "ammoType",
    "area",
    "bagFamily",
    "block",
    "bonding",
    "buyPrice",
    "class",
    "containerSlots",
    "delay",
    "displayInfoID",
    "flags",
    "inventoryType",
    "itemLevel",
    "itemSet",
    "languageID",
    "lockID",
    "map",
    "material",
    "maxCount",
    "maxDurability",
    "pageMaterial",
    "pageText",
    "quality",
    "randomProperty",
    "rangedModRange",
    "requiredCityRank",
    "requiredHonorRank",
    "requiredLevel",
    "requiredRep",
    "requiredRepRank",
    "requiredSkill",
    "requiredSkillRank",
    "requiredSpell",
    "sellPrice",
    "sheatheType",
    "stackable",
    "startQuestID",
    "subclass",
    "bonusStat",
    "bonusAmount",
    "minDamage",
    "maxDamage",
    "damageType",
    "resistances",
    "spellID",
    "spellTrigger",
    "spellCharges",
    "spellCooldown",
    "spellCategory",
    "spellCategoryCooldown",
];

pub fn install(lua: &Lua, g: &Table, np: &Np) -> mlua::Result<()> {
    let n = np.clone();
    set(
        lua,
        g,
        "GetItemStats",
        move |lua, (id, _copy): (Value, Value)| {
            let st = n.lock();
            let Some(info) = int(&id).and_then(|id| st.mirror.items.get(&(id as u32))) else {
                return Ok(Value::Nil);
            };
            let t = lua.create_table()?;
            for name in STATS_FIELDS {
                if let Some(v) = stats_value(lua, info, name)? {
                    t.set(*name, v)?;
                }
            }
            Ok(Value::Table(t))
        },
    )?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetItemStatsField",
        move |lua, (id, name, _copy): (Value, String, Value)| {
            let st = n.lock();
            let Some(info) = int(&id).and_then(|id| st.mirror.items.get(&(id as u32))) else {
                return Ok(Value::Nil);
            };
            match stats_value(lua, info, &name)? {
                Some(v) => Ok(v),
                None => Err(mlua::Error::runtime(format!(
                    "GetItemStatsField: unknown field '{name}'"
                ))),
            }
        },
    )?;

    let n = np.clone();
    set(lua, g, "GetItemLevel", move |_, id: Value| {
        let st = n.lock();
        match int(&id).and_then(|id| st.mirror.items.get(&(id as u32))) {
            Some(info) => Ok(info.item_level),
            None => Err(mlua::Error::runtime("GetItemLevel: unknown item id")),
        }
    })?;

    let n = np.clone();
    set(lua, g, "GetItemIconTexture", move |_, id: Value| {
        Ok(int(&id).and_then(|id| n.db.item_icon(id as u32)))
    })?;

    // `FindPlayerItemSlot(idOrName)`: equipment first (nil, slot 0-18), then the bags in
    // order, 1-based slots.
    let n = np.clone();
    set(lua, g, "FindPlayerItemSlot", move |_, want: Value| {
        let st = n.lock();
        let hit = matches(&st, &want);
        for (slot, guid) in equipped(&st).into_iter().enumerate() {
            if hit(entry(&st, guid)) {
                return Ok((None, Some(slot as i64)));
            }
        }
        for bag in [0, 1, 2, 3, 4, -1, 5, 6, 7, 8, 9, 10, -2] {
            for (slot, guid) in bag_slots(&st, bag)
                .unwrap_or_default()
                .into_iter()
                .enumerate()
            {
                if hit(entry(&st, guid)) {
                    return Ok((Some(bag), Some(slot as i64 + 1)));
                }
            }
        }
        Ok((None, None))
    })?;

    let n = np.clone();
    set(lua, g, "GetEquippedItems", move |lua, token: Value| {
        let st = n.lock();
        let token = if token.is_nil() {
            Value::String(lua.create_string("player")?)
        } else {
            token
        };
        let Some(guid) = unit(&st, &token) else {
            return Ok(Value::Nil);
        };
        let t = lua.create_table()?;
        if guid == st.mirror.player {
            for (slot, item) in equipped(&st).into_iter().enumerate() {
                if let Some(f) = st.mirror.objects.get(&item) {
                    t.set(slot + 1, own_item(lua, f)?)?;
                }
            }
        } else {
            let Some(f) = st.mirror.objects.get(&guid) else {
                return Ok(Value::Nil);
            };
            for slot in 0..19 {
                if let Some(item) = visible_item(lua, f, slot)? {
                    t.set(slot + 1, item)?;
                }
            }
        }
        Ok(Value::Table(t))
    })?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetEquippedItem",
        move |lua, (token, slot): (Value, Value)| {
            let st = n.lock();
            let (Some(guid), Some(slot)) = (unit(&st, &token), int(&slot)) else {
                return Ok(None);
            };
            if !(1..=19).contains(&slot) {
                return Ok(None);
            }
            let slot = slot as usize - 1;
            if guid == st.mirror.player {
                let item = equipped(&st)[slot];
                return st
                    .mirror
                    .objects
                    .get(&item)
                    .map(|f| own_item(lua, f))
                    .transpose();
            }
            match st.mirror.objects.get(&guid) {
                Some(f) => visible_item(lua, f, slot),
                None => Ok(None),
            }
        },
    )?;

    let n = np.clone();
    set(lua, g, "GetBagItems", move |lua, bag: Value| {
        let st = n.lock();
        let one = |bag: i64| -> mlua::Result<Option<Table>> {
            let Some(slots) = bag_slots(&st, bag) else {
                return Ok(None);
            };
            let t = lua.create_table()?;
            for (i, guid) in slots.into_iter().enumerate() {
                if let Some(f) = st.mirror.objects.get(&guid) {
                    t.set(i + 1, own_item(lua, f)?)?;
                }
            }
            Ok(Some(t))
        };
        if let Some(bag) = int(&bag) {
            return Ok(one(bag)?.map(Value::Table).unwrap_or(Value::Nil));
        }
        let all = lua.create_table()?;
        for bag in [0, 1, 2, 3, 4, -1, 5, 6, 7, 8, 9, 10, -2] {
            if let Some(t) = one(bag)? {
                all.set(bag, t)?;
            }
        }
        Ok(Value::Table(all))
    })?;

    let n = np.clone();
    set(
        lua,
        g,
        "GetBagItem",
        move |lua, (bag, slot): (Value, Value)| {
            let st = n.lock();
            let (Some(bag), Some(slot)) = (int(&bag), int(&slot)) else {
                return Ok(None);
            };
            let guid = bag_slots(&st, bag)
                .and_then(|s| s.get((slot - 1).max(0) as usize).copied())
                .filter(|_| slot >= 1)
                .unwrap_or(0);
            st.mirror
                .objects
                .get(&guid)
                .map(|f| own_item(lua, f))
                .transpose()
        },
    )?;

    // `GetAmmo()`: the equipped ammo and how many the backpack and bags hold.
    let n = np.clone();
    set(lua, g, "GetAmmo", move |_, ()| {
        let st = n.lock();
        let ammo = st
            .mirror
            .player_fields()
            .map_or(0, |f| f.u32(field::PLAYER_AMMO_ID));
        if ammo == 0 {
            return Ok((None, None));
        }
        let count: u32 = (0..=4)
            .flat_map(|bag| bag_slots(&st, bag).unwrap_or_default())
            .filter(|g| entry(&st, *g) == ammo)
            .filter_map(|g| st.mirror.objects.get(&g))
            .map(|f| f.u32(field::ITEM_STACK_COUNT))
            .sum();
        Ok((Some(ammo), Some(count)))
    })?;

    // `GetTrinkets()`: equipped trinkets (bag nil, slot 1-2), then trinkets in bags 0-4.
    let n = np.clone();
    set(lua, g, "GetTrinkets", move |lua, _copy: Value| {
        let st = n.lock();
        let out = lua.create_table()?;
        let push = |item_id: u32, bag: Option<i64>, slot: i64| -> mlua::Result<()> {
            let info = st.mirror.items.get(&item_id);
            let t = lua.create_table()?;
            t.set("itemId", item_id)?;
            t.set(
                "trinketName",
                info.map_or_else(|| "Unknown".to_string(), |i| i.name.clone()),
            )?;
            t.set(
                "texture",
                info.and_then(|i| n.db.item_icon(i.display_info_id)),
            )?;
            t.set("itemLevel", info.map_or(0, |i| i.item_level))?;
            t.set("bagIndex", bag)?;
            t.set("slotIndex", slot)?;
            out.push(t)
        };
        let worn = equipped(&st);
        for (i, slot) in [12usize, 13].into_iter().enumerate() {
            let id = entry(&st, worn[slot]);
            if id != 0 {
                push(id, None, i as i64 + 1)?;
            }
        }
        for bag in 0..=4 {
            for (i, guid) in bag_slots(&st, bag)
                .unwrap_or_default()
                .into_iter()
                .enumerate()
            {
                let id = entry(&st, guid);
                // `INVTYPE_TRINKET` is 12.
                if id != 0
                    && st
                        .mirror
                        .items
                        .get(&id)
                        .is_some_and(|t| t.inventory_type == 12)
                {
                    push(id, Some(bag), i as i64 + 1)?;
                }
            }
        }
        Ok(out)
    })?;

    // The equipped trinket slot (13 or 14) a slot number, id or name names, or nil.
    let n = np.clone();
    set(lua, g, "NP_TrinketSlot", move |_, want: Value| {
        let st = n.lock();
        let worn = equipped(&st);
        if let Some(i) = match &want {
            Value::Integer(_) | Value::Number(_) => int(&want),
            _ => None,
        } {
            match i {
                1 | 13 => return Ok(Some(13)),
                2 | 14 => return Ok(Some(14)),
                _ => {}
            }
        }
        let hit = matches(&st, &want);
        Ok([13i64, 14]
            .into_iter()
            .find(|slot| hit(entry(&st, worn[*slot as usize - 1]))))
    })?;

    Ok(())
}

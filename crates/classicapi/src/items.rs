//! `Item::Location`, `Item::Arg`, `Item::ID` and `Item::Link` over the mirror. The DLL asks the
//! engine's inventory manager for a slot's `CGItem`; that manager's linear slots are the player
//! descriptor's `PLAYER_FIELD_INV_SLOT_HEAD` guids (equipment 0-18, bags 19-22, backpack 23-38,
//! bank 39-62, bank bags 63-68, buyback 69-80, keyring 81-112), and a bag's own slots are its
//! container descriptor's, so a location resolves to an item guid and the item's descriptor.

use mlua::{Lua, Table, Value};

use crate::lua::{as_string, atoi, is_number, to_int};
use crate::mirror::{field, Fields, Mirror};

/// `BACKPACK_LINEAR_BASE`, the backpack's first linear slot.
pub const BACKPACK_LINEAR_BASE: usize = 23;
pub const BACKPACK_NUM_SLOTS: usize = 16;
pub const BANK_LINEAR_BASE: usize = 39;
pub const BANK_NUM_SLOTS: usize = 24;
pub const BANK_BAG_LINEAR_BASE: usize = 63;
pub const KEYRING_LINEAR_BASE: usize = 81;
pub const KEYRING_NUM_SLOTS: usize = 32;
/// `INVSLOT_BAG1`, the first bag's 1-based equipment slot.
pub const INVSLOT_BAG1: usize = 20;
/// `EQUIPMENT_SLOT_FIRST` and `_LAST`, 1-based.
pub const EQUIPMENT_FIRST: i64 = 1;
pub const EQUIPMENT_LAST: i64 = 19;
/// `KEYRING_CONTAINER` and `BANK_CONTAINER`.
pub const KEYRING_CONTAINER: i64 = -2;
pub const BANK_CONTAINER: i64 = -1;

/// The guid in the player's linear inventory slot `linear`.
pub fn linear_guid(m: &Mirror, linear: usize) -> u64 {
    m.me()
        .map_or(0, |me| me.guid(field::PLAYER_INV_SLOT_HEAD + 2 * linear))
}

/// The container object a bag id names: bags 1-4, bank bags 5-10.
fn bag_object(m: &Mirror, bag: i64) -> Option<&Fields> {
    let linear = match bag {
        1..=4 => INVSLOT_BAG1 - 1 + (bag as usize - 1),
        5..=10 => BANK_BAG_LINEAR_BASE + (bag as usize - 5),
        _ => return None,
    };
    let guid = linear_guid(m, linear);
    (guid != 0).then(|| m.object(guid)).flatten()
}

/// `GetBagSlotCount`: 16 for the backpack, 24 the bank, 32 the keyring, a bag's own capacity.
pub fn bag_slot_count(m: &Mirror, bag: i64) -> usize {
    match bag {
        0 => BACKPACK_NUM_SLOTS,
        BANK_CONTAINER => BANK_NUM_SLOTS,
        KEYRING_CONTAINER => KEYRING_NUM_SLOTS,
        _ => bag_object(m, bag).map_or(0, |b| b.u32(field::CONTAINER_NUM_SLOTS) as usize),
    }
}

/// `ResolveBagSlot`: the item guid at `(bag, slot)`, slot 1-based; `None` for a bad location or an
/// empty slot.
pub fn bag_slot(m: &Mirror, bag: i64, slot: i64) -> Option<u64> {
    if slot < 1 || slot as usize > bag_slot_count(m, bag) {
        return None;
    }
    let s = slot as usize - 1;
    let guid = match bag {
        0 => linear_guid(m, BACKPACK_LINEAR_BASE + s),
        BANK_CONTAINER => linear_guid(m, BANK_LINEAR_BASE + s),
        KEYRING_CONTAINER => linear_guid(m, KEYRING_LINEAR_BASE + s),
        _ => bag_object(m, bag)?.guid(field::CONTAINER_SLOT_1 + 2 * s),
    };
    (guid != 0).then_some(guid)
}

/// `ResolveEquipmentSlot`: the item guid in 1-based character-pane slot `slot`.
pub fn equipment_slot(m: &Mirror, slot: i64) -> Option<u64> {
    if !(1..=23).contains(&slot) {
        return None;
    }
    let guid = linear_guid(m, slot as usize - 1);
    (guid != 0).then_some(guid)
}

/// `Item::ID::FromCGItem`: the item's entry; 0 for an unknown object.
pub fn item_id(m: &Mirror, guid: u64) -> u32 {
    m.object(guid).map_or(0, Fields::entry)
}

/// `Item::Arg::Resolved`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arg {
    pub item_id: i64,
    /// The `item:` string's third field, the random-property id.
    pub suffix: i64,
    pub name: Option<String>,
}

impl Arg {
    pub fn is_empty(&self) -> bool {
        self.item_id <= 0 && self.name.is_none()
    }
}

/// `Item::Arg::Resolve`: a number is an item id; a string is an `item:` link, a numeric id, or
/// a name.
pub fn resolve_arg(v: &Value) -> Arg {
    if is_number(v) {
        return Arg {
            item_id: to_int(v),
            ..Arg::default()
        };
    }
    match as_string(v) {
        Some(s) => resolve_string(&s),
        None => Arg::default(),
    }
}

/// `Item::Arg::ResolveString`.
pub fn resolve_string(s: &str) -> Arg {
    if let Some(at) = s.find("item:") {
        let rest = &s[at + 5..];
        let mut out = Arg {
            item_id: atoi(rest),
            ..Arg::default()
        };
        let mut colons = 0;
        for (i, c) in rest.char_indices() {
            if c == '|' {
                break;
            }
            if c == ':' {
                colons += 1;
                if colons == 2 {
                    out.suffix = atoi(&rest[i + 1..]);
                    break;
                }
            }
        }
        return out;
    }
    let numeric = atoi(s);
    if numeric > 0 {
        Arg {
            item_id: numeric,
            ..Arg::default()
        }
    } else {
        Arg {
            name: Some(s.to_string()),
            ..Arg::default()
        }
    }
}

/// `Guid::Parse`.
pub fn parse_guid(s: &str) -> Option<u64> {
    crate::guid::parse(s)
}

/// `Guid::FormatAsString`, the guid string form.
pub fn guid_string(guid: u64) -> String {
    crate::guid::format(guid)
}

/// What a native needs about one item, read under the lock: entry, roll, stack and the object's
/// own descriptor.
#[derive(Clone, Debug)]
pub struct ItemSnap {
    pub guid: u64,
    pub entry: u32,
    pub fields: Fields,
}

impl ItemSnap {
    pub fn of(m: &Mirror, guid: u64) -> Option<Self> {
        let fields = m.object(guid)?.clone();
        Some(Self {
            guid,
            entry: fields.entry(),
            fields,
        })
    }

    pub fn random_property(&self) -> i32 {
        self.fields.i32(field::ITEM_RANDOM_PROPERTIES_ID)
    }

    pub fn stack(&self) -> u32 {
        self.fields.u32(field::ITEM_STACK_COUNT)
    }
}

/// The item's base template name, asking for the template when it is not cached.
pub fn base_name(lua: &Lua, entry: u32) -> Option<String> {
    crate::itemdb::record(lua, entry)
        .map(|t| t.name.clone())
        .filter(|n| !n.is_empty())
}

/// The engine's name formatter `0x5d8b00(entry, randomPropertyId)`: `ITEM_SUFFIX_TEMPLATE`'s
/// `"%s %s"` with the roll's suffix, or the plain name.
pub fn display_name(lua: &Lua, entry: u32, random_property: i32) -> Option<String> {
    let base = base_name(lua, entry)?;
    let suffix = u32::try_from(random_property)
        .ok()
        .filter(|r| *r != 0)
        .and_then(|r| benilla_ui::script::ext_read::random_property_suffix(lua, r));
    Some(match suffix {
        Some(s) => format!("{base} {s}"),
        None => base,
    })
}

/// The client's link colour escape by quality (`0x52ad90`): 7 and up read white.
pub fn quality_color(quality: u32) -> &'static str {
    match quality {
        0 => "ff9d9d9d",
        2 => "ff1eff00",
        3 => "ff0070dd",
        4 => "ffa335ee",
        5 => "ffff8000",
        6 => "ffe6cc80",
        _ => "ffffffff",
    }
}

/// `0x52adb0`'s hyperlink, `|c<q>|Hitem:id:enchant:roll:factor|h[name]|h|r`, as benilla builds
/// every item link.
pub fn link(lua: &Lua, entry: u32, random_property: i32) -> Option<String> {
    let t = crate::itemdb::record(lua, entry)?;
    let name = display_name(lua, entry, random_property)?;
    Some(format!(
        "|c{}|Hitem:{entry}:0:{random_property}:0|h[{name}]|h|r",
        quality_color(t.quality)
    ))
}

/// `Item::Link::FromCGItem` for an item snapshot.
pub fn item_link(lua: &Lua, item: &ItemSnap) -> Option<String> {
    link(lua, item.entry, item.random_property())
}

/// `MatchesArg`: an exact id, or the decorated display name case-insensitively.
pub fn matches(lua: &Lua, item: &ItemSnap, arg: &Arg) -> bool {
    if item.entry == 0 {
        return false;
    }
    if arg.item_id > 0 {
        return i64::from(item.entry) == arg.item_id;
    }
    let Some(want) = &arg.name else {
        return false;
    };
    display_name(lua, item.entry, item.random_property())
        .is_some_and(|n| n.eq_ignore_ascii_case(want))
}

/// Where a walk found an item: `equipment` 1-19 for a worn item, else `(bag, slot)`.
#[derive(Clone, Debug)]
pub struct Found {
    pub equipment: i64,
    pub bag: i64,
    pub slot: i64,
    pub item: ItemSnap,
}

/// Every item in the equipment slots (1-19), as snapshots, in slot order.
pub fn equipped(m: &Mirror) -> Vec<(i64, ItemSnap)> {
    (EQUIPMENT_FIRST..=EQUIPMENT_LAST)
        .filter_map(|s| Some((s, ItemSnap::of(m, equipment_slot(m, s)?)?)))
        .collect()
}

/// Every item in bags `bags`, as `(bag, slot, snapshot)` in walk order.
pub fn bagged(m: &Mirror, bags: impl IntoIterator<Item = i64>) -> Vec<(i64, i64, ItemSnap)> {
    let mut out = Vec::new();
    for bag in bags {
        for slot in 1..=bag_slot_count(m, bag) as i64 {
            if let Some(item) = bag_slot(m, bag, slot).and_then(|g| ItemSnap::of(m, g)) {
                out.push((bag, slot, item));
            }
        }
    }
    out
}

/// A snapshot of the walks the finders need, taken under the lock.
pub struct Carried {
    pub equipped: Vec<(i64, ItemSnap)>,
    pub bags: Vec<(i64, i64, ItemSnap)>,
}

impl Carried {
    pub fn of(m: &Mirror) -> Self {
        Self {
            equipped: equipped(m),
            bags: bagged(m, 0..=4),
        }
    }

    /// `FindByArgInBags`: the first bag item matching `arg`.
    pub fn find_in_bags(&self, lua: &Lua, arg: &Arg) -> Option<Found> {
        if arg.is_empty() {
            return None;
        }
        self.bags
            .iter()
            .find(|(_, _, it)| matches(lua, it, arg))
            .map(|(bag, slot, it)| Found {
                equipment: 0,
                bag: *bag,
                slot: *slot,
                item: it.clone(),
            })
    }

    /// `FindByArg`: equipment first, then bags.
    pub fn find(&self, lua: &Lua, arg: &Arg) -> Option<Found> {
        if arg.is_empty() {
            return None;
        }
        if let Some((s, it)) = self.equipped.iter().find(|(_, it)| matches(lua, it, arg)) {
            return Some(Found {
                equipment: *s,
                bag: 0,
                slot: 0,
                item: it.clone(),
            });
        }
        self.find_in_bags(lua, arg)
    }

    /// `FindByGUID`: equipment first, then bags.
    pub fn find_guid(&self, guid: u64) -> Option<Found> {
        if guid == 0 {
            return None;
        }
        if let Some((s, it)) = self.equipped.iter().find(|(_, it)| it.guid == guid) {
            return Some(Found {
                equipment: *s,
                bag: 0,
                slot: 0,
                item: it.clone(),
            });
        }
        self.bags
            .iter()
            .find(|(_, _, it)| it.guid == guid)
            .map(|(bag, slot, it)| Found {
                equipment: 0,
                bag: *bag,
                slot: *slot,
                item: it.clone(),
            })
    }
}

/// `TryReadIntField`: `t[name]` when it is a number.
fn int_field(t: &Table, name: &str) -> Option<i64> {
    let v: Value = t.get(name).ok()?;
    is_number(&v).then(|| to_int(&v))
}

/// `IsLocationArg`: a table or a string.
pub fn is_location_arg(v: &Value) -> bool {
    matches!(v, Value::Table(_) | Value::String(_))
}

/// `ResolveLocationDetail`: a guid string, `{equipmentSlotIndex = N}` or `{bagID, slotIndex}`.
pub fn resolve_location(m: &Mirror, v: &Value) -> Option<Found> {
    match v {
        Value::String(s) => {
            let guid = parse_guid(&s.to_string_lossy())?;
            Carried::of(m).find_guid(guid)
        }
        Value::Table(t) => {
            if let Some(eq) = int_field(t, "equipmentSlotIndex") {
                let item = ItemSnap::of(m, equipment_slot(m, eq)?)?;
                return Some(Found {
                    equipment: eq,
                    bag: 0,
                    slot: 0,
                    item,
                });
            }
            let (bag, slot) = (int_field(t, "bagID")?, int_field(t, "slotIndex")?);
            let item = ItemSnap::of(m, bag_slot(m, bag, slot)?)?;
            Some(Found {
                equipment: 0,
                bag,
                slot,
                item,
            })
        }
        _ => None,
    }
}

/// `FindItemArgOrLocation`: a table or a guid string is a location; anything else an item
/// reference searched for in the bags.
pub fn resolve_item_or_location(lua: &Lua, ca: &crate::Ca, v: &Value) -> Option<Found> {
    if let Value::Table(_) = v {
        return resolve_location(&ca.lock().mirror, v);
    }
    if let Value::String(s) = v {
        if let Some(guid) = parse_guid(&s.to_string_lossy()) {
            return Carried::of(&ca.lock().mirror).find_guid(guid);
        }
    }
    let arg = resolve_arg(v);
    if arg.is_empty() {
        return None;
    }
    let carried = Carried::of(&ca.lock().mirror);
    carried.find_in_bags(lua, &arg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_strings_resolve_like_the_dll() {
        assert_eq!(
            resolve_string("|cff1eff00|Hitem:6948:0:5:0|h[x]|h|r").item_id,
            6948
        );
        assert_eq!(resolve_string("item:6948:0:705:0").suffix, 705);
        assert_eq!(resolve_string("item:6948").suffix, 0);
        assert_eq!(resolve_string("6948").item_id, 6948);
        assert_eq!(
            resolve_string("Hearthstone").name.as_deref(),
            Some("Hearthstone")
        );
        assert_eq!(
            parse_guid("0x4000000000000123"),
            Some(0x4000_0000_0000_0123)
        );
        assert_eq!(parse_guid("0x123"), None);
        assert_eq!(parse_guid("0x00000123"), Some(0x123));
        assert_eq!(guid_string(0x123), "0x0000000000000123");
    }
}

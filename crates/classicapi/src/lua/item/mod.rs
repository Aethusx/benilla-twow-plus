//! `item/`: `C_Item` and the item globals over the template records ([`crate::itemdb`]) and the
//! carried items' descriptors ([`crate::items`]).

use std::sync::Arc;

use benilla_protocol::ItemInfo;
use mlua::{Lua, Value};

use crate::items::{self, ItemSnap};
use crate::lua::Api;
use crate::Ca;

pub(crate) mod actions;
pub(crate) mod data;
pub(crate) mod inventory;
mod newitems;
pub(crate) mod stats;

pub(crate) use data::{icon_for_display, on_use_spell};
pub(crate) use newitems::NewItems;

/// `Item::Arg::ResolveItemID`: an id, an `item:` string's id, a numeric string; 0 for a name.
pub(crate) fn arg_id(v: &Value) -> i64 {
    items::resolve_arg(v).item_id
}

/// The item a location names (`Item::Location::Resolve`).
pub(crate) fn located(ca: &Ca, v: &Value) -> Option<ItemSnap> {
    items::resolve_location(&ca.lock().mirror, v).map(|f| f.item)
}

/// `IsLocationArg`, else the usage error the location natives raise.
pub(crate) fn location_arg(v: &Value, usage: &str) -> mlua::Result<()> {
    if items::is_location_arg(v) {
        Ok(())
    } else {
        Err(mlua::Error::runtime(usage.to_string()))
    }
}

/// `PeekRecord` with `WarmCache` on a miss, by id; `None` for an id at or below 0.
pub(crate) fn record(lua: &Lua, id: i64) -> Option<Arc<ItemInfo>> {
    u32::try_from(id)
        .ok()
        .filter(|i| *i > 0)
        .and_then(|i| crate::itemdb::record(lua, i))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    data::install(api)?;
    stats::install(api)?;
    inventory::install(api)?;
    actions::install(api)?;
    newitems::install(api)?;
    Ok(())
}

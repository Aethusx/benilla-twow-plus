//! `item/scrape/` and `mail/`: the `Get*ItemID` companions of the stock link getters, and
//! `GetInboxItemLink` / `GetSendMailItemLink`.
//!
//! The DLL reads each item id from the engine storage behind the stock getter; here each asks
//! that getter for its link and reads the id out of it, so the answer is the stock one's by
//! construction. The inbox, the auction sell slot and the mail attachment have no stock link
//! getter and read benilla's model.

use mlua::{IntoLuaMulti, Lua, MultiValue, Value};

use crate::items;
use crate::lua::{is_number, to_int, Api};

/// `(name, stock link getter, argument kinds)`: `n` a number, `s` a string.
const COMPANIONS: &[(&str, &str, &str)] = &[
    ("GetAuctionItemID", "GetAuctionItemLink", "sn"),
    ("GetCraftReagentItemID", "GetCraftReagentItemLink", "nn"),
    ("GetLootRollItemID", "GetLootRollItemLink", "n"),
    ("GetLootSlotItemID", "GetLootSlotLink", "n"),
    ("GetMerchantItemID", "GetMerchantItemLink", "n"),
    ("GetQuestItemID", "GetQuestItemLink", "sn"),
    ("GetQuestLogItemID", "GetQuestLogItemLink", "sn"),
    ("GetTradePlayerItemID", "GetTradePlayerItemLink", "n"),
    ("GetTradeTargetItemID", "GetTradeTargetItemLink", "n"),
    ("GetTradeSkillItemID", "GetTradeSkillItemLink", "n"),
    (
        "GetTradeSkillReagentItemID",
        "GetTradeSkillReagentItemLink",
        "nn",
    ),
];

/// The item id in a link, `None` for no link or no id.
fn link_id(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => Some(items::resolve_string(&s.to_string_lossy()).item_id),
        _ => None,
    }
    .filter(|id| *id > 0)
}

/// `(link, itemID)` with the basic `item:N` link, or nothing.
fn link_and_id(lua: &Lua, id: Option<u32>) -> mlua::Result<MultiValue> {
    match id.and_then(|id| Some((items::link(lua, id, 0)?, id))) {
        Some(pair) => pair.into_lua_multi(lua),
        None => Ok(MultiValue::new()),
    }
}

/// The optional modern attachment index: absent or 1 is vanilla's one attachment.
fn first_attachment(v: Option<&Value>) -> bool {
    match v {
        Some(v) if is_number(v) => to_int(v) == 1,
        _ => true,
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    for (name, getter, kinds) in COMPANIONS {
        let params: Vec<&str> = kinds
            .chars()
            .map(|k| if k == 's' { "type" } else { "index" })
            .collect();
        let usage = format!("Usage: {name}({})", params.join(", "));
        api.global(name, move |lua, args: MultiValue| {
            let a: Vec<Value> = args.into_iter().collect();
            let ok = kinds.chars().enumerate().all(|(i, k)| match a.get(i) {
                Some(v) if k == 'n' => is_number(v),
                Some(Value::String(_)) => k == 's',
                _ => false,
            });
            if !ok {
                return Err(mlua::Error::runtime(usage.clone()));
            }
            let Ok(f) = lua.globals().get::<mlua::Function>(*getter) else {
                return Ok(None);
            };
            let link: Value = f
                .call::<MultiValue>(MultiValue::from_vec(a))
                .ok()
                .and_then(|r| r.into_iter().next())
                .unwrap_or(Value::Nil);
            Ok(link_id(&link))
        })?;
    }

    api.global("GetAuctionSellItemID", |lua, ()| {
        Ok(benilla_ui::script::ext_read::auction_sell_item(lua)
            .map(|(id, _)| id)
            .filter(|id| *id != 0))
    })?;

    api.global("GetInboxItemID", |lua, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: GetInboxItemID(mailID)"));
        }
        let row = usize::try_from(to_int(&v)).unwrap_or(0);
        Ok(benilla_ui::script::ext_read::inbox_item(lua, row))
    })?;

    // `(link, itemID)`: the basic link, as 3.3.5's ignores the per-instance fields.
    api.global("GetInboxItemLink", |lua, args: MultiValue| {
        let a: Vec<Value> = args.into_iter().collect();
        let Some(row) = a.first().filter(|v| is_number(v)) else {
            return Err(mlua::Error::runtime(
                "Usage: GetInboxItemLink(messageIndex[, attachmentIndex])",
            ));
        };
        if !first_attachment(a.get(1)) {
            return Ok(MultiValue::new());
        }
        let row = usize::try_from(to_int(row)).unwrap_or(0);
        link_and_id(lua, benilla_ui::script::ext_read::inbox_item(lua, row))
    })?;

    // `(link, itemID)`: the attached instance's own link.
    api.global("GetSendMailItemLink", |lua, args: MultiValue| {
        let a: Vec<Value> = args.into_iter().collect();
        if !first_attachment(a.first()) {
            return Ok(MultiValue::new());
        }
        match benilla_ui::script::ext_read::send_mail_item(lua) {
            Some((id, Some(link))) if id != 0 => (link, id).into_lua_multi(lua),
            Some((id, None)) if id != 0 => link_and_id(lua, Some(id)),
            _ => Ok(MultiValue::new()),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn a_companion_reads_the_id_out_of_the_stock_link() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                function GetMerchantItemLink(i)
                  if i == 2 then return "|cff1eff00|Hitem:4338:0:0:0|h[Mageweave Cloth]|h|r" end
                end
                return tostring(GetMerchantItemID(2)) .. " " .. tostring(GetMerchantItemID(3))
                  .. " " .. tostring(pcall(GetMerchantItemID, "x"))
                  .. " " .. tostring(GetInboxItemID(1)) .. " " .. tostring(GetAuctionSellItemID())
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "4338 nil false nil nil");
    }
}

//! `equipmentset/Action.cpp`: equipment sets on the action bar. `C_EquipmentSet.PickupEquipmentSet`
//! puts a set on the cursor, a bar slot takes it, and using the slot fires `WEAR_EQUIPMENT_SET`
//! for the UI's EquipmentManager to equip, as 3.3.5's fourth action type does.
//!
//! The DLL writes the set into the engine's action table and hooks each reader. benilla's table
//! is the app's, so the set is an overlay instead: the slot stays empty in the engine and on the
//! server (whose `CMSG_SET_ACTION_BUTTON` handler drops the type, which is why the DLL sends the
//! slot empty too), the set file records the placement, and the action verbs are wrapped to
//! answer for it. A slot the engine fills wins and the set leaves it, the DLL's "server wins" at
//! login. The held set is the engine's crate payload (`ext_read::set_cursor_ext`).

use benilla_ui::script::ext_read::{self, CursorView};
use benilla_ui::script::CursorExt;
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Value};

use super::equipmentset::{self as sets, Face};
use crate::lua::{is_number, none, to_number, Api};
use crate::Ca;

const TAG: &str = "equipmentset";
const SLOTS: i64 = 120;

/// The stock action verbs, captured before they are wrapped.
#[derive(Clone)]
struct Stock {
    has: Option<Function>,
    texture: Option<Function>,
    text: Option<Function>,
    usable: Option<Function>,
    use_action: Option<Function>,
    pickup: Option<Function>,
    place: Option<Function>,
}

fn call(f: &Option<Function>, args: MultiValue) -> mlua::Result<MultiValue> {
    match f {
        Some(f) => f.call(args),
        None => Ok(none()),
    }
}

/// `IconPath`: `Interface\Icons\` and the icon's last path segment.
fn icon_path(icon: &str) -> String {
    let base = icon.rsplit(['\\', '/']).next().unwrap_or(icon);
    format!("Interface\\Icons\\{base}")
}

/// `UseAction`'s cursor flag: `nil`, `false` and `0` are false.
fn flag_set(v: &Value) -> bool {
    match v {
        Value::Nil | Value::Boolean(false) => false,
        v if is_number(v) => to_number(v) != 0.0,
        _ => true,
    }
}

/// A 1-based action id from a Lua argument, `None` off the table.
fn action_arg(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    is_number(v)
        .then(|| to_number(v) as i64)
        .filter(|a| (1..=SLOTS).contains(a))
}

/// The set shown on action `a`: the slot's set while the engine's slot is empty. A slot the
/// engine has filled drops the set, as the DLL's notifier does for any engine-placed entry.
fn set_at(lua: &Lua, ca: &Ca, a: i64) -> Option<Face> {
    let face = sets::on_slot(lua, ca, a - 1)?;
    if ext_read::action_slot(lua, a as u32).is_some() {
        sets::set_action_slot(lua, ca, a - 1, 0);
        return None;
    }
    Some(face)
}

/// The set on the cursor.
fn held(lua: &Lua) -> Option<u32> {
    match ext_read::cursor(lua) {
        Some(CursorView::Ext { tag, id }) if tag == TAG => Some(id),
        _ => None,
    }
}

/// `Pickup`: the set onto the cursor, clearing what it held; false for no such set.
fn pickup(lua: &Lua, ca: &Ca, id: u32) -> bool {
    let Some((_, _, icon)) = sets::face(lua, ca, id) else {
        return false;
    };
    ext_read::set_cursor_ext(
        lua,
        Some(CursorExt {
            tag: TAG.into(),
            id,
            texture: Some(icon_path(&icon)),
        }),
    );
    true
}

/// `PlaceSet`: the held set into action `a`. What the slot held, an engine action or another
/// set, goes onto the cursor; placing a set onto itself only clears the cursor.
fn place(lua: &Lua, ca: &Ca, stock: &Stock, a: i64, id: u32) -> mlua::Result<()> {
    let same = set_at(lua, ca, a).is_some_and(|(s, _, _)| s == id);
    ext_read::set_cursor_ext(lua, None);
    if same || sets::face(lua, ca, id).is_none() {
        return Ok(());
    }
    if let Some((old, _, _)) = set_at(lua, ca, a) {
        pickup(lua, ca, old);
    } else if ext_read::action_slot(lua, a as u32).is_some() {
        call(&stock.pickup, a.into_lua_multi(lua)?)?;
    }
    sets::set_action_slot(lua, ca, a - 1, id);
    sets::repaint(lua, &[a - 1]);
    Ok(())
}

/// An engine verb that may place the cursor's payload onto `a`: a set it covered hops onto the
/// cursor, the DLL's `Place_h` tail.
fn engine_place(
    lua: &Lua,
    ca: &Ca,
    f: &Option<Function>,
    a: i64,
    args: MultiValue,
) -> mlua::Result<MultiValue> {
    let before = set_at(lua, ca, a);
    let out = call(f, args)?;
    if let Some((id, _, _)) = before {
        if ext_read::action_slot(lua, a as u32).is_some() && ext_read::cursor(lua).is_none() {
            sets::set_action_slot(lua, ca, a - 1, 0);
            pickup(lua, ca, id);
        }
    }
    Ok(out)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let g = api.lua.globals();
    let stock = Stock {
        has: g.get("HasAction").ok(),
        texture: g.get("GetActionTexture").ok(),
        text: g.get("GetActionText").ok(),
        usable: g.get("IsUsableAction").ok(),
        use_action: g.get("UseAction").ok(),
        pickup: g.get("PickupAction").ok(),
        place: g.get("PlaceAction").ok(),
    };

    let c = api.ca.clone();
    api.table(
        "C_EquipmentSet",
        "PickupEquipmentSet",
        move |lua, v: Value| {
            if !is_number(&v) {
                return Err(mlua::Error::runtime(
                    "Usage: C_EquipmentSet.PickupEquipmentSet(setID)",
                ));
            }
            let id = to_number(&v);
            if id >= 1.0 {
                pickup(lua, &c, id as u32);
            }
            Ok(())
        },
    )?;

    let (c, s) = (api.ca.clone(), stock.clone());
    api.global(
        "HasAction",
        move |lua, args: MultiValue| match action_arg(args.front()).and_then(|a| set_at(lua, &c, a))
        {
            Some(_) => 1.into_lua_multi(lua),
            None => call(&s.has, args),
        },
    )?;
    let (c, s) = (api.ca.clone(), stock.clone());
    api.global(
        "GetActionTexture",
        move |lua, args: MultiValue| match action_arg(args.front()).and_then(|a| set_at(lua, &c, a))
        {
            Some((_, _, icon)) => icon_path(&icon).into_lua_multi(lua),
            None => call(&s.texture, args),
        },
    )?;
    let (c, s) = (api.ca.clone(), stock.clone());
    api.global(
        "GetActionText",
        move |lua, args: MultiValue| match action_arg(args.front()).and_then(|a| set_at(lua, &c, a))
        {
            Some((_, name, _)) => name.into_lua_multi(lua),
            None => call(&s.text, args),
        },
    )?;
    // `Usable_h`: unusable while an item of the set is mid-transaction.
    let (c, s) = (api.ca.clone(), stock.clone());
    api.global(
        "IsUsableAction",
        move |lua, args: MultiValue| match action_arg(args.front()).and_then(|a| set_at(lua, &c, a))
        {
            Some((id, _, _)) => {
                let usable = sets::contains_locked(lua, &c, id)? == Some(false);
                (usable.then_some(1), Value::Nil).into_lua_multi(lua)
            }
            None => call(&s.usable, args),
        },
    )?;
    // `UseAction_h`: a place with the cursor check, else `WEAR_EQUIPMENT_SET` for a set slot.
    let (c, s) = (api.ca.clone(), stock.clone());
    api.global("UseAction", move |lua, args: MultiValue| {
        let Some(a) = action_arg(args.front()) else {
            return call(&s.use_action, args);
        };
        if args.get(1).is_some_and(flag_set) {
            if let Some(id) = held(lua) {
                place(lua, &c, &s, a, id)?;
                return Ok(none());
            }
            if ext_read::cursor(lua).is_some() {
                return engine_place(lua, &c, &s.use_action, a, args);
            }
        }
        let Some((id, _, _)) = set_at(lua, &c, a) else {
            return call(&s.use_action, args);
        };
        ext_read::fire_event(
            lua,
            "WEAR_EQUIPMENT_SET",
            vec![benilla_app::ext::ScriptValue::Int(i64::from(id))],
        );
        Ok(none())
    })?;
    // `Pickup_h`: a held set places; a set slot under an empty cursor is picked up and cleared.
    let (c, s) = (api.ca.clone(), stock.clone());
    api.global("PickupAction", move |lua, args: MultiValue| {
        let Some(a) = action_arg(args.front()) else {
            return call(&s.pickup, args);
        };
        if let Some(id) = held(lua) {
            place(lua, &c, &s, a, id)?;
            return true.into_lua_multi(lua);
        }
        if ext_read::cursor(lua).is_none() {
            if let Some((id, _, _)) = set_at(lua, &c, a) {
                pickup(lua, &c, id);
                sets::set_action_slot(lua, &c, a - 1, 0);
                sets::repaint(lua, &[a - 1]);
                return true.into_lua_multi(lua);
            }
        }
        engine_place(lua, &c, &s.pickup, a, args)
    })?;
    let (c, s) = (api.ca.clone(), stock);
    api.global("PlaceAction", move |lua, args: MultiValue| {
        let Some(a) = action_arg(args.front()) else {
            return call(&s.place, args);
        };
        if let Some(id) = held(lua) {
            place(lua, &c, &s, a, id)?;
            return true.into_lua_multi(lua);
        }
        engine_place(lua, &c, &s.place, a, args)
    })?;

    // `GameTooltip:SetAction` on a set slot: the set's summary, refreshed while shown.
    let c = api.ca.clone();
    let stock_tip: std::rc::Rc<std::cell::RefCell<Option<Function>>> = Default::default();
    let stock_tip2 = stock_tip.clone();
    let wrapper = api.lua.create_function(move |lua, args: MultiValue| {
        if let (Some(Value::Table(tip)), Some(a)) = (args.front(), action_arg(args.get(1))) {
            if let Some((_, name, _)) = set_at(lua, &c, a) {
                use mlua::ObjectLike;
                tip.call_method::<()>("SetEquipmentSet", name)?;
                return 1.into_lua_multi(lua);
            }
        }
        match stock_tip2.borrow().as_ref() {
            Some(f) => f.call::<MultiValue>(args),
            None => Ok(none()),
        }
    })?;
    // A VM without the tooltip methods (a bare test VM) has nothing to wrap.
    if let Ok(old) = ext_read::replace_tooltip_method(api.lua, "SetAction", wrapper) {
        *stock_tip.borrow_mut() = old;
    }
    Ok(())
}

/// `GetActionInfo`'s set arm: `"equipmentset", name`.
pub(super) fn action_info(lua: &Lua, ca: &Ca, a: i64) -> Option<String> {
    set_at(lua, ca, a).map(|(_, name, _)| name)
}

/// `GetCursorInfo`'s set arm: `"equipmentset", name`.
pub(super) fn cursor_info(lua: &Lua, ca: &Ca) -> Option<String> {
    sets::face(lua, ca, held(lua)?).map(|(_, name, _)| name)
}

#[cfg(test)]
mod tests {
    use benilla_ui::script::ActionSlot;

    use super::super::equipmentset::{seed, Set};

    #[test]
    fn a_set_is_picked_up_placed_used_and_yields_to_the_engine() {
        let ca = crate::Ca::default();
        let mut script = crate::lua::test_support::vm(&ca);
        let dir = std::env::temp_dir().join(format!("ca-setaction-{}", std::process::id()));
        let file = dir.join("sets.txt");
        seed(
            &ca,
            file.clone(),
            vec![Set {
                id: 1,
                name: "Tank".into(),
                icon: "INV_Shield_06".into(),
                ..Set::default()
            }],
        );
        script.set_action(
            6,
            Some(ActionSlot {
                texture: Some(r"Interface\Icons\Spell_Fire_FlameBolt".into()),
                kind: 0,
                action: 133,
                count: 0,
                consumable: false,
            }),
        );
        let out: String = script
            .lua()
            .load(
                r#"
                local worn
                local f = CreateFrame("Frame")
                f:RegisterEvent("WEAR_EQUIPMENT_SET")
                f:SetScript("OnEvent", function() worn = arg1 end)
                local r = {}
                C_EquipmentSet.PickupEquipmentSet(1)
                table.insert(r, table.concat({ GetCursorInfo() }, ","))
                PlaceAction(5)
                table.insert(r, tostring((GetCursorInfo())))
                table.insert(r, HasAction(5) .. GetActionTexture(5) .. GetActionText(5))
                table.insert(r, table.concat({ GetActionInfo(5) }, ","))
                UseAction(5, 0)
                table.insert(r, tostring(worn))
                PickupAction(5)
                table.insert(r, tostring((HasAction(5))) .. "," .. (GetCursorInfo()))
                PlaceAction(6)
                table.insert(r, GetActionText(6) .. "," .. table.concat({ GetCursorInfo() }, ","))
                ClearCursor()
                return table.concat(r, " ")
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            r"equipmentset,Tank nil 1Interface\Icons\INV_Shield_06Tank equipmentset,Tank 1 nil,equipmentset Tank,spell,0,spell,133"
        );
        assert!(std::fs::read_to_string(&file).unwrap().contains("action 6"));
        // The engine fills the slot (a server-sent button): the set leaves it.
        script.set_action(
            6,
            Some(ActionSlot {
                texture: None,
                kind: 0,
                action: 133,
                count: 0,
                consumable: false,
            }),
        );
        let text: mlua::Value = script.lua().load("return GetActionText(6)").eval().unwrap();
        assert!(text.is_nil());
        assert!(!std::fs::read_to_string(&file).unwrap().contains("action 6"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

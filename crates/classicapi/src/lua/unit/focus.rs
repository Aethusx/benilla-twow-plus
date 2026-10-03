//! `unit/Focus.cpp`'s `FocusUnit` / `ClearFocus` and `unit/TokenResolve.cpp`'s `IsUnitToken`.

use mlua::{Lua, Value};

use crate::lua::{to_str, Api};
use crate::Ca;

/// `Unit::Focus::Set`: a change fires `PLAYER_FOCUS_CHANGED` at once; the same unit is a no-op.
fn set_focus(lua: &Lua, ca: &Ca, guid: u64) {
    {
        let mut t = ca.tokens.lock();
        if t.focus == guid {
            return;
        }
        t.focus = guid;
    }
    benilla_ui::script::ext_read::fire_event(lua, "PLAYER_FOCUS_CHANGED", vec![]);
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    // `FocusUnit(unit)`, `"target"` when no token is given, as `/focus` defaults.
    let c = api.ca.clone();
    api.global("FocusUnit", move |lua, v: Value| {
        let token = match &v {
            Value::String(_) | Value::Integer(_) | Value::Number(_) => {
                to_str(&v).unwrap_or_default()
            }
            _ => "target".to_string(),
        };
        let guid = benilla_ui::script::unit_token_guid_in(lua, &token).unwrap_or(0);
        set_focus(lua, &c, guid);
        Ok(())
    })?;
    let c = api.ca.clone();
    api.global("ClearFocus", move |lua, ()| {
        set_focus(lua, &c, 0);
        Ok(())
    })?;
    // `IsUnitToken(value)`: whether the resolver recognises it, never an error; a token naming
    // nobody is still a token.
    // Probed through the resolver as installed, before any addon can replace `UnitExists`.
    let probe: Option<mlua::Function> = api.lua.globals().get("UnitExists").ok();
    api.global("IsUnitToken", move |_, v: Value| {
        let (Some(token), Some(probe)) = (to_str(&v).filter(|s| !s.is_empty()), &probe) else {
            return Ok(false);
        };
        Ok(probe.call::<mlua::MultiValue>(token).is_ok())
    })?;
    Ok(())
}

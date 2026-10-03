//! `frame/`: the frame-level backports.
//!
//! - `ScriptArgs.cpp`: modern positional script arguments, every handler also called as
//!   `(self, [event,] arg1..argN)` beside 1.12's `this`/`event`/`argN` globals, through benilla's
//!   handler seam; on by default, `SetModernScriptArgs(false)` turns it off. Deviation: the DLL
//!   passes them only to a handler whose prototype declares a parameter; a parameterless Lua
//!   function ignores them, so the difference is unobservable.

use mlua::Value;

use crate::lua::{truthy, Api};

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    benilla_ui::script::ext_read::set_modern_script_args(api.lua, true);
    api.global("SetModernScriptArgs", |lua, v: Value| {
        benilla_ui::script::ext_read::set_modern_script_args(lua, truthy(&v));
        Ok(())
    })?;
    api.global("GetModernScriptArgs", |lua, ()| {
        Ok(benilla_ui::script::ext_read::modern_script_args(lua))
    })?;
    Ok(())
}

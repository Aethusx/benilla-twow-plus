//! `src/unit/`: the unit API and its tokens.

use mlua::{Lua, Value};

use super::Api;

mod basic;
pub(crate) mod body;
mod flags;
mod focus;
pub(crate) mod identity;
mod state;
pub(crate) mod world;

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    focus::install(api)?;
    basic::install(api)?;
    flags::install(api)?;
    identity::install(api)?;
    world::install(api)?;
    body::install(api)?;
    state::install(api)?;
    Ok(())
}

/// A `unit` argument: a string, else the `usage` error, as every `Unit*` verb raises.
pub(crate) fn token_arg(v: &Value, usage: &str) -> mlua::Result<String> {
    match v {
        Value::String(_) | Value::Integer(_) | Value::Number(_) => {
            Ok(crate::lua::to_str(v).unwrap_or_default())
        }
        _ => Err(mlua::Error::runtime(usage.to_string())),
    }
}

/// The resolver's guid for a token, raising `Unknown unit name` as the stock verbs do.
pub(crate) fn unit_guid(lua: &Lua, token: &str) -> mlua::Result<Option<u64>> {
    benilla_ui::script::ext_read::unit_guid(lua, token)
}

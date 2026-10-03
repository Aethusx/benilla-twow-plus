//! `luasyntax/`: the Lua 5.1 syntax backport at runtime.
//!
//! - `Transpile.cpp`: every chunk passes [`crate::transpile`] at benilla's compile funnel
//!   ([`benilla_ui::source::set_transform`]), with `__len`, `__mod` and the gated `__addonns` the
//!   rewrites call, and the `_classicapi_*Transpile*` diagnostics.
//! - `StringMethods.cpp`: strings index the `string` table, so `s:upper()` works;
//!   `_classicapi_SetStringMethods(false)` turns it off.
//! - `Upvalues.cpp` lifts 5.0's 32-upvalue parser limit to 5.1's 60, which benilla's 5.1 parser
//!   already has; nothing to port.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, Table, Value};

use crate::lua::{is_number, to_number, truthy, Api};
use crate::transpile::{self, Options};

/// The registry key of the addon-name -> namespace map (`PushAddonNamespace`).
const NS_KEY: &str = "__classicapi_addon_ns";

#[derive(Default)]
struct Shared {
    options: Options,
    /// The addon whose file chunk was just rewritten with the namespace preamble: the one
    /// `__addonns` answers, once.
    grant: Option<String>,
    chunks: u64,
    bytes: u64,
    tokenized: u64,
    tokenized_bytes: u64,
    time: Duration,
}

/// `PushAddonNamespace`: one table per addon, keyed case-folded.
pub(crate) fn addon_namespace(lua: &Lua, name: &str) -> mlua::Result<Table> {
    let map: Table = match lua.named_registry_value::<Value>(NS_KEY)? {
        Value::Table(t) => t,
        _ => {
            let t = lua.create_table()?;
            lua.set_named_registry_value(NS_KEY, t.clone())?;
            t
        }
    };
    let key = name.to_ascii_lowercase();
    if let Value::Table(t) = map.raw_get::<Value>(key.as_str())? {
        return Ok(t);
    }
    let t = lua.create_table()?;
    map.raw_set(key, t.clone())?;
    Ok(t)
}

fn set_string_methods(lua: &Lua, on: bool) -> mlua::Result<()> {
    if on {
        let mt = lua.create_table()?;
        mt.set("__index", lua.globals().get::<Table>("string")?)?;
        lua.set_type_metatable::<mlua::String>(Some(mt));
    } else {
        lua.set_type_metatable::<mlua::String>(None);
    }
    Ok(())
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let lock = |s: &Arc<Mutex<Shared>>| s.lock().unwrap_or_else(|e| e.into_inner()).options;

    let s = shared.clone();
    benilla_ui::source::set_transform(
        api.lua,
        Box::new(move |src, name, kind| {
            let options = lock(&s);
            let started = Instant::now();
            let file = kind == benilla_ui::source::ChunkKind::File;
            let out = transpile::load_rewrite(src, name, file, options);
            let mut st = s.lock().unwrap_or_else(|e| e.into_inner());
            st.chunks += 1;
            st.bytes += src.len() as u64;
            st.time += started.elapsed();
            let (source, wrapped, grant) = out?;
            st.tokenized += 1;
            st.tokenized_bytes += src.len() as u64;
            if grant.is_some() {
                st.grant = grant;
            }
            Some((source, wrapped))
        }),
    );

    api.global("__len", |_, v: Value| match v {
        Value::String(s) => Ok(s.as_bytes().len() as i64),
        Value::Table(t) => Ok(t.raw_len() as i64),
        _ => Err(mlua::Error::runtime(
            "attempt to get length of a non-table, non-string value",
        )),
    })?;
    api.global("__mod", |_, (a, b): (Value, Value)| {
        if !is_number(&a) || !is_number(&b) {
            return Err(mlua::Error::runtime(
                "attempt to perform arithmetic (modulo) on a non-number value",
            ));
        }
        let (a, b) = (to_number(&a), to_number(&b));
        Ok(a - (a / b).floor() * b)
    })?;
    // `__addonns(name)`: the addon's own namespace, only for the chunk just granted it, once.
    let s = shared.clone();
    api.global("__addonns", move |lua, v: Value| {
        let Value::String(name) = v else {
            return Ok(Value::Nil);
        };
        let name = name.to_string_lossy();
        let granted = {
            let mut st = s.lock().unwrap_or_else(|e| e.into_inner());
            match &st.grant {
                Some(g) if g.eq_ignore_ascii_case(&name) => {
                    st.grant = None;
                    true
                }
                _ => false,
            }
        };
        if !granted {
            return Ok(Value::Nil);
        }
        Ok(Value::Table(addon_namespace(lua, &name)?))
    })?;

    let s = shared.clone();
    api.global("_classicapi_Transpile", move |lua, v: Value| {
        let Value::String(src) = v else {
            return Ok(Value::Nil);
        };
        let out = transpile::run(&src.as_bytes(), lock(&s)).map(|r| r.source);
        Ok(Value::String(match out {
            Some(b) => lua.create_string(&b)?,
            None => src,
        }))
    })?;
    let s = shared.clone();
    api.global("_classicapi_TranspileStats", move |_, ()| {
        let st = s.lock().unwrap_or_else(|e| e.into_inner());
        Ok((
            st.chunks,
            st.bytes,
            st.tokenized,
            st.tokenized_bytes,
            st.time.as_secs_f64() * 1000.0,
        ))
    })?;
    let s = shared.clone();
    api.global(
        "_classicapi_SetTranspileOption",
        move |_, (n, v): (Value, Value)| {
            let Value::String(n) = n else {
                return Ok(None);
            };
            let mut st = s.lock().unwrap_or_else(|e| e.into_inner());
            Ok(st.options.flag(&n.to_string_lossy()).map(|f| {
                *f = truthy(&v);
                *f
            }))
        },
    )?;
    let s = shared;
    api.global("_classicapi_GetTranspileOption", move |_, n: Value| {
        let Value::String(n) = n else {
            return Ok(None);
        };
        let mut st = s.lock().unwrap_or_else(|e| e.into_inner());
        Ok(st.options.flag(&n.to_string_lossy()).map(|f| *f))
    })?;

    set_string_methods(api.lua, true)?;
    api.global("_classicapi_SetStringMethods", |lua, v: Value| {
        set_string_methods(lua, truthy(&v))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;
    use crate::Ca;

    #[test]
    fn modern_syntax_compiles_and_runs() {
        let script = vm(&Ca::default());
        script
            .run_chunk_named(
                b"local t = {1, 2, 3}\nN = #t + (-1 % 3) * 10 + 0x10\nS = (\"ab\"):upper()\nF = loadstring(\"return ...\")\n",
                "@Interface\\AddOns\\Test\\a.lua",
            )
            .expect("chunk");
        let n: f64 = script.lua().globals().get("N").unwrap();
        assert_eq!(n, 3.0 + 20.0 + 16.0);
        let s: String = script.lua().globals().get("S").unwrap();
        assert_eq!(s, "AB");
        let f: mlua::Function = script.lua().globals().get("F").unwrap();
        let (a, b): (i64, i64) = f.call((7, 8)).unwrap();
        assert_eq!((a, b), (7, 8));
    }

    #[test]
    fn an_addon_file_gets_its_name_and_namespace() {
        let script = vm(&Ca::default());
        script
            .run_chunk_named(
                b"local name, ns = ...\nNAME = name\nns.x = 1\nNS = ns",
                "@Interface\\AddOns\\Foo\\a.lua",
            )
            .expect("chunk");
        let name: String = script.lua().globals().get("NAME").unwrap();
        assert_eq!(name, "Foo");
        let ns: mlua::Table = script.lua().globals().get("NS").unwrap();
        assert_eq!(ns.get::<i64>("x").unwrap(), 1);
        // The grant is spent: a later call answers nil.
        let again: mlua::Value = script.lua().load("return __addonns('Foo')").eval().unwrap();
        assert!(again.is_nil());
    }
}

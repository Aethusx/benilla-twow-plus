//! `addons/`: `C_AddOns` over the stock addon verbs, and the bundled `!!!ClassicAPI` addon.
//!
//! The DLL serves `Interface\AddOns\!!!ClassicAPI\` from its own image through a hook on the
//! engine's file read and moves the entry to the head of the load order; here the files are in
//! the binary and registered through benilla's embedded-addon seam, which loads them first. The
//! modern `## LoadSavedVariablesFirst` directive is honoured, as the DLL does
//! (`SavedVarsFirst.cpp`). Deviation: the flavor-TOC and flavor-bindings selection, the per-line
//! TOC conditions and the `/reload` rescan (`FlavorToc.cpp`, `FlavorBindings.cpp`,
//! `TocRewrite.cpp`, `Rescan.cpp`) are not built: they rewrite benilla's own addon discovery.

use mlua::{Function, IntoLuaMulti, MultiValue, Value};

use benilla_app::ext::UiScript;

use crate::lua::{none, Api};

mod files {
    include!(concat!(env!("OUT_DIR"), "/embedded_addon.rs"));
}

/// The bundled addon's folder name.
pub(crate) const NAME: &str = "!!!ClassicAPI";

/// Register the bundled addon and the directive on a VM before its UI loads.
pub(crate) fn register(script: &mut UiScript) {
    script.add_embedded_addon(benilla_ui::script::EmbeddedAddon {
        name: NAME.to_string(),
        files: std::sync::Arc::new(
            files::FILES
                .iter()
                .map(|(p, b)| ((*p).to_string(), *b))
                .collect(),
        ),
    });
    script.honor_saved_variables_first(true);
}

/// `GetAddOnInfo`'s seven values for an index or a name.
fn info(get: &Function, key: &Value) -> Option<Vec<Value>> {
    get.call::<MultiValue>(key.clone())
        .ok()
        .map(|m| m.into_iter().collect())
}

fn text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s.to_string_lossy()),
        _ => None,
    }
}

/// `ResolveAddOnName`: an index or a name; anything else is nothing.
fn key(v: &Value) -> Option<Value> {
    match v {
        Value::Integer(_) | Value::Number(_) | Value::String(_) => Some(v.clone()),
        _ => None,
    }
}

/// `AddOnExistsByName`: the registry knows the name (`GetAddOnInfo`'s reason is not `MISSING`).
fn exists(get: &Function, key: &Value) -> bool {
    info(get, key).is_some_and(|i| {
        !matches!(i.first(), None | Some(Value::Nil))
            && text(i.get(5)).as_deref() != Some("MISSING")
    })
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let g = api.lua.globals();
    let get: Function = g.get("GetAddOnInfo")?;
    let loaded: Function = g.get("IsAddOnLoaded")?;

    let gi = get.clone();
    api.table("C_AddOns", "GetAddOnName", move |lua, v: Value| {
        let Some(k) = key(&v) else {
            return Value::Nil.into_lua_multi(lua);
        };
        if matches!(k, Value::String(_)) && !exists(&gi, &k) {
            return Value::Nil.into_lua_multi(lua);
        }
        info(&gi, &k)
            .and_then(|i| i.into_iter().next())
            .unwrap_or(Value::Nil)
            .into_lua_multi(lua)
    })?;
    for (name, idx) in [("GetAddOnTitle", 1), ("GetAddOnNotes", 2)] {
        let gi = get.clone();
        api.table("C_AddOns", name, move |_, v: Value| {
            Ok(key(&v)
                .and_then(|k| info(&gi, &k))
                .and_then(|i| text(i.get(idx))))
        })?;
    }

    // `IsAddOnLoadable(addon)` -> loadable, the reason token.
    let (gi, li) = (get.clone(), loaded.clone());
    api.table("C_AddOns", "IsAddOnLoadable", move |lua, v: Value| {
        let Some(k) = key(&v) else {
            return (false, Value::Nil).into_lua_multi(lua);
        };
        if crate::lua::truthy(&li.call::<Value>(k.clone()).unwrap_or(Value::Nil)) {
            return (true, Value::Nil).into_lua_multi(lua);
        }
        let i = info(&gi, &k).unwrap_or_default();
        let loadable = i.get(4).is_some_and(crate::lua::truthy);
        (loadable, text(i.get(5))).into_lua_multi(lua)
    })?;

    // `GetAddOnSecurity(addon)`: `Enum.AddOnSecurityStatus`.
    let gi = get.clone();
    api.table("C_AddOns", "GetAddOnSecurity", move |_, v: Value| {
        let Some(k) = key(&v).filter(|k| !matches!(k, Value::String(_)) || exists(&gi, k)) else {
            return Ok(3);
        };
        Ok(
            match info(&gi, &k).and_then(|i| text(i.get(6))).as_deref() {
                Some("SECURE") => 0,
                Some("INSECURE") => 1,
                Some("BANNED") => 2,
                _ => 3,
            },
        )
    })?;

    // `IsAddOnLoaded(addon)` -> loadedOrLoading, loaded.
    let li = loaded;
    api.table("C_AddOns", "IsAddOnLoaded", move |lua, v: Value| {
        let Some(k) = key(&v) else {
            return (false, false).into_lua_multi(lua);
        };
        let on = crate::lua::truthy(&li.call::<Value>(k).unwrap_or(Value::Nil));
        (on, on).into_lua_multi(lua)
    })?;

    let gi = get.clone();
    api.table("C_AddOns", "DoesAddOnExist", move |_, v: Value| {
        Ok(match &v {
            Value::Integer(_) | Value::Number(_) => {
                info(&gi, &v).is_some_and(|i| !matches!(i.first(), None | Some(Value::Nil)))
            }
            Value::String(_) => exists(&gi, &v),
            _ => false,
        })
    })?;

    if let Ok(load) = g.get::<Function>("LoadAddOn") {
        api.function(Some("C_AddOns"), "LoadAddOn", load)?;
    }

    // `GetAddOnOptionalDependencies(addon)`: the manifest's `## OptionalDeps`, one value each.
    let gi = get;
    api.table(
        "C_AddOns",
        "GetAddOnOptionalDependencies",
        move |lua, v: Value| {
            let Some(k) = key(&v) else {
                return Err(mlua::Error::runtime(
                    "Usage: C_AddOns.GetAddOnOptionalDependencies(index or \"name\")",
                ));
            };
            let Some(name) = info(&gi, &k)
                .and_then(|i| text(i.first()))
                .filter(|_| exists(&gi, &k))
            else {
                return Ok(none());
            };
            let deps = benilla_ui::script::ext_read::addon_directive(lua, &name, "OptionalDeps")
                .or_else(|| {
                    benilla_ui::script::ext_read::addon_directive(
                        lua,
                        &name,
                        "OptionalDependencies",
                    )
                })
                .unwrap_or_default();
            deps.split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
                .into_lua_multi(lua)
        },
    )?;

    // `GetAddOnLocalTable(name)`: a loaded addon's namespace, when its manifest opts in with
    // `## AllowAddOnTableAccess: 1`.
    let li = g.get::<Function>("IsAddOnLoaded")?;
    api.table("C_AddOns", "GetAddOnLocalTable", move |lua, v: Value| {
        let Value::String(name) = v else {
            return Ok(Value::Nil);
        };
        let name = name.to_string_lossy();
        if name.is_empty() || name.contains(['/', '\\']) {
            return Ok(Value::Nil);
        }
        if !crate::lua::truthy(&li.call::<Value>(name.as_str()).unwrap_or(Value::Nil)) {
            return Ok(Value::Nil);
        }
        let allowed =
            benilla_ui::script::ext_read::addon_directive(lua, &name, "AllowAddOnTableAccess")
                .is_some_and(|v| crate::lua::atoi(&v) != 0);
        if !allowed {
            return Ok(Value::Nil);
        }
        Ok(Value::Table(super::luasyntax::addon_namespace(lua, &name)?))
    })?;

    api.int_enum(
        "Enum",
        "AddOnSecurityStatus",
        &[
            ("Secure", 0),
            ("Insecure", 1),
            ("Banned", 2),
            ("NotAvailable", 3),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_lua_file_compiles_under_the_50_grammar() {
        assert!(files::FILES.iter().any(|(p, _)| *p == "!!!ClassicAPI.toc"));
        let script = crate::lua::test_support::vm(&crate::Ca::default());
        let mut failures = Vec::new();
        for (path, bytes) in files::FILES {
            if !path.ends_with(".lua") {
                continue;
            }
            let name = format!(r"@Interface\AddOns\{NAME}\{}", path.replace('/', r"\"));
            if let Err(e) = benilla_ui::source::compile(
                script.lua(),
                benilla_ui::source::chunk(bytes),
                &name,
                benilla_ui::source::ChunkKind::File,
            ) {
                failures.push(format!("{path}: {e}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}

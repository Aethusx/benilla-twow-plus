//! The file functions: `WriteCustomFile`, `ReadCustomFile`, `CustomFileExists`,
//! `ExecuteCustomLuaFile` in `CustomData`, and SuperWoW's `ImportFile`/`ExportFile` in `Imports`.
//! Deviation: the DLL keeps both folders beside `WoW.exe`; benilla never writes into the install,
//! so they live in the local state folder, `benilla-config/`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use mlua::{Lua, Table, Value};

use super::{set, text};

/// A bare file name: no separator, no `..`, none of the characters Windows refuses.
fn checked(name: &str) -> mlua::Result<&str> {
    let bad = name.is_empty()
        || name == "."
        || name.contains("..")
        || name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c < ' ');
    if bad {
        return Err(mlua::Error::runtime(format!("invalid file name '{name}'")));
    }
    Ok(name)
}

fn folder(sub: &str) -> mlua::Result<PathBuf> {
    benilla_app::ext::local_state_dir()
        .map(|home| home.join(sub))
        .ok_or_else(|| mlua::Error::runtime("no local state folder in this run"))
}

fn path(sub: &str, name: &str) -> mlua::Result<PathBuf> {
    Ok(folder(sub)?.join(checked(name)?))
}

fn read(path: PathBuf) -> mlua::Result<Option<String>> {
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(mlua::Error::runtime(format!("{}: {e}", path.display()))),
    }
}

fn write(path: PathBuf, content: &[u8], append: bool) -> mlua::Result<()> {
    let fail = |e: std::io::Error| mlua::Error::runtime(format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&path)
        .map_err(fail)?;
    f.write_all(content).map_err(fail)
}

pub fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    set(
        lua,
        g,
        "WriteCustomFile",
        |_, (name, content, mode): (String, Value, Option<String>)| {
            let content = text(&content).unwrap_or_default();
            let append = match mode.as_deref().unwrap_or("w") {
                "w" | "b" => false,
                "a" => true,
                m => {
                    return Err(mlua::Error::runtime(format!(
                        "WriteCustomFile: invalid mode '{m}'"
                    )))
                }
            };
            write(path("CustomData", &name)?, content.as_bytes(), append)
        },
    )?;
    set(lua, g, "ReadCustomFile", |_, name: String| {
        read(path("CustomData", &name)?)
    })?;
    set(lua, g, "CustomFileExists", |_, name: String| {
        Ok(path("CustomData", &name)?.is_file())
    })?;
    set(lua, g, "ImportFile", |_, name: String| {
        read(path("Imports", &format!("{name}.txt"))?)
    })?;
    set(
        lua,
        g,
        "ExportFile",
        |_, (name, content): (String, Value)| {
            let content = text(&content).unwrap_or_default();
            write(
                path("Imports", &format!("{name}.txt"))?,
                content.as_bytes(),
                false,
            )
        },
    )?;
    set(lua, g, "ExecuteCustomLuaFile", |lua, name: String| {
        if !name.to_ascii_lowercase().ends_with(".lua") {
            return Err(mlua::Error::runtime(
                "ExecuteCustomLuaFile: only .lua files run",
            ));
        }
        let Some(chunk) = read(path("CustomData", &name)?)? else {
            return Err(mlua::Error::runtime(format!(
                "ExecuteCustomLuaFile: no file '{name}'"
            )));
        };
        lua.load(chunk).set_name(name).exec()
    })?;
    // benilla writes its combat log as it goes; there is no buffer to flush.
    set(lua, g, "CombatLogFlush", |_, ()| Ok(()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bare_names_pass() {
        assert!(checked("notes.txt").is_ok());
        for bad in ["", "..", "a/b", "a\\b", "c:x", "x?", "../up"] {
            assert!(checked(bad).is_err(), "{bad}");
        }
    }
}

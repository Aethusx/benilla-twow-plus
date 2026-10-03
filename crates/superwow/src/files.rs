//! `ImportFile`, `ExportFile` and `CombatLogAdd`. Deviation: the DLL keeps `imports` and `Logs`
//! beside `WoW.exe`; benilla never writes into the install, so both live in the local state
//! folder, `benilla-config/Imports` (nampower's folder for the same two functions) and
//! `benilla-config/Logs`, where benilla's own combat log is.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use mlua::{Lua, Table, Value};

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

/// `Imports/<name>.txt`.
fn import_path(name: &str) -> mlua::Result<PathBuf> {
    Ok(folder("Imports")?.join(format!("{}.txt", checked(name)?)))
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

/// The text of a string or number argument.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string_lossy(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    g.set(
        "ImportFile",
        lua.create_function(|_, name: String| {
            let path = import_path(&name)?;
            match fs::read(&path) {
                Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(mlua::Error::runtime(format!("{}: {e}", path.display()))),
            }
        })?,
    )?;
    g.set(
        "ExportFile",
        lua.create_function(|_, (name, content): (String, Value)| {
            write(import_path(&name)?, text(&content).as_bytes(), false)
        })?,
    )?;
    // CombatLogAdd("text" [, raw]): one stamped line in `WoWCombatLog.txt`, or with the flag in
    // `WoWRawCombatLog.txt`.
    g.set(
        "CombatLogAdd",
        lua.create_function(|_, (line, raw): (Value, Value)| {
            let raw = !matches!(raw, Value::Nil | Value::Boolean(false));
            let name = if raw {
                "WoWRawCombatLog.txt"
            } else {
                "WoWCombatLog.txt"
            };
            let entry = format!("{}  {}\n", benilla_app::ext::log_stamp(), text(&line));
            write(folder("Logs")?.join(name), entry.as_bytes(), true)
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bare_names_pass() {
        assert!(checked("notes").is_ok());
        for bad in ["", "..", "a/b", "a\\b", "c:x", "x?", "../up"] {
            assert!(checked(bad).is_err(), "{bad}");
        }
    }
}

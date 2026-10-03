//! `unit/Identity.cpp`'s Lua surface: `UnitGUID`, `UnitTokenFromGUID`, `UnitTokenFromName`, and
//! the guid-to-token reverse maps other modules fan events out through.

use mlua::{Lua, Value};

use super::{token_arg, unit_guid};
use crate::lua::{truthy, Api};
use crate::Ca;

/// The guid a token names now, 0 for nobody or a token no grammar knows.
fn guid_of(lua: &Lua, token: &str) -> u64 {
    benilla_ui::script::ext_read::unit_guid(lua, token)
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// `TokenFromGUID`: the first token naming `target`, in retail's order with the post-vanilla
/// tokens dropped: player, pet, partyN, partypetN, raidN, raidpetN, nameplateN, target, focus,
/// npc, mouseover.
pub(crate) fn token_from_guid(lua: &Lua, ca: &Ca, target: u64) -> Option<String> {
    if target == 0 {
        return None;
    }
    for t in ["player", "pet"] {
        if guid_of(lua, t) == target {
            return Some(t.to_string());
        }
    }
    for (family, n) in [("party", 4), ("partypet", 4), ("raid", 40), ("raidpet", 40)] {
        for i in 1..=n {
            let t = format!("{family}{i}");
            if guid_of(lua, &t) == target {
                return Some(t);
            }
        }
    }
    if let Some(i) = ca.tokens.lock().plate_index(target) {
        return Some(format!("nameplate{i}"));
    }
    ["target", "focus", "npc", "mouseover"]
        .into_iter()
        .find(|t| guid_of(lua, t) == target)
        .map(str::to_string)
}

/// `TokensForGUID`: every token naming `guid`, the engine's own (player, target, mouseover, pet,
/// party, raid, npc) then `focus` and each `nameplateN`.
pub(crate) fn tokens_for_guid(lua: &Lua, ca: &Ca, guid: u64) -> Vec<String> {
    if guid == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = ["player", "target", "mouseover", "pet"]
        .into_iter()
        .filter(|t| guid_of(lua, t) == guid)
        .map(str::to_string)
        .collect();
    for (family, n) in [("party", 4), ("partypet", 4), ("raid", 40), ("raidpet", 40)] {
        for i in 1..=n {
            let t = format!("{family}{i}");
            if guid_of(lua, &t) == guid {
                out.push(t);
            }
        }
    }
    if guid_of(lua, "npc") == guid {
        out.push("npc".to_string());
    }
    let t = ca.tokens.lock();
    if t.focus == guid {
        out.push("focus".to_string());
    }
    for (i, g) in t.plates.iter().enumerate() {
        if *g == guid {
            out.push(format!("nameplate{}", i + 1));
        }
    }
    out
}

/// One by-name candidate's standing: whole-name match, common-prefix length, squared distance.
type Rank = (bool, usize, f32);

/// Exact, then the longer prefix, then the strictly nearer; a tie keeps the incumbent.
fn beats(a: Rank, b: Rank) -> bool {
    match (a.0, b.0) {
        (true, false) => true,
        (false, true) => false,
        _ => a.1 > b.1 || (a.1 == b.1 && a.2 < b.2),
    }
}

/// The by-name search behind `TargetByName` (`0x493aa0`, typemask 8): a whole-name match, else
/// the longest common prefix unless `exact`, both case-insensitive, nearest first on a tie, as
/// benilla's `/target` ranks it.
fn find_by_name(ca: &Ca, name: &str, exact: bool) -> Option<u64> {
    let st = ca.lock();
    let m = &st.mirror;
    let me = m.place(m.player).map(|p| p.pos);
    let mut best: Option<(Rank, u64)> = None;
    for (guid, n) in &m.names {
        if !matches!(
            crate::guid::classify(*guid),
            crate::guid::Kind::Player | crate::guid::Kind::Creature | crate::guid::Kind::Pet
        ) {
            continue;
        }
        let whole = name.eq_ignore_ascii_case(n);
        let prefix = if whole {
            name.len()
        } else if exact {
            continue;
        } else {
            match name
                .bytes()
                .zip(n.bytes())
                .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                .count()
            {
                0 => continue,
                p => p,
            }
        };
        let d2 = match (me, m.place(*guid)) {
            (Some(a), Some(b)) => (0..3).map(|i| (a[i] - b.pos[i]).powi(2)).sum(),
            _ => f32::MAX,
        };
        let rank = (whole, prefix, d2);
        if best.is_none_or(|(b, _)| beats(rank, b)) {
            best = Some((rank, *guid));
        }
    }
    best.map(|(_, g)| g)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.global("UnitGUID", |lua, v: Value| {
        let token = token_arg(&v, "Usage: UnitGUID(\"unit\")")?;
        Ok(unit_guid(lua, &token)?
            .filter(|g| *g != 0)
            .map(crate::guid::format))
    })?;

    let c = api.ca.clone();
    api.global("UnitTokenFromGUID", move |lua, v: Value| {
        let s = token_arg(&v, "Usage: UnitTokenFromGUID(\"unitGUID\")")?;
        let guid = crate::guid::parse(&s).unwrap_or(0);
        Ok(token_from_guid(lua, &c, guid))
    })?;

    // A standard token when one names the unit, else the guid literal the resolver also takes.
    let c = api.ca.clone();
    api.global(
        "UnitTokenFromName",
        move |lua, (v, exact): (Value, Value)| {
            let name = token_arg(&v, "Usage: UnitTokenFromName(\"name\" [, exactMatch])")?;
            if name.is_empty() {
                return Ok(None);
            }
            let Some(guid) = find_by_name(&c, &name, truthy(&exact)) else {
                return Ok(None);
            };
            Ok(Some(
                token_from_guid(lua, &c, guid).unwrap_or_else(|| crate::guid::format(guid)),
            ))
        },
    )?;
    Ok(())
}

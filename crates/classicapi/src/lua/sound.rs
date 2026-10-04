//! `sound/`: `C_Sound`, the id form of the global `PlaySound`, `MuteSoundFile` /
//! `UnmuteSoundFile`, and `SOUNDKIT_FINISHED`, over benilla's sound-effect funnel
//! ([`benilla_app::ext::ExtSound`]).
//!
//! - A sound kit id is a `SoundEntries.dbc` id. A play is queued under a fresh handle and started
//!   by benilla the same frame, 2D on the effects slider as `PlaySound` plays. Deviation: the DLL
//!   learns at once whether the engine opened a stream; here `willPlay` is whether the kit exists,
//!   and a play a mixer gate drops (the bus cap, a no-duplicates kit) shows as `IsPlaying` false
//!   two frames on, firing `SOUNDKIT_FINISHED` if it asked to.
//! - `channel` and `forceNoDuplicates` are accepted and do nothing, as in the DLL;
//!   `volumeOverride` scales the kit's own volume.
//! - Muting refuses the file at the funnel, so a muted kit plays nothing. The recent list holds the
//!   last 64 distinct files opened, newest first, with `time` in milliseconds on `GetTime()`'s
//!   epoch; it is always on, as the DLL's is.
//! - `PlayItemSound` walks `ItemDisplayInfo` → `ItemGroupSounds` to the type's kit;
//!   `PlayVocalErrorSound` finds the player's race's `VocalUISounds` row for the error.

use std::collections::{HashMap, HashSet};

use benilla_app::ext::{ExtSound, ExtSoundPlay};
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Value};

use crate::lua::{is_number, to_int, to_number, to_str, truthy, Api};
use crate::Ca;

/// How many distinct files the recent list holds.
const RECENT_SLOTS: usize = 64;
/// Frames a handed-off play may go unseen before it counts as dropped.
const AWAIT_FRAMES: u8 = 2;
/// `ItemDisplayInfo.dbc`'s `ItemGroupSounds` id.
const DISPLAY_GROUP_SOUNDS: usize = 11;
/// `VocalUISounds.dbc`: the error line, the race and the male and female kits.
const VOCAL_LINE: usize = 1;
const VOCAL_RACE: usize = 2;
const VOCAL_NORMAL: usize = 3;

/// One recently opened file.
#[derive(Clone, Debug, PartialEq)]
struct Recent {
    file: String,
    seq: u64,
    time_ms: f64,
    muted: bool,
}

/// The sound state the natives and the frame share.
#[derive(Default)]
pub(crate) struct Sound {
    next: u64,
    /// Plays the natives queued, handed to benilla next frame.
    queued: Vec<ExtSoundPlay>,
    /// Handed-off plays not yet seen sounding, with the frames waited.
    awaiting: Vec<(u64, u8)>,
    /// Plays sounding at the last frame, with their volume.
    live: HashMap<u64, f32>,
    /// Plays that asked for `SOUNDKIT_FINISHED`.
    tracked: HashSet<u64>,
    /// Muted paths, lowercased with `\` separators.
    muted: HashSet<String>,
    muted_dirty: bool,
    recent: Vec<Recent>,
    seq: u64,
}

/// `Normalize`: lowercase, `/` folded to `\`.
fn normalize(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

impl Sound {
    fn queue(&mut self, kit: u32, volume: Option<f32>, finish: bool) -> u64 {
        self.next += 1;
        let token = self.next;
        self.queued.push(ExtSoundPlay { token, kit, volume });
        if finish {
            self.tracked.insert(token);
        }
        token
    }

    fn playing(&self, token: u64) -> bool {
        self.queued.iter().any(|p| p.token == token)
            || self.awaiting.iter().any(|a| a.0 == token)
            || self.live.contains_key(&token)
    }

    /// `Record`: a replayed file refreshes its slot, so the list holds distinct files.
    fn record(&mut self, file: String, muted: bool, time_ms: f64) {
        self.seq += 1;
        let seq = self.seq;
        if let Some(r) = self
            .recent
            .iter_mut()
            .find(|r| normalize(&r.file) == normalize(&file))
        {
            r.seq = seq;
            r.time_ms = time_ms;
            r.muted = muted;
            return;
        }
        let entry = Recent {
            file,
            seq,
            time_ms,
            muted,
        };
        if self.recent.len() < RECENT_SLOTS {
            self.recent.push(entry);
        } else if let Some(oldest) = self.recent.iter_mut().min_by_key(|r| r.seq) {
            *oldest = entry;
        }
    }

    /// The frame's exchange with benilla: mutes and plays out, the file log and the live plays
    /// in. Returns the tracked plays that ended, for `SOUNDKIT_FINISHED`.
    pub(crate) fn sync(&mut self, ext: &mut ExtSound, now_ms: f64) -> Vec<u64> {
        if !ext.record {
            ext.record = true;
        }
        if std::mem::take(&mut self.muted_dirty) {
            ext.muted = self.muted.clone();
        }
        if !ext.opened.is_empty() {
            for (file, muted) in std::mem::take(&mut ext.opened) {
                self.record(file, muted, now_ms);
            }
        }
        let snapshot = &ext.live;
        let mut ended: Vec<u64> = self
            .live
            .keys()
            .filter(|t| !snapshot.contains_key(t))
            .copied()
            .collect();
        self.awaiting.retain_mut(|(token, age)| {
            if snapshot.contains_key(token) {
                return false;
            }
            *age += 1;
            if *age > AWAIT_FRAMES {
                ended.push(*token);
                return false;
            }
            true
        });
        self.live = snapshot.clone();
        for p in self.queued.drain(..) {
            self.awaiting.push((p.token, 0));
            ext.plays.push(p);
        }
        ended.sort_unstable();
        ended.retain(|t| self.tracked.remove(t));
        ended
    }
}

/// The kit a `SoundEntries` id names, `None` for an id with no row.
fn kit_exists(ca: &Ca, id: i64) -> Option<u32> {
    let id = u32::try_from(id).ok().filter(|i| *i > 0)?;
    ca.db.get("SoundEntries")?.row(id).map(|_| id)
}

/// `PlayWithOptions`: queue the kit, `willPlay, soundHandle`.
fn play(
    lua: &Lua,
    ca: &Ca,
    id: i64,
    volume: Option<f32>,
    finish: bool,
) -> mlua::Result<MultiValue> {
    match kit_exists(ca, id) {
        Some(kit) => {
            let token = ca.lock().sound.queue(kit, volume, finish);
            (true, token).into_lua_multi(lua)
        }
        None => (false, Value::Nil).into_lua_multi(lua),
    }
}

/// The Lua number type proper, not a numeric string (`lua_type == LUA_TNUMBER`).
fn is_number_type(v: &Value) -> bool {
    matches!(v, Value::Integer(_) | Value::Number(_))
}

/// `PlayItemSound`'s kit: the item's display, its sound group, the type's column.
fn item_kit(lua: &Lua, ca: &Ca, sound_type: i64, item: &Value) -> Option<u32> {
    let found = crate::items::resolve_item_or_location(lua, ca, item)?;
    let record = crate::lua::item::record(lua, i64::from(found.item.entry))?;
    let display = record.display_info_id;
    let group = ca
        .db
        .get("ItemDisplayInfo")?
        .row(display)?
        .u32(DISPLAY_GROUP_SOUNDS);
    let kit = ca
        .db
        .get("ItemGroupSounds")?
        .row(group)?
        .u32(1 + sound_type as usize);
    (kit != 0).then_some(kit)
}

/// `SoundForError`: the player's race's row for the line, the sex's ordinary kit.
fn vocal_kit(ca: &Ca, line: i64) -> Option<u32> {
    let (race, sex) = {
        let st = ca.lock();
        let me = st.mirror.me()?;
        (u32::from(me.race()), u32::from(me.gender() != 0))
    };
    if race == 0 {
        return None;
    }
    let table = ca.db.get("VocalUISounds")?;
    let row = table
        .rows()
        .find(|r| i64::from(r.i32(VOCAL_LINE)) == line && r.u32(VOCAL_RACE) == race)?;
    let kit = row.i32(VOCAL_NORMAL + sex as usize);
    u32::try_from(kit).ok().filter(|k| *k > 0)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_Sound";
    let c = api.ca.clone();
    api.table(
        NS,
        "PlaySound",
        move |lua, (id, _channel, _no_dup, finish): (Value, Value, Value, Value)| {
            if !is_number_type(&id) {
                return Err(mlua::Error::runtime(
                    "Usage: C_Sound.PlaySound(soundKitID [, channel [, forceNoDuplicates [, runFinishCallback]]])",
                ));
            }
            play(lua, &c, to_int(&id), None, truthy(&finish))
        },
    )?;
    let c = api.ca.clone();
    api.table(NS, "PlaySoundWithOptions", move |lua, v: Value| {
        let Value::Table(t) = v else {
            return Err(mlua::Error::runtime(
                "Usage: C_Sound.PlaySoundWithOptions(params)",
            ));
        };
        let num = |k: &str, fallback: f64| -> mlua::Result<f64> {
            let v: Value = t.get(k)?;
            Ok(if is_number_type(&v) {
                to_number(&v)
            } else {
                fallback
            })
        };
        let id = num("soundKitID", 0.0)? as i64;
        let finish = truthy(&t.get::<Value>("runFinishCallback")?);
        let volume = Some(num("volumeOverride", -1.0)?)
            .filter(|v| *v >= 0.0)
            .map(|v| v as f32);
        play(lua, &c, id, volume, finish)
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetSoundScaledVolume", move |_, v: Value| {
        if !is_number(&v) {
            return Ok(None);
        }
        let token = to_number(&v) as u64;
        Ok(c.lock().sound.live.get(&token).map(|v| f64::from(*v)))
    })?;
    let c = api.ca.clone();
    api.table(NS, "IsPlaying", move |_, v: Value| {
        Ok(is_number(&v) && c.lock().sound.playing(to_number(&v) as u64))
    })?;

    // The global `PlaySound`: a number plays by id and answers `willPlay, soundHandle`; anything
    // else goes to the stock name form untouched.
    let stock: Option<Function> = api.lua.globals().get("PlaySound").ok();
    let c = api.ca.clone();
    api.global("PlaySound", move |lua, args: MultiValue| {
        let first = args.front().cloned().unwrap_or(Value::Nil);
        if is_number_type(&first) {
            return play(lua, &c, to_int(&first), None, false);
        }
        match &stock {
            Some(f) => f.call::<MultiValue>(args),
            None => Ok(MultiValue::new()),
        }
    })?;

    let c = api.ca.clone();
    api.global("MuteSoundFile", move |_, v: Value| {
        let Some(path) = to_str(&v).filter(|p| !p.is_empty()) else {
            return Ok(false);
        };
        let mut st = c.lock();
        st.sound.muted.insert(normalize(&path));
        st.sound.muted_dirty = true;
        Ok(true)
    })?;
    let c = api.ca.clone();
    api.global("UnmuteSoundFile", move |_, v: Value| {
        let Some(path) = to_str(&v).filter(|p| !p.is_empty()) else {
            return Ok(false);
        };
        let mut st = c.lock();
        let had = st.sound.muted.remove(&normalize(&path));
        st.sound.muted_dirty |= had;
        Ok(had)
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetRecentSoundFiles", move |lua, ()| {
        let mut recent = c.lock().sound.recent.clone();
        recent.sort_by_key(|r| std::cmp::Reverse(r.seq));
        let out = lua.create_table()?;
        for (i, r) in recent.into_iter().enumerate() {
            let t = lua.create_table()?;
            t.set("file", r.file)?;
            t.set("time", r.time_ms)?;
            t.set("muted", r.muted)?;
            out.raw_set(i + 1, t)?;
        }
        Ok(out)
    })?;

    let c = api.ca.clone();
    api.table(
        NS,
        "PlayItemSound",
        move |lua, (ty, item): (Value, Value)| {
            if !is_number(&ty) {
                return Err(mlua::Error::runtime(
                    "Usage: C_Sound.PlayItemSound(soundType, item)",
                ));
            }
            let ty = to_int(&ty);
            if (0..=3).contains(&ty) {
                if let Some(kit) = item_kit(lua, &c, ty, &item) {
                    c.lock().sound.queue(kit, None, false);
                }
            }
            Ok(())
        },
    )?;
    api.int_enum(
        "Enum",
        "ItemSoundType",
        &[("Pickup", 0), ("Drop", 1), ("Use", 2), ("Close", 3)],
    )?;
    let c = api.ca.clone();
    api.table(NS, "PlayVocalErrorSound", move |_, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime(
                "Usage: C_Sound.PlayVocalErrorSound(vocalErrorSoundID)",
            ));
        }
        if let Some(kit) = vocal_kit(&c, to_int(&v)) {
            c.lock().sound.queue(kit, None, false);
        }
        Ok(())
    })?;
    api.int_enum("Enum", "Vocalerrorsounds", VOCAL_ERRORS)
}

/// `Enum.Vocalerrorsounds`, the `VocalUISounds` line column's values.
const VOCAL_ERRORS: &[(&str, i64)] = &[
    ("Inventoryfull", 0),
    ("Outofammo", 1),
    ("NoequipLevel", 2),
    ("NoequipEver", 3),
    ("BoundNodrop", 4),
    ("Itemcooling", 5),
    ("Cantdrinkmore", 6),
    ("Canteatmore", 7),
    ("Cantinvite", 8),
    ("Inviteebusy", 9),
    ("Targettoofar", 10),
    ("Invalidtarget", 11),
    ("Spellcooling", 12),
    ("CantlearnLevel", 13),
    ("Locked", 14),
    ("Nomana", 15),
    ("Notwhiledead", 16),
    ("Cantloot", 17),
    ("Cantcreate", 18),
    ("Declinegroup", 19),
    ("Alreadyingroup", 20),
    ("Alreadyinguild", 21),
    ("Cantaffordbankslot", 22),
    ("Toomanybankslots", 23),
    ("CanteatMoving", 24),
    ("Notabag", 25),
    ("Cantputbag", 26),
    ("Wrongslot", 27),
    ("Ammoonlyinbag", 28),
    ("Bagfull", 29),
    ("Itemmaxcount", 30),
    ("CantlootDidntkill", 31),
    ("CantlootWrongfacing", 32),
    ("CantlootLocked", 33),
    ("CantlootNotstandingObsolete", 34),
    ("CantlootToofar", 35),
    ("Cantattackrongdirection", 36),
    ("CantattackNotstandingObsolete", 37),
    ("CantattackNotarget", 38),
    ("Notenoughgold", 39),
    ("Notenoughmoney", 40),
    ("Cantequip2HSkill", 41),
    ("Cantequip2Hequipped", 42),
    ("Cantequip2HNoskill", 43),
    ("Notequippable", 44),
    ("Genericnotarget", 45),
    ("CantcastOutofrange", 46),
    ("Potioncooling", 47),
    ("Proficiencyneeded", 48),
    ("Mustequippitem", 49),
    ("Abilitycooling", 50),
    ("Cantuseitem", 51),
    ("Chestinuse", 52),
    ("FoodcoolingObsolete", 53),
    ("CanttaxiNomoney", 54),
    ("Cantuselocked", 55),
    ("Noequipslotavailable", 56),
    ("Cantusetoofar", 57),
    ("Cantswap", 58),
    ("CanttradeSoulbound", 59),
    ("Cantflyhere", 60),
    ("Itemlocked", 61),
    ("Guildpermissions", 62),
    ("Norage", 63),
    ("Noenergy", 64),
    ("Noessence", 65),
    ("Invaliditemtarget", 66),
    ("ExhaustedObsolete", 67),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbc::Dbc;

    #[test]
    fn plays_hand_off_and_tracked_ends_fire_once() {
        let mut s = Sound::default();
        let mut ext = ExtSound::default();
        let a = s.queue(850, None, true);
        let b = s.queue(851, Some(0.5), true);
        assert!(s.playing(a));
        assert!(s.sync(&mut ext, 0.0).is_empty());
        assert!(ext.record, "the log is always on");
        assert_eq!(ext.plays.len(), 2);
        assert_eq!(ext.plays[1].volume, Some(0.5));
        // benilla started a; b was dropped by a gate.
        ext.live.insert(a, 1.0);
        assert!(s.sync(&mut ext, 0.0).is_empty());
        assert!(s.playing(a) && s.playing(b));
        assert!(s.sync(&mut ext, 0.0).is_empty());
        assert_eq!(s.sync(&mut ext, 0.0), vec![b]);
        assert!(!s.playing(b));
        ext.live.clear();
        assert_eq!(s.sync(&mut ext, 0.0), vec![a]);
        assert!(s.sync(&mut ext, 0.0).is_empty());
    }

    #[test]
    fn the_recent_list_keeps_distinct_files_and_mutes_reach_the_funnel() {
        let mut s = Sound::default();
        let mut ext = ExtSound {
            opened: vec![
                (r"Sound\A.wav".into(), false),
                (r"Sound\B.wav".into(), false),
                ("sound/a.WAV".into(), true),
            ],
            ..Default::default()
        };
        s.muted.insert(normalize("Sound/A.wav"));
        s.muted_dirty = true;
        s.sync(&mut ext, 1500.0);
        assert!(ext.muted.contains(r"sound\a.wav"));
        assert!(ext.opened.is_empty());
        assert_eq!(s.recent.len(), 2);
        let newest = s.recent.iter().max_by_key(|r| r.seq).unwrap();
        assert_eq!(
            (newest.file.as_str(), newest.muted, newest.time_ms),
            (r"Sound\A.wav", true, 1500.0)
        );
    }

    #[test]
    fn the_natives_queue_by_id_and_the_name_form_passes_through() {
        let ca = crate::Ca::default();
        let mut row = vec![0u32; 2];
        row[0] = 850;
        ca.db.seed("SoundEntries", Dbc::from_rows(&[row], b"\0"));
        let script = crate::lua::test_support::vm(&ca);
        let out: String = script
            .lua()
            .load(
                r#"
                local ok, h = C_Sound.PlaySound(850, "SFX", false, true)
                local bad, none = PlaySound(9)
                local n = table.getn({ PlaySound("igMainMenuOpen") })
                local muted = MuteSoundFile("Sound/X.wav")
                local un = UnmuteSoundFile("SOUND\\X.WAV")
                local again = UnmuteSoundFile("Sound\\X.wav")
                local err = pcall(C_Sound.PlaySound, "850")
                return tostring(ok) .. h .. tostring(C_Sound.IsPlaying(h)) .. " "
                  .. tostring(bad) .. tostring(none) .. n .. " "
                  .. tostring(muted) .. tostring(un) .. tostring(again) .. tostring(err)
                  .. " " .. Enum.ItemSoundType.Close .. Enum.Vocalerrorsounds.Nomana
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "true1true falsenil0 truetruefalsefalse 315");
    }
}

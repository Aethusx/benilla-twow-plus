//! SuperWoW on top of benilla, built from the public documentation of the closed-source
//! `SuperWoWhook.dll` (`balakethelock/SuperWoW`, wiki "Features" and "Changelog" as of 2.2), never
//! from its code. The DLL widens the 1.12.1 client's Lua API; here the same API reaches benilla
//! through the seams of `benilla_app::ext` and benilla-ui's unit-token extension.
//!
//! - [`tokens`]: guid, `markN` and `owner` unit tokens, pure and unit-tested.
//! - [`lua`]: the natives and the bootstrap chunk that wraps the stock verbs.
//! - [`net`]: `UNIT_CASTEVENT` off the server's cast and swing packets.
//! - [`spells`]: the `Spell.dbc` reads `SpellInfo` answers from.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use benilla_app::ext::{
    ExtCast, ExtCastSet, ExtRaidMark, ExtView, ExtWorld, NameplateHook, ScriptInstallers,
    ScriptValue, UiScript,
};
use benilla_protocol::messages::ObjectType;
use benilla_world::view::WorldCamera;
use bevy::camera::Projection;
use bevy::prelude::*;

mod files;
mod lua;
mod net;
pub mod spells;
pub mod tokens;

/// The SuperWoW release this port follows, `SUPERWOW_VERSION`.
pub const VERSION: &str = "2.2";

/// `MOVEFLAG_SWIMMING`.
const MOVEFLAG_SWIMMING: u32 = 0x0020_0000;

/// One streamed unit, as the tokens and the natives read it. Positions are WoW's
/// (`x` north, `y` west, `z` up).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Unit {
    pub target: u64,
    pub pet: u64,
    pub owner: u64,
    pub lootable: bool,
    pub mounted: bool,
    pub hostile: bool,
    pub pos: Option<[f32; 3]>,
}

/// Everything the natives, the net handlers and the frame system share, behind one lock that
/// none holds across a call back into Lua.
#[derive(Default)]
pub struct State {
    pub player: u64,
    /// The unit wearing each raid mark, star to skull.
    pub marks: [u64; 8],
    pub units: HashMap<u64, Unit>,
    /// The spellbook as learned, for `CastSpellByName`'s name lookup.
    pub book: Vec<u32>,
    pub move_flags: u32,
    /// Our run and swim speeds, yd/s.
    pub speeds: (f32, f32),
    pub autoloot: bool,
    pub clickthrough: bool,
    pub tracked: HashSet<u64>,
    /// The unit `SetMouseoverUnit` named, for the casts this crate sends.
    pub mouseover: Option<u64>,
    /// Whether some frame registered `UNIT_CASTEVENT`.
    pub cast_events: bool,
    marks_out: Vec<ExtRaidMark>,
    casts: Vec<ExtCast>,
    events: Vec<(&'static str, Vec<ScriptValue>)>,
}

impl tokens::Hops for State {
    fn target(&self, guid: u64) -> u64 {
        self.units.get(&guid).map_or(0, |u| u.target)
    }
    fn pet(&self, guid: u64) -> u64 {
        self.units.get(&guid).map_or(0, |u| u.pet)
    }
    fn owner(&self, guid: u64) -> u64 {
        self.units.get(&guid).map_or(0, |u| u.owner)
    }
}

impl State {
    /// Queue `UNIT_CASTEVENT` when some frame registered it.
    pub fn cast_event(&mut self, caster: u64, target: u64, kind: &str, spell_id: u32, ms: u32) {
        if self.cast_events {
            self.events.push((
                "UNIT_CASTEVENT",
                vec![
                    ScriptValue::Str(tokens::guid_string(caster)),
                    ScriptValue::Str(tokens::guid_string(target)),
                    ScriptValue::Str(kind.to_string()),
                    ScriptValue::Int(i64::from(spell_id)),
                    ScriptValue::Int(i64::from(ms)),
                ],
            ));
        }
    }

    pub fn cast(&mut self, spell_id: u32, target: Option<u64>) {
        self.casts.push(ExtCast {
            spell_id,
            target,
            item: None,
        });
    }

    pub fn set_local_mark(&mut self, icon: u8, guid: u64) {
        self.marks_out.push(ExtRaidMark { icon, guid });
    }
}

/// The shared state and the spell tables.
#[derive(Resource, Clone)]
pub struct Sw {
    state: Arc<Mutex<State>>,
    pub spells: Arc<spells::Spells>,
}

impl Sw {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct SuperWowPlugin;

impl Plugin for SuperWowPlugin {
    fn build(&self, app: &mut App) {
        let sw = Sw {
            state: Arc::default(),
            spells: Arc::new(spells::Spells::load()),
        };
        app.insert_resource(sw.clone());
        if let Some(mut installers) = app.world_mut().get_resource_mut::<ScriptInstallers>() {
            installers.add(move |_, script| lua::install(&sw, script));
        }
        net::register(app);
        app.add_systems(Update, frame.in_set(ExtCastSet))
            .add_systems(PostUpdate, field_of_view);
    }
}

/// The unit a descriptor describes, keeping the position the placements write.
fn unit_of(f: &benilla_protocol::ObjectFields, old: Option<&Unit>) -> Option<Unit> {
    if !matches!(f.created_as(), Some(ObjectType::Unit | ObjectType::Player)) {
        return None;
    }
    Some(Unit {
        target: f.unit_target().unwrap_or(0),
        pet: f.unit_pet_guid().unwrap_or(0),
        owner: f
            .unit_summoned_by()
            .or_else(|| f.unit_charmed_by())
            .or_else(|| f.unit_created_by())
            .unwrap_or(0),
        lootable: f.unit_lootable(),
        mounted: f.unit_mount_display_id() != 0,
        ..old.copied().unwrap_or_default()
    })
}

/// Bevy's Y-up position back to WoW's: `wow_to_bevy` is `(-y, z, -x)`.
pub fn wow_position(p: Vec3) -> [f32; 3] {
    [-p.z, -p.x, p.y]
}

/// The frame: mirror the units, marks and our movement, feed benilla the guids the tokens name,
/// apply the nameplate range, then send the local marks and casts and fire the events.
#[allow(clippy::too_many_arguments)]
fn frame(
    sw: Res<Sw>,
    view: ExtView,
    world: ExtWorld,
    mut casts: MessageWriter<ExtCast>,
    mut marks: MessageWriter<ExtRaidMark>,
    mut plates: ResMut<NameplateHook>,
    script: Option<NonSendMut<UiScript>>,
    mut owns_range: Local<bool>,
) {
    let mut script = script;
    let (marks_out, casts_out, events, extra) = {
        let mut st = sw.lock();
        st.player = view.unit_guid("player").unwrap_or(0);
        for (i, mark) in st.marks.iter_mut().enumerate() {
            *mark = view.raid_mark(i as u8 + 1).unwrap_or(0);
        }
        for (guid, fields) in view.changed_objects() {
            let fresh = unit_of(fields, st.units.get(&guid));
            if let Some(u) = fresh {
                st.units.insert(guid, u);
            }
        }
        let placed: HashMap<u64, Vec3> = world.placements().map(|(g, p, _)| (g, p)).collect();
        st.units.retain(|g, _| placed.contains_key(g));
        let guids: Vec<u64> = st.units.keys().copied().collect();
        for guid in &guids {
            let hostile = world.can_attack(*guid);
            if let Some(u) = st.units.get_mut(guid) {
                u.pos = Some(wow_position(placed[guid]));
                u.hostile = hostile;
            }
        }
        st.move_flags = view.move_flags();
        let player = st.player;
        st.speeds = world
            .speeds(player)
            .map_or((7.0, 4.722), |s| (s.run, s.swim));
        if let Some(s) = script.as_deref() {
            st.cast_events = s.has_event_registrations("UNIT_CASTEVENT");
            // `NameplateRange`, 10-80 yd, outranks every other range; the reference's 20 leaves
            // the range to the others.
            let range = s
                .cvar("NameplateRange")
                .and_then(|v| v.trim().parse::<f32>().ok())
                .map(|r| r.clamp(10.0, 80.0))
                .filter(|r| *r != 20.0);
            if range.is_some() || *owns_range {
                if plates.max_distance != range {
                    plates.max_distance = range;
                }
                *owns_range = range.is_some();
            }
        }
        let mut extra = guids;
        extra.sort_unstable();
        (
            std::mem::take(&mut st.marks_out),
            std::mem::take(&mut st.casts),
            std::mem::take(&mut st.events),
            extra,
        )
    };
    for mark in marks_out {
        marks.write(mark);
    }
    for cast in casts_out {
        casts.write(cast);
    }
    if let Some(s) = script.as_deref_mut() {
        s.set_extra_unit_guids(extra);
        for (name, args) in events {
            s.queue_event(name, args);
        }
    }
}

/// `FoV`'s documented default, the 1.12 camera's field of view.
const REFERENCE_FOV: f32 = 1.57;

/// `FoV` (0.1 to 3.14): the view widened or narrowed by the ratio of `tan(fov/2)` to the
/// reference's, so the default leaves benilla's own projection as it is.
fn field_of_view(
    script: Option<NonSend<UiScript>>,
    mut cameras: Query<&mut Projection, With<WorldCamera>>,
    mut base: Local<Option<f32>>,
) {
    let Some(script) = script else {
        return;
    };
    let Ok(mut projection) = cameras.single_mut() else {
        return;
    };
    let Projection::Perspective(p) = projection.as_mut() else {
        return;
    };
    let want = script
        .cvar("FoV")
        .and_then(|v| v.trim().parse::<f32>().ok())
        .map(|f| f.clamp(0.1, std::f32::consts::PI))
        .unwrap_or(REFERENCE_FOV);
    let base = *base.get_or_insert(p.fov);
    let fov = scaled_fov(base, want);
    if (p.fov - fov).abs() > 1e-5 {
        p.fov = fov;
    }
}

/// `base` scaled as `want` scales the reference's field of view.
pub fn scaled_fov(base: f32, want: f32) -> f32 {
    let ratio = (want * 0.5).tan() / (REFERENCE_FOV * 0.5).tan();
    2.0 * ((base * 0.5).tan() * ratio).atan()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_fov_keeps_the_projection() {
        let base = std::f32::consts::FRAC_PI_4;
        assert!((scaled_fov(base, REFERENCE_FOV) - base).abs() < 1e-4);
        assert!(scaled_fov(base, 2.5) > base);
        assert!(scaled_fov(base, 1.0) < base);
    }

    #[test]
    fn positions_go_back_to_wow_axes() {
        // wow (10, 20, 30) is bevy (-20, 30, -10).
        assert_eq!(
            wow_position(Vec3::new(-20.0, 30.0, -10.0)),
            [10.0, 20.0, 30.0]
        );
    }
}

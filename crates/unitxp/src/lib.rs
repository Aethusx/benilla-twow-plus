//! UnitXP Service Pack 3 on top of benilla, ported from the `UnitXP_SP3.dll` that patches the
//! 1.12.1 client (`brues-code/UnitXP_SP3`, the maintained fork). The DLL widens the stock
//! `UnitXP(unit)` into `UnitXP(command, ...)`; here the same dispatcher reaches benilla through
//! the seams of `benilla_app::ext` and `benilla_world`.
//!
//! - [`geometry`] and [`targeting`]: the measures and the target picks, pure and unit-tested.
//! - [`lua`]: the dispatcher.
//! - [`camera`], [`fps`], [`notify`]: the camera offsets, the frame cap, the OS notifications.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use benilla_app::ext::{
    CombatTextHook, ExtCastSet, ExtCombatText, ExtSelect, ExtView, ExtWorld, NameplateHook,
    ScriptInstallers, UiScript,
};
use benilla_world::collision::WorldCollision;
use benilla_world::weather::{WeatherKind, WeatherState};
use bevy::math::{Quat, Vec3};
use bevy::prelude::*;

pub mod camera;
pub mod fps;
pub mod geometry;
mod lua;
mod notify;
pub mod targeting;

use geometry::Body;

/// The upstream commit time this port follows, `UnitXP("version", "coffTimeDateStamp")`'s
/// answer: addons compare it to gate features.
pub const COFF_TIME_DATE_STAMP: f64 = 1_785_877_811.0;

/// `UNIT_FIELD_*` indices this crate reads.
mod field {
    pub const SCALE: usize = 4;
    pub const TARGET: usize = 16;
    pub const HEALTH: usize = 22;
    pub const MAX_HEALTH: usize = 28;
    pub const FLAGS: usize = 46;
    pub const BOUNDING_RADIUS: usize = 129;
    pub const COMBAT_REACH: usize = 130;
}

/// `UNIT_FLAG_PLAYER_CONTROLLED`.
const FLAG_PLAYER_CONTROLLED: u32 = 0x8;
/// `UNIT_FLAG_IN_COMBAT`.
const FLAG_IN_COMBAT: u32 = 0x8_0000;
/// How far around us sight lines are kept fresh.
const SIGHT_RANGE: f32 = 100.0;
/// How long a sight line asked for between two other units stays traced.
const PAIR_TTL: Duration = Duration::from_secs(2);
/// A unit's eye height for sight lines, scaled by `OBJECT_FIELD_SCALE_X`. Deviation: the
/// reference reads the model's collision box height, which benilla does not publish.
const EYE_HEIGHT: f32 = 2.0;

/// One streamed unit, as the dispatcher reads it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unit {
    pub body: Body,
    pub health: u32,
    pub max_health: u32,
    pub flags: u32,
    pub target: u64,
    pub moving: bool,
    pub creature_type: u32,
    pub rank: u32,
    pub can_attack: bool,
}

impl Unit {
    pub fn in_combat(&self) -> bool {
        self.flags & FLAG_IN_COMBAT != 0
    }
}

/// The settings `UnitXP(...)` reads and writes. They live for the session, as in the DLL; an
/// addon re-applies them at load.
#[derive(Clone, Debug)]
pub struct Settings {
    pub tuning: targeting::Tuning,
    pub behind_threshold: f32,
    pub modern_nameplates: bool,
    pub hide_critter_nameplate: bool,
    pub prioritize_target_nameplate: bool,
    pub prioritize_marked_nameplate: bool,
    pub nameplate_combat_filter: bool,
    pub in_combat_nameplates_near_player: bool,
    pub camera: camera::Offsets,
    pub fps_cap: f64,
    pub background_fps_cap: f64,
    pub no_rain: bool,
    pub no_snow: bool,
    pub no_sandstorm: bool,
    pub hide_exp_text: bool,
    pub combat_text_sp3: bool,
    pub font_size: f64,
    pub nameplate_height: f64,
    pub font_name: String,
    /// `SCREENSHOT_FILETYPE`: 0 jpg, 1 png.
    pub screenshot: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tuning: targeting::Tuning::default(),
            behind_threshold: std::f32::consts::FRAC_PI_2,
            modern_nameplates: true,
            hide_critter_nameplate: true,
            prioritize_target_nameplate: false,
            prioritize_marked_nameplate: false,
            nameplate_combat_filter: false,
            in_combat_nameplates_near_player: false,
            camera: camera::Offsets::default(),
            fps_cap: 0.0,
            background_fps_cap: 0.0,
            no_rain: false,
            no_snow: false,
            no_sandstorm: false,
            hide_exp_text: false,
            combat_text_sp3: false,
            font_size: 30.0,
            nameplate_height: 0.0,
            font_name: String::new(),
            screenshot: 1,
        }
    }
}

/// One `UnitXP("timer", "arm", ...)` timer.
struct Timer {
    id: u32,
    next: Instant,
    period: Duration,
    handler: String,
}

/// Everything the dispatcher and the systems share.
#[derive(Default)]
pub struct State {
    pub settings: Settings,
    pub player: u64,
    pub selection: u64,
    pub marks: [u64; 8],
    pub tokens: Vec<(String, u64)>,
    pub units: HashMap<u64, Unit>,
    pub camera: Option<(Vec3, Vec3)>,
    /// Sight lines by unordered pair.
    sight: HashMap<(u64, u64), bool>,
    /// Pairs not involving us that Lua asked about, and when.
    asked: HashMap<(u64, u64), Instant>,
    /// Units the camera sees (the DLL's `camera_inSight`).
    camera_sight: HashMap<u64, bool>,
    timers: Vec<Timer>,
    next_timer: u32,
    selects: Vec<u64>,
    texts: Vec<ExtCombatText>,
    pub flash: bool,
    pub sound: Option<String>,
    pub focused: bool,
}

fn pair(a: u64, b: u64) -> (u64, u64) {
    (a.min(b), a.max(b))
}

impl State {
    /// A unit token or `0x` guid, as the DLL's `vanilla1121_unitGUID` reads one.
    pub fn resolve(&self, token: &str) -> Option<u64> {
        let t = token.trim();
        if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            return u64::from_str_radix(hex, 16).ok().filter(|g| *g != 0);
        }
        let lower = t.to_ascii_lowercase();
        // `targettarget`-style hops off the base token.
        let mut base = lower.as_str();
        let mut hops = 0;
        while base.len() > "target".len() && base.ends_with("target") {
            base = &base[..base.len() - "target".len()];
            hops += 1;
        }
        let mut guid = self
            .tokens
            .iter()
            .find(|(n, _)| n == base)
            .map(|(_, g)| *g)
            .filter(|g| *g != 0)?;
        for _ in 0..hops {
            guid = self.units.get(&guid)?.target;
            if guid == 0 {
                return None;
            }
        }
        Some(guid)
    }

    /// The sight line between two units: `None` until it has been traced.
    pub fn sight(&mut self, a: u64, b: u64) -> Option<bool> {
        if a == b {
            return Some(true);
        }
        let key = pair(a, b);
        if a != self.player && b != self.player {
            self.asked.insert(key, Instant::now());
        }
        self.sight.get(&key).copied()
    }

    pub fn select(&mut self, guid: u64) {
        self.selects.push(guid);
    }

    pub fn text(&mut self, text: ExtCombatText) {
        self.texts.push(text);
    }

    pub fn arm_timer(&mut self, first_ms: u64, period_ms: u64, handler: String) -> u32 {
        self.next_timer += 1;
        let id = self.next_timer;
        self.timers.push(Timer {
            id,
            next: Instant::now() + Duration::from_millis(first_ms),
            period: Duration::from_millis(period_ms),
            handler,
        });
        id
    }

    pub fn disarm_timer(&mut self, id: u32) -> bool {
        let before = self.timers.len();
        self.timers.retain(|t| t.id != id);
        self.timers.len() != before
    }

    pub fn timer_count(&self) -> usize {
        self.timers.len()
    }

    /// The timers due now, re-armed or retired.
    fn due_timers(&mut self, now: Instant) -> Vec<(u32, String)> {
        let mut due = Vec::new();
        self.timers.retain_mut(|t| {
            if t.next > now {
                return true;
            }
            due.push((t.id, t.handler.clone()));
            if t.period.is_zero() {
                return false;
            }
            t.next = (t.next + t.period).max(now);
            true
        });
        due
    }

    /// Whether the unit should have a nameplate (`shouldHaveNameplate`).
    fn wants_nameplate(&self, guid: u64, any_marked: bool) -> bool {
        let s = &self.settings;
        let Some(u) = self.units.get(&guid) else {
            return true;
        };
        let marked = self.marks.contains(&guid);
        if (s.prioritize_marked_nameplate || s.prioritize_target_nameplate)
            && (self.selection != 0 || any_marked)
        {
            match (s.prioritize_target_nameplate, s.prioritize_marked_nameplate) {
                (true, true) => return guid == self.selection || marked,
                (true, false) if self.selection != 0 => return guid == self.selection,
                (false, true) if any_marked => return marked,
                _ => {}
            }
        }
        if s.hide_critter_nameplate && u.creature_type == 8 && !u.in_combat() {
            return false;
        }
        if s.nameplate_combat_filter {
            let full = u.health >= u.max_health;
            let fighting = u.in_combat() || (u.body.is_player && u.can_attack);
            if full && !fighting {
                return false;
            }
        }
        if s.in_combat_nameplates_near_player && u.in_combat() {
            if let Some(me) = self.units.get(&self.player) {
                if geometry::distance(&u.body, &me.body, geometry::Meter::Ranged) < 8.0 {
                    return true;
                }
            }
        }
        let Some((cam, _)) = self.camera else {
            return true;
        };
        cam.distance(u.body.pos) <= 10.0 || self.camera_sight.get(&guid).copied().unwrap_or(true)
    }
}

/// The shared state.
#[derive(Resource, Clone, Default)]
pub struct Ux(Arc<Mutex<State>>);

impl Ux {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct UnitXpPlugin;

impl Plugin for UnitXpPlugin {
    fn build(&self, app: &mut App) {
        let ux = Ux::default();
        app.insert_resource(ux.clone());
        if let Some(mut installers) = app.world_mut().get_resource_mut::<ScriptInstallers>() {
            installers.add(move |_, script| lua::install(&ux, script));
        }
        app.add_systems(Update, frame.in_set(ExtCastSet))
            .add_systems(
                PostUpdate,
                camera::apply_offsets.before(bevy::transform::TransformSystems::Propagate),
            )
            .add_systems(Last, (notify::notify, fps::cap));
    }
}

fn unit_of(f: &benilla_protocol::ObjectFields) -> Option<Unit> {
    let mut unit = Unit::default();
    let (mut type_mask, mut scale, mut target) = (0, 0.0f32, [0u32; 2]);
    for (i, x) in f.raw_fields() {
        match usize::from(i) {
            2 => type_mask = x,
            field::SCALE => scale = f32::from_bits(x),
            field::TARGET => target[0] = x,
            i if i == field::TARGET + 1 => target[1] = x,
            field::HEALTH => unit.health = x,
            field::MAX_HEALTH => unit.max_health = x,
            field::FLAGS => unit.flags = x,
            field::BOUNDING_RADIUS => unit.body.bounding_radius = f32::from_bits(x),
            field::COMBAT_REACH => unit.body.combat_reach = f32::from_bits(x),
            _ => {}
        }
    }
    // `OBJECT_FIELD_TYPE`: units and players carry TYPEMASK_UNIT, players TYPEMASK_PLAYER.
    if type_mask & 0x8 == 0 {
        return None;
    }
    unit.body.is_player = type_mask & 0x10 != 0;
    unit.body.height = EYE_HEIGHT * if scale > 0.0 { scale } else { 1.0 };
    unit.target = u64::from(target[0]) | (u64::from(target[1]) << 32);
    Some(unit)
}

/// The frame: refresh the units, trace the sight lines, decide the nameplates, apply the weather
/// and combat-text settings, send the selections and texts, and run the due timers.
#[allow(clippy::too_many_arguments)]
fn frame(
    ux: Res<Ux>,
    view: ExtView,
    world: ExtWorld,
    collision: WorldCollision,
    mut selects: MessageWriter<ExtSelect>,
    mut texts: MessageWriter<ExtCombatText>,
    mut plates: ResMut<NameplateHook>,
    mut combat_text: ResMut<CombatTextHook>,
    weather: Option<ResMut<WeatherState>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    script: Option<NonSendMut<UiScript>>,
) {
    let now = Instant::now();
    let blocked = |from: Vec3, to: Vec3| collision.sight(from, to).is_some();
    let due = {
        let mut st = ux.lock();
        st.focused = windows.single().is_ok_and(|w| w.focused);
        if st.tokens.is_empty() {
            st.tokens = ["player", "target", "pet", "mouseover"]
                .into_iter()
                .map(String::from)
                .chain((1..=4).flat_map(|i| [format!("party{i}"), format!("partypet{i}")]))
                .chain((1..=40).flat_map(|i| [format!("raid{i}"), format!("raidpet{i}")]))
                .map(|t| (t, 0))
                .collect();
        }
        let tokens: Vec<u64> = st
            .tokens
            .iter()
            .map(|(t, _)| view.unit_guid(t).unwrap_or(0))
            .collect();
        for (slot, guid) in st.tokens.iter_mut().zip(tokens) {
            slot.1 = guid;
        }
        st.player = view.unit_guid("player").unwrap_or(0);
        st.selection = view.target_guid().unwrap_or(0);
        for (i, mark) in st.marks.iter_mut().enumerate() {
            *mark = view.raid_mark(i as u8 + 1).unwrap_or(0);
        }
        st.camera = world.camera();
        // Units: descriptors as they change, placements every frame.
        for (guid, fields) in view.changed_objects() {
            if let Some(fresh) = unit_of(fields) {
                let old = st.units.get(&guid).copied().unwrap_or_default();
                st.units.insert(
                    guid,
                    Unit {
                        body: Body {
                            pos: old.body.pos,
                            rotation: old.body.rotation,
                            ..fresh.body
                        },
                        ..fresh
                    },
                );
            }
        }
        let placed: HashMap<u64, (Vec3, Quat)> =
            world.placements().map(|(g, p, r)| (g, (p, r))).collect();
        st.units.retain(|g, _| placed.contains_key(g));
        let guids: Vec<u64> = st.units.keys().copied().collect();
        for guid in guids {
            let (pos, rotation) = placed[&guid];
            let creature = world.creature(guid);
            let can_attack = world.can_attack(guid);
            if let Some(u) = st.units.get_mut(&guid) {
                u.moving = u.body.pos.distance_squared(pos) > 1e-4;
                u.body.pos = pos;
                u.body.rotation = rotation;
                (u.creature_type, u.rank) = creature.unwrap_or((0, 0));
                u.can_attack = can_attack;
            }
        }
        // Sight lines: us to everything near, plus the pairs Lua asked about lately.
        st.sight.clear();
        st.asked.retain(|_, at| now.duration_since(*at) < PAIR_TTL);
        let me = st.player;
        let mut lines: Vec<(u64, u64)> = Vec::new();
        if let Some(mine) = st.units.get(&me) {
            for (g, u) in &st.units {
                if *g != me && u.body.pos.distance(mine.body.pos) <= SIGHT_RANGE {
                    lines.push((me, *g));
                }
            }
        }
        lines.extend(st.asked.keys().copied());
        for (a, b) in lines {
            if let (Some(ua), Some(ub)) = (st.units.get(&a), st.units.get(&b)) {
                let seen = geometry::in_sight(&ua.body, &ub.body, blocked);
                st.sight.insert(pair(a, b), seen);
            }
        }
        // The camera's own sight, for the nameplates (`camera_inSight`: 2.1 yd up the unit).
        st.camera_sight.clear();
        if let Some((cam, _)) = st.camera {
            let near: Vec<(u64, Vec3)> = st
                .units
                .iter()
                .filter(|(_, u)| u.body.pos.distance(cam) <= 60.0)
                .map(|(g, u)| (*g, u.body.pos))
                .collect();
            for (g, pos) in near {
                let seen = g == me || !blocked(cam, pos + Vec3::Y * 2.1);
                st.camera_sight.insert(g, seen);
            }
        }
        // Nameplates.
        plates.hidden.clear();
        if st.settings.modern_nameplates {
            let any_marked = st.marks.iter().any(|m| *m != 0 && st.units.contains_key(m));
            let hidden: Vec<u64> = st
                .units
                .keys()
                .copied()
                .filter(|g| *g != me && !st.wants_nameplate(*g, any_marked))
                .collect();
            plates.hidden.extend(hidden);
        }
        if combat_text.hide_exp != st.settings.hide_exp_text {
            combat_text.hide_exp = st.settings.hide_exp_text;
        }
        if let Some(mut w) = weather {
            let s = &st.settings;
            let want: Vec<WeatherKind> = [
                (s.no_rain, WeatherKind::Rain),
                (s.no_snow, WeatherKind::Snow),
                (s.no_sandstorm, WeatherKind::Sand),
            ]
            .into_iter()
            .filter_map(|(off, k)| off.then_some(k))
            .collect();
            if w.suppressed != want {
                w.suppressed = want;
            }
        }
        for guid in st.selects.drain(..) {
            selects.write(ExtSelect { guid });
        }
        for t in st.texts.drain(..) {
            texts.write(t);
        }
        st.due_timers(now)
    };
    // Timer callbacks run with the lock free: a handler may call `UnitXP` back.
    if let Some(script) = script {
        for (id, handler) in due {
            let f = script
                .lua()
                .globals()
                .get::<mlua::Function>(handler.as_str());
            if let Ok(f) = f {
                if let Err(e) = f.call::<()>(id) {
                    script.report_script_error(&e.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_hop_through_targets_and_guids_parse() {
        let mut st = State {
            tokens: vec![("player".into(), 1), ("target".into(), 2)],
            ..Default::default()
        };
        st.units.insert(
            2,
            Unit {
                target: 3,
                ..Default::default()
            },
        );
        st.units.insert(
            3,
            Unit {
                target: 1,
                ..Default::default()
            },
        );
        assert_eq!(st.resolve("target"), Some(2));
        assert_eq!(st.resolve("TargetTarget"), Some(3));
        assert_eq!(st.resolve("targettargettarget"), Some(1));
        assert_eq!(
            st.resolve("0xF5300000000000A5"),
            Some(0xF530_0000_0000_00A5)
        );
        assert_eq!(st.resolve("focus"), None);
    }

    #[test]
    fn timers_fire_once_or_repeat() {
        let mut st = State::default();
        let once = st.arm_timer(0, 0, "a".into());
        let every = st.arm_timer(0, 1000, "b".into());
        let due = st.due_timers(Instant::now() + Duration::from_millis(1));
        assert_eq!(due.len(), 2);
        assert_eq!(st.timer_count(), 1);
        assert!(!st.disarm_timer(once));
        assert!(st.disarm_timer(every));
    }

    #[test]
    fn critter_plates_hide_until_they_fight() {
        let mut st = State::default();
        st.units.insert(
            5,
            Unit {
                creature_type: 8,
                ..Default::default()
            },
        );
        assert!(!st.wants_nameplate(5, false));
        st.units.get_mut(&5).unwrap().flags = FLAG_IN_COMBAT;
        assert!(st.wants_nameplate(5, false));
    }
}

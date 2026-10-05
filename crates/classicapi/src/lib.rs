//! ClassicAPI on top of benilla, ported from `ClassicAPI.dll` (`brues-code/ClassicAPI`), the DLL
//! that backports the modern WoW API into the 1.12.1 client. The DLL hooks `WoW.exe` and reads
//! its memory; here the same API reaches benilla through the seams of `benilla_app::ext` and
//! benilla-ui's `ext_read`, and nothing in benilla knows ClassicAPI exists.
//!
//! - [`dbc`]: the client databases, opened from the player's own patch chain.
//! - [`lua`]: the Lua API, natives and the bootstrap chunks, one module per DLL source folder.
//! - [`net`]: the server events the API reads.
//! - [`mirror`]: the game state the natives read, copied once a frame.

use std::sync::{Arc, Mutex, MutexGuard};

use std::time::Instant;

use benilla_app::ext::{
    CastOutcome, ExtAttack, ExtCancelAura, ExtCast, ExtCastNote, ExtCastSet, ExtQuery, ExtRange,
    ExtUnitTokens, ExtUsable, ExtView, ExtWorld, ScriptInstallers, ScriptValue, UiScript,
};
use bevy::prelude::*;

pub mod aura;
pub mod cast;
pub mod cooldown;
pub mod dbc;
pub mod guid;
pub mod itemdb;
pub mod items;
mod lua;
pub mod mirror;
mod net;
pub mod prof;
pub mod spellmod;
pub mod spells;
pub mod talents;
pub mod templates;
pub mod tokens;
pub mod transpile;

/// The ClassicAPI release this port follows.
pub const VERSION: (u32, u32, u32) = (1, 15, 16);

/// `CLASSIC_API_VERSION`, `X*10000 + Y*100 + Z` for release `vX.Y.Z`.
pub const VERSION_VALUE: u32 = VERSION.0 * 10000 + VERSION.1 * 100 + VERSION.2;

/// Everything the natives, the net handlers and the frame system share, behind one lock that
/// none holds across a call back into Lua.
#[derive(Default)]
pub struct State {
    /// Every spell the player knows, the engine's known-spell bitmap (`VAR_PLAYER_SPELL_BITMAP`):
    /// `SMSG_INITIAL_SPELLS`, then each learn, unlearn and supersede.
    pub known: Vec<u32>,
    /// The talent spell-modifier tables, from the modifier packets.
    pub mods: spellmod::Tables,
    pub mirror: mirror::Mirror,
    /// Events the frame system fires next, in order.
    events: Vec<(&'static str, Vec<ScriptValue>)>,
    /// Our auras to cancel, `CMSG_CANCEL_AURA` each.
    cancels: Vec<u32>,
    /// Casts the natives queued, sent through benilla's ladder next frame.
    pub(crate) casts: Vec<ExtCast>,
    /// Melee starts and stops the natives queued.
    pub(crate) attacks: Vec<ExtAttack>,
    /// The six modifier keys held, `Input::Modifier`'s mask.
    pub(crate) modifiers: u8,
    /// The creature, gameobject and quest records and their loads (`Cache::QueryLoad`).
    pub templates: templates::Templates,
    /// `C_FriendList`'s notes and `/who` queries.
    pub(crate) friends: lua::friends::Friends,
    /// The reputation diff behind `FACTION_STANDING_CHANGED` (`Faction::StandingChanged`).
    pub(crate) faction_watch: lua::faction::Watch,
    /// The nearest and directional target asks (`Target::Nearest`).
    pub(crate) targeting: lua::targeting::Targeting,
    /// The totem bar (`Totem::Tracker`).
    pub(crate) totems: lua::totem::Totems,
    /// `C_CVar`'s temporary values (`CVar::Temp`).
    pub(crate) temp_cvars: Vec<lua::cvar::Temp>,
    /// The override binding layer (`Bindings::Api`).
    pub(crate) overrides: lua::bindings::Overrides,
    /// The mouse buttons held and the clipboard writes (`Input::GlobalMouse`, `Clipboard::Copy`).
    pub(crate) input: lua::minor::Input,
    /// The console's list and queued lines (`Console::Commands`, `Console::Shell`).
    pub(crate) console: lua::console::Console,
    /// `C_Loot`'s sends, walk and pending take, and `C_LootHistory`.
    pub(crate) loot: lua::loot::Loot,
    /// `C_Sound`'s plays, mutes and recent files (`Sound::Play`, `Sound::Mute`).
    pub(crate) sound: lua::sound::Sound,
    /// `C_Map`'s user waypoint (`Map::Waypoint`), kept across a UI reload.
    pub(crate) waypoint: Option<lua::map::Waypoint>,
    /// `#showtooltip` / `#show` macros (`Macro::ShowTooltip`).
    pub(crate) showtooltip: lua::showtooltip::ShowTooltip,
    /// The nameplate diff: last frame's `(guid, frame)` pairs and every frame ever announced.
    plates_last: Vec<(u64, u32)>,
    plates_seen: std::collections::HashSet<u32>,
    /// The plate whose `NAME_PLATE_UNIT_REMOVED` is being dispatched, `(guid, frame)`: its unit
    /// binding is already gone, so `GetNamePlateForUnit` answers from here (`PlateBeingRemoved`).
    pub plate_removing: Option<(u64, u32)>,
    /// The player's form byte at the last frame, `UPDATE_SHAPESHIFT_FORM`'s edge.
    last_form: Option<u8>,
    /// `Unit::MirrorTimer`'s three slots by timer type: EXHAUSTION, BREATH, FEIGNDEATH.
    pub mirror_timers: [Option<MirrorTimer>; 3],
    /// Whether the mouseover named a unit last frame, `UPDATE_MOUSEOVER_UNIT`'s loss edge.
    mouseover_unit: bool,
    /// Every aura's caster and expiry (`Aura::Source`).
    pub auras: aura::Source,
    /// The player's and every observed unit's cast (`Spell::Cast`, `Spell::CastEvents`).
    pub cast: cast::Tracker,
    /// `C_NewItems`' baseline and flags.
    pub(crate) new_items: lua::item::NewItems,
    /// `C_LossOfControl`'s school lockouts and last frame's effect keys.
    pub(crate) school_locks: lua::lossofcontrol::Locks,
    pub(crate) loc_prev: Vec<(u32, u32)>,
    /// The server and realm clocks `C_DateAndTime` reads.
    pub(crate) clocks: lua::time::Clocks,
    /// `C_EquipmentSet`'s sets, loaded per character.
    pub(crate) equipment_sets: lua::equipmentset::Store,
    /// When the player first resolved, and whether the carried templates were warmed
    /// (`Item::Data`'s owned prefetch, a second after entering the world).
    in_world_at: Option<Instant>,
    owned_warmed: bool,
}

/// One mirror timer as its last packet left it.
#[derive(Clone, Copy, Debug)]
pub struct MirrorTimer {
    pub kind: u32,
    /// The value the packet carried, ms.
    pub value: i64,
    pub max: i64,
    /// Bar ms per ms: -1 draining, positive refilling.
    pub scale: i64,
    pub paused: bool,
    pub spell_id: u32,
    /// When `value` held.
    pub base: std::time::Instant,
}

impl MirrorTimer {
    /// `LiveValue`: the packet's value plus `elapsed * scale`, clamped to `0..=max`, frozen while
    /// paused.
    pub fn live(&self, now: std::time::Instant) -> i64 {
        if self.paused {
            return self.value;
        }
        let elapsed = now.saturating_duration_since(self.base).as_millis() as i64;
        let v = self.value + elapsed * self.scale;
        if v < 0 {
            0
        } else if self.max > 0 && v > self.max {
            self.max
        } else {
            v
        }
    }
}

impl State {
    /// Queue `event` for the frame's flush.
    pub fn emit(&mut self, event: &'static str, args: Vec<ScriptValue>) {
        self.events.push((event, args));
    }

    /// `FUN_CANCEL_AURA_SEND`: ask the server to cancel our aura of `spell_id`.
    pub fn cancel_aura(&mut self, spell_id: u32) {
        self.cancels.push(spell_id);
    }
}

/// The shared state, the token table and the client databases.
#[derive(Resource, Clone, Default)]
pub struct Ca {
    state: Arc<Mutex<State>>,
    pub tokens: tokens::Tokens,
    pub db: Arc<dbc::Databases>,
    /// The item templates, behind their own lock.
    pub items: itemdb::ItemDb,
}

impl Ca {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cast tracker with the same environment the aura cache reads.
    pub fn with_cast<R>(&self, f: impl FnOnce(&mut cast::Tracker, &aura::Env) -> R) -> R {
        self.with_auras_and(|_, cast, _, env| f(cast, env))
    }

    /// The totem tracker with the aura cache's environment.
    pub(crate) fn with_totems<R>(
        &self,
        f: impl FnOnce(&mut lua::totem::Totems, &aura::Env) -> R,
    ) -> R {
        self.with_auras_and(|_, _, totems, env| f(totems, env))
    }

    /// The aura cache and what it reads, under the lock.
    pub fn with_auras<R>(&self, f: impl FnOnce(&mut aura::Source, &aura::Env) -> R) -> R {
        self.with_auras_and(|auras, _, _, env| f(auras, env))
    }

    fn with_auras_and<R>(
        &self,
        f: impl FnOnce(&mut aura::Source, &mut cast::Tracker, &mut lua::totem::Totems, &aura::Env) -> R,
    ) -> R {
        let spells = spells::table(&self.db);
        let durations = self.db.get("SpellDuration");
        let mut st = self.lock();
        let family = spellmod::player_family(&self.db, &st.mirror);
        let State {
            auras,
            cast,
            totems,
            mirror,
            mods,
            known,
            ..
        } = &mut *st;
        let env = aura::Env {
            spells: spells.as_deref(),
            durations: durations.as_deref(),
            mirror,
            mods,
            family,
            known,
        };
        f(auras, cast, totems, &env)
    }
}

pub struct ClassicApiPlugin;

impl Plugin for ClassicApiPlugin {
    fn build(&self, app: &mut App) {
        let ca = Ca::default();
        app.insert_resource(ca.clone());
        if let Some(mut installers) = app.world_mut().get_resource_mut::<ScriptInstallers>() {
            installers.add(move |_, script| lua::install(&ca, script));
        }
        net::register(app);
        app.add_message::<ExtCastNote>();
        app.add_systems(Update, frame.in_set(ExtCastSet));
    }
}

/// Everything the frame reads off the world.
#[derive(bevy::ecs::system::SystemParam)]
struct Inputs<'w, 's> {
    view: ExtView<'w, 's>,
    world: ExtWorld<'w, 's>,
    range: ExtRange<'w, 's>,
    usable: ExtUsable<'w, 's>,
    collision: benilla_world::collision::WorldCollision<'w, 's>,
    point: benilla_world::world_point::WorldPoint<'w, 's>,
    map: Option<Res<'w, benilla_world::world_map::CurrentMap>>,
    ext_loot: Res<'w, benilla_app::ext::ExtLoot>,
    mouse: Option<Res<'w, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>>,
    clipboard: MessageWriter<'w, benilla_app::ext::ExtClipboard>,
}

/// The frame: mirror the world, then fire the events the net handlers and the natives queued.
fn frame(
    ca: Res<Ca>,
    inputs: Inputs,
    mut cancels: MessageWriter<ExtCancelAura>,
    mut casts: MessageWriter<ExtCast>,
    mut attacks: MessageWriter<ExtAttack>,
    mut selects: MessageWriter<benilla_app::ext::ExtSelect>,
    mut queries: MessageWriter<ExtQuery>,
    mut macro_display: ResMut<benilla_app::ext::ExtMacroDisplay>,
    keys: Option<Res<bevy::input::ButtonInput<bevy::input::keyboard::KeyCode>>>,
    mut ext_tokens: ResMut<ExtUnitTokens>,
    mut ext_sound: ResMut<benilla_app::ext::ExtSound>,
    mut loot_sends: MessageWriter<benilla_app::ext::ExtLootSend>,
    mut ext_console: ResMut<benilla_app::ext::ExtConsole>,
    mut notes: MessageReader<ExtCastNote>,
    script: Option<NonSendMut<UiScript>>,
) {
    let Inputs {
        view,
        world,
        range,
        usable,
        collision,
        point,
        map,
        ext_loot,
        mouse,
        mut clipboard,
    } = inputs;
    let now = Instant::now();
    let mut lap = prof::Lap::new();
    // `GetTime()` read with the lock free, so the mirror can put instants on its clock.
    let ui_now = script.as_deref().and_then(|s| {
        s.lua()
            .globals()
            .get::<mlua::Function>("GetTime")
            .and_then(|f| f.call::<f64>(()))
            .ok()
    });
    // `Turtle::Detected`: the realm's interface sets `TURTLE_WOW_VERSION`.
    let turtle = script.as_deref().is_some_and(|s| {
        matches!(
            s.lua().globals().get::<mlua::Value>("TURTLE_WOW_VERSION"),
            Ok(mlua::Value::String(_))
        )
    });
    let notes: Vec<ExtCastNote> = notes.read().copied().collect();
    let sent: Vec<u32> = notes
        .iter()
        .filter(|n| matches!(n.outcome, CastOutcome::Sent))
        .map(|n| n.spell_id)
        .collect();
    let changes = {
        let mut st = ca.lock();
        st.mirror.refresh(&view, &world, &range, now)
    };
    lap.mark("mirror");
    let aura_signals = ca.with_auras(|auras, env| {
        auras.turtle |= turtle;
        if auras.turtle {
            auras.register_turtle(env);
        }
        // `ComboDuration`'s send hook: the points a finisher leaves with.
        let points = env
            .mirror
            .me()
            .map_or(0, |f| f.byte(mirror::field::PLAYER_BYTES_FIELD, 1));
        for &spell in &sent {
            auras.capture_combo(spell, points);
            if auras.turtle {
                auras.carnage_arm(env, spell);
            }
        }
        for c in &changes {
            if let Some(new) = env.mirror.object(c.guid) {
                auras.diff(env, c.guid, c.old.as_ref(), new);
            }
        }
        auras.tick(env);
        auras.take_signals()
    });
    lap.mark("auras");
    let cast_fires = ca.with_cast(|cast, env| {
        let now = cast::now_ms();
        for n in &notes {
            match n.outcome {
                CastOutcome::Sent => cast.on_sent(env, n.spell_id, n.target, n.cast_time_ms, now),
                CastOutcome::Refused(reason) => cast.on_failed(n.spell_id, reason, now),
                CastOutcome::Pending => {}
            }
        }
        cast.tick(env, now);
        cast.take_fires()
    });
    let (totem_updates, totem_select) =
        ca.with_totems(|t, env| (t.tick(env, now), t.select.take()));
    let target_pick = {
        let mut st = ca.lock();
        let State {
            targeting, mirror, ..
        } = &mut *st;
        targeting.resolve(mirror, &|g, hostile| world.tab_valid(g, hostile), now)
    };
    lap.mark("cast");
    let mask = keys.as_deref().map_or(0, lua::misc::modifier_mask);
    let (events, cancel, queued, attack, loads, focus_lost) = {
        let mut st = ca.lock();
        st.mirror.map_id = map.as_deref().map_or(0, |m| m.0);
        st.mirror.area = point.area();
        st.console.sync(&mut ext_console);
        // `GLOBAL_MOUSE_DOWN`/`GLOBAL_MOUSE_UP(button)` per edge, UI hit or not.
        let held = mouse.as_deref().map_or(0, |m| {
            use bevy::input::mouse::MouseButton as B;
            [B::Left, B::Right, B::Middle, B::Back, B::Forward]
                .iter()
                .enumerate()
                .filter(|(_, b)| m.pressed(**b))
                .fold(0u8, |acc, (i, _)| acc | 1 << i)
        });
        for (button, down) in st.input.mouse(held) {
            let event = if down {
                "GLOBAL_MOUSE_DOWN"
            } else {
                "GLOBAL_MOUSE_UP"
            };
            st.emit(event, vec![ScriptValue::Str(button.to_string())]);
        }
        for text in std::mem::take(&mut st.input.clipboard) {
            clipboard.write(benilla_app::ext::ExtClipboard(text));
        }
        // `C_Loot`'s walk and pending take; `LOOT_SCAN_COMPLETED` when a walk ends.
        let State { loot, mirror, .. } = &mut *st;
        let (sends, walked) = loot.tick(&ext_loot, mirror, now);
        loot_sends.write_batch(sends);
        if walked {
            st.emit("LOOT_SCAN_COMPLETED", vec![]);
        }
        // `SOUNDKIT_FINISHED(handle)` for each opted-in play that ended.
        let now_ms = ui_now.unwrap_or(0.0) * 1000.0;
        for token in st.sound.sync(&mut ext_sound, now_ms) {
            st.emit("SOUNDKIT_FINISHED", vec![ScriptValue::Int(token as i64)]);
        }
        st.mirror.indoors = (st.mirror.player != 0).then(|| point.interior().is_some());
        st.mirror
            .refresh_sight(|a, b| collision.sight(a, b).is_some(), now);
        let focus_lost = upkeep_tokens(&ca.tokens, &st.mirror, &view);
        // `MODIFIER_STATE_CHANGED(key, down)` for each modifier that moved.
        let moved = st.modifiers ^ mask;
        for (i, key) in lua::misc::MODIFIER_KEYS.iter().enumerate() {
            if moved & (1 << i) != 0 {
                let down = i64::from(mask >> i & 1);
                st.emit(
                    "MODIFIER_STATE_CHANGED",
                    vec![ScriptValue::Str((*key).to_string()), ScriptValue::Int(down)],
                );
            }
        }
        st.modifiers = mask;
        // `UPDATE_SHAPESHIFT_FORM`, argless, when the form byte (`UNIT_FIELD_BYTES_1` byte 2)
        // moves; an unresolved player is no change.
        if let Some(form) = st
            .mirror
            .me()
            .map(|f| f.byte(mirror::field::UNIT_BYTES_1, 2))
        {
            if st.last_form.is_some_and(|l| l != form) {
                st.emit("UPDATE_SHAPESHIFT_FORM", vec![]);
            }
            st.last_form = Some(form);
        }
        // `Unit::Mouseover`: the engine fires `UPDATE_MOUSEOVER_UNIT` on a gain only; a unit
        // mouseover lost (to nothing or a GameObject) fires it too, as retail does.
        let State {
            new_items, mirror, ..
        } = &mut *st;
        new_items.frame(mirror, now);
        if std::mem::take(&mut st.new_items.fire) {
            st.emit("BAG_NEW_ITEMS_UPDATED", vec![]);
        }
        // `Item::Data`'s owned prefetch: every carried template, a second into the world.
        if st.mirror.player == 0 {
            st.in_world_at = None;
            st.owned_warmed = false;
        } else if st.in_world_at.get_or_insert(now).elapsed() >= std::time::Duration::from_secs(1)
            && !st.owned_warmed
        {
            st.owned_warmed = true;
            let m = &st.mirror;
            let mut owned: Vec<u32> = items::equipped(m).iter().map(|(_, it)| it.entry).collect();
            owned.extend(items::bagged(m, 0..=4).iter().map(|(_, _, it)| it.entry));
            let mut db = ca.items.lock();
            for id in owned {
                db.warm(id);
            }
        }
        let mouseover_unit = view.unit_guid("mouseover").is_some();
        if st.mouseover_unit && !mouseover_unit {
            st.emit("UPDATE_MOUSEOVER_UNIT", vec![]);
        }
        st.mouseover_unit = mouseover_unit;
        let mut wanted = st.known.clone();
        wanted.extend(st.mirror.usable_extra.iter().copied());
        st.mirror.refresh_usable(&usable, &wanted, now);
        if let Some(t) = ui_now {
            st.mirror.clock = Some((now, t));
        }
        (
            std::mem::take(&mut st.events),
            std::mem::take(&mut st.cancels),
            std::mem::take(&mut st.casts),
            std::mem::take(&mut st.attacks),
            st.templates.tick(now),
            focus_lost,
        )
    };
    lap.mark("state");
    for spell_id in cancel {
        cancels.write(ExtCancelAura { spell_id });
    }
    casts.write_batch(queued);
    attacks.write_batch(attack);
    for guid in [totem_select, target_pick].into_iter().flatten() {
        selects.write(benilla_app::ext::ExtSelect { guid });
    }
    let (asks, load_results) = loads;
    queries.write_batch(asks);
    let Some(mut script) = script else {
        return;
    };
    if focus_lost {
        script.queue_event("PLAYER_FOCUS_CHANGED", vec![]);
    }
    sync_nameplates(&ca, &mut script);
    let named = ca.tokens.lock().named();
    let guids: Vec<u64> = named.iter().map(|(_, g)| *g).collect();
    script.set_extra_unit_guids_for("classicapi", guids);
    if ext_tokens.0 != named {
        ext_tokens.0 = named;
    }
    for (name, args) in events {
        script.queue_event(name, args);
    }
    lap.mark("plates");
    lua::time::tick(script.lua());
    lap.mark("timers");
    for (name, args) in lua::lossofcontrol::tick(&ca) {
        script.queue_event(name, args);
    }
    lap.mark("loc");
    // The template loads: the asks benilla sends, and the events their answers fire.
    let (loaded, asks) = ca.items.lock().tick();
    for id in asks {
        benilla_ui::script::ext_read::ask_item(script.lua(), id);
    }
    for (name, id, ok) in loaded {
        script.queue_event(
            name,
            vec![ScriptValue::Number(f64::from(id)), ScriptValue::Bool(ok)],
        );
    }
    // A cached duration edit has no descriptor write behind it: `UNIT_AURA` once per token.
    for guid in aura_signals {
        for token in lua::unit::identity::tokens_for_guid(script.lua(), &ca, guid) {
            script.queue_event("UNIT_AURA", vec![ScriptValue::Str(token)]);
        }
    }
    for (name, args) in cast_events(&ca, script.lua(), cast_fires) {
        script.queue_event(name, args);
    }
    lua::showtooltip::tick(script.lua(), &ca, &mut macro_display, now);
    lua::cvar::tick(script.lua(), &ca);
    lua::friends::tick(script.lua(), &ca, now);
    let faction_events = {
        let mut watch = std::mem::take(&mut ca.lock().faction_watch);
        let events = watch.tick(script.lua());
        ca.lock().faction_watch = watch;
        events
    };
    for (name, args) in faction_events {
        script.queue_event(name, args);
    }
    for slot in totem_updates {
        script.queue_event(
            "PLAYER_TOTEM_UPDATE",
            vec![ScriptValue::Number(slot as f64 + 1.0)],
        );
    }
    for (name, id, ok) in load_results {
        script.queue_event(
            name,
            vec![ScriptValue::Number(f64::from(id)), ScriptValue::Bool(ok)],
        );
    }
    lap.mark("items+signals");
}

/// The tracker's `UNIT_SPELLCAST_*` events, each `(unit, castGUID, spellID, spellName, rank)`
/// (SENT puts the target's token second) and a remote unit's once per token naming it. A reticle
/// event's castGUID is nil, as retail's; the DLL sends "" only because its dispatcher cannot
/// put a nil mid-list.
fn cast_events(
    ca: &Ca,
    lua: &mlua::Lua,
    fires: Vec<cast::Fire>,
) -> Vec<(&'static str, Vec<ScriptValue>)> {
    let mut out = Vec::new();
    let Some(spells) = spells::table(&ca.db).filter(|_| !fires.is_empty()) else {
        return out;
    };
    for f in fires {
        if !benilla_ui::script::ext_read::has_listeners(lua, f.event) {
            continue;
        }
        let Some(rec) = spells.row(f.spell) else {
            continue;
        };
        let (name, rank) = cast::name_rank(&rec);
        let tokens = match f.who {
            cast::Who::Player => vec!["player".to_string()],
            cast::Who::Unit(guid) => lua::unit::identity::tokens_for_guid(lua, ca, guid),
        };
        let target = f
            .sent_target
            .map(|g| lua::unit::identity::token_from_guid(lua, ca, g).unwrap_or_default());
        for token in tokens {
            let mut args = vec![ScriptValue::Str(token)];
            if let Some(t) = &target {
                args.push(ScriptValue::Str(t.clone()));
            }
            args.push(
                f.cast_guid
                    .clone()
                    .map_or(ScriptValue::Nil, ScriptValue::Str),
            );
            args.push(ScriptValue::Number(f64::from(f.spell)));
            args.push(ScriptValue::Str(name.clone()));
            args.push(ScriptValue::Str(rank.clone()));
            out.push((f.event, args));
        }
    }
    out
}

/// The token table's per-frame half: the marks off the raid-target table, and the focus dropped
/// once its unit leaves the object table, unless it is a groupmate, whose roster guid outlives
/// its object (3.3.5's `FUN_00512a30` gate). Whether focus was dropped.
fn upkeep_tokens(tokens: &tokens::Tokens, m: &mirror::Mirror, view: &ExtView) -> bool {
    let mut t = tokens.lock();
    t.marks = m.marks;
    if t.focus != 0 && !view.is_streamed(t.focus) && !m.group.contains(&t.focus) {
        t.focus = 0;
        return true;
    }
    false
}

/// `NamePlate::Events::OnWorldTick`: diff the plates bound this frame against last frame's.
/// A frame never seen fires `NAME_PLATE_CREATED` with the frame; a new unit takes a slot and
/// fires `NAME_PLATE_UNIT_ADDED("nameplateN")`; a gone unit fires `NAME_PLATE_UNIT_REMOVED` with
/// its slot's token, then frees it.
fn sync_nameplates(ca: &Ca, script: &mut UiScript) {
    let live = benilla_ui::script::ext_read::nameplates(script.lua());
    let (created, added, removed) = {
        let mut st = ca.lock();
        let mut created = Vec::new();
        for (_, frame) in &live {
            if st.plates_seen.insert(*frame) {
                created.push(*frame);
            }
        }
        let added: Vec<u64> = live
            .iter()
            .filter(|(g, _)| !st.plates_last.iter().any(|(l, _)| l == g))
            .map(|(g, _)| *g)
            .collect();
        let removed: Vec<(u64, u32)> = st
            .plates_last
            .iter()
            .filter(|(g, _)| !live.iter().any(|(l, _)| l == g))
            .copied()
            .collect();
        st.plates_last = live.clone();
        (created, added, removed)
    };
    for frame in created {
        script.fire_event("NAME_PLATE_CREATED", vec![ScriptValue::Object(frame)]);
    }
    for guid in added {
        let slot = ca.tokens.lock().assign_plate(guid);
        script.fire_event(
            "NAME_PLATE_UNIT_ADDED",
            vec![ScriptValue::Str(format!("nameplate{}", slot + 1))],
        );
    }
    for (guid, frame) in removed {
        let Some(slot) = ca.tokens.lock().plate_index(guid) else {
            continue;
        };
        // A frame rebound to another unit this frame belongs to that unit now.
        let reassigned = live.iter().any(|(_, f)| *f == frame);
        if !reassigned {
            ca.lock().plate_removing = Some((guid, frame));
        }
        script.fire_event(
            "NAME_PLATE_UNIT_REMOVED",
            vec![ScriptValue::Str(format!("nameplate{slot}"))],
        );
        ca.lock().plate_removing = None;
        ca.tokens.lock().free_plate(guid);
    }
}

//! Nampower on top of benilla: the spell queue, its `NP_` CVars, and its Lua API and events,
//! ported from the `nampower.dll` that patches the 1.12.1 client (`brues-code/nampower` 4.6.1).
//! The DLL detours the client's own functions; here the same behaviour reaches benilla through
//! the seams of `benilla_app::ext`, and nothing in benilla knows nampower exists.
//!
//! - [`engine`]: the cast state machine, timers and queues, pure and unit-tested.
//! - [`gate`]: the engine behind benilla's cast gate.
//! - [`net`]: the server events the engine and the custom events read.
//! - [`lua`]: the Lua API, native functions and the bootstrap chunk.
//! - [`mirror`]: the game state the Lua functions read.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use benilla_app::ext::{
    CastGateHook, ExtCancelAura, ExtCast, ExtCastSet, ExtRaidMark, ExtView, NameplateHook,
    ScriptInstallers, ScriptValue, UiScript,
};
use bevy::prelude::*;

pub mod dbc;
pub mod engine;
mod events;
mod gate;
mod keys;
mod lua;
pub mod mirror;
mod net;
pub mod queue;
pub mod settings;

use engine::{Action, Engine, Origin};
use mirror::Mirror;

/// The nampower release this port follows, `GetNampowerVersion`'s answer.
pub const VERSION: (u32, u32, u32) = (4, 6, 1);

/// Everything nampower keeps, behind one lock: the gate, the net handlers, the frame system and
/// the Lua functions share it, and none holds it across a call back into Lua.
pub struct State {
    pub engine: Engine,
    pub mirror: Mirror,
    epoch: Instant,
    /// Casts sent as [`ExtCast`]s, oldest first, matched to their attempt at the gate.
    pending: VecDeque<(u32, Origin, Option<u64>)>,
    /// What the frame system flushes next.
    outbox: Vec<Action>,
    /// Local raid marks to write.
    marks: Vec<ExtRaidMark>,
    /// Our auras to cancel.
    cancels: Vec<u32>,
    /// The nampower events some frame registered, refreshed each frame.
    wanted: HashSet<&'static str>,
    latency_read_at: Option<Instant>,
}

impl State {
    fn new() -> Self {
        Self {
            engine: Engine::default(),
            mirror: Mirror::default(),
            epoch: Instant::now(),
            pending: VecDeque::new(),
            outbox: Vec::new(),
            marks: Vec::new(),
            cancels: Vec::new(),
            wanted: HashSet::new(),
            latency_read_at: None,
        }
    }

    /// `at` on the engine's ms scale.
    pub fn ms(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_millis() as u64
    }

    pub fn now_ms(&self) -> u64 {
        self.ms(Instant::now())
    }

    /// Whether a frame registered `event`.
    pub fn wants(&self, event: &str) -> bool {
        self.wanted.contains(event)
    }

    /// Queue `event` for the frame's flush when some frame registered it.
    pub fn emit(&mut self, event: &'static str, args: impl FnOnce() -> Vec<ScriptValue>) {
        if self.wants(event) {
            self.outbox.push(Action::Event(event, args()));
        }
    }

    /// Send a cast through the ladder, remembered so its attempt carries `origin`.
    pub fn cast(&mut self, spell_id: u32, target: Option<u64>, origin: Origin) {
        self.outbox.push(Action::Cast {
            spell_id,
            target,
            item: None,
            origin,
        });
    }

    pub fn set_local_mark(&mut self, icon: u8, guid: u64) {
        self.marks.push(ExtRaidMark { icon, guid });
    }

    pub fn cancel_aura(&mut self, spell_id: u32) {
        self.cancels.push(spell_id);
    }
}

/// The shared state and the client databases.
#[derive(Resource, Clone)]
pub struct Np {
    state: Arc<Mutex<State>>,
    pub db: Arc<dbc::Databases>,
}

impl Np {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct NampowerPlugin;

impl Plugin for NampowerPlugin {
    fn build(&self, app: &mut App) {
        let np = Np {
            state: Arc::new(Mutex::new(State::new())),
            db: Arc::new(dbc::Databases::load()),
        };
        if np.db.spell.is_none() {
            warn!("nampower: no Spell.dbc in the install; the queue passes every cast through");
        }
        app.insert_resource(np.clone());
        if let Some(mut hook) = app.world_mut().get_resource_mut::<CastGateHook>() {
            hook.0 = Some(Box::new(gate::NpGate(np.clone())));
        }
        let installer = np.clone();
        if let Some(mut installers) = app.world_mut().get_resource_mut::<ScriptInstallers>() {
            installers.add(move |_, script| lua::install(&installer, script));
        }
        net::register(app);
        app.add_systems(Update, (keys::key_events, frame).chain().in_set(ExtCastSet));
    }
}

/// The frame: mirror the world, refresh the wanted events, sample latency, tick the engine,
/// then send its casts and fire its events and scripts with the lock released.
fn frame(
    np: Res<Np>,
    view: ExtView,
    mut casts: MessageWriter<ExtCast>,
    mut marks: MessageWriter<ExtRaidMark>,
    mut cancels: MessageWriter<ExtCancelAura>,
    mut plates: ResMut<NameplateHook>,
    script: Option<NonSendMut<UiScript>>,
) {
    let now = Instant::now();
    let mut script = script;
    // Lua reads first, with the lock free: the VM's clock and, once a second, the latency.
    let (ui_now, latency) = match script.as_deref() {
        Some(s) => {
            let read_latency = np
                .lock()
                .latency_read_at
                .is_none_or(|at| now.duration_since(at).as_secs() >= 1);
            let call = |name: &str| -> mlua::Result<mlua::MultiValue> {
                s.lua().globals().get::<mlua::Function>(name)?.call(())
            };
            let number = |v: Option<&mlua::Value>| match v {
                Some(mlua::Value::Number(n)) => Some(*n),
                Some(mlua::Value::Integer(i)) => Some(*i as f64),
                _ => None,
            };
            (
                call("GetTime").ok().and_then(|v| number(v.front())),
                read_latency
                    .then(|| call("GetNetStats").ok().and_then(|v| number(v.get(2))))
                    .flatten(),
            )
        }
        None => (None, None),
    };
    let (actions, raid_marks, aura_cancels) = {
        let mut st = np.lock();
        let ms = st.ms(now);
        if let Some(t) = ui_now {
            st.mirror.clock_anchor = Some((now, t));
        }
        if let Some(l) = latency {
            st.engine.sample_latency(l.max(0.0) as u64);
            st.latency_read_at = Some(now);
        }
        if let Some(s) = script.as_deref() {
            st.wanted = events::ALL
                .iter()
                .copied()
                .filter(|e| s.has_event_registrations(e))
                .collect();
        }
        let changes = st.mirror.refresh(&view, now);
        events::unit_diffs(&mut st, &np.db, &changes);
        // `NP_NameplateDistance`: the reference's 20 yd leaves benilla's own range alone.
        let range = st.engine.settings.nameplate_distance;
        let wanted = (range != 20.0).then_some(range);
        if plates.max_distance != wanted {
            plates.max_distance = wanted;
        }
        st.engine.tick(ms);
        let engine_out = std::mem::take(&mut st.engine.out);
        st.outbox.extend(engine_out);
        let actions = std::mem::take(&mut st.outbox);
        for a in &actions {
            if let Action::Cast {
                spell_id,
                target,
                origin,
                ..
            } = a
            {
                st.pending.push_back((*spell_id, *origin, *target));
            }
        }
        (
            actions,
            std::mem::take(&mut st.marks),
            std::mem::take(&mut st.cancels),
        )
    };
    for mark in raid_marks {
        marks.write(mark);
    }
    for spell_id in aura_cancels {
        cancels.write(ExtCancelAura { spell_id });
    }
    for action in actions {
        match action {
            Action::Cast {
                spell_id,
                target,
                item,
                ..
            } => {
                casts.write(ExtCast {
                    spell_id,
                    target,
                    item,
                    place: None,
                });
            }
            Action::Script(chunk) => {
                if let Some(s) = script.as_deref_mut() {
                    if let Err(e) = s.run_chunk_named(chunk.as_bytes(), "QueueScript") {
                        s.report_script_error(&e.to_string());
                    }
                }
            }
            Action::Event(name, args) => {
                if let Some(s) = script.as_deref_mut() {
                    s.queue_event(name, args);
                }
            }
            Action::UiError(text) => {
                if let Some(s) = script.as_deref_mut() {
                    s.queue_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
                }
            }
            Action::DisenchantStop => {
                if let Some(s) = script.as_deref_mut() {
                    let _ = s.run("if NP_DisenchantStop then NP_DisenchantStop() end");
                }
            }
        }
    }
}

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
    ExtCancelAura, ExtCastSet, ExtRange, ExtUsable, ExtView, ExtWorld, ScriptInstallers,
    ScriptValue, UiScript,
};
use bevy::prelude::*;

pub mod cooldown;
pub mod dbc;
pub mod items;
mod lua;
pub mod mirror;
mod net;
pub mod spellmod;
pub mod spells;
pub mod talents;

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

/// The shared state and the client databases.
#[derive(Resource, Clone, Default)]
pub struct Ca {
    state: Arc<Mutex<State>>,
    pub db: Arc<dbc::Databases>,
}

impl Ca {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
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
        app.add_systems(Update, frame.in_set(ExtCastSet));
    }
}

/// The frame: mirror the world, then fire the events the net handlers and the natives queued.
fn frame(
    ca: Res<Ca>,
    view: ExtView,
    world: ExtWorld,
    range: ExtRange,
    usable: ExtUsable,
    mut cancels: MessageWriter<ExtCancelAura>,
    script: Option<NonSendMut<UiScript>>,
) {
    let now = Instant::now();
    // `GetTime()` read with the lock free, so the mirror can put instants on its clock.
    let ui_now = script.as_deref().and_then(|s| {
        s.lua()
            .globals()
            .get::<mlua::Function>("GetTime")
            .and_then(|f| f.call::<f64>(()))
            .ok()
    });
    let (events, cancel) = {
        let mut st = ca.lock();
        st.mirror.refresh(&view, &world, &range, now);
        let known = st.known.clone();
        st.mirror.refresh_usable(&usable, &known, now);
        if let Some(t) = ui_now {
            st.mirror.clock = Some((now, t));
        }
        (
            std::mem::take(&mut st.events),
            std::mem::take(&mut st.cancels),
        )
    };
    for spell_id in cancel {
        cancels.write(ExtCancelAura { spell_id });
    }
    let Some(mut script) = script else {
        return;
    };
    for (name, args) in events {
        script.queue_event(name, args);
    }
}

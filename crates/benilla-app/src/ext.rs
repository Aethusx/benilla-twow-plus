//! The seams a crate built on top of benilla reaches through ([`crate::run_with`]). Each opens one
//! piece of benilla's own 1.12.1 machinery, read-only or as a single hook, and none changes what
//! benilla does while nothing is installed in it:
//!
//! - [`NetHandlerApp`]: register a handler on any decoded server event, run after benilla's own.
//! - [`ScriptInstallers`]: install Lua globals on each in-game VM before the UI and addons load.
//! - [`CastGateHook`]: see every cast press before the cast ladder, and hold it or let it past
//!   the in-flight guard; [`CastOutcome`] reports what the ladder then did.
//! - [`ExtCast`]: send a cast through the ladder, as a press would, at a chosen unit.
//! - [`ExtView`]: read the selection, unit tokens, spell and item records, cooldowns and objects.
//! - `UiScript::set_unit_token_extension` and `set_extra_unit_guids` (benilla-ui): a wider unit
//!   token grammar, whose units' snapshots and aura lists the feeds here push.

use std::time::Instant;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

pub use crate::net::handlers::NetHandlerApp;
pub use benilla_formats::SpellDisplay;
pub use benilla_protocol::messages::ItemInfo;
pub use benilla_protocol::{SessionEvent, SessionEventKind};
pub use benilla_ui::script::{ScriptValue, UiScript};

use crate::net::{Guid, ObjectStore};
use crate::spell::CastCommit;

/// A Lua installer: runs on each in-game VM after the CVar table is seeded and before the stock
/// interface and the addons load, so a global it sets is there when an addon's file scope runs.
pub type ScriptInstaller = Box<dyn Fn(&mut World, &mut UiScript) + Send + Sync>;

/// The installers, run in the order they were added on every world entry and `ReloadUI`.
#[derive(Resource, Default)]
pub struct ScriptInstallers(Vec<ScriptInstaller>);

impl ScriptInstallers {
    pub fn add(&mut self, installer: impl Fn(&mut World, &mut UiScript) + Send + Sync + 'static) {
        self.0.push(Box::new(installer));
    }
}

/// Run every installer on a VM the world does not hold yet.
pub(crate) fn run_script_installers(world: &mut World, script: &mut UiScript) {
    let Some(installers) = world.remove_resource::<ScriptInstallers>() else {
        return;
    };
    for installer in &installers.0 {
        installer(world, script);
    }
    world.insert_resource(installers);
}

/// An item use as the cast ladder commits it: the item's wire position and spell ordinal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemUse {
    pub bag_index: u8,
    pub slot: u8,
    /// The item's template entry.
    pub entry: u32,
    pub spell_index: u8,
}

/// One cast press, as the gate sees it before the ladder runs.
pub struct CastAttempt<'a> {
    pub spell_id: u32,
    /// The spell's record; `None` for a spell not in `Spell.dbc`.
    pub spell: Option<&'a SpellDisplay>,
    /// Set for an item use.
    pub item: Option<ItemUse>,
    /// The unit the press binds before the ladder resolves it: the selection, the caster for a
    /// self-cast, or the unit an [`ExtCast`] named.
    pub target: Option<u64>,
    /// The caster's guid.
    pub caster: Option<u64>,
    /// The cast time with the caster's level scaling and talent modifiers, in ms.
    pub cast_time_ms: u32,
    /// The global cooldown this cast arms, with talent modifiers; 0 for an off-GCD spell.
    pub gcd_ms: u32,
    /// The longest cooldown remainder the not-ready rung reads (spell, category and GCD legs).
    pub cooldown_remaining_ms: u32,
    /// Sent by an [`ExtCast`], not pressed.
    pub requeued: bool,
    pub now: Instant,
}

/// What the gate makes of a press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateVerdict {
    /// Run the ladder as benilla does.
    Pass,
    /// Run the ladder with the in-flight guard released: the previous cast is taken as done.
    PassOverInFlight,
    /// Run nothing: the gate keeps the press or drops it.
    Stop,
}

/// What the ladder did with a press the gate passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastOutcome {
    /// The cast packet went out.
    Sent,
    /// Refused locally with this cast-result reason.
    Refused(u8),
    /// Neither: the targeting cursor came up, the press waits at the attack pick, or the press
    /// bailed silently.
    Pending,
}

/// A hook on the cast-send path, called for every press that reaches the ladder.
pub trait CastGate: Send + Sync + 'static {
    fn attempt(&mut self, attempt: &CastAttempt) -> GateVerdict;
    fn outcome(&mut self, attempt: &CastAttempt, outcome: CastOutcome);
    /// A targeting cursor's click sent `spell_id`, the cast an earlier [`CastOutcome::Pending`]
    /// left waiting at the cursor.
    fn targeted_sent(&mut self, _spell_id: u32, _now: Instant) {}
}

/// The installed gate; empty, every press runs the ladder unchanged.
#[derive(Resource, Default)]
pub struct CastGateHook(pub Option<Box<dyn CastGate>>);

/// A cast sent through the ladder at `target` (the selection when `None`), marked
/// [`CastAttempt::requeued`]. Written in [`ExtCastSet`], applied after the frame's script calls.
#[derive(Message, Clone, Copy, Debug)]
pub struct ExtCast {
    pub spell_id: u32,
    pub target: Option<u64>,
    pub item: Option<ItemUse>,
}

/// Set raid mark `icon` (0-7, star to skull) on `guid` (0 clears it) in the client's own table,
/// with no packet: what `MSG_RAID_TARGET_UPDATE` would have written.
#[derive(Message, Clone, Copy, Debug)]
pub struct ExtRaidMark {
    pub icon: u8,
    pub guid: u64,
}

/// Select the unit `guid` names, as `TargetUnit` does; an unstreamed or non-unit guid is a no-op.
#[derive(Message, Clone, Copy, Debug)]
pub struct ExtSelect {
    pub guid: u64,
}

/// Float a combat text over `guid` (our own player when `None`) through benilla's floating text,
/// in category `category` (0 number, 1 absorb, 2 crit, 3 miss word, 4 XP, 5 honor) and an
/// optional `0xAARRGGBB` colour.
#[derive(Message, Clone, Debug)]
pub struct ExtCombatText {
    pub guid: Option<u64>,
    pub text: String,
    pub category: u8,
    pub color: Option<u32>,
}

/// What a crate on top changes in the floating combat text; the default changes nothing.
#[derive(Resource, Default)]
pub struct CombatTextHook {
    /// Drop category 4, the XP numbers.
    pub hide_exp: bool,
}

/// What a crate on top changes in the nameplates; the default changes nothing.
#[derive(Resource, Default)]
pub struct NameplateHook {
    /// The plate range in yards, in place of the reference's 20.
    pub max_distance: Option<f32>,
    /// Units that get no plate this frame.
    pub hidden: std::collections::HashSet<u64>,
}

/// Cancel our aura of `spell_id` (`CMSG_CANCEL_AURA`), as a right-click on its icon does.
#[derive(Message, Clone, Copy, Debug)]
pub struct ExtCancelAura {
    pub spell_id: u32,
}

/// The local state folder (`benilla-config/`), or `None` when persistence is off.
pub fn local_state_dir() -> Option<std::path::PathBuf> {
    crate::local_state::home()
}

/// The local time as the chat and combat logs stamp each line, `M/D HH:MM:SS.mmm`.
pub fn log_stamp() -> String {
    crate::ui_chat::logging::stamp()
}

/// One cooldown record, as the client's `SpellHistory` node holds it: three timers as
/// `(start, duration)`, a zero duration untracked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CooldownRecord {
    pub spell_id: u32,
    /// The cast item's entry, 0 for a spell.
    pub item_id: u32,
    pub category: u32,
    /// The category matches every query (wand Shoot's 351).
    pub category_wildcard: bool,
    pub gcd_category: u32,
    pub recovery: (Instant, std::time::Duration),
    pub category_recovery: (Instant, std::time::Duration),
    pub gcd: (Instant, std::time::Duration),
    /// Parked until `SMSG_COOLDOWN_EVENT`.
    pub on_hold: bool,
}

/// The set an [`ExtCast`] writer runs in, ahead of the apply.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtCastSet;

impl From<ItemUse> for CastCommit {
    fn from(item: ItemUse) -> Self {
        CastCommit::Item {
            bag_index: item.bag_index,
            slot: item.slot,
            entry: item.entry,
            spell_index: item.spell_index,
            on_object: None,
        }
    }
}

fn apply_ext_raid_marks(
    mut marks: MessageReader<ExtRaidMark>,
    mut group: ResMut<crate::ui_party::GroupState>,
) {
    for mark in marks.read() {
        group.apply_raid_target(mark.icon, mark.guid);
    }
}

fn apply_ext_cancel_auras(
    mut cancels: MessageReader<ExtCancelAura>,
    commands: Res<crate::net::NetCommands>,
) {
    for cancel in cancels.read() {
        let _ = commands.0.send(crate::net::ClientCommand::CancelAura {
            spell_id: cancel.spell_id,
        });
    }
}

fn apply_ext_selects(
    mut selects: MessageReader<ExtSelect>,
    mut commit: crate::target::SelectCommit,
) {
    for select in selects.read() {
        commit.select_unit(select.guid);
    }
}

fn apply_ext_combat_text(
    mut texts: MessageReader<ExtCombatText>,
    mut spawns: MessageWriter<crate::combat_text::CombatTextSpawn>,
    index: Res<crate::net::GuidIndex>,
    me: Query<Entity, With<crate::net::SelfPlayer>>,
) {
    for t in texts.read() {
        let anchor = match t.guid {
            Some(guid) => index.0.get(&guid).copied(),
            None => me.single().ok(),
        };
        let Some(anchor) = anchor else {
            continue;
        };
        spawns.write(crate::combat_text::CombatTextSpawn {
            anchor,
            text: t.text.clone(),
            category: t.category.min(5),
            color: t.color,
        });
    }
}

/// Send this frame's [`ExtCast`]s through the ladder, in order.
pub(crate) fn apply_ext_casts(
    mut casts: MessageReader<ExtCast>,
    mut cast: crate::spell::ScriptCast,
) {
    for request in casts.read() {
        let commit = request.item.map_or(CastCommit::Spell, CastCommit::from);
        let crate::spell::ScriptCast { targeting, ladder } = &mut cast;
        let ctx = match request.target {
            Some(guid) => targeting.context_at(guid),
            None => targeting.context(),
        };
        ladder.send_requeued(request.spell_id, &ctx, commit);
    }
}

pub(crate) struct ExtPlugin;

impl Plugin for ExtPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScriptInstallers>()
            .init_resource::<CastGateHook>()
            .add_message::<ExtCast>()
            .add_message::<ExtRaidMark>()
            .add_message::<ExtCancelAura>()
            .add_message::<ExtSelect>()
            .add_message::<ExtCombatText>()
            .init_resource::<CombatTextHook>()
            .init_resource::<NameplateHook>()
            .configure_sets(
                Update,
                ExtCastSet
                    .before(apply_ext_casts)
                    .before(apply_ext_raid_marks)
                    .before(apply_ext_cancel_auras)
                    .before(apply_ext_selects)
                    .before(apply_ext_combat_text),
            )
            // `apply_ext_casts` runs in the target chain, ahead of the frame's script calls: a
            // queued cast whose time came goes out before a press made this frame.
            .add_systems(
                Update,
                (
                    apply_ext_raid_marks,
                    apply_ext_cancel_auras,
                    apply_ext_selects.in_set(crate::target::TargetUpdate),
                    apply_ext_combat_text.before(crate::ui_pass::UiQuadAppend),
                ),
            );
    }
}

/// One cooldown read, as `GetSpellCooldown` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CooldownRead {
    pub remaining_ms: u32,
    pub duration_ms: u32,
    /// `false` for a cooldown parked until its event (Stealth, Feign Death).
    pub enabled: bool,
}

/// Read-only views of the game state a crate on top reads.
#[derive(SystemParam)]
pub struct ExtView<'w, 's> {
    units: crate::ui_unit::UnitTokens<'w, 's>,
    selection: Res<'w, crate::target::Selection>,
    spells: Option<Res<'w, crate::ui_action::Spells>>,
    items: Res<'w, crate::items::Items>,
    cooldowns: Res<'w, crate::spell::Cooldowns>,
    channel: Res<'w, crate::spell::ActiveChannel>,
    pending: Res<'w, crate::spell::PendingCast>,
    group: Res<'w, crate::ui_party::GroupState>,
    spell_mods: Res<'w, crate::spell::SpellModifiers>,
    player: Option<Res<'w, crate::player::Player>>,
    changed: Query<'w, 's, (&'static Guid, &'static ObjectStore), Changed<ObjectStore>>,
    all: Query<'w, 's, (&'static Guid, &'static ObjectStore)>,
    placed: Query<'w, 's, (&'static Guid, &'static GlobalTransform), With<ObjectStore>>,
    engaged: Query<
        'w,
        's,
        (),
        (
            With<crate::net::SelfPlayer>,
            With<crate::creature_anim::Engaged>,
        ),
    >,
    auto_repeat: Res<'w, crate::spell::AutoRepeatActive>,
}

impl ExtView<'_, '_> {
    /// The guid a unit token names, as the stock unit functions resolve it (`0x515970`).
    pub fn unit_guid(&self, token: &str) -> Option<u64> {
        self.units
            .resolve(token, &self.selection)
            .map(|(_, guid)| guid)
    }

    /// The selection's guid.
    pub fn target_guid(&self) -> Option<u64> {
        self.selection.guid
    }

    /// A spell's record.
    pub fn spell(&self, spell_id: u32) -> Option<&SpellDisplay> {
        self.spells.as_ref()?.catalog.get(spell_id)
    }

    /// An item template the client has cached.
    pub fn item_template(&self, entry: u32) -> Option<&ItemInfo> {
        self.items.cached(entry)
    }

    /// The cooldown read for a spell, or for an item's use spell when `item_entry` is nonzero.
    pub fn cooldown(&self, spell_id: u32, item_entry: u32, now: Instant) -> CooldownRead {
        let info = self
            .cooldowns
            .info(spell_id, item_entry, self.spell(spell_id), now);
        CooldownRead {
            remaining_ms: info.remaining_ms,
            duration_ms: info.duration_ms,
            enabled: info.enabled,
        }
    }

    /// Our running channel's spell, if one is open.
    pub fn channel(&self, now: Instant) -> Option<u32> {
        self.channel.current(now)
    }

    /// Our cast in flight between send and resolution, if one is.
    pub fn cast_in_flight(&self, now: Instant) -> Option<u32> {
        self.pending.current(now)
    }

    /// The unit wearing raid mark `index` (1-8, star to skull).
    pub fn raid_mark(&self, index: u8) -> Option<u64> {
        let slot = usize::from(index.checked_sub(1)?);
        self.group
            .raid_targets
            .get(slot)
            .copied()
            .filter(|g| *g != 0)
    }

    /// Whether we are in a party or raid.
    pub fn in_group(&self) -> bool {
        self.group.in_group
    }

    /// Every streamed object whose descriptor changed since this system last ran, first runs
    /// included.
    pub fn changed_objects(&self) -> impl Iterator<Item = (u64, &benilla_protocol::ObjectFields)> {
        self.changed.iter().map(|(g, s)| (g.0, &s.0))
    }

    /// Every streamed object.
    pub fn objects(&self) -> impl Iterator<Item = (u64, &benilla_protocol::ObjectFields)> {
        self.all.iter().map(|(g, s)| (g.0, &s.0))
    }

    /// `GetSpellModifiers` for our spell and op: the flat sum and the percent sum, or `None`
    /// when none applies.
    pub fn spell_modifier(&self, spell_id: u32, op: u8) -> Option<(i32, i32)> {
        self.spell_mods.sums(self.spell(spell_id)?, op)
    }

    /// Our movement flags (`MOVEFLAG_*`), 0 out of the world.
    pub fn move_flags(&self) -> u32 {
        self.player.as_deref().map_or(0, |p| p.move_flags())
    }

    /// Our melee auto-attack is running (the client's attack lock).
    pub fn attacking(&self) -> bool {
        !self.engaged.is_empty()
    }

    /// Our running auto-repeat spell (Auto Shot, wand Shoot).
    pub fn auto_repeat(&self) -> Option<u32> {
        self.auto_repeat.0
    }

    /// Every streamed object's world position.
    pub fn positions(&self) -> impl Iterator<Item = (u64, Vec3)> + '_ {
        self.placed.iter().map(|(g, t)| (g.0, t.translation()))
    }

    /// Every cooldown record of the player's list.
    pub fn cooldown_records(&self) -> Vec<CooldownRecord> {
        self.cooldowns.export().collect()
    }

    /// Moves whenever the cooldown list changes or prunes.
    pub fn cooldown_epoch(&self) -> u64 {
        self.cooldowns.feed_epoch()
    }

    /// Whether `guid` is streamed.
    pub fn is_streamed(&self, guid: u64) -> bool {
        self.units.held(guid).is_some()
    }
}

/// More read-only views: hostility, creature templates, orientation and the camera.
#[derive(SystemParam)]
pub struct ExtWorld<'w, 's> {
    index: Res<'w, crate::net::GuidIndex>,
    stores: Query<'w, 's, &'static ObjectStore>,
    me: Query<'w, 's, &'static ObjectStore, With<crate::net::SelfPlayer>>,
    factions: Option<Res<'w, crate::target::Factions>>,
    reputations: Res<'w, crate::net::Reputations>,
    names: Res<'w, crate::names::NameCache>,
    camera: Query<'w, 's, &'static Transform, With<benilla_world::view::WorldCamera>>,
    placed: Query<'w, 's, (&'static Guid, &'static GlobalTransform), With<ObjectStore>>,
    speeds: Query<'w, 's, &'static crate::net::UnitSpeeds>,
}

impl ExtWorld<'_, '_> {
    fn store(&self, guid: u64) -> Option<&ObjectStore> {
        self.stores.get(*self.index.0.get(&guid)?).ok()
    }

    /// `CanAttack` from our player, as the TAB scan and the nameplates read it.
    pub fn can_attack(&self, guid: u64) -> bool {
        let Some(store) = self.store(guid) else {
            return false;
        };
        crate::target::can_attack(
            Some(store),
            self.factions.as_deref(),
            &self.reputations,
            self.me.single().ok(),
        )
    }

    /// A creature's `CreatureType.dbc` id and its rank (3 is a world boss), once its template has
    /// been queried; `None` for a player or an unknown template.
    pub fn creature(&self, guid: u64) -> Option<(u32, u32)> {
        let store = self.store(guid)?;
        let entry = store.0.object_entry()?;
        let rec = self.names.creature_record(entry)?;
        Some((
            rec.creature_type,
            crate::names::gated_rank(Some(rec), Some(store)),
        ))
    }

    /// The world camera's position and forward direction.
    pub fn camera(&self) -> Option<(Vec3, Vec3)> {
        let t = self.camera.single().ok()?;
        Some((t.translation, *t.forward()))
    }

    /// A unit's movement speeds as its `LIVING` block and the speed changes last set them.
    pub fn speeds(&self, guid: u64) -> Option<benilla_protocol::MoveSpeeds> {
        self.speeds.get(*self.index.0.get(&guid)?).ok().map(|s| s.0)
    }

    /// Every streamed object's position and rotation.
    pub fn placements(&self) -> impl Iterator<Item = (u64, Vec3, Quat)> + '_ {
        self.placed.iter().map(|(g, t)| {
            let (_, rotation, translation) = t.to_scale_rotation_translation();
            (g.0, translation, rotation)
        })
    }
}

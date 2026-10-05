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
//! - [`ExtRange`]: each unit's range inputs, as `GetMinMaxRange` reads them.
//! - [`ExtUsable`]: the usability walk `IsUsableAction` runs, for any spell.
//! - [`ExtUnitTokens`]: a crate's own unit tokens, whose units fire the stock `UNIT_*` events.
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
    /// What to do with a targeting cursor the cast raises; `None` leaves it to the player.
    pub place: Option<ExtPlace>,
}

/// What to do with the targeting cursor an [`ExtCast`] raised, applied right after its send: a
/// ground-target spell's cursor is committed at the point, as a terrain click
/// commits it (`BindLocation 0x6e60f0`); any other cursor still up is cancelled (`StopTargeting
/// 0x6e4900`), unless the ask is [`ExtPlace::KeepGround`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExtPlace {
    /// Commit at a WoW-space point.
    At([f32; 3]),
    /// Commit where the cursor ray hits the world; with no hit, cancel.
    Cursor,
    /// Leave a ground cursor up for the player; cancel any other.
    KeepGround,
}

/// Start melee at `Some(guid)`, the selection for `Some(0)` (`FUN_ATTACK_RESOLVE_TARGET`), or stop it
/// for `None`: the client's StartAttack (`0x5ecb70`) and StopAttack (`0x5ecac0`), whose callers own
/// the attackability check; this one leaves it to the server, as a direct engine call does.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtAttack(pub Option<u64>);

/// What a crate shows for a macro in place of the bound spell the client derives from its body
/// (`[rec+0x564]`, `0x4efe00`), by 1-based macro index. The slot's state (usable, cooldown, range,
/// count, checked) resolves through it as through the bound spell, and a macro whose own icon is
/// the question mark shows the spell's or the item's icon. Empty, nothing changes.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct ExtMacroDisplay(pub std::collections::HashMap<u32, MacroShow>);

/// One macro's display, [`ExtMacroDisplay`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroShow {
    Spell(u32),
    Item(u32),
    /// A value named nothing the player has: the slot greys, as an unresolved `/cast` does.
    Unresolved,
    /// Nothing to show: the macro's own icon, usable, as a macro that casts nothing.
    Nothing,
}

/// Ask the server for a template by id, as the client's caches ask on a miss: `CMSG_CREATURE_QUERY`
/// and `CMSG_GAMEOBJECT_QUERY` with no guid, `CMSG_QUEST_QUERY`. The answer arrives as the usual
/// session event, which benilla's own caches take too.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtQuery {
    Creature(u32),
    GameObject(u32),
    Quest(u32),
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

/// A crate's own unit tokens and the units they name this frame, `(token, guid)`: each unit's
/// snapshot is diffed and fires the stock per-field `UNIT_*` events with that token as `arg1`, as
/// a stock token's does. Write it only when it changes; empty, nothing extra fires.
#[derive(Resource, Default, PartialEq)]
pub struct ExtUnitTokens(pub Vec<(String, u64)>);

/// What a crate on top changes in the nameplates; the default changes nothing.
#[derive(Resource, Default)]
pub struct NameplateHook {
    /// The plate range in yards, in place of the reference's 20.
    pub max_distance: Option<f32>,
    /// Units that get no plate this frame.
    pub hidden: std::collections::HashSet<u64>,
}

/// A crate's hold on the sound-effect funnel, the kit and `PlaySoundFile` plays (the client's
/// `FUN_SOUND_PLAY_BY_PATH`): kits it starts by id, paths it mutes, and a log of the files opened.
/// The default plays, mutes and logs nothing. Zone music and ambience stream through their own
/// loader and pass it by.
#[derive(Resource, Default)]
pub struct ExtSound {
    /// Kits to start this frame, 2D on the SFX slider; benilla drains it.
    pub plays: Vec<ExtSoundPlay>,
    /// Paths that never open, lowercased with `\` separators: a kit whose pick is one, or a
    /// `PlaySoundFile` of one, plays nothing.
    pub muted: std::collections::HashSet<String>,
    /// Log every file the funnel opens, muted ones included, into [`Self::opened`].
    pub record: bool,
    /// The files opened since the crate last drained, `(path, muted)`, oldest first; capped at
    /// [`EXT_SOUND_LOG_CAP`] so an undrained log cannot grow.
    pub opened: Vec<(String, bool)>,
    /// The crate's plays still sounding, `token -> volume` (the kit's volume after the scale),
    /// rewritten each frame a play is live or ends.
    pub live: std::collections::HashMap<u64, f32>,
}

/// The console for a crate: every command and CVar it answers, and lines a crate writes to it.
/// benilla's console has no screen of its own, so a written line prints as system text in chat,
/// as a command's output does.
#[derive(Resource, Default)]
pub struct ExtConsole {
    /// The registered commands in name order, then the CVars in registration order; rebuilt
    /// when either count changes.
    pub commands: Vec<ExtConsoleCommand>,
    /// Lines to print, drained by benilla.
    pub echo: Vec<String>,
}

/// One console entry, [`ExtConsole::commands`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtConsoleCommand {
    pub name: String,
    /// The registered help line; empty for a CVar.
    pub help: String,
    pub cvar: bool,
}

/// Move a bag item as the client's direct senders do, with no cursor and no client-side lock:
/// a whole stack swaps with the destination (`CMSG_SWAP_INV_ITEM` within the player's own array,
/// else `CMSG_SWAP_ITEM`), and `count` splits that many off (`CMSG_SPLIT_ITEM`). Bags and slots
/// are the Lua ones: backpack 0, bags 1-4, bank -1, bank bags 5-10, keyring -2, slots 1-based.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtItemMove {
    pub src_bag: i64,
    pub src_slot: u32,
    pub dst_bag: i64,
    pub dst_slot: u32,
    pub count: Option<u32>,
}

/// The client-side item lock (`item+0x314` bit 0) for a crate: lock a bag slot's item as a send
/// would, until unlocked; unlock an item by guid; unlock everything. Each change fires
/// `ITEM_LOCK_CHANGED` for the slots it touches. The server knows nothing of these.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtItemLock {
    /// Lock the item at `(bag, slot)`, with its guid and stack count there now.
    Lock {
        bag: i64,
        slot: u32,
        guid: u64,
        count: u32,
    },
    Unlock(u64),
    UnlockAll,
}

/// Put text on the OS pasteboard, the one the edit boxes' copy and paste chords use.
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct ExtClipboard(pub String);

/// The most entries [`ExtSound::opened`] holds; older ones drop.
pub const EXT_SOUND_LOG_CAP: usize = 256;

/// One kit [`ExtSound::plays`] starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtSoundPlay {
    /// The crate's name for the play, its key in [`ExtSound::live`].
    pub token: u64,
    /// The `SoundEntries.dbc` id.
    pub kit: u32,
    /// A multiplier on the kit's own volume; `None` is 1.0.
    pub volume: Option<f32>,
}

/// A crate's loot verbs, the client's own senders with no walk-to: `CMSG_LOOT` at a unit (arming
/// the loot latch, as `0x5df2a0` does at its send), a take by wire slot (`CMSG_AUTOSTORE_LOOT_ITEM`
/// with no bind confirm), the coin, and the release.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtLootSend {
    /// `quiet`: the answer goes to [`ExtLoot::window`] alone; the loot frame never opens.
    Open {
        guid: u64,
        quiet: bool,
    },
    Item(u8),
    Money,
    Release(u64),
}

/// The last loot window the server sent, for a crate, and the quiet session it asked for.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct ExtLoot {
    /// The guid whose answer the loot frame does not show; cleared at its release.
    pub quiet: Option<u64>,
    /// The last admitted `SMSG_LOOT_RESPONSE`, until its release.
    pub window: Option<ExtLootWindow>,
    /// Admitted responses so far, so a crate sees a new one for the same guid.
    pub responses: u64,
}

/// One loot window as the wire delivered it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtLootWindow {
    pub guid: u64,
    /// Copper; 0 for none.
    pub gold: u32,
    pub items: Vec<ExtLootRow>,
}

/// One loot row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtLootRow {
    /// The wire slot `CMSG_AUTOSTORE_LOOT_ITEM` names.
    pub slot: u8,
    pub item_id: u32,
    pub count: u32,
    pub random_property: u32,
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

/// One press as the cast ladder resolved it, for observers that hold no gate: the spell, the
/// unit it was aimed at, and whether the packet went out, the ladder refused it, or it waits.
#[derive(Message, Clone, Copy, Debug)]
pub struct ExtCastNote {
    pub spell_id: u32,
    pub target: Option<u64>,
    pub outcome: CastOutcome,
    /// The cast time the client predicts (`GetCastTime 0x6e3340` with talents and level), ms.
    pub cast_time_ms: u32,
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

fn apply_ext_attacks(
    mut asks: MessageReader<ExtAttack>,
    selection: Res<crate::target::Selection>,
    engaged: Query<
        (),
        (
            With<crate::net::SelfPlayer>,
            With<crate::creature_anim::Engaged>,
        ),
    >,
    mut seam: crate::creature_anim::AttackSeam,
) {
    for ask in asks.read() {
        let engaged = !engaged.is_empty();
        match ask.0 {
            None => seam.stop(engaged),
            Some(guid) => {
                let target = if guid != 0 {
                    Some(guid)
                } else {
                    selection.guid
                };
                if let Some(target) = target {
                    seam.start(target, engaged, false);
                }
            }
        }
    }
}

fn apply_ext_loot(
    mut sends: MessageReader<ExtLootSend>,
    net: Res<crate::net::NetCommands>,
    mut latch: ResMut<crate::ui_loot::LootLatch>,
    mut ext: ResMut<ExtLoot>,
) {
    use crate::net::ClientCommand as C;
    for send in sends.read() {
        let cmd = match *send {
            ExtLootSend::Open { guid, quiet } => {
                latch.0 = Some(guid);
                ext.quiet = quiet.then_some(guid);
                C::Loot { guid }
            }
            ExtLootSend::Item(slot) => C::AutostoreLootItem { slot },
            ExtLootSend::Money => C::LootMoney,
            ExtLootSend::Release(guid) => {
                if ext.quiet == Some(guid) {
                    ext.quiet = None;
                }
                C::LootRelease { guid }
            }
        };
        let _ = net.0.send(cmd);
    }
}

fn apply_ext_queries(mut asks: MessageReader<ExtQuery>, net: Res<crate::net::NetCommands>) {
    for ask in asks.read() {
        let cmd = match *ask {
            ExtQuery::Creature(entry) => {
                crate::net::ClientCommand::CreatureQuery { entry, guid: 0 }
            }
            ExtQuery::GameObject(entry) => {
                crate::net::ClientCommand::GameObjectQuery { entry, guid: 0 }
            }
            ExtQuery::Quest(quest) => crate::net::ClientCommand::QuestQuery { quest },
        };
        let _ = net.0.send(cmd);
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

/// Send this frame's [`ExtCast`]s through the ladder, in order, each followed by its
/// [`ExtPlace`].
pub(crate) fn apply_ext_casts(
    mut casts: MessageReader<ExtCast>,
    occlusion: Res<crate::target::PickOcclusion>,
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
        let Some(place) = request.place else {
            continue;
        };
        let at = match place {
            ExtPlace::At(at) => Some(Some(at)),
            ExtPlace::Cursor => Some(occlusion.point.map(benilla_assets::coords::bevy_to_wow)),
            ExtPlace::KeepGround => None,
        };
        crate::spell::targeting::world::place_ground_cast(ladder, at);
    }
}

pub(crate) struct ExtPlugin;

impl Plugin for ExtPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScriptInstallers>()
            .init_resource::<CastGateHook>()
            .add_message::<ExtCast>()
            .add_message::<ExtAttack>()
            .add_message::<ExtQuery>()
            .init_resource::<ExtMacroDisplay>()
            .add_message::<ExtRaidMark>()
            .add_message::<ExtCancelAura>()
            .add_message::<ExtSelect>()
            .add_message::<ExtCombatText>()
            .add_message::<ExtCastNote>()
            .init_resource::<CombatTextHook>()
            .init_resource::<NameplateHook>()
            .init_resource::<ExtUnitTokens>()
            .init_resource::<ExtSound>()
            .init_resource::<ExtConsole>()
            .add_message::<ExtClipboard>()
            .add_message::<ExtItemMove>()
            .add_message::<ExtItemLock>()
            .add_message::<ExtLootSend>()
            .init_resource::<ExtLoot>()
            .configure_sets(
                Update,
                ExtCastSet
                    .before(apply_ext_casts)
                    .before(apply_ext_raid_marks)
                    .before(apply_ext_cancel_auras)
                    .before(apply_ext_selects)
                    .before(apply_ext_attacks)
                    .before(apply_ext_queries)
                    .before(apply_ext_loot)
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
                    apply_ext_attacks.after(apply_ext_selects),
                    apply_ext_queries,
                    apply_ext_loot,
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
    targeting: Res<'w, crate::spell::targeting::SpellTargeting>,
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

    /// Our last committed, unresolved cast of any class, ranged shots included (`0xceca88`).
    pub fn cast_committed(&self, now: Instant) -> Option<u32> {
        self.pending.committed(now)
    }

    /// The spell whose targeting cursor is up (`GetTargetingSpellId 0x6e48e0`), if one is.
    pub fn targeting_spell(&self) -> Option<u32> {
        self.targeting.spell()
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

    /// The nearest-unit scan's per-candidate filter (`0x493e40`): `hostile`, mode 1, alive and
    /// attackable; else mode 2, assistable and not dead. A crate's own target cycles use it.
    pub fn tab_valid(&self, guid: u64, hostile: bool) -> bool {
        let store = self.store(guid);
        let me = self.me.single().ok();
        if hostile {
            !store.is_some_and(|s| s.0.unit_reads_dead())
                && crate::target::can_attack(store, self.factions.as_deref(), &self.reputations, me)
        } else {
            crate::target::can_assist(
                store,
                self.factions.as_deref(),
                &self.reputations,
                me,
                |owner| self.store(owner).cloned(),
            ) && !store.is_some_and(|s| s.0.unit_is_dead())
        }
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

    /// A creature's `CreatureFamily.dbc` id, once its template has been queried; 0 for no family
    /// (anything but a tameable beast or a warlock minion), `None` for a player or an unknown
    /// template.
    pub fn creature_family(&self, guid: u64) -> Option<u32> {
        let entry = self.store(guid)?.0.object_entry()?;
        Some(self.names.creature_record(entry)?.pet_family)
    }

    /// A held unit's cached name, with no query on a miss.
    pub fn unit_name(&self, guid: u64) -> Option<&str> {
        self.names.peek_unit(guid, self.store(guid))
    }

    /// Every answered player name, `(guid, name, (race, class, gender))`, the traits present when
    /// the name query answered them; with [`Self::names_generation`], which moves on each answer.
    pub fn player_names(&self) -> impl Iterator<Item = (u64, &str, Option<(u8, u8, u8)>)> {
        self.names
            .players()
            .map(|(g, n)| (g, n, self.names.player_traits(g)))
    }

    pub fn names_generation(&self) -> u64 {
        self.names.generation()
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

/// Each unit's range inputs as `GetMinMaxRange` (`0x6e3480`) reads them: its combat reach, player
/// bit and motion, the caster's and its auto-attack target's.
#[derive(SystemParam)]
pub struct ExtRange<'w, 's> {
    units: crate::spell::RangeUnits<'w, 's>,
    index: Res<'w, crate::net::GuidIndex>,
}

impl ExtRange<'_, '_> {
    /// A streamed unit's inputs; `None` for no unit or one not streamed.
    pub fn unit(&self, guid: u64) -> Option<benilla_formats::RangeUnit> {
        self.units.unit(*self.index.0.get(&guid)?)
    }

    /// Our own player, standing at the default reach until its descriptor streams.
    pub fn caster(&self) -> benilla_formats::RangeUnit {
        self.units.caster()
    }

    /// Our auto-attack target's inputs, which the melee arm falls back to.
    pub fn caster_attack_target(&self) -> Option<benilla_formats::RangeUnit> {
        self.units.caster_attack_target()
    }
}

/// The usability walk (`0x6e3d60`) the action bar runs per slot, for any spell: the reference's
/// `FUN_SPELL_IS_USABLE`, without the cooldown, which greys apart.
#[derive(SystemParam)]
pub struct ExtUsable<'w, 's> {
    spells: Option<Res<'w, crate::ui_action::Spells>>,
    me: Query<'w, 's, &'static ObjectStore, With<crate::net::SelfPlayer>>,
    stores: Query<'w, 's, &'static ObjectStore>,
    index: Res<'w, crate::net::GuidIndex>,
    selection: Res<'w, crate::target::Selection>,
    objects: crate::net::Objects<'w, 's>,
    factions: Option<Res<'w, crate::target::Factions>>,
    reputations: Res<'w, crate::net::Reputations>,
    cooldowns: Res<'w, crate::spell::Cooldowns>,
    spell_mods: Res<'w, crate::spell::SpellModifiers>,
    items: Res<'w, crate::items::Items>,
    commands: Res<'w, crate::net::NetCommands>,
}

impl ExtUsable<'_, '_> {
    /// `(usable, notEnoughMana)` for each of `spell_ids` known to `Spell.dbc`, as the bar's
    /// `IsUsableAction` would answer for a slot holding it; empty out of the world.
    pub fn verdicts(&self, spell_ids: impl IntoIterator<Item = u32>) -> Vec<(u32, bool, bool)> {
        let (Some(spells), Ok(store)) = (self.spells.as_deref(), self.me.single()) else {
            return Vec::new();
        };
        let target = self
            .selection
            .guid
            .and_then(|g| self.index.0.get(&g))
            .and_then(|&e| self.stores.get(e).ok());
        let carried = crate::ui_items::carried_counts(&store.0, &self.objects);
        let ctx = crate::spell::usable::UsableCtx {
            store,
            target_store: target,
            factions: self.factions.as_deref(),
            reputations: &self.reputations,
            cooldowns: &self.cooldowns,
            spell_mods: &self.spell_mods,
            carried: &carried,
        };
        spell_ids
            .into_iter()
            .filter_map(|id| {
                let d = spells.catalog.get(id)?;
                let (usable, oom) = crate::spell::usable::spell_usable(
                    id,
                    d,
                    spells,
                    &ctx,
                    &self.objects,
                    &self.items,
                    &self.commands,
                );
                Some((id, usable, oom))
            })
            .collect()
    }
}

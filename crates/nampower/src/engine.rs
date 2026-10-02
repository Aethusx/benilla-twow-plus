//! Nampower's cast state machine, ported from `main.cpp`, `spellcast.cpp`, `spellevents.cpp` and
//! `spellchannel.cpp`: the client-side cast and GCD timers with their dynamic buffer, the queue
//! windows, the three queues (one GCD cast, six non-GCD casts, one on-swing strike), cooldown
//! queuing, retries of server-rejected casts, and the queued script.
//!
//! The engine holds no Bevy state and reads no clock: every input carries its time in ms on one
//! monotonic scale, and what the engine wants done comes out as [`Action`]s.

use benilla_app::ext::{GateVerdict, ItemUse, ScriptValue};

use crate::queue::{CastParams, CastQueue, CastResult, CastType, QueueEvent};
use crate::settings::Settings;

/// Queued casts older than this since the last cast start are dropped instead of cast.
const MAX_TIME_SINCE_LAST_CAST_FOR_QUEUE: u64 = 10_000;
/// The step the dynamic buffer moves by.
const DYNAMIC_BUFFER_INCREMENT: u64 = 5;
/// The least time between two buffer increases.
const BUFFER_INCREASE_FREQUENCY: u64 = 5_000;
/// The least time between two buffer decreases.
const BUFFER_DECREASE_FREQUENCY: u64 = 10_000;

/// `SpellCastResult` codes the retry logic reads.
pub const SPELL_FAILED_CANT_DO_THAT_YET: u8 = 0x12;
pub const SPELL_FAILED_DONT_REPORT: u8 = 0x17;
pub const SPELL_FAILED_ITEM_NOT_READY: u8 = 0x28;
pub const SPELL_FAILED_NOT_READY: u8 = 0x3c;
pub const SPELL_FAILED_SPELL_IN_PROGRESS: u8 = 0x61;

/// Disenchant, whose failure ends a `DisenchantAll` run.
pub const DISENCHANT: u32 = 13262;
/// Turtle WoW's Arcane Surge ranks: the server sends `CANT_DO_THAT_YET` after each success.
const ARCANE_SURGE: [u32; 4] = [51933, 51934, 51935, 51936];
/// Channels that never block the queue: mind-control-style far-sight channels.
const FARSIGHT_CHANNELS: [u32; 3] = [19832, 23014, 13180];

/// The spell facts the engine reads, taken from `Spell.dbc`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpellFacts {
    pub id: u32,
    pub on_gcd: bool,
    pub channeled: bool,
    pub targeting: bool,
    pub on_swing: bool,
    pub special: bool,
    pub disabled_while_active: bool,
    pub auto_repeat: bool,
    pub mounting: bool,
    pub summon_guardian: bool,
    pub far_sight: bool,
    pub gcd_category: u32,
    /// The base channel duration, `SpellDuration.dbc` through `DurationIndex`.
    pub base_duration_ms: u64,
    /// The first nonzero `EffectAmplitude`, the channel's base tick.
    pub amplitudes: [u64; 3],
}

/// One press as the gate saw it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Attempt {
    pub spell: SpellFacts,
    pub item: Option<ItemUse>,
    /// The unit the cast goes to when explicit; `None` is the selection.
    pub target: Option<u64>,
    /// The selection when the press landed, `SPELL_CAST_EVENT`'s default guid.
    pub selection: Option<u64>,
    pub cast_time_ms: u64,
    pub gcd_ms: u64,
    pub now: u64,
    /// Sent by the engine itself ([`Action::Cast`]); see [`Origin`].
    pub origin: Origin,
}

/// Where an attempt came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    /// A press through the stock verbs.
    #[default]
    Press,
    /// A queued cast popping, with its retry count.
    Queue { retries: u32 },
    /// A Lua cast with queue overrides (`QueueSpellByName`, `CastSpellByNameNoQueue`).
    Direct { force_queue: bool, no_queue: bool },
}

/// What the ladder did with a passed attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    Refused(u8),
    Pending,
}

/// The player facts the mount guard reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerFacts {
    pub buff_capped: bool,
    pub mounted: bool,
}

/// What the engine wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Cast {
        spell_id: u32,
        target: Option<u64>,
        item: Option<ItemUse>,
        origin: Origin,
    },
    /// Run a queued `QueueScript` chunk.
    Script(String),
    /// Fire an event to the interface.
    Event(&'static str, Vec<ScriptValue>),
    /// The red error line.
    UiError(String),
    /// A `DisenchantAll` run ends.
    DisenchantStop,
}

/// `LastCastData`.
#[derive(Clone, Copy, Debug, Default)]
struct LastCast {
    attempt_ms: u64,
    attempt_spell: u32,
    cast_time_ms: u64,
    start_ms: u64,
    channel_start_ms: u64,
    on_swing_start_ms: u64,
    was_item: bool,
    was_on_gcd: bool,
}

/// `CastData`.
#[derive(Clone, Copy, Debug, Default)]
pub struct CastData {
    pub delay_end_ms: u64,
    pub cast_end_ms: u64,
    pub gcd_end_ms: u64,
    pub buffer_ms: u64,
    pub cast_spell_id: u32,
    pub on_swing_queued: bool,
    pub pending_on_swing_cast: bool,
    pub on_swing_spell_id: u32,
    pub cooldown_normal_end_ms: u64,
    pub cooldown_non_gcd_end_ms: u64,
    pub cooldown_normal_queued: bool,
    pub cooldown_non_gcd_queued: bool,
    pub normal_queued: bool,
    pub non_gcd_queued: bool,
    pub targeting_queued: bool,
    pub targeting_spell_id: u32,
    pub casting_queued_spell: bool,
    pub num_retries: u32,
    pub channeling: bool,
    pub cancel_channel_next_tick: bool,
    pub channel_start_ms: u64,
    pub channel_end_ms: u64,
    pub channel_tick_ms: u64,
    pub channel_spell_id: u32,
    pub channel_duration_ms: u64,
}

/// The cast state machine.
pub struct Engine {
    pub settings: Settings,
    pub cast: CastData,
    last: LastCast,
    /// The dynamic buffer, `gBufferTimeMs`.
    pub buffer_ms: u64,
    last_buffer_increase_ms: u64,
    last_buffer_decrease_ms: u64,
    last_server_spell_delay_ms: u64,
    last_cast_used_server_delay: bool,
    /// The running latency average, `gRunningAverageLatencyMs`.
    pub latency_ms: u64,
    next_cast_id: u64,
    last_normal: CastParams,
    last_on_swing: CastParams,
    pub non_gcd: CastQueue,
    pub history: CastQueue,
    /// The one `QueueScript` slot and its priority (1 to 3).
    script: Option<(String, u8)>,
    /// The attempt the ladder is running, for its outcome.
    current: Option<(Attempt, CastType)>,
    /// The ground cast whose cursor is up, for its commit.
    targeting_attempt: Option<Attempt>,
    pub out: Vec<Action>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(Settings::nampower())
    }
}

/// Nampower's guid string: `0x` and sixteen upper-case hex digits.
pub fn guid_string(guid: u64) -> String {
    format!("0x{guid:016X}")
}

fn s(v: impl Into<String>) -> ScriptValue {
    ScriptValue::Str(v.into())
}

fn n(v: impl Into<i64>) -> ScriptValue {
    ScriptValue::Int(v.into())
}

impl Engine {
    pub fn new(settings: Settings) -> Self {
        let buffer_ms = settings.min_buffer_time_ms;
        Self {
            settings,
            cast: CastData::default(),
            last: LastCast::default(),
            buffer_ms,
            last_buffer_increase_ms: 0,
            last_buffer_decrease_ms: 0,
            last_server_spell_delay_ms: 0,
            last_cast_used_server_delay: false,
            latency_ms: 0,
            next_cast_id: 1,
            last_normal: CastParams::default(),
            last_on_swing: CastParams::default(),
            non_gcd: CastQueue::new(6),
            history: CastQueue::new(30),
            script: None,
            current: None,
            targeting_attempt: None,
            out: Vec::new(),
        }
    }

    /// Apply one `NP_` CVar; the minimum buffer also resets the running one.
    pub fn apply_setting(&mut self, name: &str, value: &str) {
        if self.settings.apply(name, value) && name.eq_ignore_ascii_case("NP_MinBufferTimeMs") {
            self.buffer_ms = self.settings.min_buffer_time_ms;
        }
    }

    /// Fold one `GetNetStats` latency sample into the running average, ignoring spikes.
    pub fn sample_latency(&mut self, latency_ms: u64) {
        if latency_ms == 0 {
            return;
        }
        if self.latency_ms == 0 {
            self.latency_ms = latency_ms;
        } else if latency_ms < self.latency_ms * 2 {
            self.latency_ms = (self.latency_ms * 9 + latency_ms) / 10;
        }
    }

    fn queue_event(&mut self, event: QueueEvent, spell_id: u32) {
        self.out.push(Action::Event(
            "SPELL_QUEUE_EVENT",
            vec![n(event as i64), n(spell_id)],
        ));
    }

    fn cast_event(&mut self, sent: bool, a: &Attempt, cast_type: CastType) {
        let guid = a.target.or(a.selection).unwrap_or(0);
        self.out.push(Action::Event(
            "SPELL_CAST_EVENT",
            vec![
                n(i64::from(sent)),
                n(a.spell.id),
                n(cast_type as i64),
                s(guid_string(guid)),
                n(a.item.map_or(0, |i| i.entry)),
            ],
        ));
    }

    /// `GetServerDelayMs`: the measured server delay of the last cast when packet timing is on
    /// and it looks sane, else the dynamic buffer.
    fn server_delay_ms(&mut self) -> u64 {
        if self.settings.optimize_buffer_using_packet_timings
            && !self.last_cast_used_server_delay
            && (1..200).contains(&self.last_server_spell_delay_ms)
        {
            self.last_cast_used_server_delay = true;
            return self.last_server_spell_delay_ms;
        }
        self.last_cast_used_server_delay = false;
        self.buffer_ms
    }

    /// `EffectiveCastEndMs`: the channel's end while it holds the queue, else the cast or the
    /// non-GCD delay, whichever is later.
    pub fn effective_cast_end(&self, now: u64) -> u64 {
        if self.cast.channeling && self.settings.queue_channeling_spells {
            if !self.settings.interrupt_channels_outside_queue_window {
                return self.cast.channel_end_ms;
            }
            let remaining = self.cast.channel_end_ms.saturating_sub(now);
            if remaining < self.settings.channel_queue_window_ms {
                return self.cast.channel_end_ms;
            }
        }
        self.cast.cast_end_ms.max(self.cast.delay_end_ms)
    }

    /// `InSpellQueueWindow`.
    fn in_queue_window(
        &self,
        remaining_cast: u64,
        remaining_gcd: u64,
        targeting: bool,
        force: bool,
        now: u64,
    ) -> bool {
        let window = if self.cast.channeling {
            if self.settings.queue_channeling_spells {
                if self.cast.cancel_channel_next_tick {
                    return true;
                }
                let remaining = self.cast.channel_end_ms.saturating_sub(now);
                return remaining < self.settings.channel_queue_window_ms;
            }
            0
        } else if targeting {
            self.settings.targeting_queue_window_ms
        } else {
            self.settings.spell_queue_window_ms
        };
        if remaining_cast > 0 {
            return remaining_cast < window || force;
        }
        if remaining_gcd > 0 {
            return remaining_gcd < window || force;
        }
        false
    }

    pub fn reset_channeling(&mut self) {
        let c = &mut self.cast;
        c.cancel_channel_next_tick = false;
        c.channeling = false;
        c.channel_start_ms = 0;
        c.channel_end_ms = 0;
        c.channel_spell_id = 0;
        c.channel_tick_ms = 0;
    }

    /// `ResetCastFlags`: the delay stays.
    fn reset_cast_flags(&mut self) {
        self.cast.cast_end_ms = 0;
        self.cast.cast_spell_id = 0;
        self.cast.gcd_end_ms = 0;
        self.reset_channeling();
    }

    fn reset_on_swing_flags(&mut self) {
        self.cast.on_swing_queued = false;
        self.cast.on_swing_spell_id = 0;
    }

    /// `ClearQueuedSpells`.
    pub fn clear_queued(&mut self) {
        if self.cast.normal_queued || self.cast.cooldown_normal_queued {
            self.queue_event(QueueEvent::NormalQueuePopped, self.last_normal.spell_id);
            self.cast.normal_queued = false;
            self.cast.cooldown_normal_queued = false;
        }
        if self.cast.non_gcd_queued || self.cast.cooldown_non_gcd_queued {
            while let Some(p) = self.non_gcd.pop() {
                self.queue_event(QueueEvent::NonGcdQueuePopped, p.spell_id);
            }
            self.cast.non_gcd_queued = false;
            self.cast.cooldown_non_gcd_queued = false;
        }
        self.cast.targeting_queued = false;
        self.cast.targeting_spell_id = 0;
    }

    /// `QueueScript`: inside the queue window, hold the one slot and return true; outside it,
    /// return false and leave the chunk for the caller to run now.
    pub fn queue_script(&mut self, chunk: String, priority: u8, now: u64) -> bool {
        let end = self.effective_cast_end(now);
        let remaining_cast = end.saturating_sub(now);
        let remaining_gcd = self.cast.gcd_end_ms.saturating_sub(now);
        if !self.in_queue_window(remaining_cast, remaining_gcd, false, false, now) {
            return false;
        }
        self.script = Some((chunk, priority.clamp(1, 3)));
        true
    }

    /// `ChannelStopCastingNextTick`.
    pub fn stop_channel_next_tick(&mut self) {
        if self.cast.channeling {
            self.cast.cancel_channel_next_tick = true;
        }
    }

    fn run_queued_script(&mut self, priority: u8, now: u64) -> bool {
        if !matches!(&self.script, Some((_, p)) if *p == priority) {
            return false;
        }
        let delay = self.effective_cast_end(now).max(self.cast.gcd_end_ms);
        if delay > now {
            return false;
        }
        if let Some((chunk, _)) = self.script.take() {
            self.out.push(Action::Script(chunk));
        }
        true
    }

    /// The per-frame tick, `ISceneEndHook`: end a channel early when due, then process the
    /// queues, at most one cast or script per call.
    pub fn tick(&mut self, now: u64) {
        if self.cast.channeling {
            self.check_stop_channeling(now);
        }
        self.process_queues(now);
    }

    /// `checkForStopChanneling`: with a cast queued, take the channel as over a latency share
    /// before its end, or at the next tick after `ChannelStopCastingNextTick`.
    fn check_stop_channeling(&mut self, now: u64) {
        let queued = self.cast.non_gcd_queued || self.cast.normal_queued;
        if !self.settings.queue_channeling_spells || !queued {
            return;
        }
        let mut remaining = self.cast.channel_end_ms.saturating_sub(now);
        let pct = self.settings.channel_latency_reduction_percentage;
        let reduction = if self.latency_ms > 0 && pct != 0 {
            (self.latency_ms as i64 * pct / 100).max(0) as u64
        } else {
            0
        };
        remaining = remaining.saturating_sub(reduction);
        if remaining == 0 {
            self.reset_channeling();
            return;
        }
        let c = self.cast;
        if c.cancel_channel_next_tick
            && c.channel_start_ms > 0
            && now.saturating_sub(c.channel_start_ms) < 60_000
            && c.channel_tick_ms > 0
        {
            let mut next_tick = c.channel_start_ms;
            while next_tick + self.buffer_ms < now {
                next_tick += c.channel_tick_ms;
            }
            let mut left = next_tick.saturating_sub(now);
            if left > 0 {
                left = if reduction > 0 {
                    left.saturating_sub(reduction)
                } else {
                    left.saturating_sub(self.buffer_ms)
                };
            }
            if left == 0 {
                self.reset_channeling();
            }
        }
    }

    /// `processQueues`.
    fn process_queues(&mut self, now: u64) {
        if self.cast.channeling {
            return;
        }
        if self.run_queued_script(1, now) {
            return;
        }
        if self.cast.non_gcd_queued && self.effective_cast_end(now) <= now {
            self.cast_queued_non_gcd();
            return;
        }
        if self.cast.cooldown_non_gcd_queued && self.cast.cooldown_non_gcd_end_ms <= now {
            self.cast.cooldown_non_gcd_queued = false;
            self.cast.non_gcd_queued = true;
            self.cast_queued_non_gcd();
            return;
        }
        if self.run_queued_script(2, now) {
            return;
        }
        if self.cast.normal_queued {
            let delay = self.effective_cast_end(now).max(self.cast.gcd_end_ms);
            if delay <= now {
                if now.saturating_sub(self.last.start_ms) < MAX_TIME_SINCE_LAST_CAST_FOR_QUEUE {
                    self.cast_queued_normal();
                    return;
                }
                self.queue_event(QueueEvent::NormalQueuePopped, self.last_normal.spell_id);
                self.cast.normal_queued = false;
                self.cast.targeting_queued = false;
                self.cast.targeting_spell_id = 0;
            }
        }
        if self.cast.cooldown_normal_queued && self.cast.cooldown_normal_end_ms <= now {
            self.cast.cooldown_normal_queued = false;
            self.cast.normal_queued = true;
            self.cast_queued_normal();
            return;
        }
        self.run_queued_script(3, now);
    }

    fn reissue(&mut self, p: CastParams) {
        self.out.push(Action::Cast {
            spell_id: p.spell_id,
            target: p.target,
            item: p.item,
            origin: Origin::Queue { retries: p.retries },
        });
    }

    /// `CastQueuedNonGcdSpell`.
    fn cast_queued_non_gcd(&mut self) {
        if !self.cast.non_gcd_queued {
            return;
        }
        let p = self.non_gcd.pop().unwrap_or_default();
        if p.spell_id > 0 {
            self.reissue(p);
        }
        self.queue_event(QueueEvent::NonGcdQueuePopped, p.spell_id);
        self.cast.non_gcd_queued = !self.non_gcd.is_empty();
        self.cast.targeting_queued = false;
        self.cast.targeting_spell_id = 0;
    }

    /// `CastQueuedNormalSpell`.
    fn cast_queued_normal(&mut self) {
        if !self.cast.normal_queued {
            return;
        }
        let p = self.last_normal;
        if p.spell_id > 0 {
            self.reissue(p);
        }
        self.queue_event(QueueEvent::NormalQueuePopped, p.spell_id);
        self.cast.normal_queued = false;
        self.cast.targeting_queued = false;
        self.cast.targeting_spell_id = 0;
    }

    fn params(&self, a: &Attempt, cast_type: CastType, retries: u32) -> CastParams {
        CastParams {
            cast_id: 0,
            spell_id: a.spell.id,
            item: a.item,
            target: a.target,
            gcd_category: a.spell.gcd_category,
            cast_time_ms: a.cast_time_ms,
            start_ms: a.now,
            cast_type,
            retries,
            result: CastResult::WaitingForCast,
        }
    }

    fn record(&mut self, mut p: CastParams, history_target: Option<u64>) {
        p.cast_id = self.next_cast_id;
        p.target = history_target;
        self.next_cast_id += 1;
        self.history.push_front(p);
    }

    /// Queue a non-GCD cast behind the running one, or retarget its queued copy.
    fn queue_non_gcd(&mut self, a: &Attempt, targeting: bool) {
        if let Some(p) = self.non_gcd.find_spell(a.spell.id) {
            p.target = a.target;
            return;
        }
        let mut p = self.params(a, CastType::NonGcd, 0);
        p.start_ms = 0;
        let replace = self.settings.replace_matching_non_gcd_category;
        self.non_gcd.push(p, replace);
        self.queue_event(QueueEvent::NonGcdQueued, a.spell.id);
        self.cast.non_gcd_queued = true;
        if targeting {
            self.cast.targeting_queued = true;
            self.cast.targeting_spell_id = a.spell.id;
        }
    }

    fn queue_normal(&mut self, a: &Attempt, targeting: bool) {
        self.queue_event(QueueEvent::NormalQueued, a.spell.id);
        self.cast.normal_queued = true;
        if targeting {
            self.cast.targeting_queued = true;
            self.cast.targeting_spell_id = a.spell.id;
        }
    }

    /// `Spell_C_CastSpellHook` up to the original call: the mount guard, the double-press
    /// channel end, the on-swing slot, the queue windows, the running-cast refusal and spam
    /// protection.
    pub fn attempt(&mut self, a: &Attempt, player: PlayerFacts) -> GateVerdict {
        let spell = a.spell;
        let now = a.now;
        let (force_queue, no_queue, retries) = match a.origin {
            Origin::Press => (false, false, 0),
            Origin::Queue { retries } => (false, false, retries),
            Origin::Direct {
                force_queue,
                no_queue,
            } => (force_queue, no_queue, 0),
        };
        self.cast.num_retries = retries;
        self.cast.casting_queued_spell = matches!(a.origin, Origin::Queue { .. });
        if self.settings.prevent_mounting_when_buff_capped
            && spell.mounting
            && player.buff_capped
            && !player.mounted
        {
            self.out.push(Action::UiError(
                "Preventing mounting due to buff cap (breaks dismount)".into(),
            ));
            return GateVerdict::Stop;
        }
        // A second press of the channeled spell within 350 ms, 500 ms into the channel.
        if self.cast.channeling
            && !self.cast.cancel_channel_next_tick
            && retries == 0
            && self.settings.double_cast_to_end_channel_early
            && self.cast.channel_start_ms > 0
            && now.saturating_sub(self.cast.channel_start_ms) > 500
            && self.last.attempt_spell == spell.id
            && now.saturating_sub(self.last.attempt_ms) < 350
        {
            self.cast.cancel_channel_next_tick = true;
        }
        self.last.attempt_ms = now;
        self.last.attempt_spell = spell.id;
        // A new cast ends a cooldown-queued one of its kind.
        if spell.on_gcd && self.cast.cooldown_normal_queued {
            self.cast.cooldown_normal_queued = false;
            self.queue_event(QueueEvent::NormalQueuePopped, self.last_normal.spell_id);
        } else if self.cast.cooldown_non_gcd_queued {
            self.cast.cooldown_non_gcd_queued = false;
            let p = self.non_gcd.pop().unwrap_or_default();
            self.queue_event(QueueEvent::NonGcdQueuePopped, p.spell_id);
        }
        let history_target = a.target.or(a.selection);
        // On-swing strikes ride the swing, apart from the cast bar and the GCD.
        if spell.on_swing {
            self.last_on_swing = self.params(a, CastType::OnSwing, 0);
            let p = self.params(a, CastType::OnSwing, retries);
            self.record(p, history_target);
            self.current = Some((*a, CastType::OnSwing));
            return GateVerdict::Pass;
        }
        let end = self.effective_cast_end(now);
        let remaining_cast = end.saturating_sub(now);
        let remaining_gcd = self.cast.gcd_end_ms.saturating_sub(now);
        let in_window = !spell.special
            && self.in_queue_window(
                remaining_cast,
                remaining_gcd,
                spell.targeting,
                force_queue,
                now,
            );
        if spell.on_gcd {
            let cast_type = if spell.targeting {
                CastType::Targeting
            } else if spell.channeled {
                CastType::Channel
            } else {
                CastType::Normal
            };
            self.last_normal = self.params(a, cast_type, 0);
        }
        let s = &self.settings;
        if !no_queue && (!spell.channeled || s.queue_channeling_spells) && in_window {
            if spell.targeting {
                if s.queue_targeting_spells {
                    if a.cast_time_ms > 0 {
                        if s.queue_cast_time_spells {
                            self.queue_normal(a, true);
                            return GateVerdict::Stop;
                        }
                    } else if s.queue_instant_spells {
                        if spell.on_gcd {
                            self.queue_normal(a, true);
                            return GateVerdict::Stop;
                        } else if remaining_cast > 0 {
                            self.queue_non_gcd(a, true);
                            return GateVerdict::Stop;
                        }
                    }
                }
            } else if a.cast_time_ms > 0 {
                if s.queue_cast_time_spells {
                    if spell.on_gcd {
                        self.queue_normal(a, false);
                        return GateVerdict::Stop;
                    } else if remaining_cast > 0 {
                        self.queue_non_gcd(a, false);
                        return GateVerdict::Stop;
                    }
                }
            } else if (spell.channeled && s.queue_channeling_spells)
                || (!spell.channeled && s.queue_instant_spells)
            {
                if spell.on_gcd {
                    self.queue_normal(a, false);
                    return GateVerdict::Stop;
                } else if remaining_cast > 0 {
                    self.queue_non_gcd(a, false);
                    return GateVerdict::Stop;
                }
            }
        }
        if !spell.special {
            if remaining_cast > 0 {
                return GateVerdict::Stop;
            }
            self.cast.cast_end_ms = 0;
            self.cast.cast_spell_id = 0;
            if spell.on_gcd && remaining_gcd > 0 {
                return GateVerdict::Stop;
            }
            self.cast.gcd_end_ms = 0;
        }
        // Spam protection: an instant (or disabled-while-active) spell still waiting for its
        // server answer, or just accepted at the same target, is not sent again within 500 ms.
        if (a.cast_time_ms == 0 || spell.disabled_while_active)
            && self.settings.spam_protection_enabled
        {
            if let Some(p) = self.history.newest_waiting(spell.id) {
                if now.saturating_sub(p.start_ms) < 500 {
                    return GateVerdict::Stop;
                }
            } else if let Some(p) = self.history.newest_success(spell.id) {
                if p.target == history_target && now.saturating_sub(p.start_ms) < 500 {
                    return GateVerdict::Stop;
                }
            }
        }
        let cast_type = if spell.channeled {
            CastType::Channel
        } else if spell.targeting {
            if spell.on_gcd {
                CastType::Targeting
            } else {
                CastType::TargetingNonGcd
            }
        } else if !spell.on_gcd {
            CastType::NonGcd
        } else {
            CastType::Normal
        };
        let p = self.params(a, cast_type, retries);
        self.record(p, history_target);
        self.current = Some((*a, cast_type));
        // The client's own cast guard is cleared, as `clearCastingSpell` does, unless the
        // spell is a tradeskill or an on-swing strike is pending.
        if spell.special || self.cast.pending_on_swing_cast {
            GateVerdict::Pass
        } else {
            GateVerdict::PassOverInFlight
        }
    }

    /// The rest of `Spell_C_CastSpellHook`: the begin-cast timers on a send, the local failure
    /// path on a refusal, `SPELL_CAST_EVENT` either way.
    pub fn outcome(&mut self, outcome: Outcome, cooldown_remaining_ms: u64) {
        let Some((a, cast_type)) = self.current.take() else {
            return;
        };
        let sent = outcome == Outcome::Sent;
        if cast_type == CastType::OnSwing {
            if sent {
                self.cast.pending_on_swing_cast = true;
                self.cast.on_swing_spell_id = a.spell.id;
            }
            self.cast_event(sent, &a, cast_type);
            let no_queue = matches!(a.origin, Origin::Direct { no_queue: true, .. });
            if !sent && self.settings.queue_on_swing_spells && !no_queue {
                if a.now.saturating_sub(self.last.on_swing_start_ms)
                    > self.settings.on_swing_buffer_cooldown_ms
                {
                    self.queue_event(QueueEvent::OnSwingQueued, a.spell.id);
                    self.cast.on_swing_queued = true;
                }
            } else {
                self.last.on_swing_start_ms = a.now;
            }
            return;
        }
        match outcome {
            Outcome::Sent => self.begin_cast(&a),
            Outcome::Refused(reason) => {
                self.spell_failed(a.spell.id, reason, false, a.now, cooldown_remaining_ms, 0)
            }
            // The ground cursor is up: its click commits ([`Self::targeted_sent`]).
            Outcome::Pending if a.spell.targeting => self.targeting_attempt = Some(a),
            Outcome::Pending => {}
        }
        self.cast_event(sent, &a, cast_type);
    }

    /// The ground cursor's click sent the cast whose cursor [`Self::outcome`] parked.
    pub fn targeted_sent(&mut self, spell_id: u32, now: u64) {
        if let Some(mut a) = self.targeting_attempt.take() {
            if a.spell.id == spell_id {
                a.now = now;
                if let Some(h) = self.history.find_spell(spell_id) {
                    h.start_ms = now;
                }
                self.begin_cast(&a);
            }
        }
    }

    /// `BeginCast`, at the send: the cast end and the GCD end with the buffer, the non-GCD
    /// delay, and the buffer's slow decay.
    fn begin_cast(&mut self, a: &Attempt) {
        let now = a.now;
        let spell = a.spell;
        let cast_time = a.cast_time_ms;
        self.last.cast_time_ms = cast_time;
        self.cast.channeling = spell.channeled;
        if spell.channeled {
            self.cast.channel_duration_ms = spell.base_duration_ms;
            self.cast.channel_end_ms = now + spell.base_duration_ms;
        }
        self.last.was_on_gcd = spell.on_gcd;
        if let Some(h) = self.history.peek_mut() {
            self.last.was_item = h.item.is_some();
            h.result = CastResult::WaitingForServer;
        }
        let mut buffer = self.server_delay_ms();
        self.last_server_spell_delay_ms = 0;
        if spell.on_gcd {
            // An item's spell on the GCD reports its item cooldown; never past 1.5 s.
            let gcd = if spell.id == crate::dbc::consts::POWER_OVERWHELMING {
                0
            } else {
                a.gcd_ms.min(1500)
            };
            if cast_time + 50 < gcd {
                buffer = 0;
            } else if cast_time < gcd {
                buffer = buffer.saturating_sub(gcd - cast_time);
            }
            // One ms past the GCD: this clock floors to whole ms, and benilla's own GCD, armed
            // at the same send, must have run out when a queued cast pops.
            self.cast.gcd_end_ms = now + gcd + buffer + u64::from(gcd > 0);
        } else {
            self.cast.delay_end_ms = now + self.settings.non_gcd_buffer_time_ms;
        }
        self.cast.cast_end_ms = if cast_time > 0 {
            now + cast_time + buffer
        } else {
            0
        };
        self.cast.cast_spell_id = if cast_time > 0 { spell.id } else { 0 };
        self.cast.buffer_ms = buffer;
        if now.saturating_sub(self.last_buffer_decrease_ms) > BUFFER_DECREASE_FREQUENCY {
            if self.buffer_ms > self.settings.min_buffer_time_ms {
                self.buffer_ms -= DYNAMIC_BUFFER_INCREMENT;
            }
            self.last_buffer_decrease_ms = now;
        }
        self.last.start_ms = now;
    }

    /// `Spell_C_SpellFailedHook`: `SPELL_FAILED_SELF`, the cast state reset, and for not-ready
    /// and in-progress failures a cooldown queue or a retry with a longer buffer.
    pub fn spell_failed(
        &mut self,
        spell_id: u32,
        reason: u8,
        by_server: bool,
        now: u64,
        cooldown_remaining_ms: u64,
        item_cooldown_remaining_ms: u64,
    ) {
        if reason == SPELL_FAILED_DONT_REPORT || spell_id == crate::dbc::consts::AUTO_SHOT {
            return;
        }
        if spell_id == DISENCHANT {
            self.out.push(Action::DisenchantStop);
        }
        if reason == SPELL_FAILED_CANT_DO_THAT_YET && ARCANE_SURGE.contains(&spell_id) {
            return;
        }
        self.out.push(Action::Event(
            "SPELL_FAILED_SELF",
            vec![n(spell_id), n(reason), n(i64::from(by_server))],
        ));
        self.reset_cast_flags();
        if spell_id == self.cast.on_swing_spell_id {
            self.reset_on_swing_flags();
        }
        if !matches!(
            reason,
            SPELL_FAILED_NOT_READY | SPELL_FAILED_ITEM_NOT_READY | SPELL_FAILED_SPELL_IN_PROGRESS
        ) {
            return;
        }
        if reason == SPELL_FAILED_NOT_READY && cooldown_remaining_ms > 0 {
            let window = self.settings.cooldown_queue_window_ms;
            let queue = self.settings.queue_spells_on_cooldown;
            let Some(p) = self.history.find_spell(spell_id) else {
                return;
            };
            p.result = CastResult::ServerFailure;
            let p = *p;
            if queue && cooldown_remaining_ms < window {
                if p.cast_type.non_gcd() {
                    self.cast.cooldown_non_gcd_queued = true;
                    self.cast.cooldown_non_gcd_end_ms = now + cooldown_remaining_ms;
                    self.queue_event(QueueEvent::NonGcdQueued, spell_id);
                    let replace = self.settings.replace_matching_non_gcd_category;
                    self.non_gcd.push(p, replace);
                } else {
                    self.cast.cooldown_normal_queued = true;
                    self.cast.cooldown_normal_end_ms = now + cooldown_remaining_ms;
                    self.queue_event(QueueEvent::NormalQueued, spell_id);
                    self.last_normal = p;
                }
            }
            return;
        }
        if reason == SPELL_FAILED_ITEM_NOT_READY && item_cooldown_remaining_ms > 0 {
            if let Some(p) = self.history.find_spell(spell_id) {
                if p.item.is_some() {
                    p.result = CastResult::ServerFailure;
                    return;
                }
            }
        }
        if !self.settings.retry_server_rejected_spells {
            return;
        }
        let replace = self.settings.replace_matching_non_gcd_category;
        let gcd_active = self.cast.gcd_end_ms > now;
        let buffer = self.buffer_ms;
        let recent_success = self
            .history
            .newest_success(spell_id)
            .is_some_and(|p| p.start_ms + 1000 > now);
        let Some(p) = self.history.find_spell(spell_id) else {
            return;
        };
        let mut retry = None;
        if p.start_ms + 500 > now && p.retries < 3 {
            p.retries += 1;
            retry = Some(*p);
        }
        if let Some(p) = retry {
            if p.cast_type.non_gcd() {
                // A recent success means the failure was a spammed duplicate.
                if recent_success {
                    return;
                }
                self.cast.delay_end_ms = now + buffer;
                self.queue_event(QueueEvent::NonGcdQueued, spell_id);
                self.cast.non_gcd_queued = true;
                self.non_gcd.push(p, replace);
            } else if !gcd_active {
                self.queue_event(QueueEvent::NormalQueued, spell_id);
                self.cast.normal_queued = true;
                self.last_normal = p;
            }
        }
        if now.saturating_sub(self.last_buffer_increase_ms) > BUFFER_INCREASE_FREQUENCY
            && self.buffer_ms - self.settings.min_buffer_time_ms
                < self.settings.max_buffer_increase_ms
        {
            self.buffer_ms += DYNAMIC_BUFFER_INCREMENT;
            self.last_buffer_increase_ms = now;
        }
    }

    /// `CastResultHandlerHook`: the server delay estimate from a success, and the history
    /// verdict (a success is the oldest waiting cast's, a failure the newest's).
    pub fn cast_result(&mut self, spell_id: u32, success: bool, now: u64) {
        self.last_server_spell_delay_ms = 0;
        if success && self.latency_ms > 0 {
            let latency = self.latency_ms;
            if let Some(p) = self.history.oldest_waiting(spell_id) {
                let response = now as i64 - p.start_ms as i64;
                let delay = response - (latency + p.cast_time_ms) as i64;
                self.last_server_spell_delay_ms = if delay > 0 { delay as u64 + 15 } else { 1 };
            }
        }
        let p = if success {
            self.history.oldest_waiting(spell_id)
        } else {
            self.history.newest_waiting(spell_id)
        };
        if let Some(p) = p {
            p.result = if success {
                CastResult::ServerSuccess
            } else {
                CastResult::ServerFailure
            };
        }
    }

    /// `SpellStartHandlerHook` for our own cast: re-time the cast end to the server's cast
    /// time when it differs by more than 5 ms, never below the GCD.
    pub fn spell_start_self(&mut self, spell: SpellFacts, cast_time_ms: u64, gcd_ms: u64) {
        let buffer = self.buffer_ms;
        let mut cast_end = None;
        if let Some(p) = self.history.find_spell(spell.id) {
            if !spell.auto_repeat {
                if p.cast_time_ms + 5 < cast_time_ms {
                    cast_end =
                        Some(p.start_ms + cast_time_ms + if cast_time_ms > 0 { buffer } else { 0 });
                    p.cast_time_ms = cast_time_ms;
                } else if cast_time_ms + 5 < p.cast_time_ms {
                    let gcd = gcd_ms.min(1500);
                    if gcd > cast_time_ms {
                        cast_end = Some(p.start_ms + gcd);
                        p.cast_time_ms = gcd;
                    } else {
                        cast_end = Some(p.start_ms + cast_time_ms + buffer);
                        p.cast_time_ms = cast_time_ms;
                    }
                }
            }
            p.retries = 0;
        }
        if let Some(end) = cast_end {
            self.cast.cast_end_ms = end;
        }
        self.last_normal.retries = 0;
    }

    /// The on-swing strike resolved in our `SMSG_SPELL_GO`: send the queued one, if any.
    pub fn spell_go_self(&mut self, spell: SpellFacts, now: u64) {
        if self.cast.channeling || !spell.on_swing {
            return;
        }
        self.last.on_swing_start_ms = now;
        self.cast.pending_on_swing_cast = false;
        if self.cast.on_swing_queued {
            let p = self.last_on_swing;
            self.queue_event(QueueEvent::OnSwingQueuePopped, p.spell_id);
            self.out.push(Action::Cast {
                spell_id: p.spell_id,
                target: p.target,
                item: p.item,
                origin: Origin::Queue { retries: 0 },
            });
            self.cast.on_swing_queued = false;
        }
    }

    /// `SpellDelayedHook` for our own cast: pushback moves the cast end.
    pub fn spell_delayed_self(&mut self, delay_ms: u64, now: u64) {
        if now < self.cast.cast_end_ms {
            self.cast.cast_end_ms += delay_ms;
        }
    }

    /// `SpellChannelStartHandlerHook`: the channel's own clock and its tick from the first
    /// amplitude, scaled by how far haste shortened the duration.
    pub fn channel_start(&mut self, spell: Option<SpellFacts>, duration_ms: u64, now: u64) {
        let Some(spell) =
            spell.filter(|s| duration_ms > 0 && !s.far_sight && !FARSIGHT_CHANNELS.contains(&s.id))
        else {
            self.reset_channeling();
            return;
        };
        let c = &mut self.cast;
        c.channeling = true;
        c.channel_start_ms = now;
        c.channel_end_ms = now + duration_ms;
        c.channel_spell_id = spell.id;
        c.channel_duration_ms = duration_ms;
        let reduction = if spell.base_duration_ms > 0 && spell.base_duration_ms < 1_000_000 {
            duration_ms as f64 / spell.base_duration_ms as f64
        } else {
            1.0
        };
        let amplitude = spell
            .amplitudes
            .into_iter()
            .find(|a| *a > 0 && *a <= duration_ms)
            .unwrap_or(duration_ms);
        let tick = (amplitude as f64 * reduction) as u64;
        c.channel_tick_ms = if tick == 0 || tick > duration_ms {
            duration_ms
        } else {
            tick
        };
        self.last.channel_start_ms = now;
    }

    /// `SpellChannelUpdateHandlerHook`: 0 ends the channel, anything else re-times it.
    pub fn channel_update(&mut self, remaining_ms: u64, now: u64) {
        if remaining_ms == 0 {
            self.reset_channeling();
        } else {
            self.cast.channel_end_ms = now + remaining_ms;
        }
    }

    /// The last normal cast's target, retargeted by `SpellTargetUnit` while the cursor is up.
    pub fn retarget_queued(&mut self, guid: u64) {
        self.last_normal.target = Some(guid);
        self.last_on_swing.target = Some(guid);
        if let Some(p) = self.non_gcd.peek_mut() {
            p.target = Some(guid);
        }
    }

    /// What `GetCastInfo` reports: the newest cast record and the timers.
    pub fn cast_info(&self) -> Option<&CastParams> {
        if self.cast.cast_spell_id == 0 && self.cast.channel_spell_id == 0 {
            return None;
        }
        self.history.peek()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREBALL: u32 = 133;
    const FIRE_BLAST: u32 = 2136;

    fn fireball() -> SpellFacts {
        SpellFacts {
            id: FIREBALL,
            on_gcd: true,
            gcd_category: 133,
            ..Default::default()
        }
    }

    fn press(spell: SpellFacts, cast_time_ms: u64, now: u64) -> Attempt {
        Attempt {
            spell,
            cast_time_ms,
            gcd_ms: 1500,
            now,
            ..Default::default()
        }
    }

    fn sent(e: &mut Engine, a: Attempt) -> GateVerdict {
        let v = e.attempt(&a, PlayerFacts::default());
        if v != GateVerdict::Stop {
            e.outcome(Outcome::Sent, 0);
        }
        v
    }

    fn queue_events(e: &mut Engine) -> Vec<(i64, i64)> {
        e.out
            .drain(..)
            .filter_map(|a| match a {
                Action::Event("SPELL_QUEUE_EVENT", args) => match args.as_slice() {
                    [ScriptValue::Int(code), ScriptValue::Int(id)] => Some((*code, *id)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_press_inside_the_window_queues_and_pops_when_the_cast_ends() {
        let mut e = Engine::default();
        assert_eq!(
            sent(&mut e, press(fireball(), 3000, 0)),
            GateVerdict::PassOverInFlight
        );
        // Cast end = 3000 + the 55 ms buffer; 600 ms before is outside the 500 ms window.
        assert_eq!(
            e.attempt(&press(fireball(), 3000, 2455), PlayerFacts::default()),
            GateVerdict::Stop
        );
        assert!(!e.cast.normal_queued);
        e.out.clear();
        assert_eq!(
            e.attempt(&press(fireball(), 3000, 2700), PlayerFacts::default()),
            GateVerdict::Stop
        );
        assert!(e.cast.normal_queued);
        assert_eq!(
            queue_events(&mut e),
            vec![(QueueEvent::NormalQueued as i64, 133)]
        );
        e.tick(3000);
        assert!(e.out.is_empty(), "nothing pops before the buffered end");
        e.tick(3055);
        assert!(matches!(
            e.out.first(),
            Some(Action::Cast {
                spell_id: FIREBALL,
                origin: Origin::Queue { retries: 0 },
                ..
            })
        ));
        assert!(!e.cast.normal_queued);
    }

    #[test]
    fn a_short_cast_under_the_gcd_needs_no_buffer() {
        let mut e = Engine::default();
        let instant = SpellFacts {
            id: 1,
            ..fireball()
        };
        sent(&mut e, press(instant, 0, 0));
        assert_eq!(e.cast.gcd_end_ms, 1501);
        assert_eq!(e.cast.cast_end_ms, 0);
        let mut e = Engine::default();
        sent(&mut e, press(fireball(), 1480, 0));
        // 20 ms short of the GCD takes 20 ms off the 55 ms buffer.
        assert_eq!(e.cast.gcd_end_ms, 1500 + 35 + 1);
        assert_eq!(e.cast.cast_end_ms, 1480 + 35);
    }

    #[test]
    fn non_gcd_spells_queue_behind_a_cast_and_fire_in_order() {
        let mut e = Engine::default();
        sent(&mut e, press(fireball(), 3000, 0));
        let blast = SpellFacts {
            id: FIRE_BLAST,
            on_gcd: false,
            ..Default::default()
        };
        let other = SpellFacts { id: 99, ..blast };
        assert_eq!(
            e.attempt(&press(blast, 0, 2800), PlayerFacts::default()),
            GateVerdict::Stop
        );
        assert_eq!(
            e.attempt(&press(other, 0, 2810), PlayerFacts::default()),
            GateVerdict::Stop
        );
        // A repeat press only retargets the queued copy.
        assert_eq!(
            e.attempt(&press(blast, 0, 2820), PlayerFacts::default()),
            GateVerdict::Stop
        );
        assert_eq!(e.non_gcd.len(), 2);
        e.out.clear();
        e.tick(3055);
        assert!(matches!(
            e.out.first(),
            Some(Action::Cast {
                spell_id: FIRE_BLAST,
                ..
            })
        ));
        assert!(e.cast.non_gcd_queued, "the second still waits");
    }

    #[test]
    fn spam_protection_holds_an_instant_waiting_on_the_server() {
        let mut e = Engine::default();
        let instant = SpellFacts {
            id: 5,
            on_gcd: false,
            ..Default::default()
        };
        sent(&mut e, press(instant, 0, 0));
        // Past the 100 ms non-GCD delay, within 500 ms of a waiting cast.
        assert_eq!(
            e.attempt(&press(instant, 0, 200), PlayerFacts::default()),
            GateVerdict::Stop
        );
        e.cast_result(5, true, 250);
        assert_eq!(
            e.attempt(&press(instant, 0, 300), PlayerFacts::default()),
            GateVerdict::Stop
        );
        assert_eq!(
            e.attempt(&press(instant, 0, 600), PlayerFacts::default()),
            GateVerdict::PassOverInFlight
        );
    }

    #[test]
    fn a_rejected_cast_retries_and_the_buffer_grows() {
        let mut e = Engine::default();
        sent(&mut e, press(fireball(), 0, 0));
        e.spell_failed(FIREBALL, SPELL_FAILED_SPELL_IN_PROGRESS, true, 6000, 0, 0);
        // Too old (6 s) to retry, but the buffer still grows.
        assert!(!e.cast.normal_queued);
        assert_eq!(e.buffer_ms, 60);
        sent(&mut e, press(fireball(), 0, 7000));
        e.spell_failed(FIREBALL, SPELL_FAILED_SPELL_IN_PROGRESS, true, 7100, 0, 0);
        assert!(
            e.cast.normal_queued,
            "the failure cleared the GCD, so it retries"
        );
        assert_eq!(e.buffer_ms, 60, "one increase per 5 s");
    }

    #[test]
    fn a_not_ready_refusal_queues_for_the_cooldown_end() {
        let mut e = Engine::default();
        let blast = SpellFacts {
            id: FIRE_BLAST,
            ..fireball()
        };
        let v = e.attempt(&press(blast, 0, 1000), PlayerFacts::default());
        assert_eq!(v, GateVerdict::PassOverInFlight);
        e.outcome(Outcome::Refused(SPELL_FAILED_NOT_READY), 200);
        assert!(e.cast.cooldown_normal_queued);
        e.tick(1150);
        assert!(!e.out.iter().any(|a| matches!(a, Action::Cast { .. })));
        e.tick(1200);
        assert!(e.out.iter().any(|a| matches!(
            a,
            Action::Cast {
                spell_id: FIRE_BLAST,
                ..
            }
        )));
    }

    #[test]
    fn a_queued_script_waits_for_the_window_and_runs_by_priority() {
        let mut e = Engine::default();
        assert!(
            !e.queue_script("a()".into(), 1, 0),
            "nothing running: it runs now"
        );
        sent(&mut e, press(fireball(), 1000, 0));
        assert!(e.queue_script("b()".into(), 3, 700));
        e.queue_normal(&press(fireball(), 1000, 800), false);
        e.out.clear();
        e.tick(1600);
        assert!(
            matches!(e.out.first(), Some(Action::Cast { .. })),
            "the cast goes before priority 3"
        );
        e.out.clear();
        e.tick(1601);
        assert_eq!(e.out.first(), Some(&Action::Script("b()".into())));
    }

    #[test]
    fn the_mount_guard_refuses_at_the_buff_cap_only() {
        let mut e = Engine::default();
        let mount = SpellFacts {
            id: 470,
            mounting: true,
            ..fireball()
        };
        let capped = PlayerFacts {
            buff_capped: true,
            mounted: false,
        };
        assert_eq!(e.attempt(&press(mount, 3000, 0), capped), GateVerdict::Stop);
        assert!(matches!(e.out.first(), Some(Action::UiError(_))));
        assert_ne!(
            e.attempt(&press(mount, 3000, 0), PlayerFacts::default()),
            GateVerdict::Stop
        );
    }

    #[test]
    fn a_channel_holds_the_queue_until_its_latency_trimmed_end() {
        let mut e = Engine::default();
        e.sample_latency(100);
        let drain = SpellFacts {
            id: 689,
            channeled: true,
            base_duration_ms: 5000,
            amplitudes: [1000, 0, 0],
            ..fireball()
        };
        sent(&mut e, press(drain, 0, 0));
        e.channel_start(Some(drain), 5000, 100);
        assert_eq!(e.cast.channel_tick_ms, 1000);
        // Inside the 1500 ms channel window, a press queues.
        assert_eq!(
            e.attempt(&press(fireball(), 3000, 4000), PlayerFacts::default()),
            GateVerdict::Stop
        );
        assert!(e.cast.normal_queued);
        e.out.clear();
        e.tick(4900);
        assert!(
            e.cast.channeling,
            "75% of 100 ms latency is not yet reached"
        );
        e.tick(5026);
        assert!(!e.cast.channeling);
        assert!(e.out.iter().any(|a| matches!(
            a,
            Action::Cast {
                spell_id: FIREBALL,
                ..
            }
        )));
    }
}

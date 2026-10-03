//! The cast tracker: `spell/Cast.cpp`'s cast and channel state, `spell/CastEvents.cpp`'s
//! `UNIT_SPELLCAST_*` derivation and `spell/Interruptible.cpp`'s `notInterruptible`.
//!
//! 1.12 keeps no readable cast times, so the player's cast is stamped the moment benilla sends it
//! (with the client's predicted cast time), snapped to `SMSG_SPELL_START`'s server time, stretched
//! by `SMSG_SPELL_DELAYED`, and cleared when benilla's in-flight cast resolves. A chained
//! same-spell recast, which the client never re-sends, is stamped from `SMSG_SPELL_START` alone.
//! The channel comes from `SMSG_SPELL_START` (instant channels) and `MSG_CHANNEL_START`, ending at
//! its time, at `MSG_CHANNEL_UPDATE` 0, or when `UNIT_CHANNEL_SPELL` drops after showing it.
//! Other units' casts are cached from `SMSG_SPELL_START`, ended by their `SMSG_SPELL_GO` or
//! `SMSG_SPELL_FAILED_OTHER`.
//!
//! All times are ms on the tracker's own clock; the Lua side puts them on `GetTime()`'s.

use std::collections::HashMap;

use crate::aura::{Env, AURA_TOTAL};
use crate::dbc::Row;
use crate::mirror::field;
use crate::spells::{col, EFFECTS};

/// `SPELL_ATTR_TRADESPELL`: a profession recipe cast.
const ATTR_TRADESPELL: u32 = 0x20;
/// `SPELL_ATTR_RANGED`: the server adds 500 ms of aim time to these.
const ATTR_RANGED: u32 = 0x2;
/// `SPELL_ATTR_EX_CHANNELED`.
const ATTR_EX_CHANNELED: u32 = 0x4 | 0x40;
/// `SPELL_ATTR_EX2_AUTOREPEAT_FLAG`: Auto Shot and Shoot, no aim time.
const ATTR_EX2_AUTOREPEAT: u32 = 0x20;
/// `SPELL_INTERRUPT_FLAG_MOVEMENT`; a cast without it (a grenade) survives a move.
const INTERRUPT_FLAG_MOVEMENT: u32 = 0x1;
/// `SPELL_INTERRUPT_FLAG_DAMAGE`, the bit an interrupt effect needs on a cast.
const INTERRUPT_FLAG_DAMAGE: u32 = 0x2;
/// `CHANNEL_FLAG_INTERRUPT`, the same for a channel.
const CHANNEL_FLAG_INTERRUPT: u32 = 0x4;
/// `MOVEFLAG_MASK_CAST_DROP`: forward, backward, both strafes, falling.
const MOVEFLAG_MASK_CAST_DROP: u32 = 0x200f;
/// `SPELL_PREVENTION_TYPE_SILENCE`.
const PREVENTION_TYPE_SILENCE: i32 = 1;
const EFFECT_APPLY_AURA: u32 = 6;
const EFFECT_TRIGGER_SPELL: u32 = 64;
const EFFECT_INTERRUPT_CAST: u32 = 68;
const AURA_MOD_SILENCE: u32 = 27;
const AURA_EFFECT_IMMUNITY: u32 = 37;
const AURA_STATE_IMMUNITY: u32 = 38;
const AURA_SCHOOL_IMMUNITY: u32 = 39;
const AURA_MECHANIC_IMMUNITY: u32 = 77;
const MECHANIC_SILENCE: i32 = 9;
const MECHANIC_INTERRUPT: i32 = 26;

/// Auto Shot, whose ramp spams client failures; filtered from the failure events.
const AUTO_SHOT: u32 = 75;
/// How recently the client stamp must have started for `SMSG_SPELL_START` to confirm it rather
/// than start a chained recast.
const CAST_START_DEDUP_MS: i64 = 500;
/// The grace past a move-dropped cast's end before it is given up on.
const MOVE_DROPPED_GRACE_MS: i64 = 750;
const INTERRUPT_DEDUP_MS: i64 = 1000;
const END_WINDOW_MS: i64 = 1000;
const CHAN_SUCC_DEFER_MS: i64 = 500;
/// `SpellCastResult` codes that interrupt a started cast: INTERRUPTED, INTERRUPTED_COMBAT, MOVING.
const INTERRUPT_RESULTS: [u8; 3] = [35, 36, 46];
/// The codes retail surfaces as `UNIT_SPELLCAST_FAILED_QUIET`: CHARMED, DONT_REPORT,
/// SPELL_IN_PROGRESS.
const QUIET_RESULTS: [u8; 3] = [20, 23, 97];

/// The tracker's clock: ms since the first call.
pub fn now_ms() -> i64 {
    clock().elapsed().as_millis() as i64
}

/// A tracker time as an instant, for the `GetTime()` conversion.
pub fn instant(ms: i64) -> std::time::Instant {
    clock() + std::time::Duration::from_millis(ms.max(0) as u64)
}

fn clock() -> std::time::Instant {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(std::time::Instant::now)
}

/// The record's localized name and rank.
pub fn name_rank(rec: &Row) -> (String, String) {
    (
        rec.loc(col::NAME).to_string(),
        rec.loc(col::RANK).to_string(),
    )
}

pub fn is_channel(rec: &Row) -> bool {
    rec.u32(col::ATTRIBUTES_EX) & ATTR_EX_CHANNELED != 0
}

pub fn is_tradeskill(rec: &Row) -> bool {
    rec.u32(col::ATTRIBUTES) & ATTR_TRADESPELL != 0
}

/// A cast or channel being tracked; spell 0 is none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tracked {
    pub spell: u32,
    pub start: i64,
    pub end: i64,
    /// Accumulated pushback, ms.
    pub delay: i64,
}

/// Another unit's cast from its `SMSG_SPELL_START`.
#[derive(Clone, Copy, Debug)]
pub struct RemoteCast {
    pub target: u64,
    pub spell: u32,
    pub start: i64,
    pub end: i64,
    pub channel: bool,
}

/// One remote cast's event progress.
#[derive(Clone, Copy, Debug, Default)]
struct RemoteEvt {
    spell: u32,
    uid: u32,
    end: i64,
    channel: bool,
    pending_start: bool,
    start_fired: bool,
    succeeded: bool,
    succeeded_fired: bool,
    aborted: bool,
}

/// Whom an event names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Player,
    /// Every token naming this guid.
    Unit(u64),
}

/// One `UNIT_SPELLCAST_*` event to fire.
#[derive(Clone, Debug, PartialEq)]
pub struct Fire {
    pub who: Who,
    pub event: &'static str,
    pub spell: u32,
    /// The castGUID; `None` for a reticle event, which has no cast yet.
    pub cast_guid: Option<String>,
    /// `UNIT_SPELLCAST_SENT`'s target, by guid; 0 for none.
    pub sent_target: Option<u64>,
}

/// `BuildCastGuid`: `Cast-<type>-0-0-0-<spellID>-<castUID>`, type 3 for a real cast and 2 for a
/// local-only failure.
pub fn cast_guid(kind: u32, spell: u32, uid: u32) -> String {
    format!("Cast-{kind}-0-0-0-{spell}-{uid:010X}")
}

/// What the player can stop a cast with, as school masks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Capability {
    interrupt: u32,
    silence: u32,
}

impl Capability {
    fn any(&self) -> bool {
        self.interrupt | self.silence != 0
    }
}

#[derive(Default)]
pub struct Tracker {
    pub cast: Tracked,
    pub channel: Tracked,
    /// The unit the player's cast or channel aims at, from `SMSG_SPELL_START`; 0 for none.
    pub target: u64,
    /// The cast was stamped from `SMSG_SPELL_START` (a chained recast benilla never sent), so the
    /// in-flight clear does not apply.
    from_server: bool,
    /// The cast survived a move that cleared benilla's in-flight cast (no movement interrupt).
    move_dropped: bool,
    /// The stamped cast's `SMSG_SPELL_GO` came.
    cast_done: bool,
    /// `UNIT_CHANNEL_SPELL` has shown the channel, so its drop to 0 ends it.
    channel_confirmed: bool,
    pub remote: HashMap<u64, RemoteCast>,

    // `CastEvents`.
    guid_counter: u32,
    uid_clock: (u64, u32),
    ev_cast: Tracked,
    ev_cast_uid: u32,
    cast_succeeded: bool,
    last_interrupt: (u32, i64),
    ev_chan_spell: u32,
    ev_chan_uid: u32,
    ev_chan_start: i64,
    /// The castGUID minted at SENT, `(uid, spell)`, until a downstream event takes it.
    pending: Option<(u32, u32)>,
    /// The last cast that stopped, `(spell, when, uid)`.
    ended: (u32, i64, u32),
    pending_chan_succ: Option<(u32, i64)>,
    reticle: u32,
    reticle_placed: bool,
    remote_evt: HashMap<u64, RemoteEvt>,
    fires: Vec<Fire>,
    cap: Option<(u64, Capability)>,
}

impl Tracker {
    pub fn take_fires(&mut self) -> Vec<Fire> {
        std::mem::take(&mut self.fires)
    }

    /// `MakeCastUID`: the unix second modulo 2²³, with a per-second counter above it.
    fn make_uid(&mut self) -> u32 {
        let sec = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if sec == self.uid_clock.0 {
            self.uid_clock.1 += 1;
        } else {
            self.uid_clock = (sec, 0);
        }
        (self.uid_clock.1 << 23) | (sec as u32 & 0x7F_FFFF)
    }

    /// `NextCastGuid`: the SENT-minted uid when it is this spell's, else a fresh one.
    fn next_uid(&mut self, spell: u32) -> u32 {
        match self.pending {
            Some((uid, s)) if s == spell => {
                self.pending = None;
                uid
            }
            _ => self.make_uid(),
        }
    }

    fn fire(&mut self, who: Who, event: &'static str, spell: u32, kind: u32, uid: u32) {
        self.fires.push(Fire {
            who,
            event,
            spell,
            cast_guid: Some(cast_guid(kind, spell, uid)),
            sent_target: None,
        });
    }

    fn fire_player(&mut self, event: &'static str, spell: u32, uid: u32) {
        self.fire(Who::Player, event, spell, 3, uid);
    }

    // ---- The player's cast: the send, the server, the frame ----

    /// A press benilla sent (`CastStartSet_h` and `OnSend`): SENT with a fresh castGUID, and the
    /// cast stamped now with the predicted time when it has one.
    pub fn on_sent(
        &mut self,
        env: &Env,
        spell: u32,
        target: Option<u64>,
        predicted_ms: u32,
        now: i64,
    ) {
        if spell == 0 {
            return;
        }
        if self.reticle != 0 && spell == self.reticle {
            self.reticle_placed = true;
        }
        let uid = self.make_uid();
        self.pending = Some((uid, spell));
        self.fires.push(Fire {
            who: Who::Player,
            event: "UNIT_SPELLCAST_SENT",
            spell,
            cast_guid: Some(cast_guid(3, spell, uid)),
            sent_target: Some(target.unwrap_or(0)),
        });
        let dur = predicted_cast_ms(env, spell, predicted_ms);
        if dur > 0 {
            self.cast = Tracked {
                spell,
                start: now,
                end: now + dur,
                delay: 0,
            };
            self.from_server = false;
            self.move_dropped = false;
            self.cast_done = false;
            self.channel.spell = 0;
            self.target = 0;
        }
    }

    /// `HandleSpellStart`.
    pub fn on_spell_start(
        &mut self,
        env: &Env,
        caster: u64,
        spell: u32,
        cast_ms: u32,
        target: u64,
        now: i64,
    ) {
        let Some(rec) = env.spells.and_then(|t| t.row(spell)) else {
            return;
        };
        if caster == 0 {
            return;
        }
        let channel = is_channel(&rec);
        let cast_ms = i64::from(cast_ms);
        if caster == env.mirror.player {
            if channel && cast_ms == 0 {
                let dur = i64::from(env.duration_ms(&rec, false));
                if dur > 0 {
                    self.stamp_channel(spell, now, now + dur);
                    self.target = target;
                }
                return;
            }
            if cast_ms == 0 {
                return;
            }
            if self.cast.spell == spell
                && now < self.cast.end
                && now - self.cast.start < CAST_START_DEDUP_MS
            {
                // The confirming packet: keep the start, snap the end to the server's time.
                self.cast.end = self.cast.start + cast_ms + self.cast.delay;
                self.target = target;
                return;
            }
            self.cast = Tracked {
                spell,
                start: now,
                end: now + cast_ms,
                delay: 0,
            };
            self.from_server = true;
            self.move_dropped = false;
            self.cast_done = false;
            self.channel.spell = 0;
            self.target = target;
            return;
        }
        // A cast-then-channel spell (Mind Control) is in its cast phase here.
        let instant_channel = channel && cast_ms == 0;
        let end = if instant_channel {
            now + i64::from(env.duration_ms(&rec, true))
        } else {
            now + cast_ms
        };
        self.remote.insert(
            caster,
            RemoteCast {
                target,
                spell,
                start: now,
                end,
                channel: instant_channel,
            },
        );
        if cast_ms > 0 || instant_channel {
            let uid = self.make_uid();
            self.remote_evt.insert(
                caster,
                RemoteEvt {
                    spell,
                    uid,
                    end,
                    channel: instant_channel,
                    pending_start: true,
                    ..Default::default()
                },
            );
        }
    }

    fn stamp_channel(&mut self, spell: u32, start: i64, end: i64) {
        self.channel = Tracked {
            spell,
            start,
            end,
            delay: 0,
        };
        self.channel_confirmed = false;
        self.cast.spell = 0;
    }

    /// `SMSG_SPELL_DELAYED`: pushback on our cast.
    pub fn on_delayed(&mut self, player: u64, caster: u64, delay_ms: u32) {
        if delay_ms != 0 && self.cast.spell != 0 && caster == player {
            self.cast.end += i64::from(delay_ms);
            self.cast.delay += i64::from(delay_ms);
        }
    }

    /// `MSG_CHANNEL_START`: the server's duration.
    pub fn on_channel_start(&mut self, spell: u32, duration_ms: u32, now: i64) {
        if spell != 0 && duration_ms > 0 {
            self.stamp_channel(spell, now, now + i64::from(duration_ms));
        }
    }

    /// `MSG_CHANNEL_UPDATE`: pushback re-anchors the end; 0 ends the channel.
    pub fn on_channel_update(&mut self, remaining_ms: u32, now: i64) {
        let spell = self.channel.spell;
        if spell == 0 {
            return;
        }
        if remaining_ms == 0 {
            self.channel.spell = 0;
            return;
        }
        self.channel.end = now + i64::from(remaining_ms);
        if self.ev_chan_spell == spell {
            let uid = self.ev_chan_uid;
            self.fire_player("UNIT_SPELLCAST_CHANNEL_UPDATE", spell, uid);
        }
    }

    /// `HandleCastAborted`, from `SMSG_SPELL_FAILED_OTHER`.
    pub fn on_aborted(&mut self, env: &Env, caster: u64, spell: u32) {
        if caster == 0 || spell == 0 {
            return;
        }
        if self.remote.get(&caster).is_some_and(|r| r.spell == spell) {
            self.remote.remove(&caster);
        }
        if caster == env.mirror.player {
            if self.cast.spell == spell {
                if !movement_interruptible(env, spell) && player_moving(env) {
                    self.move_dropped = true;
                } else {
                    self.cast.spell = 0;
                }
            }
            return;
        }
        if let Some(e) = self.remote_evt.get_mut(&caster) {
            if e.spell == spell {
                e.aborted = true;
            }
        }
    }

    /// `SMSG_SPELL_GO`: the player's SUCCEEDED (`OnPlayerSucceeded`), or a remote cast's
    /// completion, which also ends its cached cast as the engine's `ClearCastingSpell` does.
    pub fn on_spell_go(&mut self, env: &Env, caster: u64, spell: u32, now: i64) {
        if caster == 0 || spell == 0 {
            return;
        }
        if caster == env.mirror.player {
            let channel = env
                .spells
                .and_then(|t| t.row(spell))
                .is_some_and(|r| is_channel(&r));
            if channel {
                self.pending_chan_succ = Some((spell, now));
                return;
            }
            if self.cast.spell == spell {
                self.cast_done = true;
            }
            let uid = if self.ev_cast.spell == spell {
                self.cast_succeeded = true;
                self.ev_cast_uid
            } else {
                self.next_uid(spell)
            };
            self.fire_player("UNIT_SPELLCAST_SUCCEEDED", spell, uid);
            return;
        }
        if self
            .remote
            .get(&caster)
            .is_some_and(|r| r.spell == spell && !r.channel)
        {
            self.remote.remove(&caster);
        }
        if let Some(e) = self.remote_evt.get_mut(&caster) {
            if e.spell == spell {
                e.succeeded = true;
                return;
            }
        }
        let uid = self.make_uid();
        self.fire(Who::Unit(caster), "UNIT_SPELLCAST_SUCCEEDED", spell, 3, uid);
    }

    /// `SpellFailed_h`: a refusal of our cast, benilla's own or the server's `SMSG_CAST_RESULT`.
    pub fn on_failed(&mut self, spell: u32, result: u8, now: i64) {
        if spell == 0 || spell == AUTO_SHOT {
            return;
        }
        if QUIET_RESULTS.contains(&result) {
            self.guid_counter += 1;
            let uid = self.guid_counter;
            self.fire(Who::Player, "UNIT_SPELLCAST_FAILED_QUIET", spell, 2, uid);
            return;
        }
        let recent = self.ended.0 == spell && now - self.ended.1 < END_WINDOW_MS;
        if INTERRUPT_RESULTS.contains(&result) {
            let uid = if self.ev_cast.spell == spell {
                self.ev_cast_uid
            } else if recent {
                self.ended.2
            } else {
                self.next_uid(spell)
            };
            self.last_interrupt = (spell, now);
            self.fire_player("UNIT_SPELLCAST_INTERRUPTED", spell, uid);
            return;
        }
        if self.ev_cast.spell == spell || recent {
            return;
        }
        self.guid_counter += 1;
        let uid = self.guid_counter;
        self.fire(Who::Player, "UNIT_SPELLCAST_FAILED", spell, 2, uid);
    }

    /// `OnWorldTick`: the clears, then the event polls. benilla's committed cast stands for the
    /// engine's current-cast global. Deviation: benilla drops that on a move as the engine does,
    /// so the movement-immune test the DLL makes on the failure packet is made here; and a
    /// server-stamped cast ends at its `SPELL_GO` or a grace past its end, where the DLL leaves it
    /// to the next cast.
    pub fn tick(&mut self, env: &Env, now: i64) {
        let targeting = env.mirror.targeting;
        if self.cast.spell != 0
            && !self.from_server
            && !self.move_dropped
            && env.mirror.cast_committed == 0
        {
            if !movement_interruptible(env, self.cast.spell) && player_moving(env) {
                self.move_dropped = true;
            } else {
                self.cast.spell = 0;
            }
        }
        if self.cast.spell != 0
            && (self.move_dropped || self.from_server)
            && (self.cast_done || now >= self.cast.end + MOVE_DROPPED_GRACE_MS)
        {
            self.cast.spell = 0;
            self.move_dropped = false;
        }
        // A reticle spell is not cast until it is placed; SMSG_SPELL_START stamps it then.
        if self.cast.spell != 0 && !self.from_server && targeting == self.cast.spell {
            self.cast.spell = 0;
        }
        if self.channel.spell != 0 {
            if let Some(me) = env.mirror.me() {
                let chan = me.u32(field::UNIT_CHANNEL_SPELL);
                if chan == self.channel.spell {
                    self.channel_confirmed = true;
                } else if chan == 0 && self.channel_confirmed {
                    self.channel.spell = 0;
                }
            }
            if self.channel.spell != 0 && self.channel.end != 0 && now >= self.channel.end {
                self.channel.spell = 0;
            }
        }
        self.poll_player(self.cast, self.channel, now);
        self.poll_remote(now);
        self.poll_reticle(targeting);
        self.remote.retain(|_, r| now < r.end);
    }

    /// `PollPlayer`.
    fn poll_player(&mut self, cast: Tracked, chan: Tracked, now: i64) {
        let new_cast = cast.spell != 0
            && (cast.spell != self.ev_cast.spell || cast.start != self.ev_cast.start);
        if self.ev_cast.spell != 0 && (cast.spell == 0 || new_cast) {
            let (spell, uid) = (self.ev_cast.spell, self.ev_cast_uid);
            let already =
                self.last_interrupt.0 == spell && now - self.last_interrupt.1 < INTERRUPT_DEDUP_MS;
            if !self.cast_succeeded && !already {
                self.fire_player("UNIT_SPELLCAST_INTERRUPTED", spell, uid);
            }
            self.fire_player("UNIT_SPELLCAST_STOP", spell, uid);
            self.ended = (spell, now, uid);
            self.ev_cast.spell = 0;
        }
        if new_cast {
            self.ev_cast_uid = self.next_uid(cast.spell);
            self.ev_cast = cast;
            self.cast_succeeded = false;
            let uid = self.ev_cast_uid;
            self.fire_player("UNIT_SPELLCAST_START", cast.spell, uid);
        } else if self.ev_cast.spell != 0 && cast.end != self.ev_cast.end {
            self.ev_cast.end = cast.end;
            let (spell, uid) = (self.ev_cast.spell, self.ev_cast_uid);
            self.fire_player("UNIT_SPELLCAST_DELAYED", spell, uid);
        }

        let chan_spell = if chan.spell != 0 && (chan.end == 0 || now < chan.end) {
            chan.spell
        } else {
            0
        };
        let re_channel = chan_spell != 0
            && chan_spell == self.ev_chan_spell
            && chan.start != self.ev_chan_start
            && self.pending.is_some_and(|(_, s)| s == chan_spell);
        let new_chan = chan_spell != 0 && (chan_spell != self.ev_chan_spell || re_channel);
        if self.ev_chan_spell != 0 && (chan_spell == 0 || new_chan) {
            let (spell, uid) = (self.ev_chan_spell, self.ev_chan_uid);
            self.fire_player("UNIT_SPELLCAST_CHANNEL_STOP", spell, uid);
            self.ev_chan_spell = 0;
        }
        if new_chan {
            self.ev_chan_uid = self.next_uid(chan_spell);
            self.ev_chan_spell = chan_spell;
            self.ev_chan_start = chan.start;
            let uid = self.ev_chan_uid;
            self.fire_player("UNIT_SPELLCAST_CHANNEL_START", chan_spell, uid);
            if self.pending_chan_succ.is_some_and(|(s, _)| s == chan_spell) {
                self.fire_player("UNIT_SPELLCAST_SUCCEEDED", chan_spell, uid);
                self.pending_chan_succ = None;
            }
        }
        if let Some((spell, at)) = self.pending_chan_succ {
            if now - at >= CHAN_SUCC_DEFER_MS {
                let uid = self.next_uid(spell);
                self.fire_player("UNIT_SPELLCAST_SUCCEEDED", spell, uid);
                self.pending_chan_succ = None;
            }
        }
    }

    /// `PollRemote`: START, then SUCCEEDED (and a cast's STOP), an abort's INTERRUPTED and STOP,
    /// or the natural end.
    fn poll_remote(&mut self, now: i64) {
        let mut out = Vec::new();
        self.remote_evt.retain(|&guid, e| {
            let mut fire = |event: &'static str| out.push((guid, event, e.spell, e.uid));
            if e.pending_start {
                fire(if e.channel {
                    "UNIT_SPELLCAST_CHANNEL_START"
                } else {
                    "UNIT_SPELLCAST_START"
                });
                e.pending_start = false;
                e.start_fired = true;
            }
            if e.succeeded && !e.succeeded_fired {
                fire("UNIT_SPELLCAST_SUCCEEDED");
                e.succeeded_fired = true;
                if !e.channel {
                    fire("UNIT_SPELLCAST_STOP");
                    return false;
                }
                e.aborted = false;
            }
            let stop = if e.channel {
                "UNIT_SPELLCAST_CHANNEL_STOP"
            } else {
                "UNIT_SPELLCAST_STOP"
            };
            if e.aborted {
                if e.start_fired && !e.channel {
                    fire("UNIT_SPELLCAST_INTERRUPTED");
                }
                fire(stop);
                return false;
            }
            if e.end != 0 && now >= e.end {
                fire(stop);
                return false;
            }
            true
        });
        for (guid, event, spell, uid) in out {
            self.fire(Who::Unit(guid), event, spell, 3, uid);
        }
    }

    /// `PollReticle`: RETICLE_TARGET when the cursor comes up, RETICLE_CLEAR when it is
    /// cancelled rather than placed.
    fn poll_reticle(&mut self, targeting: u32) {
        let event = if targeting != 0 {
            if self.reticle != 0 {
                return;
            }
            self.reticle = targeting;
            self.reticle_placed = false;
            "UNIT_SPELLCAST_RETICLE_TARGET"
        } else {
            if self.reticle == 0 {
                return;
            }
            let (spell, placed) = (self.reticle, self.reticle_placed);
            self.reticle = 0;
            self.reticle_placed = false;
            if placed {
                return;
            }
            self.fires.push(Fire {
                who: Who::Player,
                event: "UNIT_SPELLCAST_RETICLE_CLEAR",
                spell,
                cast_guid: None,
                sent_target: None,
            });
            return;
        };
        self.fires.push(Fire {
            who: Who::Player,
            event,
            spell: targeting,
            cast_guid: None,
            sent_target: None,
        });
    }

    // ---- Readers ----

    /// `CurrentCastGuid`: the castGUID of the cast `caster` has running, channels excluded.
    pub fn current_cast_guid(&self, player: u64, caster: u64, spell: u32) -> Option<String> {
        if caster == 0 || spell == 0 {
            return None;
        }
        let uid = if caster == player {
            (self.ev_cast.spell == spell).then_some(self.ev_cast_uid)?
        } else {
            let e = self.remote_evt.get(&caster)?;
            (e.spell == spell && !e.channel).then_some(e.uid)?
        };
        Some(cast_guid(3, spell, uid))
    }

    /// The player's live cast, none once past its end.
    pub fn player_cast(&self, now: i64) -> Option<Tracked> {
        (self.cast.spell != 0 && now < self.cast.end).then_some(self.cast)
    }

    /// The player's live channel.
    pub fn player_channel(&self, now: i64) -> Option<Tracked> {
        let c = self.channel;
        (c.spell != 0 && (c.end == 0 || now < c.end)).then_some(c)
    }

    /// `TargetGuidForCaster`: whom `caster`'s live cast or channel aims at; 0 for none.
    pub fn target_of(&self, player: u64, caster: u64, now: i64) -> u64 {
        if caster == 0 {
            return 0;
        }
        if caster == player {
            let live = self.player_cast(now).is_some() || self.player_channel(now).is_some();
            return if live { self.target } else { 0 };
        }
        self.remote
            .get(&caster)
            .filter(|r| now < r.end)
            .map_or(0, |r| r.target)
    }

    /// `NotInterruptible`: none of the player's interrupts or silences can stop this cast on
    /// `caster` (0 for an unknown caster, whose immunities are not read).
    pub fn not_interruptible(&mut self, env: &Env, caster: u64, rec: &Row, channel: bool) -> bool {
        let mut cap = self.capability(env);
        if !cap.any() {
            return false;
        }
        if rec.i32(col::PREVENTION_TYPE) != PREVENTION_TYPE_SILENCE {
            return true;
        }
        strip_immunities(env, caster, &mut cap);
        let (flags, needed) = if channel {
            (
                rec.u32(col::CHANNEL_INTERRUPT_FLAGS),
                CHANNEL_FLAG_INTERRUPT,
            )
        } else {
            (rec.u32(col::INTERRUPT_FLAGS), INTERRUPT_FLAG_DAMAGE)
        };
        let interrupt_works = cap.interrupt != 0 && flags & needed != 0;
        !(interrupt_works || cap.silence != 0)
    }

    /// `PlayerCapability`, rebuilt when the known spells change.
    fn capability(&mut self, env: &Env) -> Capability {
        let key = env.known.iter().fold(env.known.len() as u64, |h, &s| {
            h.wrapping_mul(31).wrapping_add(u64::from(s))
        });
        if let Some((k, cap)) = self.cap {
            if k == key {
                return cap;
            }
        }
        let mut cap = Capability::default();
        if let Some(spells) = env.spells {
            for &id in env.known {
                if let Some(rec) = spells.row(id) {
                    fold(env, &rec, &mut cap, true);
                }
            }
        }
        self.cap = Some((key, cap));
        cap
    }
}

/// `CastTimeMs`: the predicted time plus the server's 500 ms aim on a ranged, non-autorepeat spell
/// that has a cast time.
fn predicted_cast_ms(env: &Env, spell: u32, predicted_ms: u32) -> i64 {
    let mut ms = i64::from(predicted_ms);
    if ms > 0 {
        if let Some(rec) = env.spells.and_then(|t| t.row(spell)) {
            if rec.u32(col::ATTRIBUTES) & ATTR_RANGED != 0
                && rec.u32(col::ATTRIBUTES_EX2) & ATTR_EX2_AUTOREPEAT == 0
            {
                ms += 500;
            }
        }
    }
    ms
}

/// `MovementInterruptible`: an unknown spell counts as interruptible.
fn movement_interruptible(env: &Env, spell: u32) -> bool {
    env.spells
        .and_then(|t| t.row(spell))
        .is_none_or(|r| r.u32(col::INTERRUPT_FLAGS) & INTERRUPT_FLAG_MOVEMENT != 0)
}

fn player_moving(env: &Env) -> bool {
    env.mirror.move_flags & MOVEFLAG_MASK_CAST_DROP != 0
}

fn school_mask(rec: &Row) -> u32 {
    let school = rec.u32(col::SCHOOL);
    if school < 32 {
        1 << school
    } else {
        0
    }
}

/// `Fold`: the record's school into each capability its effects give, one trigger hop deep
/// (Feral Charge's interrupt lives on the spell it triggers).
fn fold(env: &Env, rec: &Row, cap: &mut Capability, follow: bool) {
    let mask = school_mask(rec);
    for i in 0..EFFECTS {
        let effect = rec.u32(col::EFFECT + i);
        if effect == EFFECT_INTERRUPT_CAST {
            cap.interrupt |= mask;
        } else if effect == EFFECT_APPLY_AURA
            && rec.u32(col::EFFECT_APPLY_AURA_NAME + i) == AURA_MOD_SILENCE
        {
            cap.silence |= mask;
        } else if follow && effect == EFFECT_TRIGGER_SPELL {
            let trigger = rec.i32(col::EFFECT_TRIGGER_SPELL + i);
            if trigger > 0 {
                if let Some(t) = env.spells.and_then(|s| s.row(trigger as u32)) {
                    fold(env, &t, cap, false);
                }
            }
        }
    }
}

/// `StripImmunities`: what the caster's auras, hidden ones included, make it immune to.
fn strip_immunities(env: &Env, caster: u64, cap: &mut Capability) {
    let (Some(fields), Some(spells)) = (env.mirror.object(caster), env.spells) else {
        return;
    };
    for slot in 0..AURA_TOTAL {
        if !cap.any() {
            break;
        }
        let Some(rec) = spells.row(fields.u32(field::UNIT_AURA + slot)) else {
            continue;
        };
        for i in 0..EFFECTS {
            let misc = rec.i32(col::EFFECT_MISC_VALUE + i);
            match rec.u32(col::EFFECT_APPLY_AURA_NAME + i) {
                AURA_EFFECT_IMMUNITY if misc as u32 == EFFECT_INTERRUPT_CAST => cap.interrupt = 0,
                AURA_STATE_IMMUNITY if misc as u32 == AURA_MOD_SILENCE => cap.silence = 0,
                AURA_SCHOOL_IMMUNITY => {
                    cap.interrupt &= !(misc as u32);
                    cap.silence &= !(misc as u32);
                }
                AURA_MECHANIC_IMMUNITY if misc == MECHANIC_INTERRUPT => cap.interrupt = 0,
                AURA_MECHANIC_IMMUNITY if misc == MECHANIC_SILENCE => cap.silence = 0,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbc::Dbc;
    use crate::mirror::Mirror;
    use crate::spellmod;

    const ME: u64 = 0x10;
    const OTHER: u64 = 0xF130_0000_0000_0001;
    const FROSTBOLT: u32 = 116;
    const DRAIN: u32 = 689;
    const KICK: u32 = 1766;

    fn spells() -> Dbc {
        let blank = || vec![0u32; 0x2B4 / 4];
        let mut frostbolt = blank();
        frostbolt[0] = FROSTBOLT;
        frostbolt[col::NAME] = 1;
        frostbolt[col::INTERRUPT_FLAGS] = INTERRUPT_FLAG_MOVEMENT | INTERRUPT_FLAG_DAMAGE;
        frostbolt[col::PREVENTION_TYPE] = PREVENTION_TYPE_SILENCE as u32;
        let mut drain = blank();
        drain[0] = DRAIN;
        drain[col::ATTRIBUTES_EX] = 0x4;
        drain[col::DURATION_INDEX] = 1;
        let mut kick = blank();
        kick[0] = KICK;
        kick[col::EFFECT] = EFFECT_INTERRUPT_CAST;
        Dbc::from_rows(&[frostbolt, drain, kick], b"\0Frostbolt\0")
    }

    fn durations() -> Dbc {
        Dbc::from_rows(&[vec![1, 5000, 0, 5000]], b"\0")
    }

    fn mirror() -> Mirror {
        let mut m = Mirror::default();
        m.player = ME;
        m
    }

    fn env<'a>(s: &'a Dbc, d: &'a Dbc, m: &'a Mirror, t: &'a spellmod::Tables) -> Env<'a> {
        Env {
            spells: Some(s),
            durations: Some(d),
            mirror: m,
            mods: t,
            family: 0,
            known: &[],
        }
    }

    fn events(c: &mut Tracker) -> Vec<(&'static str, Who, Option<String>)> {
        c.take_fires()
            .into_iter()
            .map(|f| (f.event, f.who, f.cast_guid))
            .collect()
    }

    fn names(ev: &[(&'static str, Who, Option<String>)]) -> Vec<&'static str> {
        ev.iter().map(|e| e.0).collect()
    }

    #[test]
    fn a_cast_runs_sent_start_delayed_succeeded_stop_on_one_guid() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let mut m = mirror();
        let mut c = Tracker::default();
        c.on_sent(&env(&s, &d, &m, &t), FROSTBOLT, Some(OTHER), 2500, 0);
        m.cast_committed = FROSTBOLT;
        c.tick(&env(&s, &d, &m, &t), 0);
        assert_eq!(c.player_cast(10).map(|x| x.end), Some(2500));
        // The server's time snaps the end without restarting the cast.
        c.on_spell_start(&env(&s, &d, &m, &t), ME, FROSTBOLT, 3000, OTHER, 100);
        c.tick(&env(&s, &d, &m, &t), 100);
        assert_eq!(
            c.player_cast(110).map(|x| (x.start, x.end)),
            Some((0, 3000))
        );
        assert_eq!(c.target_of(ME, ME, 110), OTHER);
        c.on_spell_go(&env(&s, &d, &m, &t), ME, FROSTBOLT, 3000);
        m.cast_committed = 0;
        c.tick(&env(&s, &d, &m, &t), 3000);
        let ev = events(&mut c);
        assert_eq!(
            names(&ev),
            [
                "UNIT_SPELLCAST_SENT",
                "UNIT_SPELLCAST_START",
                "UNIT_SPELLCAST_DELAYED",
                "UNIT_SPELLCAST_SUCCEEDED",
                "UNIT_SPELLCAST_STOP"
            ]
        );
        assert!(ev.iter().all(|e| e.2 == ev[0].2 && e.1 == Who::Player));
        assert!(c.player_cast(3000).is_none());
    }

    #[test]
    fn a_cast_dropped_without_its_spell_go_is_interrupted() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let mut m = mirror();
        let mut c = Tracker::default();
        c.on_sent(&env(&s, &d, &m, &t), FROSTBOLT, None, 2500, 0);
        m.cast_committed = FROSTBOLT;
        c.tick(&env(&s, &d, &m, &t), 0);
        m.cast_committed = 0;
        c.tick(&env(&s, &d, &m, &t), 800);
        assert_eq!(
            names(&events(&mut c)),
            [
                "UNIT_SPELLCAST_SENT",
                "UNIT_SPELLCAST_START",
                "UNIT_SPELLCAST_INTERRUPTED",
                "UNIT_SPELLCAST_STOP"
            ]
        );
        // A refusal before any cast started is a local-only FAILED, a type-2 guid.
        c.on_failed(FROSTBOLT, 0x0C, 5000);
        let ev = events(&mut c);
        assert_eq!(names(&ev), ["UNIT_SPELLCAST_FAILED"]);
        assert!(ev[0].2.as_deref().unwrap().starts_with("Cast-2-"));
    }

    #[test]
    fn a_chained_recast_the_client_never_sent_starts_from_the_server() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let mut m = mirror();
        let mut c = Tracker::default();
        c.on_sent(&env(&s, &d, &m, &t), FROSTBOLT, None, 2500, 0);
        m.cast_committed = FROSTBOLT;
        c.tick(&env(&s, &d, &m, &t), 0);
        // One frame: the first cast lands and the second starts.
        c.on_spell_go(&env(&s, &d, &m, &t), ME, FROSTBOLT, 2500);
        c.on_spell_start(&env(&s, &d, &m, &t), ME, FROSTBOLT, 2500, 0, 2500);
        m.cast_committed = 0;
        c.tick(&env(&s, &d, &m, &t), 2500);
        let ev = names(&events(&mut c));
        assert_eq!(
            &ev[2..],
            [
                "UNIT_SPELLCAST_SUCCEEDED",
                "UNIT_SPELLCAST_STOP",
                "UNIT_SPELLCAST_START"
            ]
        );
        assert_eq!(c.player_cast(2600).map(|x| x.start), Some(2500));
    }

    #[test]
    fn another_units_cast_starts_and_is_interrupted_by_its_failure_packet() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let m = mirror();
        let e = env(&s, &d, &m, &t);
        let mut c = Tracker::default();
        c.on_spell_start(&e, OTHER, FROSTBOLT, 3000, ME, 0);
        c.tick(&e, 0);
        assert_eq!(c.target_of(ME, OTHER, 10), ME);
        assert!(c.current_cast_guid(ME, OTHER, FROSTBOLT).is_some());
        c.on_aborted(&e, OTHER, FROSTBOLT);
        c.tick(&e, 500);
        let ev = events(&mut c);
        assert_eq!(
            names(&ev),
            [
                "UNIT_SPELLCAST_START",
                "UNIT_SPELLCAST_INTERRUPTED",
                "UNIT_SPELLCAST_STOP"
            ]
        );
        assert!(ev.iter().all(|x| x.1 == Who::Unit(OTHER)));
        assert!(c.remote.is_empty());
    }

    #[test]
    fn a_channel_defers_its_succeeded_past_channel_start_and_ends_at_update_zero() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let m = mirror();
        let e = env(&s, &d, &m, &t);
        let mut c = Tracker::default();
        c.on_spell_start(&e, ME, DRAIN, 0, OTHER, 0);
        c.on_spell_go(&e, ME, DRAIN, 0);
        c.tick(&e, 0);
        assert_eq!(c.player_channel(10).map(|x| x.end), Some(5000));
        c.on_channel_update(0, 1000);
        c.tick(&e, 1000);
        assert_eq!(
            names(&events(&mut c)),
            [
                "UNIT_SPELLCAST_CHANNEL_START",
                "UNIT_SPELLCAST_SUCCEEDED",
                "UNIT_SPELLCAST_CHANNEL_STOP"
            ]
        );
    }

    #[test]
    fn not_interruptible_is_relative_to_what_the_player_can_stop() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let m = mirror();
        let mut c = Tracker::default();
        let frostbolt = s.row(FROSTBOLT).unwrap();
        let drain = s.row(DRAIN).unwrap();
        // No interrupt known: never flagged.
        assert!(!c.not_interruptible(&env(&s, &d, &m, &t), OTHER, &drain, true));
        let known = [KICK];
        let e = Env {
            known: &known,
            ..env(&s, &d, &m, &t)
        };
        assert!(!c.not_interruptible(&e, OTHER, &frostbolt, false));
        // Prevention type other than silence: nothing stops it.
        assert!(c.not_interruptible(&e, OTHER, &drain, true));
    }
}

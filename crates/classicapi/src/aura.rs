//! `aura/Source.cpp`, `aura/ComboDuration.cpp`, `aura/JudgementRefresh.cpp` and the Turtle rules
//! that feed them: the caster and expiry of every aura, which `UNIT_FIELD_AURA` does not carry.
//!
//! The descriptor stores spell ids only. The one place the client sees an aura's caster and a
//! server duration is `SMSG_SPELL_GO`, so each aura-applying cast is remembered per
//! `(target, spell, caster)`, the server's own identity for an aura, and bound to the descriptor
//! slot its application seats ([`Source::on_aura_added`]). The cache is best-effort: it knows
//! the casts seen since login, and a miss leaves the modern-truthful defaults (no source, no
//! expiry).

use std::collections::HashMap;
use std::time::Instant;

use crate::dbc::{Dbc, Row};
use crate::mirror::{field, typemask, Fields, Mirror};
use crate::spellmod;
use crate::spells::{self, col};

/// `UNIT_FIELD_AURA`'s 48 slots: 32 helpful, then 16 harmful.
pub const AURA_TOTAL: usize = 48;
pub const BUFF_COUNT: usize = 32;
/// The `slot` of an entry no descriptor slot is bound to, and of a query with none to go on.
pub const SLOT_UNBOUND: i16 = -1;

/// The cache's capacity: a fully raid-buffed 40-man plus its debuff load.
const CACHE_SIZE: usize = 2048;
/// A capture younger than this is exempt from [`Source::evict_absent`]: `SMSG_SPELL_GO` beats the
/// update that seats the aura.
const EVICT_GRACE_MS: u64 = 2000;
/// The oldest capture an arriving application may seat: an instant's lands within a server tick,
/// a projectile's at impact.
const SEAT_WINDOW_MS: u64 = 3000;
const RECENT_CAST_COUNT: usize = 16;
const RECENT_CAST_TTL_MS: u64 = 1500;
const PENDING_MOD_MAX: usize = 8;
const PENDING_MOD_TTL_MS: u64 = 2000;
const MAX_MODS: usize = 128;
const COMPRESSION_MAX: usize = 4;
const COMPRESS_STATE_MAX: usize = 16;
const JUDGE_ARM_MAX: usize = 4;
const JUDGE_ARM_TTL_MS: u64 = 2000;
const TRIG_APP_MAX: usize = 32;
const TRIG_APP_ARM_MAX: usize = 8;
const TRIG_APP_ARM_TTL_MS: u64 = 2000;
const GROUP_SNAPSHOTS: usize = 44;
const GROUP_SNAPSHOT_TTL_MS: u64 = 30_000;
const COMBO_CAPTURES: usize = 8;
const COMBO_CAPTURE_TTL_MS: u64 = 2000;
const MAX_COMBO_OVERRIDES: usize = 64;

/// `SPELL_PALADIN_JUDGEMENT`, the one spell every seal judges through.
const SPELL_JUDGEMENT: u32 = 20271;
const FAMILY_PALADIN: u32 = 10;
/// `SPELL_ATTR_EX3_ALWAYS_HIT`, the judgement-debuff marker.
const ATTR_EX3_ALWAYS_HIT: u32 = 0x0004_0000;
/// `SPELL_AURA_PERIODIC_DAMAGE`, `SPELL_AURA_PERIODIC_LEECH`.
const AURA_PERIODIC_DAMAGE: i32 = 3;
const AURA_PERIODIC_LEECH: i32 = 53;

/// Helpful or harmful, as the aura's descriptor slot said when it was applied; `Unknown` for a
/// cast seen only in `SMSG_SPELL_GO`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Unknown,
    Helpful,
    Harmful,
}

/// One aura instance.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub target: u64,
    /// 0: applied, its cast never seen.
    pub caster: u64,
    pub spell: u32,
    /// On [`Source::now_ms`]'s clock; 0 infinite or unknown.
    pub expiration: u64,
    /// The applied duration with the caster's modifiers, ms; 0 none.
    pub duration: u32,
    /// The last write, the evict grace's clock.
    stamp: u64,
    /// The descriptor slot it is seated in, [`SLOT_UNBOUND`] until bound.
    pub slot: i16,
    pub kind: Kind,
}

/// `DURATION_MOD_*`, the Lua op strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModOp {
    Refresh,
    Reduce,
    Set,
    Remove,
}

impl ModOp {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "refresh" => Some(Self::Refresh),
            "reduce" => Some(Self::Reduce),
            "set" => Some(Self::Set),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }

    /// Refresh and set land on the same value when run twice; reduce and remove do not.
    fn idempotent(self) -> bool {
        matches!(self, Self::Refresh | Self::Set)
    }
}

/// A server duration edit the client is never told of, mirrored from its trigger's
/// `SMSG_SPELL_GO`. The trigger matches by exact spell, else by family and school; the affected
/// aura by family plus a flag overlap and/or an icon.
#[derive(Clone, Copy, Debug)]
struct DurationMod {
    trigger_spell: u32,
    trigger_family: u32,
    /// -1 any school.
    trigger_school: i32,
    affected_family: u32,
    affected_mask: u64,
    affected_icon: u32,
    op: ModOp,
    value_ms: i32,
}

/// An idempotent edit armed for the aura its trigger is about to create.
#[derive(Clone, Copy, Debug)]
struct PendingMod {
    target: u64,
    caster: u64,
    family: u32,
    mask: u64,
    icon: u32,
    op: ModOp,
    value_ms: i32,
    until: u64,
}

/// While a trigger aura is up, the same caster's periodic auras on the target drain `pct`% faster.
#[derive(Clone, Copy, Debug)]
struct TickCompression {
    trigger_family: u32,
    trigger_mask: u64,
    affected_family: u32,
    affected_mask: u64,
    pct: u32,
}

#[derive(Clone, Copy, Debug)]
struct CompressState {
    target: u64,
    caster: u64,
    spell: u32,
    start: u64,
    anchor: u64,
    last_written: u64,
    last_signal: u64,
    touched: bool,
}

/// A server `AddAura` off another spell's hit, attributed to the player at `pct`% duration.
#[derive(Clone, Copy, Debug)]
struct TriggeredApplication {
    trigger_spell: u32,
    trigger_family: u32,
    trigger_mask: u64,
    gate_spell: u32,
    affected_family: u32,
    affected_mask: u64,
    pct: u32,
}

#[derive(Clone, Copy, Debug)]
struct TrigAppArm {
    victim: u64,
    family: u32,
    mask: u64,
    pct: u32,
    until: u64,
}

#[derive(Clone, Copy, Debug)]
struct JudgeArm {
    victim: u64,
    caster: u64,
    until: u64,
}

/// The combo points a finisher was sent with, which the server spends before its `SPELL_GO`.
#[derive(Clone, Copy, Debug)]
struct ComboCapture {
    spell: u32,
    points: u8,
    at: u64,
}

/// What the cache reads off the rest of the state.
pub struct Env<'a> {
    pub spells: Option<&'a Dbc>,
    pub durations: Option<&'a Dbc>,
    pub mirror: &'a Mirror,
    pub mods: &'a spellmod::Tables,
    /// The player's `SpellFamilyName`, the modifier tables' selector.
    pub family: u32,
    pub known: &'a [u32],
}

impl Env<'_> {
    fn rec(&self, spell: u32) -> Option<Row<'_>> {
        self.spells?.row(spell)
    }

    fn player(&self) -> u64 {
        self.mirror.player
    }

    fn level(&self) -> u32 {
        self.mirror.me().map_or(0, |f| f.level())
    }

    /// `PlayerKnowsSpell`: the known-spell bitmap, read when the trigger lands.
    fn knows(&self, spell: u32) -> bool {
        spell != 0 && self.known.contains(&spell)
    }

    /// `DescriptorListsAura`: the unit is held and lists the spell in some slot.
    fn descriptor_lists(&self, guid: u64, spell: u32) -> bool {
        guid != 0
            && spell != 0
            && self
                .mirror
                .object(guid)
                .filter(|f| f.is(typemask::UNIT))
                .is_some_and(|f| (0..AURA_TOTAL).any(|s| f.u32(field::UNIT_AURA + s) == spell))
    }

    /// `FUN_GET_SPELL_DURATION`: the `SpellDuration.dbc` row with level scaling against the
    /// player's level, capped at the row's max, then the player's duration modifier unless
    /// `skip_mod`; 0 for none or infinite.
    pub fn duration_ms(&self, rec: &Row, skip_mod: bool) -> u32 {
        let Some(row) = self
            .durations
            .and_then(|d| d.row(rec.u32(col::DURATION_INDEX)))
        else {
            return 0;
        };
        let (base, per_level, max) = (row.i32(1), row.i32(2), row.i32(3));
        if base < 0 && per_level < 1 {
            return 0;
        }
        let base_level = rec.i32(col::BASE_LEVEL);
        let level = (self.level() as i32).max(base_level);
        let mut ms = (level - base_level) * per_level + base;
        if max > 0 && ms > max {
            ms = max;
        }
        if !skip_mod {
            ms = spellmod::apply(
                self.mods,
                self.family,
                rec,
                spellmod::OP_DURATION,
                ms as f32,
            ) as i32;
        }
        u32::try_from(ms).unwrap_or(0)
    }
}

/// `UNIT_AURA_VISIBLE_MASK`, the flag nibble bits the engine's visibility gate tests.
pub const VISIBLE_MASK: u8 = 0x0E;
/// Turtle's polarity nibble: `CANCELABLE`, `HELPFUL`, `HARMFUL` (stock: effect-index bits).
const FLAG_CANCELABLE: u8 = 0x01;
const FLAG_HELPFUL: u8 = 0x04;
const FLAG_HARMFUL: u8 = 0x08;

/// The spell in descriptor slot `slot`.
pub fn slot_spell(f: &Fields, slot: usize) -> u32 {
    if slot < AURA_TOTAL {
        f.u32(field::UNIT_AURA + slot)
    } else {
        0
    }
}

/// `UNIT_FIELD_AURAFLAGS`' nibble for `slot`.
pub fn slot_nibble(f: &Fields, slot: usize) -> u8 {
    (f.byte(field::UNIT_AURA_FLAGS, slot / 2) >> ((slot & 1) * 4)) & 0xF
}

/// `ReadStacks`: `UNIT_FIELD_AURAAPPLICATIONS` stores the count less one.
pub fn slot_stacks(f: &Fields, slot: usize) -> u32 {
    u32::from(f.byte(field::UNIT_AURA_APPLICATIONS, slot)) + 1
}

/// A cast applies an aura when any of its three effects names one.
pub fn applies_aura(rec: &Row) -> bool {
    (0..spells::EFFECTS).any(|i| rec.i32(col::EFFECT_APPLY_AURA_NAME + i) != 0)
}

/// `AffectedMatchesRaw`: the family always, then a flag overlap and/or the icon; a zero half is
/// dropped from the test.
fn affected_matches(rec: Option<&Row>, family: u32, mask: u64, icon: u32) -> bool {
    let Some(rec) = rec else {
        return false;
    };
    rec.u32(col::FAMILY_NAME) == family
        && (mask == 0 || spells::fits_family(rec, family, mask))
        && (icon == 0 || rec.u32(col::ICON_ID) == icon)
}

fn fits(rec: Option<&Row>, family: u32, mask: u64) -> bool {
    rec.is_some_and(|r| spells::fits_family(r, family, mask))
}

/// The judgement-debuff selector: `SPELLFAMILY_PALADIN` with `SPELL_ATTR_EX3_ALWAYS_HIT`.
fn is_judgement(rec: Option<&Row>) -> bool {
    rec.is_some_and(|r| {
        r.u32(col::FAMILY_NAME) == FAMILY_PALADIN
            && r.u32(col::ATTRIBUTES_EX3) & ATTR_EX3_ALWAYS_HIT != 0
    })
}

/// The server's tick-compression gate: a periodic damage or leech effect.
fn periodic_damage_or_leech(rec: &Row) -> bool {
    (0..spells::EFFECTS).any(|i| {
        matches!(
            rec.i32(col::EFFECT_APPLY_AURA_NAME + i),
            AURA_PERIODIC_DAMAGE | AURA_PERIODIC_LEECH
        )
    })
}

/// Turtle's reworked Rip, whose duration index dangles in the client data:
/// `turtle/ComboDuration.cpp`'s `(base, max)` ms.
fn turtle_dangling_combo(spell: u32) -> Option<(i32, i32)> {
    matches!(spell, 1079 | 9492 | 9493 | 9752 | 9894 | 9896).then_some((8000, 18000))
}

/// The entries by stable index, with each unit's indices in insertion order: a lookup reads only
/// its unit's few entries.
#[derive(Default)]
struct Slab {
    slots: Vec<Option<Entry>>,
    free: Vec<usize>,
    by_target: HashMap<u64, Vec<usize>>,
    len: usize,
}

impl Slab {
    fn len(&self) -> usize {
        self.len
    }

    fn insert(&mut self, e: Entry) -> usize {
        let i = match self.free.pop() {
            Some(i) => {
                self.slots[i] = Some(e);
                i
            }
            None => {
                self.slots.push(Some(e));
                self.slots.len() - 1
            }
        };
        self.by_target.entry(e.target).or_default().push(i);
        self.len += 1;
        i
    }

    fn remove(&mut self, i: usize) {
        let Some(e) = self.slots.get_mut(i).and_then(Option::take) else {
            return;
        };
        if let Some(ids) = self.by_target.get_mut(&e.target) {
            ids.retain(|x| *x != i);
            if ids.is_empty() {
                self.by_target.remove(&e.target);
            }
        }
        self.free.push(i);
        self.len -= 1;
    }

    fn get(&self, i: usize) -> Option<&Entry> {
        self.slots.get(i).and_then(Option::as_ref)
    }

    fn get_mut(&mut self, i: usize) -> Option<&mut Entry> {
        self.slots.get_mut(i).and_then(Option::as_mut)
    }

    /// The unit's entry indices, oldest first.
    fn of(&self, target: u64) -> Vec<usize> {
        self.by_target.get(&target).cloned().unwrap_or_default()
    }

    fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.slots.iter().flatten()
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.slots.iter_mut().flatten()
    }

    fn retain(&mut self, mut keep: impl FnMut(&Entry) -> bool) {
        let gone: Vec<usize> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().filter(|e| !keep(e)).map(|_| i))
            .collect();
        for i in gone {
            self.remove(i);
        }
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

impl std::ops::Index<usize> for Slab {
    type Output = Entry;
    fn index(&self, i: usize) -> &Entry {
        self.get(i).expect("live aura cache entry")
    }
}

impl std::ops::IndexMut<usize> for Slab {
    fn index_mut(&mut self, i: usize) -> &mut Entry {
        self.get_mut(i).expect("live aura cache entry")
    }
}

/// The cache and every rule that edits it.
pub struct Source {
    epoch: Instant,
    entries: Slab,
    recent: [(u32, u64); RECENT_CAST_COUNT],
    recent_cursor: usize,
    mods: Vec<DurationMod>,
    pending: Vec<PendingMod>,
    compressions: Vec<TickCompression>,
    compress: Vec<CompressState>,
    judge_arms: Vec<JudgeArm>,
    trig_apps: Vec<TriggeredApplication>,
    trig_arms: Vec<TrigAppArm>,
    group: HashMap<u64, ([u16; AURA_TOTAL], u64)>,
    combo: [Option<ComboCapture>; COMBO_CAPTURES],
    combo_cursor: usize,
    combo_overrides: Vec<(u32, i32, i32)>,
    /// Units whose cached timing moved with no descriptor write: `UNIT_AURA` again for each.
    signals: Vec<u64>,
    last_map: Option<u32>,
    /// The server is Turtle's (`Turtle::Detected`: `TURTLE_WOW_VERSION` is set).
    pub turtle: bool,
    /// Latched once a nibble proves the flags are not Turtle's polarity encoding.
    nibble_contradicted: bool,
    /// Turtle's server rules, registered once Turtle is seen.
    turtle_registered: bool,
    /// Turtle's Carnage: the Ferocious Bite's target, window end and last combo points.
    carnage: Option<(u64, u64, u8)>,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            entries: Slab::default(),
            recent: [(0, 0); RECENT_CAST_COUNT],
            recent_cursor: 0,
            mods: Vec::new(),
            pending: Vec::new(),
            compressions: Vec::new(),
            compress: Vec::new(),
            judge_arms: Vec::new(),
            trig_apps: Vec::new(),
            trig_arms: Vec::new(),
            group: HashMap::new(),
            combo: [None; COMBO_CAPTURES],
            combo_cursor: 0,
            combo_overrides: Vec::new(),
            signals: Vec::new(),
            last_map: None,
            turtle: false,
            nibble_contradicted: false,
            turtle_registered: false,
            carnage: None,
        }
    }
}

impl Source {
    /// The cache's clock, ms since it was made; never 0 past the first millisecond.
    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64 + 1
    }

    /// A cache time as an instant, for the `GetTime()` conversion.
    pub fn instant(&self, ms: u64) -> Instant {
        self.epoch + std::time::Duration::from_millis(ms.saturating_sub(1))
    }

    /// `IsSlotHarmful`. The slot range is the polarity, except on Turtle, which spills debuffs
    /// into buff slots and writes the real polarity into the flag nibble; a nibble Turtle could
    /// not have written falls back to the range for good.
    pub fn slot_harmful(&mut self, f: &Fields, slot: usize) -> bool {
        if slot >= AURA_TOTAL {
            return false;
        }
        if !self.turtle || self.nibble_contradicted {
            return slot >= BUFF_COUNT;
        }
        let nibble = slot_nibble(f, slot);
        let turtle_shaped = matches!(nibble, FLAG_HELPFUL | FLAG_HARMFUL)
            || nibble == FLAG_HELPFUL | FLAG_CANCELABLE;
        if nibble != 0 && !turtle_shaped {
            self.nibble_contradicted = true;
            return slot >= BUFF_COUNT;
        }
        nibble & FLAG_HARMFUL != 0
    }

    /// The engine's per-slot callbacks off one descriptor update, ascending: `OnAuraRemoved` for a
    /// spell that left a slot, `OnAuraAdded` for one that arrived, `OnAuraStacksChanged` for a
    /// count that moved.
    pub fn diff(&mut self, env: &Env, guid: u64, old: Option<&Fields>, new: &Fields) {
        if !new.is(typemask::UNIT) {
            return;
        }
        for slot in 0..AURA_TOTAL {
            let was = old.map_or(0, |o| slot_spell(o, slot));
            let now = slot_spell(new, slot);
            if was != now {
                if was != 0 {
                    self.on_aura_removed(guid, was, slot);
                }
                if now != 0 {
                    let harmful = self.slot_harmful(new, slot);
                    self.stamp_application(env, guid, now, slot, harmful);
                }
            } else if now != 0
                && old.is_some_and(|o| slot_stacks(o, slot) != slot_stacks(new, slot))
            {
                let harmful = self.slot_harmful(new, slot);
                self.stamp_application(env, guid, now, slot, harmful);
            }
        }
    }

    /// Units to fire `UNIT_AURA` for, taken.
    pub fn take_signals(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.signals)
    }

    fn signal(&mut self, guid: u64) {
        if guid != 0 && !self.signals.contains(&guid) {
            self.signals.push(guid);
        }
    }

    // ---- Recent player casts ----

    fn remember_player_cast(&mut self, spell: u32, now: u64) {
        if let Some(r) = self.recent.iter_mut().find(|r| r.0 == spell) {
            r.1 = now;
            return;
        }
        self.recent[self.recent_cursor] = (spell, now);
        self.recent_cursor = (self.recent_cursor + 1) % RECENT_CAST_COUNT;
    }

    fn was_recent_player_cast(&self, spell: u32, now: u64) -> bool {
        spell != 0
            && self
                .recent
                .iter()
                .any(|r| r.0 == spell && r.1 != 0 && now - r.1 <= RECENT_CAST_TTL_MS)
    }

    // ---- Lookup ----

    fn find_by_caster(&self, target: u64, spell: u32, caster: u64) -> Option<usize> {
        self.entries.of(target).into_iter().find(|&i| {
            let e = &self.entries[i];
            e.spell == spell && e.caster == caster
        })
    }

    fn find_by_slot(&self, target: u64, spell: u32, slot: i16) -> Option<usize> {
        if slot < 0 {
            return None;
        }
        self.entries.of(target).into_iter().find(|&i| {
            let e = &self.entries[i];
            e.spell == spell && e.slot == slot
        })
    }

    /// The oldest unbound capture inside the seat window: two casters' same-spell auras seat in
    /// cast order, ascending slot.
    fn find_oldest_unbound(&self, target: u64, spell: u32, now: u64) -> Option<usize> {
        self.entries
            .of(target)
            .into_iter()
            .filter(|&i| {
                let e = &self.entries[i];
                e.spell == spell && e.slot == SLOT_UNBOUND && now - e.stamp <= SEAT_WINDOW_MS
            })
            .min_by_key(|&i| std::cmp::Reverse(now - self.entries[i].stamp))
    }

    /// The one entry for `(target, spell)`; none when there are several, which is two casters no
    /// spell id can tell apart.
    fn find_sole(&self, target: u64, spell: u32) -> Option<usize> {
        let mut it = self
            .entries
            .of(target)
            .into_iter()
            .filter(|&i| self.entries[i].spell == spell);
        let first = it.next()?;
        it.next().is_none().then_some(first)
    }

    fn find_instance(&self, target: u64, spell: u32, slot: i16) -> Option<usize> {
        self.find_by_slot(target, spell, slot)
            .or_else(|| self.find_sole(target, spell))
    }

    /// Whether an entry's unit is gone as far as the cache can tell: not held, not the player,
    /// not a groupmate (whose out-of-range auras read the cache).
    fn orphan(env: &Env, e: &Entry) -> bool {
        e.target != env.mirror.player
            && env.mirror.object(e.target).is_none()
            && !env.mirror.group.contains(&e.target)
    }

    /// A new entry's index. When the table is full: first every orphan whose aura has no duration
    /// or has run out, then every expired entry no descriptor backs, then the oldest entry.
    /// Deviation: the DLL frees one slot per new entry, scanning the whole table each time;
    /// reclaiming in bulk keeps a full table from costing a scan per aura.
    fn claim(&mut self, env: &Env, entry: Entry, now: u64) -> usize {
        if self.entries.len() >= CACHE_SIZE {
            self.entries
                .retain(|e| !(Self::orphan(env, e) && (e.expiration == 0 || now >= e.expiration)));
        }
        if self.entries.len() >= CACHE_SIZE {
            self.entries.retain(|e| {
                e.expiration == 0 || now < e.expiration || env.descriptor_lists(e.target, e.spell)
            });
        }
        while self.entries.len() >= CACHE_SIZE {
            let oldest = self
                .entries
                .slots
                .iter()
                .enumerate()
                .filter_map(|(i, e)| e.as_ref().map(|e| (i, e.stamp)))
                .min_by_key(|(_, stamp)| *stamp)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => self.entries.remove(i),
                None => break,
            }
        }
        self.entries.insert(entry)
    }

    // ---- Writes ----

    /// The `SPELL_GO` path: authoritative caster and talented timing. A recast by the same caster
    /// refreshes theirs; a lone caster-less entry for the aura is adopted.
    fn store_from_cast(
        &mut self,
        env: &Env,
        target: u64,
        spell: u32,
        caster: u64,
        expiration: u64,
        duration: u32,
    ) {
        if target == 0 || spell == 0 {
            return;
        }
        let now = self.now_ms();
        let i = self.find_by_caster(target, spell, caster).or_else(|| {
            self.find_sole(target, spell)
                .filter(|&i| self.entries[i].caster == 0)
        });
        let Some(i) = i else {
            let i = self.claim(
                env,
                Entry {
                    target,
                    caster,
                    spell,
                    expiration,
                    duration,
                    stamp: now,
                    slot: SLOT_UNBOUND,
                    kind: Kind::Unknown,
                },
                now,
            );
            self.consume_pending(env, i, now);
            return;
        };
        let e = &mut self.entries[i];
        e.stamp = now;
        e.caster = caster;
        e.expiration = expiration;
        e.duration = duration;
        // An in-place refresh writes no descriptor field, so no engine `UNIT_AURA` follows.
        self.signal(target);
        self.consume_pending(env, i, now);
    }

    /// The application path: a slot and a polarity, base timing, no caster but the player's own
    /// recent cast. Seats the cast capture it belongs to and never clobbers what `SPELL_GO` owns.
    #[allow(clippy::too_many_arguments)]
    fn store_from_application(
        &mut self,
        env: &Env,
        target: u64,
        spell: u32,
        caster: u64,
        expiration: u64,
        duration: u32,
        slot: i16,
        kind: Kind,
    ) {
        if target == 0 || spell == 0 {
            return;
        }
        let now = self.now_ms();
        let slot = if (0..AURA_TOTAL as i16).contains(&slot) {
            slot
        } else {
            SLOT_UNBOUND
        };
        let found = self
            .find_by_slot(target, spell, slot)
            .or_else(|| self.find_oldest_unbound(target, spell, now))
            .or_else(|| (slot < 0).then(|| self.find_sole(target, spell)).flatten());
        let Some(i) = found else {
            let i = self.claim(
                env,
                Entry {
                    target,
                    caster,
                    spell,
                    expiration,
                    duration,
                    stamp: now,
                    slot,
                    kind,
                },
                now,
            );
            self.consume_pending(env, i, now);
            return;
        };
        {
            let e = &mut self.entries[i];
            e.stamp = now;
            if slot >= 0 {
                e.slot = slot;
            }
            if kind != Kind::Unknown {
                e.kind = kind;
            }
        }
        // An armed server edit is newer than the cast this entry came from.
        self.consume_pending(env, i, now);
        let Some(e) = self.entries.get_mut(i) else {
            return;
        };
        if e.caster != 0 {
            return;
        }
        if caster != 0 {
            e.caster = caster;
        }
        e.expiration = expiration;
        e.duration = duration;
    }

    /// `Evict`: the instance the vacated slot held, else the spell's sole entry.
    fn evict(&mut self, target: u64, spell: u32, slot: i16) {
        if target == 0 || spell == 0 {
            return;
        }
        if let Some(i) = self.find_instance(target, spell, slot) {
            self.entries.remove(i);
        }
    }

    // ---- Server duration edits ----

    /// One op on entry `i`; a reduce past now or a remove drops it, as the server removes it.
    fn apply_op(&mut self, i: usize, op: ModOp, value_ms: i32, now: u64) {
        let target = self.entries[i].target;
        let e = &mut self.entries[i];
        let (signal, drop) = match op {
            ModOp::Refresh if e.duration > 0 => {
                e.expiration = now + u64::from(e.duration);
                (true, false)
            }
            ModOp::Refresh => (false, false),
            ModOp::Set => {
                e.duration = value_ms.max(0) as u32;
                e.expiration = now + u64::from(e.duration);
                (true, false)
            }
            ModOp::Reduce if e.expiration != 0 => {
                let v = value_ms.max(0) as u64;
                if e.expiration > now + v {
                    e.expiration -= v;
                    (true, false)
                } else {
                    (false, true)
                }
            }
            ModOp::Reduce => (false, false),
            ModOp::Remove => (false, true),
        };
        if drop {
            self.entries.remove(i);
        }
        if signal {
            self.signal(target);
        }
    }

    fn arm_pending(&mut self, target: u64, caster: u64, m: &DurationMod, now: u64) {
        let armed = PendingMod {
            target,
            caster,
            family: m.affected_family,
            mask: m.affected_mask,
            icon: m.affected_icon,
            op: m.op,
            value_ms: m.value_ms,
            until: now + PENDING_MOD_TTL_MS,
        };
        if let Some(p) = self.pending.iter_mut().find(|p| {
            p.target == target
                && p.family == m.affected_family
                && p.mask == m.affected_mask
                && p.icon == m.affected_icon
        }) {
            *p = armed;
            return;
        }
        self.pending.retain(|p| now < p.until);
        if self.pending.len() >= PENDING_MOD_MAX {
            self.pending.remove(0);
        }
        self.pending.push(armed);
    }

    /// A store landed: apply the first armed edit for it, one-shot.
    fn consume_pending(&mut self, env: &Env, i: usize, now: u64) {
        let Some(e) = self.entries.get(i).copied() else {
            return;
        };
        let rec = env.rec(e.spell);
        let hit = self.pending.iter().position(|p| {
            p.target == e.target
                && now < p.until
                && (e.caster == 0 || e.caster == p.caster)
                && affected_matches(rec.as_ref(), p.family, p.mask, p.icon)
        });
        let Some(p) = hit.map(|h| self.pending.remove(h)) else {
            return;
        };
        if self.entries[i].caster == 0 {
            self.entries[i].caster = p.caster;
        }
        self.apply_op(i, p.op, p.value_ms, now);
    }

    fn trigger_matches(m: &DurationMod, spell: u32, rec: Option<&Row>) -> bool {
        if m.trigger_spell != 0 {
            return m.trigger_spell == spell;
        }
        let Some(rec) = rec else {
            return false;
        };
        rec.u32(col::FAMILY_NAME) == m.trigger_family
            && (m.trigger_school < 0 || rec.i32(col::SCHOOL) == m.trigger_school)
    }

    /// `ApplyDurationModifiers`: each matching rule edits the trigger caster's own aura on each
    /// hit target, adopting a caster-less one, and arms the idempotent ops.
    fn apply_duration_mods(&mut self, env: &Env, spell: u32, caster: u64, targets: &[u64]) {
        if spell == 0 || caster == 0 || targets.is_empty() || self.mods.is_empty() {
            return;
        }
        let trigger = env.rec(spell);
        let now = self.now_ms();
        let rules: Vec<DurationMod> = self
            .mods
            .iter()
            .filter(|m| Self::trigger_matches(m, spell, trigger.as_ref()))
            .copied()
            .collect();
        for m in rules {
            for &t in targets {
                let hit = self.entries.of(t).into_iter().find(|&i| {
                    let e = &self.entries[i];
                    (e.caster == 0 || e.caster == caster)
                        && affected_matches(
                            env.rec(e.spell).as_ref(),
                            m.affected_family,
                            m.affected_mask,
                            m.affected_icon,
                        )
                });
                if let Some(i) = hit {
                    if self.entries[i].caster == 0 {
                        self.entries[i].caster = caster;
                    }
                    self.apply_op(i, m.op, m.value_ms, now);
                }
                if m.op.idempotent() {
                    self.arm_pending(t, caster, &m, now);
                }
            }
        }
    }

    /// `RegisterDurationMod`; false on a degenerate rule or a full table.
    #[allow(clippy::too_many_arguments)]
    pub fn register_duration_mod(
        &mut self,
        trigger_spell: u32,
        trigger_family: u32,
        trigger_school: i32,
        affected_family: u32,
        affected_mask: u64,
        affected_icon: u32,
        op: ModOp,
        value_ms: i32,
    ) -> bool {
        if (trigger_spell == 0 && trigger_family == 0) || (affected_mask == 0 && affected_icon == 0)
        {
            return false;
        }
        if let Some(m) = self.mods.iter_mut().find(|m| {
            m.trigger_spell == trigger_spell
                && m.trigger_family == trigger_family
                && m.trigger_school == trigger_school
                && m.affected_family == affected_family
                && m.affected_mask == affected_mask
                && m.affected_icon == affected_icon
        }) {
            m.op = op;
            m.value_ms = value_ms;
            return true;
        }
        if self.mods.len() >= MAX_MODS {
            return false;
        }
        self.mods.push(DurationMod {
            trigger_spell,
            trigger_family,
            trigger_school,
            affected_family,
            affected_mask,
            affected_icon,
            op,
            value_ms,
        });
        true
    }

    // ---- Tick compression ----

    pub fn add_tick_compression(
        &mut self,
        trigger_family: u32,
        trigger_mask: u64,
        affected_family: u32,
        affected_mask: u64,
        pct: u32,
    ) -> bool {
        if trigger_family == 0 || trigger_mask == 0 || affected_mask == 0 || pct == 0 || pct > 100 {
            return false;
        }
        if let Some(r) = self.compressions.iter_mut().find(|r| {
            r.trigger_family == trigger_family
                && r.trigger_mask == trigger_mask
                && r.affected_family == affected_family
                && r.affected_mask == affected_mask
        }) {
            r.pct = pct;
            return true;
        }
        if self.compressions.len() >= COMPRESSION_MAX {
            return false;
        }
        self.compressions.push(TickCompression {
            trigger_family,
            trigger_mask,
            affected_family,
            affected_mask,
            pct,
        });
        true
    }

    /// Anchored, not incremental: each compressed entry is set to `anchor - elapsed * pct / 100`,
    /// re-anchored when something else re-stamps it.
    fn compress_target(
        &mut self,
        env: &Env,
        c: TickCompression,
        target: u64,
        caster: u64,
        now: u64,
    ) {
        for i in self.entries.of(target) {
            let e = self.entries[i];
            if e.caster != caster || e.expiration == 0 {
                continue;
            }
            let Some(rec) = env.rec(e.spell) else {
                continue;
            };
            if !affected_matches(Some(&rec), c.affected_family, c.affected_mask, 0)
                || !periodic_damage_or_leech(&rec)
            {
                continue;
            }
            let si =
                match self.compress.iter().position(|s| {
                    s.target == e.target && s.caster == e.caster && s.spell == e.spell
                }) {
                    Some(si) => {
                        let s = &mut self.compress[si];
                        if e.expiration != s.last_written {
                            s.start = now;
                            s.anchor = e.expiration;
                        }
                        si
                    }
                    None if self.compress.len() < COMPRESS_STATE_MAX => {
                        self.compress.push(CompressState {
                            target: e.target,
                            caster: e.caster,
                            spell: e.spell,
                            start: now,
                            anchor: e.expiration,
                            last_written: e.expiration,
                            last_signal: 0,
                            touched: false,
                        });
                        self.compress.len() - 1
                    }
                    None => continue,
                };
            let s = &mut self.compress[si];
            s.touched = true;
            let cut = (now - s.start) * u64::from(c.pct) / 100;
            let expiration = s.anchor.saturating_sub(cut);
            s.last_written = expiration;
            let nudge = s.last_signal == 0 || now - s.last_signal >= 1000;
            if nudge {
                s.last_signal = now;
            }
            self.entries[i].expiration = expiration;
            if nudge {
                self.signal(target);
            }
        }
    }

    fn apply_tick_compressions(&mut self, env: &Env, now: u64) {
        if self.compressions.is_empty() {
            return;
        }
        for s in &mut self.compress {
            s.touched = false;
        }
        for c in self.compressions.clone() {
            let triggers: Vec<(u64, u64)> = self
                .entries
                .iter()
                .filter(|t| {
                    t.caster != 0
                        && (t.expiration == 0 || now < t.expiration)
                        && fits(env.rec(t.spell).as_ref(), c.trigger_family, c.trigger_mask)
                })
                .map(|t| (t.target, t.caster))
                .collect();
            for (target, caster) in triggers {
                self.compress_target(env, c, target, caster, now);
            }
        }
        let gone: Vec<u64> = self
            .compress
            .iter()
            .filter(|s| !s.touched)
            .map(|s| s.target)
            .collect();
        for g in gone {
            self.signal(g);
        }
        self.compress.retain(|s| s.touched);
    }

    // ---- Judgements ----

    fn arm_judgement(&mut self, victim: u64, caster: u64, now: u64) {
        let arm = JudgeArm {
            victim,
            caster,
            until: now + JUDGE_ARM_TTL_MS,
        };
        if let Some(a) = self
            .judge_arms
            .iter_mut()
            .find(|a| a.victim == victim && a.caster == caster)
        {
            *a = arm;
            return;
        }
        self.judge_arms.retain(|a| now < a.until);
        if self.judge_arms.len() >= JUDGE_ARM_MAX {
            self.judge_arms.remove(0);
        }
        self.judge_arms.push(arm);
    }

    fn consume_judgement(&mut self, victim: u64, now: u64) -> u64 {
        match self
            .judge_arms
            .iter()
            .position(|a| a.victim == victim && now < a.until)
        {
            Some(i) => self.judge_arms.remove(i).caster,
            None => 0,
        }
    }

    /// The judgement refresh: `adopt` lets the Judgement cast claim a caster-less entry; a swing
    /// stays caster-strict.
    fn refresh_judgements_impl(
        &mut self,
        env: &Env,
        unit: u64,
        attacker: u64,
        adopt: bool,
    ) -> usize {
        if unit == 0 || attacker == 0 {
            return 0;
        }
        let now = self.now_ms();
        let mut n = 0;
        for i in self.entries.of(unit) {
            let e = self.entries[i];
            if e.duration == 0 {
                continue;
            }
            if e.caster != attacker && !(adopt && e.caster == 0) {
                continue;
            }
            if !is_judgement(env.rec(e.spell).as_ref()) {
                continue;
            }
            let e = &mut self.entries[i];
            e.caster = attacker;
            e.expiration = now + u64::from(e.duration);
            e.stamp = now;
            self.signal(unit);
            n += 1;
        }
        n
    }

    /// `RefreshJudgements`, from a white swing that dealt damage.
    pub fn refresh_judgements(&mut self, env: &Env, unit: u64, attacker: u64) -> usize {
        self.refresh_judgements_impl(env, unit, attacker, false)
    }

    // ---- Triggered applications ----

    #[allow(clippy::too_many_arguments)]
    pub fn add_triggered_application(
        &mut self,
        trigger_spell: u32,
        trigger_family: u32,
        trigger_mask: u64,
        gate_spell: u32,
        affected_family: u32,
        affected_mask: u64,
        pct: u32,
    ) -> bool {
        if (trigger_spell == 0 && (trigger_family == 0 || trigger_mask == 0))
            || affected_mask == 0
            || pct == 0
        {
            return false;
        }
        if let Some(r) = self.trig_apps.iter_mut().find(|r| {
            r.trigger_spell == trigger_spell
                && r.trigger_family == trigger_family
                && r.trigger_mask == trigger_mask
                && r.gate_spell == gate_spell
                && r.affected_family == affected_family
                && r.affected_mask == affected_mask
        }) {
            r.pct = pct;
            return true;
        }
        if self.trig_apps.len() >= TRIG_APP_MAX {
            return false;
        }
        self.trig_apps.push(TriggeredApplication {
            trigger_spell,
            trigger_family,
            trigger_mask,
            gate_spell,
            affected_family,
            affected_mask,
            pct,
        });
        true
    }

    /// The server's re-add over an existing matching aura reuses the slot, so the trigger refreshes
    /// it here, keeping the longer remaining time of a player-stamped one.
    fn refresh_triggered_existing(
        &mut self,
        env: &Env,
        victim: u64,
        r: TriggeredApplication,
        now: u64,
    ) {
        let me = env.player();
        for i in self.entries.of(victim) {
            let e = self.entries[i];
            let mine = e.caster == me;
            if !mine && e.caster != 0 {
                continue;
            }
            let Some(rec) = env.rec(e.spell) else {
                continue;
            };
            if !spells::fits_family(&rec, r.affected_family, r.affected_mask) {
                continue;
            }
            let base = env.duration_ms(&rec, false);
            if base == 0 {
                continue;
            }
            let new = (base * r.pct / 100).max(1);
            if mine && e.expiration != 0 && e.expiration.saturating_sub(now) > u64::from(new) {
                continue;
            }
            let e = &mut self.entries[i];
            e.caster = me;
            e.duration = new;
            e.expiration = now + u64::from(new);
            e.stamp = now;
            self.signal(victim);
            break;
        }
    }

    /// A player trigger landed on `victim`: the first live rule arms it.
    fn arm_triggered(&mut self, env: &Env, victim: u64, spell: u32, rec: Option<&Row>) {
        let rule = self.trig_apps.iter().copied().find(|r| {
            let hit = if r.trigger_spell != 0 {
                r.trigger_spell == spell
            } else {
                fits(rec, r.trigger_family, r.trigger_mask)
            };
            hit && (r.gate_spell == 0 || env.knows(r.gate_spell))
        });
        let Some(r) = rule else {
            return;
        };
        let now = self.now_ms();
        self.refresh_triggered_existing(env, victim, r, now);
        let arm = TrigAppArm {
            victim,
            family: r.affected_family,
            mask: r.affected_mask,
            pct: r.pct,
            until: now + TRIG_APP_ARM_TTL_MS,
        };
        if let Some(a) = self.trig_arms.iter_mut().find(|a| {
            a.victim == victim && a.family == r.affected_family && a.mask == r.affected_mask
        }) {
            *a = arm;
            return;
        }
        self.trig_arms.retain(|a| now < a.until);
        if self.trig_arms.len() >= TRIG_APP_ARM_MAX {
            self.trig_arms.remove(0);
        }
        self.trig_arms.push(arm);
    }

    fn consume_triggered(&mut self, victim: u64, rec: &Row, now: u64) -> u32 {
        match self.trig_arms.iter().position(|a| {
            a.victim == victim && now < a.until && spells::fits_family(rec, a.family, a.mask)
        }) {
            Some(i) => self.trig_arms.remove(i).pct,
            None => 0,
        }
    }

    // ---- Combo-scaled durations ----

    /// The combo points `CMSG_CAST_SPELL` left with, snapshot per spell.
    pub fn capture_combo(&mut self, spell: u32, points: u8) {
        if spell == 0 {
            return;
        }
        let now = self.now_ms();
        let cap = ComboCapture {
            spell,
            points,
            at: now,
        };
        if let Some(c) = self.combo.iter_mut().flatten().find(|c| c.spell == spell) {
            *c = cap;
            return;
        }
        self.combo[self.combo_cursor] = Some(cap);
        self.combo_cursor = (self.combo_cursor + 1) % COMBO_CAPTURES;
    }

    fn consume_combo(&mut self, spell: u32, now: u64) -> u8 {
        let Some(slot) = self
            .combo
            .iter_mut()
            .find(|c| c.is_some_and(|c| c.spell == spell))
        else {
            return 0;
        };
        let got = slot.take().unwrap();
        if got.points == 0 || now - got.at >= COMBO_CAPTURE_TTL_MS {
            return 0;
        }
        got.points
    }

    /// `C_UnitAuras.RegisterComboDuration`'s table; false when full.
    pub fn register_combo_duration(&mut self, spell: u32, base_ms: i32, max_ms: i32) -> bool {
        if let Some(o) = self.combo_overrides.iter_mut().find(|o| o.0 == spell) {
            *o = (spell, base_ms, max_ms);
            return true;
        }
        if self.combo_overrides.len() >= MAX_COMBO_OVERRIDES {
            return false;
        }
        self.combo_overrides.push((spell, base_ms, max_ms));
        true
    }

    /// `TryComboScaledMs`: `base + (max - base) * cp / 5`, then the player's duration modifier;
    /// 0 when the cast carried no points or the row has no combo range.
    fn combo_scaled_ms(&mut self, env: &Env, rec: &Row, spell: u32, now: u64) -> u32 {
        let cp = i32::from(self.consume_combo(spell, now));
        if !(1..=5).contains(&cp) {
            return 0;
        }
        let (base, max) = if let Some(o) = self.combo_overrides.iter().find(|o| o.0 == spell) {
            (o.1, o.2)
        } else {
            let idx = rec.u32(col::DURATION_INDEX);
            match env.durations.and_then(|d| d.row(idx)) {
                Some(row) => (row.i32(1), row.i32(3)),
                None if idx != 0 => match turtle_dangling_combo(spell) {
                    Some(v) => v,
                    None => return 0,
                },
                None => return 0,
            }
        };
        if base < 0 || max <= 0 || base == max {
            return 0;
        }
        let ms = base + (max - base) * cp / 5;
        if ms <= 0 {
            return 0;
        }
        let ms =
            spellmod::apply(env.mods, env.family, rec, spellmod::OP_DURATION, ms as f32) as i32;
        u32::try_from(ms).unwrap_or(0)
    }

    // ---- The packets ----

    /// `HandleSpellGo`, after the succeeded events: duration edits, judgement attribution and
    /// triggered applications before the aura gate (their triggers apply no aura), then the
    /// cast's caster and timing on each hit target, the caster itself with no hit list.
    pub fn on_spell_go(&mut self, env: &Env, caster: u64, spell: u32, hits: &[u64]) {
        if caster == 0 || spell == 0 {
            return;
        }
        let targets = &hits[..hits.len().min(16)];
        self.apply_duration_mods(env, spell, caster, targets);
        if spell == SPELL_JUDGEMENT {
            let now = self.now_ms();
            for &t in targets {
                self.refresh_judgements_impl(env, t, caster, true);
                self.arm_judgement(t, caster, now);
            }
        }
        let rec = env.rec(spell);
        let by_player = caster == env.player();
        if !self.trig_apps.is_empty() && by_player {
            for &t in targets {
                self.arm_triggered(env, t, spell, rec.as_ref());
            }
        }
        let Some(rec) = rec.filter(applies_aura) else {
            return;
        };
        let now = self.now_ms();
        let mut duration = if by_player {
            self.combo_scaled_ms(env, &rec, spell, now)
        } else {
            0
        };
        if duration == 0 {
            duration = env.duration_ms(&rec, !by_player);
        }
        let expiration = if duration > 0 {
            now + u64::from(duration)
        } else {
            0
        };
        if by_player {
            self.remember_player_cast(spell, now);
        }
        if targets.is_empty() {
            self.store_from_cast(env, caster, spell, caster, expiration, duration);
            return;
        }
        for &t in targets {
            self.store_from_cast(env, t, spell, caster, expiration, duration);
        }
    }

    /// `StampApplication`: an aura landed or restacked in `slot`. A triggered-application arm
    /// names this exact target and wins; then the player's recent cast of the spell; then a
    /// judging paladin's arm; else base timing and no caster.
    pub fn stamp_application(
        &mut self,
        env: &Env,
        unit: u64,
        spell: u32,
        slot: usize,
        harmful: bool,
    ) {
        if spell == 0 || unit == 0 {
            return;
        }
        let Some(rec) = env.rec(spell).filter(applies_aura) else {
            return;
        };
        let now = self.now_ms();
        let mut caster = 0;
        let mut duration = 0;
        let pct = self.consume_triggered(unit, &rec, now);
        if pct != 0 {
            let base = env.duration_ms(&rec, false);
            if base > 0 {
                caster = env.player();
                duration = (base * pct / 100).max(1);
            }
        }
        if caster == 0 {
            let by_player = self.was_recent_player_cast(spell, now);
            duration = env.duration_ms(&rec, !by_player);
            caster = if by_player { env.player() } else { 0 };
            if caster == 0 && is_judgement(Some(&rec)) {
                caster = self.consume_judgement(unit, now);
            }
        }
        let expiration = if duration > 0 {
            now + u64::from(duration)
        } else {
            0
        };
        let kind = if harmful {
            Kind::Harmful
        } else {
            Kind::Helpful
        };
        self.store_from_application(
            env,
            unit,
            spell,
            caster,
            expiration,
            duration,
            slot as i16,
            kind,
        );
    }

    /// `OnAuraRemoved`: the slot went empty.
    pub fn on_aura_removed(&mut self, unit: u64, spell: u32, slot: usize) {
        self.evict(unit, spell, slot as i16);
    }

    /// `RestampPlayerChannel`: the channel's pushback, on every hit target's aura of it.
    pub fn restamp_player_channel(&mut self, player: u64, spell: u32, remaining_ms: u32) {
        if spell == 0 || remaining_ms == 0 || player == 0 {
            return;
        }
        let now = self.now_ms();
        let mut touched = Vec::new();
        for e in self.entries.iter_mut() {
            if e.caster == player && e.spell == spell {
                e.expiration = now + u64::from(remaining_ms);
                e.stamp = now;
                touched.push(e.target);
            }
        }
        for t in touched {
            self.signal(t);
        }
    }

    /// `RefreshDurationByFamily`: the caller's aura matching the selector, back to its own
    /// applied duration; the spell refreshed, 0 for none.
    pub fn refresh_by_family(
        &mut self,
        env: &Env,
        unit: u64,
        family: u32,
        mask: u64,
        icon: u32,
        caster: u64,
    ) -> u32 {
        if unit == 0 || (mask == 0 && icon == 0) || caster == 0 {
            return 0;
        }
        let now = self.now_ms();
        let hit = self.entries.of(unit).into_iter().find(|&i| {
            let e = &self.entries[i];
            (e.caster == 0 || e.caster == caster)
                && e.duration != 0
                && affected_matches(env.rec(e.spell).as_ref(), family, mask, icon)
        });
        let Some(i) = hit else {
            return 0;
        };
        let e = &mut self.entries[i];
        e.caster = caster;
        e.expiration = now + u64::from(e.duration);
        e.stamp = now;
        let spell = e.spell;
        self.signal(unit);
        spell
    }

    // ---- Reads ----

    /// `Get`: the caster, expiry and applied duration of the instance in `slot`, else of the
    /// unit's sole entry for the spell.
    pub fn get(&self, unit: u64, spell: u32, slot: i16) -> Option<(u64, u64, u32)> {
        if unit == 0 || spell == 0 {
            return None;
        }
        self.find_instance(unit, spell, slot).map(|i| {
            (
                self.entries[i].caster,
                self.entries[i].expiration,
                self.entries[i].duration,
            )
        })
    }

    /// `EvictAbsent`: drop each entry the unit's synced descriptor contradicts; an all-empty
    /// array, which cannot be told from out of range, reconciles nothing.
    pub fn evict_absent(&mut self, unit: u64, slots: &[u32; AURA_TOTAL]) {
        if unit == 0 || slots.iter().all(|s| *s == 0) {
            return;
        }
        let now = self.now_ms();
        for i in self.entries.of(unit) {
            let e = self.entries[i];
            if now - e.stamp < EVICT_GRACE_MS {
                continue;
            }
            let present = if e.slot >= 0 {
                slots[e.slot as usize] == e.spell
            } else {
                slots.contains(&e.spell)
            };
            if !present {
                self.entries.remove(i);
            }
        }
    }

    /// `Enumerate`: the unexpired entries on `unit` of one polarity, in cache order.
    pub fn enumerate(&self, unit: u64, harmful: bool) -> Vec<Entry> {
        let want = if harmful {
            Kind::Harmful
        } else {
            Kind::Helpful
        };
        let now = self.now_ms();
        self.entries
            .of(unit)
            .into_iter()
            .map(|i| self.entries[i])
            .filter(|e| e.kind == want && (e.expiration == 0 || now < e.expiration))
            .take(AURA_TOTAL)
            .collect()
    }

    /// `ObserveGroupAuras`: an out-of-range member's aura array; a spell newly present is stamped
    /// `now + base duration`, casterless. The first sight of a member only records it.
    pub fn observe_group(&mut self, env: &Env, guid: u64, arr: &[u16; AURA_TOTAL]) {
        if guid == 0 {
            return;
        }
        let now = self.now_ms();
        let Some((prev, _)) = self.group.get(&guid).copied() else {
            if self.group.len() >= GROUP_SNAPSHOTS {
                if let Some(old) = self.group.iter().min_by_key(|(_, s)| s.1).map(|(g, _)| *g) {
                    self.group.remove(&old);
                }
            }
            self.group.insert(guid, (*arr, now));
            return;
        };
        for (slot, &id) in arr.iter().enumerate() {
            if id == 0 || prev.contains(&id) {
                continue;
            }
            let Some(rec) = env.rec(u32::from(id)).filter(applies_aura) else {
                continue;
            };
            let base = env.duration_ms(&rec, true);
            if base == 0 {
                continue;
            }
            let kind = if slot < BUFF_COUNT {
                Kind::Helpful
            } else {
                Kind::Harmful
            };
            self.store_from_application(
                env,
                guid,
                u32::from(id),
                0,
                now + u64::from(base),
                base,
                SLOT_UNBOUND,
                kind,
            );
        }
        self.group.insert(guid, (*arr, now));
    }

    // ---- The tick ----

    /// `OnWorldTick`: flush on a map change, compress, reclaim expired entries no descriptor
    /// backs, forget group snapshots not polled for 30 s.
    pub fn tick(&mut self, env: &Env) {
        let map = env.mirror.map_id;
        if self.last_map.is_some_and(|m| m != map) {
            self.entries.clear();
            self.group.clear();
            self.compress.clear();
        }
        self.last_map = Some(map);
        let now = self.now_ms();
        self.apply_tick_compressions(env, now);
        // Timed entries run out unless their unit still lists them; an orphan's entry with no
        // duration goes too, as nothing would ever remove it (see `claim`'s deviation).
        self.entries.retain(|e| {
            let expired = e.expiration != 0 && now >= e.expiration;
            if Self::orphan(env, e) {
                return !(expired || e.expiration == 0);
            }
            !expired || env.descriptor_lists(e.target, e.spell)
        });
        self.group.retain(|_, s| now - s.1 <= GROUP_SNAPSHOT_TTL_MS);
        self.carnage_tick(env, now);
    }

    // ---- Turtle ----

    /// The Turtle server's built-in rules (`turtle/DurationMods.cpp`, `DarkHarvest.cpp`,
    /// `StingingNettle.cpp`), registered once Turtle is seen.
    pub fn register_turtle(&mut self, env: &Env) {
        if self.turtle_registered {
            return;
        }
        self.turtle_registered = true;
        const WARLOCK: u32 = 5;
        const SHAMAN: u32 = 11;
        const HUNTER: u32 = 9;
        for id in [17962, 18930, 18931, 18932] {
            self.register_duration_mod(id, 0, -1, WARLOCK, 0x4, 0, ModOp::Reduce, 3000);
        }
        for id in [36916, 36917, 36918, 36919, 36920, 36921] {
            self.register_duration_mod(id, 0, -1, SHAMAN, 0x1000_0000, 0, ModOp::Refresh, 0);
        }
        let bw = env
            .rec(19574)
            .map(|r| env.duration_ms(&r, true) as i32)
            .filter(|ms| *ms > 0)
            .unwrap_or(18000);
        self.register_duration_mod(19574, 0, -1, HUNTER, 0, 2245, ModOp::Set, bw);

        let harvest = env
            .rec(52550)
            .map(|r| r.i32(col::EFFECT_BASE_POINTS + 1) + 1)
            .filter(|p| *p < 0)
            .map_or(30, |p| p.unsigned_abs());
        let affliction = 0x2 | 0x8 | 0x400 | 0x4000 | 0x1_0000_0000 | 0x2_0000_0000;
        self.add_tick_compression(WARLOCK, 0x40_0000_0000, WARLOCK, affliction, harvest);

        let nettle = |id: u32, fallback: u32| {
            env.rec(id)
                .map(|r| r.i32(col::EFFECT_BASE_POINTS) + 1)
                .filter(|p| *p > 0)
                .map_or(fallback, |p| p as u32)
        };
        let (r2, r1) = (nettle(51580, 40), nettle(51579, 20));
        const SERPENT_STING: u64 = 0x4000;
        self.add_triggered_application(0, HUNTER, 0x4, 51580, HUNTER, SERPENT_STING, r2);
        self.add_triggered_application(0, HUNTER, 0x4, 51579, HUNTER, SERPENT_STING, r1);
        for id in [1495, 14269, 14270, 14271] {
            self.add_triggered_application(id, 0, 0, 51580, HUNTER, SERPENT_STING, r2);
            self.add_triggered_application(id, 0, 0, 51579, HUNTER, SERPENT_STING, r1);
        }
    }

    /// Turtle's Carnage (`turtle/Carnage.cpp`): a Ferocious Bite that leaves combo points behind
    /// refreshed the player's Rip and Rake on its target. Armed by the send for 500 ms.
    pub fn carnage_arm(&mut self, env: &Env, spell: u32) {
        const DRUID: u32 = 7;
        let Some(rec) = env.rec(spell) else {
            return;
        };
        let bite = spells::fits_family(&rec, DRUID, 0x80_0000)
            && (0..spells::EFFECTS).any(|i| rec.u32(col::EFFECT + i) == 2);
        if !bite {
            return;
        }
        let Some(me) = env.mirror.me() else {
            return;
        };
        let target = me.guid(field::PLAYER_COMBO_TARGET);
        let points = me.byte(field::PLAYER_BYTES_FIELD, 1);
        self.carnage = Some((target, self.now_ms() + 500, points));
    }

    fn carnage_tick(&mut self, env: &Env, now: u64) {
        let Some((target, until, last)) = self.carnage else {
            return;
        };
        if now >= until {
            self.carnage = None;
            return;
        }
        let Some(cp) = env
            .mirror
            .me()
            .map(|f| f.byte(field::PLAYER_BYTES_FIELD, 1))
        else {
            return;
        };
        if cp == last {
            return;
        }
        self.carnage = Some((target, until, cp));
        if cp > 0 {
            let me = env.player();
            self.refresh_by_family(env, target, 7, 0x80_0000, 108, me);
            self.refresh_by_family(env, target, 7, 0x1000, 494, me);
            self.carnage = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOSS: u64 = 0xF130_0000_0000_0001;
    const ME: u64 = 0x10;
    const OTHER: u64 = 0x20;

    fn spells() -> Dbc {
        // Spell 172 (a DoT, family 5) with duration index 1 (18 s).
        let mut row = vec![0u32; 0x2B4 / 4];
        row[0] = 172;
        row[col::DURATION_INDEX] = 1;
        row[col::EFFECT_APPLY_AURA_NAME] = AURA_PERIODIC_DAMAGE as u32;
        row[col::FAMILY_NAME] = 5;
        row[col::FAMILY_FLAGS] = 0x2;
        Dbc::from_rows(&[row], b"\0")
    }

    fn durations() -> Dbc {
        Dbc::from_rows(&[vec![1, 18000, 0, 18000]], b"\0")
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

    #[test]
    fn two_casters_of_one_spell_are_two_entries_and_each_application_seats_in_cast_order() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let mut m = Mirror::default();
        m.player = ME;
        let env = env(&s, &d, &m, &t);
        let mut src = Source::default();
        src.on_spell_go(&env, ME, 172, &[BOSS]);
        src.on_spell_go(&env, OTHER, 172, &[BOSS]);
        src.stamp_application(&env, BOSS, 172, 32, true);
        src.stamp_application(&env, BOSS, 172, 33, true);
        assert_eq!(src.get(BOSS, 172, 32).map(|g| g.0), Some(ME));
        assert_eq!(src.get(BOSS, 172, 33).map(|g| g.0), Some(OTHER));
        // Unbound and ambiguous: a miss, never a coin flip.
        assert_eq!(src.get(BOSS, 172, SLOT_UNBOUND), None);
        // One copy falling off retires only its own entry.
        src.on_aura_removed(BOSS, 172, 32);
        assert_eq!(src.get(BOSS, 172, SLOT_UNBOUND).map(|g| g.0), Some(OTHER));
    }

    #[test]
    fn a_cast_applies_its_duration_and_a_reduce_rule_shaves_it() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let mut m = Mirror::default();
        m.player = ME;
        let env = env(&s, &d, &m, &t);
        let mut src = Source::default();
        assert!(src.register_duration_mod(999, 0, -1, 5, 0x2, 0, ModOp::Reduce, 3000));
        src.on_spell_go(&env, ME, 172, &[BOSS]);
        let (_, exp, dur) = src.get(BOSS, 172, SLOT_UNBOUND).unwrap();
        assert_eq!(dur, 18000);
        src.on_spell_go(&env, ME, 999, &[BOSS]);
        let (_, after, _) = src.get(BOSS, 172, SLOT_UNBOUND).unwrap();
        assert_eq!(exp - after, 3000);
        assert!(src.take_signals().contains(&BOSS));
    }

    #[test]
    fn evict_absent_ignores_an_empty_array_and_a_fresh_capture() {
        let (s, d, t) = (spells(), durations(), spellmod::Tables::default());
        let m = Mirror::default();
        let env = env(&s, &d, &m, &t);
        let mut src = Source::default();
        src.on_spell_go(&env, OTHER, 172, &[BOSS]);
        let mut slots = [0u32; AURA_TOTAL];
        src.evict_absent(BOSS, &slots);
        slots[0] = 1;
        src.evict_absent(BOSS, &slots);
        assert!(src.get(BOSS, 172, SLOT_UNBOUND).is_some());
    }
}

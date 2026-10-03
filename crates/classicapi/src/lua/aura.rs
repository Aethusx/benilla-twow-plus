//! `aura/Api.cpp` and `aura/Data.cpp`: `C_UnitAuras`, modern aura data over the 1.12 descriptor.
//!
//! A held unit's auras are its `UNIT_FIELD_AURA` slots; an out-of-range groupmate's are the
//! roster's aura block. Each is enriched from [`crate::aura::Source`] with the caster and timing
//! the descriptor never carries, and when a held unit's descriptor has been cleared wholesale
//! (stealth, party range) the cache itself answers after the descriptor's entries.
//!
//! Filters tokenize the modern `AuraFilters` string on `|` and whitespace, each token optionally
//! negated by `!`. Honoured: `HELPFUL`/`HARMFUL`, `PLAYER` (cast by the player or the pet),
//! `DISPELLABLE` (Magic, Curse, Disease, Poison) and `CROWD_CONTROL`; every other token is
//! accepted and ignored.

use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use super::unit::unit_guid;
use crate::aura::{self, Entry, Env, Source, AURA_TOTAL, BUFF_COUNT, SLOT_UNBOUND};
use crate::guid::{self, Kind};
use crate::lua::{as_string, is_number, to_int, to_number, Api};
use crate::mirror::{field, typemask, Fields};
use crate::spells::{self, col};
use crate::Ca;

/// `Data::OPAQUE_STRIDE`: an opaque slot id is `kind * 0x100 + k`. Kind 0 a descriptor slot, 1/2
/// the k-th helpful/harmful cache fallback, 3 a group-array slot, 4/5 the k-th helpful/harmful
/// group cache fallback.
const OPAQUE_STRIDE: i64 = 0x100;
const OPAQUE_CACHE_HELPFUL: i64 = OPAQUE_STRIDE;
const OPAQUE_CACHE_HARMFUL: i64 = 2 * OPAQUE_STRIDE;
const OPAQUE_GROUP: i64 = 3 * OPAQUE_STRIDE;
const OPAQUE_GROUP_CACHE_HELPFUL: i64 = 4 * OPAQUE_STRIDE;
const OPAQUE_GROUP_CACHE_HARMFUL: i64 = 5 * OPAQUE_STRIDE;
const OPAQUE_SLOTS_MAX: usize = 2 * AURA_TOTAL;

/// `C_UIColor`'s debuff-type colours, `ui/ColorData.h`'s packed ARGB, for when the colour
/// globals are not up.
const DEBUFF_COLORS: [(&str, u32); 7] = [
    ("DEBUFF_TYPE_MAGIC_COLOR", 0xFF00_81FF),
    ("DEBUFF_TYPE_POISON_COLOR", 0xFF7B_C700),
    ("DEBUFF_TYPE_CURSE_COLOR", 0xFF9F_06E4),
    ("DEBUFF_TYPE_BLEED_COLOR", 0xFFB8_000F),
    ("DEBUFF_TYPE_DISEASE_COLOR", 0xFFF1_6A09),
    ("DEBUFF_TYPE_NONE_COLOR", 0xFFCC_0000),
    ("DEBUFF_TYPE_ENRAGE_COLOR", 0xFFFF_8C00),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Filter {
    Helpful,
    Harmful,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Emit {
    Table,
    Positional,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Mode {
    #[default]
    Any,
    Only,
    Not,
}

/// The caller's caster, dispel and crowd-control restrictions.
#[derive(Clone, Copy, Debug, Default)]
struct Match {
    caster: Mode,
    dispel: Mode,
    cc: Mode,
}

#[derive(Debug, Default)]
struct Parsed {
    helpful: bool,
    harmful: bool,
    m: Match,
}

impl Parsed {
    /// The one range an indexed getter reads: harmful when only `HARMFUL` was given.
    fn range(&self) -> Filter {
        if self.harmful && !self.helpful {
            Filter::Harmful
        } else {
            Filter::Helpful
        }
    }

    /// The by-id and by-name getters' range: none when no range token was given.
    fn range_opt(&self) -> Option<Filter> {
        (self.helpful || self.harmful).then(|| self.range())
    }
}

/// `ParseFilters`: case-sensitive whole tokens; `!` negates `PLAYER`, `DISPELLABLE` and
/// `CROWD_CONTROL`, and is meaningless on the range tokens.
fn parse_filters(filter: Option<&str>) -> Parsed {
    let mut out = Parsed::default();
    let Some(filter) = filter else {
        return out;
    };
    for raw in filter.split(['|', ' ', '\t']).filter(|t| !t.is_empty()) {
        let (negate, tok) = match raw.strip_prefix('!') {
            Some(t) => (true, t),
            None => (false, raw),
        };
        let mode = if negate { Mode::Not } else { Mode::Only };
        match tok {
            "HELPFUL" => out.helpful = true,
            "HARMFUL" => out.harmful = true,
            "PLAYER" => out.m.caster = mode,
            "DISPELLABLE" => out.m.dispel = mode,
            "CROWD_CONTROL" => out.m.cc = mode,
            _ => {}
        }
    }
    out
}

fn mode_matches(mode: Mode, yes: bool) -> bool {
    match mode {
        Mode::Any => true,
        Mode::Only => yes,
        Mode::Not => !yes,
    }
}

/// One aura as read, before it is turned into Lua values.
#[derive(Clone, Copy, Debug)]
struct Got {
    spell: u32,
    helpful: bool,
    stacks: u32,
    caster: u64,
    /// The cache's expiry on the `GetTime()` clock, 0 unknown or elapsed.
    cache_expiration: f64,
    /// The applied duration, ms; 0 falls back to the spell's base duration.
    duration_ms: u32,
}

/// The unit a token names, as the aura paths read it.
enum Target {
    /// A held unit, its descriptor.
    Live(u64),
    /// An out-of-range groupmate: its roster aura block as a 48-slot array, and its level.
    Group(u64, [u16; AURA_TOTAL], u32),
    Nobody,
}

/// `ResolveUnit`, then `GuidForOutOfRange`: a token no grammar knows raises.
fn target(lua: &Lua, ca: &Ca, token: &str) -> mlua::Result<Target> {
    let Some(guid) = unit_guid(lua, token)? else {
        return Ok(Target::Nobody);
    };
    let live = ca
        .lock()
        .mirror
        .object(guid)
        .is_some_and(|f| f.is(typemask::UNIT));
    if live {
        return Ok(Target::Live(guid));
    }
    let Some(list) = benilla_ui::script::ext_read::unit_aura_list(lua, guid) else {
        return Ok(Target::Nobody);
    };
    let mut arr = [0u16; AURA_TOTAL];
    let (mut buff, mut debuff) = (0, BUFF_COUNT);
    for (spell, helpful) in list {
        let slot = if helpful { &mut buff } else { &mut debuff };
        let limit = if helpful { BUFF_COUNT } else { AURA_TOTAL };
        if *slot < limit {
            arr[*slot] = spell as u16;
            *slot += 1;
        }
    }
    let level = benilla_ui::script::ext_read::unit_state(lua, token)?.map_or(0, |s| s.level);
    Ok(Target::Group(guid, arr, level))
}

// ---- The read side, under the lock ----

/// The pieces every read needs: the cache, its environment and the unit's descriptor.
struct Reader<'a, 'e> {
    src: &'a mut Source,
    env: &'a Env<'e>,
    guid: u64,
}

impl Reader<'_, '_> {
    fn visible(&self, spell: u32) -> bool {
        self.env
            .spells
            .and_then(|t| t.row(spell))
            .is_some_and(|r| spells::aura_visible(&r))
    }

    /// `IsSlotPopulated`: a spell, a visible flag nibble, a visible spell.
    fn populated(&self, f: &Fields, slot: usize) -> bool {
        let spell = aura::slot_spell(f, slot);
        spell != 0 && aura::slot_nibble(f, slot) & aura::VISIBLE_MASK != 0 && self.visible(spell)
    }

    /// `Attribute`: the cache's caster and timing, else the unit itself for a self-only buff.
    fn attribute(&self, spell: u32, slot: i16) -> (u64, u64, u32) {
        let mut a = self.src.get(self.guid, spell, slot).unwrap_or((0, 0, 0));
        if a.0 == 0 && self.self_buff(spell) {
            a.0 = self.guid;
        }
        a
    }

    fn self_buff(&self, spell: u32) -> bool {
        self.env
            .spells
            .and_then(|t| t.row(spell))
            .is_some_and(|r| spells::self_buff(&r))
    }

    /// `IsPlayerOrPetCaster`; a 0 caster is a miss, never a match.
    fn player_or_pet(&self, caster: u64) -> bool {
        let m = self.env.mirror;
        caster != 0
            && (caster == m.player
                || m.me()
                    .map(|f| f.guid(field::UNIT_SUMMON))
                    .is_some_and(|p| p != 0 && p == caster))
    }

    /// `MatchesAura`'s dispel and crowd-control legs.
    fn spell_matches(&self, m: &Match, spell: u32) -> bool {
        let rec = self.env.spells.and_then(|t| t.row(spell));
        let dispellable = rec
            .as_ref()
            .is_some_and(|r| (1..=4).contains(&r.u32(col::DISPEL)));
        let cc = rec.as_ref().is_some_and(spells::is_crowd_control);
        mode_matches(m.dispel, dispellable) && mode_matches(m.cc, cc)
    }

    /// `PopulatedSlotMatches`: the caster lookup only when the match restricts on it.
    fn populated_slot_matches(&self, f: &Fields, slot: usize, m: &Match) -> bool {
        let spell = aura::slot_spell(f, slot);
        let by_player =
            m.caster != Mode::Any && self.player_or_pet(self.attribute(spell, slot as i16).0);
        mode_matches(m.caster, by_player) && self.spell_matches(m, spell)
    }

    fn slot_matches_filter(&mut self, f: &Fields, slot: usize, filter: Filter, m: &Match) -> bool {
        self.populated(f, slot)
            && self.src.slot_harmful(f, slot) == (filter == Filter::Harmful)
            && self.populated_slot_matches(f, slot, m)
    }

    fn find_nth(&mut self, f: &Fields, n: i64, filter: Filter, m: &Match) -> Option<usize> {
        if n < 1 {
            return None;
        }
        let mut seen = 0;
        for i in 0..AURA_TOTAL {
            let slot = in_filter_order(filter, i);
            if self.slot_matches_filter(f, slot, filter, m) {
                seen += 1;
                if seen == n {
                    return Some(slot);
                }
            }
        }
        None
    }

    /// `FindSlotBySpellID` / `FindSlotBySpellName`: the first populated slot whose spell passes
    /// `which`, in the filter's order when one is given.
    fn find_by(
        &mut self,
        f: &Fields,
        which: &dyn Fn(u32) -> bool,
        filter: Option<Filter>,
        m: &Match,
    ) -> Option<usize> {
        for i in 0..AURA_TOTAL {
            let slot = filter.map_or(i, |fl| in_filter_order(fl, i));
            if !self.populated(f, slot) || !which(aura::slot_spell(f, slot)) {
                continue;
            }
            if filter.is_some_and(|fl| self.src.slot_harmful(f, slot) != (fl == Filter::Harmful)) {
                continue;
            }
            if self.populated_slot_matches(f, slot, m) {
                return Some(slot);
            }
        }
        None
    }

    /// The cache expiry on the `GetTime()` clock, 0 when unknown or elapsed: a non-player
    /// caster's estimate can run out while the aura is still up.
    fn ui_expiration(&self, ms: u64) -> f64 {
        if ms == 0 || ms <= self.src.now_ms() {
            return 0.0;
        }
        self.env.mirror.ui_time(self.src.instant(ms))
    }

    /// `Push` for a descriptor slot.
    fn got_slot(&mut self, f: &Fields, slot: usize) -> Got {
        let spell = aura::slot_spell(f, slot);
        let helpful = !self.src.slot_harmful(f, slot);
        let (caster, exp, dur) = self.attribute(spell, slot as i16);
        Got {
            spell,
            helpful,
            stacks: aura::slot_stacks(f, slot),
            caster,
            cache_expiration: self.ui_expiration(exp),
            duration_ms: dur,
        }
    }

    /// A cache entry, its attribution known; one application, as `SPELL_GO` carries no stacks.
    fn got_cached(&self, e: &Entry, helpful: bool) -> Got {
        let caster = if e.caster == 0 && self.self_buff(e.spell) {
            self.guid
        } else {
            e.caster
        };
        Got {
            spell: e.spell,
            helpful,
            stacks: 1,
            caster,
            cache_expiration: self.ui_expiration(e.expiration),
            duration_ms: e.duration,
        }
    }

    fn cached_matches(&self, e: &Entry, m: &Match) -> bool {
        mode_matches(m.caster, self.player_or_pet(e.caster)) && self.spell_matches(m, e.spell)
    }

    /// `EligibleFallbacks`: reconcile against the synced descriptor, then the cache's unexpired
    /// visible entries of the polarity, but only while the descriptor shows no visible aura.
    fn eligible_fallbacks(&mut self, f: &Fields, filter: Filter) -> Vec<Entry> {
        let mut present = [0u32; AURA_TOTAL];
        for (slot, p) in present.iter_mut().enumerate() {
            *p = aura::slot_spell(f, slot);
        }
        self.src.evict_absent(self.guid, &present);
        if (0..AURA_TOTAL).any(|s| self.populated(f, s)) {
            return Vec::new();
        }
        self.src
            .enumerate(self.guid, filter == Filter::Harmful)
            .into_iter()
            .filter(|e| self.visible(e.spell))
            .collect()
    }

    // ---- The out-of-range group array ----

    fn group_visible(&self, arr: &[u16; AURA_TOTAL], slot: usize) -> bool {
        arr[slot] != 0 && self.visible(u32::from(arr[slot]))
    }

    fn group_matches(&self, arr: &[u16; AURA_TOTAL], slot: usize, m: &Match) -> bool {
        if !self.group_visible(arr, slot) {
            return false;
        }
        let id = u32::from(arr[slot]);
        let by_player =
            m.caster != Mode::Any && self.player_or_pet(self.attribute(id, SLOT_UNBOUND).0);
        mode_matches(m.caster, by_player) && self.spell_matches(m, id)
    }

    fn got_group(&self, spell: u32, helpful: bool) -> Got {
        let (caster, exp, dur) = self.attribute(spell, SLOT_UNBOUND);
        Got {
            spell,
            helpful,
            stacks: 1,
            caster,
            cache_expiration: self.ui_expiration(exp),
            duration_ms: dur,
        }
    }

    /// `EligibleGroupFallbacks`: the cache, only while the array shows nothing visible (the
    /// server resends out-of-range stats as deltas, so a member seen in range leaves it empty).
    fn eligible_group_fallbacks(&self, arr: &[u16; AURA_TOTAL], harmful: bool) -> Vec<Entry> {
        if (0..AURA_TOTAL).any(|s| self.group_visible(arr, s)) {
            return Vec::new();
        }
        self.src
            .enumerate(self.guid, harmful)
            .into_iter()
            .filter(|e| self.visible(e.spell) && !arr.contains(&(e.spell as u16)))
            .collect()
    }
}

/// `SlotInFilterOrder`: harmful reads 32..47 then 0..31, where a full harmful range spills.
fn in_filter_order(filter: Filter, i: usize) -> usize {
    match filter {
        Filter::Harmful => (i + BUFF_COUNT) % AURA_TOTAL,
        Filter::Helpful => i,
    }
}

fn group_range(filter: Filter) -> std::ops::Range<usize> {
    match filter {
        Filter::Harmful => BUFF_COUNT..AURA_TOTAL,
        Filter::Helpful => 0..BUFF_COUNT,
    }
}

/// Run `f` over the target's auras under the lock. `None` when the unit is not there.
fn read<R>(ca: &Ca, guid: u64, f: impl FnOnce(&mut Reader, Option<&Fields>) -> R) -> R {
    ca.with_auras(|src, env| {
        let fields = env
            .mirror
            .object(guid)
            .filter(|f| f.is(typemask::UNIT))
            .cloned();
        let mut r = Reader { src, env, guid };
        f(&mut r, fields.as_ref())
    })
}

/// The group array's reads open by observing it, so a just-appeared aura carries its guess.
fn read_group<R>(
    ca: &Ca,
    guid: u64,
    arr: &[u16; AURA_TOTAL],
    f: impl FnOnce(&mut Reader) -> R,
) -> R {
    ca.with_auras(|src, env| {
        src.observe_group(env, guid, arr);
        let mut r = Reader { src, env, guid };
        f(&mut r)
    })
}

/// The n-th aura matching the filter: descriptor slots, then the cache fallback.
fn nth(ca: &Ca, t: &Target, n: i64, filter: Filter, m: &Match) -> Option<Got> {
    match t {
        Target::Live(guid) => read(ca, *guid, |r, f| {
            let f = f?;
            if let Some(slot) = r.find_nth(f, n, filter, m) {
                return Some(r.got_slot(f, slot));
            }
            let fb = r.eligible_fallbacks(f, filter);
            fb.iter()
                .filter(|e| r.cached_matches(e, m))
                .nth(usize::try_from(n - 1).ok()?)
                .map(|e| r.got_cached(e, filter == Filter::Helpful))
        }),
        Target::Group(guid, arr, _) => read_group(ca, *guid, arr, |r| {
            if n < 1 {
                return None;
            }
            let idx = usize::try_from(n - 1).ok()?;
            let hit = group_range(filter)
                .filter(|&s| r.group_matches(arr, s, m))
                .nth(idx);
            if let Some(s) = hit {
                return Some(r.got_group(u32::from(arr[s]), filter == Filter::Helpful));
            }
            let fb = r.eligible_group_fallbacks(arr, filter == Filter::Harmful);
            fb.iter()
                .filter(|e| r.cached_matches(e, m))
                .nth(idx)
                .map(|e| r.got_cached(e, filter == Filter::Helpful))
        }),
        Target::Nobody => None,
    }
}

/// The first aura whose spell passes `which`: descriptor slots for a held unit; the array then
/// its cache fallback, helpful before harmful, for a groupmate.
fn by_spell(
    ca: &Ca,
    t: &Target,
    which: &dyn Fn(u32) -> bool,
    filter: Option<Filter>,
    m: &Match,
) -> Option<Got> {
    match t {
        Target::Live(guid) => read(ca, *guid, |r, f| {
            let f = f?;
            let slot = r.find_by(f, which, filter, m)?;
            Some(r.got_slot(f, slot))
        }),
        Target::Group(guid, arr, _) => read_group(ca, *guid, arr, |r| {
            let start = if filter == Some(Filter::Harmful) {
                BUFF_COUNT
            } else {
                0
            };
            let end = if filter == Some(Filter::Helpful) {
                BUFF_COUNT
            } else {
                AURA_TOTAL
            };
            for slot in start..end {
                let id = u32::from(arr[slot]);
                if id != 0 && which(id) && r.group_matches(arr, slot, m) {
                    return Some(r.got_group(id, slot < BUFF_COUNT));
                }
            }
            for harmful in [false, true] {
                if filter.is_some_and(|fl| (fl == Filter::Harmful) != harmful) {
                    continue;
                }
                let fb = r.eligible_group_fallbacks(arr, harmful);
                if let Some(e) = fb.iter().find(|e| which(e.spell) && r.cached_matches(e, m)) {
                    return Some(r.got_cached(e, !harmful));
                }
            }
            None
        }),
        Target::Nobody => None,
    }
}

/// Every aura of one polarity in getter order, the bulk read behind `GetUnitAuras`.
fn all(ca: &Ca, t: &Target, filter: Filter, m: &Match) -> Vec<Got> {
    match t {
        Target::Live(guid) => read(ca, *guid, |r, f| {
            let Some(f) = f else {
                return Vec::new();
            };
            let mut out: Vec<Got> = Vec::new();
            for i in 0..AURA_TOTAL {
                let slot = in_filter_order(filter, i);
                if r.slot_matches_filter(f, slot, filter, m) {
                    out.push(r.got_slot(f, slot));
                }
            }
            let fb = r.eligible_fallbacks(f, filter);
            out.extend(
                fb.iter()
                    .filter(|e| r.cached_matches(e, m))
                    .map(|e| r.got_cached(e, filter == Filter::Helpful)),
            );
            out
        }),
        Target::Group(guid, arr, _) => read_group(ca, *guid, arr, |r| {
            let mut out: Vec<Got> = group_range(filter)
                .filter(|&s| r.group_matches(arr, s, m))
                .map(|s| r.got_group(u32::from(arr[s]), filter == Filter::Helpful))
                .collect();
            let fb = r.eligible_group_fallbacks(arr, filter == Filter::Harmful);
            out.extend(
                fb.iter()
                    .filter(|e| r.cached_matches(e, m))
                    .map(|e| r.got_cached(e, filter == Filter::Helpful)),
            );
            out
        }),
        Target::Nobody => Vec::new(),
    }
}

/// `CollectSlots`: the opaque ids of every matching aura, in the by-index getters' order.
fn collect_slots(ca: &Ca, t: &Target, filter: Filter, m: &Match) -> Vec<i64> {
    let cache_base = |helpful: i64, harmful: i64| {
        if filter == Filter::Harmful {
            harmful
        } else {
            helpful
        }
    };
    match t {
        Target::Live(guid) => read(ca, *guid, |r, f| {
            let Some(f) = f else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for i in 0..AURA_TOTAL {
                let slot = in_filter_order(filter, i);
                if r.slot_matches_filter(f, slot, filter, m) {
                    out.push(slot as i64);
                }
            }
            let base = cache_base(OPAQUE_CACHE_HELPFUL, OPAQUE_CACHE_HARMFUL);
            let fb = r.eligible_fallbacks(f, filter);
            for (k, e) in fb.iter().enumerate() {
                if out.len() < OPAQUE_SLOTS_MAX && r.cached_matches(e, m) {
                    out.push(base + k as i64);
                }
            }
            out.truncate(OPAQUE_SLOTS_MAX);
            out
        }),
        Target::Group(guid, arr, _) => read_group(ca, *guid, arr, |r| {
            let mut out: Vec<i64> = group_range(filter)
                .filter(|&s| r.group_matches(arr, s, m))
                .map(|s| OPAQUE_GROUP + s as i64)
                .collect();
            let base = cache_base(OPAQUE_GROUP_CACHE_HELPFUL, OPAQUE_GROUP_CACHE_HARMFUL);
            let fb = r.eligible_group_fallbacks(arr, filter == Filter::Harmful);
            for (k, e) in fb.iter().enumerate() {
                if out.len() < OPAQUE_SLOTS_MAX && r.cached_matches(e, m) {
                    out.push(base + k as i64);
                }
            }
            out.truncate(OPAQUE_SLOTS_MAX);
            out
        }),
        Target::Nobody => Vec::new(),
    }
}

/// `PushBySlot`: the aura an opaque id names, if it still names one.
fn by_opaque(ca: &Ca, t: &Target, id: i64) -> Option<Got> {
    if id < 0 {
        return None;
    }
    let (kind, k) = (
        id / OPAQUE_STRIDE,
        usize::try_from(id % OPAQUE_STRIDE).ok()?,
    );
    match (kind, t) {
        (0, Target::Live(guid)) => read(ca, *guid, |r, f| {
            let f = f?;
            (k < AURA_TOTAL && r.populated(f, k)).then(|| r.got_slot(f, k))
        }),
        (1 | 2, Target::Live(guid)) => read(ca, *guid, |r, f| {
            let f = f?;
            let filter = if kind == 2 {
                Filter::Harmful
            } else {
                Filter::Helpful
            };
            let fb = r.eligible_fallbacks(f, filter);
            fb.get(k)
                .map(|e| r.got_cached(e, filter == Filter::Helpful))
        }),
        (3, Target::Group(guid, arr, _)) => read_group(ca, *guid, arr, |r| {
            (k < AURA_TOTAL && r.group_visible(arr, k))
                .then(|| r.got_group(u32::from(arr[k]), k < BUFF_COUNT))
        }),
        (4 | 5, Target::Group(guid, arr, _)) => ca.with_auras(|src, env| {
            let r = Reader {
                src,
                env,
                guid: *guid,
            };
            let harmful = kind == 5;
            let fb = r.eligible_group_fallbacks(arr, harmful);
            fb.get(k).map(|e| r.got_cached(e, !harmful))
        }),
        _ => None,
    }
}

// ---- The Lua side ----

/// `SpellBaseDurationSeconds`: the `SpellDuration.dbc` row scaled by the unit's level (0 skips
/// the scaling), 0 for none or infinite.
fn base_duration(ca: &Ca, spell: u32, level: u32) -> f64 {
    let Some(rec) = spells::table(&ca.db).and_then(|t| {
        t.row(spell)
            .map(|r| (r.u32(col::DURATION_INDEX), r.i32(col::BASE_LEVEL)))
    }) else {
        return 0.0;
    };
    let (idx, base_level) = rec;
    if idx == 0 {
        return 0.0;
    }
    let Some((base, per, max)) = ca
        .db
        .get("SpellDuration")
        .and_then(|d| d.row(idx).map(|r| (r.i32(1), r.i32(2), r.i32(3))))
    else {
        return 0.0;
    };
    if base < 0 && per < 1 {
        return 0.0;
    }
    let eff = (level as i32).max(base_level);
    let mut ms = (eff - base_level) * per + base;
    if max > 0 && ms > max {
        ms = max;
    }
    if ms <= 0 {
        0.0
    } else {
        f64::from(ms) * 0.001
    }
}

/// `ResolveDisplay`: the localized name, the icon path, the dispel type's name.
fn display(ca: &Ca, spell: u32) -> (Option<String>, Option<String>, Option<String>) {
    let Some(table) = spells::table(&ca.db) else {
        return (None, None, None);
    };
    let Some(rec) = table.row(spell) else {
        return (None, None, None);
    };
    let name = Some(rec.loc(col::NAME).to_string()).filter(|s| !s.is_empty());
    let icon = spells::spell_icon(&ca.db, &rec, false);
    let dispel = match rec.u32(col::DISPEL) {
        0 => None,
        d => ca
            .db
            .get("SpellDispelType")
            .and_then(|t| t.row(d).map(|r| r.loc(1).to_string()))
            .filter(|s| !s.is_empty()),
    };
    (name, icon, dispel)
}

fn get_time(lua: &Lua) -> f64 {
    lua.globals()
        .get::<mlua::Function>("GetTime")
        .and_then(|f| f.call::<f64>(()))
        .unwrap_or(0.0)
}

/// `PushEnriched`: the player's own timer from its duration packets (none once elapsed, as
/// `GetPlayerBuffTimeLeft` reads it), else the cache's; the applied duration when the cache has
/// it, else the base.
fn emit(
    lua: &Lua,
    ca: &Ca,
    g: &Got,
    level: u32,
    is_player: bool,
    how: Emit,
) -> mlua::Result<MultiValue> {
    let mut duration = base_duration(ca, g.spell, level);
    let mut expiration = 0.0;
    if is_player {
        let now = get_time(lua);
        if let Some(e) = benilla_ui::script::ext_read::player_aura_expiration(lua, g.spell) {
            if e > now {
                expiration = e;
            }
        }
    }
    if expiration == 0.0 {
        expiration = g.cache_expiration;
    }
    if g.duration_ms != 0 {
        duration = f64::from(g.duration_ms) * 0.001;
    }
    let (name, icon, dispel) = display(ca, g.spell);
    let by_player = matches!(guid::classify(g.caster), Kind::Player | Kind::Pet) && g.caster != 0;
    let source = super::unit::identity::token_from_guid(lua, ca, g.caster);
    match how {
        Emit::Positional => (
            name.unwrap_or_default(),
            icon.unwrap_or_default(),
            g.stacks,
            dispel.unwrap_or_default(),
            duration,
            expiration,
            source,
            false,
            false,
            g.spell,
            false,
            false,
            by_player,
            false,
            1,
        )
            .into_lua_multi(lua),
        Emit::Table => {
            let t = lua.create_table()?;
            t.set("name", name.unwrap_or_default())?;
            t.set("icon", icon.unwrap_or_default())?;
            t.set("applications", g.stacks)?;
            t.set("spellId", g.spell)?;
            t.set("dispelName", dispel.unwrap_or_default())?;
            t.set("isHelpful", g.helpful)?;
            t.set("isHarmful", !g.helpful)?;
            t.set("duration", duration)?;
            t.set("expirationTime", expiration)?;
            if g.caster != 0 {
                t.set("sourceUnit", source)?;
                t.set("sourceGUID", guid::format(g.caster))?;
            }
            t.set("charges", 0)?;
            t.set("maxCharges", 0)?;
            t.set("timeMod", 1)?;
            t.set("isFromPlayerOrPlayerPet", by_player)?;
            for k in [
                "isStealable",
                "isBossAura",
                "isNameplateOnly",
                "nameplateShowAll",
                "nameplateShowPersonal",
                "canApplyAura",
                "shouldConsolidate",
                "isRaid",
            ] {
                t.set(k, false)?;
            }
            t.into_lua_multi(lua)
        }
    }
}

/// The unit level the base duration scales by, and whether the target is the player.
fn context(ca: &Ca, t: &Target) -> (u32, bool) {
    match t {
        Target::Live(g) => {
            let st = ca.lock();
            (
                st.mirror.object(*g).map_or(0, |f| f.level()),
                *g == st.mirror.player,
            )
        }
        Target::Group(_, _, level) => (*level, false),
        Target::Nobody => (0, false),
    }
}

fn emit_or_nil(
    lua: &Lua,
    ca: &Ca,
    t: &Target,
    g: Option<Got>,
    how: Emit,
) -> mlua::Result<MultiValue> {
    match g {
        Some(g) => {
            let (level, is_player) = context(ca, t);
            emit(lua, ca, &g, level, is_player, how)
        }
        None => Value::Nil.into_lua_multi(lua),
    }
}

/// `ArgUnit`/`ArgOptString`: a string or a number's text, else nothing.
fn arg_str(v: &Value) -> Option<String> {
    match v {
        Value::String(_) | Value::Integer(_) | Value::Number(_) => crate::lua::to_str(v),
        _ => None,
    }
}

/// `ArgInt`: a number truncated, else 0.
fn arg_int(v: &Value) -> i64 {
    if is_number(v) {
        to_int(v)
    } else {
        0
    }
}

/// The by-index getters: `(unit, index [, filter])`, nil for no unit or an index below 1.
fn by_index(
    lua: &Lua,
    ca: &Ca,
    args: (Value, Value, Value),
    lock: Option<Filter>,
    how: Emit,
) -> mlua::Result<MultiValue> {
    let (u, i, f) = args;
    let index = arg_int(&i);
    let Some(token) = arg_str(&u).filter(|_| index >= 1) else {
        return Value::Nil.into_lua_multi(lua);
    };
    let p = parse_filters(arg_str(&f).as_deref());
    let filter = lock.unwrap_or_else(|| p.range());
    let t = target(lua, ca, &token)?;
    let g = nth(ca, &t, index, filter, &p.m);
    emit_or_nil(lua, ca, &t, g, how)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    for (name, lock, how) in [
        ("GetAuraDataByIndex", None, Emit::Table),
        ("UnitAura", None, Emit::Positional),
    ] {
        let c = api.ca.clone();
        api.table(
            "C_UnitAuras",
            name,
            move |lua, args: (Value, Value, Value)| by_index(lua, &c, args, lock, how),
        )?;
    }
    for (name, lock, how) in [
        ("UnitBuff", Filter::Helpful, Emit::Positional),
        ("UnitDebuff", Filter::Harmful, Emit::Positional),
    ] {
        let c = api.ca.clone();
        api.table(
            "C_UnitAuras",
            name,
            move |lua, args: (Value, Value, Value)| by_index(lua, &c, args, Some(lock), how),
        )?;
    }
    // The table aliases lock the range and take no filter.
    for (name, lock) in [
        ("GetBuffDataByIndex", Filter::Helpful),
        ("GetDebuffDataByIndex", Filter::Harmful),
    ] {
        let c = api.ca.clone();
        api.table("C_UnitAuras", name, move |lua, (u, i): (Value, Value)| {
            by_index(lua, &c, (u, i, Value::Nil), Some(lock), Emit::Table)
        })?;
    }

    let c = api.ca.clone();
    api.table(
        "C_UnitAuras",
        "GetUnitAuraBySpellID",
        move |lua, (u, s, f): (Value, Value, Value)| {
            let spell = arg_int(&s);
            let Some(token) = arg_str(&u).filter(|_| spell > 0) else {
                return Value::Nil.into_lua_multi(lua);
            };
            let p = parse_filters(arg_str(&f).as_deref());
            let t = target(lua, &c, &token)?;
            let want = spell as u32;
            let g = by_spell(&c, &t, &|id| id == want, p.range_opt(), &p.m);
            emit_or_nil(lua, &c, &t, g, Emit::Table)
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_UnitAuras",
        "GetAuraDataBySpellName",
        move |lua, (u, n, f): (Value, Value, Value)| {
            let (Some(token), Some(name)) = (arg_str(&u), arg_str(&n).filter(|s| !s.is_empty()))
            else {
                return Value::Nil.into_lua_multi(lua);
            };
            let p = parse_filters(arg_str(&f).as_deref());
            let t = target(lua, &c, &token)?;
            let table = spells::table(&c.db);
            let named = |id: u32| {
                table
                    .as_ref()
                    .and_then(|t| t.row(id))
                    .is_some_and(|r| r.loc(col::NAME) == name)
            };
            let g = by_spell(&c, &t, &named, p.range_opt(), &p.m);
            emit_or_nil(lua, &c, &t, g, Emit::Table)
        },
    )?;

    let c = api.ca.clone();
    api.table(
        "C_UnitAuras",
        "GetPlayerAuraBySpellID",
        move |lua, s: Value| {
            let spell = arg_int(&s);
            if spell <= 0 {
                return Value::Nil.into_lua_multi(lua);
            }
            let player = c.lock().mirror.player;
            let t = if player == 0 {
                Target::Nobody
            } else {
                Target::Live(player)
            };
            let want = spell as u32;
            let g = by_spell(&c, &t, &|id| id == want, None, &Match::default());
            emit_or_nil(lua, &c, &t, g, Emit::Table)
        },
    )?;

    // `GetAuraSlots(unit [, filter [, maxSlots [, continuationToken [, fill]]]])`: the
    // continuation token, then the slot ids; with a table fifth, the ids fill it (`t[1..n]`, the
    // stale tail cleared, `t.n`) and the call answers `token, n`.
    let c = api.ca.clone();
    api.table(
        "C_UnitAuras",
        "GetAuraSlots",
        move |lua, args: mlua::Variadic<Value>| {
            let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Nil);
            let max = arg_int(&arg(2));
            let token = arg_int(&arg(3));
            let slots = match arg_str(&arg(0)) {
                Some(unit) => {
                    let p = parse_filters(arg_str(&arg(1)).as_deref());
                    let t = target(lua, &c, &unit)?;
                    collect_slots(&c, &t, p.range(), &p.m)
                }
                None => Vec::new(),
            };
            let total = slots.len();
            let start = usize::try_from(token - 1).unwrap_or(0).min(total);
            let mut n = total - start;
            if max > 0 && n > max as usize {
                n = max as usize;
            }
            let more = start + n < total;
            let next = more.then_some((start + n + 1) as i64);
            if let Value::Table(fill) = arg(4) {
                for i in 0..n {
                    fill.raw_set(i + 1, slots[start + i])?;
                }
                let mut k = n + 1;
                while !matches!(fill.raw_get::<Value>(k)?, Value::Nil) {
                    fill.raw_set(k, Value::Nil)?;
                    k += 1;
                }
                fill.raw_set("n", n)?;
                return (next, n).into_lua_multi(lua);
            }
            let mut out = vec![next.map_or(Value::Nil, Value::Integer)];
            out.extend(slots[start..start + n].iter().map(|s| Value::Integer(*s)));
            Ok(MultiValue::from_vec(out))
        },
    )?;

    for (name, how) in [
        ("GetAuraDataBySlot", Emit::Table),
        ("UnitAuraBySlot", Emit::Positional),
    ] {
        let c = api.ca.clone();
        api.table("C_UnitAuras", name, move |lua, (u, s): (Value, Value)| {
            let Some(token) = arg_str(&u).filter(|_| is_number(&s)) else {
                return Value::Nil.into_lua_multi(lua);
            };
            let t = target(lua, &c, &token)?;
            let g = by_opaque(&c, &t, to_int(&s));
            emit_or_nil(lua, &c, &t, g, how)
        })?;
    }

    // `GetUnitAuras(unit [, filter])`: an explicit range selects it, neither selects both.
    let c = api.ca.clone();
    api.table(
        "C_UnitAuras",
        "GetUnitAuras",
        move |lua, (u, f): (Value, Value)| {
            let out = lua.create_table()?;
            let Some(token) = arg_str(&u) else {
                return Ok(out);
            };
            let p = parse_filters(arg_str(&f).as_deref());
            let both = !p.helpful && !p.harmful;
            let t = target(lua, &c, &token)?;
            let (level, is_player) = context(&c, &t);
            let mut key = 1;
            for (want, filter) in [(p.helpful, Filter::Helpful), (p.harmful, Filter::Harmful)] {
                if !(want || both) {
                    continue;
                }
                for g in all(&c, &t, filter, &p.m) {
                    let v = emit(lua, &c, &g, level, is_player, Emit::Table)?;
                    out.raw_set(key, v.into_iter().next().unwrap_or(Value::Nil))?;
                    key += 1;
                }
            }
            Ok(out)
        },
    )?;

    // `_G["DEBUFF_TYPE_"..type:upper().."_COLOR"] or DEBUFF_TYPE_NONE_COLOR`, a plain colour
    // table from the packed data when the colour globals are not up.
    api.table("C_UnitAuras", "GetAuraDispelTypeColor", |lua, v: Value| {
        let tag = match arg_str(&v).filter(|s| !s.is_empty()) {
            Some(t) => format!("DEBUFF_TYPE_{}_COLOR", t.to_ascii_uppercase()),
            None => "DEBUFF_TYPE_NONE_COLOR".to_string(),
        };
        if let Ok(Value::Table(t)) = lua.globals().get::<Value>(tag.as_str()) {
            return Ok(Value::Table(t));
        }
        let argb = DEBUFF_COLORS
            .iter()
            .find(|(k, _)| *k == tag)
            .or_else(|| {
                DEBUFF_COLORS
                    .iter()
                    .find(|(k, _)| *k == "DEBUFF_TYPE_NONE_COLOR")
            })
            .map_or(0, |(_, v)| *v);
        Ok(Value::Table(plain_color(lua, argb)?))
    })?;

    // `RegisterAuraDurationModifierByTrigger(triggerFamily, triggerSchool, affectedFamily,
    // affectedFamilyFlags, affectedIcon, op [, valueSeconds])`.
    let c = api.ca.clone();
    api.table(
        "C_UnitAuras",
        "RegisterAuraDurationModifierByTrigger",
        move |_, args: mlua::Variadic<Value>| {
            let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Nil);
            if !(0..5).all(|i| is_number(&arg(i))) || as_string(&arg(5)).is_none() {
                return Err(mlua::Error::runtime(
                    "Usage: C_UnitAuras.RegisterAuraDurationModifierByTrigger(triggerFamily, triggerSchool, affectedFamily, affectedFamilyFlags, affectedIcon, op[, valueSeconds])",
                ));
            }
            let family = to_number(&arg(0)) as u32;
            let school = to_number(&arg(1)) as i32;
            let affected = to_number(&arg(2)) as u32;
            let mask = to_number(&arg(3)) as u64;
            let icon = to_number(&arg(4)) as u32;
            let op = as_string(&arg(5)).and_then(|s| aura::ModOp::parse(&s));
            let value_ms = if is_number(&arg(6)) {
                (to_number(&arg(6)) * 1000.0) as i32
            } else {
                0
            };
            let (Some(op), true) = (op, family != 0) else {
                return Ok(false);
            };
            Ok(c.lock()
                .auras
                .register_duration_mod(0, family, school, affected, mask, icon, op, value_ms))
        },
    )?;

    // `RegisterComboDuration(spellID, baseSeconds, maxSeconds)`: a finisher's combo range when
    // the client's duration row is missing or wrong.
    let c = api.ca.clone();
    api.table(
        "C_UnitAuras",
        "RegisterComboDuration",
        move |_, (s, b, m): (Value, Value, Value)| {
            if !is_number(&s) || !is_number(&b) || !is_number(&m) {
                return Err(mlua::Error::runtime(
                    "Usage: C_UnitAuras.RegisterComboDuration(spellID, baseSeconds, maxSeconds)",
                ));
            }
            let spell = to_number(&s) as u32;
            let base = (to_number(&b) * 1000.0) as i32;
            let max = (to_number(&m) * 1000.0) as i32;
            if spell == 0 || base < 0 || max <= 0 {
                return Ok(false);
            }
            Ok(c.lock().auras.register_combo_duration(spell, base, max))
        },
    )?;
    Ok(())
}

/// `PushPlainColorTable`: `{r, g, b, a}` from packed ARGB.
fn plain_color(lua: &Lua, argb: u32) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let ch = |shift: u32| f64::from((argb >> shift) & 0xFF) / 255.0;
    t.set("r", ch(16))?;
    t.set("g", ch(8))?;
    t.set("b", ch(0))?;
    t.set("a", ch(24))?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_whole_tokens_and_bang_negates() {
        let p = parse_filters(Some("HARMFUL|!PLAYER RAID_PLAYER_DISPELLABLE"));
        assert!(p.harmful && !p.helpful);
        assert_eq!(p.m.caster, Mode::Not);
        assert_eq!(p.m.dispel, Mode::Any);
        assert_eq!(p.range(), Filter::Harmful);
        assert_eq!(parse_filters(None).range_opt(), None);
        assert_eq!(
            parse_filters(Some("HELPFUL HARMFUL")).range(),
            Filter::Helpful
        );
    }

    #[test]
    fn harmful_order_reads_the_debuff_range_first() {
        assert_eq!(in_filter_order(Filter::Harmful, 0), 32);
        assert_eq!(in_filter_order(Filter::Harmful, 16), 0);
        assert_eq!(in_filter_order(Filter::Helpful, 5), 5);
    }

    #[test]
    fn the_colour_table_unpacks_argb() {
        let lua = Lua::new();
        let t = plain_color(&lua, 0xFF00_81FF).unwrap();
        assert_eq!(t.get::<f64>("r").unwrap(), 0.0);
        assert_eq!(t.get::<f64>("a").unwrap(), 1.0);
        assert!((t.get::<f64>("g").unwrap() - 129.0 / 255.0).abs() < 1e-9);
    }
}

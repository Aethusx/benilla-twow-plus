//! The game state nampower's Lua functions read, mirrored once a frame: a Lua call runs inside
//! the interface tick, where the world cannot be borrowed, so each descriptor that changed is
//! copied here dense, by index, with the unit tokens, the marks, the spellbook and the cooldowns.

use std::collections::HashMap;
use std::time::Instant;

use benilla_app::ext::{CooldownRecord, ExtView, ItemInfo};
use bevy::math::Vec3;

/// `OBJECT_FIELD_TYPE`, `UNIT_FIELD_*` and `PLAYER_FIELD_*` indices (1.12.1, build 5875).
pub mod field {
    pub const OBJECT_ENTRY: usize = 3;
    pub const UNIT_CHARM: usize = 6;
    pub const UNIT_SUMMON: usize = 8;
    pub const UNIT_CHARMED_BY: usize = 10;
    pub const UNIT_SUMMONED_BY: usize = 12;
    pub const UNIT_CREATED_BY: usize = 14;
    pub const UNIT_TARGET: usize = 16;
    pub const UNIT_CHANNEL_OBJECT: usize = 20;
    pub const UNIT_HEALTH: usize = 22;
    pub const UNIT_POWER1: usize = 23;
    pub const UNIT_MAX_HEALTH: usize = 28;
    pub const UNIT_LEVEL: usize = 34;
    pub const UNIT_BYTES_0: usize = 36;
    pub const UNIT_FLAGS: usize = 46;
    pub const UNIT_AURA: usize = 47;
    pub const UNIT_AURA_FLAGS: usize = 95;
    pub const UNIT_AURA_LEVELS: usize = 101;
    pub const UNIT_AURA_APPLICATIONS: usize = 113;
    pub const UNIT_DISPLAY_ID: usize = 131;
    pub const UNIT_MOUNT_DISPLAY_ID: usize = 133;
    pub const UNIT_DYNAMIC_FLAGS: usize = 143;
    pub const UNIT_CHANNEL_SPELL: usize = 144;
    pub const UNIT_END: usize = 188;
    pub const ITEM_STACK_COUNT: usize = 14;
    pub const ITEM_DURATION: usize = 15;
    pub const ITEM_SPELL_CHARGES: usize = 16;
    pub const ITEM_FLAGS: usize = 21;
    pub const ITEM_ENCHANTMENT: usize = 22;
    pub const ITEM_RANDOM_PROPERTIES_ID: usize = 44;
    pub const ITEM_DURABILITY: usize = 46;
    pub const ITEM_MAX_DURABILITY: usize = 47;
    pub const CONTAINER_NUM_SLOTS: usize = 48;
    pub const CONTAINER_SLOT_1: usize = 50;
    pub const PLAYER_QUEST_LOG_1_1: usize = 198;
    /// `PLAYER_VISIBLE_ITEM_1_CREATOR`; a slot is 12 fields, the entry at +2, enchants at +3.
    pub const PLAYER_VISIBLE_ITEM_1: usize = 258;
    pub const PLAYER_INV_SLOT_HEAD: usize = 486;
    pub const PLAYER_PACK_SLOT_1: usize = 532;
    pub const PLAYER_BANK_SLOT_1: usize = 564;
    pub const PLAYER_BANK_BAG_SLOT_1: usize = 612;
    pub const PLAYER_KEYRING_SLOT_1: usize = 648;
    pub const PLAYER_MOD_DAMAGE_DONE_POS: usize = 1201;
    pub const PLAYER_MOD_DAMAGE_DONE_NEG: usize = 1208;
    pub const PLAYER_AMMO_ID: usize = 1223;
}

/// One object's descriptor, dense from index 0; an unset index reads 0.
#[derive(Clone, Debug, Default)]
pub struct Fields(Vec<u32>);

impl Fields {
    pub fn from_object(fields: &benilla_protocol::ObjectFields) -> Self {
        let mut out = Vec::new();
        for (index, value) in fields.raw_fields() {
            let i = usize::from(index);
            if out.len() <= i {
                out.resize(i + 1, 0);
            }
            out[i] = value;
        }
        Fields(out)
    }

    #[cfg(test)]
    pub(crate) fn from_vec(v: Vec<u32>) -> Self {
        Fields(v)
    }

    pub fn u32(&self, i: usize) -> u32 {
        self.0.get(i).copied().unwrap_or(0)
    }

    pub fn f32(&self, i: usize) -> f32 {
        f32::from_bits(self.u32(i))
    }

    pub fn guid(&self, i: usize) -> u64 {
        u64::from(self.u32(i)) | (u64::from(self.u32(i + 1)) << 32)
    }

    pub fn byte(&self, base: usize, slot: usize) -> u8 {
        (self.u32(base + slot / 4) >> ((slot % 4) * 8)) as u8
    }

    pub fn is_unit(&self) -> bool {
        // `OBJECT_FIELD_TYPE` carries TYPEMASK_UNIT (0x8) for units and players.
        self.u32(2) & 0x8 != 0
    }

    pub fn aura(&self, slot: usize) -> u32 {
        self.u32(field::UNIT_AURA + slot)
    }

    /// Every buff slot (0-31) taken: the server refuses a 33rd buff.
    pub fn buff_capped(&self) -> bool {
        (0..32).all(|s| self.aura(s) != 0)
    }

    /// Every debuff slot (32-47) taken.
    pub fn debuff_capped(&self) -> bool {
        (32..48).all(|s| self.aura(s) != 0)
    }
}

/// The base unit tokens resolved each frame; derived forms resolve off them through the
/// descriptors.
pub fn base_tokens() -> Vec<String> {
    let mut t: Vec<String> = ["player", "target", "pet", "mouseover"]
        .into_iter()
        .map(String::from)
        .collect();
    for i in 1..=4 {
        t.push(format!("party{i}"));
        t.push(format!("partypet{i}"));
    }
    for i in 1..=40 {
        t.push(format!("raid{i}"));
        t.push(format!("raidpet{i}"));
    }
    t
}

/// Our auras' expiry by raw slot, engine ms; 0 for none.
#[derive(Clone, Copy)]
pub struct AuraExpiry(pub [u64; 48]);

impl Default for AuraExpiry {
    fn default() -> Self {
        Self([0; 48])
    }
}

impl std::ops::Deref for AuraExpiry {
    type Target = [u64; 48];
    fn deref(&self) -> &[u64; 48] {
        &self.0
    }
}

impl std::ops::DerefMut for AuraExpiry {
    fn deref_mut(&mut self) -> &mut [u64; 48] {
        &mut self.0
    }
}

#[derive(Default)]
pub struct Mirror {
    pub player: u64,
    /// `(token, guid)`, 0 for none, in [`base_tokens`] order.
    pub tokens: Vec<(String, u64)>,
    pub objects: HashMap<u64, Fields>,
    /// The client's raid-target table, slots 1-8 at 0-7.
    pub marks: [u64; 8],
    pub in_group: bool,
    /// The spellbook and the pet's, as learned.
    pub book: Vec<u32>,
    pub pet_book: Vec<u32>,
    pub items: HashMap<u32, ItemInfo>,
    /// Our auras' expiry by raw slot, engine ms; 0 for none.
    pub aura_expiry: AuraExpiry,
    /// The quest the open quest dialog shows.
    pub quest_dialog: Option<u32>,
    pub cooldowns: Vec<CooldownRecord>,
    cooldown_epoch: u64,
    /// Every streamed unit's position, for the range check.
    pub positions: HashMap<u64, Vec3>,
    pub move_flags: u32,
    pub attacking: bool,
    pub auto_repeat: u32,
    /// `GetSpellModifiers` for each book spell and op, `(flat, percent)`, refilled when a
    /// modifier or the book changes.
    pub mods: HashMap<(u32, u8), (i32, i32)>,
    pub mods_dirty: bool,
    /// The instant the cooldowns were read at and what the GetTime clock read then.
    pub clock_anchor: Option<(Instant, f64)>,
    /// When stale objects were last pruned.
    pruned_at: Option<Instant>,
}

/// The changes [`Mirror::refresh`] saw on a unit, for the diff-driven events.
pub struct UnitChange {
    pub guid: u64,
    pub old: Option<Fields>,
}

impl Mirror {
    pub fn token(&self, name: &str) -> Option<u64> {
        self.tokens
            .iter()
            .find(|(t, _)| t.eq_ignore_ascii_case(name))
            .map(|(_, g)| *g)
            .filter(|g| *g != 0)
    }

    /// Re-read the frame's state; returns the units whose descriptor changed, with their last
    /// copy.
    pub fn refresh(&mut self, view: &ExtView, now: Instant) -> Vec<UnitChange> {
        if self.tokens.is_empty() {
            self.tokens = base_tokens().into_iter().map(|t| (t, 0)).collect();
        }
        for (token, guid) in &mut self.tokens {
            *guid = view.unit_guid(token).unwrap_or(0);
        }
        self.player = self.token("player").unwrap_or(0);
        for (i, mark) in self.marks.iter_mut().enumerate() {
            *mark = view.raid_mark(i as u8 + 1).unwrap_or(0);
        }
        self.in_group = view.in_group();
        let mut changes = Vec::new();
        for (guid, fields) in view.changed_objects() {
            let fresh = Fields::from_object(fields);
            let old = self.objects.insert(guid, fresh);
            if self.objects[&guid].is_unit() {
                changes.push(UnitChange { guid, old });
            }
        }
        if self
            .pruned_at
            .is_none_or(|at| now.duration_since(at).as_secs() >= 2)
        {
            self.objects.retain(|g, _| view.is_streamed(*g));
            self.pruned_at = Some(now);
        }
        self.positions.clear();
        self.positions.extend(view.positions());
        self.move_flags = view.move_flags();
        self.attacking = view.attacking();
        self.auto_repeat = view.auto_repeat().unwrap_or(0);
        if self.mods_dirty {
            self.mods.clear();
            for &spell in self.book.iter().chain(self.pet_book.iter()) {
                for op in 0..29u8 {
                    if let Some(m) = view.spell_modifier(spell, op) {
                        self.mods.insert((spell, op), m);
                    }
                }
            }
            self.mods_dirty = false;
        }
        let epoch = view.cooldown_epoch();
        if epoch != self.cooldown_epoch || self.cooldowns.is_empty() {
            self.cooldowns = view.cooldown_records();
            self.cooldown_epoch = epoch;
        }
        changes
    }

    pub fn player_fields(&self) -> Option<&Fields> {
        self.objects.get(&self.player)
    }

    /// A unit token in nampower's extended grammar: a base token, `markN`, or `0x` and sixteen
    /// hex digits, then any run of `target`, `pet` and `owner` suffixes.
    pub fn resolve(&self, token: &str) -> Option<u64> {
        let lower = token.trim().to_ascii_lowercase();
        let (mut guid, mut rest) = self.base(&lower)?;
        while !rest.is_empty() {
            let (next, tail) = self.hop(guid, rest)?;
            if next == 0 {
                return None;
            }
            guid = next;
            rest = tail;
        }
        Some(guid)
    }

    /// One suffix of the grammar off `guid`'s descriptor: `target`, `pet` or `owner`.
    fn hop<'a>(&self, guid: u64, rest: &'a str) -> Option<(u64, &'a str)> {
        let f = self.objects.get(&guid)?;
        if let Some(t) = rest.strip_prefix("target") {
            return Some((f.guid(field::UNIT_TARGET), t));
        }
        if let Some(t) = rest.strip_prefix("pet") {
            return Some((f.guid(field::UNIT_SUMMON), t));
        }
        let t = rest.strip_prefix("owner")?;
        let owner = [
            field::UNIT_SUMMONED_BY,
            field::UNIT_CHARMED_BY,
            field::UNIT_CREATED_BY,
        ]
        .into_iter()
        .map(|i| f.guid(i))
        .find(|g| *g != 0)?;
        Some((owner, t))
    }

    /// The longest base token `lower` starts with, and what follows it.
    fn base<'a>(&self, lower: &'a str) -> Option<(u64, &'a str)> {
        if let Some(hex) = lower.strip_prefix("0x") {
            let digits = hex
                .bytes()
                .take_while(u8::is_ascii_hexdigit)
                .count()
                .min(16);
            let guid = u64::from_str_radix(&hex[..digits], 16).ok()?;
            return (guid != 0).then_some((guid, &hex[digits..]));
        }
        if let Some(n) = lower.strip_prefix("mark") {
            let digit = n.as_bytes().first().filter(|b| (b'1'..=b'8').contains(b))?;
            let guid = self.marks[usize::from(digit - b'1')];
            return (guid != 0).then_some((guid, &n[1..]));
        }
        self.tokens
            .iter()
            .filter(|(t, _)| lower.starts_with(t.as_str()))
            .max_by_key(|(t, _)| t.len())
            .and_then(|(t, g)| (*g != 0).then_some((*g, &lower[t.len()..])))
    }

    /// The token flags the `*_GUID` events carry: player, target, mouseover, pet, party and raid
    /// index.
    pub fn token_flags(&self, guid: u64) -> (bool, bool, bool, bool, u32, u32) {
        let is = |t: &str| self.token(t) == Some(guid);
        let index = |prefix: &str, n: u32| {
            (1..=n)
                .find(|i| self.token(&format!("{prefix}{i}")) == Some(guid))
                .unwrap_or(0)
        };
        (
            is("player"),
            is("target"),
            is("mouseover"),
            is("pet"),
            index("party", 4),
            index("raid", 40),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(pairs: &[(usize, u32)]) -> Fields {
        let mut v = vec![0u32; field::UNIT_END];
        v[2] = 0x9;
        for &(i, x) in pairs {
            v[i] = x;
        }
        Fields::from_vec(v)
    }

    fn mirror() -> Mirror {
        let mut m = Mirror {
            tokens: base_tokens().into_iter().map(|t| (t, 0)).collect(),
            ..Default::default()
        };
        m.tokens[1].1 = 0x50; // target
        m.tokens[4].1 = 0x60; // party1
        m.marks[0] = 0x70;
        m.objects.insert(
            0x50,
            unit(&[(field::UNIT_TARGET, 0x60), (field::UNIT_SUMMONED_BY, 0x70)]),
        );
        m.objects.insert(0x60, unit(&[(field::UNIT_SUMMON, 0x80)]));
        m.objects.insert(0x70, unit(&[(field::UNIT_TARGET, 0x50)]));
        m
    }

    #[test]
    fn the_extended_grammar_resolves_bases_and_suffix_chains() {
        let m = mirror();
        assert_eq!(m.resolve("target"), Some(0x50));
        assert_eq!(m.resolve("TargetTarget"), Some(0x60));
        assert_eq!(m.resolve("targetowner"), Some(0x70));
        assert_eq!(m.resolve("party1pet"), Some(0x80));
        assert_eq!(m.resolve("mark1target"), Some(0x50));
        assert_eq!(m.resolve("0x0000000000000050target"), Some(0x60));
        assert_eq!(
            m.resolve("partypet1"),
            None,
            "longest base wins, and it is empty"
        );
        assert_eq!(m.resolve("mark9"), None);
        assert_eq!(m.resolve("targetfoo"), None);
    }
}

//! The game state the natives read, mirrored once a frame. The DLL reads the engine's objects and
//! globals inside the call; a native here runs inside the interface tick, where the world cannot be
//! borrowed, so each frame copies what the API reads: every streamed object's descriptor (dense,
//! by field index, which is the DLL's descriptor byte offset over 4), where each object stands,
//! and the player's movement and cooldowns.

use std::collections::HashMap;
use std::time::Instant;

use benilla_app::ext::{CooldownRecord, ExtRange, ExtView, ExtWorld};
use benilla_formats::RangeUnit;
use bevy::math::{Quat, Vec3};

/// Descriptor field indices (1.12.1, build 5875), benilla-protocol's names.
pub mod field {
    pub const OBJECT_GUID: usize = 0;
    pub const OBJECT_TYPE: usize = 2;
    pub const OBJECT_ENTRY: usize = 3;
    pub const OBJECT_SCALE_X: usize = 4;
    pub const ITEM_OWNER: usize = 6;
    pub const ITEM_CONTAINED: usize = 8;
    pub const ITEM_CREATOR: usize = 10;
    pub const ITEM_GIFTCREATOR: usize = 12;
    pub const ITEM_STACK_COUNT: usize = 14;
    pub const ITEM_DURATION: usize = 15;
    pub const ITEM_SPELL_CHARGES: usize = 16;
    pub const ITEM_FLAGS: usize = 21;
    pub const ITEM_ENCHANTMENT: usize = 22;
    pub const ITEM_PROPERTY_SEED: usize = 43;
    pub const ITEM_RANDOM_PROPERTIES_ID: usize = 44;
    pub const ITEM_TEXT_ID: usize = 45;
    pub const ITEM_DURABILITY: usize = 46;
    pub const ITEM_MAX_DURABILITY: usize = 47;
    pub const CONTAINER_NUM_SLOTS: usize = 48;
    pub const CONTAINER_SLOT_1: usize = 50;
    pub const UNIT_CHARM: usize = 6;
    pub const UNIT_SUMMON: usize = 8;
    pub const UNIT_CHARMED_BY: usize = 10;
    pub const UNIT_SUMMONED_BY: usize = 12;
    pub const UNIT_CREATED_BY: usize = 14;
    pub const UNIT_TARGET: usize = 16;
    pub const UNIT_PERSUADED: usize = 18;
    pub const UNIT_CHANNEL_OBJECT: usize = 20;
    pub const UNIT_HEALTH: usize = 22;
    pub const UNIT_POWER1: usize = 23;
    pub const UNIT_MAX_HEALTH: usize = 28;
    pub const UNIT_MAX_POWER1: usize = 29;
    pub const UNIT_LEVEL: usize = 34;
    pub const UNIT_FACTION_TEMPLATE: usize = 35;
    pub const UNIT_BYTES_0: usize = 36;
    pub const UNIT_VIRTUAL_ITEM_SLOT_DISPLAY: usize = 37;
    pub const UNIT_VIRTUAL_ITEM_INFO: usize = 40;
    pub const UNIT_FLAGS: usize = 46;
    pub const UNIT_AURA: usize = 47;
    pub const UNIT_AURA_FLAGS: usize = 95;
    pub const UNIT_AURA_LEVELS: usize = 101;
    pub const UNIT_AURA_APPLICATIONS: usize = 113;
    pub const UNIT_AURA_STATE: usize = 125;
    pub const UNIT_BASE_ATTACK_TIME: usize = 126;
    pub const UNIT_RANGED_ATTACK_TIME: usize = 128;
    pub const UNIT_BOUNDING_RADIUS: usize = 129;
    pub const UNIT_COMBAT_REACH: usize = 130;
    pub const UNIT_DISPLAY_ID: usize = 131;
    pub const UNIT_NATIVE_DISPLAY_ID: usize = 132;
    pub const UNIT_MOUNT_DISPLAY_ID: usize = 133;
    pub const UNIT_MIN_DAMAGE: usize = 134;
    pub const UNIT_MAX_DAMAGE: usize = 135;
    pub const UNIT_BYTES_1: usize = 138;
    pub const UNIT_PET_NUMBER: usize = 139;
    pub const UNIT_DYNAMIC_FLAGS: usize = 143;
    pub const UNIT_CHANNEL_SPELL: usize = 144;
    pub const UNIT_MOD_CAST_SPEED: usize = 145;
    pub const UNIT_CREATED_BY_SPELL: usize = 146;
    pub const UNIT_NPC_FLAGS: usize = 147;
    pub const UNIT_NPC_EMOTESTATE: usize = 148;
    pub const UNIT_TRAINING_POINTS: usize = 149;
    pub const UNIT_STAT0: usize = 150;
    pub const UNIT_RESISTANCES: usize = 155;
    pub const UNIT_BASE_MANA: usize = 162;
    pub const UNIT_BASE_HEALTH: usize = 163;
    pub const UNIT_BYTES_2: usize = 164;
    pub const UNIT_ATTACK_POWER: usize = 165;
    pub const UNIT_RANGED_ATTACK_POWER: usize = 168;
    pub const UNIT_END: usize = 188;
    pub const PLAYER_DUEL_ARBITER: usize = 188;
    pub const PLAYER_FLAGS: usize = 190;
    pub const PLAYER_GUILDID: usize = 191;
    pub const PLAYER_GUILDRANK: usize = 192;
    pub const PLAYER_BYTES: usize = 193;
    pub const PLAYER_BYTES_2: usize = 194;
    pub const PLAYER_BYTES_3: usize = 195;
    pub const PLAYER_QUEST_LOG_1_1: usize = 198;
    /// `PLAYER_VISIBLE_ITEM_1_CREATOR`; a slot is 12 fields, the entry at +2, enchants at +3.
    pub const PLAYER_VISIBLE_ITEM_1: usize = 258;
    /// 23 slots: equipment 0-18, bags 19-22.
    pub const PLAYER_INV_SLOT_HEAD: usize = 486;
    pub const PLAYER_PACK_SLOT_1: usize = 532;
    pub const PLAYER_BANK_SLOT_1: usize = 564;
    pub const PLAYER_BANK_BAG_SLOT_1: usize = 612;
    pub const PLAYER_VENDORBUYBACK_SLOT_1: usize = 624;
    pub const PLAYER_KEYRING_SLOT_1: usize = 648;
    pub const PLAYER_FARSIGHT: usize = 712;
    pub const PLAYER_COMBO_TARGET: usize = 714;
    pub const PLAYER_XP: usize = 716;
    pub const PLAYER_NEXT_LEVEL_XP: usize = 717;
    /// 128 skills x 3 dwords: id and step, value and max, bonuses.
    pub const PLAYER_SKILL_INFO_1_1: usize = 718;
    pub const PLAYER_CHARACTER_POINTS1: usize = 1102;
    pub const PLAYER_TRACK_CREATURES: usize = 1104;
    pub const PLAYER_TRACK_RESOURCES: usize = 1105;
    pub const PLAYER_EXPLORED_ZONES_1: usize = 1111;
    pub const PLAYER_REST_STATE_EXPERIENCE: usize = 1175;
    pub const PLAYER_COINAGE: usize = 1176;
    pub const PLAYER_MOD_DAMAGE_DONE_POS: usize = 1201;
    pub const PLAYER_MOD_DAMAGE_DONE_NEG: usize = 1208;
    pub const PLAYER_MOD_DAMAGE_DONE_PCT: usize = 1215;
    pub const PLAYER_BYTES_FIELD: usize = 1222;
    pub const PLAYER_AMMO_ID: usize = 1223;
    pub const PLAYER_SELF_RES_SPELL: usize = 1224;
    pub const PLAYER_BUYBACK_PRICE_1: usize = 1226;
    pub const PLAYER_BYTES2_FIELD: usize = 1260;
    pub const PLAYER_WATCHED_FACTION_INDEX: usize = 1261;
    pub const GAMEOBJECT_CREATED_BY: usize = 6;
    pub const GAMEOBJECT_DISPLAYID: usize = 8;
    pub const GAMEOBJECT_FLAGS: usize = 9;
    pub const GAMEOBJECT_STATE: usize = 14;
    pub const GAMEOBJECT_POS_X: usize = 15;
    pub const GAMEOBJECT_FACING: usize = 18;
    pub const GAMEOBJECT_DYN_FLAGS: usize = 19;
    pub const GAMEOBJECT_FACTION: usize = 20;
    pub const GAMEOBJECT_TYPE_ID: usize = 21;
    pub const GAMEOBJECT_LEVEL: usize = 22;
}

/// `TYPEMASK_*`, `OBJECT_FIELD_TYPE`'s bits.
pub mod typemask {
    pub const ITEM: u32 = 0x2;
    pub const CONTAINER: u32 = 0x4;
    pub const UNIT: u32 = 0x8;
    pub const PLAYER: u32 = 0x10;
    pub const GAMEOBJECT: u32 = 0x20;
    pub const DYNAMICOBJECT: u32 = 0x40;
    pub const CORPSE: u32 = 0x80;
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
    pub fn from_vec(v: Vec<u32>) -> Self {
        Fields(v)
    }

    pub fn u32(&self, i: usize) -> u32 {
        self.0.get(i).copied().unwrap_or(0)
    }

    pub fn i32(&self, i: usize) -> i32 {
        self.u32(i) as i32
    }

    pub fn f32(&self, i: usize) -> f32 {
        f32::from_bits(self.u32(i))
    }

    pub fn guid(&self, i: usize) -> u64 {
        u64::from(self.u32(i)) | (u64::from(self.u32(i + 1)) << 32)
    }

    /// Byte `slot` of the byte array starting at field `base`.
    pub fn byte(&self, base: usize, slot: usize) -> u8 {
        (self.u32(base + slot / 4) >> ((slot % 4) * 8)) as u8
    }

    /// The low or high 16 bits of field `i`.
    pub fn half(&self, i: usize, high: bool) -> u16 {
        (self.u32(i) >> if high { 16 } else { 0 }) as u16
    }

    pub fn type_mask(&self) -> u32 {
        self.u32(field::OBJECT_TYPE)
    }

    pub fn is(&self, mask: u32) -> bool {
        self.type_mask() & mask != 0
    }

    pub fn entry(&self) -> u32 {
        self.u32(field::OBJECT_ENTRY)
    }

    /// `UNIT_FIELD_BYTES_0`: race, class, gender, power type.
    pub fn race(&self) -> u8 {
        self.byte(field::UNIT_BYTES_0, 0)
    }
    pub fn class(&self) -> u8 {
        self.byte(field::UNIT_BYTES_0, 1)
    }
    pub fn gender(&self) -> u8 {
        self.byte(field::UNIT_BYTES_0, 2)
    }
    pub fn power_type(&self) -> u8 {
        self.byte(field::UNIT_BYTES_0, 3)
    }

    pub fn level(&self) -> u32 {
        self.u32(field::UNIT_LEVEL)
    }
}

/// Where an object stands, WoW's axes (`x` north, `y` west, `z` up), and which way it faces,
/// radians from north, counter-clockwise.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Place {
    pub pos: [f32; 3],
    pub facing: f32,
}

/// Bevy's Y-up position back to WoW's: `wow_to_bevy` is `(-y, z, -x)`.
pub fn wow_position(p: Vec3) -> [f32; 3] {
    [-p.z, -p.x, p.y]
}

/// The orientation a Bevy rotation faces, as the client's `[unit+0x9c4]` holds it: a model looks
/// down its local `-Z`.
pub fn wow_facing(rotation: Quat) -> f32 {
    let f = rotation * Vec3::NEG_Z;
    let (x, y) = (-f.z, -f.x);
    let a = y.atan2(x);
    if a < 0.0 {
        a + std::f32::consts::TAU
    } else {
        a
    }
}

#[derive(Default)]
pub struct Mirror {
    pub player: u64,
    pub target: u64,
    pub objects: HashMap<u64, Fields>,
    pub places: HashMap<u64, Place>,
    pub speeds: HashMap<u64, benilla_protocol::MoveSpeeds>,
    /// Each streamed unit's `GetMinMaxRange` inputs: reach, player bit, motion.
    pub range_units: HashMap<u64, RangeUnit>,
    pub caster: Option<RangeUnit>,
    pub caster_attack_target: Option<RangeUnit>,
    pub move_flags: u32,
    pub attacking: bool,
    pub auto_repeat: u32,
    pub channel: u32,
    pub cast_in_flight: u32,
    pub in_group: bool,
    /// The client's raid-target table, slots 1-8 at 0-7.
    pub marks: [u64; 8],
    pub cooldowns: Vec<CooldownRecord>,
    cooldown_epoch: u64,
    /// `(usable, notEnoughMana)` per known spell, the bar's own walk.
    pub usable: HashMap<u32, (bool, bool)>,
    /// A native asked for usability; the walk runs every frame until then.
    pub usable_wanted_until: Option<Instant>,
    usable_at: Option<Instant>,
    /// The instant this frame mirrored at, and what `GetTime()` read then.
    pub clock: Option<(Instant, f64)>,
    pruned_at: Option<Instant>,
}

/// One unit whose descriptor changed this frame, with its last copy, for the diff-driven events.
pub struct Change {
    pub guid: u64,
    pub old: Option<Fields>,
}

impl Mirror {
    /// Re-read the frame's state; returns the objects whose descriptor changed.
    pub fn refresh(
        &mut self,
        view: &ExtView,
        world: &ExtWorld,
        range: &ExtRange,
        now: Instant,
    ) -> Vec<Change> {
        self.player = view.unit_guid("player").unwrap_or(0);
        self.target = view.target_guid().unwrap_or(0);
        for (i, mark) in self.marks.iter_mut().enumerate() {
            *mark = view.raid_mark(i as u8 + 1).unwrap_or(0);
        }
        self.in_group = view.in_group();
        let mut changes = Vec::new();
        for (guid, fields) in view.changed_objects() {
            let old = self.objects.insert(guid, Fields::from_object(fields));
            changes.push(Change { guid, old });
        }
        if self
            .pruned_at
            .is_none_or(|at| now.duration_since(at).as_secs() >= 2)
        {
            self.objects.retain(|g, _| view.is_streamed(*g));
            self.pruned_at = Some(now);
        }
        self.places.clear();
        self.speeds.clear();
        self.range_units.clear();
        for (guid, pos, rot) in world.placements() {
            self.places.insert(
                guid,
                Place {
                    pos: wow_position(pos),
                    facing: wow_facing(rot),
                },
            );
            if let Some(s) = world.speeds(guid) {
                self.speeds.insert(guid, s);
            }
            if let Some(u) = range.unit(guid) {
                self.range_units.insert(guid, u);
            }
        }
        self.caster = Some(range.caster());
        self.caster_attack_target = range.caster_attack_target();
        self.move_flags = view.move_flags();
        self.attacking = view.attacking();
        self.auto_repeat = view.auto_repeat().unwrap_or(0);
        self.channel = view.channel(now).unwrap_or(0);
        self.cast_in_flight = view.cast_in_flight(now).unwrap_or(0);
        let epoch = view.cooldown_epoch();
        if epoch != self.cooldown_epoch || self.cooldowns.is_empty() {
            self.cooldowns = view.cooldown_records();
            self.cooldown_epoch = epoch;
        }
        changes
    }

    pub fn me(&self) -> Option<&Fields> {
        self.objects.get(&self.player)
    }

    pub fn object(&self, guid: u64) -> Option<&Fields> {
        self.objects.get(&guid)
    }

    pub fn place(&self, guid: u64) -> Option<Place> {
        self.places.get(&guid).copied()
    }

    /// `instant` on the `GetTime()` clock.
    pub fn ui_time(&self, at: Instant) -> f64 {
        let Some((anchor, ui)) = self.clock else {
            return 0.0;
        };
        if at >= anchor {
            ui + at.duration_since(anchor).as_secs_f64()
        } else {
            ui - anchor.duration_since(at).as_secs_f64()
        }
    }
}

impl Mirror {
    /// Refresh the usability verdicts for `spells`: every frame while a native wants them, else
    /// four times a second, so a first ask reads a recent answer.
    pub fn refresh_usable(
        &mut self,
        usable: &benilla_app::ext::ExtUsable,
        spells: &[u32],
        now: Instant,
    ) {
        let wanted = self.usable_wanted_until.is_some_and(|t| now < t);
        let due = self
            .usable_at
            .is_none_or(|at| now.duration_since(at).as_millis() >= 250);
        if !wanted && !due {
            return;
        }
        self.usable_at = Some(now);
        self.usable.clear();
        for (id, u, oom) in usable.verdicts(spells.iter().copied()) {
            self.usable.insert(id, (u, oom));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rotation_reads_back_as_wow_facing() {
        // Facing north (+x WoW) is facing bevy -Z, the identity rotation.
        assert!(wow_facing(Quat::IDENTITY).abs() < 1e-5);
        // A quarter turn left about up faces west, +y WoW, pi/2.
        let west = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        assert!((wow_facing(west) - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    }

    #[test]
    fn positions_go_back_to_wow_axes() {
        assert_eq!(
            wow_position(Vec3::new(-20.0, 30.0, -10.0)),
            [10.0, 20.0, 30.0]
        );
    }
}

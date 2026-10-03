//! The `NP_` CVars: nampower's settings as the player's CVar table holds them. The bootstrap
//! declares each with `RegisterCVar`, as an addon declares its own, so the value persists in
//! `benilla-config/config.toml` and `GetCVar`/`SetCVar` reach it; `SetCVar` is wrapped to apply a
//! change here as it lands.

/// Every `NP_` CVar and its default, as nampower 4.6.1 registers them.
pub const CVARS: &[(&str, &str)] = &[
    ("NP_QueueCastTimeSpells", "1"),
    ("NP_QueueInstantSpells", "1"),
    ("NP_QueueChannelingSpells", "1"),
    ("NP_QueueTargetingSpells", "1"),
    ("NP_QueueOnSwingSpells", "0"),
    ("NP_QueueSpellsOnCooldown", "1"),
    ("NP_InterruptChannelsOutsideQueueWindow", "0"),
    ("NP_RetryServerRejectedSpells", "1"),
    ("NP_QuickcastTargetingSpells", "0"),
    ("NP_QuickcastOnDoubleCast", "0"),
    ("NP_ReplaceMatchingNonGcdCategory", "0"),
    ("NP_OptimizeBufferUsingPacketTimings", "0"),
    ("NP_PreventRightClickTargetChange", "0"),
    ("NP_PreventRightClickPvPAttack", "0"),
    ("NP_DoubleCastToEndChannelEarly", "0"),
    ("NP_SpamProtectionEnabled", "1"),
    ("NP_PreserveGreaterDemonAutocast", "1"),
    ("NP_FelguardAutocastData", ""),
    ("NP_DoomguardAutocastData", ""),
    ("NP_InfernalAutocastData", ""),
    ("NP_EnableUnitEventsPet", "1"),
    ("NP_EnableUnitEventsParty", "1"),
    ("NP_EnableUnitEventsRaid", "1"),
    ("NP_EnableUnitEventsMouseover", "1"),
    ("NP_EnableUnitEventsGuid", "1"),
    ("NP_EnableUnitEventsGuidFiltering", "0"),
    ("NP_EnableAuraCastEvents", "0"),
    ("NP_EnableAutoAttackEvents", "0"),
    ("NP_EnableSpellStartEvents", "0"),
    ("NP_EnableSpellGoEvents", "0"),
    ("NP_EnableSpellHealEvents", "0"),
    ("NP_EnableSpellEnergizeEvents", "0"),
    ("NP_PreventMountingWhenBuffCapped", "1"),
    ("NP_EnableEnhancedTooltips", "1"),
    ("NP_EnableLocalSetRaidTarget", "1"),
    ("NP_MinBufferTimeMs", "55"),
    ("NP_NonGcdBufferTimeMs", "100"),
    ("NP_MaxBufferIncreaseMs", "30"),
    ("NP_SpellQueueWindowMs", "500"),
    ("NP_OnSwingBufferCooldownMs", "500"),
    ("NP_ChannelQueueWindowMs", "1500"),
    ("NP_TargetingQueueWindowMs", "500"),
    ("NP_CooldownQueueWindowMs", "250"),
    ("NP_ChannelLatencyReductionPercentage", "75"),
    ("NP_NameplateDistance", "20"),
    ("NP_ChatBubbleDistance", "60"),
    ("NP_ChatBubblesWhisper", "0"),
    ("NP_ChatBubblesRaid", "0"),
    ("NP_ChatBubblesBattleground", "0"),
];

/// The settings the queue and the event layer read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub queue_cast_time_spells: bool,
    pub queue_instant_spells: bool,
    pub queue_on_swing_spells: bool,
    pub queue_channeling_spells: bool,
    pub queue_targeting_spells: bool,
    pub queue_spells_on_cooldown: bool,
    pub interrupt_channels_outside_queue_window: bool,
    pub retry_server_rejected_spells: bool,
    pub quickcast_targeting_spells: bool,
    pub quickcast_on_double_cast: bool,
    pub replace_matching_non_gcd_category: bool,
    pub optimize_buffer_using_packet_timings: bool,
    pub prevent_right_click_target_change: bool,
    pub prevent_right_click_pvp_attack: bool,
    pub double_cast_to_end_channel_early: bool,
    pub spam_protection_enabled: bool,
    pub preserve_greater_demon_autocast: bool,
    pub enable_unit_events_pet: bool,
    pub enable_unit_events_party: bool,
    pub enable_unit_events_raid: bool,
    pub enable_unit_events_mouseover: bool,
    pub enable_unit_events_guid: bool,
    pub enable_unit_events_guid_filtering: bool,
    pub enable_aura_cast_events: bool,
    pub enable_auto_attack_events: bool,
    pub enable_spell_start_events: bool,
    pub enable_spell_go_events: bool,
    pub enable_spell_heal_events: bool,
    pub enable_spell_energize_events: bool,
    pub prevent_mounting_when_buff_capped: bool,
    pub enable_enhanced_tooltips: bool,
    pub enable_local_set_raid_target: bool,
    pub spell_queue_window_ms: u64,
    pub on_swing_buffer_cooldown_ms: u64,
    pub channel_queue_window_ms: u64,
    pub targeting_queue_window_ms: u64,
    pub cooldown_queue_window_ms: u64,
    pub min_buffer_time_ms: u64,
    pub max_buffer_increase_ms: u64,
    pub non_gcd_buffer_time_ms: u64,
    pub channel_latency_reduction_percentage: i64,
    pub chat_bubble_distance: u32,
    /// The nameplate range in yards; 20 is the reference's own.
    pub nameplate_distance: f32,
}

/// `atoi`: the leading integer, 0 for none, as nampower reads every value.
fn atoi(value: &str) -> i64 {
    let t = value.trim_start();
    let (sign, digits) = match t.as_bytes().first() {
        Some(b'-') => (-1, &t[1..]),
        Some(b'+') => (1, &t[1..]),
        _ => (1, t),
    };
    let end = digits
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(digits.len());
    sign * digits[..end].parse::<i64>().unwrap_or(0)
}

impl Settings {
    /// The settings at nampower's defaults.
    pub fn nampower() -> Self {
        let mut s = Self::default();
        for (name, value) in CVARS {
            s.apply(name, value);
        }
        s
    }

    /// Apply one CVar's value, as nampower's `updateFromCvar` does; any other name is ignored.
    /// Returns whether the name was one of ours.
    pub fn apply(&mut self, name: &str, value: &str) -> bool {
        let flag = atoi(value) != 0;
        let ms = atoi(value).max(0) as u64;
        let key = name.to_ascii_lowercase();
        match key.as_str() {
            "np_queuecasttimespells" => self.queue_cast_time_spells = flag,
            "np_queueinstantspells" => self.queue_instant_spells = flag,
            "np_queueonswingspells" => self.queue_on_swing_spells = flag,
            "np_queuechannelingspells" => self.queue_channeling_spells = flag,
            "np_queuetargetingspells" => self.queue_targeting_spells = flag,
            "np_queuespellsoncooldown" => self.queue_spells_on_cooldown = flag,
            "np_interruptchannelsoutsidequeuewindow" => {
                self.interrupt_channels_outside_queue_window = flag
            }
            "np_retryserverrejectedspells" => self.retry_server_rejected_spells = flag,
            "np_quickcasttargetingspells" => self.quickcast_targeting_spells = flag,
            "np_quickcastondoublecast" => self.quickcast_on_double_cast = flag,
            "np_replacematchingnongcdcategory" => self.replace_matching_non_gcd_category = flag,
            "np_optimizebufferusingpackettimings" => {
                self.optimize_buffer_using_packet_timings = flag
            }
            "np_preventrightclicktargetchange" => self.prevent_right_click_target_change = flag,
            "np_preventrightclickpvpattack" => self.prevent_right_click_pvp_attack = flag,
            "np_doublecasttoendchannelearly" => self.double_cast_to_end_channel_early = flag,
            "np_spamprotectionenabled" => self.spam_protection_enabled = flag,
            "np_preservegreaterdemonautocast" => self.preserve_greater_demon_autocast = flag,
            "np_enableuniteventspet" => self.enable_unit_events_pet = flag,
            "np_enableuniteventsparty" => self.enable_unit_events_party = flag,
            "np_enableuniteventsraid" => self.enable_unit_events_raid = flag,
            "np_enableuniteventsmouseover" => self.enable_unit_events_mouseover = flag,
            "np_enableuniteventsguid" => self.enable_unit_events_guid = flag,
            "np_enableuniteventsguidfiltering" => self.enable_unit_events_guid_filtering = flag,
            "np_enableauracastevents" => self.enable_aura_cast_events = flag,
            "np_enableautoattackevents" => self.enable_auto_attack_events = flag,
            "np_enablespellstartevents" => self.enable_spell_start_events = flag,
            "np_enablespellgoevents" => self.enable_spell_go_events = flag,
            "np_enablespellhealevents" => self.enable_spell_heal_events = flag,
            "np_enablespellenergizeevents" => self.enable_spell_energize_events = flag,
            "np_preventmountingwhenbuffcapped" => self.prevent_mounting_when_buff_capped = flag,
            "np_enableenhancedtooltips" => self.enable_enhanced_tooltips = flag,
            "np_enablelocalsetraidtarget" => self.enable_local_set_raid_target = flag,
            "np_minbuffertimems" => self.min_buffer_time_ms = ms,
            "np_nongcdbuffertimems" => self.non_gcd_buffer_time_ms = ms,
            "np_maxbufferincreasems" => self.max_buffer_increase_ms = ms,
            "np_spellqueuewindowms" => self.spell_queue_window_ms = ms,
            "np_onswingbuffercooldownms" => self.on_swing_buffer_cooldown_ms = ms,
            "np_channelqueuewindowms" => self.channel_queue_window_ms = ms,
            "np_targetingqueuewindowms" => self.targeting_queue_window_ms = ms,
            "np_cooldownqueuewindowms" => self.cooldown_queue_window_ms = ms,
            "np_channellatencyreductionpercentage" => {
                self.channel_latency_reduction_percentage = atoi(value)
            }
            "np_chatbubbledistance" => self.chat_bubble_distance = ms as u32,
            "np_nameplatedistance" => {
                if let Ok(d) = value.trim().parse::<f32>() {
                    self.nameplate_distance = d.clamp(0.0, 200.0);
                }
            }
            _ => return key.starts_with("np_"),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_nampowers() {
        let s = Settings::nampower();
        assert!(s.queue_cast_time_spells && s.queue_instant_spells && !s.queue_on_swing_spells);
        assert_eq!(s.min_buffer_time_ms, 55);
        assert_eq!(s.spell_queue_window_ms, 500);
        assert_eq!(s.channel_queue_window_ms, 1500);
        assert_eq!(s.channel_latency_reduction_percentage, 75);
        assert!(s.spam_protection_enabled && s.retry_server_rejected_spells);
    }

    #[test]
    fn values_read_as_atoi_does_and_names_fold_case() {
        let mut s = Settings::nampower();
        assert!(s.apply("np_spellqueuewindowms", "250ms"));
        assert_eq!(s.spell_queue_window_ms, 250);
        assert!(s.apply("NP_QueueInstantSpells", "0"));
        assert!(!s.queue_instant_spells);
        assert!(s.apply("NP_QueueInstantSpells", " 7"));
        assert!(s.queue_instant_spells);
        assert!(!s.apply("autoSelfCast", "1"));
    }
}

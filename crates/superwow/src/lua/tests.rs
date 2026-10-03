use std::sync::Arc;

use benilla_app::ext::UiScript;
use benilla_ui::script::{AuraState, UnitGuids, UnitState};

use super::install;
use crate::{Sw, Unit};

const ME: u64 = 0x10;
const MOB: u64 = 0xF130_0000_4500_0001;
const TOTEM: u64 = 0xF130_0000_1700_0002;

fn sw() -> Sw {
    Sw {
        state: Arc::default(),
        spells: Arc::default(),
    }
}

fn state(guid: u64, name: &str) -> UnitState {
    UnitState {
        exists: true,
        has_object: true,
        guid,
        name: Some(name.into()),
        ..Default::default()
    }
}

/// Us targeting a totem whose shaman is `ME`, a mob marked skull, every unit fed by guid as
/// benilla's feed does for the extra guids.
fn world() -> (Sw, UiScript) {
    let sw = sw();
    {
        let mut st = sw.lock();
        st.player = ME;
        st.marks[7] = MOB;
        st.units.insert(
            TOTEM,
            Unit {
                owner: ME,
                pos: Some([1.0, 2.0, 3.0]),
                ..Default::default()
            },
        );
        st.units.insert(
            MOB,
            Unit {
                target: ME,
                hostile: true,
                pos: Some([4.0, 5.0, 6.0]),
                lootable: true,
                ..Default::default()
            },
        );
        st.units.insert(ME, Unit::default());
    }
    let mut script = UiScript::new().unwrap();
    install(&sw, &mut script);
    script.set_unit_guids(&UnitGuids {
        player: ME,
        target: TOTEM,
        ..Default::default()
    });
    script.set_unit("player", Some(state(ME, "Me")));
    script.set_unit("target", Some(state(TOTEM, "Totem")));
    for (guid, name) in [(ME, "Me"), (TOTEM, "Totem"), (MOB, "Mob")] {
        script.set_unit_by_guid(guid, Some(state(guid, name)));
    }
    script.set_extra_unit_guids(vec![ME, MOB, TOTEM]);
    (sw, script)
}

#[test]
fn the_bootstrap_loads_and_announces_the_mod() {
    let (_, script) = world();
    assert_eq!(script.errors(), Vec::<String>::new());
    let v: String = script.eval("return SUPERWOW_VERSION").unwrap();
    assert_eq!(v, crate::VERSION);
    assert_eq!(script.cvar("FoV").as_deref(), Some("1.57"));
    assert_eq!(script.cvar("NameplateRange").as_deref(), Some("20"));
}

#[test]
fn unit_exists_answers_the_guid() {
    let (_, script) = world();
    let (e, g): (i64, String) = script.eval(r#"return UnitExists("target")"#).unwrap();
    assert_eq!((e, g.as_str()), (1, "0xF130000017000002"));
}

#[test]
fn stock_verbs_take_guid_mark_and_owner_tokens() {
    let (_, script) = world();
    let name = |token: &str| -> Option<String> {
        script
            .eval(&format!(r#"return UnitName("{token}")"#))
            .unwrap()
    };
    assert_eq!(name("0xF130000045000001").as_deref(), Some("Mob"));
    assert_eq!(name("mark8").as_deref(), Some("Mob"));
    assert_eq!(name("mark8target").as_deref(), Some("Me"));
    assert_eq!(name("targetowner").as_deref(), Some("Me"));
    assert_eq!(name("mark1"), None);
    let same: Option<i64> = script
        .eval(r#"return UnitIsUnit("mark8", "0xf130000045000001")"#)
        .unwrap();
    assert_eq!(same, Some(1));
    // Text outside both grammars still raises, as the stock resolver does.
    assert!(script.eval::<()>(r#"return UnitName("focus")"#).is_err());
}

#[test]
fn unit_buff_appends_the_spell_id() {
    let (_, mut script) = world();
    script.set_unit_auras(
        MOB,
        Some(vec![AuraState {
            spell_id: 1243,
            helpful: true,
            icon: Some("Interface\\Icons\\Spell_Holy_WordFortitude".into()),
            count: 1,
            ..Default::default()
        }]),
    );
    let mut guids = UnitGuids {
        player: ME,
        target: TOTEM,
        ..Default::default()
    };
    guids.held.insert(MOB, ME);
    script.set_unit_guids(&guids);
    let (_, _, id): (String, i64, i64) = script.eval(r#"return UnitBuff("mark8", 1)"#).unwrap();
    assert_eq!(id, 1243);
}

#[test]
fn positions_loot_and_marks() {
    let (sw, script) = world();
    let (x, y, z): (f64, f64, f64) = script.eval(r#"return UnitPosition("target")"#).unwrap();
    assert_eq!((x, y, z), (1.0, 2.0, 3.0));
    let hostile: Option<f64> = script.eval(r#"return UnitPosition("mark8")"#).unwrap();
    assert_eq!(hostile, None, "a hostile unit has no position");
    let loot: Option<i64> = script.eval(r#"return CanLootUnit("mark8")"#).unwrap();
    assert_eq!(loot, Some(1));
    script
        .run(r#"SetRaidTarget("target", 8, "local")"#)
        .unwrap();
    let marks = std::mem::take(&mut sw.lock().marks_out);
    assert_eq!((marks[0].icon, marks[0].guid), (7, TOTEM));
}

#[test]
fn switches_answer_their_state() {
    let (_, script) = world();
    let on: i64 = script.eval("return SetAutoloot(1)").unwrap();
    let still: i64 = script.eval("return SetAutoloot()").unwrap();
    let off: i64 = script.eval("return Clickthrough(0)").unwrap();
    assert_eq!((on, still, off), (1, 1, 0));
}

#[test]
fn cast_spell_by_name_at_a_unit_queues_the_book_spell() {
    let (sw, script) = world();
    sw.lock().book = vec![133];
    // No Spell.dbc here: an id casts, a name does not resolve.
    let sent: bool = script.eval(r#"return SW_CastAt(133, "mark8")"#).unwrap();
    assert!(sent);
    let casts = std::mem::take(&mut sw.lock().casts);
    assert_eq!((casts[0].spell_id, casts[0].target), (133, Some(MOB)));
}

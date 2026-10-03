//! `UnitXP(command, ...)`, the DLL's dispatcher (`detoured_UnitXP`): with two or more arguments
//! it answers the command; otherwise, or for a command it does not know, the stock
//! `UnitXP(unit)` answers as before.

use benilla_app::ext::{ExtCombatText, UiScript};
use mlua::{Function, Lua, MultiValue, Value};

use crate::geometry::{self, Meter};
use crate::targeting;
use crate::{State, Ux};

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        Value::String(s) => s.to_str().ok()?.trim().parse().ok(),
        _ => None,
    }
}

/// Install the dispatcher over the stock `UnitXP`.
pub fn install(ux: &Ux, script: &mut UiScript) {
    let lua = script.lua();
    let original: Option<Function> = lua.globals().get("UnitXP").ok();
    let ux = ux.clone();
    let dispatcher = lua.create_function(move |lua, args: MultiValue| {
        if args.len() >= 2 {
            if let Some(answer) = dispatch(lua, &ux, &args)? {
                return Ok(answer);
            }
        }
        match &original {
            Some(f) => f.call::<MultiValue>(args),
            None => Ok(MultiValue::new()),
        }
    });
    let result = dispatcher.and_then(|f| lua.globals().set("UnitXP", f));
    if let Err(e) = result {
        script.report_script_error(&format!("unitxp: {e}"));
    }
}

fn values(lua: &Lua, vals: impl IntoIterator<Item = Value>) -> mlua::Result<Option<MultiValue>> {
    let _ = lua;
    Ok(Some(vals.into_iter().collect()))
}

fn one(v: Value) -> mlua::Result<Option<MultiValue>> {
    Ok(Some(MultiValue::from_vec(vec![v])))
}

fn boolean(b: bool) -> mlua::Result<Option<MultiValue>> {
    one(Value::Boolean(b))
}

fn num(n: f64) -> mlua::Result<Option<MultiValue>> {
    one(Value::Number(n))
}

/// An `enable`/`disable` toggle: apply it, answer the state.
fn toggle(flag: &mut bool, sub: Option<&str>) -> mlua::Result<Option<MultiValue>> {
    match sub {
        Some("enable") => *flag = true,
        Some("disable") => *flag = false,
        _ => {}
    }
    boolean(*flag)
}

/// A `set` value clamped to `[lo, hi]`: apply it, answer the value.
fn setting(
    value: &mut f32,
    args: &MultiValue,
    lo: f32,
    hi: f32,
) -> mlua::Result<Option<MultiValue>> {
    if text(args.get(1)).as_deref() == Some("set") {
        if let Some(n) = number(args.get(2)) {
            *value = (n as f32).clamp(lo, hi);
        }
    }
    num(f64::from(*value))
}

/// The measured candidates for the target modes.
fn candidates(st: &mut State) -> Vec<targeting::Candidate> {
    let me = st.player;
    let Some(mine) = st.units.get(&me).copied() else {
        return Vec::new();
    };
    let cone = st.settings.tuning.cone;
    let cam = st.camera;
    let guids: Vec<u64> = st.units.keys().copied().filter(|g| *g != me).collect();
    guids
        .into_iter()
        .filter_map(|g| {
            let u = *st.units.get(&g)?;
            let in_sight = st.sight(me, g).unwrap_or(false);
            Some(targeting::Candidate {
                guid: g,
                is_player: u.body.is_player,
                player_controlled: u.flags & crate::FLAG_PLAYER_CONTROLLED != 0,
                can_attack: u.can_attack,
                dead: u.health == 0,
                in_combat: u.in_combat(),
                critter: u.creature_type == 8,
                world_boss: u.rank == 3,
                distance: geometry::distance(&mine.body, &u.body, Meter::Gaussian),
                ranged: geometry::distance(&mine.body, &u.body, Meter::Ranged),
                melee: geometry::distance(&mine.body, &u.body, Meter::MeleeAutoAttack),
                health: u.health,
                in_cone: cam.is_none_or(|(c, f)| geometry::in_cone(c, f, u.body.pos, cone)),
                in_sight,
                mark: st
                    .marks
                    .iter()
                    .position(|m| *m == g)
                    .map_or(0, |i| i as u8 + 1),
            })
        })
        .collect()
}

fn target(st: &mut State, args: &MultiValue) -> mlua::Result<Option<MultiValue>> {
    let sub = text(args.get(1)).unwrap_or_default();
    let tuning = st.settings.tuning;
    match sub.as_str() {
        "rangeCone" => {
            if let Some(n) = number(args.get(2)).filter(|n| *n > 1.99 && *n < f64::from(f32::MAX)) {
                st.settings.tuning.cone = n as f32;
            }
            return num(f64::from(st.settings.tuning.cone));
        }
        "farRange" => {
            if let Some(n) = number(args.get(2)).filter(|n| *n > 25.0 && *n < 61.0) {
                st.settings.tuning.far_range = n as f32;
            }
            return num(f64::from(st.settings.tuning.far_range));
        }
        "disableInCombatFilter" => {
            st.settings.tuning.in_combat_filter = false;
            return boolean(false);
        }
        "enableInCombatFilter" => {
            st.settings.tuning.in_combat_filter = true;
            return boolean(true);
        }
        _ => {}
    }
    let cands = candidates(st);
    let in_combat = st.units.get(&st.player).is_some_and(|u| u.in_combat());
    let current = (st.selection != 0).then_some(st.selection);
    let pick = match sub.as_str() {
        "nearestEnemy" => targeting::nearest(&cands, f32::MAX, &tuning, in_combat),
        "nextEnemyConsideringDistance" => {
            targeting::considering_distance(&cands, current, true, &tuning, in_combat)
        }
        "previousEnemyConsideringDistance" => {
            targeting::considering_distance(&cands, current, false, &tuning, in_combat)
        }
        "nextEnemyInCycle" => targeting::in_cycle(&cands, current, true, &tuning, in_combat),
        "previousEnemyInCycle" => targeting::in_cycle(&cands, current, false, &tuning, in_combat),
        "nextMarkedEnemyInCycle" | "previousMarkedEnemyInCycle" => {
            let priority = targeting::mark_priority(&text(args.get(2)).unwrap_or_default());
            let forward = sub.starts_with("next");
            targeting::marked_in_cycle(&cands, current, forward, &priority, &tuning, in_combat)
        }
        "mostHP" => targeting::most_hp(&cands, &tuning, in_combat),
        "worldBoss" => targeting::world_boss(&cands, current, &tuning, in_combat),
        _ => return one(Value::Nil),
    };
    if let Some(guid) = pick {
        st.select(guid);
    }
    boolean(pick.is_some())
}

/// One command; `None` hands the call to the stock `UnitXP`.
fn dispatch(lua: &Lua, ux: &Ux, args: &MultiValue) -> mlua::Result<Option<MultiValue>> {
    let cmd = text(args.front()).unwrap_or_default();
    let sub = text(args.get(1));
    let sub = sub.as_deref();
    let mut st = ux.lock();
    match cmd.as_str() {
        "nop" => boolean(true),
        "inSight" if args.len() >= 3 => {
            let (Some(a), Some(b)) = (
                sub.and_then(|t| st.resolve(t)),
                text(args.get(2)).and_then(|t| st.resolve(&t)),
            ) else {
                return one(Value::Nil);
            };
            match st.sight(a, b) {
                Some(seen) => boolean(seen),
                None => one(Value::Nil),
            }
        }
        "distanceBetween" if args.len() >= 3 => {
            let meter = Meter::from_name(text(args.get(3)).as_deref());
            let a = sub
                .and_then(|t| st.resolve(t))
                .and_then(|g| st.units.get(&g));
            let b = text(args.get(2))
                .and_then(|t| st.resolve(&t))
                .and_then(|g| st.units.get(&g));
            match (a, b) {
                (Some(a), Some(b)) => num(f64::from(geometry::distance(&a.body, &b.body, meter))),
                _ => one(Value::Nil),
            }
        }
        "behind" if args.len() >= 3 => {
            let me = sub.and_then(|t| st.resolve(t));
            let mob = text(args.get(2)).and_then(|t| st.resolve(&t));
            let (Some(me), Some(mob)) = (me, mob) else {
                return one(Value::Nil);
            };
            if me == mob {
                return one(Value::Nil);
            }
            let (Some(a), Some(b)) = (st.units.get(&me).copied(), st.units.get(&mob).copied())
            else {
                return one(Value::Nil);
            };
            // A creature standing in melee faces its target, whatever its stale facing says.
            let mut facing = None;
            if !b.body.is_player && b.in_combat() && !b.moving && b.target != 0 {
                if let Some(t) = st.units.get(&b.target).copied() {
                    if st.sight(mob, b.target) == Some(true) {
                        facing = Some(t.body.pos - b.body.pos);
                    }
                }
            }
            let threshold = st.settings.behind_threshold;
            boolean(geometry::behind(&a.body, &b.body, facing, threshold))
        }
        "behindThreshold" => {
            if sub == Some("set") {
                if let Some(n) = number(args.get(2)) {
                    st.settings.behind_threshold = (n as f32).clamp(0.0, std::f32::consts::PI);
                }
            }
            num(f64::from(st.settings.behind_threshold))
        }
        "target" => target(&mut st, args),
        "onEvent" => match sub {
            Some(
                "PLAYER_ENTERING_WORLD"
                | "PLAYER_LEAVING_WORLD"
                | "PLAYER_REGEN_ENABLED"
                | "PLAYER_REGEN_DISABLED",
            ) => boolean(true),
            _ => one(Value::Nil),
        },
        "timer" => match sub {
            Some("arm") if args.len() >= 5 => {
                let (Some(first), Some(period), Some(handler)) =
                    (number(args.get(2)), number(args.get(3)), text(args.get(4)))
                else {
                    return one(Value::Nil);
                };
                let id = st.arm_timer(first.max(0.0) as u64, period.max(0.0) as u64, handler);
                num(f64::from(id))
            }
            Some("disarm") => match number(args.get(2)) {
                Some(id) => boolean(st.disarm_timer(id as u32)),
                None => one(Value::Nil),
            },
            Some("size") => num(st.timer_count() as f64),
            _ => one(Value::Nil),
        },
        "notify" => match sub {
            Some("taskbarIcon") => {
                st.flash = !st.focused;
                boolean(true)
            }
            Some("systemSound") => {
                let name = text(args.get(2)).unwrap_or_default();
                let ok = !st.focused && crate::notify::SOUNDS.contains(&name.as_str());
                if ok {
                    st.sound = Some(name);
                }
                boolean(ok)
            }
            _ => one(Value::Nil),
        },
        "debug" if sub == Some("breakpoint") => num(0.0),
        "modernNameplateDistance" => toggle(&mut st.settings.modern_nameplates, sub),
        "hideCritterNameplate" => toggle(&mut st.settings.hide_critter_nameplate, sub),
        "prioritizeTargetNameplate" => toggle(&mut st.settings.prioritize_target_nameplate, sub),
        "prioritizeMarkedNameplate" => toggle(&mut st.settings.prioritize_marked_nameplate, sub),
        "nameplateCombatFilter" => toggle(&mut st.settings.nameplate_combat_filter, sub),
        "showInCombatNameplatesNearPlayer" => {
            toggle(&mut st.settings.in_combat_nameplates_near_player, sub)
        }
        "FPScap" | "backgroundFPScap" => {
            let cap = if cmd == "FPScap" {
                &mut st.settings.fps_cap
            } else {
                &mut st.settings.background_fps_cap
            };
            if let Some(v) = number(args.get(1)) {
                *cap = if v < 1.0 { 0.0 } else { v.min(500.0).floor() };
            }
            num(*cap)
        }
        "cameraHeight" => setting(&mut st.settings.camera.vertical, args, 0.0, 4.0),
        "cameraVerticalDisplacement" => setting(&mut st.settings.camera.vertical, args, -1.0, 4.0),
        "cameraHorizontalDisplacement" => {
            setting(&mut st.settings.camera.horizontal, args, -4.0, 4.0)
        }
        "cameraPitch" => setting(&mut st.settings.camera.pitch, args, 0.0, 0.3),
        "cameraFollowTarget" => toggle(&mut st.settings.camera.follow_target, sub),
        "cameraOrganicSmooth" => toggle(&mut st.settings.camera.organic_smooth, sub),
        "cameraPinHeight" => toggle(&mut st.settings.camera.pin_height, sub),
        "version" => match sub {
            Some("coffTimeDateStamp") => num(crate::COFF_TIME_DATE_STAMP),
            Some("additionalInformation") => {
                one(Value::String(lua.create_string("benilla-unitxp")?))
            }
            _ => Ok(None),
        },
        "weatherAlwaysClear" => {
            let s = &mut st.settings;
            match sub {
                Some("enable") => {
                    (s.no_rain, s.no_snow, s.no_sandstorm) = (true, true, true);
                    boolean(true)
                }
                Some("disable") => {
                    (s.no_rain, s.no_snow, s.no_sandstorm) = (false, false, false);
                    boolean(false)
                }
                Some("enableRain") => {
                    s.no_rain = false;
                    boolean(true)
                }
                Some("disableRain") => {
                    s.no_rain = true;
                    boolean(false)
                }
                Some("enableSnow") => {
                    s.no_snow = false;
                    boolean(true)
                }
                Some("disableSnow") => {
                    s.no_snow = true;
                    boolean(false)
                }
                Some("enableSandstorm") => {
                    s.no_sandstorm = false;
                    boolean(true)
                }
                Some("disableSandstorm") => {
                    s.no_sandstorm = true;
                    boolean(false)
                }
                _ => values(
                    lua,
                    [
                        Value::Boolean(!s.no_rain),
                        Value::Boolean(!s.no_snow),
                        Value::Boolean(!s.no_sandstorm),
                    ],
                ),
            }
        }
        "hideEXPtext" => toggle(&mut st.settings.hide_exp_text, sub),
        // benilla draws combat text with its own renderer, so the SP3 renderer reads as off
        // (`scene_isEnabled` false) and its settings are kept for their getters.
        "combatTextSP3" => {
            let s = &mut st.settings;
            let first = match sub {
                Some("enable") => {
                    s.combat_text_sp3 = true;
                    Value::Boolean(true)
                }
                Some("disable") => {
                    s.combat_text_sp3 = false;
                    Value::Boolean(false)
                }
                Some("setFontSize") => match number(args.get(2)) {
                    Some(n) => {
                        s.font_size = n.clamp(10.0, 100.0).floor();
                        Value::Number(s.font_size)
                    }
                    None => return one(Value::Nil),
                },
                Some("setNameplateHeight") => match number(args.get(2)) {
                    Some(n) => {
                        s.nameplate_height = n.clamp(0.0, 256.0);
                        Value::Number(s.nameplate_height)
                    }
                    None => return one(Value::Nil),
                },
                Some("setFontName") => match text(args.get(2)) {
                    Some(name) => {
                        s.font_name = name;
                        Value::String(lua.create_string(&s.font_name)?)
                    }
                    None => return one(Value::Nil),
                },
                Some("debugText") => {
                    Value::String(lua.create_string("benilla renders combat text")?)
                }
                _ => return one(Value::Nil),
            };
            values(lua, [first, Value::Boolean(false)])
        }
        "addCombatText" if args.len() >= 6 => {
            let kind = sub.unwrap_or_default();
            let words = text(args.get(2)).unwrap_or_default();
            let channel = |i: usize| {
                let n = number(args.get(i)).unwrap_or(1.0) * 255.0;
                if (0.0..256.0).contains(&n) {
                    n as u32
                } else {
                    255
                }
            };
            let color = 0xFF00_0000 | channel(3) << 16 | channel(4) << 8 | channel(5);
            st.text(ExtCombatText {
                guid: None,
                text: words,
                category: if kind == "crit" { 2 } else { 0 },
                color: Some(color),
            });
            boolean(true)
        }
        "performanceProfile" => one(Value::String(
            lua.create_string("benilla: frame timing lives in its own perf panel")?,
        )),
        // `vanilla1121_gameLocale`: benilla reads its install's locale for the DBC strings and
        // runs the enUS index.
        "gameLocale" => num(0.0),
        "screenshot" => {
            st.settings.screenshot = u8::from(sub == Some("perfect"));
            // benilla saves every screenshot as PNG.
            num(1.0)
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> (Ux, UiScript) {
        let ux = Ux::default();
        let mut script = UiScript::new().unwrap();
        script.run("function UnitXP(u) return 42 end").unwrap();
        install(&ux, &mut script);
        (ux, script)
    }

    #[test]
    fn one_argument_falls_through_to_the_stock_function() {
        let (_, script) = vm();
        assert_eq!(script.eval::<i64>("return UnitXP('player')").unwrap(), 42);
        assert!(script.eval::<bool>("return UnitXP('nop', 'nop')").unwrap());
        assert_eq!(
            script.eval::<i64>("return UnitXP('unknown', 'x')").unwrap(),
            42
        );
    }

    #[test]
    fn settings_round_trip_with_their_clamps() {
        let (ux, script) = vm();
        assert_eq!(
            script
                .eval::<f64>("return UnitXP('cameraPitch', 'set', 9)")
                .unwrap(),
            0.3f32 as f64
        );
        assert_eq!(
            script.eval::<f64>("return UnitXP('FPScap', 9000)").unwrap(),
            500.0
        );
        assert!(!script
            .eval::<bool>("return UnitXP('hideCritterNameplate', 'disable')")
            .unwrap());
        let (rain, snow, sand): (bool, bool, bool) =
            script.eval("UnitXP('weatherAlwaysClear', 'disableSnow') return UnitXP('weatherAlwaysClear', 'x')").unwrap();
        assert!(rain && !snow && sand);
        assert!(ux.lock().settings.no_snow);
    }

    #[test]
    fn timers_arm_count_and_disarm() {
        let (_, script) = vm();
        let id: f64 = script
            .eval("return UnitXP('timer', 'arm', 100, 0, 'F')")
            .unwrap();
        assert_eq!(
            script
                .eval::<f64>("return UnitXP('timer', 'size')")
                .unwrap(),
            1.0
        );
        assert!(script
            .eval::<bool>(&format!("return UnitXP('timer', 'disarm', {id})"))
            .unwrap());
    }
}

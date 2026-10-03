# UnitXP SP3 on benilla

A port of UnitXP Service Pack 3 to benilla, from
[brues-code/UnitXP_SP3](https://github.com/brues-code/UnitXP_SP3). That fork is the maintained
one; the original `allfoxwy/UnitXP_SP3` repository no longer exists. The DLL widens the stock
`UnitXP(unit)` into `UnitXP(command, ...)`. This crate installs the same dispatcher through
benilla's extension hooks (`benilla_app::ext`, plus two in `benilla_world`). With one argument,
or an unknown command, the stock `UnitXP` answers as before.

```text
cargo run -p unitxp --bin benilla-unitxp        # UnitXP alone
cargo run -p benilla-mods --bin benilla-mods    # UnitXP and nampower together
```

## Layout

| UnitXP source | here |
| --- | --- |
| `dllmain.cpp` (the dispatcher) | `lua.rs` |
| `distanceBetween.cpp`, `inSight.cpp` | `geometry.rs`, line of sight traced in `lib.rs` |
| `targeting.cpp` | `targeting.rs` |
| `modernNameplateDistance.cpp` | `State::wants_nameplate` in `lib.rs` |
| `editCamera.cpp` | `camera.rs` |
| `FPScap.cpp` | `fps.rs` |
| `notifyOS.cpp` | `notify.rs` |
| `timer.cpp`, `cpptime.h` | the timers in `lib.rs` |
| `weather.cpp` | the weather mask in `lib.rs` |

`geometry.rs` and `targeting.rs` are pure functions, unit-tested on their own:
`cargo test -p unitxp`.

## What it touches in benilla

Beyond nampower's hooks (see `crates/nampower/README.md`):

| file | change |
| --- | --- |
| `benilla-app/src/ext.rs` | `ExtSelect`, `ExtCombatText`, `CombatTextHook`, `NameplateHook`, `ExtWorld` |
| `benilla-app/src/vplates.rs` | reads `NameplateHook`: a range override and hidden units |
| `benilla-app/src/combat_text/mod.rs` | reads `CombatTextHook`: XP numbers off |
| `benilla-world/src/collision.rs` | `WorldCollision::sight`, the line-of-sight ray |
| `benilla-world/src/weather/mod.rs` | `WeatherState::suppressed` |

## Where it differs from the DLL

- **Line of sight** is traced once a frame: from us to every unit within 100 yd, and between
  any other two units for 2 s after Lua asks about them. The first `inSight` question about a new
  pair that doesn't involve us answers `nil` until the next frame. The DLL instead caches each
  answer for about 100 ms.
- **Eye height** is 2 yd times the unit's scale. The DLL reads the model's collision box height,
  which benilla does not publish.
- **Combat text**: benilla's own floating text renders everything. `addCombatText` and
  `hideEXPtext` work through it. `combatTextSP3` keeps its settings for its getters and reports
  its renderer off. The SP3 fonts, crit grouping and arcs are not drawn.
- **Camera**: the displacements, pitch and target follow apply on top of benilla's camera.
  `cameraOrganicSmooth` and `cameraPinHeight` are kept for their getters only.
- **Settings** live for the session, as in the DLL; an addon re-applies them at load.
- **`notify`** works on Windows only, as in the DLL.
- **`screenshot`**: benilla always saves PNG.
- **`gameLocale`** answers 0.
- **`debug breakpoint`** answers 0.
- **Not ported**: the DLL's patches to the client's math, renderer, sockets and quit path. They
  have no counterpart in benilla.

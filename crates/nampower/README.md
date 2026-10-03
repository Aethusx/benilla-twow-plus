# nampower on benilla

A port of [nampower](https://github.com/brues-code/nampower) (4.6.1) to benilla: the spell queue,
its `NP_` CVars, its Lua API and its events. The DLL detours `WoW.exe`'s functions. This crate
reaches the same behaviour through benilla's extension seams (`benilla_app::ext`), and benilla
itself knows nothing about nampower.

```text
cargo run -p nampower --bin benilla-nampower            # dev build
cargo build -p nampower --no-default-features --release # player build
```

The launcher shares the workspace's `WoW` link, `benilla-config/` and `.probe-identity`. Its build
line reads `extended`.

## Layout

| nampower source | here |
| --- | --- |
| `main.cpp` (CVars, `processQueues`, buffer, channel end) | `settings.rs`, `engine.rs` |
| `spellcast.cpp` (`Spell_C_CastSpellHook`, `BeginCast`) | `engine.rs`, `gate.rs` |
| `spellevents.cpp`, `spellchannel.cpp` (packet hooks) | `net.rs`, `engine.rs` |
| `castqueue.h` | `queue.rs` |
| `dbc_fields.cpp`, `game.hpp` `SpellRec` | `dbc.rs` |
| `unit_fields.cpp` | `lua/units.rs` |
| `spell_scripts.cpp`, `misc_scripts.cpp` | `lua/spells.rs`, `lua/casting.rs` |
| `item_scripts.cpp`, `items.cpp` | `lua/items.rs` |
| `cooldown_scripts.cpp` | `lua/cooldowns.rs` |
| `file_scripts.cpp` | `lua/files.rs` |
| `auras.cpp`, unit GUID events | `events.rs` |
| `CSimpleTop` key hooks | `keys.rs` |
| `DisenchantAll`, `UseItemIdOrName`, `UseTrinket`, `LearnTalentRank`, the widened spellbook verbs | `lua/nampower.lua` |

`engine.rs` holds no Bevy state and reads no clock, so the queue logic is unit-tested on its own:
`cargo test -p nampower`.

## What it touches in benilla

Every hook nampower needs lives in **`crates/benilla-app/src/ext.rs`**. The rest of benilla
changes only at these call sites, so they are the only places an upstream merge can conflict:

| file | change |
| --- | --- |
| `lib.rs` | `pub mod ext;` |
| `game_plugins.rs` | adds `ext::ExtPlugin` |
| `net/handlers.rs` | `NetHandlerApp` is `pub` |
| `spell/cast_send.rs` | `CastLadder` carries the gate: `send_gated`, `send_requeued`, the targeted-commit call |
| `spell/inflight.rs` | `PendingCast::release`, `PendingCast::stamp` |
| `spell/cast_target.rs` | `CastTargeting::context_at` |
| `spell/cooldowns.rs` | `Cooldowns::export` |
| `spell/mods.rs` | `SpellModifiers::sums` |
| `items.rs` | `Items::cached` |
| `target/mod.rs` | `ext::apply_ext_casts` ahead of the script calls |
| `ui_party/mod.rs` | `GroupState` is `pub(crate)` |
| `ui_script/lifecycle.rs` | runs the script installers before the UI loads |
| `benilla-ui/src/script/tick.rs` | `UiScript::has_event_registrations` |

With no extension installed, every hook does nothing. Stock `benilla` behaves exactly as upstream.

## Syncing with upstream benilla

```text
scripts/nampower-sync.sh            # fetch upstream, merge upstream/main, build and test
```

The script refuses to run on a dirty tree. A conflict can only land in the files listed above.
Resolve it by keeping upstream's version and re-applying the hook's few lines. If upstream renames
something a hook reads, the compiler names the line in `ext.rs` to fix.

## Updating to a new nampower release

Diff nampower between the two tags, find each changed file in the layout table, and port the
change to the matching module. `VERSION` in `lib.rs` is what `GetNampowerVersion` reports. New
CVars go in `settings::CVARS` and `Settings::apply`, and new events go in `events::ALL`.

## Where it differs from the DLL

- **Settings** are `NP_` CVars that the bootstrap declares with `RegisterCVar`, the way an addon
  declares its own. They persist in `benilla-config/config.toml`.
- **`NP_NameplateDistance`** sets benilla's nameplate range through `ext::NameplateHook`; its
  default is the reference's 20 yd, where the DLL reads the game's current value.
- **Events** fire only once some frame registers them, which is how nampower 4.5+ enables its
  gated events. The `NP_Enable*Events` toggles are still accepted.
- **Files**: `CustomData/` and `Imports/` live in `benilla-config/`, not beside `WoW.exe`.
  benilla never writes into the install.
- **Reusable tables**: every table-returning function returns a fresh table, so the `copy`
  argument changes nothing and is always safe.
- **Ground spells**: a queued ground-targeted spell raises the cursor when it pops. There is no
  quickcast at the mouse (`NP_QuickcastTargetingSpells`, `NP_QuickcastOnDoubleCast`).
- **`LearnTalentRank`** learns one rank per call, through the stock `LearnTalent`.
- **Not ported**: patches to fixed `WoW.exe` addresses with no benilla counterpart. That covers
  the chat-bubble patch and its distance, right-click target guards, greater
  demon autocast memory, enhanced tooltips, `SetMouseoverUnit`, the extended unit tokens inside
  the *stock* unit functions (`GetUnitGUID` and every nampower function do accept them), the
  `NP_EnableUnitEvents{Pet,Party,Raid,Mouseover}` toggles, and the glue-only
  `EncryptPassword`/`EncryptedServerLogin`.

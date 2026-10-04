//! `macro/ShowTooltip.cpp`, with `Spell.cpp`, `Item.cpp`, `IconPath.cpp` and `Display.cpp`:
//! `#showtooltip` / `#show` macros, and `GetMacroSpell`, `GetMacroItem`, `C_Macro.GetMacroIcon`
//! and `C_Macro.SetMacroDisplay`.
//!
//! - Parse. Line 1 must be `#showtooltip [args]` or `#show [args]`. With args, that line is the
//!   option list. Bare, the args of each later `/cast`, `/use`, `/castsequence`, `/castrandom` or
//!   `/userandom` line (the live `SLASH_*%d` names), up to and including the first whose args do
//!   not start with `[`.
//! - Evaluate. A line with `[` or `;` goes through the bundled addon's `SecureCmdOptionParse`
//!   (quiet), a plain line as is; the first non-empty value wins, a `/castsequence` value replaced
//!   by its current step (`QueryCastSequence`). A line using a condition the parser does not own
//!   belongs to another macro addon, and the macro is left alone.
//! - Resolve. `spell:N`; `bag slot` and equipment slots 1-19 to the item there; a carried item by
//!   name, link or `item:N`; an explicit `item:N` even when not carried; a spell in the book by
//!   name or id; an explicit spell id from `Spell.dbc` alone.
//! - Show. The result goes to benilla's [`ExtMacroDisplay`], which the slot's state, its icon
//!   (a question-mark macro) and an item's count follow, as the DLL's write to the macro's
//!   cached cast (`+0x564`) makes the engine follow it.
//! - Cadence. A conditional directive re-evaluates when a modifier, the mouseover, the target or
//!   combat changes, and every 200 ms; a static one every second; a macro edit re-parses.
//! - Tooltip (`tooltip/SetAction.cpp`). `GameTooltip:SetAction` on a `#showtooltip` macro shows
//!   the spell (`SetSpell` on its book slot) or the item (`SetInventoryItem`, `SetBagItem`, else an
//!   `item:` hyperlink), refreshing while its directive is conditional; anything else, `#show`
//!   included, is the stock tooltip, which for a macro is its name.
//! - Stand-down. With SuperCleveRoidMacros loaded and not disabled, and not announcing
//!   `ClassicAPIMacroDisplay`, it owns macro display and only published macros are shown.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use benilla_app::ext::{ExtMacroDisplay, MacroShow};
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Value};

use crate::lua::{to_int, to_str, truthy, Api};
use crate::mirror::field;
use crate::Ca;

/// Macros 1-36: the account list, then the character list.
const MACROS: u32 = 36;
const MAX_OPTION_LINES: usize = 8;
const CONDITIONAL_EVERY: Duration = Duration::from_millis(200);
const STATIC_EVERY: Duration = Duration::from_secs(1);
const YIELD_CHECK_EVERY: Duration = Duration::from_secs(1);
const QUESTION_MARK: &str = "Interface\\Icons\\INV_Misc_QuestionMark";
/// `UNIT_FLAG_IN_COMBAT`.
const IN_COMBAT: u32 = 0x8_0000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    None,
    Show,
    ShowTooltip,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Target {
    #[default]
    None,
    Spell(u32),
    Item(u32),
}

#[derive(Clone, Debug, Default)]
struct Parsed {
    kind: Kind,
    conditional: bool,
    explicit: bool,
    /// Each option line, and whether its value is a `/castsequence` sequence.
    options: Vec<(String, bool)>,
}

#[derive(Clone, Debug, Default)]
struct Entry {
    body: String,
    parsed: Parsed,
    /// Another macro addon's conditions: not ours to describe.
    foreign: bool,
    /// Published through `C_Macro.SetMacroDisplay`: the publisher owns it.
    external: bool,
    /// The last evaluation had a value, even one that resolved to nothing.
    matched: bool,
    target: Target,
    last_eval: Option<Instant>,
}

impl Entry {
    fn managed(&self) -> bool {
        self.external
            || (self.parsed.kind != Kind::None && !self.parsed.options.is_empty() && !self.foreign)
    }

    fn show(&self) -> MacroShow {
        match self.target {
            Target::Spell(id) => MacroShow::Spell(id),
            Target::Item(id) => MacroShow::Item(id),
            Target::None if self.matched => MacroShow::Unresolved,
            Target::None => MacroShow::Nothing,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct World {
    modifiers: u8,
    mouseover: u64,
    target: u64,
    combat: bool,
}

#[derive(Default)]
pub struct ShowTooltip {
    entries: HashMap<u32, Entry>,
    generation: Option<u64>,
    slash: Option<Vec<(String, bool)>>,
    yielding: bool,
    yield_checked: Option<Instant>,
    world: World,
}

/// `#showtooltip` / `#show` at the start of the line, then end or blank; the rest, trimmed.
fn directive(line: &str) -> Option<(Kind, &str)> {
    let line = line.trim_start_matches([' ', '\t']);
    for (word, kind) in [("#showtooltip", Kind::ShowTooltip), ("#show", Kind::Show)] {
        let Some(head) = line.get(..word.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(word) {
            continue;
        }
        let rest = &line[word.len()..];
        if rest.is_empty() || rest.starts_with([' ', '\t']) {
            return Some((kind, rest.trim_matches([' ', '\t'])));
        }
    }
    None
}

/// The args after a cast-family command, the command and a space; whether it is a sequence.
fn cast_args<'a>(line: &'a str, slash: &[(String, bool)]) -> Option<(&'a str, bool)> {
    let line = line.trim_start_matches([' ', '\t']);
    slash.iter().find_map(|(name, seq)| {
        let head = line.get(..name.len())?;
        (head.eq_ignore_ascii_case(name) && line[name.len()..].starts_with(' '))
            .then(|| (line[name.len()..].trim_start_matches([' ', '\t']), *seq))
    })
}

fn parse(body: &str, slash: &[(String, bool)]) -> Parsed {
    let mut out = Parsed::default();
    let mut lines = body.split(['\r', '\n']).filter(|l| !l.is_empty());
    let Some((kind, args)) = lines.next().and_then(directive) else {
        return out;
    };
    out.kind = kind;
    let push = |out: &mut Parsed, args: &str, seq: bool| -> bool {
        let args = args.trim_matches([' ', '\t']);
        if args.is_empty() || out.options.len() >= MAX_OPTION_LINES {
            return false;
        }
        out.conditional |= args.contains('[');
        out.options.push((args.to_string(), seq));
        true
    };
    if !args.is_empty() {
        out.explicit = true;
        push(&mut out, args, false);
        return out;
    }
    for line in lines {
        if out.options.len() >= MAX_OPTION_LINES {
            break;
        }
        let Some((args, seq)) = cast_args(line, slash) else {
            continue;
        };
        if push(&mut out, args, seq) && !args.trim_start().starts_with('[') {
            break;
        }
    }
    out
}

/// The live command names, `SLASH_CAST%d` and its families in 3.3.5's order.
fn slash_names(lua: &Lua) -> Vec<(String, bool)> {
    let g = lua.globals();
    let mut out = Vec::new();
    for (family, seq) in [
        ("SLASH_CAST", false),
        ("SLASH_USE", false),
        ("SLASH_CASTSEQUENCE", true),
        ("SLASH_CASTRANDOM", false),
        ("SLASH_USERANDOM", false),
    ] {
        for i in 1.. {
            match g.raw_get::<Value>(format!("{family}{i}")) {
                Ok(Value::String(s)) if !s.as_bytes().is_empty() => {
                    out.push((s.to_string_lossy(), seq));
                }
                _ => break,
            }
        }
    }
    out
}

/// One option line's value: a plain line as is, else `SecureCmdOptionParse`'s first clause.
/// `Err(())` for a line using a condition another addon owns.
fn option_value(lua: &Lua, options: &str) -> Result<Option<String>, ()> {
    if !options.contains('[') && !options.contains(';') {
        return Ok(Some(options.trim().to_string()));
    }
    let Ok(parse) = lua.globals().get::<Function>("SecureCmdOptionParse") else {
        return Ok(None);
    };
    let Ok(ret) = parse.call::<MultiValue>((options, true)) else {
        return Ok(None);
    };
    let ret: Vec<Value> = ret.into_iter().collect();
    if matches!(ret.get(2), Some(Value::String(_))) {
        return Err(());
    }
    Ok(match ret.first() {
        Some(Value::String(s)) => Some(s.to_string_lossy().trim().to_string()),
        _ => None,
    })
}

/// `QueryCastSequence`'s current step, the item before the spell.
fn sequence_step(lua: &Lua, sequence: &str) -> Option<String> {
    let f: Function = lua.globals().get("QueryCastSequence").ok()?;
    let ret: Vec<Value> = f.call::<MultiValue>(sequence).ok()?.into_iter().collect();
    let pick = |v: Option<&Value>| match v {
        Some(Value::String(s)) if !s.as_bytes().is_empty() => Some(s.to_string_lossy()),
        _ => None,
    };
    pick(ret.get(1))
        .or_else(|| pick(ret.get(2)))
        .map(|s| s.trim().to_string())
}

fn uint(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// `ResolveValue`: item forms first, then a carried item, then a spell.
fn resolve(lua: &Lua, ca: &Ca, value: &str, explicit: bool) -> Target {
    let spell_known = |id: u32| crate::spells::table(&ca.db).is_some_and(|t| t.row(id).is_some());
    if let Some(rest) = value
        .get(..6)
        .filter(|h| h.eq_ignore_ascii_case("spell:"))
        .map(|_| value[6..].trim())
    {
        return uint(rest)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|id| *id > 0 && spell_known(*id))
            .map_or(Target::None, Target::Spell);
    }
    let item_at = |guid: Option<u64>| {
        let st = ca.lock();
        guid.map(|g| crate::items::item_id(&st.mirror, g))
            .filter(|id| *id > 0)
            .map_or(Target::None, Target::Item)
    };
    let words: Vec<&str> = value.split_whitespace().collect();
    if let [bag, slot] = words[..] {
        if let (Some(bag), Some(slot)) = (uint(bag), uint(slot)) {
            let guid = crate::items::bag_slot(&ca.lock().mirror, bag, slot);
            return item_at(guid);
        }
    }
    let numeric = uint(value);
    if let Some(n) = numeric.filter(|n| (1..=19).contains(n)) {
        let guid = crate::items::equipment_slot(&ca.lock().mirror, n);
        return item_at(guid);
    }
    if numeric.is_none() {
        let arg = crate::items::resolve_string(value);
        let carried = crate::items::Carried::of(&ca.lock().mirror);
        if let Some(found) = carried.find(lua, &arg) {
            if found.item.entry > 0 {
                return Target::Item(found.item.entry);
            }
        }
        if arg.item_id > 0 {
            return Target::Item(arg.item_id as u32);
        }
    }
    let name = value.trim_start_matches('!').trim();
    let book = match numeric {
        Some(n) => super::spell::find_book_slot(lua, n).map(|_| n),
        None => Some(super::spell::name_to_spell_id(lua, name)).filter(|id| *id > 0),
    };
    if let Some(id) = book.and_then(|n| u32::try_from(n).ok()) {
        return Target::Spell(id);
    }
    match numeric.and_then(|n| u32::try_from(n).ok()) {
        Some(id) if explicit && id > 0 && spell_known(id) => Target::Spell(id),
        _ => Target::None,
    }
}

/// `Evaluate`: the first option line with a value, resolved. `None` when the macro turned out
/// to be another addon's.
fn evaluate(lua: &Lua, ca: &Ca, parsed: &Parsed) -> Option<(bool, Target)> {
    for (options, seq) in &parsed.options {
        let mut value = option_value(lua, options).ok()?;
        if *seq {
            value = value
                .filter(|v| !v.is_empty())
                .and_then(|v| sequence_step(lua, &v));
        }
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            return Some((true, resolve(lua, ca, &v, parsed.explicit)));
        }
    }
    Some((false, Target::None))
}

/// `DetectYield`: SuperCleveRoidMacros owns macro display unless disabled or announcing that it
/// drives ours.
fn detect_yield(lua: &Lua) -> bool {
    match lua.globals().raw_get::<Value>("CleveRoids") {
        Ok(Value::Table(t)) => {
            let flag = |k: &str| t.raw_get::<Value>(k).is_ok_and(|v| truthy(&v));
            !flag("disabled") && !flag("ClassicAPIMacroDisplay")
        }
        _ => false,
    }
}

fn world(lua: &Lua, ca: &Ca) -> World {
    let unit = |t: &str| {
        crate::lua::unit::unit_guid(lua, t)
            .ok()
            .flatten()
            .unwrap_or(0)
    };
    let (mouseover, target) = (unit("mouseover"), unit("target"));
    let st = ca.lock();
    World {
        modifiers: st.modifiers,
        mouseover,
        target,
        combat: st
            .mirror
            .me()
            .is_some_and(|f| f.u32(field::UNIT_FLAGS) & IN_COMBAT != 0),
    }
}

/// The frame's work: re-parse on a macro edit, re-evaluate on the cadence, and write the
/// display when it moved.
pub(crate) fn tick(lua: &Lua, ca: &Ca, display: &mut ExtMacroDisplay, now: Instant) {
    // In the world, with the evaluator loaded: before then nothing is shown.
    if ca.lock().mirror.player == 0
        || lua
            .globals()
            .get::<Function>("SecureCmdOptionParse")
            .is_err()
    {
        return;
    }
    let mut me = std::mem::take(&mut ca.lock().showtooltip);
    if me
        .yield_checked
        .is_none_or(|at| now.duration_since(at) >= YIELD_CHECK_EVERY)
    {
        me.yielding = detect_yield(lua);
        me.yield_checked = Some(now);
    }
    let slash = me.slash.get_or_insert_with(|| slash_names(lua)).clone();
    let generation = benilla_ui::script::ext_read::macros_generation(lua);
    if me.generation != Some(generation) {
        me.generation = Some(generation);
        for index in 1..=MACROS {
            let Some((_, _, body)) = benilla_ui::script::ext_read::macro_view(lua, index) else {
                me.entries.remove(&index);
                continue;
            };
            let e = me.entries.entry(index).or_default();
            if e.external || e.body == body {
                continue;
            }
            *e = Entry {
                parsed: parse(&body, &slash),
                body,
                ..Entry::default()
            };
        }
    }
    let state = world(lua, ca);
    let moved = state != me.world;
    me.world = state;
    if !me.yielding {
        for e in me.entries.values_mut() {
            if e.external || !e.managed() {
                continue;
            }
            let every = if e.parsed.conditional {
                CONDITIONAL_EVERY
            } else {
                STATIC_EVERY
            };
            let due = e.last_eval.is_none_or(|at| now.duration_since(at) >= every)
                || (e.parsed.conditional && moved);
            if !due {
                continue;
            }
            e.last_eval = Some(now);
            match evaluate(lua, ca, &e.parsed) {
                Some((matched, target)) => {
                    e.matched = matched;
                    e.target = target;
                }
                None => e.foreign = true,
            }
        }
    }
    let fresh: HashMap<u32, MacroShow> = me
        .entries
        .iter()
        .filter(|(_, e)| e.managed() && (e.external || !me.yielding))
        .map(|(i, e)| (*i, e.show()))
        .collect();
    if display.0 != fresh {
        display.0 = fresh;
    }
    ca.lock().showtooltip = me;
}

/// What the macro shows, as `Lookup` answers it: `None` when it is not ours to describe.
fn lookup(ca: &Ca, index: u32) -> Option<(Kind, Target)> {
    lookup_full(ca, index).map(|(k, t, _)| (k, t))
}

/// [`lookup`] with whether the directive is conditional (a published one never is).
fn lookup_full(ca: &Ca, index: u32) -> Option<(Kind, Target, bool)> {
    let st = ca.lock();
    let me = &st.showtooltip;
    let e = me.entries.get(&index)?;
    if !e.managed() || (me.yielding && !e.external) || e.target == Target::None {
        return None;
    }
    Some((e.parsed.kind, e.target, e.parsed.conditional && !e.external))
}

/// The macro kind's bits in an action slot's kind byte.
const ACTION_KIND_MACRO: u8 = 0x40;

/// `Script_SetAction`'s macro arm: the resolved spell's or item's tooltip, or `None` for the
/// stock method to answer.
fn set_action(
    lua: &Lua,
    ca: &Ca,
    tip: &mlua::Table,
    slot: &Value,
) -> mlua::Result<Option<MultiValue>> {
    use mlua::ObjectLike;
    if !crate::lua::is_number(slot) {
        return Ok(None);
    }
    let Ok(slot) = u32::try_from(to_int(slot)) else {
        return Ok(None);
    };
    let index = match benilla_ui::script::ext_read::action_slot(lua, slot) {
        Some((kind, action)) if kind & 0xf0 == ACTION_KIND_MACRO => action,
        _ => return Ok(None),
    };
    let Some((Kind::ShowTooltip, target, conditional)) = lookup_full(ca, index) else {
        return Ok(None);
    };
    let refresh = |ret: MultiValue| -> mlua::Result<Option<MultiValue>> {
        if conditional {
            Ok(Some(1.into_lua_multi(lua)?))
        } else {
            Ok(Some(ret))
        }
    };
    match target {
        Target::Spell(id) => {
            let Some((book_slot, pet)) = super::spell::find_book_slot(lua, i64::from(id)) else {
                return Ok(None);
            };
            let book = if pet { "pet" } else { "spell" };
            refresh(tip.call_method("SetSpell", (book_slot, book))?)
        }
        Target::Item(id) => {
            let arg = crate::items::Arg {
                item_id: i64::from(id),
                ..Default::default()
            };
            let found = crate::items::Carried::of(&ca.lock().mirror).find(lua, &arg);
            let ret: MultiValue = match found {
                Some(f) if f.equipment != 0 => {
                    tip.call_method("SetInventoryItem", ("player", f.equipment))?
                }
                Some(f) => tip.call_method("SetBagItem", (f.bag, f.slot))?,
                None => tip.call_method("SetHyperlink", format!("item:{id}:0:0:0"))?,
            };
            refresh(ret)
        }
        Target::None => Ok(None),
    }
}

fn spell_name_rank(ca: &Ca, id: u32) -> Option<(String, String)> {
    let t = crate::spells::table(&ca.db)?;
    let r = t.row(id)?;
    Some((
        r.loc(crate::spells::col::NAME).to_string(),
        r.loc(crate::spells::col::RANK).to_string(),
    ))
}

/// The icon the macro shows: a resolved spell's or item's when its own is the question mark.
pub(crate) fn macro_icon(lua: &Lua, ca: &Ca, index: u32) -> Option<String> {
    let (_, own, _) = benilla_ui::script::ext_read::macro_view(lua, index)?;
    let own = own?;
    if !own.eq_ignore_ascii_case(QUESTION_MARK) {
        return Some(own);
    }
    let resolved = match lookup(ca, index).map(|l| l.1) {
        Some(Target::Spell(id)) => crate::spells::table(&ca.db).and_then(|t| {
            t.row(id)
                .and_then(|r| crate::spells::spell_icon(&ca.db, &r, false))
        }),
        Some(Target::Item(id)) => super::item::record(lua, i64::from(id))
            .and_then(|r| super::item::data::icon_for_display(&ca.db, r.display_info_id)),
        _ => None,
    };
    Some(resolved.unwrap_or(own))
}

/// `Publish`: an addon names what the macro shows, in a `#showtooltip` line's forms; `None`
/// releases it to our own parse. Whether a value resolved.
fn publish(lua: &Lua, ca: &Ca, index: u32, value: Option<&str>) -> bool {
    if !(1..=MACROS).contains(&index) {
        return false;
    }
    let Some((_, _, body)) = benilla_ui::script::ext_read::macro_view(lua, index) else {
        return false;
    };
    let Some(value) = value else {
        let mut st = ca.lock();
        if st
            .showtooltip
            .entries
            .get(&index)
            .is_some_and(|e| e.external)
        {
            st.showtooltip.entries.remove(&index);
            st.showtooltip.generation = None;
        }
        return false;
    };
    let target = if value.is_empty() {
        Target::None
    } else {
        resolve(lua, ca, value, true)
    };
    let slash = ca.lock().showtooltip.slash.clone().unwrap_or_default();
    let kind = match parse(&body, &slash).kind {
        Kind::ShowTooltip => Kind::ShowTooltip,
        _ => Kind::Show,
    };
    ca.lock().showtooltip.entries.insert(
        index,
        Entry {
            body,
            parsed: Parsed {
                kind,
                ..Parsed::default()
            },
            external: true,
            matched: !value.is_empty(),
            target,
            ..Entry::default()
        },
    );
    target != Target::None
}

fn slot_arg(v: &Value, usage: &str) -> mlua::Result<u32> {
    if !crate::lua::is_number(v) {
        return Err(mlua::Error::runtime(usage.to_string()));
    }
    Ok(u32::try_from(to_int(v)).unwrap_or(0))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    let stock: std::rc::Rc<std::cell::RefCell<Option<Function>>> = Default::default();
    let stock2 = stock.clone();
    let wrapper = api.lua.create_function(move |lua, args: MultiValue| {
        let a: Vec<Value> = args.clone().into_iter().collect();
        if let (Some(Value::Table(tip)), Some(slot)) = (a.first(), a.get(1)) {
            if let Some(ret) = set_action(lua, &c, tip, slot)? {
                return Ok(ret);
            }
        }
        match stock2.borrow().as_ref() {
            Some(f) => f.call::<MultiValue>(args),
            None => Ok(MultiValue::new()),
        }
    })?;
    // A VM without the tooltip methods (a bare test VM) has nothing to wrap.
    if let Ok(old) =
        benilla_ui::script::ext_read::replace_tooltip_method(api.lua, "SetAction", wrapper)
    {
        *stock.borrow_mut() = old;
    }

    // `(name, rank, spellID)`: our resolution when we describe the macro, else the spell the app
    // derived from its body.
    let c = api.ca.clone();
    api.global("GetMacroSpell", move |lua, v: Value| {
        let index = slot_arg(&v, "Usage: GetMacroSpell(macroSlot)")?;
        let ours = {
            let st = c.lock();
            st.showtooltip
                .entries
                .get(&index)
                .filter(|e| e.managed() && (e.external || !st.showtooltip.yielding))
                .map(|e| e.target)
        };
        let id = match ours {
            Some(Target::Spell(id)) => id,
            Some(_) => return Ok(MultiValue::new()),
            None => match benilla_ui::script::ext_read::macro_binding(lua, index) {
                Some(b) if b.spell > 0 => b.spell as u32,
                _ => return Ok(MultiValue::new()),
            },
        };
        match spell_name_rank(&c, id) {
            Some((name, rank)) => (name, rank, id).into_lua_multi(lua),
            None => Ok(MultiValue::new()),
        }
    })?;

    // `(name, link)` of the item the directive resolved to: the carried instance's when held.
    let c = api.ca.clone();
    api.global("GetMacroItem", move |lua, v: Value| {
        let index = slot_arg(&v, "Usage: GetMacroItem(macroSlot)")?;
        let Some((_, Target::Item(id))) = lookup(&c, index) else {
            return Ok(MultiValue::new());
        };
        let arg = crate::items::Arg {
            item_id: i64::from(id),
            ..Default::default()
        };
        let carried = crate::items::Carried::of(&c.lock().mirror);
        let (name, link) = match carried.find(lua, &arg) {
            Some(f) => (
                crate::items::display_name(lua, f.item.entry, f.item.random_property()),
                crate::items::item_link(lua, &f.item),
            ),
            None => (
                crate::items::base_name(lua, id),
                crate::items::link(lua, id, 0),
            ),
        };
        match (name, link) {
            (Some(n), Some(l)) => (n, l).into_lua_multi(lua),
            _ => Ok(MultiValue::new()),
        }
    })?;

    let c = api.ca.clone();
    api.table("C_Macro", "GetMacroIcon", move |lua, v: Value| {
        let index = slot_arg(&v, "Usage: C_Macro.GetMacroIcon(macroSlot)")?;
        Ok(macro_icon(lua, &c, index))
    })?;

    // `(macroSlot, value)`: a value resolves and shows it, false or "" claims the macro with
    // nothing to show, nil releases it.
    let c = api.ca.clone();
    api.table(
        "C_Macro",
        "SetMacroDisplay",
        move |lua, args: MultiValue| {
            let a: Vec<Value> = args.into_iter().collect();
            let index = slot_arg(
                a.first().unwrap_or(&Value::Nil),
                "Usage: C_Macro.SetMacroDisplay(macroSlot, value)",
            )?;
            let value = match a.get(1) {
                None | Some(Value::Nil) => None,
                Some(v @ Value::String(_)) => Some(to_str(v).unwrap_or_default()),
                Some(_) => Some(String::new()),
            };
            Ok(publish(lua, &c, index, value.as_deref()))
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slash() -> Vec<(String, bool)> {
        vec![
            ("/cast".into(), false),
            ("/use".into(), false),
            ("/castsequence".into(), true),
        ]
    }

    #[test]
    fn the_directive_and_its_option_lines_parse() {
        let p = parse("#showtooltip Frostbolt\n/cast Fireball", &slash());
        assert_eq!(p.kind, Kind::ShowTooltip);
        assert!(p.explicit);
        assert_eq!(p.options, [("Frostbolt".to_string(), false)]);

        let p = parse(
            "#show\n/say hi\n/cast [mod:shift] Blink\n/castsequence Fireball, Frostbolt\n/cast Ignored",
            &slash(),
        );
        assert_eq!(p.kind, Kind::Show);
        assert!(p.conditional && !p.explicit);
        assert_eq!(
            p.options,
            [
                ("[mod:shift] Blink".to_string(), false),
                ("Fireball, Frostbolt".to_string(), true)
            ]
        );

        assert_eq!(parse("/cast Fireball", &slash()).kind, Kind::None);
        assert_eq!(parse("#showtooltipx", &slash()).kind, Kind::None);
        assert_eq!(parse("  #SHOWTOOLTIP", &slash()).kind, Kind::ShowTooltip);
    }

    #[test]
    fn set_action_falls_through_for_a_plain_slot() {
        let ca = Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        let out: String = script
            .lua()
            .load(
                r#"
                local t = CreateFrame("GameTooltip", "CAPITestTip", UIParent)
                local ok = pcall(t.SetAction, t, 1)
                return tostring(ok) .. " " .. type(t.SetAction)
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "true function");
    }

    #[test]
    fn a_published_macro_shows_and_releases() {
        let ca = Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        let lua = script.lua();
        lua.load(r##"CreateMacro("m", 1, "#showtooltip\n/cast Fireball", nil, nil)"##)
            .exec()
            .ok();
        assert!(
            benilla_ui::script::ext_read::macro_view(lua, 1).is_some(),
            "CreateMacro made macro 1"
        );
        assert!(!publish(lua, &ca, 1, Some("")));
        assert_eq!(
            ca.lock().showtooltip.entries.get(&1).map(Entry::show),
            Some(MacroShow::Nothing)
        );
        publish(lua, &ca, 1, None);
        assert!(!ca.lock().showtooltip.entries.contains_key(&1));
    }
}

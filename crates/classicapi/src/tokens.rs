//! `unit/TokenExtensions.cpp`, `unit/Focus.cpp`, `unit/RaidTarget.cpp` and the slot table of
//! `nameplate/Events.cpp`: the `focus`, `nameplateN` and `markN` unit tokens, and `0x<hex>` guid
//! literals when SuperWoW is not loaded (whose own resolver owns that form). Each composes with
//! `target` hops, the engine's suffix walker. The table lives behind its own lock, apart from the
//! crate state, because benilla's resolver asks it from inside natives that may hold that one.

use std::sync::{Arc, Mutex, MutexGuard};

use benilla_ui::script::{UnitGuids, UnitTokenExtension};

/// What the token grammar reads.
#[derive(Default)]
pub struct TokenTable {
    /// `Unit::Focus`'s guid, 0 for none.
    pub focus: u64,
    /// Nameplate slots: slot `i` is `nameplate(i+1)`'s guid, 0 while free. A plate keeps its slot
    /// for life and a new plate takes the lowest free one, so survivors never renumber.
    pub plates: Vec<u64>,
    /// The raid-target table, `mark1` (star) to `mark8` (skull).
    pub marks: [u64; 8],
    /// Whether `0x<hex>` literals are ours: SuperWoW resolves them when it is loaded.
    pub guid_literals: bool,
}

impl TokenTable {
    /// `AssignSlot`: the lowest free slot, growing only when every slot is taken; 0-based.
    pub fn assign_plate(&mut self, guid: u64) -> usize {
        if let Some(i) = self.plates.iter().position(|g| *g == 0) {
            self.plates[i] = guid;
            return i;
        }
        self.plates.push(guid);
        self.plates.len() - 1
    }

    /// Free `guid`'s slot without shifting the others, trimming trailing free slots; the 0-based
    /// slot it held.
    pub fn free_plate(&mut self, guid: u64) -> Option<usize> {
        let i = self.plates.iter().position(|g| *g == guid)?;
        self.plates[i] = 0;
        while self.plates.last() == Some(&0) {
            self.plates.pop();
        }
        Some(i)
    }

    /// The 1-based `nameplateN` index `guid` holds.
    pub fn plate_index(&self, guid: u64) -> Option<usize> {
        self.plates
            .iter()
            .position(|g| *g == guid && guid != 0)
            .map(|i| i + 1)
    }

    /// The `(token, guid)` pairs this table names now, for the unit events: `focus`, each live
    /// `nameplateN` and each set `markN`.
    pub fn named(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        if self.focus != 0 {
            out.push(("focus".to_string(), self.focus));
        }
        for (i, g) in self.plates.iter().enumerate() {
            if *g != 0 {
                out.push((format!("nameplate{}", i + 1), *g));
            }
        }
        for (i, g) in self.marks.iter().enumerate() {
            if *g != 0 {
                out.push((format!("mark{}", i + 1), *g));
            }
        }
        out
    }
}

/// The shared table.
#[derive(Clone, Default)]
pub struct Tokens(Arc<Mutex<TokenTable>>);

impl Tokens {
    pub fn lock(&self) -> MutexGuard<'_, TokenTable> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The resolver extension benilla asks where its own grammar names nobody or raises.
    pub fn extension(&self) -> UnitTokenExtension {
        let table = self.clone();
        Box::new(move |stock, token| {
            let t = table.lock();
            resolve(&t, stock, token)
        })
    }
}

/// `WalkSuffix`: each `target` (any case) hops to the unit's `UNIT_FIELD_TARGET`; anything else
/// names nobody, as does a hop off a unit the client does not hold.
fn walk_suffix(stock: &UnitGuids, mut guid: u64, mut rest: &str) -> u64 {
    while guid != 0 && !rest.is_empty() {
        let Some(tail) = strip_prefix_ci(rest, "target") else {
            return 0;
        };
        rest = tail;
        guid = match stock.held.get(&guid) {
            Some(t) => *t,
            None => return 0,
        };
    }
    guid
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// `Hook_h`'s families, in its order: a guid literal, `focus`, `nameplateN`, `markN`. `None`
/// leaves the token to the next grammar.
pub fn resolve(t: &TokenTable, stock: &UnitGuids, token: &str) -> Option<Option<u64>> {
    if token.is_empty() {
        return None;
    }
    let name = |g: u64| Some((g != 0).then_some(g));
    if t.guid_literals {
        if let Some(hex) = strip_prefix_ci(token, "0x") {
            let digits = hex.bytes().take_while(u8::is_ascii_hexdigit).count();
            if digits > 0 {
                // Up to the first non-hex digit; more than sixteen digits keep the low 64 bits.
                let guid = hex[..digits].bytes().fold(0u64, |g, b| {
                    (g << 4) | u64::from((b as char).to_digit(16).unwrap_or(0))
                });
                return name(walk_suffix(stock, guid, &hex[digits..]));
            }
        }
    }
    if let Some(rest) = strip_prefix_ci(token, "focus") {
        if t.focus == 0 {
            return Some(None);
        }
        return name(walk_suffix(stock, t.focus, rest));
    }
    if let Some(rest) = strip_prefix_ci(token, "nameplate") {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            let index: usize = rest[..digits].parse().unwrap_or(0);
            let guid = index
                .checked_sub(1)
                .and_then(|i| t.plates.get(i))
                .copied()
                .unwrap_or(0);
            if guid == 0 {
                return Some(None);
            }
            return name(walk_suffix(stock, guid, &rest[digits..]));
        }
    }
    if let Some(rest) = strip_prefix_ci(token, "mark") {
        if let Some(d) = rest.bytes().next().filter(|b| (b'1'..=b'8').contains(b)) {
            let guid = t.marks[usize::from(d - b'1')];
            if guid == 0 {
                return Some(None);
            }
            return name(walk_suffix(stock, guid, &rest[1..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock() -> UnitGuids {
        let mut s = UnitGuids::default();
        s.held.insert(0x10, 0x20);
        s.held.insert(0x20, 0);
        s
    }

    #[test]
    fn the_families_resolve_and_walk_targets() {
        let mut t = TokenTable {
            focus: 0x10,
            guid_literals: true,
            ..Default::default()
        };
        t.marks[7] = 0x20;
        assert_eq!(t.assign_plate(0x10), 0);
        let s = stock();
        assert_eq!(resolve(&t, &s, "focus"), Some(Some(0x10)));
        assert_eq!(resolve(&t, &s, "FocusTarget"), Some(Some(0x20)));
        assert_eq!(resolve(&t, &s, "focustargettarget"), Some(None));
        assert_eq!(resolve(&t, &s, "focusfoo"), Some(None));
        assert_eq!(resolve(&t, &s, "nameplate1"), Some(Some(0x10)));
        assert_eq!(resolve(&t, &s, "nameplate2"), Some(None));
        assert_eq!(resolve(&t, &s, "nameplate"), None);
        assert_eq!(resolve(&t, &s, "mark8"), Some(Some(0x20)));
        assert_eq!(resolve(&t, &s, "mark9"), None);
        assert_eq!(resolve(&t, &s, "0x10target"), Some(Some(0x20)));
        assert_eq!(resolve(&t, &s, "target"), None);
        t.guid_literals = false;
        assert_eq!(resolve(&t, &s, "0x10"), None);
    }

    #[test]
    fn plate_slots_never_renumber_survivors() {
        let mut t = TokenTable::default();
        t.assign_plate(1);
        t.assign_plate(2);
        t.assign_plate(3);
        assert_eq!(t.free_plate(2), Some(1));
        assert_eq!(t.plate_index(3), Some(3));
        assert_eq!(t.assign_plate(4), 1, "the freed middle slot is reused");
        t.free_plate(3);
        t.free_plate(4);
        assert_eq!(t.plates, vec![1], "trailing free slots trim");
    }
}

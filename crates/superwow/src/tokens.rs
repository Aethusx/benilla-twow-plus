//! SuperWoW's unit-token grammar, asked by benilla's resolver only where the stock grammar names
//! nobody or raises: a base, then any number of `target`, `pet` and `owner` hops. A base is a
//! stock token (`player`, `party1`, `raid3target`), a guid string (`0xF13000...`, what
//! `UnitExists` and `GetName(1)` hand out) or `mark1`-`mark8`, the unit wearing that raid mark
//! (1 star, 8 skull). Every compare folds ASCII case, as the stock resolver does.

use benilla_ui::script::{parse_unit_token, UnitGuids, UnitTokenParse};

/// The hops a token can take off a unit, as the unit's descriptor names them.
pub trait Hops {
    /// `UNIT_FIELD_TARGET`.
    fn target(&self, guid: u64) -> u64;
    /// `UNIT_FIELD_CHARM`, else `UNIT_FIELD_SUMMON`, as the stock `pet` token reads ours.
    fn pet(&self, guid: u64) -> u64;
    /// `UNIT_FIELD_SUMMONEDBY`, else `CHARMEDBY`, else `CREATEDBY`: a pet's or a totem's master.
    fn owner(&self, guid: u64) -> u64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hop {
    Target,
    Pet,
    Owner,
}

/// The hops `rest` spells, or `None` for text that is not one.
fn hops(mut rest: &str) -> Option<Vec<Hop>> {
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (hop, len) = [
            ("target", Hop::Target),
            ("pet", Hop::Pet),
            ("owner", Hop::Owner),
        ]
        .into_iter()
        .find(|(word, _)| rest.starts_with(word))
        .map(|(word, hop)| (hop, word.len()))?;
        out.push(hop);
        rest = &rest[len..];
    }
    Some(out)
}

/// The base unit `lower` starts with, and the hops after it: `None` for a token outside the
/// grammar. A base that names nobody is `Some((0, ..))`.
fn base(stock: &UnitGuids, marks: &[u64; 8], lower: &str) -> Option<(u64, Vec<Hop>)> {
    if let Some(hex) = lower.strip_prefix("0x") {
        let digits = hex
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .count()
            .min(16);
        if digits == 0 {
            return None;
        }
        let guid = u64::from_str_radix(&hex[..digits], 16).ok()?;
        return Some((guid, hops(&hex[digits..])?));
    }
    if let Some(n) = lower.strip_prefix("mark") {
        let digit = *n.as_bytes().first()?;
        if !(b'1'..=b'8').contains(&digit) {
            return None;
        }
        let guid = marks[usize::from(digit - b'1')];
        return Some((guid, hops(&n[1..])?));
    }
    // The longest stock token the text starts with whose tail is all hops: `targetowner` is
    // `target` then `owner`, `party1pettarget` is `party1` then `pet` and `target`.
    (1..=lower.len())
        .rev()
        .filter(|&end| lower.is_char_boundary(end))
        .find_map(|end| {
            let head = &lower[..end];
            matches!(parse_unit_token(head), UnitTokenParse::Unit { .. })
                .then(|| hops(&lower[end..]))
                .flatten()
                .map(|h| (stock.guid(head).unwrap_or(0), h))
        })
}

/// What the grammar makes of `token`: `None` outside it, else the unit it names (`None` for
/// nobody).
pub fn resolve(
    stock: &UnitGuids,
    marks: &[u64; 8],
    units: &impl Hops,
    token: &str,
) -> Option<Option<u64>> {
    let lower = token.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let (mut guid, path) = base(stock, marks, &lower)?;
    for hop in path {
        if guid == 0 {
            break;
        }
        guid = match hop {
            Hop::Target => units.target(guid),
            Hop::Pet => units.pet(guid),
            Hop::Owner => units.owner(guid),
        };
    }
    Some((guid != 0).then_some(guid))
}

/// A guid as SuperWoW writes one: `0x` and sixteen upper-case hex digits.
pub fn guid_string(guid: u64) -> String {
    format!("0x{guid:016X}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const ME: u64 = 0x10;
    const MOB: u64 = 0xF130_0000_4500_0001;
    const TOTEM: u64 = 0xF130_0000_1700_0002;
    const SHAMAN: u64 = 0x21;
    const WOLF: u64 = 0xF140_0000_0000_0021;

    #[derive(Default)]
    struct Units(HashMap<u64, (u64, u64, u64)>);

    impl Hops for Units {
        fn target(&self, g: u64) -> u64 {
            self.0.get(&g).map_or(0, |u| u.0)
        }
        fn pet(&self, g: u64) -> u64 {
            self.0.get(&g).map_or(0, |u| u.1)
        }
        fn owner(&self, g: u64) -> u64 {
            self.0.get(&g).map_or(0, |u| u.2)
        }
    }

    fn world() -> (UnitGuids, [u64; 8], Units) {
        let stock = UnitGuids {
            player: ME,
            target: TOTEM,
            party: [SHAMAN, 0, 0, 0],
            ..Default::default()
        };
        let mut marks = [0; 8];
        marks[7] = MOB;
        let mut units = Units::default();
        units.0.insert(TOTEM, (0, 0, SHAMAN));
        units.0.insert(SHAMAN, (MOB, WOLF, 0));
        units.0.insert(WOLF, (MOB, 0, SHAMAN));
        units.0.insert(MOB, (SHAMAN, 0, 0));
        (stock, marks, units)
    }

    fn at(token: &str) -> Option<Option<u64>> {
        let (stock, marks, units) = world();
        resolve(&stock, &marks, &units, token)
    }

    #[test]
    fn owner_hops_off_a_stock_token() {
        assert_eq!(at("targetowner"), Some(Some(SHAMAN)));
        assert_eq!(at("TargetOwnerTarget"), Some(Some(MOB)));
        assert_eq!(at("party1pet"), Some(Some(WOLF)));
        assert_eq!(at("party1petowner"), Some(Some(SHAMAN)));
        assert_eq!(
            at("playerowner"),
            Some(None),
            "a unit with no master is nobody"
        );
    }

    #[test]
    fn guid_strings_name_their_unit_and_take_hops() {
        assert_eq!(at(&guid_string(MOB)), Some(Some(MOB)));
        assert_eq!(at("0xf130000045000001"), Some(Some(MOB)));
        assert_eq!(
            at(&format!("{}target", guid_string(MOB))),
            Some(Some(SHAMAN))
        );
        assert_eq!(at("0x0000000000000000"), Some(None));
        assert_eq!(at("0x"), None);
    }

    #[test]
    fn marks_name_the_marked_unit() {
        assert_eq!(at("mark8"), Some(Some(MOB)));
        assert_eq!(at("MARK8target"), Some(Some(SHAMAN)));
        assert_eq!(at("mark1"), Some(None));
        assert_eq!(at("mark9"), None);
    }

    #[test]
    fn other_text_is_left_to_the_stock_grammar() {
        assert_eq!(at("focus"), None);
        assert_eq!(at("targetfoo"), None);
        assert_eq!(at(""), None);
    }
}

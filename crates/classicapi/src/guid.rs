//! `guid/Guid.cpp`: 1.12 guids as the DLL reads them. The top 16 bits are the type prefix; a
//! creature guid packs its template entry in bits 24-47.

/// The type a guid's prefix names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Unknown,
    Player,
    Item,
    Creature,
    Pet,
    GameObject,
    DynamicObject,
    Corpse,
}

/// `Guid::Classify`: 0 and any unrecognized prefix are `Unknown`.
pub fn classify(guid: u64) -> Kind {
    if guid == 0 {
        return Kind::Unknown;
    }
    match (guid >> 48) as u16 {
        0x0000 => Kind::Player,
        0x4000 => Kind::Item,
        0xF130 => Kind::Creature,
        0xF140 => Kind::Pet,
        0xF110 => Kind::GameObject,
        0xF100 => Kind::DynamicObject,
        0xF101 => Kind::Corpse,
        _ => Kind::Unknown,
    }
}

/// `Guid::CreatureEntry`: a creature guid's template entry, 0 for any other kind (a pet's bits
/// are its pet number).
pub fn creature_entry(guid: u64) -> u32 {
    if classify(guid) != Kind::Creature {
        return 0;
    }
    ((guid >> 24) & 0xFF_FFFF) as u32
}

/// `Guid::Parse`: `0x` and exactly 8 or 16 hex digits; the 8-digit form's high dword is 0.
pub fn parse(s: &str) -> Option<u64> {
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    if !(hex.len() == 8 || hex.len() == 16) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

/// `Guid::FormatAsString`: `0x%08X%08X`, high dword first.
pub fn format(guid: u64) -> String {
    format!("0x{:08X}{:08X}", (guid >> 32) as u32, guid as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_parse_classify_and_format() {
        assert_eq!(parse("0x00000123"), Some(0x123));
        assert_eq!(parse("0xF130000ABC000001"), Some(0xF130_000A_BC00_0001));
        assert_eq!(parse("0x123"), None);
        assert_eq!(classify(0xF130_000A_BC00_0001), Kind::Creature);
        assert_eq!(creature_entry(0xF130_000A_BC00_0001), 0xABC);
        assert_eq!(creature_entry(0xF140_000A_BC00_0001), 0);
        assert_eq!(format(0x123), "0x0000000000000123");
    }
}

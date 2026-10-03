//! The `Spell.dbc` reads behind `SpellInfo`, the spell names `CastSpellByName` looks up and the
//! channel durations `UNIT_CASTEVENT` reports, loaded once off the player's own patch chain.

use benilla_formats::{SpellCatalog, SpellDisplay, SpellDurationCatalog, SpellRangeCatalog};

/// `SPELL_ATTR_EX_CHANNELED_1 | SPELL_ATTR_EX_CHANNELED_2`.
const ATTR_EX_CHANNELED: u32 = 0x4 | 0x40;

/// The tables; each empty when the install lacks it.
#[derive(Default)]
pub struct Spells {
    catalog: Option<SpellCatalog>,
    ranges: Option<SpellRangeCatalog>,
    durations: Option<SpellDurationCatalog>,
}

impl Spells {
    /// Read the tables from the install; a missing install loads nothing.
    pub fn load() -> Self {
        let Some(data) = benilla_formats::wow_data() else {
            return Self::default();
        };
        let Ok(mut chain) = benilla_formats::open_chain(&data) else {
            return Self::default();
        };
        Self {
            catalog: benilla_formats::load_spell_catalog(&mut chain).ok(),
            ranges: benilla_formats::load_spell_ranges(&mut chain).ok(),
            durations: benilla_formats::load_spell_durations(&mut chain).ok(),
        }
    }

    pub fn get(&self, id: u32) -> Option<&SpellDisplay> {
        self.catalog.as_ref()?.get(id)
    }

    /// `SpellInfo(id)`: name, rank, icon texture, and the range's min and max yards.
    pub fn info(&self, id: u32) -> Option<(String, String, String, f32, f32)> {
        let s = self.get(id)?;
        let (min, max) = self
            .ranges
            .as_ref()
            .and_then(|r| r.get(s.range_index))
            .map_or((0.0, 0.0), |r| (r.min, r.max));
        Some((
            s.name.clone(),
            s.rank.clone().unwrap_or_default(),
            s.icon.clone().unwrap_or_default(),
            min,
            max,
        ))
    }

    /// A channeled spell's base duration in ms; `None` for a spell that does not channel.
    pub fn channel_ms(&self, id: u32) -> Option<u32> {
        let s = self.get(id)?;
        if s.attributes_ex & ATTR_EX_CHANNELED == 0 {
            return None;
        }
        let base = self.durations.as_ref()?.get(s.duration_index)?.base_ms;
        Some(u32::try_from(base).unwrap_or(0))
    }

    /// The book spell `name` names: `"Name(Rank N)"` that rank, a bare name the highest rank.
    pub fn in_book(&self, book: &[u32], name: &str) -> Option<u32> {
        let lower = name.trim().to_lowercase();
        let (want, rank) = match lower.find('(') {
            Some(open) if lower.ends_with(')') => (
                lower[..open].trim_end().to_string(),
                Some(lower[open + 1..lower.len() - 1].to_string()),
            ),
            _ => (lower, None),
        };
        let mut best: Option<(u32, u32)> = None;
        for &id in book {
            let Some(s) = self.get(id) else {
                continue;
            };
            if s.name.to_lowercase() != want {
                continue;
            }
            let r = s.rank.as_deref().unwrap_or("").to_lowercase();
            if let Some(rank) = &rank {
                if &r == rank {
                    return Some(id);
                }
                continue;
            }
            let n: u32 = r.trim_start_matches("rank").trim().parse().unwrap_or(0);
            if best.is_none_or(|(_, b)| n >= b) {
                best = Some((id, n));
            }
        }
        best.map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn spell(id: u32, name: &str, rank: &str) -> SpellDisplay {
        SpellDisplay {
            id,
            name: name.into(),
            rank: Some(rank.into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_bare_name_takes_the_highest_rank() {
        let spells = Spells {
            catalog: Some(SpellCatalog::from_displays(HashMap::from([
                (133, spell(133, "Fireball", "Rank 1")),
                (143, spell(143, "Fireball", "Rank 2")),
                (145, spell(145, "Fireball", "Rank 3")),
            ]))),
            ..Default::default()
        };
        let book = [133, 143];
        assert_eq!(spells.in_book(&book, "fireball"), Some(143));
        assert_eq!(spells.in_book(&book, "Fireball(Rank 1)"), Some(133));
        assert_eq!(spells.in_book(&book, "Fireball(Rank 3)"), None);
        assert_eq!(spells.in_book(&book, "Frostbolt"), None);
    }
}

//! Tag colours. A tag's colour is its own, kept in the config. kuulo offers twelve swatches,
//! designed to sit side by side on the window's dark theme; a new tag gets one nobody uses
//! yet, at random, and once all are taken one of the least used. Any other colour can be set
//! by hand.

use std::hash::{BuildHasher, Hasher};

use crate::config::TagDef;

/// The swatches, round the colour wheel, as a picker shows them.
pub const SWATCHES: [(&str, &str); 12] = [
    ("red", "#e05561"),
    ("coral", "#e8826b"),
    ("amber", "#e2ab4e"),
    ("yellow", "#d6c95c"),
    ("green", "#8fbf5e"),
    ("teal", "#4fb3a4"),
    ("sky", "#62a7ea"),
    ("indigo", "#8189ec"),
    ("violet", "#b07fe0"),
    ("pink", "#e07fc0"),
    ("brown", "#b08a6e"),
    ("grey", "#9aa1aa"),
];

/// The colour of the swatch called `name`.
pub fn swatch(name: &str) -> Option<&'static str> {
    SWATCHES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, hex)| *hex)
}

/// Gives every tag without a colour, or with one that does not parse, a swatch of its own.
/// Whether any changed.
pub fn fill(tags: &mut [TagDef]) -> bool {
    let missing: Vec<usize> = (0..tags.len()).filter(|&i| tags[i].color.as_deref().and_then(parse).is_none()).collect();
    for &i in &missing {
        tags[i].color = Some(fresh(tags));
    }
    !missing.is_empty()
}

/// A swatch for a tag added after `tags`: one of the least used, at random.
pub fn fresh(tags: &[TagDef]) -> String {
    let uses = |hex: &str| tags.iter().filter(|t| t.color.as_deref().and_then(parse) == parse(hex)).count();
    let least = SWATCHES.iter().map(|(_, hex)| uses(hex)).min().unwrap_or(0);
    let open: Vec<&str> = SWATCHES.iter().map(|(_, hex)| *hex).filter(|hex| uses(hex) == least).collect();
    let roll = std::collections::hash_map::RandomState::new().build_hasher().finish();
    open[roll as usize % open.len()].to_string()
}

/// The bytes of `#rrggbb`, or of `rrggbb`, around any whitespace.
pub fn parse(color: &str) -> Option<[u8; 3]> {
    let s = color.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(color: Option<&str>) -> TagDef {
        TagDef { name: String::new(), color: color.map(str::to_string) }
    }

    /// A new tag takes a swatch nobody uses while there is one, then one of the least used;
    /// filling leaves the colours there are alone.
    #[test]
    fn a_new_tag_takes_an_unused_swatch() {
        let mut tags: Vec<TagDef> = vec![tag(Some("#123456"))];
        for _ in 0..SWATCHES.len() {
            let color = fresh(&tags);
            assert!(tags.iter().all(|t| t.color.as_deref() != Some(color.as_str())), "{color} is taken");
            tags.push(tag(Some(&color)));
        }
        let twice = fresh(&tags);
        tags.push(tag(Some(&twice)));
        assert_ne!(fresh(&tags), twice, "a swatch used twice while others are used once");
        let mut mixed = vec![tag(Some("#e05561")), tag(None), tag(Some("not a colour"))];
        assert!(fill(&mut mixed));
        assert_eq!(mixed[0].color.as_deref(), Some("#e05561"));
        assert!(mixed.iter().all(|t| SWATCHES.iter().any(|(_, hex)| t.color.as_deref() == Some(*hex))));
        assert!(!fill(&mut mixed), "colours already given changed");
    }
}

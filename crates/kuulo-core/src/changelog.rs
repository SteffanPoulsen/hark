//! What changed in each release: the repository's CHANGELOG.md, built into kuulo, so a new
//! version can say what is new without asking anyone.

use serde::Serialize;

use crate::update::is_newer;

const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// One release's lines as written: markdown with `**bold**` and `` `code` `` spans, a list
/// item keeping its "- ".
#[derive(Clone, Debug, Serialize)]
pub struct Notes {
    pub version: String,
    pub lines: Vec<String>,
}

/// This build's release.
pub fn this_version() -> Option<Notes> {
    releases(CHANGELOG).find(|n| n.version == crate::VERSION)
}

/// The releases after `seen` up to this build, newest first.
pub fn since(seen: &str) -> Vec<Notes> {
    between(CHANGELOG, seen, crate::VERSION)
}

fn between(text: &str, seen: &str, current: &str) -> Vec<Notes> {
    releases(text).filter(|n| is_newer(&n.version, seen) && !is_newer(&n.version, current)).collect()
}

fn releases(text: &str) -> impl Iterator<Item = Notes> + '_ {
    text.split("\n## ").skip(1).map(|section| {
        let mut lines = section.lines();
        let version = lines.next().unwrap_or("").trim().to_string();
        Notes { version, lines: lines.map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect() }
    })
}

#[cfg(test)]
mod tests {
    use super::between;

    const SAMPLE: &str = "# Changelog\n\nIntro.\n\n## Unreleased\n\n- next\n\n## 0.6.1\n\n- b\n\n## 0.6.0\n\n- a\n- a2\n\n## 0.5.4\n\n- old\n";

    fn versions(seen: &str, current: &str) -> Vec<String> {
        between(SAMPLE, seen, current).into_iter().map(|n| n.version).collect()
    }

    #[test]
    fn an_update_shows_every_release_it_skipped_and_none_after_it() {
        assert_eq!(versions("0.5.4", "0.6.1"), ["0.6.1", "0.6.0"]);
        assert_eq!(versions("0.5.4", "0.6.0"), ["0.6.0"]);
        assert!(versions("0.6.0", "0.6.0").is_empty());
        assert!(versions("0.6.1", "0.6.0").is_empty());
        assert_eq!(between(SAMPLE, "0.5.4", "0.6.0")[0].lines, ["- a", "- a2"]);
    }
}

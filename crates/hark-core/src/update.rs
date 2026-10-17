//! Whether a newer hark is out: the repository's latest GitHub release against this build.
//! Asked only when the user asks; hark makes no other network request.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

#[derive(Clone, Debug, Serialize)]
pub struct Release {
    pub version: String,
    /// The release's page, with its notes and downloads.
    pub url: String,
    pub newer: bool,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

pub fn releases_url() -> String {
    format!("{REPOSITORY}/releases")
}

pub fn latest() -> Result<Release> {
    let repo = REPOSITORY.strip_prefix("https://github.com/").ok_or_else(|| anyhow!("{REPOSITORY} is not a GitHub repository"))?;
    let r: GithubRelease = ureq::get(format!("https://api.github.com/repos/{repo}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", &format!("hark/{}", crate::VERSION))
        .call()?
        .body_mut()
        .read_json()?;
    let version = r.tag_name.trim_start_matches('v').to_string();
    let newer = is_newer(&version, crate::VERSION);
    Ok(Release { version, url: r.html_url, newer })
}

fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('-').next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(candidate) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.10.0", "0.9.3"));
        assert!(is_newer("0.4.0", "0.3.1"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.0", "0.3.1"));
    }
}

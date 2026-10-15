//! A project file: the target of the work, kept beside the game it serves. `<name>.hark` holds
//! JSON; the output folder is stored relative to the file when it sits below it, so the pair
//! moves together.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub const EXTENSION: &str = "hark";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// Favorites key on it, so renaming the file or the project loses nothing.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_root: Option<PathBuf>,
    /// Library folders this project cares about; empty means the whole library.
    #[serde(default)]
    pub scope: Vec<PathBuf>,
}

impl Project {
    pub fn new(name: &str) -> Project {
        Project { id: fresh_id(), name: name.to_string(), output_root: None, scope: Vec::new() }
    }

    /// Reads a project file; the output folder comes back absolute.
    pub fn load(path: &Path) -> Result<Project> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut p: Project = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        if p.id.is_empty() {
            return Err(anyhow!("{} has no id", path.display()));
        }
        if let (Some(out), Some(dir)) = (&p.output_root, path.parent()) {
            if out.is_relative() {
                p.output_root = Some(dir.join(out));
            }
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut on_disk = self.clone();
        if let (Some(out), Some(dir)) = (&self.output_root, path.parent()) {
            if let Ok(rel) = out.strip_prefix(dir) {
                on_disk.output_root = Some(rel.to_path_buf());
            }
        }
        crate::paths::ensure_parent(path)?;
        std::fs::write(path, serde_json::to_vec_pretty(&on_disk)?).with_context(|| format!("write {}", path.display()))
    }
}

/// The name a project file suggests when it cannot be read: its stem.
pub fn name_of_path(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// Eight hex digits from the clock and the process, enough to tell projects apart.
fn fresh_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mix = (nanos as u64) ^ ((std::process::id() as u64) << 32) ^ ((nanos >> 64) as u64);
    format!("{:08x}", (mix ^ (mix >> 29)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32)
}

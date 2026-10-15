//! A project file: the target of the work, kept beside the game it serves. `<name>.hark` holds
//! JSON: the library folders this project browses, where its exports go, and an id its
//! favorites key on. The output folder is stored relative to the file when it sits below it,
//! so the pair moves together. The project's name is the file's stem.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Root;

pub const EXTENSION: &str = "hark";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// Favorites key on it, so renaming the file loses nothing.
    pub id: String,
    /// The library folders this project browses. A new project has none.
    #[serde(default)]
    pub roots: Vec<Root>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_root: Option<PathBuf>,
    /// The folder under the output root the last export went to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_folder: Option<PathBuf>,
    /// From before folders belonged to projects: a narrowing of the then-global library, turned
    /// into `roots` by `Config::adopt`.
    #[serde(default, rename = "scope", skip_serializing)]
    pub legacy_scope: Vec<PathBuf>,
    /// The file predates per-project folders and showed the whole library; `Config::adopt`
    /// gives it the default folders.
    #[serde(skip)]
    pub legacy_whole_library: bool,
}

impl Project {
    pub fn new() -> Project {
        Project { id: fresh_id(), roots: Vec::new(), output_root: None, export_folder: None, legacy_scope: Vec::new(), legacy_whole_library: false }
    }

    /// Reads a project file; the output folder comes back absolute.
    pub fn load(path: &Path) -> Result<Project> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        let had_roots = value.get("roots").is_some();
        let mut p: Project = serde_json::from_value(value).with_context(|| format!("parse {}", path.display()))?;
        if p.id.is_empty() {
            return Err(anyhow!("{} has no id", path.display()));
        }
        p.legacy_scope = p.legacy_scope.iter().filter(|_| !had_roots).cloned().collect();
        p.legacy_whole_library = !had_roots && p.legacy_scope.is_empty();
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

/// A project's name: its file's stem.
pub fn name_of_path(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// Eight hex digits from the clock and the process, enough to tell projects apart.
fn fresh_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mix = (nanos as u64) ^ ((std::process::id() as u64) << 32) ^ ((nanos >> 64) as u64);
    format!("{:08x}", (mix ^ (mix >> 29)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32)
}

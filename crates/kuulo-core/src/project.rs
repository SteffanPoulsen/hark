//! A project file: the target of the work, kept beside the game it serves. `<name>.kuulo` holds
//! JSON: the library folders this project browses, where its exports go, where its copies
//! go when it has a place for them, and an id its favorites key on. The output and copy
//! folders are stored relative to the file when they sit below it, so the set moves together.
//! The project's name is the file's stem.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Root;

pub const EXTENSION: &str = "kuulo";

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
    /// Where this project's copies go; `None` leaves them to the config's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_folder: Option<PathBuf>,
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
    /// A project with an id of its own; no two are alike, so there is no default one.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Project {
        Project { id: fresh_id(), roots: Vec::new(), output_root: None, export_folder: None, copy_folder: None, legacy_scope: Vec::new(), legacy_whole_library: false }
    }

    /// Reads a project file; its folders come back absolute.
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
        if let Some(dir) = path.parent() {
            for folder in [&mut p.output_root, &mut p.copy_folder].into_iter().flatten() {
                if folder.is_relative() {
                    *folder = dir.join(&*folder);
                }
            }
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut on_disk = self.clone();
        if let Some(dir) = path.parent() {
            for folder in [&mut on_disk.output_root, &mut on_disk.copy_folder].into_iter().flatten() {
                if let Ok(rel) = folder.strip_prefix(dir) {
                    *folder = rel.to_path_buf();
                }
            }
        }
        crate::paths::write_whole(path, &serde_json::to_vec_pretty(&on_disk)?).with_context(|| format!("write {}", path.display()))
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

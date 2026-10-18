use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::keys::Keymap;
use crate::library::Sort;
use crate::pcm::{DownmixMode, FadeShape};
use crate::project::Project;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TagDef {
    pub name: String,
    /// `#rrggbb`. Loading the config gives a tag without one a color of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Wav,
    Flac,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "wav" => Ok(OutputFormat::Wav),
            "flac" => Ok(OutputFormat::Flac),
            _ => Err(format!("unknown format {s}: wav or flac")),
        }
    }
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Wav => "wav",
            OutputFormat::Flac => "flac",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Normalize {
    #[default]
    None,
    Peak { target_dbfs: f64 },
    Lufs { target_lufs: f64, true_peak_ceiling_dbfs: f64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExportPreset {
    #[serde(default)]
    pub format: OutputFormat,
    #[serde(default = "default_bits")]
    pub bits: u16,
    /// `None` keeps the source rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// `Some` downmixes to one channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono: Option<DownmixMode>,
    #[serde(default)]
    pub normalize: Normalize,
    #[serde(default = "default_true")]
    pub dither: bool,
    #[serde(default)]
    pub fade_shape: FadeShape,
    /// Output path inside the export folder. Placeholders: `{name}` source stem, `{root}` library
    /// root name, `{folder}` source folder name, `{relpath}` source folder relative to its root,
    /// `{date}` `YYYY-MM-DD`.
    #[serde(default = "default_template")]
    pub path_template: String,
}

fn default_bits() -> u16 {
    24
}
fn default_true() -> bool {
    true
}
fn default_template() -> String {
    "{name}".to_string()
}

impl ExportPreset {
    /// The values as the writer applies them: FLAC stops at 24 bit.
    pub fn effective(mut self) -> ExportPreset {
        if self.format == OutputFormat::Flac && self.bits > 24 {
            self.bits = 24;
        }
        self
    }
}

impl Default for ExportPreset {
    fn default() -> Self {
        ExportPreset {
            format: OutputFormat::Wav,
            bits: 24,
            sample_rate: None,
            mono: None,
            normalize: Normalize::None,
            dither: true,
            fade_shape: FadeShape::EqualPower,
            path_template: default_template(),
        }
    }
}

/// The favorites key of the default state, with no project open.
pub const DEFAULT_PROJECT: &str = "default";

/// The project file in use, loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveProject {
    pub path: PathBuf,
    pub project: Project,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// The default state's library folders; a project brings its own.
    #[serde(default)]
    pub roots: Vec<Root>,
    /// Where exports go with no project open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_root: Option<PathBuf>,
    /// The folder under the output root the last export went to, with no project open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_folder: Option<PathBuf>,
    /// Where copies go unless the open project has its own folder; `None` takes
    /// `paths::copies_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_folder: Option<PathBuf>,
    /// Project files hark knows: the switcher lists them.
    #[serde(default)]
    pub project_files: Vec<PathBuf>,
    /// The project file in use, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// Projects from when they lived in this file; written out as project files on load.
    #[serde(default, rename = "projects", skip_serializing)]
    legacy_projects: Vec<serde_json::Value>,
    #[serde(default, rename = "active_project", skip_serializing)]
    legacy_active: Option<String>,
    #[serde(skip)]
    pub active: Option<ActiveProject>,
    #[serde(default)]
    pub tags: Vec<TagDef>,
    #[serde(default)]
    pub presets: BTreeMap<String, ExportPreset>,
    #[serde(default = "default_preset_name")]
    pub default_preset: String,
    /// Sample rate the output device is opened at; `None` takes the device default.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "audition_sample_rate")]
    pub output_sample_rate: Option<u32>,
    /// The output device playback opens, by the id `audition::outputs` lists; `None` takes
    /// the system default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    /// Files whose decoded audio is larger than this many megabytes stream from disk while
    /// they play instead of being decoded whole.
    #[serde(default = "default_stream_above_mb")]
    pub stream_above_mb: f64,
    /// Window zoom on top of the display scale; Ctrl+= and Ctrl+- change it in the app.
    #[serde(default = "default_ui_zoom")]
    pub ui_zoom: f32,
    /// The order the window lists files in; the column headers change it.
    #[serde(default)]
    pub list_sort: Sort,
    /// Selecting a file in the window plays it.
    #[serde(default = "default_true")]
    pub autoplay: bool,
    /// Playback in the window loops the selection, or the whole file.
    #[serde(default)]
    pub looping: bool,
    /// A palette file (`{"bg": "#14121B", "accent": "#1AD1D1", ...}`) that overrides the built-in theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_file: Option<PathBuf>,
    /// The window's keys that differ from the defaults, action to chord.
    #[serde(default, skip_serializing_if = "Keymap::is_empty")]
    pub keys: Keymap,
}

fn default_stream_above_mb() -> f64 {
    256.0
}

fn default_ui_zoom() -> f32 {
    1.0
}

fn default_preset_name() -> String {
    "clean".to_string()
}

impl Default for Config {
    fn default() -> Self {
        let mut presets = BTreeMap::new();
        presets.insert("clean".to_string(), ExportPreset::default());
        presets.insert(
            "sfx".to_string(),
            ExportPreset {
                mono: Some(DownmixMode::Average),
                sample_rate: Some(48_000),
                normalize: Normalize::Lufs { target_lufs: -16.0, true_peak_ceiling_dbfs: -1.0 },
                ..ExportPreset::default()
            },
        );
        presets.insert(
            "ambient".to_string(),
            ExportPreset {
                sample_rate: Some(48_000),
                normalize: Normalize::Lufs { target_lufs: -23.0, true_peak_ceiling_dbfs: -1.0 },
                ..ExportPreset::default()
            },
        );
        presets.insert(
            "voice".to_string(),
            ExportPreset {
                bits: 16,
                mono: Some(DownmixMode::Average),
                sample_rate: Some(48_000),
                normalize: Normalize::Lufs { target_lufs: -18.0, true_peak_ceiling_dbfs: -1.0 },
                ..ExportPreset::default()
            },
        );
        presets.insert(
            "music".to_string(),
            ExportPreset {
                sample_rate: Some(48_000),
                normalize: Normalize::Lufs { target_lufs: -14.0, true_peak_ceiling_dbfs: -1.0 },
                ..ExportPreset::default()
            },
        );
        let tag = |(name, swatch): (&str, &str)| TagDef { name: name.into(), color: crate::tag_color::swatch(swatch).map(str::to_string) };
        let tags = [("sfx", "coral"), ("ambient", "teal"), ("music", "violet"), ("voice", "amber"), ("ui", "sky"), ("foley", "green")].map(tag).to_vec();
        Config {
            roots: Vec::new(),
            output_root: None,
            export_folder: None,
            copy_folder: None,
            project_files: Vec::new(),
            project: None,
            legacy_projects: Vec::new(),
            legacy_active: None,
            active: None,
            tags,
            presets,
            default_preset: default_preset_name(),
            output_sample_rate: None,
            output_device: None,
            stream_above_mb: default_stream_above_mb(),
            ui_zoom: default_ui_zoom(),
            list_sort: Sort::default(),
            autoplay: true,
            looping: false,
            theme_file: None,
            keys: Keymap::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Config> {
        Self::load_from(&crate::paths::config_file())
    }

    pub fn load_from(path: &Path) -> Result<Config> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut cfg: Config = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
                if cfg.migrate() | cfg.drop_tag_placeholders() | crate::tag_color::fill(&mut cfg.tags) {
                    cfg.save_to(path)?;
                }
                if let Some(file) = cfg.project.clone() {
                    match Project::load(&file) {
                        Ok(project) => {
                            let project = cfg.adopt(project, &file);
                            cfg.active = Some(ActiveProject { path: file, project });
                        }
                        Err(e) => {
                            eprintln!("project {}: {e:#}; back to the default", file.display());
                            cfg.project = None;
                        }
                    }
                }
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&crate::paths::config_file())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        crate::paths::write_whole(path, &serde_json::to_vec_pretty(self)?).with_context(|| format!("write {}", path.display()))
    }

    /// Projects that lived in the config become files under the config folder, keeping their
    /// name as the id so their favorites stay attached; the old default project's output
    /// folder becomes the default output folder. Returns whether anything moved.
    fn migrate(&mut self) -> bool {
        if self.legacy_projects.is_empty() {
            return false;
        }
        let dir = crate::paths::projects_dir();
        for v in std::mem::take(&mut self.legacy_projects) {
            let Some(name) = v.get("name").and_then(|n| n.as_str()).map(str::to_string) else { continue };
            let output_root = v.get("output_root").and_then(|o| o.as_str()).map(PathBuf::from);
            let scope: Vec<PathBuf> = v.get("scope").and_then(|s| serde_json::from_value(s.clone()).ok()).unwrap_or_default();
            if name == DEFAULT_PROJECT && scope.is_empty() {
                if self.output_root.is_none() {
                    self.output_root = output_root;
                }
                continue;
            }
            let file = dir.join(format!("{name}.{}", crate::project::EXTENSION));
            let project = Project { id: name.clone(), roots: self.roots_from_paths(&scope), output_root, export_folder: None, copy_folder: None, legacy_scope: Vec::new(), legacy_whole_library: scope.is_empty() };
            let project = { let mut p = project; if p.legacy_whole_library { p.roots = self.roots.clone(); } p };
            if let Err(e) = project.save(&file) {
                eprintln!("project {name}: {e:#}");
                continue;
            }
            if !self.project_files.contains(&file) {
                self.project_files.push(file.clone());
            }
            if self.legacy_active.as_deref() == Some(&name) {
                self.project = Some(file);
            }
        }
        self.legacy_active = None;
        true
    }

    /// Templates once put the first tag in the path; tags are metadata now, so that segment goes.
    fn drop_tag_placeholders(&mut self) -> bool {
        let mut changed = false;
        for p in self.presets.values_mut() {
            if p.path_template.contains("{tag") {
                let parts: Vec<&str> = p.path_template.split('/').filter(|s| !s.contains("{tag")).collect();
                p.path_template = if parts.is_empty() { default_template() } else { parts.join("/") };
                changed = true;
            }
        }
        changed
    }

    /// The folder under the output root exports go to, remembered per project.
    pub fn export_folder(&self) -> Option<&Path> {
        match &self.active {
            Some(a) => a.project.export_folder.as_deref(),
            None => self.export_folder.as_deref(),
        }
    }

    pub fn set_export_folder(&mut self, folder: Option<PathBuf>) -> Result<()> {
        match &mut self.active {
            Some(a) => {
                a.project.export_folder = folder;
                a.project.save(&a.path)
            }
            None => {
                self.export_folder = folder;
                self.save()
            }
        }
    }

    /// Where a copy goes: the open project's folder, else the config's, else hark's own.
    pub fn copy_folder(&self) -> PathBuf {
        self.active.as_ref().and_then(|a| a.project.copy_folder.clone()).or_else(|| self.copy_folder.clone()).unwrap_or_else(crate::paths::copies_dir)
    }

    /// The favorites key: the open project's id, or the default's.
    pub fn project_id(&self) -> &str {
        self.active.as_ref().map(|a| a.project.id.as_str()).unwrap_or(DEFAULT_PROJECT)
    }

    pub fn project_name(&self) -> String {
        self.active.as_ref().map(|a| crate::project::name_of_path(&a.path)).unwrap_or_else(|| "Default".into())
    }

    /// The library folders in use: the open project's, or the default state's.
    pub fn roots(&self) -> &[Root] {
        match &self.active {
            Some(a) => &a.project.roots,
            None => &self.roots,
        }
    }

    pub fn roots_mut(&mut self) -> &mut Vec<Root> {
        match &mut self.active {
            Some(a) => &mut a.project.roots,
            None => &mut self.roots,
        }
    }

    /// Writes whatever holds the folders and the output: the open project's file, or the config.
    pub fn save_target(&self) -> Result<()> {
        match &self.active {
            Some(a) => a.project.save(&a.path),
            None => self.save(),
        }
    }

    /// Folders from paths, named by the root they sit in or by their own name.
    fn roots_from_paths(&self, paths: &[PathBuf]) -> Vec<Root> {
        paths
            .iter()
            .map(|p| {
                let name = match self.roots.iter().filter(|r| p.starts_with(&r.path)).max_by_key(|r| r.path.as_os_str().len()) {
                    Some(r) if r.path == *p => r.name.clone(),
                    Some(r) => format!("{}/{}", r.name, p.strip_prefix(&r.path).map(|x| x.display().to_string()).unwrap_or_default()),
                    None => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
                };
                Root { name, path: p.clone() }
            })
            .collect()
    }

    /// Gives a project file from before per-project folders the folders it showed then, and
    /// writes it so the move happens once.
    fn adopt(&self, mut project: Project, path: &Path) -> Project {
        if project.legacy_whole_library {
            project.roots = self.roots.clone();
        } else if !project.legacy_scope.is_empty() {
            project.roots = self.roots_from_paths(&project.legacy_scope);
        } else {
            return project;
        }
        project.legacy_scope.clear();
        project.legacy_whole_library = false;
        if let Err(e) = project.save(path) {
            eprintln!("project {}: {e:#}", path.display());
        }
        project
    }

    pub fn output_root(&self) -> Option<&Path> {
        match &self.active {
            Some(a) => a.project.output_root.as_deref(),
            None => self.output_root.as_deref(),
        }
    }

    /// Sets the output folder of the open project, or the default one, and writes it down.
    pub fn set_output_root(&mut self, path: Option<PathBuf>) -> Result<()> {
        match &mut self.active {
            Some(a) => {
                a.project.output_root = path;
                a.project.save(&a.path)
            }
            None => {
                self.output_root = path;
                self.save()
            }
        }
    }

    /// Writes the open project's file after a change to it.
    pub fn save_project(&self) -> Result<()> {
        match &self.active {
            Some(a) => a.project.save(&a.path),
            None => Ok(()),
        }
    }

    /// Opens a project file, remembers it and makes it the one in use.
    pub fn open_project(&mut self, path: &Path) -> Result<()> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let project = self.adopt(Project::load(&path)?, &path);
        if !self.project_files.contains(&path) {
            self.project_files.push(path.clone());
        }
        self.project = Some(path.clone());
        self.active = Some(ActiveProject { path, project });
        self.save()
    }

    /// Writes a new, empty project file and opens it.
    pub fn create_project(&mut self, path: &Path) -> Result<()> {
        let mut path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if path.extension().and_then(|e| e.to_str()) != Some(crate::project::EXTENSION) {
            path.set_extension(crate::project::EXTENSION);
        }
        if path.exists() {
            return Err(anyhow!("{} exists", path.display()));
        }
        Project::new().save(&path)?;
        self.open_project(&path)
    }

    /// Back to the default state; the file stays known.
    pub fn close_project(&mut self) -> Result<()> {
        self.project = None;
        self.active = None;
        self.save()
    }

    /// Drops a project file from the known list, closing it if open. The file and its favorites
    /// stay.
    pub fn forget_project(&mut self, path: &Path) -> Result<()> {
        self.project_files.retain(|p| p != path);
        if self.project.as_deref() == Some(path) {
            self.project = None;
            self.active = None;
        }
        self.save()
    }

    /// A known project file by its path or its name.
    pub fn find_project(&self, key: &str) -> Option<PathBuf> {
        let as_path = std::path::absolute(key).ok();
        self.project_files.iter().find(|p| as_path.as_deref() == Some(p) || crate::project::name_of_path(p) == key).cloned()
    }

    pub fn root_of(&self, path: &Path) -> Option<&Root> {
        self.roots().iter().filter(|r| path.starts_with(&r.path)).max_by_key(|r| r.path.as_os_str().len())
    }

    pub fn tag(&self, name: &str) -> Option<&TagDef> {
        self.tags.iter().find(|t| t.name == name)
    }

    pub fn preset(&self, name: &str) -> Option<&ExportPreset> {
        self.presets.get(name)
    }

    /// A preset name not yet taken, from `base` or `base2`, `base3`, …
    pub fn free_preset_name(&self, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while self.presets.contains_key(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        name
    }

    /// Renames a preset; the default follows it.
    pub fn rename_preset(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        if new.is_empty() || self.presets.contains_key(new) {
            return Err(anyhow!("preset name {new:?} is taken or empty"));
        }
        let preset = self.presets.remove(old).ok_or_else(|| anyhow!("no preset {old}"))?;
        self.presets.insert(new.to_string(), preset);
        if self.default_preset == old {
            self.default_preset = new.to_string();
        }
        Ok(())
    }

    /// Removes a preset; the default falls back to the first remaining one. The last preset
    /// stays.
    pub fn remove_preset(&mut self, name: &str) -> Result<()> {
        if !self.presets.contains_key(name) {
            return Err(anyhow!("no preset {name}"));
        }
        if self.presets.len() == 1 {
            return Err(anyhow!("cannot remove the only preset"));
        }
        self.presets.remove(name);
        if self.default_preset == name {
            self.default_preset = self.presets.keys().next().cloned().unwrap_or_default();
        }
        Ok(())
    }

    /// The default preset, or the first one when the default names none.
    pub fn default_preset(&self) -> (String, &ExportPreset) {
        match self.presets.get(&self.default_preset) {
            Some(p) => (self.default_preset.clone(), p),
            None => self.presets.iter().next().map(|(n, p)| (n.clone(), p)).expect("at least one preset"),
        }
    }
}

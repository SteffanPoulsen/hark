use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::pcm::{DownmixMode, FadeShape};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TagDef {
    pub name: String,
    /// `#rrggbb`; a tag without one takes a color from its position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Export preset this tag selects by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Wav,
    Flac,
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrimSilence {
    pub threshold_dbfs: f64,
    pub pad_ms: f64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_silence: Option<TrimSilence>,
    #[serde(default = "default_true")]
    pub dither: bool,
    #[serde(default)]
    pub fade_shape: FadeShape,
    /// Output path relative to the output root. Placeholders: `{name}` source stem, `{tag}` first
    /// tag or `untagged`, `{tags}` all tags joined by `+`, `{root}` library root name, `{folder}`
    /// source folder name, `{relpath}` source folder relative to its root, `{date}` `YYYY-MM-DD`.
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
    "{tag}/{name}".to_string()
}

impl Default for ExportPreset {
    fn default() -> Self {
        ExportPreset {
            format: OutputFormat::Wav,
            bits: 24,
            sample_rate: None,
            mono: None,
            normalize: Normalize::None,
            trim_silence: None,
            dither: true,
            fade_shape: FadeShape::EqualPower,
            path_template: default_template(),
        }
    }
}

/// The target of the work: where exports go, which part of the library matters, and its own
/// favorites. Tags and presets stay global.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_root: Option<PathBuf>,
    /// Library folders this project cares about; empty means the whole library.
    #[serde(default)]
    pub scope: Vec<PathBuf>,
}

pub const DEFAULT_PROJECT: &str = "default";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub roots: Vec<Root>,
    /// Output folder from before projects existed; moved into the default project on load.
    #[serde(default, skip_serializing)]
    output_root: Option<PathBuf>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default = "default_project_name")]
    pub active_project: String,
    #[serde(default)]
    pub tags: Vec<TagDef>,
    #[serde(default)]
    pub presets: BTreeMap<String, ExportPreset>,
    #[serde(default = "default_preset_name")]
    pub default_preset: String,
    /// Device sample rate the audition engine asks for; `None` takes the device default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audition_sample_rate: Option<u32>,
    /// Window zoom on top of the display scale; Ctrl+= and Ctrl+- change it in the app.
    #[serde(default = "default_ui_zoom")]
    pub ui_zoom: f32,
    /// A palette file (`{"bg": "#14121B", "accent": "#1AD1D1", ...}`) that overrides the built-in theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_file: Option<PathBuf>,
}

fn default_ui_zoom() -> f32 {
    1.2
}

fn default_preset_name() -> String {
    "clean".to_string()
}

fn default_project_name() -> String {
    DEFAULT_PROJECT.to_string()
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
        let tag = |name: &str, preset: &str| TagDef { name: name.into(), color: None, preset: Some(preset.into()) };
        Config {
            roots: Vec::new(),
            output_root: None,
            projects: vec![Project { name: DEFAULT_PROJECT.into(), output_root: None, scope: Vec::new() }],
            active_project: DEFAULT_PROJECT.into(),
            tags: vec![tag("sfx", "sfx"), tag("ambient", "ambient"), tag("music", "music"), tag("voice", "voice"), tag("ui", "sfx"), tag("foley", "sfx")],
            presets,
            default_preset: default_preset_name(),
            audition_sample_rate: None,
            ui_zoom: default_ui_zoom(),
            theme_file: None,
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
                cfg.migrate();
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
        crate::paths::ensure_parent(path)?;
        std::fs::write(path, serde_json::to_vec_pretty(self)?).with_context(|| format!("write {}", path.display()))
    }

    fn migrate(&mut self) {
        if self.projects.is_empty() {
            self.projects.push(Project { name: DEFAULT_PROJECT.into(), output_root: self.output_root.take(), scope: Vec::new() });
        }
        if !self.projects.iter().any(|p| p.name == self.active_project) {
            self.active_project = self.projects[0].name.clone();
        }
    }

    pub fn project(&self) -> &Project {
        self.projects.iter().find(|p| p.name == self.active_project).unwrap_or(&self.projects[0])
    }

    pub fn project_mut(&mut self) -> &mut Project {
        let i = self.projects.iter().position(|p| p.name == self.active_project).unwrap_or(0);
        &mut self.projects[i]
    }

    pub fn output_root(&self) -> Option<&Path> {
        self.project().output_root.as_deref()
    }

    /// The folders the tree starts from under the active project: its scope, or every root.
    pub fn scope_folders(&self) -> Vec<(String, PathBuf)> {
        let project = self.project();
        if project.scope.is_empty() {
            self.roots.iter().map(|r| (r.name.clone(), r.path.clone())).collect()
        } else {
            project
                .scope
                .iter()
                .map(|p| {
                    let name = match self.root_of(p) {
                        Some(r) if r.path == *p => r.name.clone(),
                        Some(r) => format!("{}/{}", r.name, p.strip_prefix(&r.path).map(|x| x.display().to_string()).unwrap_or_default()),
                        None => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
                    };
                    (name, p.clone())
                })
                .collect()
        }
    }

    pub fn root_of(&self, path: &Path) -> Option<&Root> {
        self.roots.iter().filter(|r| path.starts_with(&r.path)).max_by_key(|r| r.path.as_os_str().len())
    }

    pub fn tag(&self, name: &str) -> Option<&TagDef> {
        self.tags.iter().find(|t| t.name == name)
    }

    pub fn preset(&self, name: &str) -> Option<&ExportPreset> {
        self.presets.get(name)
    }

    /// Preset the file's tags select: the first tag with a preset, else the default preset.
    pub fn preset_for_tags<'a>(&'a self, tags: &[String]) -> (String, &'a ExportPreset) {
        for t in tags {
            if let Some(name) = self.tags.iter().find(|d| &d.name == t).and_then(|d| d.preset.as_ref()) {
                if let Some(p) = self.presets.get(name) {
                    return (name.clone(), p);
                }
            }
        }
        match self.presets.get(&self.default_preset) {
            Some(p) => (self.default_preset.clone(), p),
            None => self.presets.iter().next().map(|(n, p)| (n.clone(), p)).expect("at least one preset"),
        }
    }
}

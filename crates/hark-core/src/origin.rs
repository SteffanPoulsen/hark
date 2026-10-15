//! What an exported file carries about where it came from and how it was made.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::ExportPreset;
use crate::pcm::FadeShape;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceRef {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_path: Option<PathBuf>,
    pub file_name: String,
    pub size: u64,
    pub blake3: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_secs: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct SelectionRef {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OutputRef {
    pub format: String,
    pub bits: u16,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_secs: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Measured {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_lufs: Option<f64>,
    pub applied_gain_db: f64,
    #[serde(default)]
    pub limited_by_true_peak: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_lufs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_true_peak_dbfs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trimmed: Option<SelectionRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Origin {
    pub hark: String,
    pub exported_at: String,
    pub source: SourceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionRef>,
    pub fade_in_secs: f64,
    pub fade_out_secs: f64,
    pub fade_shape: FadeShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_name: Option<String>,
    pub preset: ExportPreset,
    pub output: OutputRef,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub measured: Measured,
}

impl Origin {
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("origin serialises")
    }

    /// One line for humans and for metadata fields with a length limit.
    pub fn summary(&self) -> String {
        let src = match &self.source.root {
            Some(r) => format!("{}:{}", r, self.source.path.strip_prefix(self.source.root_path.as_deref().unwrap_or(Path::new(""))).unwrap_or(&self.source.path).display()),
            None => self.source.path.display().to_string(),
        };
        match self.selection {
            Some(s) => format!("hark: {} [{:.3}s-{:.3}s]", src, s.start_secs, s.end_secs),
            None => format!("hark: {}", src),
        }
    }
}

/// Reads the embedded origin from a file hark exported, by container.
pub fn read(path: &Path) -> Result<Option<Origin>> {
    let bytes = match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("wav") | Some("wave") | Some("bwf") => crate::wav::read_chunk(path, crate::wav::HARK_CHUNK)?,
        Some("flac") => crate::flac::read_application_block(path, crate::flac::HARK_APP_ID)?,
        _ => None,
    };
    match bytes {
        Some(b) => Ok(Some(serde_json::from_slice(&b)?)),
        None => Ok(None),
    }
}

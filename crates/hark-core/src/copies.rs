//! A copy: the selection as heard, written as a file for another program to take by drag
//! or paste. It runs the export pipeline with a fixed preset, since the export card's preset
//! masters a deliverable and a copy is the sound itself: WAV, 24 bit, the source's rate
//! and channels, no normalisation, the fades and gain applied, origin embedded.
//!
//! Every copy stays until the user clears the folder. The receiving program may reference
//! the file in place (Pro Tools, Reaper and Ableton at factory settings) or copy it after the
//! drop, and the drag protocol does not say which, so nothing here ever removes one.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use crate::config::{Config, ExportPreset, Root};
use crate::export::{ExportRequest, ExportResult, Selection};
use crate::pcm::FadeShape;

#[derive(Clone, Debug)]
pub struct CopyRequest {
    pub source: PathBuf,
    pub root: Option<Root>,
    pub selection: Option<Selection>,
    pub fade_in_secs: f64,
    pub fade_out_secs: f64,
    pub fade_shape: FadeShape,
    pub gain_db: f64,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub folder: PathBuf,
    /// The source's blake3 when known, as the library keeps it.
    pub source_hash: Option<String>,
}

impl CopyRequest {
    /// The whole file, as is, into the folder the config resolves.
    pub fn from_config(config: &Config, source: &Path, tags: Vec<String>, favorite: bool) -> CopyRequest {
        CopyRequest {
            source: source.to_path_buf(),
            root: config.root_of(source).cloned(),
            selection: None,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_shape: FadeShape::default(),
            gain_db: 0.0,
            tags,
            favorite,
            folder: config.copy_folder(),
            source_hash: None,
        }
    }

    fn preset(&self) -> ExportPreset {
        ExportPreset { fade_shape: self.fade_shape, ..ExportPreset::default() }
    }

    /// A copy already in the folder made from this very recipe: same source, selection, fades
    /// and gain. The origin inside each candidate says; the source matches by content hash
    /// when one is known, else by path and size.
    pub fn existing(&self) -> Option<PathBuf> {
        let stem = self.file_name();
        let stem = stem.trim_end_matches(".wav");
        let size = std::fs::metadata(&self.source).map(|m| m.len()).ok();
        let (_, files) = list(&self.folder);
        files.into_iter().find(|f| {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !name.starts_with(stem) || !name.ends_with(".wav") {
                return false;
            }
            let Ok(Some(o)) = crate::origin::read(f) else { return false };
            let same_source = match &self.source_hash {
                Some(h) => &o.source.blake3 == h,
                None => o.source.path == self.source && Some(o.source.size) == size,
            };
            same_source
                && o.selection.map(|s| (s.start_secs, s.end_secs)) == self.selection.map(|s| (s.start_secs, s.end_secs))
                && o.fade_in_secs == self.fade_in_secs
                && o.fade_out_secs == self.fade_out_secs
                && o.gain_db == self.gain_db
                && o.preset == self.preset()
        })
    }

    /// `<stem> <in>-<out>.wav`, or `<stem>.wav` for the whole file; times in seconds, since a
    /// colon is not a file name character everywhere.
    pub fn file_name(&self) -> String {
        let stem = self.source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "copy".into());
        match self.selection {
            Some(s) => format!("{stem} {:.3}-{:.3}.wav", s.start_secs, s.end_secs),
            None => format!("{stem}.wav"),
        }
    }

    /// Where this copy lands, and the export that writes it unless a copy from the same recipe
    /// is there already, so a repeated Ctrl+C or drag of one selection makes one file. The
    /// path is settled here, before anything is decoded, so a drag can start at once while
    /// the write runs elsewhere. A file a session may reference is never written over: a new
    /// recipe under a taken name gets ` 2`, ` 3`, …
    pub fn plan(&self) -> Result<Plan> {
        std::fs::create_dir_all(&self.folder).with_context(|| format!("create {}", self.folder.display()))?;
        if let Some(output) = self.existing() {
            return Ok(Plan { output, pending: None });
        }
        let name = self.file_name();
        let mut output = self.folder.join(&name);
        let mut n = 2;
        while output.exists() || part_of(&output).exists() {
            output = self.folder.join(format!("{} {n}.wav", name.trim_end_matches(".wav")));
            n += 1;
        }
        let export = ExportRequest {
            source: self.source.clone(),
            root: self.root.clone(),
            selection: self.selection,
            trim: false,
            fade_in_secs: self.fade_in_secs,
            fade_out_secs: self.fade_out_secs,
            gain_db: self.gain_db,
            preset_name: None,
            preset: self.preset(),
            output_root: self.folder.clone(),
            subfolder: None,
            output_path: Some(part_of(&output)),
            tags: self.tags.clone(),
            favorite: self.favorite,
            overwrite: true,
            source_hash: self.source_hash.clone(),
            measure_output: false,
        };
        Ok(Plan { output, pending: Some(export) })
    }
}

/// A copy with its path decided: there already, or still to be written.
#[derive(Clone, Debug)]
pub struct Plan {
    pub output: PathBuf,
    pub pending: Option<ExportRequest>,
}

/// The name a copy is written under until it is complete, so a receiver never opens half a
/// file; the listing skips it.
fn part_of(output: &Path) -> PathBuf {
    output.with_file_name(format!(".{}.part", output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
}

/// Writes the planned copy and moves it to its name, or reads back the one already there.
pub fn finish(plan: &Plan) -> Result<ExportResult> {
    match &plan.pending {
        None => {
            let origin = crate::origin::read(&plan.output)?.ok_or_else(|| anyhow!("{} carries no origin", plan.output.display()))?;
            Ok(ExportResult { output: plan.output.clone(), origin, warnings: Vec::new() })
        }
        Some(export) => {
            let result = crate::export::run(export)?;
            std::fs::rename(&result.output, &plan.output).with_context(|| format!("move {} to {}", result.output.display(), plan.output.display()))?;
            Ok(ExportResult { output: plan.output.clone(), ..result })
        }
    }
}

pub fn write(req: &CopyRequest) -> Result<ExportResult> {
    finish(&req.plan()?)
}

/// What a copy folder holds.
#[derive(Clone, Debug, Serialize)]
pub struct FolderSummary {
    pub folder: PathBuf,
    pub files: usize,
    pub bytes: u64,
}

/// The folder's files, newest first, with the total; a folder not yet created is empty.
pub fn list(folder: &Path) -> (FolderSummary, Vec<PathBuf>) {
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (e.path(), meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH)))
        })
        .collect();
    files.sort_by(|a, b| b.2.cmp(&a.2));
    let summary = FolderSummary { folder: folder.to_path_buf(), files: files.len(), bytes: files.iter().map(|f| f.1).sum() };
    (summary, files.into_iter().map(|f| f.0).collect())
}

/// Removes every file in the folder, the user's call; returns how many went.
pub fn clear(folder: &Path) -> Result<usize> {
    let (_, files) = list(folder);
    for f in &files {
        std::fs::remove_file(f).with_context(|| format!("remove {}", f.display()))?;
    }
    Ok(files.len())
}

//! A copy: the selection as it would export, written as a file for another program to take
//! by drag or paste. It runs the export pipeline with the preset the caller hands it, the
//! export card's values in the window, so what lands in a session is what an export would
//! have been: format, rate, channels and loudness as set, the fades and gain applied, origin
//! embedded. Only the folder and the name differ from an export.
//!
//! Every copy stays until the user clears the folder. The receiving program may reference
//! the file in place (Pro Tools, Reaper and Ableton at factory settings) or copy it after the
//! drop, and the drag protocol does not say which, so nothing here ever removes one.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use crate::config::{Config, ExportPreset, Root};
use crate::export::{ExportRequest, ExportResult, Selection};
use crate::loudness::Level;
use crate::pcm::Channels;

#[derive(Clone, Debug)]
pub struct CopyRequest {
    pub source: PathBuf,
    pub root: Option<Root>,
    pub selection: Option<Selection>,
    pub channels: Option<Channels>,
    pub reverse: bool,
    pub fade_in_secs: f64,
    pub fade_out_secs: f64,
    /// What the copy is written through; the path template is not used.
    pub preset: ExportPreset,
    pub gain_db: f64,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub folder: PathBuf,
    /// The source's blake3 when known, as the library keeps it.
    pub source_hash: Option<String>,
    /// Measurements the caller holds, for the export to take instead of measuring again.
    pub levels: Vec<Level>,
}

impl CopyRequest {
    /// The whole file through `preset`, into the folder the config resolves.
    pub fn from_config(config: &Config, source: &Path, preset: ExportPreset, tags: Vec<String>, favorite: bool) -> CopyRequest {
        CopyRequest {
            source: source.to_path_buf(),
            root: config.root_of(source).cloned(),
            selection: None,
            channels: None,
            reverse: false,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            preset,
            gain_db: 0.0,
            tags,
            favorite,
            folder: config.copy_folder(),
            source_hash: None,
            levels: Vec::new(),
        }
    }

    fn extension(&self) -> &'static str {
        self.preset.format.extension()
    }

    /// A copy already in the folder made from this very recipe: same source, selection,
    /// channels, direction, fades and gain. The origin inside each candidate says; the source matches by content hash
    /// when one is known, else by path and size.
    pub fn existing(&self) -> Option<PathBuf> {
        let stem = self.file_name();
        let stem = stem.trim_end_matches(self.extension());
        let size = std::fs::metadata(&self.source).map(|m| m.len()).ok();
        let (_, files) = list(&self.folder);
        files.into_iter().find(|f| {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !name.starts_with(stem) || !name.ends_with(self.extension()) {
                return false;
            }
            let Ok(Some(o)) = crate::origin::read(f) else { return false };
            let same_source = match &self.source_hash {
                Some(h) => &o.source.blake3 == h,
                None => o.source.path == self.source && Some(o.source.size) == size,
            };
            same_source
                && o.selection.map(|s| (s.start_secs, s.end_secs)) == self.selection.map(|s| (s.start_secs, s.end_secs))
                && o.channels == self.channels
                && o.reverse == self.reverse
                && o.fade_in_secs == self.fade_in_secs
                && o.fade_out_secs == self.fade_out_secs
                && o.gain_db == self.gain_db
                && o.preset == self.preset.clone().effective()
        })
    }

    /// Whether `other` writes the same copy, by the recipe `existing` matches a finished one
    /// by.
    pub fn same_recipe(&self, other: &CopyRequest) -> bool {
        self.source == other.source
            && self.folder == other.folder
            && self.selection == other.selection
            && self.channels == other.channels
            && self.reverse == other.reverse
            && self.fade_in_secs == other.fade_in_secs
            && self.fade_out_secs == other.fade_out_secs
            && self.gain_db == other.gain_db
            && self.preset.clone().effective() == other.preset.clone().effective()
    }

    /// `<stem> <in>-<out> ch<n> rev.<ext>`: the span, when there is one, in seconds, since a
    /// colon is not a file name character everywhere; the channels kept, when not all are;
    /// `rev` when it plays backwards.
    pub fn file_name(&self) -> String {
        let mut name = self.source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "copy".into());
        if let Some(s) = self.selection {
            name.push_str(&format!(" {:.3}-{:.3}", s.start_secs, s.end_secs));
        }
        if let Some(c) = self.channels {
            name.push_str(&format!(" ch{}", c.label()));
        }
        if self.reverse {
            name.push_str(" rev");
        }
        format!("{name}.{}", self.extension())
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
        // Creating the part file claims the name, so a second plan made before this one is
        // written takes the next.
        loop {
            if !output.exists() {
                match std::fs::OpenOptions::new().write(true).create_new(true).open(part_of(&output)) {
                    Ok(_) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e).with_context(|| format!("create {}", part_of(&output).display())),
                }
            }
            output = self.folder.join(format!("{} {n}.{}", name.trim_end_matches(self.extension()).trim_end_matches('.'), self.extension()));
            n += 1;
        }
        let export = ExportRequest {
            source: self.source.clone(),
            root: self.root.clone(),
            selection: self.selection,
            channels: self.channels,
            reverse: self.reverse,
            trim: false,
            fade_in_secs: self.fade_in_secs,
            fade_out_secs: self.fade_out_secs,
            gain_db: self.gain_db,
            preset_name: None,
            preset: self.preset.clone(),
            output_root: self.folder.clone(),
            subfolder: None,
            output_path: Some(part_of(&output)),
            tags: self.tags.clone(),
            favorite: self.favorite,
            overwrite: true,
            source_hash: self.source_hash.clone(),
            measure_output: false,
            levels: self.levels.clone(),
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

impl Plan {
    /// Lets go of the name this plan claimed, for a copy that will not be written.
    pub fn abandon(self) {
        if self.pending.is_some() {
            let _ = std::fs::remove_file(part_of(&self.output));
        }
    }
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
            let result = crate::export::run(export).inspect_err(|_| {
                let _ = std::fs::remove_file(part_of(&plan.output));
            })?;
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
    files.sort_by_key(|f| std::cmp::Reverse(f.2));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two recipes for one span planned before either is written, as a drag that follows a
    /// copy is, land under different names, and neither writes over the other.
    #[test]
    fn plans_in_flight_take_their_own_names() {
        let dir = std::env::temp_dir().join(format!("kuulo-copies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("hit.wav");
        let pcm = crate::pcm::Pcm { sample_rate: 48_000, channels: 1, samples: (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect() };
        crate::wav::write(&source, &pcm, 24, false, b"{}", &Default::default()).unwrap();
        let request = |gain_db: f64| CopyRequest {
            source: source.clone(),
            root: None,
            selection: Some(Selection { start_secs: 0.1, end_secs: 0.6 }),
            channels: None,
            reverse: false,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            preset: ExportPreset::default(),
            gain_db,
            tags: Vec::new(),
            favorite: false,
            folder: dir.join("copies"),
            source_hash: None,
            levels: Vec::new(),
        };
        let quiet = request(-6.0).plan().unwrap();
        let loud = request(0.0).plan().unwrap();
        assert_ne!(quiet.output, loud.output);
        finish(&quiet).unwrap();
        finish(&loud).unwrap();
        let gain_of = |p: &Path| crate::origin::read(p).unwrap().unwrap().gain_db;
        assert_eq!((gain_of(&quiet.output), gain_of(&loud.output)), (-6.0, 0.0));

        let mut missing = request(-3.0);
        missing.source = dir.join("gone.wav");
        let plan = missing.plan().unwrap();
        assert!(finish(&plan).is_err());
        assert!(!part_of(&plan.output).exists(), "a failed copy keeps its name claimed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

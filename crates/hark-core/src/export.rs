//! The export pipeline: decode, select, trim, fade, downmix, resample, normalise, encode, and
//! embed the origin so the file can say where it came from.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ExportPreset, Normalize, OutputFormat, Root};
use crate::loudness;
use crate::origin::{Measured, Origin, OutputRef, SelectionRef, SourceRef};
use crate::pcm::{db_to_gain, Pcm};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Selection {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub source: PathBuf,
    pub root: Option<Root>,
    pub selection: Option<Selection>,
    /// Shrink the selection, or the whole file, to its sound before anything else.
    pub trim: bool,
    pub fade_in_secs: f64,
    pub fade_out_secs: f64,
    /// The level chosen by hand, in dB; applied unless the preset normalises.
    pub gain_db: f64,
    pub preset_name: Option<String>,
    pub preset: ExportPreset,
    pub output_root: PathBuf,
    /// The folder under the output root the file goes to.
    pub subfolder: Option<PathBuf>,
    /// A full output path that replaces the template.
    pub output_path: Option<PathBuf>,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub overwrite: bool,
    /// The source's blake3 when the caller already knows it, so the file is not hashed again.
    pub source_hash: Option<String>,
    /// Measure the result's loudness for the origin and the BWF chunk. An export does; a copy
    /// skips it, since on a long selection the measurement costs more than the rest together.
    pub measure_output: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportResult {
    pub output: PathBuf,
    pub origin: Origin,
    pub warnings: Vec<String>,
}

impl ExportRequest {
    pub fn from_config(config: &Config, source: &Path, preset_name: Option<&str>, tags: Vec<String>, favorite: bool) -> Result<ExportRequest> {
        let output_root = config.output_root().map(|p| p.to_path_buf()).ok_or_else(|| anyhow!("{} has no output folder", config.project_name()))?;
        let (preset_name, preset) = match preset_name {
            Some(n) => (n.to_string(), config.preset(n).ok_or_else(|| anyhow!("unknown preset {n}"))?.clone()),
            None => {
                let (n, p) = config.default_preset();
                (n, p.clone())
            }
        };
        Ok(ExportRequest {
            source: source.to_path_buf(),
            root: config.root_of(source).cloned(),
            selection: None,
            trim: false,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            gain_db: 0.0,
            preset_name: Some(preset_name),
            preset,
            output_root,
            subfolder: config.export_folder().map(|p| p.to_path_buf()),
            output_path: None,
            tags,
            favorite,
            overwrite: false,
            source_hash: None,
            measure_output: true,
        })
    }

    /// An explicit output path with a known extension picks the format; the template takes the
    /// preset's.
    pub fn output_path(&self) -> PathBuf {
        if let Some(p) = &self.output_path {
            return p.clone();
        }
        let rendered = render_template(&self.preset.path_template, self);
        let mut path = match &self.subfolder {
            Some(sub) => self.output_root.join(sub).join(rendered),
            None => self.output_root.join(rendered),
        };
        path.set_extension(self.preset.format.extension());
        path
    }

    pub fn effective_preset(&self) -> ExportPreset {
        let mut preset = self.preset.clone();
        let ext = self.output_path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("flac") => preset.format = OutputFormat::Flac,
            Some("wav") | Some("wave") => preset.format = OutputFormat::Wav,
            _ => {}
        }
        preset.effective()
    }
}

pub fn render_template(template: &str, req: &ExportRequest) -> String {
    let stem = req.source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "export".into());
    let folder = req.source.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let relpath = match &req.root {
        Some(r) => req.source.parent().and_then(|p| p.strip_prefix(&r.path).ok()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        None => String::new(),
    };
    let root = req.root.as_ref().map(|r| r.name.clone()).unwrap_or_default();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let out = template
        .replace("{name}", &stem)
        .replace("{root}", &root)
        .replace("{folder}", &folder)
        .replace("{relpath}", &relpath)
        .replace("{date}", &date);
    out.split(['/', '\\']).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(std::path::MAIN_SEPARATOR_STR)
}

pub fn run(req: &ExportRequest) -> Result<ExportResult> {
    let output = req.output_path();
    if output.exists() && !req.overwrite {
        return Err(anyhow!("{} exists (use overwrite)", output.display()));
    }
    let source_meta = std::fs::metadata(&req.source).with_context(|| format!("stat {}", req.source.display()))?;
    let info = crate::decode::probe(&req.source)?;
    let preset = req.effective_preset();
    let mut req = req.clone();
    // Only the selection is decoded, so a short span of a long file costs what the span does.
    let mut source = match req.selection {
        Some(s) if s.end_secs <= s.start_secs => return Err(anyhow!("empty selection")),
        Some(s) => crate::decode::decode_span(&req.source, s.start_secs, s.end_secs)?,
        None => crate::decode::decode(&req.source)?,
    };
    if req.trim {
        let (a, b) = source.sound_span(0.0, source.duration_secs()).ok_or_else(|| anyhow!("silent; nothing to trim to"))?;
        let base = req.selection.map(|s| s.start_secs).unwrap_or(0.0);
        req.selection = Some(Selection { start_secs: base + a, end_secs: base + b });
        source = source.slice_frames(source.frame_at(a), source.frame_at(b));
    }
    let req = &req;
    let (pcm, warnings, measured) = process(source, req, &preset)?;
    let output_loudness = if req.measure_output { loudness::measure(&pcm).ok() } else { None };
    let measured = Measured {
        output_lufs: output_loudness.map(|l| l.integrated_lufs),
        output_true_peak_dbfs: output_loudness.map(|l| l.true_peak_dbfs),
        ..measured
    };
    let now = chrono::Local::now();
    let origin = Origin {
        hark: crate::VERSION.to_string(),
        exported_at: now.to_rfc3339(),
        source: SourceRef {
            path: req.source.clone(),
            root: req.root.as_ref().map(|r| r.name.clone()),
            root_path: req.root.as_ref().map(|r| r.path.clone()),
            file_name: req.source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            size: source_meta.len(),
            blake3: match &req.source_hash {
                Some(h) => h.clone(),
                None => crate::library::hash_file(&req.source)?,
            },
            sample_rate: info.sample_rate,
            channels: info.channels,
            duration_secs: info.duration_secs,
        },
        selection: req.selection.map(|s| SelectionRef { start_secs: s.start_secs, end_secs: s.end_secs }),
        fade_in_secs: req.fade_in_secs,
        fade_out_secs: req.fade_out_secs,
        gain_db: req.gain_db,
        fade_shape: preset.fade_shape,
        preset_name: req.preset_name.clone(),
        output_subfolder: req.subfolder.clone(),
        preset: preset.clone(),
        output: OutputRef {
            format: preset.format.extension().to_string(),
            bits: preset.bits,
            sample_rate: pcm.sample_rate,
            channels: pcm.channels,
            duration_secs: pcm.duration_secs(),
        },
        tags: req.tags.clone(),
        favorite: req.favorite,
        measured,
    };
    let json = origin.to_json();
    match preset.format {
        OutputFormat::Wav => {
            let bext = crate::wav::Bext {
                description: origin.summary(),
                originator: format!("hark {}", crate::VERSION),
                originator_reference: origin.source.blake3.chars().take(32).collect(),
                date: now.format("%Y-%m-%d").to_string(),
                time: now.format("%H:%M:%S").to_string(),
                loudness_lufs: output_loudness.map(|l| l.integrated_lufs),
                loudness_range_lu: output_loudness.and_then(|l| l.range_lu),
                max_true_peak_dbtp: output_loudness.map(|l| l.true_peak_dbfs),
            };
            crate::wav::write(&output, &pcm, preset.bits, preset.dither, &json, &bext)?;
        }
        OutputFormat::Flac => crate::flac::write(&output, &pcm, preset.bits, preset.dither, &json, &origin.summary())?,
    }
    Ok(ExportResult { output, origin, warnings })
}

/// Shapes the selection, already cut from the source, into the output.
fn process(mut pcm: Pcm, req: &ExportRequest, preset: &ExportPreset) -> Result<(Pcm, Vec<String>, Measured)> {
    let mut warnings = Vec::new();
    let mut measured = Measured::default();

    if req.fade_in_secs > 0.0 {
        pcm.fade_in((req.fade_in_secs * pcm.sample_rate as f64).round() as usize, preset.fade_shape);
    }
    if req.fade_out_secs > 0.0 {
        pcm.fade_out((req.fade_out_secs * pcm.sample_rate as f64).round() as usize, preset.fade_shape);
    }

    if let Some(mode) = preset.mono {
        pcm = pcm.to_mono(mode);
    }

    if let Some(rate) = preset.sample_rate {
        if rate != pcm.sample_rate {
            pcm = crate::resample::resample(&pcm, rate)?;
        }
    }

    match preset.normalize {
        Normalize::None => {
            if req.gain_db != 0.0 {
                measured.applied_gain_db = req.gain_db;
                pcm.apply_gain(db_to_gain(req.gain_db));
                let peak = pcm.peak();
                if peak > 1.0 {
                    warnings.push(format!("{:+.2} dB pushes the peak to {:+.2} dBFS; it clips", req.gain_db, 20.0 * (peak as f64).log10()));
                }
            }
        }
        Normalize::Peak { target_dbfs } => {
            let peak = pcm.peak();
            if peak > 0.0 {
                let gain = db_to_gain(target_dbfs) / peak;
                measured.applied_gain_db = 20.0 * (gain as f64).log10();
                pcm.apply_gain(gain);
            } else {
                warnings.push("silent output; peak normalisation skipped".into());
            }
        }
        Normalize::Lufs { target_lufs, true_peak_ceiling_dbfs } => {
            let l = loudness::measure(&pcm)?;
            measured.source_lufs = Some(l.integrated_lufs);
            if l.integrated_lufs.is_finite() && l.integrated_lufs > -70.0 {
                let mut gain_db = target_lufs - l.integrated_lufs;
                if l.true_peak_dbfs + gain_db > true_peak_ceiling_dbfs {
                    gain_db = true_peak_ceiling_dbfs - l.true_peak_dbfs;
                    measured.limited_by_true_peak = true;
                    warnings.push(format!(
                        "gain held to {gain_db:+.2} dB by the {true_peak_ceiling_dbfs} dBTP ceiling; output sits below {target_lufs} LUFS"
                    ));
                }
                if l.momentary {
                    warnings.push("too short for integrated loudness; normalised on the loudest 400 ms window".into());
                }
                measured.applied_gain_db = gain_db;
                pcm.apply_gain(db_to_gain(gain_db));
            } else {
                warnings.push("silent output; loudness normalisation skipped".into());
            }
        }
    }

    Ok((pcm, warnings, measured))
}

/// Rebuilds the request an exported file carries, so it can be run again with changes.
pub fn request_from_origin(origin: &Origin, output_root: &Path, output_path: Option<PathBuf>) -> ExportRequest {
    ExportRequest {
        source: origin.source.path.clone(),
        root: match (&origin.source.root, &origin.source.root_path) {
            (Some(name), Some(path)) => Some(Root { name: name.clone(), path: path.clone() }),
            _ => None,
        },
        trim: false,
        selection: origin.selection.map(|s| Selection { start_secs: s.start_secs, end_secs: s.end_secs }),
        fade_in_secs: origin.fade_in_secs,
        fade_out_secs: origin.fade_out_secs,
        gain_db: origin.gain_db,
        preset_name: origin.preset_name.clone(),
        preset: origin.preset.clone(),
        output_root: output_root.to_path_buf(),
        subfolder: origin.output_subfolder.clone(),
        output_path,
        tags: origin.tags.clone(),
        favorite: origin.favorite,
        overwrite: true,
        source_hash: None,
        measure_output: true,
    }
}

//! The export pipeline: decode, select, trim, fade, downmix, resample, normalise, encode, and
//! embed the origin so the file can say where it came from.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ExportPreset, Normalize, OutputFormat, Root};
use crate::loudness::{self, Level, Loudness, Span};
use crate::origin::{Measured, Origin, OutputRef, SelectionRef, SourceRef};
use crate::pcm::{db_to_gain, gain_to_db, Channels, Fades, Pcm};

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
    /// The source's channels kept, before anything else is done; `None` keeps them all.
    pub channels: Option<Channels>,
    /// Shrink the selection, or the whole file, to the sound in its kept channels.
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
    /// Measurements the caller already holds. One of exactly the audio this export shapes
    /// stands in for measuring it again.
    pub levels: Vec<Level>,
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
            channels: None,
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
            levels: Vec::new(),
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

    pub fn fades(&self, preset: &ExportPreset) -> Fades {
        Fades { in_secs: self.fade_in_secs, out_secs: self.fade_out_secs, shape: preset.fade_shape }
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
    if let Some(pick) = req.channels {
        if !pick.fits(info.channels) {
            return Err(anyhow!("{} has {} channels; it has no channel {}", req.source.display(), info.channels, pick.label()));
        }
        source = source.pick(pick);
    }
    if req.trim {
        let (a, b) = source.sound_span(0.0, source.duration_secs(), None).ok_or_else(|| anyhow!("silent; nothing to trim to"))?;
        let base = req.selection.map(|s| s.start_secs).unwrap_or(0.0);
        req.selection = Some(Selection { start_secs: base + a, end_secs: base + b });
        source = source.slice_frames(source.frame_at(a), source.frame_at(b));
    }
    let req = &req;
    let span = Span::new(&req.source, req.selection.map(|s| (s.start_secs, s.end_secs)), req.fades(&preset), preset.mono, preset.sample_rate, source.sample_rate).picking(req.channels);
    let start = req.selection.map(|s| (s.start_secs.max(0.0) * source.sample_rate as f64).round() as u64).unwrap_or(0);
    let frames = (start, start + source.frames() as u64);
    let stamp = loudness::Stamp::from(&source_meta);
    let known = req.levels.iter().find(|l| l.span == span && l.stamp == stamp && l.frames == frames).map(|l| l.loudness);
    let (pcm, warnings, measured) = process(source, req, &preset, known)?;
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
        channels: req.channels,
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

/// The selection, already cut from the source, faded, folded and resampled: what an export's
/// level is set on, and what a measurement of that level is of.
pub fn shape(mut pcm: Pcm, fades: Fades, fold: Option<crate::pcm::DownmixMode>, rate: Option<u32>) -> Result<Pcm> {
    fades.apply(&mut pcm);
    if let Some(mode) = fold {
        pcm = pcm.to_mono(mode);
    }
    match rate {
        Some(rate) if rate != pcm.sample_rate => crate::resample::resample(&pcm, rate),
        _ => Ok(pcm),
    }
}

/// Shapes the selection into the output and sets its level, from `known` when the caller
/// measured exactly this audio already.
fn process(pcm: Pcm, req: &ExportRequest, preset: &ExportPreset, known: Option<Loudness>) -> Result<(Pcm, Vec<String>, Measured)> {
    let mut warnings = Vec::new();
    let mut measured = Measured::default();
    let mut pcm = shape(pcm, req.fades(preset), preset.mono, preset.sample_rate)?;

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
        normalize => {
            if let Some(gain_db) = normalize_gain(&normalize, pcm.peak(), || known.map(Ok).unwrap_or_else(|| loudness::measure(&pcm)), &mut measured, &mut warnings)? {
                measured.applied_gain_db = gain_db;
                pcm.apply_gain(db_to_gain(gain_db));
            }
        }
    }

    Ok((pcm, warnings, measured))
}

/// The gain in dB a normalising preset gives audio with this sample peak and loudness, or
/// `None` for silence, which stays as it is. `loudness` is only measured for a loudness
/// target.
fn normalize_gain(normalize: &Normalize, sample_peak: f32, loudness: impl FnOnce() -> Result<Loudness>, measured: &mut Measured, warnings: &mut Vec<String>) -> Result<Option<f64>> {
    match *normalize {
        Normalize::None => Ok(None),
        Normalize::Peak { target_dbfs } => {
            if sample_peak > 0.0 {
                Ok(Some(target_dbfs - gain_to_db(sample_peak as f64)))
            } else {
                warnings.push("silent output; peak normalisation skipped".into());
                Ok(None)
            }
        }
        Normalize::Lufs { target_lufs, true_peak_ceiling_dbfs } => {
            let l = loudness()?;
            measured.source_lufs = Some(l.integrated_lufs);
            if !(l.integrated_lufs.is_finite() && l.integrated_lufs > -70.0) {
                warnings.push("silent output; loudness normalisation skipped".into());
                return Ok(None);
            }
            let mut gain_db = target_lufs - l.integrated_lufs;
            if l.true_peak_dbfs + gain_db > true_peak_ceiling_dbfs {
                gain_db = true_peak_ceiling_dbfs - l.true_peak_dbfs;
                measured.limited_by_true_peak = true;
                warnings.push(format!("gain held to {gain_db:+.2} dB by the {true_peak_ceiling_dbfs} dBTP ceiling; output sits below {target_lufs} LUFS"));
            }
            if l.momentary {
                warnings.push("too short for integrated loudness; normalised on the loudest 400 ms window".into());
            }
            Ok(Some(gain_db))
        }
    }
}

/// The level the preview plays a file at under a normalising preset.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PreviewLevel {
    /// `None` when the preset does not normalise or the file is silent.
    pub gain_db: Option<f64>,
    pub warnings: Vec<String>,
}

/// The gain `preset` would give `selection` of `path`, or the whole of it, with `channels`
/// kept and under `fades`, by the rule an export applies to the audio an export shapes.
/// Measured in one streaming pass, so a long span is never held whole.
pub fn preview_level(path: &Path, preset: &ExportPreset, selection: Option<Selection>, channels: Option<Channels>, fades: Fades, abort: &dyn Fn() -> bool) -> Result<PreviewLevel> {
    if matches!(preset.normalize, Normalize::None) {
        return Ok(PreviewLevel::default());
    }
    let source_rate = crate::decode::probe(path)?.sample_rate;
    let span = Span::new(path, selection.map(|s| (s.start_secs, s.end_secs)), fades, preset.mono, preset.sample_rate, source_rate).picking(channels);
    Ok(preview_level_from(&loudness::measure_file(&span, abort)?.loudness, preset))
}

/// `preview_level` from a measurement of the span folded as `preset` folds it.
pub fn preview_level_from(l: &Loudness, preset: &ExportPreset) -> PreviewLevel {
    let mut level = PreviewLevel::default();
    level.gain_db = normalize_gain(&preset.normalize, db_to_gain(l.sample_peak_dbfs), || Ok(*l), &mut Measured::default(), &mut level.warnings).unwrap_or_default();
    level
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
        channels: origin.channels,
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
        levels: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm::{DownmixMode, FadeShape};

    /// A span streamed from the file reads as the export reads the audio it shapes from that
    /// span, through fades, a fold and a rate change; an export handed that measurement sets
    /// its level from it, and one handed a measurement of other frames measures for itself.
    #[test]
    fn streamed_level_is_the_exports_own() {
        let dir = std::env::temp_dir().join(format!("hark-export-level-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("bed.wav");
        let rate = 44_100usize;
        let samples: Vec<f32> = (0..rate * 4 * 2).map(|i| (((i / 2) as f32 / rate as f32) * 300.0 * std::f32::consts::TAU).sin() * if i % 2 == 0 { 0.6 } else { 0.2 }).collect();
        crate::wav::write(&source, &Pcm { sample_rate: rate as u32, channels: 2, samples }, 24, false, b"{}", &Default::default()).unwrap();

        let preset = ExportPreset { sample_rate: Some(48_000), mono: Some(DownmixMode::Average), normalize: Normalize::Lufs { target_lufs: -20.0, true_peak_ceiling_dbfs: -1.0 }, fade_shape: FadeShape::Linear, ..Default::default() };
        let mut req = ExportRequest {
            source: source.clone(),
            root: None,
            selection: Some(Selection { start_secs: 0.7, end_secs: 3.1 }),
            channels: None,
            trim: false,
            fade_in_secs: 0.25,
            fade_out_secs: 0.5,
            gain_db: 0.0,
            preset_name: None,
            preset: preset.clone(),
            output_root: dir.clone(),
            subfolder: None,
            output_path: Some(dir.join("out.wav")),
            tags: Vec::new(),
            favorite: false,
            overwrite: true,
            source_hash: None,
            measure_output: false,
            levels: Vec::new(),
        };
        let span = Span::new(&source, Some((0.7, 3.1)), req.fades(&preset), preset.mono, preset.sample_rate, rate as u32);
        let streamed = loudness::measure_file(&span, &|| false).unwrap();
        let own = run(&req).unwrap().origin.measured;
        assert!((streamed.loudness.integrated_lufs - own.source_lufs.unwrap()).abs() < 1e-4, "{} vs {} LUFS", streamed.loudness.integrated_lufs, own.source_lufs.unwrap());

        let mut doctored = streamed.clone();
        doctored.loudness.integrated_lufs -= 6.0;
        doctored.loudness.true_peak_dbfs = -40.0;
        req.levels = vec![doctored.clone()];
        let taken = run(&req).unwrap().origin.measured;
        assert!((taken.applied_gain_db - (own.applied_gain_db + 6.0)).abs() < 1e-6, "the handed level was not used");

        let mut other_frames = doctored.clone();
        other_frames.frames.1 += 1;
        req.levels = vec![other_frames];
        assert_eq!(run(&req).unwrap().origin.measured.applied_gain_db, own.applied_gain_db, "a level of other frames was used");

        let mut overwritten = doctored;
        overwritten.stamp.len += 1;
        req.levels = vec![overwritten];
        assert_eq!(run(&req).unwrap().origin.measured.applied_gain_db, own.applied_gain_db, "a level of the file before it was written over was used");

        // A fade-in placed by the length the header states, over a file cut short of it.
        let bytes = std::fs::read(&source).unwrap();
        std::fs::write(&source, &bytes[..bytes.len() * 3 / 5]).unwrap();
        req.selection = Some(Selection { start_secs: 0.0, end_secs: 3.5 });
        (req.fade_in_secs, req.fade_out_secs) = (3.0, 0.0);
        req.levels = Vec::new();
        let fresh = run(&req).unwrap().origin.measured;
        let mut cut = loudness::measure_file(&Span::new(&source, Some((0.0, 3.5)), req.fades(&preset), preset.mono, preset.sample_rate, rate as u32), &|| false).unwrap();
        cut.loudness.integrated_lufs -= 6.0;
        req.levels = vec![cut];
        let after_cut = run(&req).unwrap().origin.measured;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(after_cut.applied_gain_db, fresh.applied_gain_db, "a level faded by the header's length was used");
    }

    /// A channel picked from a file with a dead side exports alone, the file it makes says
    /// which, and an origin from when the preset kept that side re-exports the same audio.
    #[test]
    fn picked_channel_exports_alone() {
        let dir = std::env::temp_dir().join(format!("hark-export-pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("dual.wav");
        let left = |f: usize| (f as f32 * 0.03).sin() * 0.5;
        let samples: Vec<f32> = (0..48_000).flat_map(|f| [left(f), 0.0]).collect();
        crate::wav::write(&source, &Pcm { sample_rate: 48_000, channels: 2, samples }, 32, false, b"{}", &Default::default()).unwrap();
        let mut req = request_from_origin(&Origin { channels: Some("1".parse().unwrap()), ..stub_origin(&source) }, &dir, Some(dir.join("left.wav")));
        req.preset = ExportPreset { bits: 32, ..Default::default() };
        let result = run(&req).unwrap();
        let out = crate::decode::decode(&result.output).unwrap();
        assert_eq!(out.channels, 1);
        assert!(out.samples.iter().enumerate().all(|(f, s)| (s - left(f)).abs() < 1e-6), "the left channel was not what was written");
        let written = crate::origin::read(&result.output).unwrap().unwrap();
        assert_eq!(written.channels.map(|c| c.label()), Some("1".to_string()));

        let mut legacy = serde_json::to_value(&written).unwrap();
        legacy.as_object_mut().unwrap().remove("channels");
        legacy["preset"]["mono"] = "left".into();
        crate::wav::write(&dir.join("legacy.wav"), &out, 32, false, &serde_json::to_vec(&legacy).unwrap(), &Default::default()).unwrap();
        let upgraded = crate::origin::read(&dir.join("legacy.wav")).unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((upgraded.channels, upgraded.preset.mono), (written.channels, Some(crate::pcm::DownmixMode::Average)));
    }

    fn stub_origin(source: &Path) -> Origin {
        let info = crate::decode::probe(source).unwrap();
        Origin {
            hark: String::new(),
            exported_at: String::new(),
            source: SourceRef { path: source.to_path_buf(), root: None, root_path: None, file_name: String::new(), size: 0, blake3: String::new(), sample_rate: info.sample_rate, channels: info.channels, duration_secs: info.duration_secs },
            selection: None,
            channels: None,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            gain_db: 0.0,
            fade_shape: FadeShape::Linear,
            preset_name: None,
            output_subfolder: None,
            preset: ExportPreset::default(),
            output: OutputRef { format: "wav".into(), bits: 32, sample_rate: info.sample_rate, channels: 1, duration_secs: info.duration_secs },
            tags: Vec::new(),
            favorite: false,
            measured: Measured::default(),
        }
    }
}

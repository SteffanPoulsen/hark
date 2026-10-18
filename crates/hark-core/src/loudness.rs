use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use ebur128::{EbuR128, Mode};
use serde::{Deserialize, Serialize};

use crate::pcm::{gain_to_db, Channels, DownmixMode, Fades, Pcm};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Loudness {
    /// EBU R128 integrated loudness. For material shorter than the 400 ms gating block this is
    /// the loudest momentary reading of the silence-padded signal instead, and `momentary` is set.
    pub integrated_lufs: f64,
    pub momentary: bool,
    pub true_peak_dbfs: f64,
    pub sample_peak_dbfs: f64,
    pub range_lu: Option<f64>,
}

/// The shortest material EBU R128 gates: one momentary block.
const GATING_BLOCK_SECS: f64 = 0.4;

/// An EBU R128 meter fed interleaved audio in any chunking.
pub struct Meter {
    meter: EbuR128,
    channels: u32,
}

impl Meter {
    pub fn new(channels: u16, sample_rate: u32) -> Result<Meter> {
        let channels = channels as u32;
        let meter = EbuR128::new(channels, sample_rate, Mode::I | Mode::M | Mode::LRA | Mode::TRUE_PEAK | Mode::SAMPLE_PEAK)?;
        Ok(Meter { meter, channels })
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<()> {
        self.meter.add_frames_f32(samples)?;
        Ok(())
    }

    /// `None` when the material is too short to gate; `peaks` still holds its levels then.
    pub fn finish(&self) -> Option<Loudness> {
        let integrated_lufs = self.meter.loudness_global().ok().filter(|v| v.is_finite())?;
        let (true_peak_dbfs, sample_peak_dbfs) = self.peaks();
        let range_lu = self.meter.loudness_range().ok().filter(|v| v.is_finite());
        Some(Loudness { integrated_lufs, momentary: false, true_peak_dbfs, sample_peak_dbfs, range_lu })
    }

    /// True peak and sample peak in dBFS over all channels.
    pub fn peaks(&self) -> (f64, f64) {
        let max = |f: &dyn Fn(u32) -> Result<f64, ebur128::Error>| -> f64 { (0..self.channels).filter_map(|c| f(c).ok()).fold(0.0, f64::max) };
        (gain_to_db(max(&|c| self.meter.true_peak(c))), gain_to_db(max(&|c| self.meter.sample_peak(c))))
    }
}

pub fn measure(pcm: &Pcm) -> Result<Loudness> {
    measure_with(pcm, &|| false)
}

/// `measure`, giving up between seconds of audio as soon as `abort` says so.
pub fn measure_with(pcm: &Pcm, abort: &dyn Fn() -> bool) -> Result<Loudness> {
    let mut meter = Meter::new(pcm.channels, pcm.sample_rate)?;
    for second in pcm.samples.chunks((pcm.sample_rate as usize * pcm.channels as usize).max(1)) {
        meter.push(second)?;
        if abort() {
            anyhow::bail!("superseded");
        }
    }
    match meter.finish() {
        Some(l) => Ok(l),
        None => {
            let (true_peak_dbfs, sample_peak_dbfs) = meter.peaks();
            Ok(Loudness { integrated_lufs: momentary_max(pcm)?, momentary: true, true_peak_dbfs, sample_peak_dbfs, range_lu: None })
        }
    }
}

/// A span of a file shaped as an export shapes it before setting its level: cut, its channels
/// picked, reversed, faded, folded to one channel, resampled. What a measurement of a level
/// is of.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub path: PathBuf,
    /// Seconds into the file; `None` is all of it.
    pub secs: Option<(f64, f64)>,
    /// The channels kept; `None` keeps them all.
    pub channels: Option<Channels>,
    pub reverse: bool,
    pub fades: Fades,
    pub fold: Option<DownmixMode>,
    /// The rate it is resampled to; `None` keeps the file's own.
    pub rate: Option<u32>,
}

impl Span {
    /// `rate` is the one a preset asks for, which resamples nothing when it is `source_rate`.
    pub fn new(path: &Path, secs: Option<(f64, f64)>, fades: Fades, fold: Option<DownmixMode>, rate: Option<u32>, source_rate: u32) -> Span {
        Span { path: path.to_path_buf(), secs, channels: None, reverse: false, fades, fold, rate: rate.filter(|r| *r != source_rate) }
    }

    pub fn picking(self, channels: Option<Channels>) -> Span {
        Span { channels, ..self }
    }

    pub fn reversing(self, reverse: bool) -> Span {
        Span { reverse, ..self }
    }
}

/// A file's size and modification time when its audio was read: what tells a measurement of
/// the file from one of the file as it was before something wrote over it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl Stamp {
    pub fn of(path: &Path) -> Result<Stamp> {
        Ok(Stamp::from(&std::fs::metadata(path)?))
    }
}

impl From<&std::fs::Metadata> for Stamp {
    fn from(meta: &std::fs::Metadata) -> Stamp {
        Stamp { len: meta.len(), modified: meta.modified().ok() }
    }
}

/// The loudness of a span, the file as it was read, and the frames of it measured, so whoever
/// holds the audio can tell the measurement is of exactly that audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub span: Span,
    pub stamp: Stamp,
    /// The first frame of the file measured, and one past the last, at the file's rate.
    pub frames: (u64, u64),
    pub loudness: Loudness,
}

/// Measures a span one packet at a time, shaped as an export shapes it before setting its
/// level; only a clip too short to gate, or reversed, is decoded whole. Gives up as soon as
/// `abort` says so.
pub fn measure_file(span: &Span, abort: &dyn Fn() -> bool) -> Result<Level> {
    let Span { path, fades, fold, .. } = span;
    let (fades, fold) = (*fades, *fold);
    let stamp = Stamp::of(path)?;
    let mut reader = crate::decode::Reader::open(path)?;
    let channels = reader.channels as usize;
    if let Some(pick) = span.channels.filter(|c| !c.fits(reader.channels)) {
        anyhow::bail!("{} has {channels} channels; it has no channel {}", path.display(), pick.label());
    }
    let kept = span.channels.map_or(channels, |c| c.count());
    let rate = reader.sample_rate as f64;
    let (start, end) = match span.secs {
        Some((a, b)) if rate > 0.0 => ((a.max(0.0) * rate).round() as u64, (b.max(0.0) * rate).round() as u64),
        _ => (0, u64::MAX),
    };
    if span.reverse {
        return measure_decoded(span, stamp, start, abort);
    }
    // A fade-out needs the clip's length: the span's, cut to the length the header states, as
    // an export's decode of it is; a header that states no length leaves the fade-out out.
    let total = match (reader.frames, end) {
        (Some(n), end) => end.min(n).saturating_sub(start) as usize,
        (None, u64::MAX) => usize::MAX,
        (None, end) => (end - start) as usize,
    };
    // A seek may land late in some codecs, so it aims a second early and the frames before
    // the span are dropped, as `decode_span` does.
    let mut counted = if start > 0 { reader.seek(start.saturating_sub(reader.sample_rate as u64))? } else { 0 };
    let out_rate = span.rate.unwrap_or(reader.sample_rate);
    let out_channels = if fold.is_some() { 1 } else { kept as u16 };
    let mut resampler = crate::resample::Stream::new(reader.sample_rate, out_rate, out_channels)?;
    let mut meter = Meter::new(out_channels, out_rate)?;
    let mut chunk = Vec::new();
    let mut picked = Vec::new();
    let mut faded = Vec::new();
    let mut folded = Vec::new();
    let mut pushed = 0u64;
    while reader.next_chunk(&mut chunk)? {
        let frames = (chunk.len() / channels.max(1)) as u64;
        let pos = reader.chunk_start.unwrap_or(counted);
        counted = pos + frames;
        if pos >= end {
            break;
        }
        let from = start.saturating_sub(pos).min(frames) as usize;
        let to = (end - pos).min(frames) as usize;
        let mut samples = &chunk[from * channels..to * channels];
        let first = (pos + from as u64).saturating_sub(start) as usize;
        if let Some(pick) = span.channels {
            picked.clear();
            let mut frame = [0f32; Channels::MAX];
            for f in samples.chunks(channels) {
                let n = pick.take(f, &mut frame);
                picked.extend_from_slice(&frame[..n]);
            }
            samples = &picked;
        }
        if !fades.is_zero() {
            faded.clear();
            faded.extend(samples.chunks(kept).enumerate().flat_map(|(k, f)| {
                let g = fades.gain(first + k, total, reader.sample_rate);
                f.iter().map(move |s| s * g)
            }));
            samples = &faded;
        }
        if let Some(mode) = fold {
            folded.clear();
            folded.extend(samples.chunks(kept).map(|f| mode.fold(f)));
            samples = &folded;
        }
        resampler.push(samples, &mut |s| meter.push(s))?;
        pushed += (to - from) as u64;
        chunk.clear();
        if abort() {
            anyhow::bail!("superseded");
        }
    }
    resampler.finish(&mut |s| meter.push(s))?;
    // The frames measured, unless the fades were placed by a length the decode did not bear
    // out: then the measurement is of other audio than an export's, and says so by not matching.
    let measured_end = if fades.is_zero() || total as u64 == pushed { start + pushed } else { u64::MAX };
    let level = |loudness| Level { span: span.clone(), stamp, frames: (start, measured_end), loudness };
    match meter.finish() {
        Some(l) => Ok(level(l)),
        // Long enough to gate and nothing above the gate: silence, read as `measure` reads it,
        // without decoding a span that may be hours long.
        None if pushed as f64 >= GATING_BLOCK_SECS * rate => {
            let (true_peak_dbfs, sample_peak_dbfs) = meter.peaks();
            Ok(level(Loudness { integrated_lufs: -70.0, momentary: true, true_peak_dbfs, sample_peak_dbfs, range_lu: None }))
        }
        None => measure_decoded(span, stamp, start, abort),
    }
}

/// Measures a span decoded whole, for one too short to gate or one that plays in reverse,
/// which a reader going forwards cannot shape. `start` is its first frame in the file.
fn measure_decoded(span: &Span, stamp: Stamp, start: u64, abort: &dyn Fn() -> bool) -> Result<Level> {
    let pcm = match span.secs {
        Some((a, b)) => crate::decode::decode_span(&span.path, a, b)?,
        None => crate::decode::decode(&span.path)?,
    };
    let frames = (start, start + pcm.frames() as u64);
    let pcm = match span.channels {
        Some(pick) => pcm.pick(pick),
        None => pcm,
    };
    if abort() {
        anyhow::bail!("superseded");
    }
    let loudness = measure_with(&crate::export::shape(pcm, span.reverse, span.fades, span.fold, span.rate)?, abort)?;
    Ok(Level { span: span.clone(), stamp, frames, loudness })
}

fn momentary_max(pcm: &Pcm) -> Result<f64> {
    let channels = pcm.channels as u32;
    let mut meter = EbuR128::new(channels, pcm.sample_rate, Mode::M)?;
    let step = (pcm.sample_rate as usize / 10) * pcm.channels as usize;
    let pad = vec![0.0f32; (pcm.sample_rate as usize) * pcm.channels as usize];
    let mut best = f64::NEG_INFINITY;
    for chunk in pcm.samples.chunks(step.max(1)).chain(pad.chunks(step.max(1))) {
        meter.add_frames_f32(chunk)?;
        if let Ok(m) = meter.loudness_momentary() {
            if m.is_finite() {
                best = best.max(m);
            }
        }
    }
    Ok(if best.is_finite() { best } else { -70.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A span metered from the file, through a seek that lands early and packets the span's
    /// edges fall inside, reads the same as the span cut from the decoded file.
    #[test]
    fn streamed_span_reads_as_the_cut_span() {
        let rate = 48_000usize;
        let samples: Vec<f32> = (0..rate * 3 * 2)
            .map(|i| {
                let f = i / 2;
                let amp = if (rate..rate * 2).contains(&f) { 0.1 } else { 0.8 };
                amp * ((f as f32 / rate as f32) * 440.0 * std::f32::consts::TAU).sin()
            })
            .collect();
        let pcm = Pcm { sample_rate: rate as u32, channels: 2, samples };
        let path = std::env::temp_dir().join(format!("hark-span-{}.wav", std::process::id()));
        crate::wav::write(&path, &pcm, 24, false, b"{}", &Default::default()).unwrap();
        let span = (1.0 + 1.0 / 3.0, 1.0 + 6.0 / 7.0);
        let fades = Fades { in_secs: 0.2, out_secs: 0.3, shape: crate::pcm::FadeShape::Linear };
        let at = |fades| Span { path: path.clone(), secs: Some(span), channels: None, reverse: false, fades, fold: None, rate: None };
        let streamed = measure_file(&at(Fades::default()), &|| false).map(|l| l.loudness);
        let faded = measure_file(&at(fades), &|| false).map(|l| l.loudness);
        let _ = std::fs::remove_file(&path);
        let (streamed, faded) = (streamed.unwrap(), faded.unwrap());
        let mut cut = pcm.slice_frames(pcm.frame_at(span.0), pcm.frame_at(span.1));
        let plain = measure(&cut).unwrap();
        assert!((streamed.integrated_lufs - plain.integrated_lufs).abs() < 0.05, "{} vs {} LUFS", streamed.integrated_lufs, plain.integrated_lufs);
        assert!((streamed.sample_peak_dbfs - plain.sample_peak_dbfs).abs() < 0.05, "{} vs {} dBFS", streamed.sample_peak_dbfs, plain.sample_peak_dbfs);
        let whole = measure(&pcm).unwrap();
        assert!(whole.integrated_lufs - streamed.integrated_lufs > 6.0, "the quiet second must read well under the file");
        fades.apply(&mut cut);
        let cut = measure(&cut).unwrap();
        assert!((faded.integrated_lufs - cut.integrated_lufs).abs() < 0.05, "faded: {} vs {} LUFS", faded.integrated_lufs, cut.integrated_lufs);
        assert!(plain.integrated_lufs - faded.integrated_lufs > 0.5, "the fades must take something off the level");
    }

    /// A silent span long enough to gate reads as `measure` reads silence, from the stream.
    #[test]
    fn silent_span_reads_as_silence() {
        let pcm = Pcm { sample_rate: 48_000, channels: 2, samples: vec![0.0; 48_000 * 2 * 3] };
        let path = std::env::temp_dir().join(format!("hark-silent-{}.wav", std::process::id()));
        crate::wav::write(&path, &pcm, 24, false, b"{}", &Default::default()).unwrap();
        let streamed = measure_file(&Span { path: path.clone(), secs: Some((0.5, 2.5)), channels: None, reverse: false, fades: Fades::default(), fold: None, rate: None }, &|| false).map(|l| l.loudness);
        let _ = std::fs::remove_file(&path);
        let (streamed, whole) = (streamed.unwrap(), measure(&pcm).unwrap());
        assert_eq!((streamed.integrated_lufs, streamed.momentary), (whole.integrated_lufs, whole.momentary));
    }
}

use std::path::Path;

use anyhow::Result;
use ebur128::{EbuR128, Mode};
use serde::{Deserialize, Serialize};

use crate::pcm::{gain_to_db, DownmixMode, Fades, Pcm};

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

/// Measures `span` of a file in seconds, or the whole of it, one packet at a time, under
/// `fades` and folded to one channel when `fold` is set, as an export of it is before it is
/// normalised; only a clip too short to gate is decoded whole. Gives up as soon as `abort`
/// says so.
pub fn measure_file(path: &Path, span: Option<(f64, f64)>, fades: Fades, fold: Option<DownmixMode>, abort: &dyn Fn() -> bool) -> Result<Loudness> {
    let mut reader = crate::decode::Reader::open(path)?;
    let channels = reader.channels as usize;
    let rate = reader.sample_rate as f64;
    let (start, end) = match span {
        Some((a, b)) if rate > 0.0 => ((a.max(0.0) * rate).round() as u64, (b.max(0.0) * rate).round() as u64),
        _ => (0, u64::MAX),
    };
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
    let mut chunk = Vec::new();
    let mut shaped = Vec::new();
    let mut meter = None;
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
        let part = &chunk[from * channels..to * channels];
        let first = (pos + from as u64).saturating_sub(start) as usize;
        let gain = |k: usize| if fades.is_zero() { 1.0 } else { fades.gain(first + k, total, reader.sample_rate) };
        let samples = match fold {
            Some(mode) => {
                shaped.clear();
                shaped.extend(part.chunks(channels).enumerate().map(|(k, f)| mode.fold(f) * gain(k)));
                &shaped
            }
            None if fades.is_zero() => part,
            None => {
                shaped.clear();
                shaped.extend(part.chunks(channels).enumerate().flat_map(|(k, f)| f.iter().map(move |s| s * gain(k))));
                &shaped
            }
        };
        let m = match &mut meter {
            Some(m) => m,
            None => meter.insert(Meter::new(if fold.is_some() { 1 } else { reader.channels }, reader.sample_rate)?),
        };
        m.push(samples)?;
        pushed += (to - from) as u64;
        chunk.clear();
        if abort() {
            anyhow::bail!("superseded");
        }
    }
    match meter.as_ref().and_then(|m| m.finish()) {
        Some(l) => Ok(l),
        // Long enough to gate and nothing above the gate: silence, read as `measure` reads it,
        // without decoding a span that may be hours long.
        None if pushed as f64 >= GATING_BLOCK_SECS * rate => {
            let (true_peak_dbfs, sample_peak_dbfs) = meter.as_ref().map(Meter::peaks).unwrap_or((f64::NEG_INFINITY, f64::NEG_INFINITY));
            Ok(Loudness { integrated_lufs: -70.0, momentary: true, true_peak_dbfs, sample_peak_dbfs, range_lu: None })
        }
        None => {
            let mut pcm = match span {
                Some((a, b)) => crate::decode::decode_span(path, a, b)?,
                None => crate::decode::decode(path)?,
            };
            fades.apply(&mut pcm);
            measure(&match fold {
                Some(mode) => pcm.to_mono(mode),
                None => pcm,
            })
        }
    }
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
        let streamed = measure_file(&path, Some(span), Fades::default(), None, &|| false);
        let faded = measure_file(&path, Some(span), fades, None, &|| false);
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
        let streamed = measure_file(&path, Some((0.5, 2.5)), Fades::default(), None, &|| false);
        let _ = std::fs::remove_file(&path);
        let (streamed, whole) = (streamed.unwrap(), measure(&pcm).unwrap());
        assert_eq!((streamed.integrated_lufs, streamed.momentary), (whole.integrated_lufs, whole.momentary));
    }
}

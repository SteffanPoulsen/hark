use anyhow::Result;
use ebur128::{EbuR128, Mode};
use serde::{Deserialize, Serialize};

use crate::pcm::{gain_to_db, Pcm};

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

pub fn measure(pcm: &Pcm) -> Result<Loudness> {
    let channels = pcm.channels as u32;
    let mut meter = EbuR128::new(channels, pcm.sample_rate, Mode::I | Mode::M | Mode::LRA | Mode::TRUE_PEAK | Mode::SAMPLE_PEAK)?;
    meter.add_frames_f32(&pcm.samples)?;
    let peaks = |f: &dyn Fn(u32) -> Result<f64, ebur128::Error>| -> f64 {
        (0..channels).filter_map(|c| f(c).ok()).fold(0.0, f64::max)
    };
    let true_peak_dbfs = gain_to_db(peaks(&|c| meter.true_peak(c)));
    let sample_peak_dbfs = gain_to_db(peaks(&|c| meter.sample_peak(c)));
    let integrated = meter.loudness_global().ok().filter(|v| v.is_finite());
    let (integrated_lufs, momentary) = match integrated {
        Some(v) => (v, false),
        None => (momentary_max(pcm)?, true),
    };
    let range_lu = meter.loudness_range().ok().filter(|v| v.is_finite() && !momentary);
    Ok(Loudness { integrated_lufs, momentary, true_peak_dbfs, sample_peak_dbfs, range_lu })
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

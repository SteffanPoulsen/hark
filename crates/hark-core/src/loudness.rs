use std::path::Path;

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
    let mut meter = Meter::new(pcm.channels, pcm.sample_rate)?;
    meter.push(&pcm.samples)?;
    match meter.finish() {
        Some(l) => Ok(l),
        None => {
            let (true_peak_dbfs, sample_peak_dbfs) = meter.peaks();
            Ok(Loudness { integrated_lufs: momentary_max(pcm)?, momentary: true, true_peak_dbfs, sample_peak_dbfs, range_lu: None })
        }
    }
}

/// Measures a file one packet at a time; only a clip too short to gate is decoded whole.
pub fn measure_file(path: &Path) -> Result<Loudness> {
    let mut reader = crate::decode::Reader::open(path)?;
    let mut chunk = Vec::new();
    let mut meter = None;
    while reader.next_chunk(&mut chunk)? {
        if meter.is_none() {
            meter = Some(Meter::new(reader.channels, reader.sample_rate)?);
        }
        meter.as_mut().unwrap().push(&chunk)?;
        chunk.clear();
    }
    match meter.and_then(|m| m.finish()) {
        Some(l) => Ok(l),
        None => measure(&crate::decode::decode(path)?),
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

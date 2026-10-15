use std::io::{Read, Write};
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::pcm::Pcm;

const MAGIC: &[u8; 4] = b"HKP2";

/// Per-bin envelope of a file: min, max and RMS over a fixed number of frames, per channel.
#[derive(Clone, Debug)]
pub struct Peaks {
    pub sample_rate: u32,
    pub channels: u16,
    pub bin_frames: u32,
    pub total_frames: u64,
    /// `[channel][bin]`
    pub bins: Vec<Vec<Bin>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bin {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

impl Peaks {
    pub const DEFAULT_BIN: u32 = 256;

    pub fn from_pcm(pcm: &Pcm, bin_frames: u32) -> Peaks {
        let mut b = PeaksBuilder::new(pcm.sample_rate, pcm.channels, bin_frames);
        b.push(&pcm.samples);
        b.finish()
    }

    pub fn duration_secs(&self) -> f64 {
        self.total_frames as f64 / self.sample_rate as f64
    }

    /// Envelope of `[start_secs, end_secs)` reduced to `columns` columns for one channel.
    pub fn columns(&self, channel: usize, start_secs: f64, end_secs: f64, columns: usize) -> Vec<Bin> {
        let bins = &self.bins[channel.min(self.bins.len().saturating_sub(1))];
        let bin_secs = self.bin_frames as f64 / self.sample_rate as f64;
        let mut out = Vec::with_capacity(columns);
        let span = (end_secs - start_secs).max(1e-9);
        for i in 0..columns {
            let t0 = start_secs + span * i as f64 / columns as f64;
            let t1 = start_secs + span * (i + 1) as f64 / columns as f64;
            let b0 = (t0 / bin_secs).floor().max(0.0) as usize;
            let b1 = ((t1 / bin_secs).ceil() as usize).min(bins.len()).max(b0 + 1);
            let mut acc = Bin::default();
            if b0 < bins.len() {
                acc.min = f32::MAX;
                acc.max = f32::MIN;
                let slice = &bins[b0..b1];
                let mut sq = 0.0f32;
                for b in slice {
                    acc.min = acc.min.min(b.min);
                    acc.max = acc.max.max(b.max);
                    sq += b.rms * b.rms;
                }
                acc.rms = (sq / slice.len() as f32).sqrt();
            }
            out.push(acc);
        }
        out
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        w.write_all(MAGIC)?;
        w.write_all(&self.sample_rate.to_le_bytes())?;
        w.write_all(&self.channels.to_le_bytes())?;
        w.write_all(&self.bin_frames.to_le_bytes())?;
        w.write_all(&self.total_frames.to_le_bytes())?;
        let n = self.bins.first().map(|b| b.len()).unwrap_or(0) as u64;
        w.write_all(&n.to_le_bytes())?;
        for ch in &self.bins {
            for b in ch {
                w.write_all(&b.min.to_le_bytes())?;
                w.write_all(&b.max.to_le_bytes())?;
                w.write_all(&b.rms.to_le_bytes())?;
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Peaks> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(anyhow!("not a peaks file"));
        }
        let mut b4 = [0u8; 4];
        let mut b2 = [0u8; 2];
        let mut b8 = [0u8; 8];
        r.read_exact(&mut b4)?;
        let sample_rate = u32::from_le_bytes(b4);
        r.read_exact(&mut b2)?;
        let channels = u16::from_le_bytes(b2);
        r.read_exact(&mut b4)?;
        let bin_frames = u32::from_le_bytes(b4);
        r.read_exact(&mut b8)?;
        let total_frames = u64::from_le_bytes(b8);
        r.read_exact(&mut b8)?;
        let n = u64::from_le_bytes(b8) as usize;
        let mut bins = Vec::with_capacity(channels as usize);
        let mut f = || -> Result<f32> {
            let mut b = [0u8; 4];
            r.read_exact(&mut b)?;
            Ok(f32::from_le_bytes(b))
        };
        for _ in 0..channels {
            let mut ch = Vec::with_capacity(n);
            for _ in 0..n {
                ch.push(Bin { min: f()?, max: f()?, rms: f()? });
            }
            bins.push(ch);
        }
        Ok(Peaks { sample_rate, channels, bin_frames, total_frames, bins })
    }
}

/// Accumulates interleaved audio in any chunking into the same bins `Peaks::from_pcm` makes.
pub struct PeaksBuilder {
    sample_rate: u32,
    channels: u16,
    bin_frames: u32,
    bins: Vec<Vec<Bin>>,
    /// Per channel: running min, max and sum of squares of the open bin.
    open: Vec<(f32, f32, f32)>,
    open_frames: u32,
    total_frames: u64,
}

impl PeaksBuilder {
    pub fn new(sample_rate: u32, channels: u16, bin_frames: u32) -> PeaksBuilder {
        let c = channels as usize;
        PeaksBuilder {
            sample_rate,
            channels,
            bin_frames: bin_frames.max(1),
            bins: vec![Vec::new(); c],
            open: vec![(f32::MAX, f32::MIN, 0.0); c],
            open_frames: 0,
            total_frames: 0,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        let c = self.channels as usize;
        if c == 0 {
            return;
        }
        for frame in samples.chunks_exact(c) {
            for (acc, s) in self.open.iter_mut().zip(frame) {
                acc.0 = acc.0.min(*s);
                acc.1 = acc.1.max(*s);
                acc.2 += s * s;
            }
            self.open_frames += 1;
            if self.open_frames == self.bin_frames {
                self.close_bin();
            }
        }
        self.total_frames += (samples.len() / c) as u64;
    }

    fn close_bin(&mut self) {
        let n = self.open_frames.max(1) as f32;
        for (bins, acc) in self.bins.iter_mut().zip(&mut self.open) {
            bins.push(Bin { min: acc.0, max: acc.1, rms: (acc.2 / n).sqrt() });
            *acc = (f32::MAX, f32::MIN, 0.0);
        }
        self.open_frames = 0;
    }

    pub fn finish(mut self) -> Peaks {
        if self.open_frames > 0 {
            self.close_bin();
        }
        Peaks { sample_rate: self.sample_rate, channels: self.channels, bin_frames: self.bin_frames, total_frames: self.total_frames, bins: self.bins }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(pcm: &Pcm, bin_frames: u32) -> Peaks {
        let c = pcm.channels as usize;
        let mut bins = vec![Vec::new(); c];
        for chunk in pcm.samples.chunks(bin_frames as usize * c) {
            let n = (chunk.len() / c).max(1) as f32;
            for ch in 0..c {
                let (mut lo, mut hi, mut sq) = (f32::MAX, f32::MIN, 0.0f32);
                for frame in chunk.chunks(c) {
                    let s = frame[ch];
                    lo = lo.min(s);
                    hi = hi.max(s);
                    sq += s * s;
                }
                bins[ch].push(Bin { min: lo, max: hi, rms: (sq / n).sqrt() });
            }
        }
        Peaks { sample_rate: pcm.sample_rate, channels: pcm.channels, bin_frames, total_frames: pcm.frames() as u64, bins }
    }

    #[test]
    fn chunked_bins_match_whole() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0
        };
        let pcm = Pcm { sample_rate: 48_000, channels: 2, samples: (0..2 * 10_007).map(|_| next()).collect() };
        let want = reference(&pcm, 256);
        let mut b = PeaksBuilder::new(48_000, 2, 256);
        let mut off = 0;
        let mut step = 2;
        while off < pcm.samples.len() {
            let end = (off + step * 2).min(pcm.samples.len());
            b.push(&pcm.samples[off..end]);
            off = end;
            step = step * 3 % 1_000 + 1;
        }
        let got = b.finish();
        assert_eq!(got.total_frames, want.total_frames);
        assert_eq!(got.bins, want.bins);
    }
}

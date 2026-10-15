use std::io::{Read, Write};
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::pcm::Pcm;

const MAGIC: &[u8; 4] = b"HKPK";

/// Min/max envelope of a file at a fixed number of frames per bin, per channel.
#[derive(Clone, Debug)]
pub struct Peaks {
    pub sample_rate: u32,
    pub channels: u16,
    pub bin_frames: u32,
    pub total_frames: u64,
    /// `[channel][bin]` pairs of (min, max).
    pub bins: Vec<Vec<(f32, f32)>>,
}

impl Peaks {
    pub const DEFAULT_BIN: u32 = 256;

    pub fn from_pcm(pcm: &Pcm, bin_frames: u32) -> Peaks {
        let c = pcm.channels as usize;
        let frames = pcm.frames();
        let nbins = frames.div_ceil(bin_frames as usize);
        let mut bins = vec![Vec::with_capacity(nbins); c];
        for (b, chunk) in pcm.samples.chunks(bin_frames as usize * c).enumerate() {
            let _ = b;
            for ch in 0..c {
                let mut lo = f32::MAX;
                let mut hi = f32::MIN;
                for frame in chunk.chunks(c) {
                    let s = frame[ch];
                    lo = lo.min(s);
                    hi = hi.max(s);
                }
                bins[ch].push((lo, hi));
            }
        }
        Peaks {
            sample_rate: pcm.sample_rate,
            channels: pcm.channels,
            bin_frames,
            total_frames: frames as u64,
            bins,
        }
    }

    pub fn duration_secs(&self) -> f64 {
        self.total_frames as f64 / self.sample_rate as f64
    }

    /// Envelope of `[start_secs, end_secs)` reduced to `columns` columns for one channel.
    pub fn columns(&self, channel: usize, start_secs: f64, end_secs: f64, columns: usize) -> Vec<(f32, f32)> {
        let bins = &self.bins[channel.min(self.bins.len().saturating_sub(1))];
        let bin_secs = self.bin_frames as f64 / self.sample_rate as f64;
        let mut out = Vec::with_capacity(columns);
        let span = (end_secs - start_secs).max(1e-9);
        for i in 0..columns {
            let t0 = start_secs + span * i as f64 / columns as f64;
            let t1 = start_secs + span * (i + 1) as f64 / columns as f64;
            let b0 = (t0 / bin_secs).floor().max(0.0) as usize;
            let b1 = ((t1 / bin_secs).ceil() as usize).min(bins.len()).max(b0 + 1);
            let mut lo = 0.0f32;
            let mut hi = 0.0f32;
            if b0 < bins.len() {
                lo = f32::MAX;
                hi = f32::MIN;
                for &(a, b) in &bins[b0..b1] {
                    lo = lo.min(a);
                    hi = hi.max(b);
                }
            }
            out.push((lo, hi));
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
            for &(lo, hi) in ch {
                w.write_all(&lo.to_le_bytes())?;
                w.write_all(&hi.to_le_bytes())?;
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
        for _ in 0..channels {
            let mut ch = Vec::with_capacity(n);
            for _ in 0..n {
                r.read_exact(&mut b4)?;
                let lo = f32::from_le_bytes(b4);
                r.read_exact(&mut b4)?;
                let hi = f32::from_le_bytes(b4);
                ch.push((lo, hi));
            }
            bins.push(ch);
        }
        Ok(Peaks { sample_rate, channels, bin_frames, total_frames, bins })
    }
}

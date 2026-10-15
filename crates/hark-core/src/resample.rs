use anyhow::Result;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::pcm::Pcm;

pub fn resample(pcm: &Pcm, to_rate: u32) -> Result<Pcm> {
    if pcm.sample_rate == to_rate || pcm.frames() == 0 {
        let mut out = pcm.clone();
        out.sample_rate = to_rate;
        return Ok(out);
    }
    let channels = pcm.channels as usize;
    let mut resampler = Fft::<f32>::new(pcm.sample_rate as usize, to_rate as usize, 1024, channels, FixedSync::Both)?;
    let frames = pcm.frames();
    let input = InterleavedSlice::new(&pcm.samples, channels, frames)?;
    let out = resampler.process_all(&input, frames, None)?;
    Ok(Pcm { sample_rate: to_rate, channels: pcm.channels, samples: out.take_data() })
}

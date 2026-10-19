use anyhow::Result;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

use crate::pcm::Pcm;

const CHUNK: usize = 1024;

fn resampler(from: u32, to: u32, channels: usize) -> Result<Fft<f32>> {
    Ok(Fft::<f32>::new(from as usize, to as usize, CHUNK, channels, FixedSync::Both)?)
}

pub fn resample(pcm: &Pcm, to_rate: u32) -> Result<Pcm> {
    if pcm.sample_rate == to_rate || pcm.frames() == 0 {
        let mut out = pcm.clone();
        out.sample_rate = to_rate;
        return Ok(out);
    }
    let channels = pcm.channels as usize;
    let mut resampler = resampler(pcm.sample_rate, to_rate, channels)?;
    let frames = pcm.frames();
    let input = InterleavedSlice::new(&pcm.samples, channels, frames)?;
    let out = resampler.process_all(&input, frames, None)?;
    Ok(Pcm { sample_rate: to_rate, channels: pcm.channels, samples: out.take_data() })
}

/// `resample` for audio that arrives in pieces of any size: the same resampler fed the same
/// chunks, its last chunk partial and its startup delay trimmed, so the samples that come out
/// are the ones `resample` makes of the whole.
pub struct Stream {
    resampler: Option<Fft<f32>>,
    channels: usize,
    /// Input not yet resampled. A full chunk is held back until more follows it, since the
    /// last chunk of the whole goes through as a partial one.
    pending: Vec<f32>,
    scratch: Vec<f32>,
    /// Samples pushed, not frames: a piece may end inside a frame.
    taken: usize,
    made: usize,
}

impl Stream {
    pub fn new(from_rate: u32, to_rate: u32, channels: u16) -> Result<Stream> {
        let channels = channels.max(1) as usize;
        let resampler = if from_rate == to_rate { None } else { Some(resampler(from_rate, to_rate, channels)?) };
        let scratch = vec![0.0; resampler.as_ref().map(|r| r.output_frames_max() * channels).unwrap_or(0)];
        Ok(Stream { resampler, channels, pending: Vec::new(), scratch, taken: 0, made: 0 })
    }

    pub fn push(&mut self, samples: &[f32], out: &mut dyn FnMut(&[f32]) -> Result<()>) -> Result<()> {
        let Some(r) = &self.resampler else { return out(samples) };
        let next = r.input_frames_next();
        self.taken += samples.len();
        self.pending.extend_from_slice(samples);
        while self.pending.len() / self.channels > next {
            let used = self.run(None, out)?;
            self.pending.drain(..used * self.channels);
        }
        Ok(())
    }

    pub fn finish(mut self, out: &mut dyn FnMut(&[f32]) -> Result<()>) -> Result<()> {
        let Some(r) = &self.resampler else { return Ok(()) };
        let taken = self.taken / self.channels;
        if taken == 0 {
            return Ok(());
        }
        let wanted = (r.resample_ratio() * taken as f64).ceil() as usize + r.output_delay();
        let last = self.pending.len() / self.channels;
        self.run(Some(last), out)?;
        self.pending.clear();
        while self.made < wanted {
            self.run(Some(0), out)?;
        }
        Ok(())
    }

    /// Resamples one chunk of `pending`, or its partial last one, and passes on what comes out
    /// past the startup delay and short of the whole's length. Returns the frames it took.
    fn run(&mut self, partial: Option<usize>, out: &mut dyn FnMut(&[f32]) -> Result<()>) -> Result<usize> {
        let r = self.resampler.as_mut().expect("only a resampling stream runs chunks");
        let ch = self.channels;
        let input = InterleavedSlice::new(&self.pending, ch, self.pending.len() / ch)?;
        let room = self.scratch.len() / ch;
        let mut output = InterleavedSlice::new_mut(&mut self.scratch, ch, room)?;
        let indexing = Indexing { input_offset: 0, output_offset: 0, active_channels_mask: None, partial_len: partial };
        let (used, made) = r.process_into_buffer(&input, &mut output, Some(&indexing))?;
        let delay = r.output_delay();
        let whole = (r.resample_ratio() * (self.taken / ch) as f64).ceil() as usize;
        let from = delay.saturating_sub(self.made).min(made);
        self.made += made;
        // Before the last chunk the whole's length is not known yet, but nothing made so far
        // can reach it.
        let to = if partial.is_some() { made.min((whole + delay).saturating_sub(self.made - made)) } else { made };
        if from < to {
            out(&self.scratch[from * ch..to * ch])?;
        }
        Ok(used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audio pushed in uneven pieces, some ending inside a frame, comes out as the same samples
    /// `resample` makes of the whole, for lengths around the chunk size and both directions of
    /// rate change.
    #[test]
    fn stream_matches_whole() {
        for (from, to, channels) in [(44_100, 48_000, 2u16), (96_000, 48_000, 1), (48_000, 44_100, 2)] {
            let probe = Stream::new(from, to, channels).unwrap();
            let next = probe.resampler.as_ref().unwrap().input_frames_next();
            for frames in [1, 300, next - 1, next, next + 1, 2 * next, 3 * next + 17, from as usize + 5] {
                let c = channels as usize;
                let samples: Vec<f32> = (0..frames * c).map(|i| ((i * 7919) % 2003) as f32 / 1001.5 - 1.0).collect();
                let pcm = Pcm { sample_rate: from, channels, samples };
                let whole = resample(&pcm, to).unwrap().samples;
                let mut streamed = Vec::new();
                let mut stream = Stream::new(from, to, channels).unwrap();
                let mut take = |s: &[f32]| {
                    streamed.extend_from_slice(s);
                    Ok(())
                };
                for piece in pcm.samples.chunks(333) {
                    stream.push(piece, &mut take).unwrap();
                }
                stream.finish(&mut take).unwrap();
                assert_eq!(streamed.len(), whole.len(), "{from}→{to} {channels}ch {frames} frames: length");
                assert!(streamed == whole, "{from}→{to} {channels}ch {frames} frames: samples differ");
            }
        }
    }
}

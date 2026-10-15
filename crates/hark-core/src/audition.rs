//! One always-open output stream. Short clips are handed over already decoded at the device
//! rate and start on the next callback, so retriggering a hundred one-shots in a row costs
//! nothing. Long files stream: a decoder thread fills a lock-free ring with device-rate frames a
//! few seconds ahead of the callback, which only ever reads from it. Every start, stop and
//! retrigger is declicked with a short ramp, and a clip's own fades play back so the selection
//! sounds the way it will export.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

use crate::decode::Reader;
use crate::pcm::{FadeShape, Pcm};

const DECLICK_SECS: f64 = 0.005;
/// How far ahead of the callback a stream is decoded; also the ring's capacity.
const STREAM_AHEAD_SECS: f64 = 4.0;
/// How much of a stream is decoded before it starts playing.
const STREAM_PRIME_SECS: f64 = 2.0;
const STREAM_POLL: Duration = Duration::from_millis(5);
const RESAMPLE_CHUNK: usize = 1024;
/// Source frames decoded on each side of a resampled span as filter context.
const RESAMPLE_MARGIN: usize = 2048;

#[derive(Clone)]
pub enum Source {
    /// Decoded at the device rate, ready to play.
    Whole(Arc<Pcm>),
    /// Decoded on a thread while it plays.
    File { path: PathBuf, source_rate: u32, channels: u16, source_frames: u64 },
}

impl Source {
    pub fn channels(&self) -> u16 {
        match self {
            Source::Whole(p) => p.channels,
            Source::File { channels, .. } => *channels,
        }
    }

    /// Length in frames at `device_rate`.
    pub fn frames(&self, device_rate: u32) -> usize {
        match self {
            Source::Whole(p) => p.frames(),
            Source::File { source_rate, source_frames, .. } => {
                if *source_rate == 0 {
                    0
                } else {
                    (*source_frames as f64 * device_rate as f64 / *source_rate as f64).round() as usize
                }
            }
        }
    }

    pub fn is_streamed(&self) -> bool {
        matches!(self, Source::File { .. })
    }
}

/// Positions are frames at the device rate.
#[derive(Clone)]
pub struct Clip {
    pub source: Source,
    pub start: usize,
    pub end: usize,
    pub looped: bool,
    /// Fade lengths in frames, measured from `start` and back from `end`.
    pub fade_in: usize,
    pub fade_out: usize,
    pub fade_shape: FadeShape,
}

impl Clip {
    pub fn whole(pcm: Arc<Pcm>) -> Clip {
        let end = pcm.frames();
        Clip { source: Source::Whole(pcm), start: 0, end, looped: false, fade_in: 0, fade_out: 0, fade_shape: FadeShape::EqualPower }
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    start: usize,
    end: usize,
    looped: bool,
    fade_in: usize,
    fade_out: usize,
    fade_shape: FadeShape,
}

impl Bounds {
    fn of(clip: &Clip) -> Bounds {
        Bounds { start: clip.start, end: clip.end, looped: clip.looped, fade_in: clip.fade_in, fade_out: clip.fade_out, fade_shape: clip.fade_shape }
    }

    fn envelope(&self, pos: usize) -> f32 {
        let mut g = 1.0;
        if self.fade_in > 0 && pos < self.start + self.fade_in {
            g *= self.fade_shape.gain((pos - self.start) as f32 / self.fade_in as f32);
        }
        if self.fade_out > 0 && pos + self.fade_out > self.end {
            g *= self.fade_shape.gain((self.end - pos) as f32 / self.fade_out as f32);
        }
        g
    }
}

/// Single-producer single-consumer ring of interleaved frames. Samples are stored as bits in
/// atomics so neither side needs `unsafe`; the counters are totals, so full and empty never
/// look alike.
struct Ring {
    buf: Box<[AtomicU32]>,
    channels: usize,
    frames: usize,
    written: AtomicUsize,
    read: AtomicUsize,
}

impl Ring {
    fn new(channels: usize, frames: usize) -> Ring {
        let buf = (0..channels * frames).map(|_| AtomicU32::new(0)).collect();
        Ring { buf, channels, frames, written: AtomicUsize::new(0), read: AtomicUsize::new(0) }
    }

    /// Producer side: appends as many whole frames as fit and returns how many.
    fn push(&self, samples: &[f32]) -> usize {
        let written = self.written.load(Ordering::Relaxed);
        let free = self.frames - (written - self.read.load(Ordering::Acquire));
        let n = (samples.len() / self.channels).min(free);
        for i in 0..n {
            let slot = ((written + i) % self.frames) * self.channels;
            for c in 0..self.channels {
                self.buf[slot + c].store(samples[i * self.channels + c].to_bits(), Ordering::Relaxed);
            }
        }
        self.written.store(written + n, Ordering::Release);
        n
    }

    /// Consumer side: frames readable right now.
    fn readable(&self) -> usize {
        self.written.load(Ordering::Acquire) - self.read.load(Ordering::Relaxed)
    }

    fn sample(&self, frame: usize, channel: usize) -> f32 {
        f32::from_bits(self.buf[(frame % self.frames) * self.channels + channel].load(Ordering::Relaxed))
    }

    fn advance(&self, frames: usize) {
        self.read.store(self.read.load(Ordering::Relaxed) + frames, Ordering::Release);
    }
}

struct Stream {
    ring: Ring,
    /// Enough is decoded to start consuming.
    ready: AtomicBool,
    /// The decoder has pushed its last frame, whether it finished or failed.
    ended: AtomicBool,
}

enum Feed {
    Whole(Arc<Pcm>),
    Stream(Arc<Stream>),
}

/// The ring frames a callback may take, sampled once at its start.
struct Take {
    first: usize,
    avail: usize,
    taken: usize,
}

impl Feed {
    fn begin(&self) -> Take {
        match self {
            Feed::Whole(_) => Take { first: 0, avail: usize::MAX, taken: 0 },
            Feed::Stream(s) => Take { first: s.ring.read.load(Ordering::Relaxed), avail: s.ring.readable(), taken: 0 },
        }
    }

    /// Mixes the frame at `pos` (whole) or the next ring frame (stream) into `out` at gain
    /// `g`; `false` when the stream has nothing for it yet.
    fn mix_frame(&self, pos: usize, take: &mut Take, g: f32, out: &mut [f32]) -> bool {
        match self {
            Feed::Whole(p) => {
                let ic = p.channels as usize;
                let src = &p.samples[pos * ic..(pos + 1) * ic];
                for (i, o) in out.iter_mut().enumerate() {
                    *o += src[i.min(ic - 1)] * g;
                }
            }
            Feed::Stream(s) => {
                if !s.ready.load(Ordering::Acquire) || take.taken >= take.avail {
                    return false;
                }
                let ic = s.ring.channels;
                let frame = take.first + take.taken;
                for (i, o) in out.iter_mut().enumerate() {
                    *o += s.ring.sample(frame, i.min(ic - 1)) * g;
                }
                take.taken += 1;
            }
        }
        true
    }

    /// The stream has delivered everything it ever will and it has all been played.
    fn exhausted(&self, take: &Take) -> bool {
        match self {
            Feed::Whole(_) => false,
            Feed::Stream(s) => s.ended.load(Ordering::Acquire) && s.ring.written.load(Ordering::Acquire) == take.first + take.taken,
        }
    }

    fn ready(&self) -> bool {
        match self {
            Feed::Whole(_) => true,
            Feed::Stream(s) => s.ready.load(Ordering::Acquire),
        }
    }

    fn commit(&self, take: &Take) {
        if let Feed::Stream(s) = self {
            s.ring.advance(take.taken);
        }
    }
}

enum Cmd {
    /// A voice and the frame to start from.
    Play(Feed, Bounds, usize),
    Stop,
    Seek(usize),
}

struct Shared {
    position: AtomicUsize,
    playing: AtomicBool,
    gain: AtomicU32,
    /// Frames of silence played because a stream had not been decoded far enough.
    underruns: AtomicUsize,
}

pub struct Player {
    _stream: cpal::Stream,
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    pub sample_rate: u32,
    pub channels: u16,
    pub device_name: String,
}

struct Voice {
    feed: Feed,
    bounds: Bounds,
    pos: usize,
    /// Frames rendered since the voice started, for the declick ramp in.
    age: usize,
}

impl Voice {
    fn sample_gain(&self, declick: usize) -> f32 {
        let mut g = self.bounds.envelope(self.pos);
        if self.age < declick {
            g *= self.age as f32 / declick as f32;
        }
        if !self.bounds.looped && self.pos + declick > self.bounds.end {
            g *= (self.bounds.end - self.pos) as f32 / declick as f32;
        }
        g
    }
}

struct Mixer {
    rx: Receiver<Cmd>,
    shared: Arc<Shared>,
    voice: Option<Voice>,
    /// A voice ramping out after a stop or retrigger, with frames left in the ramp.
    dying: Option<(Voice, usize)>,
    channels: usize,
    declick: usize,
}

impl Mixer {
    fn retire(&mut self) {
        if let Some(v) = self.voice.take() {
            if v.pos < v.bounds.end {
                self.dying = Some((v, self.declick));
            }
        }
    }

    fn fill(&mut self, out: &mut [f32]) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::Play(feed, bounds, from) => {
                    self.retire();
                    let pos = from.clamp(bounds.start, bounds.end);
                    self.voice = Some(Voice { feed, bounds, pos, age: 0 });
                    self.shared.playing.store(true, Ordering::Relaxed);
                }
                Cmd::Stop => {
                    self.retire();
                    self.shared.playing.store(false, Ordering::Relaxed);
                }
                Cmd::Seek(frame) => {
                    if let Some(v) = &mut self.voice {
                        if let Feed::Whole(_) = v.feed {
                            v.pos = frame.clamp(v.bounds.start, v.bounds.end);
                            v.age = 0;
                        }
                    }
                }
            }
        }
        out.fill(0.0);
        let gain = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
        let oc = self.channels;

        if let Some((v, left)) = &mut self.dying {
            let mut take = v.feed.begin();
            let mut starved = false;
            for frame in out.chunks_mut(oc) {
                if *left == 0 || v.pos >= v.bounds.end {
                    break;
                }
                let g = gain * v.bounds.envelope(v.pos) * (*left as f32 / self.declick as f32);
                if !v.feed.mix_frame(v.pos, &mut take, g, frame) {
                    starved = true;
                    break;
                }
                v.pos += 1;
                *left -= 1;
            }
            v.feed.commit(&take);
            if starved || *left == 0 || v.pos >= v.bounds.end {
                self.dying = None;
            }
        }

        let Some(v) = &mut self.voice else { return };
        let mut take = v.feed.begin();
        let mut done = false;
        let frames = out.len() / oc;
        for (n, frame) in out.chunks_mut(oc).enumerate() {
            if v.pos >= v.bounds.end {
                if v.bounds.looped && v.bounds.end > v.bounds.start {
                    v.pos = v.bounds.start;
                } else {
                    done = true;
                    break;
                }
            }
            let g = gain * v.sample_gain(self.declick);
            if !v.feed.mix_frame(v.pos, &mut take, g, frame) {
                if v.feed.exhausted(&take) {
                    done = true;
                } else if v.feed.ready() {
                    self.shared.underruns.fetch_add(frames - n, Ordering::Relaxed);
                }
                break;
            }
            v.pos += 1;
            v.age += 1;
        }
        v.feed.commit(&take);
        self.shared.position.store(v.pos, Ordering::Relaxed);
        if done {
            self.voice = None;
            self.shared.playing.store(false, Ordering::Relaxed);
        }
    }
}

impl Player {
    pub fn open(preferred_rate: Option<u32>) -> Result<Player> {
        let (device, host_name) = pick_device()?;
        let device_name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "output".into());
        let default = device.default_output_config()?;
        let mut config: cpal::StreamConfig = default.clone().into();
        if let Some(rate) = preferred_rate {
            if device.supported_output_configs().map(|mut c| c.any(|r| r.min_sample_rate() <= rate && rate <= r.max_sample_rate())).unwrap_or(false) {
                config.sample_rate = rate;
            }
        }
        let shared = Arc::new(Shared {
            position: AtomicUsize::new(0),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
            underruns: AtomicUsize::new(0),
        });
        let (tx, rx) = channel();
        let mixer = Mixer {
            rx,
            shared: shared.clone(),
            voice: None,
            dying: None,
            channels: config.channels as usize,
            declick: (DECLICK_SECS * config.sample_rate as f64).round().max(1.0) as usize,
        };
        let stream = match default.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, mixer)?,
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, mixer)?,
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, mixer)?,
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, mixer)?,
            f => return Err(anyhow!("unsupported output sample format {f}")),
        };
        stream.play()?;
        Ok(Player {
            _stream: stream,
            tx,
            shared,
            sample_rate: config.sample_rate,
            channels: config.channels,
            device_name: format!("{device_name} ({host_name})"),
        })
    }

    pub fn play(&self, clip: Clip) {
        let from = clip.start;
        self.play_from(clip, from);
    }

    /// Plays `clip` from a frame inside it, with its fades where they would be from `start`.
    /// A streamed clip starts its decoder thread here.
    pub fn play_from(&self, clip: Clip, from: usize) {
        let bounds = Bounds::of(&clip);
        let from = from.clamp(bounds.start, bounds.end);
        let feed = match clip.source {
            Source::Whole(pcm) => Feed::Whole(pcm),
            Source::File { path, source_rate, channels, .. } => Feed::Stream(start_stream(path, source_rate, channels, self.sample_rate, bounds, from)),
        };
        let _ = self.tx.send(Cmd::Play(feed, bounds, from));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    /// Moves a whole clip's playhead. A streamed clip seeks by `play_from`, which restarts its
    /// decoder at the new frame.
    pub fn seek(&self, frame: usize) {
        let _ = self.tx.send(Cmd::Seek(frame));
    }

    pub fn set_gain(&self, gain: f32) {
        self.shared.gain.store(gain.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.shared.gain.load(Ordering::Relaxed))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Frame of the current clip the callback last reached.
    pub fn position(&self) -> usize {
        self.shared.position.load(Ordering::Relaxed)
    }

    /// Frames of silence played so far because a stream fell behind.
    pub fn underrun_frames(&self) -> usize {
        self.shared.underruns.load(Ordering::Relaxed)
    }
}

fn start_stream(path: PathBuf, source_rate: u32, channels: u16, device_rate: u32, bounds: Bounds, from: usize) -> Arc<Stream> {
    let stream = Arc::new(Stream {
        ring: Ring::new(channels.max(1) as usize, (STREAM_AHEAD_SECS * device_rate as f64) as usize),
        ready: AtomicBool::new(false),
        ended: AtomicBool::new(false),
    });
    let shared = stream.clone();
    std::thread::spawn(move || {
        let job = StreamJob { stream: &shared, path: &path, source_rate, device_rate, bounds, from };
        if let Err(e) = job.run() {
            if !job.abandoned() {
                eprintln!("stream {}: {e:#}", path.display());
            }
        }
        shared.ended.store(true, Ordering::Release);
        shared.ready.store(true, Ordering::Release);
        // The mixer only ever drops its reference inside the audio callback; releasing the ring
        // there would free memory on the audio thread, so this thread outlives the mixer's use.
        while Arc::strong_count(&shared) > 1 {
            std::thread::sleep(STREAM_POLL);
        }
    });
    stream
}

struct StreamJob<'a> {
    stream: &'a Arc<Stream>,
    path: &'a Path,
    source_rate: u32,
    device_rate: u32,
    bounds: Bounds,
    from: usize,
}

impl StreamJob<'_> {
    fn abandoned(&self) -> bool {
        Arc::strong_count(self.stream) == 1
    }

    /// Decodes `[from, end)` then, while looping, `[start, end)` again and again, delivering
    /// exactly as many device frames as the bounds span so the mixer's position stays true.
    fn run(&self) -> Result<()> {
        let mut reader = Reader::open(self.path)?;
        let channels = self.stream.ring.channels;
        if reader.channels as usize != channels {
            return Err(anyhow!("expected {channels} channels, file has {}", reader.channels));
        }
        if reader.sample_rate != self.source_rate {
            return Err(anyhow!("expected {} Hz, file has {} Hz", self.source_rate, reader.sample_rate));
        }
        let mut resampler = if self.source_rate != self.device_rate {
            Some(Fft::<f32>::new(self.source_rate as usize, self.device_rate as usize, RESAMPLE_CHUNK, channels, FixedSync::Input)?)
        } else {
            None
        };
        let prime = (STREAM_PRIME_SECS * self.device_rate as f64) as usize;
        let src_of = |dev: usize| (dev as f64 * self.source_rate as f64 / self.device_rate as f64).round() as u64;
        let mut from = self.from;
        let mut chunk = Vec::new();
        let mut pending = Vec::new();
        let mut out = Vec::new();
        loop {
            let src_from = src_of(from);
            let src_end = src_of(self.bounds.end).min(reader.frames.unwrap_or(u64::MAX));
            // The resampler sees audio on both sides of the span so the frames at a loop
            // boundary are not filtered against silence; the margin's output is dropped.
            let lead = if resampler.is_some() { (RESAMPLE_MARGIN as u64).min(src_from) } else { 0 };
            let tail = if resampler.is_some() { RESAMPLE_MARGIN as u64 } else { 0 };
            let mut skip = (src_from - lead).saturating_sub(reader.seek(src_from - lead)?) as usize;
            let mut src_left = (src_end + tail).saturating_sub(src_from - lead) as usize;
            let mut pass = Pass { want: self.bounds.end.saturating_sub(from), delivered: 0, drop: 0 };
            if let Some(r) = &mut resampler {
                r.reset();
                pass.drop = r.output_delay() + (lead as f64 * self.device_rate as f64 / self.source_rate as f64).round() as usize;
            }
            pending.clear();
            while src_left > 0 {
                chunk.clear();
                if !reader.next_chunk(&mut chunk)? {
                    break;
                }
                let frames = chunk.len() / channels;
                let s = skip.min(frames);
                skip -= s;
                let take = (frames - s).min(src_left);
                src_left -= take;
                let samples = &chunk[s * channels..(s + take) * channels];
                match &mut resampler {
                    None => self.deliver(samples, &mut pass, prime)?,
                    Some(r) => {
                        pending.extend_from_slice(samples);
                        self.resample(r, &mut pending, &mut out, &mut pass, prime, false)?;
                    }
                }
            }
            match &mut resampler {
                None => {
                    let pad = vec![0.0; (pass.want - pass.delivered) * channels];
                    self.deliver(&pad, &mut pass, prime)?;
                }
                Some(r) => self.resample(r, &mut pending, &mut out, &mut pass, prime, true)?,
            }
            if !self.bounds.looped || self.bounds.end <= self.bounds.start {
                break;
            }
            from = self.bounds.start;
        }
        Ok(())
    }

    /// Runs whole input chunks through the resampler, delivering their output; with `flush`,
    /// also the partial tail and then silence until the pass is complete.
    fn resample(&self, r: &mut Fft<f32>, pending: &mut Vec<f32>, out: &mut Vec<f32>, pass: &mut Pass, prime: usize, flush: bool) -> Result<()> {
        let channels = self.stream.ring.channels;
        out.resize(r.output_frames_max() * channels, 0.0);
        loop {
            let need = r.input_frames_next();
            let have = pending.len() / channels;
            if have < need && !flush {
                return Ok(());
            }
            if pass.delivered >= pass.want {
                return Ok(());
            }
            let frames = have.min(need);
            let indexing = if frames < need { Some(Indexing::new().partial_len(frames)) } else { None };
            let input = InterleavedSlice::new(&pending[..], channels, frames)?;
            let mut output = InterleavedSlice::new_mut(&mut out[..], channels, r.output_frames_max())?;
            let (consumed, produced) = r.process_into_buffer(&input, &mut output, indexing.as_ref())?;
            pending.drain(..consumed.min(have) * channels);
            let d = pass.drop.min(produced);
            pass.drop -= d;
            self.deliver(&out[d * channels..produced * channels], pass, prime)?;
        }
    }

    /// Pushes frames into the ring up to the pass's budget, waiting for room as the callback
    /// drains it.
    fn deliver(&self, samples: &[f32], pass: &mut Pass, prime: usize) -> Result<()> {
        let channels = self.stream.ring.channels;
        let n = (samples.len() / channels).min(pass.want - pass.delivered);
        let mut off = 0;
        while off < n {
            if self.abandoned() {
                return Err(anyhow!("abandoned"));
            }
            let pushed = self.stream.ring.push(&samples[off * channels..n * channels]);
            off += pushed;
            if pushed == 0 {
                std::thread::sleep(STREAM_POLL);
            }
        }
        pass.delivered += n;
        if !self.stream.ready.load(Ordering::Relaxed) && self.stream.ring.written.load(Ordering::Relaxed) >= prime {
            self.stream.ready.store(true, Ordering::Release);
        }
        Ok(())
    }
}

/// One trip through the bounds: how many device frames it owes, how many it has pushed, and
/// how many leading resampler-delay frames are still to drop.
struct Pass {
    want: usize,
    delivered: usize,
    drop: usize,
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, mut mixer: Mixer) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device.build_output_stream(
        config.clone(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            scratch.resize(data.len(), 0.0);
            mixer.fill(&mut scratch);
            for (o, s) in data.iter_mut().zip(&scratch) {
                *o = T::from_sample(*s);
            }
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Weak;

    const TONE_HZ: f64 = 480.0;
    const AMP: f32 = 0.5;

    fn tone(rate: u32, secs: f64) -> Pcm {
        let n = (rate as f64 * secs) as usize;
        let samples = (0..n).flat_map(|i| {
            let s = AMP * (2.0 * std::f64::consts::PI * TONE_HZ * i as f64 / rate as f64).sin() as f32;
            [s, s]
        });
        Pcm { sample_rate: rate, channels: 2, samples: samples.collect() }
    }

    fn tone_file(rate: u32, name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hark-audition-{}-{name}.wav", std::process::id()));
        crate::wav::write(&path, &tone(rate, 3.0), 32, false, b"{}", &crate::wav::Bext::default()).unwrap();
        path
    }

    struct Bench {
        mixer: Mixer,
        tx: Sender<Cmd>,
        device_rate: u32,
    }

    impl Bench {
        fn new(device_rate: u32) -> Bench {
            let shared = Arc::new(Shared {
                position: AtomicUsize::new(0),
                playing: AtomicBool::new(false),
                gain: AtomicU32::new(1.0f32.to_bits()),
                underruns: AtomicUsize::new(0),
            });
            let (tx, rx) = channel();
            let mixer = Mixer { rx, shared, voice: None, dying: None, channels: 2, declick: 1 };
            Bench { mixer, tx, device_rate }
        }

        /// Starts a stream on `path` and plays it; returns a weak handle for watching the thread.
        fn play(&self, path: &Path, source_rate: u32, bounds: Bounds, from: usize) -> Weak<Stream> {
            let stream = start_stream(path.to_path_buf(), source_rate, 2, self.device_rate, bounds, from);
            let weak = Arc::downgrade(&stream);
            self.tx.send(Cmd::Play(Feed::Stream(stream), bounds, from)).unwrap();
            weak
        }

        /// Renders `frames` frames in device-sized callbacks, paced so the decoder can keep
        /// up as a real device would, and returns the left channel.
        fn render(&mut self, stream: &Weak<Stream>, frames: usize) -> Vec<f32> {
            let mut left = Vec::with_capacity(frames);
            let mut buf = vec![0.0f32; 512 * 2];
            while left.len() < frames {
                if let Some(s) = stream.upgrade() {
                    let t0 = std::time::Instant::now();
                    while (!s.ready.load(Ordering::Acquire) || s.ring.readable() < 512) && !s.ended.load(Ordering::Acquire) && t0.elapsed() < Duration::from_secs(5) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                self.mixer.fill(&mut buf);
                left.extend(buf.chunks(2).map(|f| f[0]));
            }
            left.truncate(frames);
            left
        }
    }

    fn expected(rate: u32, frame: usize) -> f32 {
        AMP * (2.0 * std::f64::consts::PI * TONE_HZ * frame as f64 / rate as f64).sin() as f32
    }

    fn bounds(start: usize, end: usize, looped: bool) -> Bounds {
        Bounds { start, end, looped, fade_in: 0, fade_out: 0, fade_shape: FadeShape::Linear }
    }

    #[test]
    fn looped_selection_is_seamless_and_sample_aligned() {
        let path = tone_file(48_000, "loop");
        let mut b = Bench::new(48_000);
        let (start, end) = (24_000, 48_000);
        let stream = b.play(&path, 48_000, bounds(start, end, true), start);
        let out = b.render(&stream, 48_000 * 4);
        let len = end - start;
        for (k, s) in out.iter().enumerate().skip(1) {
            let want = expected(48_000, start + k % len);
            assert!((s - want).abs() < 1e-3, "frame {k}: got {s}, want {want}");
        }
        assert_eq!(b.mixer.shared.underruns.load(Ordering::Relaxed), 0);
        assert!(b.mixer.shared.playing.load(Ordering::Relaxed));
        let pos = b.mixer.shared.position.load(Ordering::Relaxed);
        assert!(pos >= start && pos <= end);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn play_from_inside_ends_exactly_at_end_and_releases_the_thread() {
        let path = tone_file(48_000, "seek");
        let mut b = Bench::new(48_000);
        let (start, end, from) = (24_000, 48_000, 36_000);
        let stream = b.play(&path, 48_000, bounds(start, end, false), from);
        let out = b.render(&stream, 48_000);
        for k in 1..(end - from) {
            let want = expected(48_000, from + k);
            assert!((out[k] - want).abs() < 1e-3, "frame {k}: got {}, want {want}", out[k]);
        }
        assert!(out[end - from..].iter().all(|s| *s == 0.0));
        assert!(!b.mixer.shared.playing.load(Ordering::Relaxed));
        assert_eq!(b.mixer.shared.position.load(Ordering::Relaxed), end);
        assert_eq!(b.mixer.shared.underruns.load(Ordering::Relaxed), 0);
        let t0 = std::time::Instant::now();
        while stream.strong_count() > 0 && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(stream.strong_count(), 0, "decoder thread still holds the stream");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn resampled_loop_keeps_its_length_and_continuity() {
        let path = tone_file(44_100, "resample");
        let mut b = Bench::new(48_000);
        let (start, end) = (12_000, 36_000);
        let stream = b.play(&path, 44_100, bounds(start, end, true), start);
        let out = b.render(&stream, 48_000 * 3);
        let len = end - start;
        let slope = (2.0 * std::f64::consts::PI * TONE_HZ / 48_000.0) as f32 * AMP * 1.5;
        for k in 200..out.len() {
            assert!((out[k] - out[k - 1]).abs() < slope, "jump at frame {k}: {} -> {}", out[k - 1], out[k]);
            let want = expected(48_000, start + k % len);
            assert!((out[k] - want).abs() < 0.01, "frame {k}: got {}, want {want}", out[k]);
        }
        assert_eq!(b.mixer.shared.underruns.load(Ordering::Relaxed), 0);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn retrigger_releases_the_old_thread() {
        let path = tone_file(48_000, "retrigger");
        let mut b = Bench::new(48_000);
        let first = b.play(&path, 48_000, bounds(0, 144_000, true), 0);
        b.render(&first, 4_096);
        let second = b.play(&path, 48_000, bounds(0, 144_000, true), 48_000);
        b.render(&second, 4_096);
        let t0 = std::time::Instant::now();
        while first.strong_count() > 0 && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(first.strong_count(), 0, "retired stream's thread still alive");
        assert!(second.strong_count() > 0);
        std::fs::remove_file(&path).unwrap();
    }
}

fn pick_device() -> Result<(cpal::Device, &'static str)> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        if let Ok(host) = cpal::host_from_id(cpal::HostId::PipeWire) {
            if let Some(d) = host.default_output_device() {
                return Ok((d, "pipewire"));
            }
        }
    }
    let host = cpal::default_host();
    #[cfg(target_os = "linux")]
    {
        let named = |want: &str| {
            host.output_devices().ok().and_then(|mut it| it.find(|d| d.description().map(|desc| desc.name() == want).unwrap_or(false)))
        };
        if let Some(d) = named("pipewire").or_else(|| named("pulse")) {
            return Ok((d, "alsa"));
        }
    }
    host.default_output_device().map(|d| (d, "default")).ok_or_else(|| anyhow!("no audio output device"))
}

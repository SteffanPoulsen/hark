use serde::{Deserialize, Serialize};

/// Interleaved 32-bit float audio.
#[derive(Clone, Debug, Default)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FadeShape {
    Linear,
    #[default]
    EqualPower,
    Logarithmic,
}

impl std::str::FromStr for FadeShape {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "linear" | "lin" => Ok(FadeShape::Linear),
            "equal_power" | "eq" => Ok(FadeShape::EqualPower),
            "logarithmic" | "log" => Ok(FadeShape::Logarithmic),
            _ => Err(format!("unknown fade shape {s}: linear, equal_power or logarithmic")),
        }
    }
}

impl FadeShape {
    pub fn name(self) -> &'static str {
        match self {
            FadeShape::Linear => "Linear",
            FadeShape::EqualPower => "Equal power",
            FadeShape::Logarithmic => "Logarithmic",
        }
    }

    /// Gain at normalised position `t` in `0..=1` of a fade-in; a fade-out uses `1 - t`.
    pub fn gain(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FadeShape::Linear => t,
            FadeShape::EqualPower => (t * std::f32::consts::FRAC_PI_2).sin(),
            FadeShape::Logarithmic => {
                if t <= 0.0 {
                    0.0
                } else {
                    (1.0 + 9.0 * t).log10()
                }
            }
        }
    }
}

/// How channels fold to one. Which channels there are to fold is the file's channel pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DownmixMode {
    /// Average of all channels. A preset from before channels were picked per file that kept
    /// one side loads as this.
    #[default]
    #[serde(alias = "left", alias = "right")]
    Average,
    /// Sum of all channels at -3 dB per doubling (constant power).
    SumMinus3,
}

impl std::str::FromStr for DownmixMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "average" | "avg" => Ok(DownmixMode::Average),
            "sum" | "sum_minus3" => Ok(DownmixMode::SumMinus3),
            "left" | "right" => Err(format!("{s} is a channel of the file, picked when it is exported: export --channels 1")),
            _ => Err(format!("unknown downmix {s}: average or sum")),
        }
    }
}

impl DownmixMode {
    pub fn name(self) -> &'static str {
        match self {
            DownmixMode::Average => "Mono, average",
            DownmixMode::SumMinus3 => "Mono, sum −3 dB",
        }
    }

    /// One interleaved frame folded to a single sample. The audition mixer calls this per
    /// frame, so it allocates nothing.
    pub fn fold(self, frame: &[f32]) -> f32 {
        let c = frame.len();
        if c == 1 {
            return frame[0];
        }
        match self {
            DownmixMode::Average => frame.iter().sum::<f32>() / c as f32,
            DownmixMode::SumMinus3 => frame.iter().sum::<f32>() / (c as f32).sqrt(),
        }
    }
}

/// The channels of a source that are kept, by index from 0, as one bit each: a choice made
/// per file, ahead of everything else done to it. Only the first 64 channels can be kept.
/// Written as the channel numbers from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "Vec<u16>", try_from = "Vec<u16>")]
pub struct Channels(u64);

impl Channels {
    pub const MAX: usize = 64;

    /// The pick that keeps `kept` of a source's `channels`: `None` when that is all of them
    /// or none, either of which picks nothing.
    pub fn pick(kept: impl IntoIterator<Item = usize>, channels: u16) -> Option<Channels> {
        let n = (channels as usize).min(Self::MAX);
        let all = if n == Self::MAX { u64::MAX } else { (1u64 << n) - 1 };
        let mask = kept.into_iter().filter(|&c| c < n).fold(0u64, |m, c| m | 1 << c);
        (mask != 0 && mask != all).then_some(Channels(mask))
    }

    pub fn contains(self, channel: usize) -> bool {
        channel < Self::MAX && self.0 & (1 << channel) != 0
    }

    pub fn count(self) -> usize {
        self.0.count_ones() as usize
    }

    pub fn iter(self) -> impl Iterator<Item = usize> {
        (0..Self::MAX).filter(move |&c| self.contains(c))
    }

    /// Whether every kept channel is one of `channels`.
    pub fn fits(self, channels: u16) -> bool {
        self.iter().all(|c| c < channels as usize)
    }

    /// Copies the kept channels of one interleaved frame into `out` and returns how many; the
    /// audio callback calls this per frame, so it allocates nothing.
    pub fn take(self, frame: &[f32], out: &mut [f32]) -> usize {
        let mut n = 0;
        for (c, s) in frame.iter().enumerate().take(Self::MAX) {
            if self.contains(c) && n < out.len() {
                out[n] = *s;
                n += 1;
            }
        }
        n
    }

    /// The channel numbers from 1, as a person and the command line count them: `1+3`.
    pub fn label(self) -> String {
        self.iter().map(|c| (c + 1).to_string()).collect::<Vec<_>>().join("+")
    }
}

impl From<Channels> for Vec<u16> {
    fn from(c: Channels) -> Vec<u16> {
        c.iter().map(|c| c as u16 + 1).collect()
    }
}

impl TryFrom<Vec<u16>> for Channels {
    type Error = String;
    fn try_from(numbers: Vec<u16>) -> Result<Channels, String> {
        let mut mask = 0u64;
        for n in numbers {
            if n == 0 || n as usize > Self::MAX {
                return Err(format!("channel {n}: channels count from 1 to {}", Self::MAX));
            }
            mask |= 1 << (n - 1);
        }
        if mask == 0 {
            return Err("no channels picked".into());
        }
        Ok(Channels(mask))
    }
}

impl std::str::FromStr for Channels {
    type Err = String;
    /// Channel numbers from 1, apart by commas or `+`: `1,3`.
    fn from_str(s: &str) -> Result<Channels, String> {
        let numbers = s.split([',', '+']).map(|n| n.trim().parse::<u16>().map_err(|_| format!("{n:?} is not a channel number"))).collect::<Result<Vec<_>, _>>()?;
        Channels::try_from(numbers)
    }
}

impl Pcm {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self { sample_rate, channels, samples: Vec::new() }
    }

    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.sample_rate as f64
        }
    }

    pub fn frame_at(&self, secs: f64) -> usize {
        ((secs * self.sample_rate as f64).round().max(0.0) as usize).min(self.frames())
    }

    pub fn slice_frames(&self, start: usize, end: usize) -> Pcm {
        let c = self.channels as usize;
        let start = start.min(self.frames());
        let end = end.clamp(start, self.frames());
        Pcm {
            sample_rate: self.sample_rate,
            channels: self.channels,
            samples: self.samples[start * c..end * c].to_vec(),
        }
    }

    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    pub fn apply_gain(&mut self, gain: f32) {
        if gain != 1.0 {
            for s in &mut self.samples {
                *s *= gain;
            }
        }
    }

    pub fn fade_in(&mut self, frames: usize, shape: FadeShape) {
        let c = self.channels as usize;
        let n = frames.min(self.frames());
        for f in 0..n {
            let g = shape.gain(f as f32 / n as f32);
            for s in &mut self.samples[f * c..(f + 1) * c] {
                *s *= g;
            }
        }
    }

    pub fn fade_out(&mut self, frames: usize, shape: FadeShape) {
        let c = self.channels as usize;
        let total = self.frames();
        let n = frames.min(total);
        for i in 0..n {
            let f = total - n + i;
            let g = shape.gain(1.0 - (i + 1) as f32 / n as f32);
            for s in &mut self.samples[f * c..(f + 1) * c] {
                *s *= g;
            }
        }
    }

    /// The kept channels alone; a pick that keeps none of these channels keeps them all.
    pub fn pick(&self, channels: Channels) -> Pcm {
        let c = self.channels as usize;
        let kept: Vec<usize> = channels.iter().filter(|&k| k < c).collect();
        if kept.is_empty() || kept.len() == c {
            return self.clone();
        }
        let samples = self.samples.chunks(c).flat_map(|f| kept.iter().map(move |&k| f[k])).collect();
        Pcm { sample_rate: self.sample_rate, channels: kept.len() as u16, samples }
    }

    /// The frames in reverse order.
    pub fn reverse(&mut self) {
        let c = self.channels.max(1) as usize;
        let frames = self.frames();
        for f in 0..frames / 2 {
            let (head, tail) = self.samples.split_at_mut((frames - 1 - f) * c);
            head[f * c..(f + 1) * c].swap_with_slice(&mut tail[..c]);
        }
    }

    pub fn to_mono(&self, mode: DownmixMode) -> Pcm {
        let c = self.channels as usize;
        if c == 1 {
            return self.clone();
        }
        Pcm { sample_rate: self.sample_rate, channels: 1, samples: self.samples.chunks(c).map(|f| mode.fold(f)).collect() }
    }

    /// The samples of one frame in the `kept` channels, or in all of them.
    fn kept(frame: &[f32], kept: Option<Channels>) -> impl Iterator<Item = &f32> {
        frame.iter().enumerate().filter(move |(c, _)| kept.is_none_or(|k| k.contains(*c))).map(|(_, s)| s)
    }

    /// The span of sound inside `start..end` seconds in the `kept` channels (all with `None`):
    /// from the first to the last frame within `TRIM_BELOW_PEAK_DB` of the span's own peak,
    /// which is where the waveform's drawing starts and stops showing anything. Relative to
    /// the peak, so a quiet recording trims like a loud one. `None` when the span is silent.
    pub fn sound_span(&self, start_secs: f64, end_secs: f64, kept: Option<Channels>) -> Option<(f64, f64)> {
        let c = self.channels as usize;
        let (from, to) = (self.frame_at(start_secs), self.frame_at(end_secs).min(self.frames()));
        if to <= from {
            return None;
        }
        let span = &self.samples[from * c..to * c];
        let peak = span.chunks(c).flat_map(|f| Self::kept(f, kept)).fold(0f32, |m, s| m.max(s.abs()));
        if peak <= 0.0 {
            return None;
        }
        let threshold = peak * db_to_gain(-TRIM_BELOW_PEAK_DB);
        let loud = |f: &[f32]| Self::kept(f, kept).any(|s| s.abs() > threshold);
        let mut frames = span.chunks(c);
        let first = frames.clone().position(loud)?;
        let last = frames.rposition(loud)?;
        let rate = self.sample_rate as f64;
        Some(((from + first) as f64 / rate, (from + last + 1) as f64 / rate))
    }

    /// The spans of sound in the `kept` channels of the file (all with `None`), in seconds:
    /// runs of frames within `TRIM_BELOW_PEAK_DB` of their peak, where a silence of
    /// `TAKE_GAP_SECS` or more ends one, so a recording of several takes with silence between
    /// them gives one span per take.
    pub fn takes(&self, kept: Option<Channels>) -> Vec<(f64, f64)> {
        let c = self.channels as usize;
        let peak = self.samples.chunks(c).flat_map(|f| Self::kept(f, kept)).fold(0f32, |m, s| m.max(s.abs()));
        if peak <= 0.0 {
            return Vec::new();
        }
        let threshold = peak * db_to_gain(-TRIM_BELOW_PEAK_DB);
        let rate = self.sample_rate as f64;
        let gap = (TAKE_GAP_SECS * rate) as usize;
        let mut out = Vec::new();
        let mut start = None;
        let mut last_loud = 0;
        for (i, f) in self.samples.chunks(c).enumerate() {
            if Self::kept(f, kept).any(|s| s.abs() > threshold) {
                start.get_or_insert(i);
                last_loud = i + 1;
            } else if let Some(s) = start {
                if i - last_loud >= gap {
                    out.push((s, last_loud));
                    start = None;
                }
            }
        }
        if let Some(s) = start {
            out.push((s, last_loud));
        }
        out.into_iter().map(|(a, b)| (a as f64 / rate, b as f64 / rate)).collect()
    }
}

/// A trim cuts what stays this far below the span's peak.
pub const TRIM_BELOW_PEAK_DB: f64 = 40.0;

/// A clip's fades: lengths in seconds from its start and back from its end, and their shape.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fades {
    pub in_secs: f64,
    pub out_secs: f64,
    pub shape: FadeShape,
}

impl Fades {
    pub fn is_zero(&self) -> bool {
        self.in_secs <= 0.0 && self.out_secs <= 0.0
    }

    /// The gain at `frame` of a clip `total` frames long at `rate`, as `apply` would leave it.
    pub fn gain(&self, frame: usize, total: usize, rate: u32) -> f32 {
        let frames = |secs: f64| ((secs * rate as f64).round() as usize).min(total);
        let (n_in, n_out) = (frames(self.in_secs), frames(self.out_secs));
        let mut g = 1.0;
        if n_in > 0 && frame < n_in {
            g *= self.shape.gain(frame as f32 / n_in as f32);
        }
        if n_out > 0 && frame + n_out >= total {
            g *= self.shape.gain(1.0 - (frame + n_out - total + 1) as f32 / n_out as f32);
        }
        g
    }

    pub fn apply(&self, pcm: &mut Pcm) {
        let rate = pcm.sample_rate as f64;
        let frames = |secs: f64| (secs * rate).round() as usize;
        pcm.fade_in(frames(self.in_secs), self.shape);
        pcm.fade_out(frames(self.out_secs), self.shape);
    }
}
/// A silence at least this long parts two takes.
pub const TAKE_GAP_SECS: f64 = 0.25;

pub fn db_to_gain(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

pub fn gain_to_db(gain: f64) -> f64 {
    if gain <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * gain.log10()
    }
}

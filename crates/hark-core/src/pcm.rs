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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DownmixMode {
    /// Average of all channels.
    #[default]
    Average,
    /// Sum of all channels at -3 dB per doubling (constant power).
    SumMinus3,
    Left,
    Right,
}

impl std::str::FromStr for DownmixMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "average" | "avg" => Ok(DownmixMode::Average),
            "sum" | "sum_minus3" => Ok(DownmixMode::SumMinus3),
            "left" => Ok(DownmixMode::Left),
            "right" => Ok(DownmixMode::Right),
            _ => Err(format!("unknown downmix {s}: average, sum, left or right")),
        }
    }
}

impl DownmixMode {
    pub fn name(self) -> &'static str {
        match self {
            DownmixMode::Average => "Mono, average",
            DownmixMode::SumMinus3 => "Mono, sum −3 dB",
            DownmixMode::Left => "Left channel",
            DownmixMode::Right => "Right channel",
        }
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

    pub fn to_mono(&self, mode: DownmixMode) -> Pcm {
        let c = self.channels as usize;
        if c == 1 {
            return self.clone();
        }
        let frames = self.frames();
        let mut out = Vec::with_capacity(frames);
        match mode {
            DownmixMode::Left => out.extend(self.samples.chunks(c).map(|f| f[0])),
            DownmixMode::Right => out.extend(self.samples.chunks(c).map(|f| f[1.min(c - 1)])),
            DownmixMode::Average => {
                let k = 1.0 / c as f32;
                out.extend(self.samples.chunks(c).map(|f| f.iter().sum::<f32>() * k));
            }
            DownmixMode::SumMinus3 => {
                let k = 1.0 / (c as f32).sqrt();
                out.extend(self.samples.chunks(c).map(|f| f.iter().sum::<f32>() * k));
            }
        }
        Pcm { sample_rate: self.sample_rate, channels: 1, samples: out }
    }

    /// The span of sound inside `start..end` seconds: from the first to the last frame within
    /// `TRIM_BELOW_PEAK_DB` of the span's own peak, which is where the waveform's drawing
    /// starts and stops showing anything. Relative to the peak, so a quiet recording trims
    /// like a loud one. `None` when the span is silent.
    pub fn sound_span(&self, start_secs: f64, end_secs: f64) -> Option<(f64, f64)> {
        let c = self.channels as usize;
        let (from, to) = (self.frame_at(start_secs), self.frame_at(end_secs).min(self.frames()));
        if to <= from {
            return None;
        }
        let span = &self.samples[from * c..to * c];
        let peak = span.iter().fold(0f32, |m, s| m.max(s.abs()));
        if peak <= 0.0 {
            return None;
        }
        let threshold = peak * db_to_gain(-TRIM_BELOW_PEAK_DB);
        let loud = |f: &[f32]| f.iter().any(|s| s.abs() > threshold);
        let mut frames = span.chunks(c);
        let first = frames.clone().position(loud)?;
        let last = frames.rposition(loud)?;
        let rate = self.sample_rate as f64;
        Some(((from + first) as f64 / rate, (from + last + 1) as f64 / rate))
    }

    /// The spans of sound in the file, in seconds: runs of frames within `TRIM_BELOW_PEAK_DB`
    /// of the file's peak, where a silence of `TAKE_GAP_SECS` or more ends one, so a
    /// recording of several takes with silence between them gives one span per take.
    pub fn takes(&self) -> Vec<(f64, f64)> {
        let c = self.channels as usize;
        let peak = self.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
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
            if f.iter().any(|s| s.abs() > threshold) {
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

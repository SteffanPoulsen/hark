use std::fs::File;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

use crate::pcm::Pcm;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioInfo {
    pub duration_secs: f64,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: Option<u32>,
    pub codec: String,
    pub container: String,
}

impl AudioInfo {
    /// Megabytes the file takes decoded whole, as 32-bit float samples.
    pub fn decoded_mb(&self) -> f64 {
        self.duration_secs * self.sample_rate as f64 * self.channels as f64 * 4.0 / 1e6
    }
}

struct Opened {
    reader: Box<dyn FormatReader>,
    track_id: u32,
    params: symphonia::core::codecs::audio::AudioCodecParameters,
    container: &'static str,
    time_base: Option<TimeBase>,
    frames: Option<u64>,
}

fn open(path: &Path) -> Result<Opened> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let reader = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .with_context(|| format!("probe {}", path.display()))?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("{}: no audio track", path.display()))?;
    let params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => return Err(anyhow!("{}: no audio codec parameters", path.display())),
    };
    let container = reader.format_info().short_name;
    Ok(Opened { track_id: track.id, time_base: track.time_base, frames: track.num_frames, reader, params, container })
}

fn codec_name(params: &symphonia::core::codecs::audio::AudioCodecParameters) -> String {
    symphonia::default::get_codecs()
        .get_audio_decoder(params.codec)
        .map(|d| d.codec.info.short_name.to_string())
        .unwrap_or_else(|| format!("{}", params.codec))
}

/// Reads the header only. Duration comes from the container when it states one and from a
/// full decode otherwise, which happens for MP3 streams without a length tag.
pub fn probe(path: &Path) -> Result<AudioInfo> {
    let opened = open(path)?;
    let sample_rate = opened.params.sample_rate.unwrap_or(0);
    let channels = opened.params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(0);
    let duration_secs = match (opened.frames, sample_rate) {
        (Some(n), r) if r > 0 => n as f64 / r as f64,
        _ => decode(path)?.duration_secs(),
    };
    Ok(AudioInfo {
        duration_secs,
        sample_rate,
        channels,
        bits_per_sample: opened.params.bits_per_sample,
        codec: codec_name(&opened.params),
        container: opened.container.to_string(),
    })
}

pub fn decode(path: &Path) -> Result<Pcm> {
    let mut reader = Reader::open(path)?;
    let mut pcm = Pcm::new(0, 0);
    while reader.next_chunk(&mut pcm.samples)? {}
    if reader.channels == 0 {
        return Err(anyhow!("no audio frames decoded"));
    }
    pcm.sample_rate = reader.sample_rate;
    pcm.channels = reader.channels;
    Ok(pcm)
}

/// Decodes `start_secs..end_secs` alone, seeking to it, so a short span of a long file costs
/// what the span does. The end is clamped to the file.
///
/// The seek lands a second early and each chunk is placed by its own timestamp: a lossy
/// decoder gives silence for its first packets after a seek while it rebuilds its state, and
/// a seek is only packet-accurate, so neither counting from the landing point nor trusting
/// the first chunks would cut the right frames.
pub fn decode_span(path: &Path, start_secs: f64, end_secs: f64) -> Result<Pcm> {
    let mut reader = Reader::open(path)?;
    if reader.sample_rate == 0 || reader.channels == 0 {
        let whole = decode(path)?;
        return Ok(whole.slice_frames(whole.frame_at(start_secs), whole.frame_at(end_secs)));
    }
    let rate = reader.sample_rate as f64;
    let start = (start_secs.max(0.0) * rate).round() as u64;
    let end = (end_secs.max(0.0) * rate).round() as u64;
    let preroll = reader.sample_rate as u64;
    let mut counted = reader.seek(start.saturating_sub(preroll))?;
    let mut pcm = Pcm::new(reader.sample_rate, reader.channels);
    let mut chunk = Vec::new();
    loop {
        if !reader.next_chunk(&mut chunk)? {
            break;
        }
        let ch = reader.channels as usize;
        let frames = (chunk.len() / ch) as u64;
        let pos = reader.chunk_start.unwrap_or(counted);
        if pos >= end {
            break;
        }
        let from = start.saturating_sub(pos).min(frames) as usize;
        let to = (end - pos).min(frames) as usize;
        if from < to {
            pcm.samples.extend_from_slice(&chunk[from * ch..to * ch]);
        }
        counted = pos + frames;
        chunk.clear();
    }
    pcm.sample_rate = reader.sample_rate;
    pcm.channels = reader.channels;
    Ok(pcm)
}

/// A file decoded one packet at a time, seekable by frame.
pub struct Reader {
    opened: Opened,
    decoder: Box<dyn AudioDecoder>,
    chunk: Vec<f32>,
    /// From the header until the first packet decodes, then from the decoded audio.
    pub sample_rate: u32,
    pub channels: u16,
    /// Length in frames when the container states it.
    pub frames: Option<u64>,
    /// Where the last chunk starts, in frames, when the container timestamps its packets.
    pub chunk_start: Option<u64>,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Reader> {
        let opened = open(path)?;
        let decoder = symphonia::default::get_codecs().make_audio_decoder(&opened.params, &AudioDecoderOptions::default())?;
        Ok(Reader {
            sample_rate: opened.params.sample_rate.unwrap_or(0),
            channels: opened.params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(0),
            frames: opened.frames,
            opened,
            decoder,
            chunk: Vec::new(),
            chunk_start: None,
        })
    }

    /// Appends the next packet's interleaved frames to `out`; `false` once the file is exhausted.
    pub fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        loop {
            let Some(packet) = self.opened.reader.next_packet()? else { return Ok(false) };
            if packet.track_id != self.opened.track_id {
                continue;
            }
            let buf = match self.decoder.decode(&packet) {
                Ok(b) => b,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            self.sample_rate = buf.spec().rate();
            self.channels = buf.spec().channels().count() as u16;
            self.chunk_start = match self.opened.time_base {
                Some(tb) => tb.calc_time(packet.pts).map(|t| (t.as_secs_f64() * self.sample_rate as f64).round() as u64),
                None => Some(packet.pts.get().max(0) as u64),
            };
            buf.copy_to_vec_interleaved(&mut self.chunk);
            out.extend_from_slice(&self.chunk);
            return Ok(true);
        }
    }

    /// Positions the reader at or before `frame` and returns the frame the next chunk starts
    /// at; the caller drops the difference.
    pub fn seek(&mut self, frame: u64) -> Result<u64> {
        if self.sample_rate == 0 {
            return Err(anyhow!("sample rate unknown, cannot seek"));
        }
        let time = Time::try_from_secs_f64(frame as f64 / self.sample_rate as f64).ok_or_else(|| anyhow!("seek out of range"))?;
        let to = self.opened.reader.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.opened.track_id) })?;
        self.decoder.reset();
        let landed = match self.opened.time_base {
            Some(tb) => tb.calc_time(to.actual_ts).map(|t| (t.as_secs_f64() * self.sample_rate as f64).round() as u64).unwrap_or(0),
            None => to.actual_ts.get().max(0) as u64,
        };
        Ok(landed.min(frame))
    }
}

pub fn is_audio_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "wav" | "wave" | "flac" | "ogg" | "oga" | "mp3" | "aif" | "aiff" | "aifc" | "m4a" | "mp4"
            | "caf" | "mka" | "webm" | "opus"
    )
}

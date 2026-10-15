use std::fs::File;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

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

struct Opened {
    reader: Box<dyn FormatReader>,
    track_id: u32,
    params: symphonia::core::codecs::audio::AudioCodecParameters,
    container: &'static str,
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
    Ok(Opened { track_id: track.id, reader, params, container })
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
    let track = opened.reader.tracks().iter().find(|t| t.id == opened.track_id);
    let frames = track.and_then(|t| t.num_frames);
    let duration_secs = match (frames, sample_rate) {
        (Some(n), r) if r > 0 => n as f64 / r as f64,
        _ => decode_opened(opened_reopen(path)?)?.duration_secs(),
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

fn opened_reopen(path: &Path) -> Result<Opened> {
    open(path)
}

pub fn decode(path: &Path) -> Result<Pcm> {
    decode_opened(open(path)?)
}

fn decode_opened(mut opened: Opened) -> Result<Pcm> {
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&opened.params, &AudioDecoderOptions::default())?;
    let mut pcm = Pcm::new(opened.params.sample_rate.unwrap_or(0), 0);
    let mut chunk: Vec<f32> = Vec::new();
    while let Some(packet) = opened.reader.next_packet()? {
        if packet.track_id != opened.track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        if pcm.channels == 0 {
            pcm.channels = buf.spec().channels().count() as u16;
            pcm.sample_rate = buf.spec().rate();
        }
        buf.copy_to_vec_interleaved(&mut chunk);
        pcm.samples.extend_from_slice(&chunk);
    }
    if pcm.channels == 0 {
        return Err(anyhow!("no audio frames decoded"));
    }
    Ok(pcm)
}

pub fn is_audio_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "wav" | "wave" | "flac" | "ogg" | "oga" | "mp3" | "aif" | "aiff" | "aifc" | "m4a" | "mp4"
            | "caf" | "mka" | "webm" | "opus"
    )
}

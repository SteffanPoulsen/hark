//! RIFF/WAVE writing with the `hark` origin chunk and a BWF `bext` chunk, plus chunk reading.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::pcm::Pcm;

pub const HARK_CHUNK: &[u8; 4] = b"hark";

#[derive(Clone, Debug, Default)]
pub struct Bext {
    pub description: String,
    pub originator: String,
    pub originator_reference: String,
    /// `YYYY-MM-DD`
    pub date: String,
    /// `HH:MM:SS`
    pub time: String,
    pub loudness_lufs: Option<f64>,
    pub loudness_range_lu: Option<f64>,
    pub max_true_peak_dbtp: Option<f64>,
}

/// Simple TPDF dither at one LSB, from a tiny xorshift so the core stays dependency-free.
pub struct Dither(u64);

impl Dither {
    pub fn new() -> Self {
        Dither(0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Zero-mean triangular noise in `(-1, 1)` LSB.
    pub fn sample(&mut self) -> f32 {
        self.next() - self.next()
    }
}

impl Default for Dither {
    fn default() -> Self {
        Self::new()
    }
}

/// Quantises float samples to signed integers of `bits` width.
pub fn quantize(samples: &[f32], bits: u16, dither: bool) -> Vec<i32> {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let min = -(1i64 << (bits - 1)) as f32;
    let mut d = Dither::new();
    samples
        .iter()
        .map(|&s| {
            let v = s * max + if dither { d.sample() } else { 0.0 };
            v.round().clamp(min, max) as i32
        })
        .collect()
}

pub fn write(path: &Path, pcm: &Pcm, bits: u16, dither: bool, origin_json: &[u8], bext: &Bext) -> Result<()> {
    let channels = pcm.channels;
    let (format_tag, bytes_per_sample) = match bits {
        16 => (1u16, 2u16),
        24 => (1u16, 3u16),
        32 => (3u16, 4u16),
        _ => return Err(anyhow!("unsupported WAV bit depth {bits}")),
    };
    let block_align = bytes_per_sample * channels;
    let byte_rate = pcm.sample_rate * block_align as u32;

    let mut data = Vec::with_capacity(pcm.samples.len() * bytes_per_sample as usize);
    match bits {
        32 => {
            for s in &pcm.samples {
                data.extend_from_slice(&s.to_le_bytes());
            }
        }
        16 => {
            for v in quantize(&pcm.samples, 16, dither) {
                data.extend_from_slice(&(v as i16).to_le_bytes());
            }
        }
        _ => {
            for v in quantize(&pcm.samples, 24, dither) {
                data.extend_from_slice(&v.to_le_bytes()[..3]);
            }
        }
    }

    let mut fmt = Vec::with_capacity(18);
    fmt.extend_from_slice(&format_tag.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&pcm.sample_rate.to_le_bytes());
    fmt.extend_from_slice(&byte_rate.to_le_bytes());
    fmt.extend_from_slice(&block_align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if format_tag == 3 {
        fmt.extend_from_slice(&0u16.to_le_bytes());
    }

    let bext_bytes = encode_bext(bext);
    let chunks: Vec<(&[u8; 4], &[u8])> = {
        let mut v: Vec<(&[u8; 4], &[u8])> = vec![(b"fmt ", &fmt)];
        if format_tag == 3 {
            v.push((b"fact", &[]));
        }
        v.push((b"bext", &bext_bytes));
        v.push((HARK_CHUNK, origin_json));
        v.push((b"data", &data));
        v
    };
    let fact_frames = (pcm.frames() as u32).to_le_bytes();
    let riff_size: u32 = 4 + chunks
        .iter()
        .map(|(id, body)| {
            let len = if *id == b"fact" { 4 } else { body.len() };
            8 + len + (len & 1)
        })
        .sum::<usize>() as u32;

    crate::paths::ensure_parent(path)?;
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&riff_size.to_le_bytes())?;
    w.write_all(b"WAVE")?;
    for (id, body) in chunks {
        let body: &[u8] = if id == b"fact" { &fact_frames } else { body };
        w.write_all(id)?;
        w.write_all(&(body.len() as u32).to_le_bytes())?;
        w.write_all(body)?;
        if body.len() & 1 == 1 {
            w.write_all(&[0])?;
        }
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bit depth reads back through symphonia with its samples, and the `hark` and
    /// `bext` chunks come back intact; the origin is an odd length so the padding byte between
    /// chunks is exercised.
    #[test]
    fn chunks_and_samples_round_trip() {
        let samples: Vec<f32> = (0..2000).map(|i| ((i % 100) as f32 / 50.0 - 1.0) * 0.9).collect();
        let pcm = Pcm { sample_rate: 44_100, channels: 2, samples };
        let origin = br#"{"hark":"test","odd":1}"#;
        assert_eq!(origin.len() % 2, 1);
        let bext = Bext { description: "hark: pack:hit.wav [0.100s-0.200s]".into(), originator: "hark test".into(), ..Default::default() };
        for bits in [16u16, 24, 32] {
            let path = std::env::temp_dir().join(format!("hark-wav-{bits}-{}.wav", std::process::id()));
            write(&path, &pcm, bits, false, origin, &bext).unwrap();
            let back = crate::decode(&path);
            let chunk = read_chunk(&path, HARK_CHUNK).unwrap();
            let desc = read_bext_description(&path).unwrap();
            let _ = std::fs::remove_file(&path);
            let back = back.unwrap_or_else(|e| panic!("{bits} bit: {e:#}"));
            assert_eq!((back.sample_rate, back.channels, back.samples.len()), (44_100, 2, pcm.samples.len()), "{bits} bit");
            let worst = pcm.samples.iter().zip(&back.samples).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst <= 2.0 / (1u32 << (bits.min(24) - 1)) as f32, "{bits} bit drifts by {worst}");
            assert_eq!(chunk.as_deref(), Some(&origin[..]), "{bits} bit");
            assert_eq!(desc.as_deref(), Some(bext.description.as_str()), "{bits} bit");
        }
    }
}

fn fixed(dst: &mut Vec<u8>, s: &str, len: usize) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.truncate(len);
    bytes.resize(len, 0);
    dst.extend_from_slice(&bytes);
}

fn loudness_field(v: Option<f64>) -> i16 {
    match v {
        Some(x) if x.is_finite() => (x * 100.0).round().clamp(-32767.0, 32767.0) as i16,
        _ => 0x7FFF,
    }
}

fn encode_bext(b: &Bext) -> Vec<u8> {
    let mut v = Vec::with_capacity(602);
    fixed(&mut v, &b.description, 256);
    fixed(&mut v, &b.originator, 32);
    fixed(&mut v, &b.originator_reference, 32);
    fixed(&mut v, &b.date, 10);
    fixed(&mut v, &b.time, 8);
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&[0u8; 64]);
    v.extend_from_slice(&loudness_field(b.loudness_lufs).to_le_bytes());
    v.extend_from_slice(&loudness_field(b.loudness_range_lu).to_le_bytes());
    v.extend_from_slice(&loudness_field(b.max_true_peak_dbtp).to_le_bytes());
    v.extend_from_slice(&0x7FFFi16.to_le_bytes());
    v.extend_from_slice(&0x7FFFi16.to_le_bytes());
    v.extend_from_slice(&[0u8; 180]);
    v
}

/// Returns the body of the first top-level chunk with `id`, when the file is RIFF/WAVE.
pub fn read_chunk(path: &Path, id: &[u8; 4]) -> Result<Option<Vec<u8>>> {
    let mut f = std::fs::File::open(path)?;
    let mut hdr = [0u8; 12];
    if f.read_exact(&mut hdr).is_err() || &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Ok(None);
    }
    let end = f.metadata()?.len();
    let mut pos = 12u64;
    let mut ch = [0u8; 8];
    while pos + 8 <= end {
        f.seek(SeekFrom::Start(pos))?;
        if f.read_exact(&mut ch).is_err() {
            break;
        }
        let len = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[0..4] == id {
            let mut body = vec![0u8; len.min(end - pos - 8) as usize];
            f.read_exact(&mut body)?;
            return Ok(Some(body));
        }
        pos += 8 + len + (len & 1);
    }
    Ok(None)
}

pub fn read_bext_description(path: &Path) -> Result<Option<String>> {
    Ok(read_chunk(path, b"bext")?.map(|b| {
        let desc = &b[..256.min(b.len())];
        let end = desc.iter().position(|&c| c == 0).unwrap_or(desc.len());
        String::from_utf8_lossy(&desc[..end]).into_owned()
    }))
}

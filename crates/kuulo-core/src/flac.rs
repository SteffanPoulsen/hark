//! FLAC writing with the origin in an APPLICATION block and a Vorbis comment, plus block reading.

use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Result};
use flacenc::component::{BitRepr, MetadataBlockData, Stream};
use flacenc::encode_fixed_size_frame;
use flacenc::error::Verify;

use crate::pcm::Pcm;
use crate::wav::quantize;

pub const ORIGIN_APP_ID: &[u8; 4] = b"kuul";
const BLOCK_APPLICATION: u8 = 2;
const BLOCK_VORBIS_COMMENT: u8 = 4;

pub fn write(path: &Path, pcm: &Pcm, bits: u16, dither: bool, origin_json: &[u8], description: &str) -> Result<()> {
    if !matches!(bits, 16 | 24) {
        return Err(anyhow!("unsupported FLAC bit depth {bits}"));
    }
    let samples = quantize(&pcm.samples, bits, dither);
    let mut stream = encode(&samples, pcm, bits)?;

    let mut app = ORIGIN_APP_ID.to_vec();
    app.extend_from_slice(origin_json);
    stream.add_metadata_block(MetadataBlockData::new_unknown(BLOCK_APPLICATION, &app).map_err(|e| anyhow!("flac application block: {e:?}"))?);
    let comment = vorbis_comment(&[("DESCRIPTION", description), ("KUULO_ORIGIN",std::str::from_utf8(origin_json)?)]);
    stream.add_metadata_block(MetadataBlockData::new_unknown(BLOCK_VORBIS_COMMENT, &comment).map_err(|e| anyhow!("flac vorbis comment: {e:?}"))?);

    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow!("flac write: {e:?}"))?;
    crate::paths::ensure_parent(path)?;
    std::fs::write(path, sink.as_slice())?;
    Ok(())
}

/// flacenc's own loop, frame by frame, around two of its faults. Its stream info takes the
/// last, shorter block as the minimum block size, which marks the stream variable-sized to
/// a strict reader; symphonia then rejects every frame, so kuulo could not read what it
/// wrote. And its Rice parameter estimate overflows on a run of residuals above 2^28, which
/// full-scale 24-bit noise through its predictors produces; the parameter it then picks
/// bloats the frame and, on a full block, corrupts it. A frame that came out larger than
/// its plain samples can only be that, and is encoded again with prediction off.
fn encode(samples: &[i32], pcm: &Pcm, bits: u16) -> Result<Stream> {
    use flacenc::source::{Context, FrameBuf, MemSource, Source};
    let (channels, bits, rate) = (pcm.channels as usize, bits as usize, pcm.sample_rate as usize);
    let predicted = flacenc::config::Encoder::default();
    let mut plain = predicted.clone();
    plain.subframe_coding.use_fixed = false;
    plain.subframe_coding.use_lpc = false;
    let predicted = predicted.into_verified().map_err(|e| anyhow!("flac config: {e:?}"))?;
    let plain = plain.into_verified().map_err(|e| anyhow!("flac config: {e:?}"))?;
    let block = predicted.block_size;

    let mut source = MemSource::from_samples(samples, channels, bits, rate);
    let mut stream = Stream::new(rate, channels, bits).map_err(|e| anyhow!("flac stream: {e:?}"))?;
    let mut buf = (FrameBuf::with_size(channels, block).map_err(|e| anyhow!("flac frame: {e:?}"))?, Context::new(bits, channels));
    loop {
        let frames = source.read_samples(block, &mut buf).map_err(|e| anyhow!("flac read: {e:?}"))?;
        if frames == 0 {
            break;
        }
        let number = buf.1.current_frame_number().unwrap_or(0);
        let mut frame = encode_fixed_size_frame(&predicted, &buf.0, number, stream.stream_info()).map_err(|e| anyhow!("flac encode: {e:?}"))?;
        if frame.count_bits() > frames * channels * bits + 1024 {
            frame = encode_fixed_size_frame(&plain, &buf.0, number, stream.stream_info()).map_err(|e| anyhow!("flac encode: {e:?}"))?;
        }
        stream.add_frame(frame);
    }
    let info = stream.stream_info_mut();
    info.set_md5_digest(&buf.1.md5_digest());
    info.set_total_samples(buf.1.total_samples());
    info.set_block_sizes(block, block).map_err(|e| anyhow!("flac block sizes: {e:?}"))?;
    Ok(stream)
}

fn vorbis_comment(fields: &[(&str, &str)]) -> Vec<u8> {
    let vendor = format!("kuulo {}", crate::VERSION);
    let mut v = Vec::new();
    v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    v.extend_from_slice(vendor.as_bytes());
    v.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    for (k, val) in fields {
        let entry = format!("{k}={val}");
        v.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        v.extend_from_slice(entry.as_bytes());
    }
    v
}

/// Body (after the 4-byte application id) of the first APPLICATION block with `app_id`.
pub fn read_application_block(path: &Path, app_id: &[u8; 4]) -> Result<Option<Vec<u8>>> {
    let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_err() || &magic != b"fLaC" {
        return Ok(None);
    }
    loop {
        let mut hdr = [0u8; 4];
        if f.read_exact(&mut hdr).is_err() {
            return Ok(None);
        }
        let last = hdr[0] & 0x80 != 0;
        let kind = hdr[0] & 0x7F;
        let len = u32::from_be_bytes([0, hdr[1], hdr[2], hdr[3]]) as usize;
        let mut body = vec![0u8; len];
        f.read_exact(&mut body)?;
        if kind == BLOCK_APPLICATION && body.len() >= 4 && &body[..4] == app_id {
            return Ok(Some(body[4..].to_vec()));
        }
        if last {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full-scale noise, two blocks of it, the case flacenc's Rice estimate misjudges: the
    /// file must decode to the same samples and take no more room than the plain samples.
    #[test]
    fn full_scale_noise_round_trips_at_plain_size() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let samples: Vec<f32> = (0..9000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 12) as f32 / (1u64 << 52) as f32 * 2.0 - 1.0
            })
            .collect();
        let pcm = Pcm { sample_rate: 44_100, channels: 1, samples };
        let path = std::env::temp_dir().join(format!("kuulo-flac-noise-{}.flac", std::process::id()));
        write(&path, &pcm, 24, false, b"{}", "noise").unwrap();
        let size = std::fs::metadata(&path).unwrap().len() as usize;
        let back = crate::decode(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(back.samples.len(), pcm.samples.len());
        let (at, worst) = pcm.samples.iter().zip(&back.samples).map(|(a, b)| (a - b).abs()).enumerate().fold((0, 0.0f32), |m, (i, d)| if d > m.1 { (i, d) } else { m });
        assert!(worst < 2.0 / (1 << 23) as f32, "sample {at} drifts by {worst}: {} became {}; first bad frame {}", pcm.samples[at], back.samples[at], at / 4096);
        let plain = pcm.samples.len() * 3;
        assert!(size < plain + plain / 10, "{size} bytes for {plain} bytes of samples");
    }
}

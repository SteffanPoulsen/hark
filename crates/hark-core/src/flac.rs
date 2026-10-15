//! FLAC writing with the origin in an APPLICATION block and a Vorbis comment, plus block reading.

use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Result};
use flacenc::component::{BitRepr, MetadataBlockData};
use flacenc::error::Verify;

use crate::pcm::Pcm;
use crate::wav::quantize;

pub const HARK_APP_ID: &[u8; 4] = b"hark";
const BLOCK_APPLICATION: u8 = 2;
const BLOCK_VORBIS_COMMENT: u8 = 4;

pub fn write(path: &Path, pcm: &Pcm, bits: u16, dither: bool, origin_json: &[u8], description: &str) -> Result<()> {
    if !matches!(bits, 16 | 24) {
        return Err(anyhow!("unsupported FLAC bit depth {bits}"));
    }
    let samples = quantize(&pcm.samples, bits, dither);
    let config = flacenc::config::Encoder::default().into_verified().map_err(|e| anyhow!("flac config: {e:?}"))?;
    let source = flacenc::source::MemSource::from_samples(&samples, pcm.channels as usize, bits as usize, pcm.sample_rate as usize);
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).map_err(|e| anyhow!("flac encode: {e:?}"))?;

    let mut app = HARK_APP_ID.to_vec();
    app.extend_from_slice(origin_json);
    stream.add_metadata_block(MetadataBlockData::new_unknown(BLOCK_APPLICATION, &app).map_err(|e| anyhow!("flac application block: {e:?}"))?);
    let comment = vorbis_comment(&[("DESCRIPTION", description), ("HARK_ORIGIN", std::str::from_utf8(origin_json)?)]);
    stream.add_metadata_block(MetadataBlockData::new_unknown(BLOCK_VORBIS_COMMENT, &comment).map_err(|e| anyhow!("flac vorbis comment: {e:?}"))?);

    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow!("flac write: {e:?}"))?;
    crate::paths::ensure_parent(path)?;
    std::fs::write(path, sink.as_slice())?;
    Ok(())
}

fn vorbis_comment(fields: &[(&str, &str)]) -> Vec<u8> {
    let vendor = format!("hark {}", crate::VERSION);
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

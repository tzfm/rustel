//! The disk cache of decoded sample PCM.
//!
//! A compressed sample is decoded and resampled to the context rate every
//! time a process first needs it: a second or two of CPU per file on a small
//! machine, paid again by every later studio because RAM holds decoded PCM
//! only while the process lives. This cache stores the decoded frames beside
//! the encoded files, keyed by the digest of the bytes that produced them,
//! so the second load anywhere is a file read. The frames are raw f32, the
//! same values the decoder produced, so the sound is bit for bit the sound
//! the uncached path renders.

use rustel_audio::DecodedSample;
use sha2::{Digest, Sha256};

use super::cache::ensure_private_dir;

/// The folder this cache owns under the sample cache directory.
pub(super) const DECODED_DIR: &str = "decoded";

/// One magic plus version, so a foreign or later file is a miss, not a
/// crash: 4 bytes magic, u32 version, u32 rate, u16 channels, u64 frames.
const HEADER_LEN: usize = 4 + 4 + 4 + 2 + 8;

/// Decode `bytes` through the decoded-PCM cache under `dir`.
///
/// A cache hit returns exactly what [`super::codecs::decode_guarded`] returned
/// for the same bytes and rate; a miss decodes, stores best-effort, and
/// returns the fresh decode either way.
pub(super) fn decode_cached(
    dir: &std::path::Path,
    url: &str,
    context_rate: Option<u32>,
    bytes: &[u8],
) -> Result<DecodedSample, String> {
    let rate = context_rate.unwrap_or(0);
    let codec = super::codecs::codec_for(url);
    let path = sidecar_path(dir, bytes, rate, codec);
    if let Some(decoded) = read_sidecar(&path) {
        return Ok(decoded);
    }
    let decoded = super::codecs::decode_guarded(url, bytes, context_rate)?;
    write_sidecar(&path, &decoded);
    Ok(decoded)
}

/// The sidecar file for these bytes under this codec at this context rate:
/// the content digest names it, so a source that changed upstream never
/// answers to an older decode, the two context rates a file can decode
/// under never collide, and the same bytes under two extensions - which
/// name two decoders - never share one decode.
fn sidecar_path(
    dir: &std::path::Path,
    bytes: &[u8],
    rate: u32,
    codec: super::Codec,
) -> std::path::PathBuf {
    let tag = match codec {
        super::Codec::Wav => b"w",
        super::Codec::Mp3 => b"m",
        super::Codec::Ogg => b"o",
    };
    let mut hasher = Sha256::new();
    hasher.update(tag);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let name = format!("{}.r{rate}.pcm", hex(&digest));
    dir.join(DECODED_DIR).join(name)
}

fn hex(digest: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(TABLE[usize::from(byte >> 4)] as char);
        out.push(TABLE[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Read one sidecar, refusing anything short, long, or misshapen.
fn read_sidecar(path: &std::path::Path) -> Option<DecodedSample> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < HEADER_LEN || &bytes[..4] != b"RSPC" {
        return None;
    }
    let mut header = [0u8; HEADER_LEN];
    header.copy_from_slice(&bytes[..HEADER_LEN]);
    let version = u32::from_le_bytes(header[4..8].try_into().ok()?);
    let rate = u32::from_le_bytes(header[8..12].try_into().ok()?);
    let channels = u16::from_le_bytes(header[12..14].try_into().ok()?);
    let frames = u64::from_le_bytes(header[14..22].try_into().ok()?);
    if version != 1 || rate == 0 || channels == 0 || frames == 0 {
        return None;
    }
    let frames = usize::try_from(frames).ok()?;
    // A damaged header can name any frame count; the size arithmetic is
    // checked so a huge one is a miss, never an overflow.
    let body = match frames
        .checked_mul(usize::from(channels))
        .and_then(|samples| samples.checked_mul(4))
    {
        Some(body) if body <= rustel_audio::sample_pcm_ceiling() => body,
        _ => return None,
    };
    if bytes.len() != HEADER_LEN + body {
        return None;
    }
    let mut pcm = vec![0.0f32; body / 4];
    let body = &bytes[HEADER_LEN..];
    for (index, slot) in pcm.iter_mut().enumerate() {
        let at = index * 4;
        *slot = f32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
    }
    DecodedSample::from_parts(rate, channels, pcm).ok()
}

/// Store one decode beside its source, best-effort: a full disk or a
/// read-only cache costs the next load its decode again, nothing more.
/// The frames stream out in bounded chunks, so a long sample never holds
/// itself twice in memory for the write.
fn write_sidecar(path: &std::path::Path, decoded: &DecodedSample) {
    let Some(parent) = path.parent() else {
        return;
    };
    if ensure_private_dir(parent).is_err() {
        return;
    }
    let body = decoded.pcm().len() * 4;
    if body > rustel_audio::sample_pcm_ceiling() {
        // A read would refuse this much PCM; do not store what cannot
        // come back.
        return;
    }
    let staged = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("pcm"),
        std::process::id()
    ));
    if write_staged(&staged, decoded).is_err() {
        let _ = std::fs::remove_file(&staged);
        return;
    }
    let _ = std::fs::rename(&staged, path);
}

fn write_staged(staged: &std::path::Path, decoded: &DecodedSample) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::fs::File::create(staged)?;
    let frames = u64::try_from(decoded.frames()).unwrap_or(u64::MAX);
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(b"RSPC");
    header[4..8].copy_from_slice(&1u32.to_le_bytes());
    header[8..12].copy_from_slice(&decoded.sample_rate().to_le_bytes());
    header[12..14].copy_from_slice(&decoded.channels().to_le_bytes());
    header[14..22].copy_from_slice(&frames.to_le_bytes());
    out.write_all(&header)?;
    let mut chunk = Vec::with_capacity(64 * 1024);
    for batch in decoded.pcm().chunks(16 * 1024) {
        chunk.clear();
        for sample in batch {
            chunk.extend_from_slice(&sample.to_le_bytes());
        }
        out.write_all(&chunk)?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rate: u32, channels: u16, pcm: Vec<f32>) -> DecodedSample {
        DecodedSample::from_parts(rate, channels, pcm).expect("sample")
    }

    /// The cache returns the decode bit for bit, and the second load never
    /// runs the decoder.
    #[test]
    fn a_stored_decode_round_trips_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let pcm: Vec<f32> = (0..48_000).map(|i| i as f32 * 0.5 - 1.0).collect();
        let stored = sample(48_000, 2, pcm.clone());
        let path = sidecar_path(dir.path(), b"bytes", 48_000, super::super::Codec::Wav);
        write_sidecar(&path, &stored);
        let loaded = read_sidecar(&path).expect("the sidecar reads back");
        assert_eq!(loaded.sample_rate(), 48_000);
        assert_eq!(loaded.channels(), 2);
        assert_eq!(loaded.pcm(), stored.pcm());

        // Different bytes, a different rate, or a different codec are
        // different entries: the same bytes under two extensions name two
        // decoders, and neither may answer to the other's decode.
        assert_ne!(
            sidecar_path(dir.path(), b"bytes", 48_000, super::super::Codec::Wav),
            sidecar_path(dir.path(), b"bytes2", 48_000, super::super::Codec::Wav)
        );
        assert_ne!(
            sidecar_path(dir.path(), b"bytes", 48_000, super::super::Codec::Wav),
            sidecar_path(dir.path(), b"bytes", 44_100, super::super::Codec::Wav)
        );
        assert_ne!(
            sidecar_path(dir.path(), b"bytes", 48_000, super::super::Codec::Wav),
            sidecar_path(dir.path(), b"bytes", 48_000, super::super::Codec::Mp3)
        );
    }

    /// A truncated or foreign file is a miss, never a panic or a wrong sound.
    #[test]
    fn a_damaged_sidecar_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let stored = sample(48_000, 1, vec![0.25; 100]);
        let path = sidecar_path(dir.path(), b"bytes", 48_000, super::super::Codec::Wav);
        write_sidecar(&path, &stored);
        let full = std::fs::read(&path).unwrap();
        assert!(read_sidecar(&path).is_some());

        std::fs::write(&path, &full[..full.len() - 1]).unwrap();
        assert!(read_sidecar(&path).is_none());
        std::fs::write(&path, b"not a sidecar at all").unwrap();
        assert!(read_sidecar(&path).is_none());
    }
}

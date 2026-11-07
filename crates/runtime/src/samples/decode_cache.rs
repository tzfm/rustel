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

/// The most bytes the sidecars hold together. A new one makes room by
/// removing the oldest, so the folder never grows past it.
const MAX_DECODED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How long a staging file goes unwritten before it counts as left by a
/// writer that ended mid-write. A writer streams its file out in seconds.
const STALE_STAGING: std::time::Duration = std::time::Duration::from_secs(10 * 60);

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
    write_sidecar(&path, &decoded, MAX_DECODED_BYTES);
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
        super::Codec::Mp4 => b"4",
        super::Codec::Matroska => b"k",
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
/// The sidecars stay under `cap` bytes together, the oldest going first.
/// The frames stream out in bounded chunks, so a long sample never holds
/// itself twice in memory for the write.
fn write_sidecar(path: &std::path::Path, decoded: &DecodedSample, cap: u64) {
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
    let size = u64::try_from(HEADER_LEN + body).unwrap_or(u64::MAX);
    if size > cap || !make_room(parent, size, cap) {
        return;
    }
    // One staging file per writer: loader threads that decode the same
    // bytes at once must not write into one file. As in the sample cache.
    static WRITER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let staged = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("pcm"),
        std::process::id(),
        WRITER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if write_staged(&staged, decoded).is_err() {
        let _ = std::fs::remove_file(&staged);
        return;
    }
    let _ = std::fs::rename(&staged, path);
}

/// Remove the oldest sidecars until `size` more bytes fit under `cap`.
/// Returns whether they fit. A staging file left by a writer that ended
/// mid-write goes too.
fn make_room(dir: &std::path::Path, size: u64, cap: u64) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut held = Vec::new();
    for entry in entries.flatten() {
        let (path, Ok(metadata)) = (entry.path(), entry.metadata()) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let written = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("pcm") => held.push((written, metadata.len(), path)),
            Some("tmp") if written.elapsed().is_ok_and(|age| age > STALE_STAGING) => {
                let _ = std::fs::remove_file(path);
            }
            _ => {}
        }
    }
    held.sort_unstable_by_key(|(written, _, _)| *written);
    let mut total: u64 = held.iter().map(|(_, len, _)| len).sum();
    for (_, len, path) in held {
        if total.saturating_add(size) <= cap {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total -= len;
        }
    }
    total.saturating_add(size) <= cap
}

fn write_staged(staged: &std::path::Path, decoded: &DecodedSample) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staged)?;
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
    // On the disk before the rename names it, so a crash cannot leave a
    // sidecar of the right length that holds no frames.
    out.sync_all()
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
        write_sidecar(&path, &stored, MAX_DECODED_BYTES);
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
        write_sidecar(&path, &stored, MAX_DECODED_BYTES);
        let full = std::fs::read(&path).unwrap();
        assert!(read_sidecar(&path).is_some());

        std::fs::write(&path, &full[..full.len() - 1]).unwrap();
        assert!(read_sidecar(&path).is_none());
        std::fs::write(&path, b"not a sidecar at all").unwrap();
        assert!(read_sidecar(&path).is_none());
    }

    /// The sidecars stay under their cap: a new one takes the place of the
    /// oldest, and one larger than the cap is not stored.
    #[test]
    fn a_new_sidecar_makes_room_by_removing_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let stored = sample(48_000, 1, vec![0.25; 100]);
        let size = (HEADER_LEN + 400) as u64;
        let path = |bytes: &[u8]| sidecar_path(dir.path(), bytes, 48_000, super::super::Codec::Wav);
        let paths = [path(b"one"), path(b"two"), path(b"six")];
        for (age, path) in (1..).zip(&paths) {
            write_sidecar(path, &stored, 2 * size);
            // A second apart, whatever the clock resolution of the disk.
            let written = std::time::UNIX_EPOCH + std::time::Duration::from_secs(age);
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(written).unwrap();
        }
        assert_eq!(
            paths.each_ref().map(|path| path.exists()),
            [false, true, true]
        );

        let large = path(b"large");
        write_sidecar(&large, &stored, size - 1);
        assert!(!large.exists() && paths[1].exists() && paths[2].exists());
    }

    /// A staging file a writer left when it ended mid-write goes at the
    /// next store. One that a writer still fills stays.
    #[test]
    fn a_store_removes_the_staging_a_crash_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), b"bytes", 48_000, super::super::Codec::Wav);
        let decoded = path.parent().unwrap();
        std::fs::create_dir_all(decoded).unwrap();
        let (left, filling) = (
            decoded.join(".a.pcm.1.0.tmp"),
            decoded.join(".b.pcm.2.0.tmp"),
        );
        for staged in [&left, &filling] {
            std::fs::write(staged, [0u8; 64]).unwrap();
        }
        let file = std::fs::File::options().write(true).open(&left).unwrap();
        file.set_modified(std::time::SystemTime::now() - 2 * STALE_STAGING)
            .unwrap();
        write_sidecar(&path, &sample(48_000, 1, vec![0.5; 10]), MAX_DECODED_BYTES);
        assert!(path.exists());
        assert!(!left.exists(), "the staging a crash left");
        assert!(filling.exists(), "the staging a writer fills");
    }
}

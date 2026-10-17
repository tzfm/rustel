//! WAV file writing for rendered speech.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Write `samples` as mono 16-bit PCM at `rate`. Returns the number of
/// samples written. Values beyond ±1 clip, the WAV convention.
pub fn write_wav(path: impl AsRef<Path>, samples: &[f32], rate: u32) -> Result<usize, String> {
    let data_bytes = samples.len() * 2;
    let file = File::create(path.as_ref()).map_err(|e| e.to_string())?;
    let mut w = BufWriter::new(file);

    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&1u16.to_le_bytes()); // mono
    header.extend_from_slice(&rate.to_le_bytes());
    header.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
    header.extend_from_slice(&2u16.to_le_bytes()); // block align
    header.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    w.write_all(&header).map_err(|e| e.to_string())?;

    let mut pcm = Vec::with_capacity(data_bytes);
    for s in samples {
        let value = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        pcm.extend_from_slice(&value.to_le_bytes());
    }
    w.write_all(&pcm).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    Ok(samples.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_parsable_header_and_body() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("talkit-wav-test-{}.wav", std::process::id()));
        let samples = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let written = write_wav(&path, &samples, 22050).unwrap();
        assert_eq!(written, 5);

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[36..40], b"data");
        let data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        assert_eq!(data_len, 10);
        assert_eq!(bytes.len(), 44 + data_len);
        let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        assert_eq!(rate, 22050);
        let first = i16::from_le_bytes(bytes[44..46].try_into().unwrap());
        assert_eq!(first, 0);
        let clip_top = i16::from_le_bytes(bytes[50..52].try_into().unwrap());
        assert_eq!(clip_top, 32767);
        std::fs::remove_file(&path).ok();
    }
}

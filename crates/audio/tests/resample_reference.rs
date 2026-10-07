//! Compare 44.1-to-48 kHz resampling with a Chrome capture.
//! The chirp exposes high-frequency loss that a bass-only fixture would miss.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Fixture {
    id: String,
    source: Source,
    expected: Expected,
}

#[derive(Deserialize)]
struct Source {
    sample_rate_hz: u32,
    channels: u16,
    frames: usize,
    wav_base64: String,
}

#[derive(Deserialize)]
struct Expected {
    sample_rate_hz: u32,
    channels: u16,
    frames: usize,
    pcm_f32le_base64: String,
}

fn base64_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [-1i8; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        table[*byte as usize] = index as i8;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let value = table[byte as usize];
        assert!(value >= 0, "fixture base64 has a stray {byte:?}");
        accumulator = (accumulator << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    out
}

fn fixture() -> Fixture {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio-fixture-resample.json");
    let text = std::fs::read_to_string(&path).expect("read the resample fixture");
    serde_json::from_str(&text).expect("parse the resample fixture")
}

#[test]
fn the_chirp_matches_browser_resampling_and_rejects_linear_interpolation() {
    let fixture = fixture();
    let decoded = rustel_audio::decode_wav(&base64_decode(&fixture.source.wav_base64))
        .expect("decode fixture");
    assert_eq!(decoded.sample_rate(), fixture.source.sample_rate_hz);
    assert_eq!(decoded.channels(), fixture.source.channels);
    assert_eq!(decoded.frames(), fixture.source.frames);

    let converted = decoded
        .resampled_to(fixture.expected.sample_rate_hz)
        .expect("convert to the context rate");
    assert_eq!(converted.sample_rate(), fixture.expected.sample_rate_hz);
    assert_eq!(converted.channels(), fixture.expected.channels);
    assert_eq!(
        converted.frames(),
        fixture.expected.frames,
        "{}: the browser truncates the converted length, and so must this",
        fixture.id
    );

    let expected: Vec<f32> = base64_decode(&fixture.expected.pcm_f32le_base64)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();
    assert_eq!(expected.len(), converted.pcm().len());
    let worst = converted
        .pcm()
        .iter()
        .zip(&expected)
        .map(|(got, want)| (got - want).abs())
        .fold(0.0f32, f32::max);
    // Kernel precision and SIMD width cause about 1.2e-4 reference error.
    // The negative control below proves that the tolerance rejects a wrong filter.
    assert!(
        worst < 3e-4,
        "{}: diverged from the browser decode by {worst}",
        fixture.id
    );

    // The same fixture must reject linear interpolation.
    let source = decoded.pcm();
    let ratio =
        f64::from(fixture.source.sample_rate_hz) / f64::from(fixture.expected.sample_rate_hz);
    let interpolated: Vec<f32> = (0..fixture.expected.frames)
        .map(|frame| {
            let position = frame as f64 * ratio;
            let index = position as usize;
            let t = (position - index as f64) as f32;
            let a = source.get(index).copied().unwrap_or(0.0);
            let b = source.get(index + 1).copied().unwrap_or(a);
            a + (b - a) * t
        })
        .collect();
    let worst = interpolated
        .iter()
        .zip(&expected)
        .map(|(got, want)| (got - want).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst > 0.1,
        "the fixture no longer separates the two filters (linear was off by only {worst})"
    );
}

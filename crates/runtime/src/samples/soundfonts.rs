//! Soundfont zone parsing, decoding, and installation.

use std::path::Path;
use std::sync::atomic::Ordering;

use rustel_audio::{DecodedSample, SAMPLE_BANK_CAPACITY};

use super::codecs::decode_guarded;
use super::{FontZone, Shared, fetch_cached_decoded_with_budget, reserve_sample_ids};
use crate::sample_fetch;

/// Fold `a+b` / `a-b` chains of numeric literals outside quoted strings:
/// `4200-140` becomes `4060`. Everything else passes through untouched.
pub(super) fn fold_number_arithmetic(json: &str) -> String {
    let bytes = json.as_bytes();
    let mut out = String::with_capacity(json.len());
    let mut index = 0usize;
    let mut in_string = false;
    let number_at = |from: usize| -> Option<(f64, usize)> {
        let mut end = from;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        (end > from)
            .then(|| {
                json[from..end]
                    .parse::<f64>()
                    .ok()
                    .map(|value| (value, end))
            })
            .flatten()
    };
    while index < bytes.len() {
        let ch = bytes[index];
        if in_string {
            out.push(ch as char);
            if ch == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == b'"' {
            in_string = true;
            out.push('"');
            index += 1;
            continue;
        }
        // A number that a `+` or `-` and another number follow, possibly
        // more than once: sum the chain. A number on its own, or a leading
        // sign, is left to the JSON parser.
        if ch.is_ascii_digit()
            && let Some((first, mut end)) = number_at(index)
        {
            let mut total = first;
            let mut folded = false;
            loop {
                let mut probe = end;
                while probe < bytes.len() && bytes[probe] == b' ' {
                    probe += 1;
                }
                let sign = match bytes.get(probe) {
                    Some(b'+') => 1.0,
                    Some(b'-') => -1.0,
                    _ => break,
                };
                probe += 1;
                while probe < bytes.len() && bytes[probe] == b' ' {
                    probe += 1;
                }
                let Some((next, next_end)) = number_at(probe) else {
                    break;
                };
                total += sign * next;
                end = next_end;
                folded = true;
            }
            if folded {
                if total.fract() == 0.0 {
                    out.push_str(&format!("{}", total as i64));
                } else {
                    out.push_str(&format!("{total}"));
                }
            } else {
                out.push_str(&json[index..end]);
            }
            index = end;
            continue;
        }
        out.push(ch as char);
        index += 1;
    }
    out
}

struct DecodedFontZone {
    key_lo: f64,
    key_hi: f64,
    base_detune: f64,
    decoded: DecodedSample,
    duration_secs: f64,
    loop_secs: Option<(f64, f64)>,
}

/// A font's zones decoded into private staging, before any holds a bank id.
struct DecodedFont {
    zones: Vec<DecodedFontZone>,
    /// Why each zone missing from `zones` did not decode.
    skipped: Vec<String>,
}

/// Fetch one soundfont `.js` through the host cache and install its zones.
/// Bytes that [`decode_font_zones`] refuses are evicted; a font refused only
/// because the bank has no free ids for it keeps its bytes.
pub(super) fn load_font(
    dir: &Path,
    url: &str,
    shared: &Shared,
    budget: &sample_fetch::FetchBudget,
) -> Result<Vec<FontZone>, String> {
    fetch_cached_decoded_with_budget(dir, url, budget, &shared.publication, |bytes| {
        decode_font_zones(bytes, shared.render_rate.load(Ordering::Acquire))
    })
    .and_then(|font| install_font(font, shared))
}

/// What [`load_font`] does with bytes already in hand.
#[cfg(test)]
pub(super) fn decode_font(bytes: &[u8], shared: &Shared) -> Result<Vec<FontZone>, String> {
    install_font(
        decode_font_zones(bytes, shared.render_rate.load(Ordering::Acquire))?,
        shared,
    )
}

/// Parse one webaudiofontdata `<font>.js` file and decode its zones: a file
/// zone is resampled to `context_rate`, or skipped when its audio does not
/// decode, and a raw `sample` zone keeps its declared rate.
///
/// The file is `var _tone_X={\n zones:[ {midi:…, file:'<base64 mp3>'} … ]};`
/// a JS object literal, not JSON. The zones array is extracted textually,
/// keys are quoted and single-quoted strings double-quoted (base64 payloads
/// contain neither quote), then serde_json takes over. Buffers decode with
/// the same codecs as bank samples (gapless mp3 = Chrome's decodeAudioData
/// alignment, which the loop points rely on).
fn decode_font_zones(bytes: &[u8], context_rate: u32) -> Result<DecodedFont, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| format!("font is not UTF-8: {error}"))?;
    let start = text
        .find("zones:[")
        .ok_or_else(|| "font has no zones array".to_owned())?;
    let array = &text[start + "zones:".len()..];
    let end = array
        .rfind(']')
        .ok_or_else(|| "font zones array is unterminated".to_owned())?;
    let array = &array[..=end];

    // Quote bare keys; single→double quotes. Regex-free single pass.
    let mut json = String::with_capacity(array.len() + 1024);
    let bytes = array.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let ch = bytes[index];
        if ch == b'\'' {
            json.push('"');
            index += 1;
            while index < bytes.len() && bytes[index] != b'\'' {
                json.push(bytes[index] as char);
                index += 1;
            }
            json.push('"');
            index += 1;
            continue;
        }
        if ch == b'/' && bytes.get(index + 1) == Some(&b'/') {
            // Line comments annotate zones ("//_tone.Fingered_Bass_A0").
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if ch == b'{' || ch == b',' {
            json.push(ch as char);
            index += 1;
            // whitespace, then a bare identifier followed by ':' becomes a key
            let mut probe = index;
            while probe < bytes.len() && (bytes[probe] as char).is_ascii_whitespace() {
                probe += 1;
            }
            let key_start = probe;
            while probe < bytes.len()
                && ((bytes[probe] as char).is_ascii_alphanumeric() || bytes[probe] == b'_')
            {
                probe += 1;
            }
            if probe > key_start && bytes.get(probe) == Some(&b':') {
                json.push_str(&array[index..key_start]);
                json.push('"');
                json.push_str(&array[key_start..probe]);
                json.push('"');
                index = probe;
            }
            continue;
        }
        json.push(ch as char);
        index += 1;
    }

    // The literal is JavaScript, and a few fonts write a number as a sum:
    // `originalPitch:4200-140` in 0253_Acoustic_Guitar. The browser
    // evaluates the script; JSON has no arithmetic, so it is folded here -
    // outside strings only, a base64 payload is full of `+` and digits.
    let json = fold_number_arithmetic(&json);
    let zones: serde_json::Value =
        serde_json::from_str(&json).map_err(|error| format!("font zones: {error}"))?;
    let zones = zones
        .as_array()
        .ok_or_else(|| "font zones did not parse to an array".to_owned())?;
    if zones.len() >= SAMPLE_BANK_CAPACITY {
        return Err(format!(
            "font has {} zones, above the sample bank limit of {}",
            zones.len(),
            SAMPLE_BANK_CAPACITY - 1
        ));
    }

    // Decode into private staging first. A malformed late zone or a capacity
    // race must not publish an orphaned prefix of PCM with no Ready font map.
    let mut decoded_zones = Vec::with_capacity(zones.len());
    // A zone whose audio will not decode is skipped, not fatal: the
    // browser's decodeAudioData does the same and the font plays with that
    // key range silent. 0110_JCLive (the vibraphone) carries a 225-byte
    // zone that decodes to nothing and took every zone down with it.
    let mut skipped: Vec<String> = Vec::new();
    for zone in zones {
        let field = |name: &str| zone.get(name).and_then(serde_json::Value::as_f64);
        let original_pitch =
            field("originalPitch").ok_or_else(|| "zone has no originalPitch".to_owned())?;
        let key_lo = field("keyRangeLow").ok_or_else(|| "zone has no keyRangeLow".to_owned())?;
        let key_hi = field("keyRangeHigh").ok_or_else(|| "zone has no keyRangeHigh".to_owned())?;
        let coarse = field("coarseTune").unwrap_or(0.0);
        let fine = field("fineTune").unwrap_or(0.0);
        let zone_rate = field("sampleRate").unwrap_or(44_100.0).max(1.0);
        let loop_start = field("loopStart").unwrap_or(0.0);
        let loop_end = field("loopEnd").unwrap_or(0.0);

        let decoded = if let Some(file) = zone.get("file").and_then(serde_json::Value::as_str) {
            let raw = base64_decode(file)?;
            let file = if raw.starts_with(b"RIFF") {
                "zone.wav"
            } else {
                "zone.mp3"
            };
            // Decode and resample inside the same panic boundary as bank samples.
            match decode_guarded(file, &raw, Some(context_rate)) {
                Ok(decoded) => decoded,
                Err(error) => {
                    skipped.push(format!("zone {key_lo}..{key_hi}: {error}"));
                    continue;
                }
            }
        } else if let Some(sample) = zone.get("sample").and_then(serde_json::Value::as_str) {
            let raw = base64_decode(sample)?;
            let mut frames = Vec::with_capacity(raw_pcm_frame_count(
                raw.len(),
                rustel_audio::sample_pcm_ceiling(),
            )?);
            for pair in raw.as_chunks::<2>().0 {
                let n = i16::from_le_bytes([pair[0], pair[1]]);
                // WebAudioFontPlayer divides by 65536, not 32768.
                frames.push(f32::from(n) / 65_536.0);
            }
            // A zone that ships raw PCM goes through `createBuffer` at its own
            // declared rate instead, and the source node resamples it.
            DecodedSample::from_parts(zone_rate as u32, 1, frames)?
        } else {
            return Err("zone has neither file nor sample".to_owned());
        };

        let duration_secs = decoded.frames() as f64 / f64::from(decoded.sample_rate());
        // `loop = loopStart > 1 && loopStart < loopEnd`,
        // loop positions in seconds of the zone's own timeline.
        let loop_secs = (loop_start > 1.0 && loop_start < loop_end)
            .then_some((loop_start / zone_rate, loop_end / zone_rate));

        decoded_zones.push(DecodedFontZone {
            key_lo,
            key_hi,
            base_detune: original_pitch - 100.0 * coarse - fine,
            decoded,
            duration_secs,
            loop_secs,
        });
    }

    if decoded_zones.is_empty() {
        return Err(format!(
            "font has no zone that decodes ({})",
            skipped.first().map(String::as_str).unwrap_or("no zones")
        ));
    }
    Ok(DecodedFont {
        zones: decoded_zones,
        skipped,
    })
}

/// Give every zone of `font` a bank id and queue its PCM. Nothing is
/// published when the bank cannot seat the whole font.
fn install_font(font: DecodedFont, shared: &Shared) -> Result<Vec<FontZone>, String> {
    let DecodedFont {
        zones: decoded_zones,
        skipped,
    } = font;
    for message in &skipped {
        shared
            .failures
            .lock()
            .expect("sample failures")
            .push(format!("font zone skipped - {message}").into());
    }
    let ids = reserve_sample_ids(shared, decoded_zones.len())?;
    let mut out = Vec::with_capacity(decoded_zones.len());
    let mut ready = shared.ready.lock().expect("ready samples");
    for (id, zone) in ids.into_iter().zip(decoded_zones) {
        ready.push((id, zone.decoded));
        out.push(FontZone {
            key_lo: zone.key_lo,
            key_hi: zone.key_hi,
            base_detune: zone.base_detune,
            id,
            duration_secs: zone.duration_secs,
            loop_secs: zone.loop_secs,
        });
    }
    Ok(out)
}

fn raw_pcm_frame_count(raw_bytes: usize, pcm_ceiling: usize) -> Result<usize, String> {
    let frames = raw_bytes / std::mem::size_of::<i16>();
    if frames > pcm_ceiling / std::mem::size_of::<f32>() {
        return Err("decoded sample exceeds the size limit".to_owned());
    }
    Ok(frames)
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    const TABLE: [i8; 256] = {
        let mut table = [-1i8; 256];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut index = 0;
        while index < 64 {
            table[alphabet[index] as usize] = index as i8;
            index += 1;
        }
        table
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u8;
    for byte in text.bytes() {
        if byte == b'=' || (byte as char).is_ascii_whitespace() {
            continue;
        }
        let value = TABLE[byte as usize];
        if value < 0 {
            return Err(format!("invalid base64 byte {byte}"));
        }
        acc = (acc << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod raw_pcm_tests {
    use super::raw_pcm_frame_count;

    #[test]
    fn raw_pcm_output_size_is_checked_before_allocating_frames() {
        // Two input bytes become four float PCM bytes. The decode loop
        // ignores an incomplete final input byte.
        assert_eq!(raw_pcm_frame_count(6, 16).unwrap(), 3);
        assert_eq!(raw_pcm_frame_count(8, 16).unwrap(), 4);
        assert_eq!(raw_pcm_frame_count(9, 16).unwrap(), 4);
        assert_eq!(
            raw_pcm_frame_count(10, 16).unwrap_err(),
            "decoded sample exceeds the size limit"
        );
        assert!(raw_pcm_frame_count(usize::MAX, 16).is_err());
    }
}

#[cfg(test)]
mod decode_guard_tests {
    use super::super::codec_tests::{base64_encode, sample_test_shared, wav_bytes};
    use super::super::codecs::panic_hook::with_decoder_panic;
    use super::super::{FontState, LoadPriority, cache_path, run_font_loader};
    use super::*;
    use std::sync::Arc;

    fn file_font(bytes: &[u8], raw_zone: bool) -> String {
        let file = base64_encode(bytes);
        let raw = if raw_zone {
            ", {originalPitch:6000, keyRangeLow:64, keyRangeHigh:127, sampleRate:22050, sample:'AAAAAA=='}"
        } else {
            ""
        };
        format!(
            "var font={{zones:[{{originalPitch:6000, keyRangeLow:0, keyRangeHigh:63, file:'{file}'}}{raw}]}};"
        )
    }

    #[test]
    fn a_panicking_file_zone_is_skipped_and_reported() {
        for (bytes, codec) in [(wav_bytes(22_050, 220), "Wav"), (Vec::new(), "Mp3")] {
            let font = file_font(&bytes, true);
            let decoded = with_decoder_panic(|| decode_font_zones(font.as_bytes(), 48_000))
                .expect("the raw zone remains playable");
            assert_eq!(decoded.zones.len(), 1);
            assert_eq!(decoded.zones[0].key_lo, 64.0);
            assert_eq!(decoded.zones[0].decoded.sample_rate(), 22_050);
            assert_eq!(
                decoded.skipped,
                [format!(
                    "zone 0..63: {codec} decoder panicked: test decoder failure"
                )]
            );

            let shared = sample_test_shared(1);
            let installed = install_font(decoded, &shared).expect("install the surviving zone");
            assert_eq!(installed.len(), 1);
            let failures = shared.failures.lock().expect("failures");
            assert_eq!(failures.len(), 1);
            assert!(failures[0].message.contains("font zone skipped"));
            assert!(failures[0].message.contains("decoder panicked"));
        }
    }

    #[test]
    fn a_failed_font_does_not_stall_the_same_loader() {
        let cache = tempfile::tempdir().expect("font cache");
        let base = "https://fonts.example.test";
        let shared = Arc::new(sample_test_shared(1));
        shared.render_rate.store(48_000, Ordering::Release);
        let font = file_font(&wav_bytes(22_050, 220), false);
        for name in ["first", "following"] {
            std::fs::write(
                cache_path(cache.path(), &format!("{base}/{name}.js")),
                &font,
            )
            .expect("cached font");
            shared
                .fonts
                .write()
                .expect("fonts")
                .insert(Arc::from(name), FontState::Loading);
            shared.font_jobs.push(Arc::from(name), LoadPriority::Now);
        }
        shared.font_jobs.close();

        let worker_shared = Arc::downgrade(&shared);
        let jobs = Arc::clone(&shared.font_jobs);
        let directory = cache.path().to_path_buf();
        std::thread::spawn(move || {
            with_decoder_panic(|| run_font_loader(worker_shared, jobs, directory, base.to_owned()));
        })
        .join()
        .expect("the same loader survives the first decoder panic");

        let fonts = shared.fonts.read().expect("fonts");
        assert!(matches!(fonts.get("first"), Some(FontState::Failed { .. })));
        let Some(FontState::Ready(zones)) = fonts.get("following") else {
            panic!("the following font must finish loading");
        };
        assert_eq!(zones.len(), 1);
        assert_eq!(shared.settled_epoch.load(Ordering::Acquire), 2);
        let ready = shared.ready.lock().expect("ready samples");
        assert_eq!(ready.samples.len(), 1);
        assert_eq!(ready.samples[0].0, zones[0].id);
        assert_eq!(ready.samples[0].1.sample_rate(), 48_000);
        assert!(ready.samples[0].1.frames() > 220);

        let failures = shared.failures.lock().expect("failures");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].message.contains("first.js"));
        assert!(failures[0].message.contains("decoder panicked"));
        assert!(!cache_path(cache.path(), &format!("{base}/first.js")).exists());
        assert!(cache_path(cache.path(), &format!("{base}/following.js")).exists());
    }
}

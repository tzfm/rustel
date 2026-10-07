//! Offline interleaved stereo PCM render helper.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::backend::{AudioBackend, OnsetEvent};

/// Render `frames` of stereo interleaved `f32` PCM at `sample_rate` using
/// `backend` and the supplied onset list.
pub fn render_pcm(
    backend: &mut dyn AudioBackend,
    sample_rate: u32,
    frames: usize,
    events: &[OnsetEvent],
) -> Result<Vec<f32>, String> {
    backend.init(sample_rate)?;
    backend.reset();
    for e in events {
        backend.note(*e);
    }
    let mut pcm = vec![0.0f32; frames * 2];
    // Process in 128-frame blocks (Dough's native block size) for realism.
    const BLOCK: usize = 128;
    let mut offset = 0;
    while offset < frames {
        let n = (frames - offset).min(BLOCK);
        let start = offset * 2;
        let end = start + n * 2;
        backend.process_block(&mut pcm[start..end], n);
        offset += n;
    }
    Ok(pcm)
}

/// Stream a deterministic 16-bit stereo WAV without holding its PCM body in
/// memory. The backend is processed in 128-frame blocks.
pub fn write_pcm16_wav(
    path: impl AsRef<Path>,
    backend: &mut dyn AudioBackend,
    sample_rate: u32,
    frames: usize,
    events: &[OnsetEvent],
) -> Result<usize, String> {
    write_pcm16_wav_reporting(path, backend, sample_rate, frames, events, None, None, None)
}

/// The error a cancelled render returns, so a caller can tell "the artist
/// stopped this" from "the render failed".
pub const RENDER_CANCELLED: &str = "render cancelled";

/// Stop a bounce when the music has finished, rather than when the clock runs
/// out.
///
/// Most scores are not `arrange`d, so their length is not written down
/// anywhere - the only way to know a set has ended is that it went quiet and
/// stayed quiet. `hold_frames` is what separates "ended" from "a bar of
/// silence the artist wrote on purpose", so it is a separate dial from the
/// threshold and defaults generously.
#[derive(Clone, Copy, Debug)]
pub struct SilenceStop {
    /// Peak below this counts as silence, in linear amplitude.
    pub floor: f32,
    /// How long it has to stay there before the render stops.
    pub hold_frames: usize,
    /// Silence before this frame is the music's own - a rest, a breakdown -
    /// and never ends the render: the length asked for is played out,
    /// and only the tail after it is cut when it has faded. Zero listens
    /// from the first sound, the way a bounce with no length does.
    pub after_frames: usize,
}

/// How a rendered sample is stored in the WAV.
///
/// The engine computes in `f32` throughout; this selects only the stored
/// format. 16-bit PCM is the export format, but it clamps at ±1.0. A
/// browser's `OfflineAudioContext` returns unclamped floats, so a parity
/// comparison against a 16-bit file measures the file format and not the
/// engine. A hard-panned drum lost 2.15 dB to this clamping alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavSampleFormat {
    /// Signed 16-bit PCM. The export format; clamps at full scale.
    Pcm16,
    /// 32-bit IEEE float, `WAVE_FORMAT_IEEE_FLOAT`. Stores what the engine
    /// actually computed, over 0 dBFS included.
    Float32,
}

impl WavSampleFormat {
    const fn bytes_per_sample(self) -> u16 {
        match self {
            Self::Pcm16 => 2,
            Self::Float32 => 4,
        }
    }

    /// The `wFormatTag` a RIFF `fmt ` chunk carries.
    const fn wave_format_tag(self) -> u16 {
        match self {
            Self::Pcm16 => 1,
            Self::Float32 => 3,
        }
    }

    const fn bits(self) -> u16 {
        self.bytes_per_sample() * 8
    }
}

/// 16-bit PCM, the long-standing behaviour. See [`write_wav_reporting`].
#[allow(clippy::too_many_arguments)]
pub fn write_pcm16_wav_reporting(
    path: impl AsRef<Path>,
    backend: &mut dyn AudioBackend,
    sample_rate: u32,
    frames: usize,
    events: &[OnsetEvent],
    progress: Option<&mut dyn FnMut(usize, usize)>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
    stop_when_silent: Option<SilenceStop>,
) -> Result<usize, String> {
    write_wav_reporting(
        path,
        backend,
        sample_rate,
        frames,
        events,
        progress,
        cancelled,
        stop_when_silent,
        WavSampleFormat::Pcm16,
    )
}

/// [`write_wav_controlled`] with a progress callback and a cancel flag. The
/// callback receives `(frames written, frames total)` about every two
/// seconds. A cancelled render finalises the partial file and returns
/// [`RENDER_CANCELLED`].
#[allow(clippy::too_many_arguments)]
pub fn write_wav_reporting(
    path: impl AsRef<Path>,
    backend: &mut dyn AudioBackend,
    sample_rate: u32,
    frames: usize,
    events: &[OnsetEvent],
    mut progress: Option<&mut dyn FnMut(usize, usize)>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
    stop_when_silent: Option<SilenceStop>,
    format: WavSampleFormat,
) -> Result<usize, String> {
    let mut spoke_at = std::time::Instant::now();
    let mut observer = |tick: RenderTick<'_>| {
        if spoke_at.elapsed() >= std::time::Duration::from_secs(2) {
            spoke_at = std::time::Instant::now();
            if let Some(report) = progress.as_deref_mut() {
                report(tick.frames_written, tick.frames_total);
            }
        }
    };
    write_wav_controlled(
        path,
        backend,
        sample_rate,
        frames,
        events,
        RenderControl {
            observer: Some(&mut observer),
            cancelled,
            finish: None,
            stop_when_silent,
            limiter: None,
        },
        format,
    )
}

/// One block of a render, as it is written: for a progress bar, a scope.
#[derive(Clone, Copy, Debug)]
pub struct RenderTick<'a> {
    /// Frames on disk once this block is, this block included.
    pub frames_written: usize,
    pub frames_total: usize,
    /// The block, interleaved stereo, as it goes to the file.
    pub block: &'a [f32],
}

/// Master limiting for a file export, using the same processor as live output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderLimiter {
    pub settings: crate::LimiterSettings,
    pub makeup: bool,
}

/// How a render is watched, processed and ended.
#[derive(Default)]
pub struct RenderControl<'o, 'f> {
    /// Called for every block written.
    pub observer: Option<&'o mut (dyn FnMut(RenderTick<'_>) + 'o)>,
    /// Abandon: the partial file is finalised and [`RENDER_CANCELLED`]
    /// is returned.
    pub cancelled: Option<&'f std::sync::atomic::AtomicBool>,
    /// End early and keep the file: the next tenth of a second fades to
    /// silence so the cut does not click, then the file is finalised and
    /// returned as a success - the artist decided the tail was long enough.
    pub finish: Option<&'f std::sync::atomic::AtomicBool>,
    pub stop_when_silent: Option<SilenceStop>,
    /// Applied before observing, detecting silence and encoding. Its lookahead
    /// is compensated so the file keeps its original timing and length.
    pub limiter: Option<RenderLimiter>,
}

/// [`write_pcm16_wav`] under a [`RenderControl`].
pub fn write_pcm16_wav_controlled(
    path: impl AsRef<Path>,
    backend: &mut dyn AudioBackend,
    sample_rate: u32,
    frames: usize,
    events: &[OnsetEvent],
    control: RenderControl<'_, '_>,
) -> Result<usize, String> {
    write_wav_controlled(
        path,
        backend,
        sample_rate,
        frames,
        events,
        control,
        WavSampleFormat::Pcm16,
    )
}

/// The render loop everything above funnels into: a [`RenderControl`] decides
/// how it is watched and ended, a [`WavSampleFormat`] decides what lands in
/// the file.
pub fn write_wav_controlled(
    path: impl AsRef<Path>,
    backend: &mut dyn AudioBackend,
    sample_rate: u32,
    frames: usize,
    events: &[OnsetEvent],
    control: RenderControl<'_, '_>,
    format: WavSampleFormat,
) -> Result<usize, String> {
    const CHANNELS: u16 = 2;
    const BLOCK: usize = 128;
    let RenderControl {
        mut observer,
        cancelled,
        finish,
        stop_when_silent,
        limiter,
    } = control;
    let mut limiter = limiter.map(|config| {
        let processor = crate::Limiter::new(
            sample_rate,
            config.settings.threshold_db,
            config.settings.character,
        );
        let makeup = if config.makeup {
            crate::meter::db_to_linear(processor.threshold_db()).recip()
        } else {
            1.0
        };
        (processor, makeup)
    });
    let latency = limiter
        .as_ref()
        .map_or(0, |(processor, _)| processor.latency_frames());
    let mut skip = latency;
    // A tenth of a second: long enough not to click, short enough that a
    // stop is a stop.
    let fade_frames = (sample_rate as usize / 10).max(1);
    let mut fade_from: Option<usize> = None;
    let bytes_per_sample = format.bytes_per_sample();

    let data_bytes = (frames as u64)
        .checked_mul(u64::from(CHANNELS))
        .and_then(|value| value.checked_mul(u64::from(bytes_per_sample)))
        .ok_or_else(|| "PCM byte count overflowed".to_string())?;
    if data_bytes + 36 > u64::from(u32::MAX) {
        return Err(format!(
            "{frames} frames need {data_bytes} PCM bytes, which a RIFF file cannot address"
        ));
    }
    let data_bytes = data_bytes as u32;
    let block_align = CHANNELS * bytes_per_sample;
    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| "WAV byte rate overflowed".to_string())?;

    backend.init(sample_rate)?;
    // Feed events as the render reaches them, not all at once. The backend's
    // pending queue is bounded (MAX_PENDING_EVENTS) because a live producer
    // must not grow it without limit. An offline render that queues every
    // onset up front exceeds that bound, and the backend drops the rest
    // without an error.
    let mut queued: Vec<crate::OnsetEvent> = events.to_vec();
    queued.sort_by_key(|event| event.onset_frame);
    let mut next_event = 0usize;

    let mut file = File::create(path).map_err(|error| error.to_string())?;
    file.write_all(b"RIFF").map_err(|error| error.to_string())?;
    file.write_all(&(36 + data_bytes).to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(b"WAVEfmt ")
        .map_err(|error| error.to_string())?;
    file.write_all(&16u32.to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(&format.wave_format_tag().to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(&CHANNELS.to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(&sample_rate.to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(&byte_rate.to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(&block_align.to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(&format.bits().to_le_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(b"data").map_err(|error| error.to_string())?;
    file.write_all(&data_bytes.to_le_bytes())
        .map_err(|error| error.to_string())?;

    // Silence only ends a render once the music has actually started: a score
    // that fades in, or opens on a rest, would otherwise stop before its first
    // note.
    let mut heard_sound = false;
    let mut silent_frames = 0usize;
    let mut pcm = [0.0f32; BLOCK * CHANNELS as usize];
    let mut encoded = [0u8; BLOCK * CHANNELS as usize * 4];
    let mut offset = 0usize;
    let mut input_offset = 0usize;
    while offset < frames {
        // Checked per block, not per progress tick: a 2 s reporting interval
        // would make Ctrl-C feel ignored for exactly as long.
        if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            finalise_partial_wav(&mut file, offset, CHANNELS, bytes_per_sample)?;
            return Err(RENDER_CANCELLED.to_owned());
        }
        let input_count = (frames - offset + skip).min(BLOCK);
        let render_count = input_count.min(frames.saturating_sub(input_offset));
        // Admit one scheduler lead plus one block so nothing is handed to the
        // backend late. Most sources remain pending until their onset. SBD
        // intentionally becomes active during this window: its connected
        // WaveShaper has a tiny zero-input output before the oscillator starts,
        // and handing it over only one block early erases that onset prehistory
        // from streamed WAVs.
        let scheduler_lookahead = (sample_rate as usize / 10).max(1);
        let admit_before = input_offset
            .saturating_add(scheduler_lookahead)
            .saturating_add(2 * BLOCK) as u64;
        while next_event < queued.len() && queued[next_event].onset_frame < admit_before {
            backend.note(queued[next_event]);
            next_event += 1;
        }
        if render_count > 0 {
            backend.process_block(&mut pcm[..render_count * CHANNELS as usize], render_count);
        }
        // Flush delayed frames with silence, without rendering extra music
        // beyond the requested end. The backend still sees its usual blocks.
        pcm[render_count * CHANNELS as usize..input_count * CHANNELS as usize].fill(0.0);
        input_offset += input_count;
        if let Some((processor, makeup)) = &mut limiter {
            processor.process_stereo(&mut pcm[..input_count * CHANNELS as usize]);
            for sample in &mut pcm[..input_count * CHANNELS as usize] {
                *sample *= *makeup;
            }
        }
        let discarded = skip.min(input_count);
        skip -= discarded;
        let count = input_count - discarded;
        if count == 0 {
            continue;
        }
        pcm.copy_within(
            discarded * CHANNELS as usize..input_count * CHANNELS as usize,
            0,
        );
        if fade_from.is_none()
            && finish.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            fade_from = Some(offset);
        }
        let mut finished_at: Option<usize> = None;
        if let Some(from) = fade_from {
            for frame in 0..count {
                let position = (offset + frame).saturating_sub(from);
                let gain = 1.0 - (position as f32 / fade_frames as f32).min(1.0);
                pcm[frame * 2] *= gain;
                pcm[frame * 2 + 1] *= gain;
                if position + 1 >= fade_frames && finished_at.is_none() {
                    finished_at = Some(offset + frame + 1);
                }
            }
        }
        if let Some(stop) = stop_when_silent {
            let peak = pcm[..count * CHANNELS as usize]
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            if peak >= stop.floor {
                heard_sound = true;
                silent_frames = 0;
            } else if offset + count <= stop.after_frames {
                // A rest inside the length is the music's; nothing to end.
                silent_frames = 0;
            } else if heard_sound {
                silent_frames += count;
                if silent_frames >= stop.hold_frames {
                    // End the file where the silence began, not after the hold
                    // interval used to confirm it.
                    let keep = (offset + count).saturating_sub(silent_frames);
                    finalise_partial_wav(&mut file, keep, CHANNELS, bytes_per_sample)?;
                    return Ok(keep * CHANNELS as usize * bytes_per_sample as usize);
                }
            }
        }
        // The writer owns the only loop that knows how far along it is, so
        // it is the only place that can say.
        if let Some(observer) = observer.as_deref_mut() {
            observer(RenderTick {
                frames_written: offset + count,
                frames_total: frames,
                block: &pcm[..count * CHANNELS as usize],
            });
        }
        let width = usize::from(bytes_per_sample);
        for (sample, bytes) in pcm[..count * CHANNELS as usize]
            .iter()
            .zip(encoded.chunks_exact_mut(width))
        {
            match format {
                WavSampleFormat::Pcm16 => {
                    let quantized = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
                    bytes.copy_from_slice(&quantized.to_le_bytes());
                }
                // Deliberately NOT clamped: storing what the engine computed
                // is the whole point of this format.
                WavSampleFormat::Float32 => bytes.copy_from_slice(&sample.to_le_bytes()),
            }
        }
        file.write_all(&encoded[..count * CHANNELS as usize * width])
            .map_err(|error| error.to_string())?;
        offset += count;
        if let Some(keep) = finished_at {
            // The fade has run its course: the file ends here, a success.
            finalise_partial_wav(&mut file, keep, CHANNELS, bytes_per_sample)?;
            return Ok(keep * CHANNELS as usize * bytes_per_sample as usize);
        }
    }
    Ok(data_bytes as usize)
}

/// Rewrite the two RIFF length fields to describe what was actually written.
///
/// The header is written up front from the requested duration, so a render
/// that stops early leaves a file claiming more audio than it holds - players
/// read past the end and produce noise, or refuse the file outright. Patching
/// both lengths turns an interrupted bounce into a shorter, valid one.
fn finalise_partial_wav(
    file: &mut File,
    frames_written: usize,
    channels: u16,
    bytes_per_sample: u16,
) -> Result<(), String> {
    use std::io::{Seek, SeekFrom};

    let data_bytes = (frames_written as u64)
        .saturating_mul(u64::from(channels))
        .saturating_mul(u64::from(bytes_per_sample))
        .min(u64::from(u32::MAX) - 36) as u32;
    file.flush().map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(4))
        .map_err(|error| error.to_string())?;
    file.write_all(&(36 + data_bytes).to_le_bytes())
        .map_err(|error| error.to_string())?;
    // 4 magic + 4 size + 8 "WAVEfmt " + 4 fmt size + 16 fmt + 4 "data".
    file.seek(SeekFrom::Start(40))
        .map_err(|error| error.to_string())?;
    file.write_all(&data_bytes.to_le_bytes())
        .map_err(|error| error.to_string())?;
    // Truncate rather than leave the frames beyond the header: a player that
    // trusts the file length instead of the chunk size would otherwise replay
    // whatever was already on disk.
    file.set_len(u64::from(data_bytes) + 44)
        .map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod silence_stop_tests {
    use super::*;
    use crate::backend::AudioBackend;

    /// A backend that plays a tone for a while, rests, plays again, then stops
    /// for good - the shape of a set with a rest in the middle.
    struct Scripted {
        frame: usize,
        rate: u32,
        /// (from_secs, to_secs) spans that make sound.
        loud: Vec<(f64, f64)>,
    }

    impl AudioBackend for Scripted {
        fn name(&self) -> &'static str {
            "scripted-test"
        }
        fn init(&mut self, sample_rate: u32) -> Result<(), String> {
            self.rate = sample_rate;
            Ok(())
        }
        fn reset(&mut self) {
            self.frame = 0;
        }
        fn note(&mut self, _event: OnsetEvent) {}
        fn process_block(&mut self, out: &mut [f32], frames: usize) {
            for frame in 0..frames {
                let t = (self.frame + frame) as f64 / f64::from(self.rate);
                let playing = self.loud.iter().any(|(from, to)| t >= *from && t < *to);
                let value = if playing { 0.5 } else { 0.0 };
                out[frame * 2] = value;
                out[frame * 2 + 1] = value;
            }
            self.frame += frames;
        }
    }

    fn rendered_seconds(path: &std::path::Path, rate: u32) -> f64 {
        let bytes = std::fs::read(path).expect("read wav");
        let data = u32::from_le_bytes(bytes[40..44].try_into().expect("data size"));
        assert_eq!(
            data as usize,
            bytes.len() - 44,
            "header must describe the file it is in"
        );
        f64::from(data) / f64::from(rate * 4)
    }

    #[test]
    fn a_bounce_ends_where_the_music_does_not_where_the_clock_does() {
        let rate = 48_000;
        let dir = std::env::temp_dir().join(format!("rustel-silence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("stop.wav");

        // Sound for 4 s, a 2 s rest, sound again for 4 s, then nothing.
        let mut backend = Scripted {
            frame: 0,
            rate,
            loud: vec![(0.0, 4.0), (6.0, 10.0)],
        };
        // A hold longer than the rest must carry through it to the real end.
        write_pcm16_wav_reporting(
            &path,
            &mut backend,
            rate,
            rate as usize * 60,
            &[],
            None,
            None,
            Some(SilenceStop {
                floor: 0.001,
                hold_frames: rate as usize * 3,
                after_frames: 0,
            }),
        )
        .expect("render");
        let seconds = rendered_seconds(&path, rate);
        assert!(
            (seconds - 10.0).abs() < 0.05,
            "should end at the last sound (10 s), got {seconds}"
        );

        // A hold shorter than the rest stops at the rest.
        backend.reset();
        write_pcm16_wav_reporting(
            &path,
            &mut backend,
            rate,
            rate as usize * 60,
            &[],
            None,
            None,
            Some(SilenceStop {
                floor: 0.001,
                hold_frames: rate as usize,
                after_frames: 0,
            }),
        )
        .expect("render");
        let seconds = rendered_seconds(&path, rate);
        assert!(
            (seconds - 4.0).abs() < 0.05,
            "a one-second hold should stop at the two-second rest, got {seconds}"
        );

        // A score that opens on silence must not end before it begins.
        let mut late = Scripted {
            frame: 0,
            rate,
            loud: vec![(3.0, 5.0)],
        };
        write_pcm16_wav_reporting(
            &path,
            &mut late,
            rate,
            rate as usize * 30,
            &[],
            None,
            None,
            Some(SilenceStop {
                floor: 0.001,
                hold_frames: rate as usize,
                after_frames: 0,
            }),
        )
        .expect("render");
        let seconds = rendered_seconds(&path, rate);
        assert!(
            (seconds - 5.0).abs() < 0.05,
            "leading silence must not end the bounce, got {seconds}"
        );

        // A length with a tail: the two-second rest inside the eight
        // seconds asked for is the music's own and never ends the bounce,
        // however short the hold; the silence after the last sound does.
        backend.reset();
        write_pcm16_wav_reporting(
            &path,
            &mut backend,
            rate,
            rate as usize * 60,
            &[],
            None,
            None,
            Some(SilenceStop {
                floor: 0.001,
                hold_frames: rate as usize,
                after_frames: rate as usize * 8,
            }),
        )
        .expect("render");
        let seconds = rendered_seconds(&path, rate);
        assert!(
            (seconds - 10.0).abs() < 0.05,
            "a rest inside the length is not the end; the tail after it is, got {seconds}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod finish_tests {
    use super::*;

    #[test]
    fn finishing_early_fades_a_tenth_of_a_second_and_keeps_the_file() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = std::env::temp_dir().join(format!("rustel-finish-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("early.wav");
        let mut backend = crate::ScalarBackend::new();
        let finish = AtomicBool::new(false);
        let mut ticks = 0usize;
        let mut observer = |tick: RenderTick<'_>| {
            ticks += 1;
            assert_eq!(tick.frames_total, 48_000);
            if tick.frames_written >= 12_000 {
                finish.store(true, Ordering::Relaxed);
            }
        };
        let bytes = write_pcm16_wav_controlled(
            &path,
            &mut backend,
            48_000,
            48_000,
            &[],
            RenderControl {
                observer: Some(&mut observer),
                cancelled: None,
                finish: Some(&finish),
                stop_when_silent: None,
                limiter: None,
            },
        )
        .unwrap();
        // Asked to finish at 12 000 frames (plus the block that saw it), it
        // fades for 4 800 more and stops: well short of the second asked for.
        let frames = bytes / 4;
        assert!((12_000..12_000 + 4_800 + 256).contains(&frames), "{frames}");
        assert!(ticks > 90, "{ticks} blocks observed");
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), 44 + bytes, "header patched to what is there");
        assert_eq!(
            u32::from_le_bytes(file[40..44].try_into().unwrap()) as usize,
            bytes
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod export_limiter_tests {
    use super::*;

    struct Signal {
        pcm: Vec<f32>,
        frames: usize,
    }

    impl AudioBackend for Signal {
        fn name(&self) -> &'static str {
            "export-limiter-test"
        }
        fn init(&mut self, _: u32) -> Result<(), String> {
            Ok(())
        }
        fn reset(&mut self) {
            self.frames = 0;
        }
        fn note(&mut self, _: OnsetEvent) {}
        fn process_block(&mut self, out: &mut [f32], frames: usize) {
            out.copy_from_slice(&self.pcm[self.frames * 2..(self.frames + frames) * 2]);
            self.frames += frames;
        }
    }

    #[test]
    fn limiting_preserves_the_first_and_last_frames_and_the_requested_length() {
        let dir =
            std::env::temp_dir().join(format!("rustel-export-latency-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bounce.wav");
        for character in crate::LimiterCharacter::ALL {
            for frames in [0, 1, 17, 127, 128, 129, 1001] {
                let expected: Vec<f32> = (0..frames * 2)
                    .map(|n| (n % 23 + 1) as f32 / 1000.0)
                    .collect();
                let mut backend = Signal {
                    pcm: expected.clone(),
                    frames: 0,
                };
                let mut observed = Vec::new();
                let mut observe = |tick: RenderTick<'_>| observed.extend_from_slice(tick.block);
                let bytes = write_wav_controlled(
                    &path,
                    &mut backend,
                    48_000,
                    frames,
                    &[],
                    RenderControl {
                        observer: Some(&mut observe),
                        limiter: Some(RenderLimiter {
                            settings: crate::LimiterSettings {
                                threshold_db: -12.0,
                                character,
                            },
                            makeup: false,
                        }),
                        ..RenderControl::default()
                    },
                    WavSampleFormat::Float32,
                )
                .unwrap();
                assert_eq!(backend.frames, frames, "no extra music rendered");
                assert_eq!(bytes, frames * 8);
                let wav = std::fs::read(&path).unwrap();
                assert_eq!(wav.len(), 44 + bytes);
                let actual: Vec<f32> = wav[44..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|sample| f32::from_le_bytes(*sample))
                    .collect();
                assert_eq!(actual, expected, "{character:?}, {frames} frames");
                assert_eq!(observed, actual, "the scope sees what is written");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod scheduler_lookahead_tests {
    use super::*;

    #[derive(Default)]
    struct AdmissionProbe {
        frame: usize,
        admissions: Vec<(u64, usize)>,
    }

    impl crate::AudioBackend for AdmissionProbe {
        fn name(&self) -> &'static str {
            "admission-probe"
        }

        fn init(&mut self, _sample_rate: u32) -> Result<(), String> {
            Ok(())
        }

        fn reset(&mut self) {
            self.frame = 0;
            self.admissions.clear();
        }

        fn note(&mut self, event: OnsetEvent) {
            self.admissions.push((event.onset_frame, self.frame));
        }

        fn process_block(&mut self, out: &mut [f32], frames: usize) {
            out.fill(0.0);
            self.frame += frames;
        }
    }

    #[test]
    fn streamed_renders_admit_events_at_least_one_scheduler_lead_early() {
        const SAMPLE_RATE: u32 = 48_000;
        let onset = u64::from(SAMPLE_RATE);
        let event = OnsetEvent::new(onset, 440.0, 1.0, 0.1);
        let dir =
            std::env::temp_dir().join(format!("rustel-render-lookahead-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("probe.wav");
        let mut backend = AdmissionProbe::default();

        write_pcm16_wav(
            &path,
            &mut backend,
            SAMPLE_RATE,
            SAMPLE_RATE as usize + 128,
            &[event],
        )
        .expect("render");

        assert_eq!(backend.admissions.len(), 1);
        let (_, admitted_at) = backend.admissions[0];
        assert!(
            admitted_at + SAMPLE_RATE as usize / 10 <= onset as usize,
            "event at {onset} was admitted at {admitted_at}, less than 100 ms early"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

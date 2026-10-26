//! Playback of rendered speech through the default output device.
//!
//! The rendered buffer is mono at [`crate::RATE`]. Devices run at their own
//! rate and channel count, so the buffer is linearly resampled and duplicated
//! to the front pair before the stream opens. Playback blocks until the whole
//! buffer has gone out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

/// Play `samples` at `rate` and return when they have all sounded.
pub fn play(samples: &[f32], rate: u32) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no audio output device".to_string())?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let device_rate = config.sample_rate();
    let channels = usize::from(config.channels()).max(1);

    let interleaved = spread(resample(samples, rate, device_rate), channels);
    let cursor = Arc::new(Mutex::new(Cursor {
        data: interleaved,
        pos: 0,
    }));
    let failed = Arc::new((AtomicBool::new(false), Mutex::new(String::new())));

    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            typed_stream::<f32>(&device, config.into(), &cursor, channels, &failed)
        }
        SampleFormat::I16 => {
            typed_stream::<i16>(&device, config.into(), &cursor, channels, &failed)
        }
        SampleFormat::U16 => {
            typed_stream::<u16>(&device, config.into(), &cursor, channels, &failed)
        }
        format => return Err(format!("output sample format {format} is not supported")),
    }?;
    stream.play().map_err(|e| e.to_string())?;

    loop {
        if failed.0.load(Ordering::Acquire) {
            return Err(failed.1.lock().expect("the lock is not poisoned").clone());
        }
        let done = {
            let c = cursor.lock().expect("the lock is not poisoned");
            c.pos >= c.data.len()
        };
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// Where in the interleaved buffer the callback has read to.
struct Cursor {
    data: Vec<f32>,
    pos: usize,
}

fn typed_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    cursor: &Arc<Mutex<Cursor>>,
    channels: usize,
    failed: &Arc<(AtomicBool, Mutex<String>)>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let cursor = cursor.clone();
    let failed = failed.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut c = cursor.lock().expect("the lock is not poisoned");
                for frame in data.chunks_mut(channels) {
                    for (ch, slot) in frame.iter_mut().enumerate() {
                        let audible = ch < 2;
                        let sample = if audible && c.pos < c.data.len() {
                            c.data[c.pos]
                        } else {
                            0.0
                        };
                        *slot = T::from_sample(sample);
                    }
                    // The whole buffer is interleaved at the device's own
                    // channel count, so every frame consumes that many
                    // samples even when only the front pair sounds.
                    c.pos = (c.pos + channels).min(c.data.len());
                }
            },
            move |err| {
                *failed.1.lock().expect("the lock is not poisoned") = err.to_string();
                failed.0.store(true, Ordering::Release);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

/// Linear resample of a mono buffer between rates.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let step = from as f64 / to as f64;
    let out_len = (samples.len() as f64 / step).ceil() as usize;
    (0..out_len)
        .map(|i| {
            let t = i as f64 * step;
            let i0 = t.floor() as usize;
            let i1 = (i0 + 1).min(samples.len() - 1);
            let f = (t - i0 as f64) as f32;
            samples[i0] * (1.0 - f) + samples[i1] * f
        })
        .collect()
}

/// Duplicate a mono buffer to the device layout: front pair sounds, any
/// further channels stay silent.
fn spread(samples: Vec<f32>, channels: usize) -> Vec<f32> {
    if channels == 1 {
        return samples;
    }
    let mut out = Vec::with_capacity(samples.len() * channels);
    for s in samples {
        for ch in 0..channels {
            out.push(if ch < 2 { s } else { 0.0 });
        }
    }
    out
}

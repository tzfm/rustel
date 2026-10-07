//! Embed Rustel's pattern engine and DSP in a host application.
//!
//! The default build contains native patterns, mini notation, the scheduler,
//! voice resolution, and audio DSP. The host owns its UI, audio device, clock,
//! and sample loading policy.
//!
//! ```
//! use rustel_engine::{fraction::Fraction, mini};
//!
//! let pattern = mini::mini("c4 e4 g4 c5").unwrap();
//! let haps = pattern.query_arc(Fraction::ZERO, Fraction::ONE);
//! assert_eq!(haps.len(), 4);
//! ```
//!
//! Run `cargo run -p rustel-engine --example native_pattern` for a complete
//! pattern → scheduler → voice → PCM example. With the `session` feature,
//! `--example session_callback --features session` shows a host-owned audio
//! callback consuming events prepared on a separate Session worker.
//!
//! # Features
//!
//! All features are opt-in. `javascript` adds `jsruntime` and `transpiler`
//! for score evaluation. `session` adds the higher-level `Session`
//! host and its sample loading, HTTP/TLS, decoding, and file-rendering support.
//! `extensions` enables the additional score extensions.
//!
//! `device-audio` adds Rustel's CPAL device adapter; the DSP and audio event
//! ring are available without it. `opus` and `mp3-export` enable their
//! Session capabilities and imply `session`.
//! No engine feature enables the command-line interface or terminal studio.
//!
//! Evaluate and query patterns, resolve voices, load assets, and prepare DSP
//! off the audio callback. [`audio::LiveScalarBackend`] and [`audio::Ring`]
//! provide the prepared consumer boundary. The first threads to push and to
//! pop own the ring's producer and consumer roles until the unsafe
//! `Ring::release_producer` or `Ring::release_consumer` hands them on; a
//! host whose callback can move to a new thread calls `release_consumer`
//! only once the old callback has provably stopped. A Session worker needs
//! `QUERY_WORKER_STACK_BYTES` of stack; construct and retain the Session on
//! that worker because its JavaScript runtime is thread-affine.
//!
//! The engine writes nothing to stderr unless the host opts in: a Session
//! collects diagnostics, including `.log()` lines and the voice resolver's
//! notices, for `Session::take_diagnostics`;
//! [`voice::with_diagnostic_policy`] returns the resolver's notices to a host
//! without a Session; and [`voice::set_default_direct_diagnostic_logging`]
//! turns direct JSON lines on for the process. With `device-audio` on Linux,
//! libasound, which the ALSA device host loads, prints its own error lines to
//! stderr; the engine leaves them there.
//!
//! A host-owned callback plays synth voices and the bundled `bd` sample
//! only: `LiveScalarBackend` has no public API to install sample PCM or
//! reverb impulse responses, so other samples are dropped and `.room()`
//! plays dry, without a diagnostic. Scores that need them play through
//! `device-audio` (`Session::play_on_device`) or render offline with
//! `Session::render_pcm` after `Session::enable_default_samples`, whose
//! library fetches sample files over the network when a score first uses
//! them.
//!
//! These modules re-export the existing component APIs and types directly.
//! With `session`, the crate root re-exports `Session`, its configuration,
//! errors and diagnostics, and its play and render reports. Other types that
//! `Session` methods take or return stay in `rustel-runtime`; a host that
//! names one depends on it with `default-features = false`. Cargo features
//! are additive: another dependency on `rustel-runtime` with its default
//! features can also enable those features in the final build.

pub use rustel_audio as audio;
pub use rustel_core as core;
pub use rustel_fraction as fraction;
pub use rustel_mini as mini;
pub use rustel_scheduler as scheduler;
pub use rustel_voice as voice;

#[cfg(feature = "javascript")]
pub use rustel_jsruntime as jsruntime;
#[cfg(feature = "javascript")]
pub use rustel_transpiler as transpiler;

#[cfg(feature = "session")]
pub use rustel_runtime::{
    PlayReport, QUERY_WORKER_STACK_BYTES, RenderFormat, RenderReport, RuntimeError,
    ScoreSampleAccess, Session, SessionConfig, SessionDiagnostic, VOICE_NOTICE_DIAGNOSTIC,
};

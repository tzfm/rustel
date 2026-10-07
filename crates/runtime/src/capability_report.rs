//! Versioned capability and runtime-fact projection shared by product views.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::{AudioStreamFacts, CapabilityRegistry, product};

/// Schema version emitted by `doctor --json`.
pub const CAPABILITY_REPORT_SCHEMA_VERSION: u32 = 1;

/// Build identity of the running binary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityBuildV1 {
    pub name: String,
    pub version: String,
    pub revision: Option<String>,
    pub target: String,
    pub profile: String,
    /// Cargo features the binary was built with, as its context names them.
    pub features: Vec<String>,
}

/// One CPU feature relevant to the running architecture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityCpuFeatureV1 {
    pub id: String,
    pub detected: bool,
}

/// CPU facts captured once by the process-wide registry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityCpuV1 {
    pub architecture: String,
    pub features: Vec<CapabilityCpuFeatureV1>,
}

/// Runtime tier and worker selection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityRuntimeV1 {
    pub acceleration_preference: String,
    pub native_query_tier: String,
    pub callback_compatibility_tier: String,
    pub query_workers: usize,
    pub available_parallelism: usize,
}

/// Native audio-host identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityAudioHostV1 {
    pub kind: String,
    pub id: Option<String>,
}

/// Opened input-stream facts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityAudioInputV1 {
    pub device_id: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_format: String,
}

/// Opened output and optional input facts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityAudioV1 {
    pub host: CapabilityAudioHostV1,
    pub output_device_id: String,
    pub sample_rate_hz: u32,
    pub output_channels: u16,
    pub sample_format: Option<String>,
    pub requested_buffer_frames: u32,
    pub reported_buffer_frames: Option<u32>,
    pub estimated_buffer_nanos: u64,
    pub playback_latency_nanos: Option<u64>,
    pub host_timestamps_available: bool,
    pub realtime_priority_active: Option<bool>,
    pub input: Option<CapabilityAudioInputV1>,
}

/// Four independent facts about one implementation present in the binary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityStatusV1 {
    pub id: String,
    pub compiled: bool,
    pub detected: bool,
    pub selected: bool,
    pub reason: String,
}

/// Current callback-safety evidence; unavailable readings remain `null`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilitySafetyV1 {
    pub allocator_tripwire_armed: Option<bool>,
    pub callback_allocations: Option<u64>,
    pub callback_frees: Option<u64>,
    pub callback_scope_misses: Option<u64>,
    pub callback_deadline_misses: Option<u64>,
    pub producer_refusals: Option<u64>,
}

/// Stable capability report used by the terminal and command-line views.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityReportV1 {
    pub schema_version: u32,
    pub build: CapabilityBuildV1,
    pub cpu: CapabilityCpuV1,
    pub runtime: CapabilityRuntimeV1,
    pub audio: Option<CapabilityAudioV1>,
    pub audio_unavailable_reason: Option<String>,
    pub capabilities: Vec<CapabilityStatusV1>,
    pub safety: CapabilitySafetyV1,
}

/// Live facts that do not belong to the immutable capability registry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CapabilitySafetyFacts {
    allocator_tripwire_armed: Option<bool>,
    callback_allocations: Option<u64>,
    callback_frees: Option<u64>,
    callback_scope_misses: Option<u64>,
    callback_deadline_misses: Option<u64>,
    producer_refusals: Option<u64>,
    playback_latency_nanos: Option<u64>,
    realtime_priority_active: Option<bool>,
}

impl CapabilitySafetyFacts {
    /// Start with the callback allocator guard state, when an output is open.
    pub const fn with_tripwire(armed: Option<bool>) -> Self {
        Self {
            allocator_tripwire_armed: armed,
            callback_allocations: None,
            callback_frees: None,
            callback_scope_misses: None,
            callback_deadline_misses: None,
            producer_refusals: None,
            playback_latency_nanos: None,
            realtime_priority_active: None,
        }
    }

    /// Attach the already-published live callback counters.
    #[cfg(feature = "device-audio")]
    pub fn with_live_report(
        mut self,
        report: &rustel_audio::LiveDeviceReport,
        producer_refusals: Option<u64>,
    ) -> Self {
        self.callback_allocations = Some(report.callback_allocations);
        self.callback_frees = Some(report.callback_frees);
        self.callback_scope_misses = Some(report.callback_scope_misses);
        self.callback_deadline_misses = Some(report.realtime_load.callbacks_over_100_percent);
        self.producer_refusals = producer_refusals;
        self.playback_latency_nanos =
            (report.playback_latency_nanos > 0).then_some(report.playback_latency_nanos);
        self
    }
}

/// Inputs borrowed while constructing one owned report.
#[derive(Clone, Copy, Debug)]
pub struct CapabilityReportContext<'a> {
    registry: &'a CapabilityRegistry,
    build_features: &'a [&'a str],
    audio: Option<&'a AudioStreamFacts>,
    audio_unavailable_reason: Option<&'a str>,
    safety: CapabilitySafetyFacts,
}

impl<'a> CapabilityReportContext<'a> {
    /// Start from the immutable process-wide registry.
    pub const fn new(registry: &'a CapabilityRegistry) -> Self {
        Self {
            registry,
            build_features: &[],
            audio: None,
            audio_unavailable_reason: None,
            safety: CapabilitySafetyFacts::with_tripwire(None),
        }
    }

    /// Name the Cargo features the running binary was built with; none
    /// are named until this is called.
    pub const fn with_build_features(mut self, features: &'a [&'a str]) -> Self {
        self.build_features = features;
        self
    }

    /// Attach facts from an already-open stream.
    pub const fn with_audio(mut self, audio: &'a AudioStreamFacts) -> Self {
        self.audio = Some(audio);
        self.audio_unavailable_reason = None;
        self
    }

    /// Explain why a standalone report could not open audio.
    pub const fn with_audio_unavailable(mut self, reason: &'a str) -> Self {
        self.audio = None;
        self.audio_unavailable_reason = Some(reason);
        self
    }

    /// Attach live safety counters already sampled outside the callback.
    pub const fn with_safety(mut self, safety: CapabilitySafetyFacts) -> Self {
        self.safety = safety;
        self
    }
}

impl CapabilityReportV1 {
    /// Capture one owned projection without repeating CPU detection.
    pub fn capture(context: CapabilityReportContext<'_>) -> Self {
        let cpu = context.registry.cpu_features();
        let build_features = context
            .build_features
            .iter()
            .copied()
            .map(str::to_owned)
            .collect();
        Self {
            schema_version: CAPABILITY_REPORT_SCHEMA_VERSION,
            build: CapabilityBuildV1 {
                name: product::NAME.to_owned(),
                version: product::VERSION.to_owned(),
                revision: product::REVISION.map(str::to_owned),
                target: product::BUILD_TARGET.to_owned(),
                profile: product::BUILD_PROFILE.to_owned(),
                features: build_features,
            },
            cpu: CapabilityCpuV1 {
                architecture: cpu.architecture().code().to_owned(),
                features: cpu
                    .statuses()
                    .map(|status| CapabilityCpuFeatureV1 {
                        id: status.id().code().to_owned(),
                        detected: status.detected(),
                    })
                    .collect(),
            },
            runtime: CapabilityRuntimeV1 {
                acceleration_preference: context.registry.preference().code().to_owned(),
                native_query_tier: "native_graph".to_owned(),
                callback_compatibility_tier: "compatibility".to_owned(),
                query_workers: 1,
                available_parallelism: available_parallelism(),
            },
            audio: context
                .audio
                .map(|audio| project_audio(audio, context.safety)),
            audio_unavailable_reason: context.audio_unavailable_reason.map(bounded_reason),
            capabilities: context
                .registry
                .statuses()
                .iter()
                .map(|status| CapabilityStatusV1 {
                    id: status.id().code().to_owned(),
                    compiled: status.compiled(),
                    detected: status.detected(),
                    selected: status.selected(),
                    reason: status.reason().code().to_owned(),
                })
                .collect(),
            safety: CapabilitySafetyV1 {
                allocator_tripwire_armed: context.safety.allocator_tripwire_armed,
                callback_allocations: context.safety.callback_allocations,
                callback_frees: context.safety.callback_frees,
                callback_scope_misses: context.safety.callback_scope_misses,
                callback_deadline_misses: context.safety.callback_deadline_misses,
                producer_refusals: context.safety.producer_refusals,
            },
        }
    }

    /// Human projection shared by the terminal About page and `doctor`.
    pub fn human_lines(&self) -> Vec<String> {
        let revision = self.build.revision.as_deref().unwrap_or("source build");
        let features = if self.build.features.is_empty() {
            "none".to_owned()
        } else {
            self.build.features.join(",")
        };
        let cpu_features = self
            .cpu
            .features
            .iter()
            .map(|feature| {
                format!(
                    "{} {}",
                    if feature.detected { "[~]" } else { "[-]" },
                    feature.id
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        let capabilities = self
            .capabilities
            .iter()
            .map(|status| {
                let mark = if status.selected {
                    "[x]"
                } else if status.compiled && status.detected {
                    "[~]"
                } else {
                    "[-]"
                };
                format!(
                    "{mark} {} ({})",
                    display_code(&status.id),
                    display_code(&status.reason)
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        let (output, format, buffer, input, priority) = self.audio.as_ref().map_or_else(
            || {
                let output = self.audio_unavailable_reason.as_ref().map_or_else(
                    || "not open".to_owned(),
                    |reason| format!("unavailable · {reason}"),
                );
                (
                    output,
                    "not available".to_owned(),
                    "not available".to_owned(),
                    "not available".to_owned(),
                    "unavailable",
                )
            },
            |audio| {
                let host = audio.host.id.as_deref().unwrap_or(&audio.host.kind);
                let output = format!("{host} · {}", audio.output_device_id);
                let format = format!(
                    "{} Hz · {} ch · {}",
                    audio.sample_rate_hz,
                    audio.output_channels,
                    audio.sample_format.as_deref().unwrap_or("unknown")
                );
                let reported = audio
                    .reported_buffer_frames
                    .map_or_else(|| "unknown".to_owned(), |frames| frames.to_string());
                let mut buffer = format!(
                    "requested {} · host {reported} · {}",
                    audio.requested_buffer_frames,
                    duration_ms(audio.estimated_buffer_nanos)
                );
                if let Some(latency) = audio.playback_latency_nanos {
                    buffer.push_str(&format!(" · playback {}", duration_ms(latency)));
                }
                let input = audio.input.as_ref().map_or_else(
                    || "not open".to_owned(),
                    |input| {
                        format!(
                            "{} · {} Hz · {} ch · {}",
                            input.device_id,
                            input.sample_rate_hz,
                            input.channels,
                            input.sample_format
                        )
                    },
                );
                let priority = match audio.realtime_priority_active {
                    Some(true) => "active",
                    Some(false) => "inactive",
                    None => "unavailable",
                };
                (output, format, buffer, input, priority)
            },
        );
        let tripwire = match self.safety.allocator_tripwire_armed {
            Some(true) => "callback allocation guard armed",
            Some(false) => "[-] callback allocation guard unavailable",
            None => "[-] no active callback",
        };
        let counter = |value: Option<u64>| {
            value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
        };
        vec![
            format!(
                "build        {} {} · {revision} · {}",
                self.build.name, self.build.version, self.build.profile
            ),
            format!("target       {} · features {features}", self.build.target),
            format!(
                "cpu          {} · {}",
                self.cpu.architecture,
                if cpu_features.is_empty() {
                    "no optional features"
                } else {
                    &cpu_features
                }
            ),
            format!(
                "runtime      policy {} · query workers {} · available {}",
                self.runtime.acceleration_preference,
                self.runtime.query_workers,
                self.runtime.available_parallelism
            ),
            format!(
                "tiers        {} · {} callbacks",
                display_code(&self.runtime.native_query_tier),
                display_code(&self.runtime.callback_compatibility_tier)
            ),
            format!("capability   {capabilities}"),
            format!("output       {output}"),
            format!("format       {format}"),
            format!("buffer       {buffer}"),
            format!("input        {input}"),
            format!("safety       {tripwire} · realtime priority {priority}"),
            format!(
                "callback     alloc {} · free {} · scope miss {} · deadline {} · refusal {}",
                counter(self.safety.callback_allocations),
                counter(self.safety.callback_frees),
                counter(self.safety.callback_scope_misses),
                counter(self.safety.callback_deadline_misses),
                counter(self.safety.producer_refusals)
            ),
        ]
    }
}

fn project_audio(audio: &AudioStreamFacts, safety: CapabilitySafetyFacts) -> CapabilityAudioV1 {
    let output = audio.output();
    let effective_frames = output
        .reported_buffer_frames()
        .unwrap_or(output.requested_buffer_frames());
    CapabilityAudioV1 {
        host: CapabilityAudioHostV1 {
            kind: output.host().kind().to_owned(),
            id: output.host().id().map(str::to_owned),
        },
        output_device_id: output.device_id().to_owned(),
        sample_rate_hz: output.sample_rate_hz(),
        output_channels: output.channels(),
        sample_format: output
            .sample_format()
            .map(|format| format.code().to_owned()),
        requested_buffer_frames: output.requested_buffer_frames(),
        reported_buffer_frames: output.reported_buffer_frames(),
        estimated_buffer_nanos: u64::from(effective_frames).saturating_mul(1_000_000_000)
            / u64::from(output.sample_rate_hz().max(1)),
        playback_latency_nanos: safety.playback_latency_nanos,
        host_timestamps_available: output.host_timestamps_available(),
        realtime_priority_active: safety.realtime_priority_active,
        input: audio.input().map(|input| CapabilityAudioInputV1 {
            device_id: input.device_id().to_owned(),
            sample_rate_hz: input.sample_rate_hz(),
            channels: input.channels(),
            sample_format: input.sample_format().code().to_owned(),
        }),
    }
}

fn available_parallelism() -> usize {
    static AVAILABLE: OnceLock<usize> = OnceLock::new();
    *AVAILABLE.get_or_init(|| std::thread::available_parallelism().map_or(1, usize::from))
}

fn display_code(code: &str) -> String {
    code.replace('_', " ")
}

fn duration_ms(nanos: u64) -> String {
    format!("{:.2} ms", nanos as f64 / 1_000_000.0)
}

fn bounded_reason(reason: &str) -> String {
    reason.chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        AccelerationPreference, AudioHost, AudioInputFacts, AudioOutputFacts, AudioSampleFormat,
        capability_registry_for,
    };

    fn keys(value: &serde_json::Value) -> BTreeSet<&str> {
        value
            .as_object()
            .expect("JSON object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn schema_has_exact_stable_sections_and_nulls() {
        let registry = capability_registry_for(AccelerationPreference::Portable);
        let report = CapabilityReportV1::capture(CapabilityReportContext::new(&registry));
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(
            keys(&json),
            BTreeSet::from([
                "audio",
                "audio_unavailable_reason",
                "build",
                "capabilities",
                "cpu",
                "runtime",
                "safety",
                "schema_version",
            ])
        );
        assert_eq!(json["schema_version"], 1);
        assert!(json["audio"].is_null());
        assert!(json["audio_unavailable_reason"].is_null());
        assert!(json["safety"]["callback_allocations"].is_null());
        assert_eq!(json["capabilities"][0]["selected"], true);
        assert_eq!(json["capabilities"][0]["reason"], "forced_by_preference");
        assert_eq!(
            keys(&json["build"]),
            BTreeSet::from([
                "features", "name", "profile", "revision", "target", "version"
            ])
        );
        assert_eq!(
            keys(&json["cpu"]),
            BTreeSet::from(["architecture", "features"])
        );
        for feature in json["cpu"]["features"].as_array().expect("CPU features") {
            assert_eq!(keys(feature), BTreeSet::from(["detected", "id"]));
        }
        assert_eq!(
            keys(&json["runtime"]),
            BTreeSet::from([
                "acceleration_preference",
                "available_parallelism",
                "callback_compatibility_tier",
                "native_query_tier",
                "query_workers",
            ])
        );
        for capability in json["capabilities"]
            .as_array()
            .expect("capability statuses")
        {
            assert_eq!(
                keys(capability),
                BTreeSet::from(["compiled", "detected", "id", "reason", "selected"])
            );
        }
        assert_eq!(
            keys(&json["safety"]),
            BTreeSet::from([
                "allocator_tripwire_armed",
                "callback_allocations",
                "callback_deadline_misses",
                "callback_frees",
                "callback_scope_misses",
                "producer_refusals",
            ])
        );
    }

    #[test]
    fn audio_and_human_views_are_projections_of_the_same_report() {
        let registry = capability_registry_for(AccelerationPreference::Portable);
        let audio = AudioStreamFacts::new(
            AudioOutputFacts::new(
                AudioHost::cpal("alsa"),
                "test output",
                48_000,
                2,
                Some(AudioSampleFormat::F32),
                128,
                Some(256),
            ),
            Some(AudioInputFacts::new(
                "test input",
                44_100,
                1,
                AudioSampleFormat::I16,
            )),
        );
        let report = CapabilityReportV1::capture(
            CapabilityReportContext::new(&registry)
                .with_audio(&audio)
                .with_safety(CapabilitySafetyFacts::with_tripwire(Some(true))),
        );
        let json = serde_json::to_value(&report).unwrap();
        let human = report.human_lines().join("\n");

        assert_eq!(json["audio"]["host"]["id"], "alsa");
        assert_eq!(json["audio"]["requested_buffer_frames"], 128);
        assert_eq!(json["audio"]["reported_buffer_frames"], 256);
        assert_eq!(
            keys(&json["audio"]),
            BTreeSet::from([
                "estimated_buffer_nanos",
                "host",
                "host_timestamps_available",
                "input",
                "output_channels",
                "output_device_id",
                "playback_latency_nanos",
                "realtime_priority_active",
                "reported_buffer_frames",
                "requested_buffer_frames",
                "sample_format",
                "sample_rate_hz",
            ])
        );
        assert_eq!(keys(&json["audio"]["host"]), BTreeSet::from(["id", "kind"]));
        assert_eq!(
            keys(&json["audio"]["input"]),
            BTreeSet::from(["channels", "device_id", "sample_format", "sample_rate_hz"])
        );
        assert!(human.contains("alsa · test output"));
        assert!(human.contains("requested 128 · host 256"));
        assert!(human.contains("callback allocation guard armed"));
    }

    #[test]
    fn build_features_are_the_ones_the_context_names() {
        let registry = capability_registry_for(AccelerationPreference::Portable);
        let unnamed = CapabilityReportV1::capture(CapabilityReportContext::new(&registry));
        assert!(unnamed.build.features.is_empty());
        assert!(unnamed.human_lines()[1].ends_with("features none"));

        let named = CapabilityReportV1::capture(
            CapabilityReportContext::new(&registry).with_build_features(&["midi", "studio"]),
        );
        assert_eq!(named.build.features, ["midi", "studio"]);
        assert!(named.human_lines()[1].ends_with("features midi,studio"));
    }
}

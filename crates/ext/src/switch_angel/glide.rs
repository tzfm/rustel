use rustel_core::extension_node::{ExtensionPatternNode, push_hap};
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};
use rustel_core::{Hap, Pattern, State, Value};

use super::shared::number;
use super::{NativeExtensionOperand, ORIGIN};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "glide",
    "glide polyphonic voices between notes",
    "Carries each prior voice toward the nearest new target: every moving note gets a pitch envelope (penv) bending from where its voice was to the new pitch, with pdecay set to the glide time and panchor, psustain, and pattack zeroed. Notes that do not move get no envelope. The voice memory only advances when the scheduler ticks (_cps), so one-off queries hear each note from rest.",
    params: [ReferenceParam {
        name: "time",
        r#type: "number | Pattern",
        description: "how long the bend takes; zero or a negative value snaps straight to the new note. Missing argument means silence.",
    }],
    examples: ["note(\"<c3 e3 g3>*1/2\").s(\"sawtooth\").glide(0.1)"],
    "tonal",
    "time"
);

#[derive(Clone, Copy)]
struct GlideVoice {
    initial_hz: f64,
    target_hz: f64,
    onset: f64,
}

#[derive(Default)]
struct GlideMemory {
    current: Vec<GlideVoice>,
    previous: Vec<GlideVoice>,
    last_onset: Option<f64>,
}

struct GlideNode {
    children: Vec<Pattern>,
    memory: std::sync::Mutex<GlideMemory>,
}

fn glide_frequency(value: &Value) -> Result<f64, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "getFrequencyFromValue expects an object".to_owned())?;
    let default_midi = if object.get("s").and_then(Value::as_str) == Some("sbd") {
        29.0
    } else {
        36.0
    };
    let mut frequency = rustel_core::util::midi_to_freq(rustel_core::util::value_to_midi(
        value,
        Some(default_midi),
    )?);
    if let Some(octave) = object.get("octave").and_then(Value::as_f64) {
        frequency *= 2f64.powf(octave);
    }
    Ok(frequency)
}

impl ExtensionPatternNode for GlideNode {
    fn kind(&self) -> &'static str {
        "switch_angel.glide"
    }

    fn children(&self) -> &[Pattern] {
        &self.children
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        let triggered = state.controls.get("_cps").is_some_and(Value::js_truthy);
        let mut memory = self
            .memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut output = Vec::new();
        for hap in self.children[1].query(state) {
            let Some(whole) = hap.whole else {
                rustel_core::signal_query_error(|| {
                    "Cannot read properties of undefined (reading 'begin')".into()
                });
                return Vec::new();
            };
            let onset = whole.begin.to_f64();
            if triggered && memory.last_onset != Some(onset) {
                memory.previous = std::mem::take(&mut memory.current);
                memory.last_onset = Some(onset);
            }
            let target_hz = match glide_frequency(&hap.value) {
                Ok(frequency) => frequency,
                Err(message) => {
                    rustel_core::signal_query_error(move || message);
                    return Vec::new();
                }
            };
            for glide_hap in self.children[0].query(&state.set_span(hap.whole_or_part())) {
                let Some(part) = hap.part.intersection(&glide_hap.part) else {
                    continue;
                };
                let glide_time = number(&glide_hap.value);
                let initial_hz = memory.previous.iter().fold(None, |closest, voice| {
                    let phase = if glide_time > 0.0 {
                        ((onset - voice.onset) / glide_time).min(1.0)
                    } else {
                        1.0
                    };
                    let candidate = voice.initial_hz + phase * (voice.target_hz - voice.initial_hz);
                    match closest {
                        None => Some(candidate),
                        Some(current)
                            if (candidate - target_hz).abs() < (current - target_hz).abs() =>
                        {
                            Some(candidate)
                        }
                        current => current,
                    }
                });
                let initial_hz = initial_hz.unwrap_or(target_hz);
                if triggered
                    && !memory.current.iter().any(|voice| {
                        voice.onset.to_bits() == onset.to_bits()
                            && voice.initial_hz.to_bits() == initial_hz.to_bits()
                            && voice.target_hz.to_bits() == target_hz.to_bits()
                    })
                {
                    // A producer retry may query the same onset again. The
                    // authored state machine appended the same voice each
                    // time, so a long-running retry loop could grow this
                    // state without changing the selected glide. Retain one
                    // exact voice state per onset instead.
                    memory.current.push(GlideVoice {
                        initial_hz,
                        target_hz,
                        onset,
                    });
                }
                let combined = hap.combine_context(&glide_hap).with_part(part);
                let combined = if (target_hz - initial_hz).abs() > 1e-6 {
                    combined.with_value(|value| {
                        let mut object = value.as_object().cloned().unwrap_or_default();
                        object.insert("panchor".into(), Value::F64(0.0));
                        object.insert("psustain".into(), Value::F64(0.0));
                        object.insert("pattack".into(), Value::F64(0.0));
                        object.insert("pdecay".into(), Value::F64(glide_time));
                        object.insert(
                            "penv".into(),
                            Value::F64(-12.0 * (target_hz / initial_hz).log2()),
                        );
                        Value::Object(object)
                    })
                } else {
                    combined
                };
                if !push_hap(&mut output, combined) {
                    return Vec::new();
                }
            }
        }
        output
    }

    fn drain_children(&mut self, output: &mut Vec<Pattern>) {
        output.append(&mut self.children);
    }
}

fn apply<P: NativeExtensionOperand>(time: &P, pattern: &P) -> P {
    P::from_extension_node(GlideNode {
        children: vec![time.pattern_handle(), pattern.pattern_handle()],
        memory: std::sync::Mutex::new(GlideMemory::default()),
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["glide"],
        REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(time) = args.first() else {
                return PatOps::pat_silence();
            };
            apply(time, &pattern)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_fraction::Fraction;

    #[test]
    fn repeated_queries_do_not_retain_duplicate_voice_state() {
        let node = GlideNode {
            children: vec![
                rustel_core::pure(Value::F64(1.0)),
                super::super::shared::source_control_pattern(
                    "note",
                    rustel_core::pure(Value::Str("c3".into())),
                ),
            ],
            memory: std::sync::Mutex::new(GlideMemory::default()),
        };
        let mut state = State::new(rustel_core::TimeSpan::new(Fraction::ZERO, Fraction::ONE));
        state.controls.push(("_cps".into(), Value::F64(0.5)));

        for _ in 0..1_000 {
            assert_eq!(node.query(&state).len(), 1);
        }
        assert_eq!(
            node.memory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .current
                .len(),
            1
        );
    }
}

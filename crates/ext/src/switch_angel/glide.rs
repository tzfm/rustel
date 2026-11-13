use rustel_core::extension_node::{ExtensionPatternNode, push_hap};
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};
use rustel_core::{Hap, Pattern, State, TimeSpan, Value};

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
}

/// The voices of the notes that begin at one onset, and the pitches of the
/// older notes that still sound there.
struct Generation {
    onset: f64,
    voices: Vec<GlideVoice>,
}

/// The two newest generations of voices from triggered queries.
#[derive(Default)]
struct GlideMemory {
    /// The generation before `current`. Notes bend from its voices.
    previous: Option<Generation>,
    /// The newest generation. It takes the voices of its own notes and the
    /// pitches of the older notes that still sound at its onset.
    current: Option<Generation>,
}

impl GlideMemory {
    /// Returns whether a note at `onset` adds its voices to the newest
    /// generation. `opens` is true when the note begins in the queried span.
    /// A note on an empty memory or at a later onset starts a new generation
    /// at `onset`. The new generation becomes the newest, and the one before
    /// it becomes the previous one. A note at an earlier onset does the same
    /// when it opens, which is a jump back. A note at the onset of the newest
    /// generation leaves the memory as it is. So does a note that began
    /// before the span and before the newest generation, but it adds no
    /// voices. The caller gives such a note to `hold`, which keeps its pitch
    /// in the newest generation while the note sounds.
    fn enter(&mut self, onset: f64, opens: bool) -> bool {
        match &self.current {
            Some(current) if current.onset == onset => true,
            Some(current) if onset < current.onset && !opens => false,
            _ => {
                self.previous = self.current.replace(Generation {
                    onset,
                    voices: Vec::new(),
                });
                true
            }
        }
    }

    /// Returns the pitch that a note at `onset` bends from toward
    /// `target_hz`. Each voice of the previous generation bends over
    /// `glide_time` from the onset of that generation. The voice whose pitch
    /// at `onset` is nearest to `target_hz` gives the result. That pitch stays
    /// between the start and the target of the voice, also when `onset` is
    /// before the previous generation. Returns `None` when there is no
    /// previous voice.
    fn initial_hz(&self, onset: f64, target_hz: f64, glide_time: f64) -> Option<f64> {
        let previous = self.previous.as_ref()?;
        let phase = if glide_time > 0.0 {
            ((onset - previous.onset) / glide_time).clamp(0.0, 1.0)
        } else {
            1.0
        };
        previous.voices.iter().fold(None, |closest, voice| {
            let candidate = voice.initial_hz + phase * (voice.target_hz - voice.initial_hz);
            match closest {
                None => Some(candidate),
                Some(current) if (candidate - target_hz).abs() < (current - target_hz).abs() => {
                    Some(candidate)
                }
                current => current,
            }
        })
    }

    /// Adds `voice` to the newest generation. A producer retry can query the
    /// same onset again. The authored glide then adds the same voice again,
    /// which grows its memory and does not change the glide. This memory
    /// keeps one copy of each exact voice.
    fn remember(&mut self, voice: GlideVoice) {
        if let Some(current) = &mut self.current
            && !current.voices.iter().any(|kept| {
                kept.initial_hz.to_bits() == voice.initial_hz.to_bits()
                    && kept.target_hz.to_bits() == voice.target_hz.to_bits()
            })
        {
            current.voices.push(voice);
        }
    }

    /// Keeps the pitch of a note that began before the newest generation in
    /// that generation, so a note of the next onset can bend from it. `end`
    /// is where the note ends. A note that ends at or before the onset of
    /// the newest generation does not sound there and is not kept. A window
    /// that is queried again can bring such a note.
    fn hold(&mut self, end: f64, target_hz: f64) {
        if self
            .current
            .as_ref()
            .is_some_and(|current| end > current.onset)
        {
            self.remember(GlideVoice {
                initial_hz: target_hz,
                target_hz,
            });
        }
    }
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

/// Returns the whole of `note`. A note without a whole signals the error
/// that the authored glide throws.
fn note_whole(note: &Hap) -> Option<TimeSpan> {
    if note.whole.is_none() {
        rustel_core::signal_query_error(|| {
            "Cannot read properties of undefined (reading 'begin')".into()
        });
    }
    note.whole
}

/// Returns the frequency of `note`, or signals why it has none.
fn note_frequency(note: &Hap) -> Option<f64> {
    match glide_frequency(&note.value) {
        Ok(frequency) => Some(frequency),
        Err(message) => {
            rustel_core::signal_query_error(move || message);
            None
        }
    }
}

/// Returns whether no note begins before the note answered ahead of it. A
/// note without a whole does not break the order, because the query stops
/// at it.
fn in_onset_order(notes: &[Hap]) -> bool {
    notes
        .windows(2)
        .all(|pair| match (pair[0].whole, pair[1].whole) {
            (Some(earlier), Some(later)) => earlier.begin <= later.begin,
            _ => true,
        })
}

/// Combines `note` with each hap of `glide_times` and pushes the results to
/// `output`. A note that moves gets a pitch envelope that bends from the
/// previous generation. The memory keeps its voices when `remember` is true.
/// Returns false when the hap budget refuses a push.
fn glide_note(
    memory: &mut GlideMemory,
    note: &Hap,
    glide_times: Vec<Hap>,
    onset: f64,
    target_hz: f64,
    remember: bool,
    output: &mut Vec<Hap>,
) -> bool {
    for glide_hap in glide_times {
        let Some(part) = note.part.intersection(&glide_hap.part) else {
            continue;
        };
        let glide_time = number(&glide_hap.value);
        let initial_hz = memory
            .initial_hz(onset, target_hz, glide_time)
            .unwrap_or(target_hz);
        if remember {
            memory.remember(GlideVoice {
                initial_hz,
                target_hz,
            });
        }
        let combined = note.combine_context(&glide_hap).with_part(part);
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
        if !push_hap(output, combined) {
            return false;
        }
    }
    true
}

impl GlideNode {
    /// Glides `notes` in onset order and answers them in the given order. A
    /// stack answers layer by layer, so a later layer can bring an earlier
    /// onset. Every note is checked in the given order first, and the first
    /// failure stops the query before the memory moves. Unlike the in-place
    /// loop in `query`, a failing note leaves the memory as it was.
    fn glide_in_onset_order(
        &self,
        memory: &mut GlideMemory,
        state: &State,
        triggered: bool,
        notes: &[Hap],
    ) -> Vec<Hap> {
        let mut checked = Vec::with_capacity(notes.len());
        for (index, note) in notes.iter().enumerate() {
            let Some(whole) = note_whole(note) else {
                return Vec::new();
            };
            let Some(target_hz) = note_frequency(note) else {
                return Vec::new();
            };
            checked.push((whole, target_hz, index));
        }
        checked.sort_by_key(|&(whole, ..)| whole.begin);
        // The haps in onset order, charged to the budget as they grow, and
        // the index of the note that each one came from.
        let mut produced = Vec::new();
        let mut sources = Vec::new();
        for (whole, target_hz, index) in checked {
            let note = &notes[index];
            let onset = whole.begin.to_f64();
            let remember = triggered && memory.enter(onset, whole.begin >= state.span.begin);
            if triggered && !remember {
                memory.hold(whole.end.to_f64(), target_hz);
            }
            let glide_times = self.children[0].query(&state.set_span(note.whole_or_part()));
            if !glide_note(
                memory,
                note,
                glide_times,
                onset,
                target_hz,
                remember,
                &mut produced,
            ) {
                return Vec::new();
            }
            sources.resize(produced.len(), index);
        }
        let mut tagged: Vec<(usize, Hap)> = sources.into_iter().zip(produced).collect();
        tagged.sort_by_key(|&(index, _)| index);
        tagged.into_iter().map(|(_, hap)| hap).collect()
    }
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
        let notes = self.children[1].query(state);
        // Only a triggered query moves the memory, so only it needs the notes
        // in onset order.
        if triggered && !in_onset_order(&notes) {
            return self.glide_in_onset_order(&mut memory, state, triggered, &notes);
        }
        let mut output = Vec::new();
        for hap in notes {
            let Some(whole) = note_whole(&hap) else {
                return Vec::new();
            };
            let onset = whole.begin.to_f64();
            let remember = triggered && memory.enter(onset, whole.begin >= state.span.begin);
            let Some(target_hz) = note_frequency(&hap) else {
                return Vec::new();
            };
            if triggered && !remember {
                memory.hold(whole.end.to_f64(), target_hz);
            }
            let glide_times = self.children[0].query(&state.set_span(hap.whole_or_part()));
            if !glide_note(
                &mut memory,
                &hap,
                glide_times,
                onset,
                target_hz,
                remember,
                &mut output,
            ) {
                return Vec::new();
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
                .as_ref()
                .map(|generation| generation.voices.len()),
            Some(1)
        );
    }

    /// A stack answers layer by layer, so a later onset can come before an
    /// earlier one. The glide moves its memory in onset order, and the
    /// scheduler still gets the notes in the order of the stack.
    #[test]
    fn a_stack_is_answered_in_its_own_order() {
        let node = GlideNode {
            children: vec![
                rustel_core::pure(Value::F64(0.05)),
                super::super::shared::source_control_pattern(
                    "note",
                    rustel_mini::mini("c3*8, g3*3").expect("the notes parse"),
                ),
            ],
            memory: std::sync::Mutex::new(GlideMemory::default()),
        };
        let mut state = State::new(rustel_core::TimeSpan::new(
            Fraction::ZERO,
            Fraction::new(1, 4),
        ));
        state.controls.push(("_cps".into(), Value::F64(0.5)));

        let haps = node.query(&state);
        let answer: Vec<_> = haps
            .iter()
            .map(|hap| (hap.value.get("note"), hap.whole.map(|whole| whole.begin)))
            .collect();
        let c3 = Value::Str("c3".into());
        let g3 = Value::Str("g3".into());
        assert_eq!(
            answer,
            [
                (Some(&c3), Some(Fraction::ZERO)),
                (Some(&c3), Some(Fraction::new(1, 8))),
                (Some(&g3), Some(Fraction::ZERO)),
            ]
        );
    }
}

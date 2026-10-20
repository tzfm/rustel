/*
score.rs - Turn MIDI notes into a Rustel score
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Quantization and score emission for MIDI import.
//!
//! One labeled statement holds one lane. Overlapping notes become `stack`
//! voices. A file longer than one bar appends `.slow(bars)` to every lane, so
//! one cycle stays one bar and the weights stay one step.

use std::collections::{BTreeMap, BTreeSet};

use crate::SmfError;
use crate::import::LaneReport;
use crate::meter::Meter;
use crate::tables::{drum_name_for_note, sound_for_program};

/// Most voices one lane may need.
pub(crate) const MAX_VOICES: usize = 32;

/// One voice: the step it is free at, and its groups of `(start, end, text)`.
type Voice = (u64, Vec<(u64, u64, String)>);

/// A note as the file wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawNote {
    pub note: u8,
    pub start: u64,
    pub end: u64,
}

/// One lane to emit.
pub(crate) struct ScoreLane<'a> {
    pub label: String,
    pub channel: u8,
    pub program: Option<u8>,
    pub notes: &'a [RawNote],
}

/// The input to score emission.
pub(crate) struct ScoreInput<'a> {
    pub format: u8,
    pub ppq: u16,
    pub tempo_bpm: f64,
    pub meter: Meter,
    pub lanes: &'a [ScoreLane<'a>],
    pub max_bars: u32,
    pub max_source_bytes: usize,
}

/// The emitted score and its report.
pub(crate) struct ScoreOutput {
    pub source: String,
    pub bars: u32,
    pub grid_steps_per_bar: u32,
    pub grid_exact: bool,
    pub lanes: Vec<LaneReport>,
    pub warnings: Vec<String>,
}

/// Quantize and emit the score.
pub(crate) fn emit(input: &ScoreInput<'_>) -> Result<ScoreOutput, SmfError> {
    let mut warnings = Vec::new();
    let bar_ticks = input.meter.ticks_per_bar(input.ppq);
    if !bar_ticks.is_finite() || bar_ticks <= 0.0 {
        return Err(SmfError::InvalidOptions(
            "the meter and ppq give no usable bar length".to_owned(),
        ));
    }
    let (steps, max_error) = choose_grid(input.lanes, bar_ticks).unwrap_or_else(|| {
        warnings.push("the timing did not fit a 64-step grid; notes were quantized".to_owned());
        (64, f64::INFINITY)
    });
    let grid_exact = max_error <= 1e-9;
    let step_ticks = bar_ticks / f64::from(steps);

    let mut total_steps = 0u64;
    let mut dropped = 0usize;
    let mut quantized = Vec::with_capacity(input.lanes.len());
    for lane in input.lanes {
        let drums = lane.channel == 10;
        let mut groups: BTreeMap<(u64, u64), BTreeSet<u8>> = BTreeMap::new();
        let mut lane_dropped = 0usize;
        for note in lane.notes {
            let start = quantize(note.start, step_ticks);
            let end = if drums {
                start + 1
            } else {
                quantize(note.end, step_ticks).max(start + 1)
            };
            if drums && drum_name_for_note(note.note).is_none() {
                lane_dropped += 1;
                continue;
            }
            groups.entry((start, end)).or_default().insert(note.note);
            total_steps = total_steps.max(end);
        }
        dropped += lane_dropped;
        quantized.push((lane, groups, drums));
    }

    let bars = if total_steps == 0 {
        1
    } else {
        total_steps.div_ceil(u64::from(steps))
    };
    if bars > u64::from(input.max_bars) {
        return Err(SmfError::TooManyBars {
            limit: input.max_bars,
        });
    }
    let total_steps = bars * u64::from(steps);

    let bpm_text = number(input.tempo_bpm, 3);
    let meter_text = format!("{}/{}", input.meter.numerator, input.meter.denominator);
    let mut source = format!(
        "// MIDI import: format {}, {} ppq, {} bpm, {}, grid {} steps/bar, {} bar{}\n",
        input.format,
        input.ppq,
        bpm_text,
        meter_text,
        steps,
        bars,
        if bars == 1 { "" } else { "s" }
    );
    let cps = input.tempo_bpm * f64::from(input.meter.denominator)
        / (4.0 * f64::from(input.meter.numerator));
    if !cps.is_finite() || cps <= 0.0 {
        return Err(SmfError::TempoOutOfRange(input.tempo_bpm));
    }
    source.push_str(&format!(
        "setcpm({}) // {} bpm in {}\n\n",
        number(cps, 9),
        bpm_text,
        meter_text
    ));

    let mut reports = Vec::new();
    for (lane, groups, drums) in quantized {
        if groups.is_empty() {
            continue;
        }
        let emitted_notes: usize = groups.values().map(BTreeSet::len).sum();
        let voices = voices_for(groups, drums, total_steps)?;
        let sound = if drums {
            "drums".to_owned()
        } else {
            sound_for_program(lane.program.unwrap_or(0))
                .unwrap_or("gm_piano")
                .to_owned()
        };
        let voice_texts: Vec<String> = voices
            .iter()
            .map(|tokens| {
                let inner = tokens.join(" ");
                if drums {
                    format!("s(\"{inner}\")")
                } else {
                    format!("note(\"{inner}\")")
                }
            })
            .collect();
        let expression = if voice_texts.len() == 1 {
            voice_texts[0].clone()
        } else {
            format!("stack({})", voice_texts.join(", "))
        };
        let mut line = format!("{}: {}", lane.label, expression);
        if !drums {
            line.push_str(&format!(".s(\"{sound}\")"));
            if let Some(program) = lane.program {
                line.push_str(&format!(".progNum({program})"));
            }
        }
        line.push_str(&format!(".midichan({})", lane.channel));
        if bars > 1 {
            line.push_str(&format!(".slow({bars})"));
        }
        source.push_str(&line);
        source.push('\n');
        reports.push(LaneReport {
            label: lane.label.clone(),
            channel: lane.channel,
            program: if drums { None } else { lane.program },
            sound,
            notes: emitted_notes,
        });
    }
    if reports.is_empty() {
        return Err(SmfError::NoNotes);
    }
    if dropped > 0 {
        warnings.push(format!(
            "{dropped} drum note(s) outside the mapped range were dropped"
        ));
    }
    if source.len() > input.max_source_bytes {
        return Err(SmfError::TooLarge {
            limit: input.max_source_bytes,
            size: source.len(),
        });
    }
    Ok(ScoreOutput {
        source,
        bars: bars as u32,
        grid_steps_per_bar: steps,
        grid_exact,
        lanes: reports,
        warnings,
    })
}

fn choose_grid(lanes: &[ScoreLane<'_>], bar_ticks: f64) -> Option<(u32, f64)> {
    for steps in crate::GRID_STEPS_PER_BAR {
        let step_ticks = bar_ticks / f64::from(steps);
        let mut max_error = 0.0f64;
        for lane in lanes {
            for note in lane.notes {
                // A drum note with no mapped name never reaches the score, so
                // it must not choose the grid for the notes that do.
                if lane.channel == 10 && drum_name_for_note(note.note).is_none() {
                    continue;
                }
                let position = note.start as f64 / step_ticks;
                max_error = max_error.max((position - position.round()).abs());
            }
        }
        if max_error <= 0.05 {
            return Some((steps, max_error));
        }
    }
    None
}

fn quantize(tick: u64, step_ticks: f64) -> u64 {
    let position = tick as f64 / step_ticks;
    if !position.is_finite() || position <= 0.0 {
        return 0;
    }
    position.round() as u64
}

fn rest(steps: u64) -> String {
    if steps == 1 {
        "~".to_owned()
    } else {
        format!("~@{steps}")
    }
}

fn voices_for(
    groups: BTreeMap<(u64, u64), BTreeSet<u8>>,
    drums: bool,
    total_steps: u64,
) -> Result<Vec<Vec<String>>, SmfError> {
    let mut voices: Vec<Voice> = Vec::new();
    for ((start, end), notes) in groups {
        let text = if drums {
            let mut names: Vec<&str> = notes
                .iter()
                .filter_map(|note| drum_name_for_note(*note))
                .collect();
            names.sort_unstable();
            names.dedup();
            if names.len() == 1 {
                names[0].to_owned()
            } else {
                format!("[{}]", names.join(","))
            }
        } else {
            let names: Vec<String> = notes
                .iter()
                .map(|note| rustel_core::util::note_name(*note))
                .collect();
            if names.len() == 1 {
                names[0].clone()
            } else {
                format!("[{}]", names.join(","))
            }
        };
        let text = if end > start + 1 {
            format!("{text}@{}", end - start)
        } else {
            text
        };
        let chosen = voices
            .iter()
            .enumerate()
            .filter(|(_, (free, _))| *free <= start)
            .max_by_key(|(_, (free, _))| *free)
            .map(|(index, _)| index);
        match chosen {
            Some(index) => {
                voices[index].1.push((start, end, text));
                voices[index].0 = end;
            }
            None => {
                if voices.len() >= MAX_VOICES {
                    return Err(SmfError::TooManyVoices { limit: MAX_VOICES });
                }
                voices.push((end, vec![(start, end, text)]));
            }
        }
    }
    let mut output = Vec::with_capacity(voices.len());
    for (_, groups) in voices {
        let mut tokens = Vec::new();
        let mut cursor = 0u64;
        for (start, end, text) in groups {
            if start > cursor {
                tokens.push(rest(start - cursor));
            }
            tokens.push(text);
            cursor = end;
        }
        if cursor < total_steps {
            tokens.push(rest(total_steps - cursor));
        }
        output.push(tokens);
    }
    Ok(output)
}

fn number(value: f64, places: usize) -> String {
    let text = format!("{value:.places$}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_one(notes: &[RawNote], channel: u8, program: Option<u8>) -> ScoreOutput {
        let lane = ScoreLane {
            label: "lead".to_owned(),
            channel,
            program,
            notes,
        };
        emit(&ScoreInput {
            format: 1,
            ppq: 480,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            lanes: &[lane],
            max_bars: 4096,
            max_source_bytes: 4 * 1024 * 1024,
        })
        .unwrap()
    }

    #[test]
    fn quarter_notes_emit_one_plain_sequence() {
        let notes = vec![
            RawNote {
                note: 60,
                start: 0,
                end: 480,
            },
            RawNote {
                note: 62,
                start: 480,
                end: 960,
            },
            RawNote {
                note: 64,
                start: 960,
                end: 1440,
            },
            RawNote {
                note: 65,
                start: 1440,
                end: 1920,
            },
        ];
        let output = emit_one(&notes, 1, Some(0));
        assert_eq!(
            output.source,
            "// MIDI import: format 1, 480 ppq, 120 bpm, 4/4, grid 4 steps/bar, 1 bar\n\
             setcpm(30) // 120 bpm in 4/4\n\n\
             lead: note(\"c4 d4 e4 f4\").s(\"gm_piano\").progNum(0).midichan(1)\n"
        );
        assert_eq!(output.grid_steps_per_bar, 4);
        assert!(output.grid_exact);
    }

    #[test]
    fn gates_and_gaps_become_weights() {
        let notes = vec![
            RawNote {
                note: 60,
                start: 0,
                end: 960,
            },
            RawNote {
                note: 62,
                start: 960,
                end: 1440,
            },
            RawNote {
                note: 64,
                start: 1440,
                end: 1920,
            },
        ];
        let output = emit_one(&notes, 1, None);
        assert!(output.source.contains("note(\"c4@2 d4 e4\")"));
    }

    #[test]
    fn a_chord_is_one_bracket_token() {
        let notes = vec![
            RawNote {
                note: 60,
                start: 0,
                end: 480,
            },
            RawNote {
                note: 64,
                start: 0,
                end: 480,
            },
            RawNote {
                note: 67,
                start: 0,
                end: 480,
            },
        ];
        let output = emit_one(&notes, 1, None);
        assert!(output.source.contains("note(\"[c4,e4,g4] ~@3\")"));
    }

    #[test]
    fn overlapping_notes_become_stacked_voices() {
        let notes = vec![
            RawNote {
                note: 60,
                start: 0,
                end: 960,
            },
            RawNote {
                note: 62,
                start: 480,
                end: 1440,
            },
        ];
        let output = emit_one(&notes, 1, None);
        assert!(
            output
                .source
                .contains("stack(note(\"c4@2 ~@2\"), note(\"~ d4@2 ~\"))")
        );
    }

    #[test]
    fn a_multi_bar_file_is_slowed_to_keep_one_bar_per_cycle() {
        let notes = vec![RawNote {
            note: 60,
            start: 0,
            end: 3840,
        }];
        let output = emit_one(&notes, 1, None);
        assert_eq!(output.bars, 2);
        assert!(output.source.contains(".slow(2)"));
        assert!(output.source.contains("c4@8"));
    }

    #[test]
    fn drums_use_the_mapped_names_and_channel_ten() {
        let notes = vec![
            RawNote {
                note: 35,
                start: 0,
                end: 480,
            },
            RawNote {
                note: 38,
                start: 480,
                end: 960,
            },
            RawNote {
                note: 52,
                start: 960,
                end: 1440,
            },
        ];
        let output = emit_one(&notes, 10, None);
        assert!(output.source.contains("s(\"bd sd ~@2\").midichan(10)"));
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("dropped"))
        );
    }

    #[test]
    fn a_dropped_drum_note_does_not_choose_the_grid() {
        let notes = vec![
            RawNote {
                note: 35,
                start: 0,
                end: 240,
            },
            RawNote {
                note: 52,
                start: 45,
                end: 285,
            },
        ];
        let output = emit_one(&notes, 10, None);
        assert_eq!(output.grid_steps_per_bar, 4);
        assert_eq!(output.lanes[0].notes, 1);
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("dropped"))
        );
    }

    #[test]
    fn a_six_eight_meter_changes_the_setcpm_value() {
        let notes = vec![RawNote {
            note: 60,
            start: 0,
            end: 1440,
        }];
        let lane = ScoreLane {
            label: "lead".to_owned(),
            channel: 1,
            program: None,
            notes: &notes,
        };
        let output = emit(&ScoreInput {
            format: 1,
            ppq: 480,
            tempo_bpm: 120.0,
            meter: Meter::new(6, 8).unwrap(),
            lanes: &[lane],
            max_bars: 4096,
            max_source_bytes: 4 * 1024 * 1024,
        })
        .unwrap();
        assert!(output.source.contains("setcpm(40)"));
    }

    #[test]
    fn a_humanized_start_falls_back_to_sixty_four_steps() {
        let notes = vec![RawNote {
            note: 60,
            start: 45,
            end: 75,
        }];
        let output = emit_one(&notes, 1, None);
        assert_eq!(output.grid_steps_per_bar, 64);
        assert!(!output.grid_exact);
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("64-step grid"))
        );
    }

    #[test]
    fn too_many_bars_is_refused() {
        let notes = vec![RawNote {
            note: 60,
            start: 0,
            end: 3_840,
        }];
        let lane = ScoreLane {
            label: "lead".to_owned(),
            channel: 1,
            program: None,
            notes: &notes,
        };
        let result = emit(&ScoreInput {
            format: 1,
            ppq: 480,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            lanes: &[lane],
            max_bars: 1,
            max_source_bytes: 4 * 1024 * 1024,
        });
        assert!(matches!(result, Err(SmfError::TooManyBars { limit: 1 })));
    }

    #[test]
    fn the_generated_source_limit_is_refused() {
        let notes = vec![RawNote {
            note: 60,
            start: 0,
            end: 480,
        }];
        let lane = ScoreLane {
            label: "lead".to_owned(),
            channel: 1,
            program: None,
            notes: &notes,
        };
        let result = emit(&ScoreInput {
            format: 1,
            ppq: 480,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            lanes: &[lane],
            max_bars: 4096,
            max_source_bytes: 1,
        });
        assert!(matches!(result, Err(SmfError::TooLarge { limit: 1, .. })));
    }

    #[test]
    fn too_many_voices_refuse_the_lane() {
        let notes: Vec<RawNote> = (0..=MAX_VOICES)
            .map(|index| RawNote {
                note: 60,
                start: 30,
                end: 30 * (u64::try_from(index).unwrap() + 2),
            })
            .collect();
        let lane = ScoreLane {
            label: "lead".to_owned(),
            channel: 1,
            program: None,
            notes: &notes,
        };
        let result = emit(&ScoreInput {
            format: 1,
            ppq: 480,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            lanes: &[lane],
            max_bars: 4096,
            max_source_bytes: 4 * 1024 * 1024,
        });
        assert!(matches!(result, Err(SmfError::TooManyVoices { .. })));
    }
}

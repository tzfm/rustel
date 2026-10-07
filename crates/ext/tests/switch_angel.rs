/*
rustel-ext - Switch Angel extensions
Rust test harness:
Copyright (C) 2026 Rustel contributors

The tested recipes are credited to Switch Angel in their implementation files.
See NOTICE.md for their source and permission status.

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Check Switch Angel extensions against their original JavaScript prebake.
//! Tests document intentional differences. These names are not Strudel built-ins.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt
}

fn haps(source: &str) -> Vec<rustel_core::Hap> {
    let rt = runtime();
    rt.evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query")
}

/// Every `note` the first cycle carries, in query order.
fn notes(source: &str) -> Vec<f64> {
    haps(source)
        .iter()
        .filter_map(|hap| hap.value.as_object()?.get("note")?.as_f64())
        .collect()
}

/// Every value of one control across the first cycle.
fn control(source: &str, key: &str) -> Vec<String> {
    haps(source)
        .iter()
        .map(|hap| match hap.value.as_object().and_then(|o| o.get(key)) {
            Some(value) => format!("{value:?}"),
            None => "-".to_owned(),
        })
        .collect()
}

/// `acidenv(x)` is `pat.lpf(100).lpenv(x * 9).lps(.2).lpd(.12).lpq(2)`, and
/// `x` is read per hap, so a pattern (or a slider) drives the envelope.
#[test]
fn acidenv_sets_the_filter_envelope_from_its_argument() {
    assert_eq!(
        control("s(\"sawtooth*2\").acidenv(\"<0.5 1>*2\")", "lpenv"),
        vec!["F64(4.5)", "F64(9.0)"]
    );
    assert_eq!(
        control("s(\"sawtooth\").acidenv(0.45)", "cutoff"),
        vec!["F64(100.0)"]
    );
    assert_eq!(
        control("s(\"sawtooth\").acidenv(0.45)", "lpsustain"),
        vec!["F64(0.2)"]
    );
    assert_eq!(
        control("s(\"sawtooth\").acidenv(0.45)", "lpdecay"),
        vec!["F64(0.12)"]
    );
    assert_eq!(
        control("s(\"sawtooth\").acidenv(0.45)", "resonance"),
        vec!["F64(2.0)"]
    );
}

#[test]
fn grab_snaps_each_note_to_the_nearest_of_a_set() {
    // `scale()` offers named modes; this takes any collection of pitches.
    // 0 → e(4), 3 → 4, 5 → 4, 7 → 7, 12 → 16: the octave is kept and the
    // pitch class snapped inside it, so a melody stays in its register.
    assert_eq!(
        notes(r#"n("0 3 5 7 12").grab("e:g:b")"#),
        vec![4.0, 4.0, 4.0, 7.0, 16.0]
    );
}

#[test]
fn grab_truncates_the_octave_toward_zero_as_hers_does() {
    // JavaScript's `(note / 12) >> 0` truncates toward zero, unlike floor.
    assert_eq!(
        notes(r#"n("-1 -5 -12 24").grab("c:e:g")"#),
        vec![0.0, 0.0, -12.0, 24.0]
    );
}

#[test]
fn grab_reads_a_note_pattern_where_hers_flattens_it() {
    // Intentional difference: the reference reads only `n`, flattening note()
    // input to the first target. Preserve the note pattern here.
    assert_eq!(
        notes(r#"note("c3 d3 e3 f3").grab("c:d#:g")"#),
        vec![48.0, 51.0, 51.0, 51.0]
    );
}

#[test]
fn filtval_transforms_only_where_a_control_holds_the_value() {
    // The kicks get the speed; nothing else is touched.
    assert_eq!(
        control(
            r#"s("bd hh sd hh").filtval("s", "bd", x => x.speed(2))"#,
            "speed"
        ),
        vec!["F64(2.0)", "-", "-", "-"]
    );
}

#[test]
fn filtval_compares_strictly_but_mini_notation_reads_a_number_first() {
    // Comparison is type-strict, but mini-notation converts numeric text before
    // it reaches filtval. The original prebake behaves the same way.
    for written_as in [r#"1"#, r#""1""#] {
        assert_eq!(
            control(
                &format!(r#"n("0 1 2 3").filtval("n", {written_as}, x => x.speed(4))"#),
                "speed"
            ),
            vec!["-", "F64(4.0)", "-", "-"],
            "written as {written_as}"
        );
    }
    // A control holding text is not matched by a number.
    assert_eq!(
        control(r#"s("bd hh").filtval("s", 0, x => x.speed(4))"#, "speed"),
        vec!["-", "-"]
    );
}

/// The color one scalar `colorparty` amount writes, on a single-event cycle.
fn colorparty(amount: &str) -> String {
    let mut colors = control(&format!(r#"s("bd").colorparty({amount})"#), "color");
    assert_eq!(colors.len(), 1, "one event, one color");
    colors.remove(0)
}

#[test]
fn colorparty_walks_the_palette_across_the_unit_interval() {
    // floor(amount × 8) over ['blue', 'yellow', 'violet', 'green', 'orange',
    // 'cyan', 'magenta', 'white']: the documented 0-to-1 range.
    assert_eq!(colorparty("0.5"), r#"Str("orange")"#);
    assert_eq!(colorparty("0.999"), r#"Str("white")"#);
}

#[test]
fn colorparty_wraps_negative_amounts_the_way_at_does() {
    // Array.at wraps negative indices once. Bipolar signals need this behavior.
    assert_eq!(colorparty("-1"), r#"Str("blue")"#); // at(floor(-8)) wraps to 0
    assert_eq!(colorparty("-0.125"), r#"Str("white")"#); // at(-1) is the last
    assert_eq!(colorparty("-0.5"), r#"Str("orange")"#); // at(-4) wraps to 4
}

#[test]
fn colorparty_stays_undefined_off_both_ends_of_the_palette() {
    // `at` answers undefined for an index still outside after the wrap, and
    // the color control carries that as the string "undefined". This holds
    // on the positive side and below -1.
    assert_eq!(colorparty("1"), r#"Str("undefined")"#);
    assert_eq!(colorparty("2"), r#"Str("undefined")"#);
    assert_eq!(colorparty("-2"), r#"Str("undefined")"#);
    assert_eq!(colorparty("-1000000000"), r#"Str("undefined")"#);
    // An infinite amount saturates the index cast; the wrap must not
    // overflow on it and `at` answers undefined for both.
    assert_eq!(colorparty("-Infinity"), r#"Str("undefined")"#);
    assert_eq!(colorparty("Infinity"), r#"Str("undefined")"#);
}

#[test]
fn colorparty_reads_a_nan_amount_as_the_first_color() {
    // Array.at converts NaN to index 0.
    assert_eq!(colorparty("NaN"), r#"Str("blue")"#);
    assert_eq!(colorparty("0/0"), r#"Str("blue")"#);
}

#[test]
fn min_floors_and_max_caps_as_hers_do() {
    // The reference examples, as one cycle of four steps.
    assert_eq!(
        control(r#"n("0 4 8 12".min(4))"#, "n"),
        vec!["F64(4.0)", "F64(4.0)", "F64(8.0)", "F64(12.0)"]
    );
    assert_eq!(
        control(r#"n("0 4 8 12".max(6))"#, "n"),
        vec!["F64(0.0)", "F64(4.0)", "F64(6.0)", "F64(6.0)"]
    );
}

#[test]
fn min_and_max_take_the_bound_where_the_comparison_fails_as_hers_do() {
    // A word bound does not compare with numbers, so it replaces each of
    // them unchanged.
    for clamp in ["min", "max"] {
        assert_eq!(
            control(&format!(r#"n("0 4".{clamp}("nope"))"#), "n"),
            vec![r#"Str("nope")"#, r#"Str("nope")"#],
            "{clamp} with a word bound"
        );
    }
    // A note name is not read as its MIDI number: it does not compare with
    // a number, so it gives the bound.
    assert_eq!(
        control(r#"n("0 c3 8".min(4))"#, "n"),
        vec!["F64(4.0)", "F64(4.0)", "F64(8.0)"]
    );
    assert_eq!(
        control(r#"n("0 c3 8".max(4))"#, "n"),
        vec!["F64(0.0)", "F64(4.0)", "F64(4.0)"]
    );
}

/// Every part span the first cycle carries. The separator is ` → ` because
/// the bounds are fractions and `0/1/1/1` reads as nothing at all.
fn spans(source: &str) -> Vec<String> {
    haps(source)
        .iter()
        .map(|hap| format!("{} → {}", hap.part.begin, hap.part.end))
        .collect()
}

/// Each voice as `note@begin`, so which voice landed where is visible.
fn voices(source: &str) -> Vec<String> {
    haps(source)
        .iter()
        .map(|hap| {
            let n = hap
                .value
                .as_object()
                .and_then(|o| o.get("n"))
                .and_then(rustel_core::Value::as_f64)
                .unwrap_or(f64::NAN);
            format!("n{n}@{}", hap.part.begin)
        })
        .collect()
}

#[test]
fn strum_spreads_a_chord_symmetrically_about_its_onset() {
    // amt * (2i/l - 1): the first voice `amt` early, the last `amt` late,
    // evenly spaced between. For four voices at 0.1 that is
    // -1/10, -1/30, +1/30, +1/10 - checked against her JavaScript.
    assert_eq!(
        spans(r#"n("0,4,7,12").strum(0.1)"#),
        vec![
            "-1/10 → 9/10",
            "-1/30 → 29/30",
            "1/30 → 31/30",
            "1/10 → 11/10",
        ]
    );
    // The whole moves with the part, so the notes keep their length and the
    // chord arrives spread rather than smeared.
    assert_eq!(
        voices(r#"n("0,4,7,12").strum(0.1)"#),
        vec!["n0@-1/10", "n4@-1/30", "n7@1/30", "n12@1/10"]
    );
}

#[test]
fn strum_leaves_a_single_voice_alone_where_hers_throws() {
    // Intentional difference: the reference returns an undefined `group` for a
    // single voice. Preserve that voice instead of throwing.
    assert_eq!(spans(r#"n("0").strum(0.1)"#), vec!["0/1 → 1/1"]);
    assert_eq!(
        spans(r#"s("bd*2").strum(0.25)"#),
        vec!["0/1 → 1/2", "1/2 → 1/1"]
    );
}

#[test]
fn strum_by_nothing_moves_nothing() {
    assert_eq!(
        spans(r#"n("0,4,7").strum(0)"#),
        spans(r#"n("0,4,7")"#),
        "a zero strum is the pattern itself"
    );
}

#[test]
fn a_negative_strum_reverses_the_order_the_voices_arrive_in() {
    // The times are symmetric either way. Only the assignment of voices to
    // times flips, so a comparison of spans alone would find these two
    // identical.
    assert_eq!(
        spans(r#"n("0,4,7").strum(0.1)"#),
        spans(r#"n("0,4,7").strum(-0.1)"#)
    );
    assert_eq!(
        voices(r#"n("0,4,7").strum(0.1)"#),
        vec!["n0@-1/10", "n4@0/1", "n7@1/10"]
    );
    assert_eq!(
        voices(r#"n("0,4,7").strum(-0.1)"#),
        vec!["n7@-1/10", "n4@0/1", "n0@1/10"],
        "downstroke: the top voice arrives first"
    );
}

/// Every control an event carries, sorted, for the whole first cycle.
fn controls(source: &str) -> Vec<String> {
    haps(source)
        .iter()
        .map(|hap| {
            let object = rustel_core::materialize_js_value(&hap.value);
            let mut pairs: Vec<String> = object
                .as_object()
                .map(|map| {
                    map.iter()
                        .map(|(key, value)| format!("{key}={value:?}"))
                        .collect()
                })
                .unwrap_or_default();
            pairs.sort();
            pairs.join(",")
        })
        .collect()
}

#[test]
fn up_writes_the_keys_it_carries_and_leaves_the_rest_alone() {
    // The difference from `set.mix`, which writes the whole value: `s` and
    // `room` survive because the right-hand pattern never mentions them.
    assert_eq!(
        controls(r#"s("bd").room(0.1).up("1 0.5".as("velocity"))"#),
        vec![
            r#"room=F64(0.1),s=Str("bd"),velocity=F64(1.0)"#,
            r#"room=F64(0.1),s=Str("bd"),velocity=F64(0.5)"#,
        ]
    );
}

#[test]
fn up_takes_the_rhythm_from_the_values_it_is_given() {
    // Half of what it does: one string sets the structure AND the controls,
    // and a rest removes the event rather than writing a hole.
    assert_eq!(controls(r#"s("hh").up("1 ~ 0.7".as("velocity"))"#).len(), 2);
}

#[test]
fn up_overwrites_a_key_the_pattern_already_had() {
    assert_eq!(
        controls(r#"s("bd").gain(0.9).up("0.2".as("gain"))"#),
        vec![r#"gain=F64(0.2),s=Str("bd")"#]
    );
}

#[test]
fn track_and_block_arrange_are_native_extension_constructors() {
    assert_eq!(
        control(r#"track([s("bd"), s("sd")], 1, "F-1")"#, "s"),
        vec![r#"Str("bd")"#, r#"Str("sd")"#]
    );
    assert_eq!(
        control(r#"blockArrange([[[s("bd")], "S"]])"#, "s"),
        vec![r#"Str("bd")"#]
    );
    assert_eq!(
        control(
            r#"blockArrange([[[s("bd")], "S"]], [[m => m.includes('S'), x => x.gain(0.25)]])"#,
            "gain"
        ),
        vec!["F64(0.25)"]
    );
}

/// `track` refuses, with an evaluation error, section lengths that fit one by
/// one but sum past the native fraction range.
#[test]
fn track_refuses_section_lengths_whose_total_leaves_the_fraction_range() {
    let error = runtime()
        .evaluate_score(
            r#"track([s("bd")], 1e38, "0", 1e38, "0")"#,
            &TranspileOptions::default(),
        )
        .expect_err("an overflowing track total must be refused");
    assert!(
        error.to_string().contains("track total cycles overflow"),
        "{error}"
    );
}

#[test]
fn register_func_presets_work_as_globals_and_pattern_methods() {
    let global = control(r#"noisehat()"#, "s");
    assert_eq!(global.len(), 16);
    assert!(global.iter().all(|sound| sound == r#"Str("white")"#));
    let method = control(r#"s("bd*2").noisehat()"#, "s");
    assert_eq!(method.len(), 16);
    assert!(method.iter().all(|sound| sound == r#"Str("white")"#));
    assert_eq!(control(r#"zap()"#, "orbit"), vec!["F64(8.0)"]);
}

#[test]
fn random_extension_nodes_are_repeatable_for_scheduler_requeries() {
    let rt = runtime();
    let source = r#"s("bd hh").gain(0.5).orbit(2).seed(77).glitch(0.2)"#;
    rt.evaluate_score(source, &TranspileOptions::default())
        .expect("evaluate glitch");
    let query = || {
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query glitch")
            .iter()
            .map(rustel_core::Hap::show)
            .collect::<Vec<_>>()
    };
    let first = query();
    assert_eq!(first, query());
    assert!(
        first.iter().all(|hap| hap.contains("orbit:2")),
        "glitch must not perturb routing: {first:?}"
    );

    let first = spans(r#"s("bd*4").seed(77).humanize(0.5)"#);
    assert_eq!(first, spans(r#"s("bd*4").seed(77).humanize(0.5)"#));
}

#[test]
fn glide_carries_the_previous_voice_into_the_next_pitch_envelope() {
    let rt = runtime();
    let compiled = rt
        .evaluate_score(
            r#"note("<c3 d3>").s("sine").glide(1)"#,
            &TranspileOptions::default(),
        )
        .expect("evaluate glide");
    assert!(
        rt.has_active_pattern(),
        "glide evaluation did not leave a native Pattern active: {}",
        compiled.output
    );
    let first = rt
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
        )
        .expect("query glide");
    let second = rt
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ONE,
            Fraction::int(2),
            &[("_cps", 0.5)],
        )
        .expect("query next glide voice");
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert!(first[0].value.get("penv").is_none());
    assert_eq!(
        second[0].value.get("pdecay"),
        Some(&rustel_core::Value::F64(1.0))
    );
    assert!(second[0].value.get("penv").is_some());
}

/// Built-in names are sealed; extension names are not.
mod sealing {
    use super::runtime;
    use rustel_transpiler::TranspileOptions;

    fn refusal(source: &str) -> Option<String> {
        runtime()
            .evaluate_score(source, &TranspileOptions::default())
            .err()
            .map(|error| error.to_string())
    }

    #[test]
    fn a_score_may_not_replace_a_builtin_name() {
        for name in ["fast", "room", "as", "pickRestart", "gain"] {
            let error = refusal(&format!(
                r#"register('{name}', (x) => x)
                   $: s("bd")"#
            ))
            .unwrap_or_else(|| panic!("register('{name}') must be refused"));
            assert!(
                error.contains(&format!("existing built-in '{name}'")),
                "the refusal identifies the built-in name: {error}"
            );
        }
    }

    #[test]
    fn a_score_may_replace_one_of_our_extensions() {
        // User definitions take precedence so new extensions do not break
        // existing scores that already use their names.
        for name in ["fill", "grab", "filtval", "strum", "inspire", "up"] {
            assert!(
                refusal(&format!(
                    r#"register('{name}', (x) => x)
                       $: s("bd")"#
                ))
                .is_none(),
                "register('{name}') must be allowed: it is ours, not strudel.cc's"
            );
        }
    }

    // Pattern prototypes outlive evaluation. Removing an override must restore
    // the built-in extension without requiring a restart.
    #[test]
    fn a_replaced_name_goes_back_when_the_score_stops_replacing_it() {
        use rustel_fraction::Fraction;
        use rustel_jsruntime::Slot;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        let rt = runtime();
        let play = |source: &str| {
            rt.evaluate_score_with_effects_cancellable(
                source,
                &TranspileOptions::default(),
                Duration::from_secs(10),
                &AtomicBool::new(false),
            )
            .unwrap_or_else(|error| panic!("{source}: {error}"));
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("query")
                .iter()
                .filter_map(|hap| hap.whole.map(|whole| whole.end))
                .collect::<Vec<_>>()
        };
        let engine = vec![Fraction::new(1, 2), Fraction::ONE];
        let untouched = vec![Fraction::new(1, 4), Fraction::new(3, 4)];

        // The engine's `fill` extends each whole to the next onset.
        assert_eq!(play(r#"$: s("bd ~ sd ~").fill()"#), engine);

        // The score's own `fill` is the identity, and while it is written that
        // is what the name means.
        assert_eq!(
            play(
                r#"register('fill', (pat) => pat)
                    $: s("bd ~ sd ~").fill()"#
            ),
            untouched
        );

        // Take it away and the engine's name answers again.
        assert_eq!(play(r#"$: s("bd ~ sd ~").fill()"#), engine);

        // A name the score INVENTS is the other half, and it stays:
        // declaring a helper in one update and calling it from the next is
        // how the studio is played. Only a name taken from the engine is
        // given back.
        rt.evaluate_score_with_effects_cancellable(
            r#"register('myOwnThing', (pat) => pat.gain(0.3))
               $: s("bd")"#,
            &TranspileOptions::default(),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .expect("the score plays");
        rt.evaluate_score_with_effects_cancellable(
            r#"$: s("bd").myOwnThing()"#,
            &TranspileOptions::default(),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .expect("a name declared in one update answers in the next");
    }
}

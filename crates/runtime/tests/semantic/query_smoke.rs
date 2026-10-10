//! Headless query regressions beyond the score corpus.

use rustel_fraction::Fraction;
use rustel_runtime::Session;

#[test]
fn continuous_arithmetic_query_report_retains_each_cycle() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("s(rand.mul(3))")
        .expect("evaluate continuous arithmetic");

    let report = session
        .query_report(Fraction::ZERO, Fraction::int(3))
        .expect("query three cycles");
    assert_eq!(report.haps.len(), 3);
    assert!(report.query_threw.is_none());
    assert!(report.haps.iter().all(|hap| hap.whole.is_none()));
    assert!(report.haps.iter().all(|hap| !hap.has_onset));
    assert_eq!(
        report
            .haps
            .iter()
            .map(|hap| hap.part.begin.as_str())
            .collect::<Vec<_>>(),
        ["0/1", "1/1", "2/1"]
    );

    session
        .evaluate("s(rand.mul(3)).sortHapsByPart()")
        .expect("evaluate explicit sort");
    let thrown = session
        .query_report(Fraction::ZERO, Fraction::int(3))
        .expect("report comparator error");
    assert!(thrown.haps.is_empty());
    assert!(thrown.query_threw.is_some());

    session.evaluate("silence").expect("evaluate silence");
    let silent = session
        .query_report(Fraction::ZERO, Fraction::int(3))
        .expect("report healthy silence");
    assert!(silent.haps.is_empty());
    assert!(silent.query_threw.is_none());
}

/// Captured from Strudel core/mini/tonal at upstream 8f81463b9cb5ddd5f117ed7baef6a1fde9445dc2.
/// The busy opening is the written rhythm on both engines; startup fixes must
/// preserve its downbeat and subdivisions rather than changing the pattern.
#[test]
fn layered_score_preserves_the_upstream_drum_grid() {
    let mut session = Session::new().expect("session");
    session.evaluate(r#"
$: stack(
  s("bd*4, [hh oh]*4, ~ cp ~ [cp cp]").bank("RolandTR707").room(.15),
  n("[0 7]*4").scale("Eb2:minor").s("gm_electric_bass_finger").clip(.5).gain(.95),
  n("0 2 4 7 [4 2] 4 2 0").scale("Eb4:minor").s("gm_string_ensemble_2").legato(.9).room(.35),
  chord("Ebm Db B Db").voicing().anchor("c5").struct("[~ x]*4").s("gm_epiano1").room(.4),
  s("cp*4").bank("RolandTR909").gain(".5 .3").room(.4).sometimesBy(.3, ply(2))
).lpf(slider(744,100,10000,1))
$: n("0 ~ 4 ~ 2 ~ ~ 7").scale("Eb:blues").s("gm_music_box").slow(4).room(.85).delay(.5).postgain(2).delay(1)
"#).expect("evaluate layered score");
    let report = session
        .query_report(Fraction::ZERO, Fraction::new(4, 1))
        .expect("query four cycles");
    assert_eq!(report.haps.len(), 233);
    let grid = |bank: &str, sound: &str| {
        let mut spans: Vec<_> = report
            .haps
            .iter()
            .filter_map(|hap| {
                let rustel_runtime::ValueJson::Raw(value) = &hap.value else {
                    return None;
                };
                if value["bank"] != bank || value["s"] != sound {
                    return None;
                }
                assert_eq!(value["cutoff"], 744);
                assert!(hap.has_onset);
                let span = hap.whole.as_ref().expect("discrete sound");
                Some((span.begin.clone(), span.end.clone()))
            })
            .collect();
        spans.sort();
        spans
    };
    let expected = |spans: Vec<(i128, i128)>| {
        let mut spans: Vec<_> = spans
            .into_iter()
            .map(|(begin, end)| (Fraction::new(begin, 8).show(), Fraction::new(end, 8).show()))
            .collect();
        spans.sort();
        spans
    };
    for (sound, offset, length) in [("bd", 0, 2), ("hh", 0, 1), ("oh", 1, 1)] {
        assert_eq!(
            grid("RolandTR707", sound),
            expected(
                (0..16)
                    .map(|beat| (beat * 2 + offset, beat * 2 + offset + length))
                    .collect()
            ),
            "{sound}"
        );
    }
    assert_eq!(
        grid("RolandTR707", "cp"),
        expected(
            (0..4)
                .flat_map(|cycle| {
                    [(2, 4), (6, 7), (7, 8)]
                        .map(|(begin, end)| (cycle * 8 + begin, cycle * 8 + end))
                })
                .collect()
        )
    );
    let claps = [
        0, 1, 2, 4, 5, 6, 7, 8, 10, 12, 14, 15, 16, 18, 20, 22, 24, 25, 26, 28, 30, 32,
    ];
    assert_eq!(
        grid("RolandTR909", "cp"),
        expected(claps.windows(2).map(|pair| (pair[0], pair[1])).collect())
    );
}

/// User-authored function arguments are bridged at every nesting depth.
/// `Value::Function` carries only an id: the host owns the JS function.
#[test]
fn user_authored_function_arguments_are_bridged() {
    for (source, expected) in [
        (r#"s("bd sd hh cp").every(2, x => x.fast(2))"#, 8),
        (r#"s("bd sd").jux(x => x.rev())"#, 4),
        (r#"s("bd sd").superimpose(x => x.fast(2))"#, 6),
        (r#"s("bd sd").layer(x => x.fast(2))"#, 4),
        // Nested inside a partial application.
        (r#"s("bd sd").jux(every(2, x => x.fast(2)))"#, 6),
    ] {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(haps.len(), expected, "{source}");
    }
}

/// A throwing callback fails loudly in its own phase, and never panics.
///
/// The EAGER shape (numeric `every`) has its callback invoked while the
/// score is CONSTRUCTED, so the throw must fail the evaluation itself. Were
/// the host to contain it into a silent fallback pattern, the score would
/// install, nothing downstream could ever report the throw, and the bounce
/// would be silent where the score is not with every exit status zero. The
/// LAZY shape (patterned cycle argument) resolves at query
/// time, where the throw reaches `queryArc` and yields no haps, exactly
/// as strudel.cc's throw-and-silence does.
#[test]
fn a_throwing_callback_fails_loudly_in_its_own_phase() {
    let mut session = Session::new().expect("session");
    let error = session
        .evaluate(r#"s("bd sd").every(1, x => { throw new Error("boom"); })"#)
        .expect_err("an eager callback that throws must fail the evaluation");
    assert!(
        error.to_string().contains("boom"),
        "the evaluation error must carry the callback's message: {error}"
    );

    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("bd sd").every(fastcat(1, 1), x => { throw new Error("boom"); })"#)
        .expect("a lazy callback's throw belongs to the query, not the evaluation");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(haps.is_empty(), "expected an empty result");
}

/// ...while a native combinator REFERENCE keeps working, at any depth.
#[test]
fn native_combinator_references_still_resolve() {
    for (source, expected) in [
        (r#"s("bd sd").every(2, rev)"#, 2),
        (r#"s("bd sd").jux(every(2, rev))"#, 4),
        // 7, not 6: the shifted copy contributes a fragment at each end.
        // Verified hap-for-hap against pinned Node, not guessed.
        (r#"s("bd sd").off(0.25, fast(2))"#, 7),
    ] {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(haps.len(), expected, "{source}");
    }
}

/// `within` applies a transform to PART of a cycle. Two details make it that
/// and not something else, and both were wrong when it was first added here.
#[test]
fn within_transforms_only_the_inside_and_includes_its_end() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"note("c e g b").within(0, 0.5, x => x.rev())"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let got: Vec<String> = haps
        .iter()
        .map(|hap| {
            let note = hap
                .value
                .as_object()
                .and_then(|map| map.get("note"))
                .map(rustel_core::Value::show)
                .unwrap_or_default();
            format!("{}@{}", note, hap.whole.expect("whole").begin)
        })
        .collect();
    // Upstream, run directly: the transform sees ONLY the inside haps, so
    // reversing the first half gives e,c there and leaves g,b alone.
    // Transforming the whole pattern and filtering afterwards moves the wrong
    // events, because rev/fast/early change positions.
    assert_eq!(got, vec!["g@1/4", "e@1/2", "c@3/4", "b@3/4"], "{got:?}");
}

/// `register` publishes to the prototype, not to `globalThis`, and a score is
/// evaluated against `globalThis` - so a combinator needs the free form
/// installed explicitly or only the method spelling works.
#[test]
fn within_is_callable_as_a_free_function_too() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"within(0, 0.5, x => x.rev(), note("c a g e"))"#)
        .expect("the free form must resolve");
    assert!(
        !session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .is_empty()
    );
}

/// `f` is a flat, and note parsing does not trim its input.
#[test]
fn note_helpers_accept_flats_and_do_not_trim() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"stack(pure(isNote('cf4')), pure(isNote(' c4 ')), pure(tokenizeNote('cf4')))"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let shown: Vec<String> = haps.iter().map(|hap| hap.value.show()).collect();
    assert_eq!(shown[0], "true", "an f accidental is a flat");
    assert_eq!(
        shown[1], "false",
        "whitespace is not trimmed, so this is not a note"
    );
    assert!(
        shown[2].contains('f'),
        "tokenizeNote lost the accidental: {shown:?}"
    );
}

/// The remaining parity batch: each of these was MISSING or silently inert.
#[test]
fn midimaps_control_and_zero_weight_are_recognised() {
    for source in [
        r#"midimaps({ mymap: { lpf: 74 } }); note("c").midimap('mymap')"#,
        r#"await midimaps({ m: { lpf: { ccn: 74, min: 0, max: 4000 } } }); note("c")"#,
        r#"defaultmidimap({ lpf: 74 }); note("c").lpf(500)"#,
        // control([ccn, ccv]) - one control for the pair.
        r#"note("c3").control([74, 0.5])"#,
    ] {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|error| panic!("{source} failed: {error}"));
        assert!(
            !session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query")
                .is_empty(),
            "{source} produced no haps"
        );
    }

    // A bad map refuses at registration, in the score's own terms.
    let mut session = Session::new().expect("session");
    let error = session
        .evaluate(r#"midimaps({ m: { lpf: { ccn: 74, min: 1, max: 1 } } }); note("c")"#)
        .expect_err("an empty scaling range cannot scale");
    assert!(error.to_string().contains("scaling range"), "{error}");
}

/// Variadic arguments on an arity-2 method form a sequence.
#[test]
fn extra_arguments_to_an_arity_two_method_sequence() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"n("0 2").scale("C:major", "C:minor")"#)
        .expect("evaluate");
    assert_eq!(
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .len(),
        2,
        "a second scale argument silently deleted the pattern"
    );
}

// Missing required arguments must report an error rather than silently empty
// the pattern. The second list preserves calls that legitimately accept no
// arguments, so broader validation cannot reject them.
#[test]
fn a_method_called_with_no_arguments_is_refused_rather_than_silenced() {
    const REFUSES: &[&str] = &[
        "adsr",
        "as",
        "control",
        "degradeBy",
        "early",
        "fast",
        "filter",
        "filterWhen",
        "ftranspose",
        "inhabit",
        "inhabitmod",
        "late",
        "linger",
        "pick",
        "pickOut",
        "pickReset",
        "pickRestart",
        "pickmod",
        "pickmodOut",
        "pickmodReset",
        "pickmodRestart",
        "rootNotes",
        "scale",
        "scaleTranspose",
        "seed",
        "segment",
        "slow",
        "sometimes",
        "swing",
        "sysex",
        "tag",
        "transpose",
        "tune",
        "undegradeBy",
        "voicings",
        "withBase",
        "xen",
        // The composer matrix, its bare alignment names, and the shortcuts
        // built on it: composing with a sequence of nothing empties the
        // pattern just as surely.
        "add",
        "keep",
        "mask",
        "mul",
        "set",
        "struct",
        "structAll",
        "sub",
        "squeeze",
        // Methods that take a pattern, a callback, or a transformer, and
        // used to read a missing one as `undefined`.
        "appBoth",
        "appLeft",
        "arp",
        "arpWith",
        "bite",
        "choose",
        "choose2",
        "filterHaps",
        "filterValues",
        "fmap",
        "layer",
        "superimpose",
        "withHap",
        "withHaps",
        "withQuerySpan",
        "withValue",
    ];
    const EMPTY_ON_PURPOSE: &[&str] = &[
        // Nothing to say about a drum: these read a value the receiver
        // does not have, which is not the same as being given nothing.
        "ceil", "floor", "round", "voicing", // Silence IS the answer.
        "hush",
    ];
    // A bare call is valid: it uses the default port and the pattern plays
    // on unchanged. The example in its own entry is the bare call.
    const DEFAULTS_WITH_NOTHING: &[&str] = &["osc"];

    let mut session = Session::new().expect("session");
    session.evaluate(r#"s("bd*4")"#).expect("baseline");
    assert_eq!(
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("baseline query")
            .len(),
        4,
        "the baseline pattern changed"
    );

    let empty_call = |session: &mut Session, name: &str| -> Option<usize> {
        let source = format!("s(\"bd*4\").{name}()");
        session.evaluate(&source).ok()?;
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .ok()
            .map(|haps| haps.len())
    };

    for name in REFUSES {
        assert_eq!(
            empty_call(&mut session, name),
            None,
            "{name}() with no arguments must be refused, not played as silence"
        );
    }
    for name in EMPTY_ON_PURPOSE {
        assert_eq!(
            empty_call(&mut session, name),
            Some(0),
            "{name}() is a legitimate call with no arguments and must still be accepted"
        );
    }
    for name in DEFAULTS_WITH_NOTHING {
        assert_eq!(
            empty_call(&mut session, name),
            Some(4),
            "{name}() with no arguments takes its default and plays the pattern on"
        );
    }
}

#[test]
fn function_taking_methods_still_enforce_their_registered_arity() {
    for source in [r#"note("c").every(2)"#, r#"note("c").chunk(4)"#] {
        let mut session = Session::new().expect("session");
        let error = session
            .evaluate(source)
            .expect_err("a missing transformer must be rejected");
        assert!(
            error.to_string().contains("expects 2 inputs but got 1"),
            "{source} returned the wrong error: {error}"
        );
    }
}

#[test]
fn single_quoted_strings_stay_literal_until_the_parser_is_explicitly_enabled() {
    let mut literal = Session::new().expect("session");
    literal.evaluate("fastcat('bd sd')").expect("literal score");
    let haps = literal
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("literal query");
    assert_eq!(haps.len(), 1, "single quotes were mini-parsed by default");
    assert_eq!(haps[0].value.show(), "bd sd");

    let mut opted_in = Session::new().expect("session");
    opted_in
        .evaluate("setStringParser(mini); fastcat('bd sd')")
        .expect("explicit parser opt-in");
    assert_eq!(
        opted_in
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("opt-in query")
            .len(),
        2
    );
}

#[test]
fn variadic_arity_two_methods_use_recursive_sequence_semantics() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("pure('x').fast([1, 2], 3)")
        .expect("nested variadic sequence");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let spans: Vec<_> = haps
        .iter()
        .map(|hap| (hap.part.begin, hap.part.end))
        .collect();
    assert_eq!(
        spans,
        [
            (Fraction::ZERO, Fraction::new(1, 4)),
            (Fraction::new(1, 4), Fraction::new(1, 2)),
            (Fraction::new(1, 2), Fraction::new(2, 3)),
            (Fraction::new(2, 3), Fraction::ONE),
        ]
    );
}

#[test]
fn restored_free_exports_preserve_method_and_curry_semantics() {
    for source in [
        r#"set({ gain: 0.5 }, s("bd"))"#,
        r#"keep({ gain: 0.5 }, s("bd"))"#,
    ] {
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("free composer");
        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        let value = haps[0].value.as_object().expect("control object");
        assert_eq!(
            value.get("s").map(rustel_core::Value::show).as_deref(),
            Some("bd")
        );
        assert_eq!(
            value.get("gain").map(rustel_core::Value::show).as_deref(),
            Some("0.5")
        );
    }

    for (source, expected_haps) in [
        (r#"superimpose([rev], s("bd sd"))"#, 4),
        (r#"within(0)(0.5)(rev)(note("c e"))"#, 2),
        (r#"keepif(true, s("bd"))"#, 1),
        (r#"bite(4, "0 1", s("bd"))"#, 2),
        (r#"control([74, 0.5], note("c"))"#, 1),
    ] {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query")
                .len(),
            expected_haps,
            "{source}"
        );
    }
}

#[test]
fn the_visual_helpers_are_native_and_the_dough_ones_are_absent() {
    // `initHydra`, `clearHydra` and `H` exist in every build, and in NEITHER
    // build do they emulate a browser. Without the `hydra` feature they are a
    // no-op that logs once, so a file carried over from a build that has the
    // visuals window keeps playing its drums instead of failing to load; with
    // it, they record a sketch for a window in another thread. `initDough`
    // has no such native answer and stays absent.
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"
              const result = [
                typeof initHydra,
                typeof clearHydra,
                typeof H,
                typeof initDough,
                isNote(new String('c4')),
                midi2note(-1),
                (() => { try { freqToMidi(440n); return false; } catch (_) { return true; } })(),
              ];
              pure(result)
            "#,
        )
        .expect("native utility surface");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let rustel_core::Value::List(values) = &haps[0].value else {
        panic!("expected result list: {haps:?}");
    };
    for value in &values[..3] {
        assert_eq!(value, &rustel_core::Value::Str("function".into()));
    }
    assert_eq!(values[3], rustel_core::Value::Str("undefined".into()));
    assert_eq!(values[4], rustel_core::Value::Bool(true));
    assert!(matches!(values[5], rustel_core::Value::F64(value) if value.is_nan()));
    assert_eq!(values[6], rustel_core::Value::Bool(true));
}

#[test]
fn a_score_written_for_the_visuals_build_still_plays_without_one() {
    // The point of the no-op: a set carried from a machine with the window
    // compiled in loads, plays, and simply shows nothing.
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"
              await initHydra({ feedStrudel: true })
              $: s("bd sd")
            "#,
        )
        .expect("a score with visuals plays its drums in any build");
    assert_eq!(
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .len(),
        2
    );
}

#[test]
fn binary_nl_keeps_the_list_returning_strudel_definition() {
    let mut session = Session::new().expect("session");
    session.evaluate("binaryNL(5, 4)").expect("binaryNL score");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(
        haps[0].value,
        rustel_core::Value::List(vec![
            rustel_core::Value::F64(0.0),
            rustel_core::Value::F64(1.0),
            rustel_core::Value::F64(0.0),
            rustel_core::Value::F64(1.0),
        ])
    );
}

#[test]
fn midi_method_refuses_ambiguous_ports_and_keeps_per_hap_routing() {
    let mut surface = Session::new().expect("session");
    surface
        .evaluate("pure(typeof midi)")
        .expect("MIDI export shape");
    assert_eq!(
        surface.query(Fraction::ZERO, Fraction::ONE).expect("query")[0].value,
        rustel_core::Value::Str("undefined".into()),
        "only Pattern.prototype.midi is exposed"
    );

    let mut bare = Session::new().expect("session");
    bare.evaluate(r#"note(60).midi()"#).expect("bare midi");
    let haps = bare.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(
        haps[0]
            .value
            .as_object()
            .and_then(|value| value.get("midiport"))
            .map(rustel_core::Value::show)
            .as_deref(),
        Some("0")
    );

    let mut routed = Session::new().expect("session");
    routed
        .evaluate(r#"note(60).midiport('B').midi('A')"#)
        .expect("per-hap route");
    let haps = routed.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(
        haps[0]
            .value
            .as_object()
            .and_then(|value| value.get("midiport"))
            .map(rustel_core::Value::show)
            .as_deref(),
        Some("B")
    );

    for source in [r#"note(60).midi(pure('A'))"#, r#"note(60).midi('A', 'B')"#] {
        let mut session = Session::new().expect("session");
        assert!(session.evaluate(source).is_err(), "{source} was accepted");
    }
}

/// `.osc(0.5)` used to stamp a fractional port onto every hap, and the OSC
/// bridge truncated it toward zero on the way out: every bundle went to UDP
/// port 0 and nothing said so. An unusable argument falls back to the
/// documented default, exactly like the other invalid values always did,
/// and a per-hap `.oscport()` still wins over `.osc()`'s argument.
#[test]
fn osc_method_falls_back_to_the_default_port_for_unusable_arguments() {
    let oscport = |source: &str| {
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect(source);
        session.query(Fraction::ZERO, Fraction::ONE).expect("query")[0]
            .value
            .as_object()
            .and_then(|value| value.get("oscport"))
            .map(rustel_core::Value::show)
    };
    for source in [
        r#"s("bd").osc(0.5)"#,
        r#"s("bd").osc(1.9)"#,
        r#"s("bd").osc(0)"#,
        r#"s("bd").osc(-1)"#,
        r#"s("bd").osc(65536)"#,
        r#"s("bd").osc()"#,
    ] {
        assert_eq!(oscport(source).as_deref(), Some("57120"), "{source}");
    }

    // An explicit whole port still routes, and a per-hap oscport still wins;
    // a fractional per-hap value survives `.osc()` onto the hap unchanged.
    // Refusing it at the bridge rather than truncating it onto the wire is
    // `osc_bridge`'s own test, `a_fractional_port_is_refused_rather_than_truncated`.
    assert_eq!(oscport(r#"s("bd").osc(57121)"#).as_deref(), Some("57121"));
    assert_eq!(
        oscport(r#"s("bd").oscport(9000).osc(57121)"#).as_deref(),
        Some("9000")
    );
    assert_eq!(
        oscport(r#"s("bd").oscport(0.5).osc()"#).as_deref(),
        Some("0.5")
    );
}

#[cfg(feature = "midi")]
fn mapped_ccs(session: &mut Session) -> Vec<(u8, f64)> {
    let report = session.play(2.0).expect("play timeline");
    let onset = report.onsets.first().expect("one onset").clone();
    session
        .with_runtime_settings(|| rustel_runtime::midi_bridge::midi_onset(&onset))
        .expect("MIDI onset")
        .controls
        .mapped_ccs
}

#[test]
#[cfg(feature = "midi")]
fn prebake_midi_maps_are_available_isolated_and_failed_updates_do_not_replace() {
    let mut left = Session::new().expect("left session");
    let mut right = Session::new().expect("right session");
    left.evaluate_prebake(
        "globalThis.__rustelRegisterMidimap = () => { throw new Error('poisoned'); }; \
         defaultmidimap({ lpf: 7 })",
    )
    .expect("fresh prebake registration");
    right
        .evaluate_prebake("defaultmidimap({ lpf: 8 })")
        .expect("isolated registration");

    for session in [&mut left, &mut right] {
        session
            .evaluate(r#"note("c").lpf(0.5).midi('virtual-test')"#)
            .expect("MIDI score");
    }
    assert_eq!(mapped_ccs(&mut left), vec![(7, 0.5)]);
    assert_eq!(mapped_ccs(&mut right), vec![(8, 0.5)]);

    let error = left
        .evaluate_prebake("defaultmidimap({ lpf: { ccn: 9, min: 1, max: 1 } })")
        .expect_err("invalid replacement must refuse");
    assert!(error.to_string().contains("scaling range"), "{error}");
    assert_eq!(mapped_ccs(&mut left), vec![(7, 0.5)]);

    let error = left
        .evaluate(
            r#"defaultmidimap({ lpf: 9 }); throw new Error('reject candidate after mutation')"#,
        )
        .expect_err("a rejected score candidate must not publish its map");
    assert!(error.to_string().contains("reject candidate"), "{error}");
    assert_eq!(
        mapped_ccs(&mut left),
        vec![(7, 0.5)],
        "a rejected candidate leaked its detached settings"
    );
}

#[test]
fn configuration_wrappers_return_undefined_instead_of_becoming_patterns() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"
              const a = defaultmidimap({ lpf: 74 });
              const b = await midimaps({ named: { lpf: 71 } });
              const c = addVoicings('one', { '7': ['0 4 7'] });
              const d = registerVoicings('two', { '7': ['0 3 7'] });
              pure([a, b, c, d].every((value) => value === undefined))
            "#,
        )
        .expect("configuration return values");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(haps[0].value, rustel_core::Value::Bool(true));

    // A configuration-only save is accepted and plays nothing.
    let mut config_only = Session::new().expect("session");
    config_only
        .evaluate("defaultmidimap({ lpf: 74 })")
        .expect("a configuration-only save is accepted");
    assert!(
        config_only
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .is_empty(),
        "a configuration-only expression became an active object pattern"
    );
}

/// `.midi(port)` records the port on every hap. The port must be a literal:
/// a name, an index, or nothing at all.
#[test]
fn midi_records_the_port_it_was_given() {
    for (source, expected) in [
        (r#"note("60").midi('IAC')"#, "IAC"),
        (r#"note("60").midi('IAC Driver Bus 1')"#, "IAC Driver Bus 1"),
        (r#"note("60").midi(0)"#, "0"),
        (r#"note("60").midi(1)"#, "1"),
        (r#"note("60").midi()"#, "0"),
    ] {
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect(source);
        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        let value = haps[0].value.show();
        assert!(
            value.contains(&format!("midiport:{expected}")),
            "{source} recorded {value}, wanted midiport:{expected}"
        );
    }
}

#[test]
fn midi_keeps_supported_connection_options_on_each_hap() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"note(60).midi('IAC', {
                isController: true,
                noteOffsetMs: 10,
                midichannel: 9,
                velocity: 0.8,
                gain: 0.7,
                midimap: 'named',
                latencyMs: 3,
                ignored: 'not carried'
            })"#,
        )
        .expect("MIDI options");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let value = haps[0].value.as_object().expect("control object");
    let options = value
        .get("midiopts")
        .and_then(rustel_core::Value::as_object)
        .expect("MIDI options object");

    assert_eq!(
        options.get("isController"),
        Some(&rustel_core::Value::Bool(true))
    );
    assert_eq!(
        options
            .get("noteOffsetMs")
            .and_then(rustel_core::Value::as_f64),
        Some(10.0)
    );
    assert_eq!(
        options
            .get("midichannel")
            .and_then(rustel_core::Value::as_f64),
        Some(9.0)
    );
    assert_eq!(
        options.get("velocity").and_then(rustel_core::Value::as_f64),
        Some(0.8)
    );
    assert_eq!(
        options.get("gain").and_then(rustel_core::Value::as_f64),
        Some(0.7)
    );
    assert_eq!(
        options.get("midimap").and_then(rustel_core::Value::as_str),
        Some("named")
    );
    assert_eq!(
        options
            .get("latencyMs")
            .and_then(rustel_core::Value::as_f64),
        Some(3.0)
    );
    assert!(!options.contains_key("ignored"));

    for source in [
        r#"note(60).midi('IAC', 'not-options')"#,
        r#"note(60).midi('IAC', [])"#,
        r#"note(60).midi('IAC', pure({ velocity: 0.8 }))"#,
        r#"note(60).midi('IAC', {}, 'extra')"#,
    ] {
        let mut session = Session::new().expect("session");
        assert!(
            session.evaluate(source).is_err(),
            "{source} accepted an invalid MIDI options argument"
        );
    }
}

#[test]
fn a_pattern_port_is_rejected_instead_of_silently_becoming_port_zero() {
    // A name in either quote reaches the hap as written. Recording a port
    // does not open it, so this needs no MIDI subsystem or device.
    for (source, name) in [
        (r#"note("60").midi("loopMIDI")"#, "loopMIDI"),
        (r#"note("60").midi('loopMIDI')"#, "loopMIDI"),
        (r#"note("60").midi("IAC Driver Bus 1")"#, "IAC Driver Bus 1"),
    ] {
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect(source);
        let shown = session.query(Fraction::ZERO, Fraction::ONE).expect("query")[0]
            .value
            .show();
        assert!(
            shown.contains(&format!("midiport:{name}")),
            "{source} recorded {shown}, wanted midiport:{name}"
        );
    }

    // A port that really is a pattern has no name to recover from it, so it is
    // refused. The failure this guards is the quiet one: falling back to the
    // first device plays the set, without a word, into whatever was plugged in
    // first.
    for source in [
        r#"note("60").midi("a b".fast(2))"#,
        r#"note("60").midi(pure("loopMIDI"))"#,
        r#"note("60").midi(sine)"#,
    ] {
        let mut session = Session::new().expect("session");
        assert!(
            session.evaluate(source).is_err(),
            "{source} accepted a pattern where a port name was required"
        );
    }
}

/// `.vst()` keeps the plugin name and the preset name as text, samples each
/// value as a pattern, and a second call adds an effect after the first.
#[test]
fn vst_names_are_text_and_values_are_patterns() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"s("saw*4").vst("Old One", { depth: 1 })
                .vst("My Plugin", { mix: "0.2 0.8", preset: "Warm Pad" })"#,
        )
        .expect("a score with a plugin");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let shown: Vec<String> = haps
        .iter()
        .map(|hap| {
            let plugin = hap.value.as_object().and_then(|map| map.get("vst"));
            plugin.map(rustel_core::Value::show).unwrap_or_default()
        })
        .collect();
    let with_mix = |mix: &str| {
        format!(
            "{{name:Old One params:{{depth:1}}}} {{name:My Plugin params:{{mix:{mix}}} preset:Warm Pad}}"
        )
    };
    assert_eq!(
        shown,
        [
            with_mix("0.2"),
            with_mix("0.2"),
            with_mix("0.8"),
            with_mix("0.8")
        ]
    );

    let mut session = Session::new().expect("session");
    assert!(session.evaluate(r#"s("saw").vst({ mix: 1 })"#).is_err());

    // An instrument and an effect are two requests on one note, and a
    // plugin call starts a chain too. Only the first argument names the
    // plugin: a key `name` in the object is a parameter.
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"vsti("Synth One", { preset: "Bass" }).note("c3").vst("Comp", { name: 0.3 })"#)
        .expect("a score with an instrument and an effect");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let shown = haps[0].value.show();
    assert!(
        shown.contains("vsti:{name:Synth One preset:Bass}"),
        "{shown}"
    );
    assert!(
        shown.contains("vst:[{name:Comp params:{name:0.3}}]"),
        "{shown}"
    );
}

#[test]
fn lfo_config_values_may_be_patterns() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("saw*4").lpf(1200).lfo({ r: "1 8", da: 2000 })"#)
        .expect("a patterned lfo config must evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let rates: Vec<Option<f64>> = haps
        .iter()
        .map(|hap| {
            hap.value
                .as_object()
                .and_then(|map| map.get("lfo"))
                .and_then(rustel_core::Value::as_object)
                .and_then(|lfo| lfo.get("0"))
                .and_then(rustel_core::Value::as_object)
                .and_then(|slot| slot.get("rate"))
                .and_then(rustel_core::Value::as_f64)
        })
        .collect();
    assert_eq!(
        rates,
        vec![Some(1.0), Some(1.0), Some(8.0), Some(8.0)],
        "the rate pattern was not sampled per hap"
    );

    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("saw*2").lpf(1200).lfo({ r: 4, da: 2000, sh: "triangle" })"#)
        .expect("a constant lfo config must still work");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let shown = haps[0].value.show();
    assert!(shown.contains("rate:4"), "constant rate lost: {shown}");
    assert!(shown.contains("triangle"), "constant shape lost: {shown}");
}

#[test]
fn midi_to_freq_is_available_in_scores() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"note("60").freq(midiToFreq(69))"#)
        .expect("midiToFreq score");
    session.query(Fraction::ZERO, Fraction::ONE).expect("query");
}

#[test]
fn loop_at_cps_uses_the_cps_it_was_given() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("bd").loopAtCps(4, 1.5)"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let speed = haps[0]
        .value
        .as_object()
        .and_then(|map| map.get("speed"))
        .and_then(rustel_core::Value::as_f64);
    assert_eq!(speed, Some(0.375));
}

#[test]
fn chop_keeps_squeezed_timing_and_context() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("bd").begin(.2).end(.8).chop(4)"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(haps.len(), 4);
    assert_eq!(
        haps.iter()
            .map(|hap| hap.whole.expect("whole"))
            .collect::<Vec<_>>(),
        [
            rustel_core::TimeSpan::new(Fraction::ZERO, Fraction::new(1, 4)),
            rustel_core::TimeSpan::new(Fraction::new(1, 4), Fraction::new(1, 2)),
            rustel_core::TimeSpan::new(Fraction::new(1, 2), Fraction::new(3, 4)),
            rustel_core::TimeSpan::new(Fraction::new(3, 4), Fraction::ONE),
        ]
    );
    assert!(haps.iter().all(|hap| hap.context.is_empty()));
}

#[test]
fn scramble_stays_native_and_deterministic() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("bd sd hh cp").scramble(4)"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(
        haps.iter()
            .map(|hap| hap.value.as_object().unwrap().get("s").unwrap().show())
            .collect::<Vec<_>>(),
        ["bd", "sd", "sd", "bd"]
    );
}

#[test]
fn visuals_tag_only_their_receiver_inside_an_unlabelled_stack() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"stack(
              s("bd hh sd hh").bank("tr909")._spiral(),
              note("[d2!2 f3 g3]*4").s("sawtooth")._punchcard()._scope()
            )"#,
        )
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(!haps.is_empty());
    for hap in haps {
        let sound = hap
            .value
            .as_object()
            .and_then(|value| value.get("s"))
            .map(rustel_core::Value::show)
            .expect("sound value");
        let expected = if sound == "sawtooth" {
            (1_u64 << 1) | (1_u64 << 2)
        } else {
            1_u64
        };
        assert_eq!(
            hap.ui_visuals_context(),
            expected,
            "unexpected mask for {sound}"
        );
    }
}

#[test]
fn labelled_scope_does_not_tag_the_other_lane() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"$: s("sawtooth").seg(6).decay(0.1).lpf(1000).note("c4")
$: s("sine").note("2").seg(4).scope()"#,
        )
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(!haps.is_empty());
    for hap in haps {
        let sound = hap
            .value
            .as_object()
            .and_then(|value| value.get("s"))
            .map(rustel_core::Value::show)
            .expect("sound value");
        assert_eq!(
            hap.ui_visuals_context(),
            u64::from(sound == "sine"),
            "unexpected scope membership for {sound}"
        );
    }
}

#[test]
fn collecting_a_visual_receiver_keeps_its_membership() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s("bd sd")._scope().collect()"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(!haps.is_empty());
    assert!(
        haps.iter().all(|hap| hap.ui_visuals_context() == 1),
        "collect discarded the receiver's visual membership"
    );
}

#[test]
fn a_bytebeat_expression_survives_as_one_literal() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"s('bytebeat').bbexpr('t*(t>>15^t>>66)')"#)
        .expect("a bytebeat expression must not be mini-parsed");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(
        haps[0].value.show(),
        "s:bytebeat byteBeatExpression:t*(t>>15^t>>66)",
        "the snapshot keeps the whole expression as one value"
    );

    let mut session = Session::new().expect("session");
    session.evaluate(r#"s("bd sd")"#).expect("evaluate");
    assert_eq!(
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .len(),
        2,
        "an ordinary control stopped patterning"
    );
}

#[test]
fn draw_line_renders_a_pattern_as_text() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"pure(drawLine("0 [1 2 3]", 12))"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(
        haps[0].value.show(),
        "|0--123|0--123",
        "drawLine produced the wrong output"
    );
}

#[test]
fn serial_method_keeps_its_optional_arguments_and_has_no_free_export() {
    for (source, expected) in [
        (r#"note(60).serial()"#, "serialport:default"),
        (r#"note(60).serial(9600)"#, "serialbaud:9600"),
        (
            r#"note(60).serial(57600, true, true, 'tty-test')"#,
            "serialport:tty-test",
        ),
    ] {
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect(source);
        let shown = session.query(Fraction::ZERO, Fraction::ONE).expect("query")[0]
            .value
            .show();
        assert!(shown.contains(expected), "{source} produced {shown}");
    }

    let mut session = Session::new().expect("session");
    session
        .evaluate("pure(typeof serial)")
        .expect("serial export shape");
    assert_eq!(
        session.query(Fraction::ZERO, Fraction::ONE).expect("query")[0].value,
        rustel_core::Value::Str("undefined".into())
    );

    let mut session = Session::new().expect("session");
    assert!(
        session
            .evaluate(r#"note(60).serial(1, false, false, 'x', 'extra')"#)
            .is_err(),
        "a fifth serial argument was accepted"
    );

    // Same rule as the MIDI test above: a name is a name in either quote, and
    // naming a device is not opening one, so no serial hardware is involved.
    for source in [
        r#"note(60).serial(9600, false, false, "tty-test")"#,
        r#"note(60).serial(9600, false, false, 'tty-test')"#,
    ] {
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect(source);
        let shown = session.query(Fraction::ZERO, Fraction::ONE).expect("query")[0]
            .value
            .show();
        assert!(
            shown.contains("serialport:tty-test"),
            "{source} produced {shown}, wanted serialport:tty-test"
        );
    }

    // A pattern has no name in it, so it is refused rather than quietly
    // becoming "default" - the first port the system reports.
    for source in [
        r#"note(60).serial(9600, false, false, pure('tty-test'))"#,
        r#"note(60).serial(9600, false, false, "a b".fast(2))"#,
    ] {
        let mut session = Session::new().expect("session");
        assert!(
            session.evaluate(source).is_err(),
            "{source} silently fell back to the first serial port"
        );
    }
}

/// Published prebake libraries lean on the defaults API and `strudelScope`.
/// Browser objects are deliberately absent from the native score realm.
#[test]
fn the_repl_environment_globals_evaluate() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"
            if (getDefaultValue('gain') !== 0.8) throw new Error('seed');
            setDefault('gain', 1);
            if (getDefaultValue('gain') !== 0.8) throw new Error('mid-eval visibility');
            resetDefaultValues();
            if (getDefaultValue('gain') !== 1) throw new Error('post-reset');
            setVersionDefaults('1.0');
            if (getDefaultValue('fanchor') !== 0.5) throw new Error('version defaults');
            strudelScope.myHelper = () => 41;
            if (myHelper() !== 41) throw new Error('strudelScope alias');
            if (typeof document !== 'undefined') throw new Error('browser DOM leaked');
            window.viaWindow = () => 42;
            if (viaWindow() !== 42) throw new Error('window alias');
            $: s("bd")
            "#,
        )
        .expect("the repl environment evaluates");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(
        !haps.is_empty(),
        "the score after the environment code still plays"
    );
}

#[test]
fn browser_keyboard_helpers_are_inert_without_silencing_the_score() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"$: s("bd").whenKey("x", x => x.fast(2))"#)
        .expect("browser-only keyboard helper remains a deterministic no-op");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(
        !haps.is_empty(),
        "an unavailable keyboard did not silence audio"
    );
}

#[test]
fn browser_ui_code_is_refused_rather_than_absorbed() {
    let mut session = Session::new().expect("session");
    let error = session
        .evaluate(r#"document.createElement('div'); $: s("bd")"#)
        .expect_err("browser UI code is refused")
        .to_string();
    assert!(
        error.contains("document"),
        "the error names what is missing: {error}"
    );
}

/// `.dict({...})` hands `voicing()` a chord dictionary as an object. The
/// object is stored whole under `dict` and read back by the voicing. Its
/// entries are not spread into every event as controls.
#[test]
fn a_chord_dictionary_reaches_the_voicing_whole() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"const dic = {'': ['0 7 12 16']}
chord("G").dict(dic).mode('root').anchor("e2").voicing()"#,
        )
        .expect("evaluate");
    let report = session
        .query_report(Fraction::ZERO, Fraction::ONE)
        .expect("query");
    let notes: Vec<String> = report
        .haps
        .iter()
        .map(|hap| {
            let rustel_runtime::ValueJson::Raw(value) = &hap.value else {
                panic!("a raw value");
            };
            assert!(
                value.get("").is_none(),
                "the dictionary leaked into the event: {value}"
            );
            value["note"].as_str().expect("a note name").to_owned()
        })
        .collect();
    assert_eq!(notes, ["G2", "D3", "G3", "B3"], "the score's own voicing");
}

/// A control called with no argument names the values it is given, and an
/// event has none.
///
/// `.gain()` on a bare value pattern is how a player names a column of
/// numbers, and it stays that. On a pattern that is already controls there
/// is no unnamed value to name: storing the whole event under `gain` gave
/// every voice a value it could not read, and the track went silent with
/// the score still looking right. Now it changes nothing.
#[test]
fn a_control_called_with_no_argument_leaves_an_event_alone() {
    let mut session = Session::new().expect("session");

    session.evaluate(r#"s("bd").gain()"#).expect("evaluate");
    let report = session
        .query_report(Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert_eq!(report.haps.len(), 1);
    assert_eq!(
        report.haps[0].value,
        rustel_runtime::ValueJson::Raw(serde_json::json!({"s": "bd"})),
        "an empty control must not store the event under its own name"
    );

    session.evaluate(r#""0.5 1".gain()"#).expect("evaluate");
    let report = session
        .query_report(Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert_eq!(report.haps.len(), 2);
    assert_eq!(
        report.haps[0].value,
        rustel_runtime::ValueJson::Raw(serde_json::json!({"gain": 0.5})),
        "naming a bare value is what an empty control is for"
    );
}

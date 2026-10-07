//! What a score's Hydra calls become.
//!
//! The score realm records; it never draws. These tests read the recording
//! back through the same validation the window thread applies, so a change
//! that would send a window something it cannot rebuild fails here instead of
//! on stage.

#![cfg(feature = "hydra")]

use rustel_hydra::{HYDRA_PROGRAM_VERSION, HydraNode, HydraSource, HydraStatement};
use rustel_runtime::{HydraUpdate, RuntimeError, Session};

#[path = "hydra/numeric_arguments.rs"]
mod numeric_arguments;

fn program(source: &str) -> HydraUpdate {
    let mut session = Session::new().expect("session");
    session
        .evaluate(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let candidate = session
        .take_pending_hydra()
        .unwrap_or_else(|| panic!("{source}: the score staged no visuals recording"));
    HydraUpdate::from_candidate(&candidate).expect("a recorded program reads back")
}

fn evaluate_error(source: &str) -> String {
    let mut session = Session::new().expect("session");
    match session.evaluate(source) {
        Ok(()) => panic!("{source}: expected a refusal"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_sketch_beside_a_pattern_records_one_window_and_one_statement() {
    let update = program(
        r#"
        await initHydra()
        osc(20, 0.1, 1.2).kaleid(5).out(o0)
        $: s("bd*4")
        "#,
    );
    assert_eq!(update.program.statements.len(), 1);
    let HydraStatement::Evaluate { node } = &update.program.statements[0] else {
        panic!(
            "a drawing statement, not an assignment: {:?}",
            update.program.statements[0]
        );
    };
    let HydraNode::Chain { head, args, calls } = node else {
        panic!("a chain: {node:?}");
    };
    assert_eq!(head, "osc");
    assert_eq!(args.len(), 3);
    assert_eq!(
        calls
            .iter()
            .map(|call| call.method.as_str())
            .collect::<Vec<_>>(),
        ["kaleid", "out"]
    );
    // `.out(o0)` carries the output it was given, as a bare Hydra global.
    assert!(matches!(
        calls[1].args.as_slice(),
        [HydraNode::Chain { head, args, calls }] if head == "o0" && args.is_empty() && calls.is_empty()
    ));
}

#[test]
fn the_music_still_plays_beside_the_visuals() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"
            await initHydra()
            osc(10).out()
            $: s("bd sd")
            "#,
        )
        .expect("a score with visuals and drums");
    let haps = session
        .query(
            rustel_fraction::Fraction::ZERO,
            rustel_fraction::Fraction::ONE,
        )
        .expect("query");
    assert_eq!(haps.len(), 2, "the drums are unaffected by the visuals");
}

#[test]
fn a_visuals_only_score_still_reaches_the_window() {
    // The canonical upstream example names no pattern at all. Its effects must
    // survive the arm that installs silence, or the window never opens.
    let update = program("await initHydra()\nosc(4, 0.1, 1.2).out()");
    assert_eq!(update.program.statements.len(), 1);
    assert_eq!(update.program.statements.len(), 1);
}

#[test]
fn h_of_a_pattern_becomes_a_signal_slot() {
    let update = program(
        r#"
        await initHydra()
        osc(20, 0.1, H("<0 0.5 1>")).out()
        "#,
    );
    assert_eq!(update.program.signals, 1);
    assert_eq!(update.signals.len(), 1);
    let HydraStatement::Evaluate {
        node: HydraNode::Chain { args, .. },
    } = &update.program.statements[0]
    else {
        panic!("a chain");
    };
    assert!(matches!(args[2], HydraNode::Signal { slot: 0 }));
}

#[test]
fn h_refuses_a_pattern_the_window_could_not_query_on_its_own() {
    let message = evaluate_error(
        r#"
        await initHydra()
        osc(20, 0.1, H(pure(1).fmap((x) => x * 2))).out()
        "#,
    );
    assert!(
        message.contains("H(...) needs a pattern the engine can query on its own"),
        "{message}"
    );
}

#[test]
fn a_function_argument_crosses_as_its_own_source() {
    let update = program(
        r#"
        await initHydra()
        osc(20).kaleid(() => 3 + Math.sin(time)).out()
        "#,
    );
    let HydraStatement::Evaluate {
        node: HydraNode::Chain { calls, .. },
    } = &update.program.statements[0]
    else {
        panic!("a chain");
    };
    let HydraNode::Source { src } = &calls[0].args[0] else {
        panic!("a function argument: {:?}", calls[0].args[0]);
    };
    assert!(src.contains("Math.sin(time)"), "{src}");
}

#[test]
fn options_describe_the_window() {
    let update = program(
        r#"
        await initHydra({ feedStrudel: true, detectAudio: true, width: 1280, height: 720, strength: 0.8 })
        osc(10).out()
        "#,
    );
    let options = &update.program.options;
    assert!(options.feed_strudel);
    assert!(options.detect_audio);
    assert_eq!(options.width, 1280);
    assert_eq!(options.height, 720);
    assert!((options.strength - 0.8).abs() < 1e-6);
}

#[test]
fn camera_sources_record_the_default_or_an_exact_device_index_without_a_draw_chain() {
    let update = program(
        r#"
        await initHydra()
        s0.initCam()
        s3.initCam(2)
        "#,
    );
    assert_eq!(update.program.version, HYDRA_PROGRAM_VERSION);
    assert_eq!(update.program.statements.len(), 2);
    assert!(matches!(
        &update.program.statements[0],
        HydraStatement::ConfigureSource {
            slot: 0,
            source: HydraSource::Camera { device: None }
        }
    ));
    assert!(matches!(
        &update.program.statements[1],
        HydraStatement::ConfigureSource {
            slot: 3,
            source: HydraSource::Camera { device: Some(2) }
        }
    ));
    assert!(
        !update.program.is_empty(),
        "a source-only score must open long enough to acquire its source"
    );
}

#[test]
fn the_exact_camera_example_records_and_composes_with_its_bare_output_modulator() {
    let update = program(
        r#"
        await initHydra()
        s0.initCam()
        src(s0).saturate(2).contrast(1.3).layer(src(o0).mask(shape(4,2).scale(0.5,0.7).scrollX(0.25)).scrollX(0.001)).modulate(o0,0.001).out(o0)
        "#,
    );
    assert!(matches!(
        &update.program.statements[0],
        HydraStatement::ConfigureSource {
            slot: 0,
            source: HydraSource::Camera { device: None }
        }
    ));
    let HydraStatement::Evaluate { node } = &update.program.statements[1] else {
        panic!("the camera example ends in one drawing chain");
    };
    rustel_hydra::glsl::compose(node, "highp")
        .expect("the exact camera chain, including bare modulate(o0), composes");
}

#[test]
fn the_linked_gallery_constructs_and_exact_image_url_record_as_typed_data() {
    // An independently authored compatibility probe for the constructs used
    // by gallery sketch L4ac2wpf2ElUjjPI. Keep its exact double-quoted image
    // URL: this direct argument is JavaScript data, not mini notation. The
    // third-party sketch itself is deliberately not copied into this tree.
    let update = program(
        r#"
        await initHydra()
        s0.initImage("https://i.imgur.com/zFttbWq.jpg")
        src(s0).modulate(noise(2),()=>a.fft[4]*0.2).out(o0)
        a.setBins(8)
        osc(12,()=>0.01+a.fft[1]*0.02,()=>a.fft[6]*2)
          .modulate(noise(3),()=>a.fft[2]*0.15)
          .mult(shape(6,0.6,0.08).scale(()=>1+a.fft[0]*0.5))
          .out(o1)
        a.hide()
        src(o1).mask(src(o0),0.9).out(o3)
        render(o3)
        "#,
    );

    assert_eq!(update.program.version, HYDRA_PROGRAM_VERSION);
    assert_eq!(
        update.program.statements.len(),
        6,
        "source + first draw + typed eight-bin setup + two draws + render(o3)"
    );
    assert!(matches!(
        &update.program.statements[0],
        HydraStatement::ConfigureSource {
            slot: 0,
            source: HydraSource::ImageUrl { url }
        } if url == "https://i.imgur.com/zFttbWq.jpg"
    ));

    let HydraStatement::Evaluate {
        node: HydraNode::Chain { head, calls, .. },
    } = &update.program.statements[1]
    else {
        panic!("the first picture is a chain");
    };
    assert_eq!(head, "src");
    assert!(matches!(
        calls[0].args.as_slice(),
        [HydraNode::Chain { head, .. }, HydraNode::Source { src }]
            if head == "noise" && src.contains("a.fft[4]")
    ));

    assert_eq!(
        update.program.statements[2],
        HydraStatement::ConfigureAudio { bins: 8 }
    );

    let HydraStatement::Evaluate {
        node: HydraNode::Chain { head, args, .. },
    } = &update.program.statements[3]
    else {
        panic!("the audio-reactive oscillator is a chain");
    };
    assert_eq!(head, "osc");
    assert!(matches!(
        args.get(2),
        Some(HydraNode::Source { src }) if src.contains("a.fft[6]")
    ));

    assert!(matches!(
        &update.program.statements[5],
        HydraStatement::Evaluate {
            node: HydraNode::Chain { head, args, calls }
        } if head == "render"
            && calls.is_empty()
            && matches!(args.as_slice(), [HydraNode::Chain { head, args, calls }]
                if head == "o3" && args.is_empty() && calls.is_empty())
    ));
    for (layer, statement) in [
        &update.program.statements[1],
        &update.program.statements[3],
        &update.program.statements[4],
    ]
    .into_iter()
    .enumerate()
    {
        let HydraStatement::Evaluate { node } = statement else {
            panic!("gallery layer {layer} is a drawing chain");
        };
        rustel_hydra::glsl::compose(node, "highp")
            .unwrap_or_else(|error| panic!("gallery layer {layer} composes: {error}"));
    }
}

#[test]
fn analyser_controls_are_typed_noops_or_clear_errors() {
    let update = program("await initHydra()\na.setBins(1)\na.setBins(16)\na.show()\na.hide()");
    assert_eq!(
        update.program.statements,
        vec![
            HydraStatement::ConfigureAudio { bins: 1 },
            HydraStatement::ConfigureAudio { bins: 16 },
        ],
        "show/hide only control the browser analyser canvas and are native no-ops"
    );

    for (call, expected) in [
        ("a.setBins()", "exactly one numeric"),
        ("a.setBins(1, 2)", "exactly one numeric"),
        ("a.setBins('8')", "numeric whole-number"),
        ("a.setBins(1.5)", "whole-number bin count from 1 through 16"),
        ("a.setBins(0)", "whole-number bin count from 1 through 16"),
        ("a.setBins(17)", "whole-number bin count from 1 through 16"),
        ("a.setCutoff(2)", "not supported by native Hydra yet"),
        ("a.setScale(4)", "not supported by native Hydra yet"),
        ("a.setSmooth(0.4)", "not supported by native Hydra yet"),
        ("a.setMax(10)", "not supported by native Hydra yet"),
        ("a.hide(1)", "accepts no arguments"),
    ] {
        let message = evaluate_error(&format!("await initHydra()\n{call}"));
        assert!(message.contains(expected), "{call}: {message}");
    }
}

#[test]
fn unsupported_global_commands_fail_in_the_score_realm() {
    for (call, expected) in [
        (
            "setResolution(1920, 1080)",
            "Studio chooses its delivery size",
        ),
        ("update(() => {})", "not supported by native Hydra"),
        ("hush(1)", "hush() accepts no arguments"),
    ] {
        let message = evaluate_error(&format!("await initHydra()\n{call}"));
        assert!(message.contains(expected), "{call}: {message}");
    }
}

#[test]
fn source_initialisers_have_strict_arity_types_and_native_support_errors() {
    for (call, expected) in [
        ("s0.initCam(0, 1)", "accepts no arguments"),
        ("s0.initCam('0')", "numeric device index"),
        ("s0.initCam(-1)", "non-negative whole-number"),
        ("s0.initCam(1.5)", "non-negative whole-number"),
        ("s0.initImage()", "exactly one JavaScript string"),
        ("s0.initImage('a', 'b')", "exactly one JavaScript string"),
        ("s0.initImage(42)", "actual JavaScript string"),
        ("s0.initImage('')", "non-empty URL"),
        ("s0.initVideo('clip.mp4')", "not supported by native Hydra"),
        ("s0.initScreen()", "not supported by native Hydra"),
        ("s0.initStream({})", "not supported by native Hydra"),
        ("s0.initCanvas({})", "not supported by native Hydra"),
        ("s0.init()", "not supported by native Hydra"),
        ("s0.clear(1)", "clear() accepts no arguments"),
        ("s0(1).initCam()", "directly on a pristine s0"),
    ] {
        let message = evaluate_error(&format!("await initHydra()\n{call}"));
        assert!(message.contains(expected), "{call}: {message}");
    }
}

#[test]
fn source_clear_is_typed_and_hush_releases_every_acquisition() {
    let update = program(
        "await initHydra()\ns0.initCam()\ns1.initImage('https://example.com/x.png')\ns0.clear()\nhush()",
    );
    assert!(matches!(
        update.program.statements.as_slice(),
        [
            HydraStatement::ConfigureSource { slot: 0, .. },
            HydraStatement::ConfigureSource { slot: 1, .. },
            HydraStatement::ClearSource { slot: 0 },
            HydraStatement::Evaluate {
                node: HydraNode::Chain { head, args, calls }
            },
        ] if head == "hush" && args.is_empty() && calls.is_empty()
    ));
}

#[test]
fn a_source_setup_is_committed_transactionally_with_the_rest_of_its_score() {
    let mut session = Session::new().expect("session");
    let error = session
        .evaluate(
            r#"
            await initHydra()
            s2.initCam(1)
            throw new Error('after the source')
            "#,
        )
        .expect_err("the score throws");
    assert!(error.to_string().contains("after the source"), "{error}");
    assert!(
        session.take_pending_hydra().is_none(),
        "a source from a failed score must never reach the acquisition service"
    );
}

#[test]
fn a_hap_budget_refusal_discards_staged_visuals_before_a_mini_score() {
    let mut session = Session::new().expect("session");
    session.set_query_hap_budget(8).expect("small hap budget");

    let error = session
        .reload_at(
            r#"
            await initHydra()
            s2.initCam(1)
            osc(10).out()
            $: s("hh*16")
            "#,
            false,
            0.0,
        )
        .expect_err("the score exceeds the hap budget");
    assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
    assert_eq!(session.generation(), 0, "the score was not installed");
    assert!(
        session.take_pending_hydra().is_none(),
        "visuals from the refused score must not reach the window"
    );

    // Explicit mini reloads do not evaluate JavaScript or replace the staged
    // Hydra recording. A stale recording would be promoted at this commit.
    session
        .reload_at("bd", true, 0.0)
        .expect("an ordinary mini score still installs");
    assert_eq!(session.generation(), 1);
    assert!(
        session.take_pending_hydra().is_none(),
        "the refused score's camera and drawing must not leak into the next score"
    );
}

/// A second `initHydra()` replaces the settings rather than opening a second
/// anything: there is one terminal to draw behind.
#[test]
fn a_later_init_replaces_the_settings() {
    let update = program(
        r#"
        await initHydra()
        osc(10).out()
        await initHydra({ feedStrudel: true, width: 320, height: 180 })
        noise(4).out()
        "#,
    );
    assert_eq!(update.program.statements.len(), 2);
    assert!(update.program.options.feed_strudel);
    assert_eq!(update.program.options.width, 320);
}

#[test]
fn a_score_that_stops_asking_for_visuals_stops_drawing() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("await initHydra()\nosc(10).out()")
        .expect("visuals");
    assert!(
        !session
            .take_pending_hydra()
            .expect("a recording")
            .is_empty()
    );

    session.evaluate(r#"$: s("bd*4")"#).expect("no visuals");
    let candidate = session
        .take_pending_hydra()
        .expect("every committed score says what it wants on screen");
    assert!(
        candidate.is_empty(),
        "a score without initHydra() is the instruction to close"
    );
}

#[test]
fn clear_hydra_closes_the_window_and_gives_the_names_back() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("await initHydra()\nclearHydra()\n$: s(\"bd\")")
        .expect("open then clear");
    let candidate = session.take_pending_hydra().expect("a recording");
    assert!(candidate.is_empty());
}

#[test]
fn the_visuals_surface_borrows_shape_and_gives_it_back() {
    let mut session = Session::new().expect("session");
    // Before: `shape` is a pattern control.
    session
        .evaluate("$: pure(typeof shape)")
        .expect("plain score");
    session
        .evaluate("await initHydra()\n$: pure(String(typeof shape))")
        .expect("score with visuals");
    // After a score with no visuals, `shape` is a pattern control again.
    // Every evaluation restores the surface first.
    session
        .evaluate("$: pure(1)")
        .expect("score without visuals");
    session
        .evaluate(r#"$: s("bd").shape(0.3)"#)
        .expect("shape is a control again");
}

#[test]
fn speed_remains_a_strudel_control_but_native_hydra_timing_assignments_are_refused() {
    let mut session = Session::new().expect("session");
    // The getter still answers with the pattern control while the Hydra
    // surface is installed.
    session
        .evaluate(
            r#"
            await initHydra()
            osc(10).out()
            $: s("bd*4").speed(2)
            "#,
        )
        .expect("the Strudel speed control remains callable");
    for name in ["speed", "bpm", "fps", "time", "update", "afterUpdate"] {
        let message = evaluate_error(&format!("await initHydra()\n{name} = 2"));
        assert!(
            message.contains(&format!("Hydra's {name}"))
                && message.contains("not supported by native Hydra"),
            "{name}: {message}"
        );
    }
}

#[test]
fn a_chain_never_looks_like_a_promise() {
    // A score is evaluated inside an async function. If a recorded chain
    // answered `.then` with something callable, the runtime would wait for a
    // drawing instruction to resolve, and the whole score would hang.
    let mut session = Session::new().expect("session");
    session
        .evaluate("await initHydra()\n$: pure(typeof osc(1).then)")
        .expect("a chain is not thenable");
    let haps = session
        .query(
            rustel_fraction::Fraction::ZERO,
            rustel_fraction::Fraction::ONE,
        )
        .expect("query");
    assert_eq!(haps[0].value, rustel_core::Value::Str("undefined".into()));
}

#[test]
fn drawing_without_a_window_says_so() {
    let message = evaluate_error("osc(10).out()\n");
    assert!(
        message.contains("no") || message.contains("out"),
        "a score that draws without initHydra() must fail clearly: {message}"
    );
}

/// A real set, recorded whole, in one file: `feedStrudel: 1` rather than
/// `true`, `src(s0)` reading the terminal's own frame back as a texture,
/// `H(...)` driving a kaleidoscope, nested generators, lanes of music and
/// an `all(...)` scope.
#[test]
fn a_whole_set_records_its_sketch_and_keeps_its_music() {
    let source = r#"
await initHydra({feedStrudel:1})
//
src(s0).kaleid(H("<4 5 6>"))
.diff(osc(1,0.5,5))
.modulateScale(osc(2,-0.25,1))
.out()
//

$: s("bd*4,[hh:0:<.5 1>]*8,~ rim").bank("RolandTR909").speed(.9)

$: note("[<g1!3 <bb1 <f1 d1>>>]*3").s("sawtooth")

.room(.75).sometimes(add(note(12))).clip(.3)
.lpa(.05).lpenv(-4).lpf(2000).lpq(8).ftype('24db')

all(x=>x.fft(4).scope({pos:0,smear:.95}))
"#;

    let mut session = Session::new().expect("session");
    session.evaluate(source).expect("the set evaluates");

    // The music is untouched by the visuals.
    let haps = session
        .query(
            rustel_fraction::Fraction::ZERO,
            rustel_fraction::Fraction::ONE,
        )
        .expect("query");
    assert!(
        haps.len() >= 12,
        "the drums, the bass and the hats all still play: {}",
        haps.len()
    );

    let candidate = session.take_pending_hydra().expect("a recording");
    let update = HydraUpdate::from_candidate(&candidate).expect("reads back");

    // `feedStrudel: 1` is the same as `true`: a score writes what it writes.
    assert!(update.program.options.feed_strudel);
    assert_eq!(update.program.signals, 1, "one H(...)");
    assert_eq!(update.program.statements.len(), 1, "one .out()");

    let HydraStatement::Evaluate {
        node: HydraNode::Chain { head, args, calls },
    } = &update.program.statements[0]
    else {
        panic!("a chain: {:?}", update.program.statements[0]);
    };
    assert_eq!(head, "src");
    // `src(s0)` - the terminal's own frame, as a bare Hydra source.
    assert!(matches!(
        args.as_slice(),
        [HydraNode::Chain { head, .. }] if head == "s0"
    ));
    assert_eq!(
        calls
            .iter()
            .map(|call| call.method.as_str())
            .collect::<Vec<_>>(),
        ["kaleid", "diff", "modulateScale", "out"]
    );
    // The kaleidoscope reads the pattern.
    assert!(matches!(
        calls[0].args.as_slice(),
        [HydraNode::Signal { slot: 0 }]
    ));
    // A generator nested inside a method argument is a chain of its own.
    assert!(matches!(
        calls[1].args.as_slice(),
        [HydraNode::Chain { head, args, .. }] if head == "osc" && args.len() == 3
    ));
}

/// A score with no visuals sends an empty recording, which has no options.
/// The empty recording must read back through the protocol like a full one,
/// or the picture never stops.
#[test]
fn the_instruction_to_stop_drawing_survives_the_wire() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("await initHydra()\nosc(10).out()")
        .expect("a score with visuals");
    let sketch = session.take_pending_hydra().expect("a recording");
    assert!(!sketch.is_empty());
    HydraUpdate::from_candidate(&sketch).expect("a sketch reads back");

    session
        .evaluate(r#"$: s("bd*4")"#)
        .expect("a score without visuals");
    let stop = session
        .take_pending_hydra()
        .expect("every committed score says what it wants drawn");
    assert!(stop.is_empty(), "it wants nothing drawn");
    let update = HydraUpdate::from_candidate(&stop)
        .expect("and that has to read back too, or the picture never stops");
    assert!(update.program.is_empty());
    assert!(update.signals.is_empty());
}

/// An array records its `fast`, `smooth`, `offset` and easing marks, and an
/// unmarked one records the defaults. As in hydra-synth, a 0 `.fast` reads as
/// 1, `fit` keeps every mark but `offset`, an unknown easing marks nothing,
/// and an easing function is refused because it cannot run at render time.
#[test]
fn an_animated_array_records_its_marks() {
    let marks = |source: &str| -> Vec<(Vec<f64>, f64, f64, String, f64)> {
        let update = program(&format!("await initHydra()\n{source}"));
        let HydraStatement::Evaluate {
            node: HydraNode::Chain { args, .. },
        } = &update.program.statements[0]
        else {
            panic!("{source}: a drawing chain: {:?}", update.program.statements);
        };
        args.iter()
            .map(|arg| {
                let HydraNode::List {
                    v,
                    speed,
                    smooth,
                    ease,
                    offset,
                } = arg
                else {
                    panic!("{source}: not a list: {arg:?}");
                };
                let entries = v
                    .iter()
                    .map(|entry| match entry {
                        HydraNode::Number { v } => *v,
                        other => panic!("{source}: not a number: {other:?}"),
                    })
                    .collect();
                (entries, *speed, *smooth, ease.clone(), *offset)
            })
            .collect()
    };
    let linear = || "linear".to_owned();
    assert_eq!(
        marks("solid([0.8,4].smooth(),[5,3],[1,20].smooth().fast(0.1)).out(o0)"),
        [
            (vec![0.8, 4.0], 1.0, 1.0, linear(), 0.0),
            (vec![5.0, 3.0], 1.0, 0.0, linear(), 0.0),
            (vec![1.0, 20.0], 0.1, 1.0, linear(), 0.0),
        ]
    );
    assert_eq!(
        marks("osc([10,90].fit(0,1).smooth(0.5).offset(0.25).ease('sin')).out(o0)"),
        [(vec![0.0, 1.0], 1.0, 1.0, "sin".to_owned(), 0.25)]
    );
    assert_eq!(
        marks("osc([10,90].fast(2).offset(0.25).ease('easeInQuad').fit(0,1)).out(o0)"),
        [(vec![0.0, 1.0], 2.0, 1.0, "easeInQuad".to_owned(), 0.0)]
    );
    assert_eq!(
        marks("osc([1,2].fast(0).smooth(0.5).ease('warp')).out(o0)"),
        [(vec![1.0, 2.0], 1.0, 0.5, linear(), 0.0)]
    );
    let refused = evaluate_error(
        r#"
        await initHydra()
        osc([1,2].ease(() => 0.5)).out(o0)
        "#,
    );
    assert!(
        refused.contains("cannot run at render time"),
        "wrong refusal: {refused}"
    );
}

/// Arrays animating `solid`'s channels, multiplied into an image chain that
/// is modulated, graded and blended with its own output, evaluate beside a
/// pattern, read back, and compose.
#[test]
fn a_solid_of_smoothed_arrays_inside_an_image_chain_composes() {
    let update = program(
        r#"
        await initHydra({feedStrudel:1,detectAudio:false})
        s1.initImage('https://example.com/picture.jpg')
        src(s1)
          .modulateKaleid(osc(0.1,1,0.2),12)
          .modulate(noise(12,0.2))
          .scale(0.8)
          .mult(solid([0.8,4].smooth(),[5,3].smooth(),[1,20].smooth().fast(0.1)))
          .saturate(0.3)
          .brightness(-0.8)
          .contrast(0.5)
          .blend(s0)
          .blend(o0,0.6)
          .out(o0)
        $: s("bd")
        "#,
    );
    let HydraStatement::Evaluate { node } = &update.program.statements[1] else {
        panic!(
            "a drawing chain after the image: {:?}",
            update.program.statements
        );
    };
    let shader = rustel_hydra::glsl::compose(node, "highp")
        .unwrap_or_else(|error| panic!("the o0 chain composes: {error}"));
    assert!(shader.contains("solid("), "{shader}");
}

use super::*;
use crate::compose::{Alignment, compose};
use crate::controls::ControlSpec;
use crate::{fastcat, pure, stack};

fn frac((n, d): (i128, i128)) -> Fraction {
    Fraction::new(n, d)
}

/// A name, and the pattern that a test builds on the layers of a
/// sequence.
type Base<'a> = (&'a str, &'a dyn Fn(&[SequenceLayer]) -> Pattern);

fn located_value(value: f64) -> Pattern {
    pure(Value::F64(value)).with_added_context(vec![(value as usize, value as usize + 1)])
}

fn step(begin: (i128, i128), end: (i128, i128), value: f64) -> SequenceStep {
    let mut locations = Locations::new();
    locations.push((value as usize, value as usize + 1));
    SequenceStep::new(frac(begin), frac(end), Value::F64(value), locations)
}

fn layer(period: (i128, i128), steps: Vec<SequenceStep>) -> SequenceLayer {
    SequenceLayer {
        period: frac(period),
        steps,
    }
}

fn spans() -> [(Fraction, Fraction); 8] {
    [
        (Fraction::ZERO, frac((3, 1))),
        (frac((1, 4)), frac((3, 4))),
        (frac((-5, 4)), frac((1, 8))),
        (frac((2, 3)), frac((23, 3))),
        (Fraction::ZERO, frac((13, 1))),
        // A span of no width asks for what sounds at its one time.
        (frac((1, 2)), frac((1, 2))),
        (frac((7, 3)), frac((7, 3))),
        (frac((3, 1)), frac((3, 1))),
    ]
}

fn answers(pattern: &Pattern, begin: Fraction, end: Fraction) -> Vec<String> {
    pattern
        .query_arc(begin, end)
        .iter()
        .map(|hap| {
            format!(
                "{hap:?} {:?} {} {} {:?} {}",
                hap.live_controls(),
                hap.slider_binding(),
                hap.ui_visuals_context(),
                hap.keys,
                hap.pick_lookup().is_some()
            )
        })
        .collect()
}

fn assert_same_answers(name: &str, one_node: &Pattern, nodes: &Pattern) {
    assert_eq!(one_node.steps(), nodes.steps(), "the steps of {name}");
    assert_same_haps(name, one_node, nodes);
}

/// A walked handle answers from the layers, and any other handle from
/// their tables.
fn assert_same_haps(name: &str, one_node: &Pattern, nodes: &Pattern) {
    for (walk, one_node) in [(false, one_node.clone()), (true, one_node.mark_walked())] {
        for (begin, end) in spans() {
            assert_eq!(
                answers(&one_node, begin, end),
                answers(nodes, begin, end),
                "{name} over {begin:?}..{end:?}, walked: {walk}"
            );
        }
    }
}

/// Sequences of numbers: steps, a rest, a chord, a slow layer with a
/// step of three cycles, a layer of one cycle and a half, a value that
/// fills each cycle, and layers with time before and after their steps.
fn sequences() -> Vec<(&'static str, Vec<SequenceLayer>)> {
    vec![
        (
            "steps",
            vec![layer(
                (1, 1),
                vec![
                    step((0, 1), (1, 4), 1.0),
                    step((1, 4), (1, 2), 2.0),
                    step((3, 4), (1, 1), 3.0),
                ],
            )],
        ),
        (
            "a chord and fast steps",
            vec![
                layer(
                    (1, 1),
                    vec![step((0, 1), (1, 2), 4.0), step((0, 1), (1, 2), 5.0)],
                ),
                layer((1, 3), vec![step((0, 1), (1, 1), 6.0)]),
            ],
        ),
        (
            "steps of several cycles",
            vec![layer(
                (4, 1),
                vec![step((0, 1), (3, 4), 7.0), step((3, 4), (1, 1), 8.0)],
            )],
        ),
        (
            "a period of a cycle and a half",
            vec![layer(
                (3, 2),
                vec![step((0, 1), (1, 2), 9.0), step((1, 2), (1, 1), 10.0)],
            )],
        ),
        (
            "one value a cycle",
            vec![layer((1, 1), vec![step((0, 1), (1, 1), 11.0)])],
        ),
        (
            "layers with time around their steps",
            vec![
                layer(
                    (1, 1),
                    vec![step((1, 2), (5, 8), 12.0), step((5, 8), (7, 8), 13.0)],
                ),
                layer((2, 1), vec![step((1, 8), (1, 4), 14.0)]),
                layer((2, 1), vec![step((3, 4), (7, 8), 15.0)]),
            ],
        ),
    ]
}

/// A step is found by the float of where it begins, and by the time
/// itself where two floats are the same and where no float orders a
/// time.
#[test]
fn a_place_orders_by_its_time_where_its_float_does_not() {
    let place = |begin: Fraction| {
        let whole = TimeSpan::new(begin, begin + Fraction::ONE);
        Place::at(whole, None, 0, Locations::new())
    };
    let third = Fraction::new(1, 3);
    // The same float of 32 bits as a third, and a time past the
    // numbers that a float holds.
    let near = third + Fraction::new(1, 1 << 40);
    let fine = Fraction::new(1, (1 << 60) + 1);
    assert_eq!(time_key(third), time_key(near));
    assert!(time_key(fine).is_nan());
    let times = [Fraction::ZERO, fine, third, near, Fraction::ONE];
    for begin in times {
        for time in times {
            let (step, key) = (place(begin), time_key(time));
            assert_eq!(
                step.begins_before(time, key),
                begin < time,
                "{begin:?} {time:?}"
            );
            assert_eq!(
                step.begins_by(time, key),
                begin <= time,
                "{begin:?} {time:?}"
            );
        }
    }
}

fn built(layers: &[SequenceLayer]) -> Pattern {
    sequence(layers.to_vec()).expect("sound layers")
}

/// The layers that a fold dresses alike wear one dress, and layers of
/// two dresses keep two: a name, a set of a control and locations
/// make one dress for all the layers of a sequence.
#[test]
fn the_layers_that_a_fold_dresses_alike_share_one_dress() {
    let dresses = |pattern: &Pattern| -> usize {
        let Node::Sequence(layers) = pattern.node_kind() else {
            panic!("a sequence");
        };
        let mut seen: Vec<*const Dress> = Vec::new();
        for layer in layers.iter() {
            let at = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
            if !seen.contains(&at) {
                seen.push(at);
            }
        }
        seen.len()
    };
    let (_, layers) = sequences().swap_remove(5);
    let spec = crate::controls::ControlSpec::new(["note"]);
    let pattern = named(&built(&layers), &spec, false).expect("a named sequence");
    assert_eq!(dresses(&pattern), 1);
    let control = Value::object([("gain".to_string(), Value::F64(0.5))]);
    let set = compose(&pattern, &pure(control), ComposeOp::Set, Alignment::In);
    assert!(matches!(set.node_kind(), Node::Sequence(_)));
    assert_eq!(dresses(&set), 1);
    let located = located(&set, &[(3, 5)]).expect("a located sequence");
    assert_eq!(dresses(&located), 1);
    // Two sequences of two dresses, one after the other in a stack.
    let other = named(&built(&sequences()[0].1), &spec, false).expect("a named sequence");
    let both = stack(vec![located, other]);
    assert_eq!(dresses(&both), 2);
}

/// A ply of a sequence by a whole count is one sequence, which answers
/// as the join of a pattern for each step. A count that is not whole,
/// a step that is cut, and more steps than a fold lists keep the join.
#[test]
fn a_ply_of_a_sequence_answers_as_the_join_of_each_step() {
    use crate::combinators::ply;
    let is_sequence = |pattern: &Pattern| matches!(pattern.node_kind(), Node::Sequence(_));
    let stepped = |layers: &[SequenceLayer]| built(layers).with_steps(Some(frac((4, 1))));
    for (name, layers) in sequences() {
        // The steps of a chord sound at one time.
        let chord = name == "a chord and fast steps";
        for count in [1, 2, 3, 7] {
            let plied = || ply(&stepped(&layers), frac((count, 1)));
            let folded = plied();
            let name = format!("{name}, each step {count} times");
            assert_eq!(is_sequence(&folded), !chord || count == 1, "{name}");
            assert_same_answers(&name, &folded, &unfolded(plied));
        }
        assert!(!is_sequence(&ply(&built(&layers), frac((3, 2)))), "{name}");
        assert!(
            !is_sequence(&ply(&built(&layers), frac((300, 1)))),
            "{name}"
        );
    }
    let whole = (frac((0, 1)), frac((3, 4)));
    let cut = step((0, 1), (3, 4), 1.0).placed(whole, (frac((1, 4)), frac((1, 2))));
    let cut = built(&[layer((2, 1), vec![cut])]);
    assert!(!is_sequence(&ply(&cut, frac((2, 1)))));
}

/// A lane whose second application raises a query error at each query.
#[test]
fn a_fold_leaves_an_application_that_fails_to_its_node() {
    let steps = || built(&sequences()[0].1);
    let failing = |pattern: Pattern| {
        let text = pure(Value::Str("no number".into()));
        compose(&pattern, &text, ComposeOp::Add, Alignment::In)
    };
    let folded = failing(steps());
    assert!(!matches!(folded.node_kind(), Node::Sequence(_)));
    assert!(!crate::query_error_pending());
    for (begin, end) in spans() {
        assert_eq!(
            format!("{:?}", folded.try_query_arc_sorted(begin, end)),
            format!(
                "{:?}",
                unfolded(|| failing(steps())).try_query_arc_sorted(begin, end)
            ),
        );
    }
}

#[test]
fn a_sequence_answers_as_the_concatenation_of_its_values() {
    let one_node = sequence(vec![layer(
        (1, 1),
        vec![
            step((0, 1), (1, 2), 1.0),
            step((1, 2), (1, 1), 2.0),
            step((1, 2), (1, 1), 3.0),
        ],
    )])
    .expect("ordered steps inside the cycle");
    let nodes = fastcat(vec![
        located_value(1.0),
        unfolded(|| stack(vec![located_value(2.0), located_value(3.0)])),
    ]);
    assert_same_haps("a concatenation", &one_node, &nodes);
}

#[test]
fn layers_answer_as_a_stack_of_concatenations_at_their_speeds() {
    let one_node = sequence(vec![
        layer(
            (1, 2),
            vec![step((0, 1), (1, 2), 1.0), step((1, 2), (1, 1), 2.0)],
        ),
        // A layer with a rest in it, slower than the cycle.
        layer((3, 1), vec![step((1, 2), (1, 1), 3.0)]),
        layer(
            (3, 2),
            vec![
                step((0, 1), (1, 3), 4.0),
                step((1, 3), (2, 3), 5.0),
                step((2, 3), (1, 1), 6.0),
            ],
        ),
    ])
    .expect("ordered steps inside each period");
    let nodes = unfolded(|| {
        stack(vec![
            fastcat(vec![located_value(1.0), located_value(2.0)]).fast(frac((2, 1))),
            fastcat(vec![silence(), located_value(3.0)]).slow(frac((3, 1))),
            fastcat(vec![
                located_value(4.0),
                located_value(5.0),
                located_value(6.0),
            ])
            .slow(frac((3, 2))),
        ])
    });
    assert_same_haps("layers", &one_node, &nodes);
}

#[test]
fn steps_out_of_order_or_outside_the_period_make_no_sequence() {
    for steps in [
        vec![step((1, 2), (1, 1), 1.0), step((0, 1), (1, 2), 2.0)],
        vec![step((0, 1), (3, 2), 1.0)],
        vec![step((1, 2), (1, 2), 1.0)],
        vec![step((-1, 2), (1, 2), 1.0)],
    ] {
        assert!(sequence(vec![layer((1, 1), steps)]).is_none());
    }
    let whole = || vec![step((0, 1), (1, 1), 1.0)];
    assert!(sequence(vec![layer((0, 1), whole())]).is_none());
    assert!(sequence(vec![layer((-1, 2), whole())]).is_none());
    assert!(sequence(Vec::new()).is_some());
}

#[test]
fn a_control_of_a_sequence_answers_as_the_map_of_each_hap() {
    for (name, layers) in sequences() {
        let control = |pattern: Pattern| pattern.map_control(&ControlSpec::new(["n"]), false);
        let folded = control(built(&layers));
        assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
        assert_same_answers(name, &folded, &unfolded(|| control(built(&layers))));
        // Steps that hold a control already, with a constant set on
        // them: the name takes each map with that constant in it.
        let again = |pattern: Pattern| {
            let named = pattern.map_control(&ControlSpec::new(["n"]), true);
            let quiet = ControlSpec::new(["gain"]).apply_to(named, Some(located_value(0.25)));
            quiet.map_control(&ControlSpec::new(["note"]), false)
        };
        assert_same_answers(
            name,
            &again(built(&layers)),
            &unfolded(|| again(built(&layers))),
        );
    }
}

#[test]
fn steps_that_another_control_names_are_another_construction() {
    let (_, layers) = &sequences()[0];
    let named = |name: &str| built(layers).map_control(&ControlSpec::new([name]), false);
    assert!(named("n").same_construction(&named("n")));
    assert!(!named("n").same_construction(&named("note")));
}

/// A scale over steps that `note` or `n` names gives each step its
/// note where it is, and answers as the scale of each hap: steps,
/// notes, steps with accidentals, a step that is none, and a name that
/// is no scale.
#[test]
fn a_scale_of_named_steps_answers_as_the_scale_of_each_hap() {
    let text = |text: &str| Value::Str(text.into());
    let melodies = [
        (
            "steps",
            vec![
                Value::F64(0.0),
                Value::F64(2.0),
                Value::F64(-3.0),
                Value::F64(7.5),
            ],
        ),
        (
            "notes",
            vec![text("c3"), text("eb4"), text("f#2"), text("a")],
        ),
        (
            "steps with accidentals",
            vec![text("0"), text("2#"), text("4b"), text("-1")],
        ),
        (
            "a step that is none",
            vec![Value::F64(1.0), text("x"), Value::Bool(true)],
        ),
        // A hap of no note stays as it is, with no tag of the scale.
        ("no note", vec![Value::F64(1.0), Value::Null]),
        (
            "a map of a value",
            vec![Value::object([("value".into(), Value::F64(2.0))])],
        ),
    ];
    for (name, values) in melodies {
        let steps = || {
            let count = values.len() as i128;
            let steps = values.iter().zip(0..).map(|(value, at)| {
                let (begin, end) = (frac((at, count)), frac((at + 1, count)));
                SequenceStep::new(begin, end, value.clone(), Locations::new())
            });
            built(&[layer((1, 1), steps.collect())])
        };
        for control in ["note", "n", "s"] {
            for scale in ["C:minor", "F#:major", "no such scale"] {
                let scaled = || {
                    let named = steps().map_control(&ControlSpec::new([control]), false);
                    crate::combinators::scale(&named, text(scale))
                };
                let name = format!("{name} under {control} in {scale}");
                assert_same_haps(&name, &scaled(), &unfolded(scaled));
                // A control that is set before the scale stays.
                let quiet = || {
                    let named = steps().map_control(&ControlSpec::new([control]), false);
                    let gain = ControlSpec::new(["gain"]);
                    let quiet = gain.apply_to(named, Some(located_value(0.25)));
                    crate::combinators::scale(&quiet, text(scale))
                };
                assert_same_haps(&name, &quiet(), &unfolded(quiet));
            }
        }
    }
}

#[test]
fn locations_of_a_sequence_answer_as_their_own_node() {
    for (name, layers) in sequences() {
        let located = |pattern: Pattern| pattern.with_added_context(vec![(70, 72), (80, 81)]);
        let folded = located(built(&layers));
        assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
        assert_same_answers(name, &folded, &unfolded(|| located(built(&layers))));
    }
}

#[test]
fn a_constant_applied_to_a_sequence_answers_as_the_application() {
    let controls = |key: &str, value: f64| {
        pure(Value::F64(value))
            .with_added_context(vec![(90, 93)])
            .map_control(&ControlSpec::new([key]), false)
    };
    for (name, layers) in sequences() {
        // Numbers under arithmetic, and control objects that a
        // constant sets, adds to and sets again.
        let chains: [(&str, &dyn Fn(Pattern) -> Pattern); 3] = [
            // A chain of constants that each set a control: a key that
            // is new, and a key that an earlier constant set.
            ("set controls", &|pattern: Pattern| {
                let notes = pattern.map_control(&ControlSpec::new(["n"]), false);
                [("gain", 0.8), ("room", 0.3), ("gain", 0.25), ("n", 7.0)]
                    .into_iter()
                    .fold(notes, |so_far, (key, value)| {
                        compose(
                            &so_far,
                            &controls(key, value),
                            ComposeOp::Set,
                            Alignment::In,
                        )
                    })
            }),
            ("arithmetic", &|pattern: Pattern| {
                let scaled = compose(
                    &pattern,
                    &pure(Value::F64(3.0)),
                    ComposeOp::Mul,
                    Alignment::In,
                );
                compose(&scaled, &located_value(0.5), ComposeOp::Add, Alignment::In)
            }),
            ("controls", &|pattern: Pattern| {
                let notes = pattern.map_control(&ControlSpec::new(["n"]), false);
                let set = compose(
                    &notes,
                    &controls("gain", 0.8),
                    ComposeOp::Set,
                    Alignment::In,
                );
                let added = compose(&set, &controls("n", 12.0), ComposeOp::Add, Alignment::In);
                compose(
                    &added,
                    &controls("gain", 0.25),
                    ComposeOp::Set,
                    Alignment::In,
                )
            }),
        ];
        for (chain_name, chain) in chains {
            let folded = chain(built(&layers));
            assert!(
                matches!(folded.node_kind(), Node::Sequence(_)),
                "{chain_name} of {name}"
            );
            assert_same_answers(
                &format!("{chain_name} of {name}"),
                &folded,
                &unfolded(|| chain(built(&layers))),
            );
        }
    }
}

#[test]
fn values_applied_to_a_sequence_answer_as_the_application() {
    let controls =
        |pattern: Pattern, key: &str| pattern.map_control(&ControlSpec::new([key]), false);
    let mut folds = Vec::new();
    for (name, layers) in sequences() {
        for (value_name, value_layers) in sequences() {
            let name = format!("{name} with {value_name}");
            // Numbers under arithmetic, and control objects that the
            // values set.
            let added = |pattern: Pattern| {
                compose(
                    &pattern,
                    &built(&value_layers),
                    ComposeOp::Add,
                    Alignment::In,
                )
            };
            let set = |pattern: Pattern| {
                compose(
                    &controls(pattern, "n"),
                    &controls(built(&value_layers), "gain"),
                    ComposeOp::Set,
                    Alignment::In,
                )
            };
            let chains: [&dyn Fn(Pattern) -> Pattern; 2] = [&added, &set];
            for chain in chains {
                let folded = chain(built(&layers));
                if matches!(folded.node_kind(), Node::Sequence(_)) {
                    folds.push(name.clone());
                }
                assert_same_answers(&name, &folded, &unfolded(|| chain(built(&layers))));
            }
        }
    }
    // Steps meet steps of another period, and steps that they cut.
    for name in [
        "steps with steps of several cycles",
        "steps of several cycles with steps",
        "steps with a period of a cycle and a half",
        "a period of a cycle and a half with steps of several cycles",
        "a chord and fast steps with one value a cycle",
    ] {
        assert_eq!(
            folds.iter().filter(|fold| *fold == name).count(),
            2,
            "{name}"
        );
    }
    // Values of two layers answer each step layer by layer.
    assert!(
        !folds
            .iter()
            .any(|fold| fold.ends_with("with a chord and fast steps"))
    );
}

#[test]
fn a_structure_of_a_sequence_answers_as_the_application() {
    // Steps that keep and steps that drop, in one cycle and in a period
    // of a cycle and a half.
    let structures = || {
        vec![
            (
                "a structure of one cycle",
                vec![layer(
                    (1, 1),
                    vec![
                        step((0, 1), (1, 4), 1.0),
                        step((1, 4), (1, 2), 0.0),
                        step((1, 2), (1, 1), 1.0),
                    ],
                )],
            ),
            (
                "a structure of two layers",
                vec![
                    layer(
                        (3, 2),
                        vec![step((0, 1), (1, 3), 0.0), step((1, 3), (1, 1), 1.0)],
                    ),
                    layer((1, 2), vec![step((1, 4), (3, 4), 1.0)]),
                ],
            ),
        ]
    };
    // The steps as they are, and with a number that fills each cycle
    // added: a step of several cycles then has a part in each of them.
    let receiver = |layers: &[SequenceLayer], cut: bool| {
        let steps = built(layers);
        if !cut {
            return steps;
        }
        let each_cycle = built(&[layer((1, 1), vec![step((0, 1), (1, 1), 16.0)])]);
        compose(&steps, &each_cycle, ComposeOp::Add, Alignment::In)
    };
    let mut folds = Vec::new();
    for (name, layers) in sequences() {
        for (structure_name, structure) in structures() {
            // `struct` and `mask`, and numbers that the values add to.
            let chains: [(&str, ComposeOp, Alignment); 3] = [
                ("struct", ComposeOp::KeepIf, Alignment::Out),
                ("mask", ComposeOp::KeepIf, Alignment::In),
                ("add out", ComposeOp::Add, Alignment::Out),
            ];
            for ((chain_name, op, how), cut) in chains
                .into_iter()
                .flat_map(|chain| [(chain, false), (chain, true)])
            {
                let cut_name = if cut { " cut at each cycle" } else { "" };
                let name = format!("{chain_name} of {name}{cut_name} with {structure_name}");
                let chain = || compose(&receiver(&layers, cut), &built(&structure), op, how);
                let folded = chain();
                if matches!(folded.node_kind(), Node::Sequence(_) | Node::Silence) {
                    folds.push(name.clone());
                }
                assert_same_answers(&name, &folded, &unfolded(chain));
            }
        }
    }
    // Values of two layers answer each step layer by layer, and so do
    // functions of two layers: any other pair is one node.
    assert!(folds.len() >= 42, "{}: {folds:#?}", folds.len());
}

#[test]
fn a_scale_of_a_sequence_answers_as_the_map_of_each_hap() {
    let scaled = |pattern: Pattern, name: &str| {
        let degrees = pattern.map_control(&ControlSpec::new(["n"]), false);
        crate::combinators::scale(&degrees, Value::Str(name.into()))
    };
    for (name, layers) in sequences() {
        // The scale names each note and its own key of the context,
        // and controls and another scale come after it.
        let chains: [(&str, &dyn Fn(Pattern) -> Pattern); 3] = [
            ("a scale", &|pattern: Pattern| scaled(pattern, "C:minor")),
            ("controls after a scale", &|pattern: Pattern| {
                let gain = pure(Value::F64(0.5)).map_control(&ControlSpec::new(["gain"]), false);
                compose(
                    &scaled(pattern, "D:dorian"),
                    &gain,
                    ComposeOp::Set,
                    Alignment::In,
                )
            }),
            ("a scale of numbers", &|pattern: Pattern| {
                crate::combinators::scale(&pattern, Value::Str("E:major".into()))
            }),
        ];
        for (chain_name, chain) in chains {
            let folded = chain(built(&layers));
            assert!(
                matches!(folded.node_kind(), Node::Sequence(_)),
                "{chain_name} of {name}"
            );
            assert_same_answers(
                &format!("{chain_name} of {name}"),
                &folded,
                &unfolded(|| chain(built(&layers))),
            );
        }
    }
    // A step that is no degree drops its hap at each query: the map
    // keeps its node.
    let text = |pattern: Pattern| {
        let words = pattern.fmap(|_| Value::Str("no degree".into()));
        crate::combinators::scale(&words, Value::Str("C:minor".into()))
    };
    let dropped = text(built(&sequences()[0].1));
    assert!(!matches!(dropped.node_kind(), Node::Sequence(_)));
    assert_same_answers(
        "a step that is no degree",
        &dropped,
        &unfolded(|| text(built(&sequences()[0].1))),
    );
}

/// A control of a constant takes the short way to its fold. The fold is
/// the one that the composition of the control's pattern gives, and it
/// answers as the nodes it stands for.
#[test]
fn a_control_of_a_constant_folds_as_its_composition_folds() {
    let gain = ControlSpec::new(["gain"]);
    let sound = ControlSpec::new(["s"]);
    let constants: [(&str, &dyn Fn() -> Pattern); 3] = [
        ("a number", &|| pure(Value::F64(0.5))),
        ("a number at a place", &|| located_value(0.25)),
        ("one step", &|| {
            built(&[layer((1, 1), vec![step((0, 1), (1, 1), 3.0)])])
        }),
    ];
    for (name, layers) in sequences() {
        let notes = || built(&layers).map_control(&ControlSpec::new(["n"]), false);
        for (constant_name, constant) in constants {
            let name = format!("{constant_name} on {name}");
            let set = gain.apply(&notes(), Some(constant()));
            let composed = compose(
                &notes(),
                &gain.pattern(&constant()),
                ComposeOp::Set,
                Alignment::In,
            );
            assert_eq!(
                matches!(set.node_kind(), Node::Sequence(_)),
                matches!(composed.node_kind(), Node::Sequence(_)),
                "{name}"
            );
            assert_same_haps(&name, &set, &composed);
            // Each control has a place of its own, so their order shows.
            let chain = |pattern: Pattern| {
                let quiet = gain.apply(&pattern, Some(constant()));
                sound.apply(&quiet, Some(located_value(7.0)))
            };
            assert_same_answers(&name, &chain(notes()), &unfolded(|| chain(notes())));
        }
    }
}

/// A part lies in one cycle in every turn of a period when each turn,
/// read on its own, holds the part in one cycle.
#[test]
fn a_part_lies_in_cycles_when_each_turn_holds_it_in_one() {
    for period in [
        (1, 1),
        (3, 1),
        (1, 2),
        (3, 2),
        (2, 3),
        (5, 3),
        (7, 4),
        (5, 6),
    ] {
        let period = frac(period);
        let twelfths = (period * frac((12, 1))).floor().numer();
        for (begin, end) in
            (0..twelfths).flat_map(|begin| (begin + 1..=twelfths).map(move |end| (begin, end)))
        {
            let part = TimeSpan::new(frac((begin, 12)), frac((end, 12)));
            let in_each_turn = (0..period.denom()).all(|turn| {
                let start = period * frac((turn, 1));
                part.end + start <= (part.begin + start).floor() + Fraction::ONE
            });
            let place = Place::at(part, None, 0, Locations::new());
            assert_eq!(
                lie_in_cycles(period, &[place]),
                in_each_turn,
                "{part:?} of a period of {period:?}"
            );
        }
    }
}

/// A step whose time in cycles leaves the native range is no sequence.
#[test]
fn a_time_outside_the_native_range_is_no_sequence() {
    let far = layer((i128::MAX, 1), vec![step((2, 3), (1, 1), 0.0)]);
    assert!(sequence(vec![far]).is_none());
}

/// The four equal steps of a cycle, at a source that reads no span
/// of a time finer than a part in a thousand.
struct Quarters;

impl StepSource for Quarters {
    fn count(&self) -> usize {
        4
    }

    fn steps(
        &self,
        begin: Fraction,
        end: Fraction,
        each: &mut dyn FnMut(SourceStep),
    ) -> Option<()> {
        for quarter in 0..4 {
            if begin.denom() > 1000 && quarter == 1 {
                return None;
            }
            let (from, to) = (frac((quarter, 4)), frac((quarter + 1, 4)));
            let holds = from <= begin && begin < to;
            if holds || (begin < from && from < end) {
                let mut locations = Locations::new();
                locations.push((quarter as usize, quarter as usize + 1));
                each(SourceStep {
                    begin: from,
                    end: to,
                    value: quarter as u32,
                    locations,
                });
            }
        }
        Some(())
    }
}

/// A layer that reads its steps from a source answers as a layer that
/// lists the same steps: for a query that walks it, for a table of it,
/// and for a fold of it, which both list the steps.
#[test]
fn steps_at_a_source_answer_as_the_same_steps_in_a_list() {
    let values = (0..4).map(|value| Value::F64(f64::from(value)));
    let sourced = SourcedLayer {
        period: Fraction::ONE,
        values: values.collect(),
        in_cycles: true,
        steps: Arc::new(Quarters),
    };
    let read = sequence_of(vec![GivenLayer::Sourced(sourced)]).expect("a sound layer");
    let steps = (0..4).map(|at| step((at, 4), (at + 1, 4), at as f64));
    let listed = built(&[layer((1, 1), steps.collect())]);
    let twice = |pattern: &Pattern| faster(pattern, frac((2, 1))).expect("a sequence");
    let instant = (frac((1, 4)), frac((1, 4)));
    // The source reads no span of a time this fine: the layer lists
    // its steps for it.
    let fine = (frac((1, 1024)), frac((3, 4)));
    // Folds that move no step leave the steps at their source: a
    // name, a scale, which tags each hap, and a location.
    let noted = |pattern: &Pattern| {
        let steps = pattern.map_control(&ControlSpec::new(["n"]), false);
        let notes = crate::combinators::scale(&steps, Value::Str("C:minor".into()));
        notes.with_added_context(vec![(7, 8)])
    };
    // A walked handle reads the source at each query, with no table.
    let (walked, noted_walk) = (read.mark_walked(), noted(&read).mark_walked());
    for (begin, end) in spans().into_iter().chain([instant, fine]) {
        assert_eq!(
            answers(&walked, begin, end),
            answers(&listed, begin, end),
            "a walk over {begin:?}..{end:?}"
        );
        assert_eq!(
            answers(&noted_walk, begin, end),
            answers(&noted(&listed), begin, end),
            "a walk of notes over {begin:?}..{end:?}"
        );
    }
    for (begin, end) in spans().into_iter().chain([instant]) {
        assert_eq!(
            answers(&read, begin, end),
            answers(&listed, begin, end),
            "over {begin:?}..{end:?}"
        );
        assert_eq!(
            answers(&twice(&read), begin, end),
            answers(&twice(&listed), begin, end),
            "twice as fast, over {begin:?}..{end:?}"
        );
    }
}

#[test]
fn a_short_source_in_a_long_period_keeps_its_rests_and_locations() {
    let period = Fraction::int(8);
    let read = sequence_of(vec![GivenLayer::Sourced(SourcedLayer {
        period,
        values: (0..4).map(|at| Value::F64(f64::from(at))).collect(),
        in_cycles: true,
        steps: Arc::new(Quarters),
    })])
    .unwrap();
    let steps = (0..4)
        .map(|at| step((at, 32), (at + 1, 32), at as f64))
        .collect();
    let listed = built(&[SequenceLayer { period, steps }]);
    let located = |pattern: Pattern| pattern.with_added_context(vec![(70, 72)]).mark_walked();
    let (read, listed) = (located(read), located(listed));
    for eighth in -67..=67 {
        let begin = Fraction::new(eighth, 8);
        for width in [Fraction::ZERO, frac((1, 7)), frac((17, 2))] {
            let end = begin + width;
            assert_eq!(
                answers(&read, begin, end),
                answers(&listed, begin, end),
                "{begin:?}..{end:?}"
            );
        }
    }
}

/// Equal data values of a layer are one value, which its steps share.
#[test]
fn equal_values_of_a_layer_are_one_value() {
    let steps = [0.0, 1.0, 0.0, 1.0, 2.0].iter().enumerate();
    let steps = steps.map(|(at, value)| {
        let at = at as i128;
        step((at, 5), (at + 1, 5), *value)
    });
    let pattern = built(&[layer((1, 1), steps.collect())]);
    let Node::Sequence(layers) = pattern.node_kind() else {
        panic!("the steps are no sequence");
    };
    let shared: Vec<u32> = layers[0].places().iter().map(|place| place.value).collect();
    assert_eq!(shared, [0, 1, 0, 1, 2]);
    assert_eq!(layers[0].held_values().len(), 3);
}

/// Sets of constant controls in a row on a pattern that is no sequence
/// are one meeting of one node, and it answers as the applications one
/// by one:
/// for maps of controls under a control of a signal, for numbers, and
/// for strings and a function of the script, which no meeting takes in
/// place. The last control of the chain sets the key of the function,
/// so the lookup of each hap shows whether the controls met it in turn.
#[test]
fn sets_of_constants_in_a_row_are_one_meeting() {
    use crate::controls::{ChainArgument, apply_chain};
    let control = |name: &str| ControlSpec::new([name]);
    // A filter keeps its node, so no control folds into the steps.
    let kept = |layers: &[SequenceLayer]| built(layers).filter_haps(|_| true);
    let function = Value::Function(std::sync::Arc::new(crate::value::FunctionRef::js(7)));
    let bases: [Base<'_>; 4] = [
        ("controls under a signal", &|layers| {
            let notes = built(layers).map_control(&control("n"), false);
            // A map of a signal is no wave, which a control goes under.
            let cutoff = crate::signal::sine().fmap(Value::clone);
            control("lpf").apply(&notes, Some(cutoff))
        }),
        ("numbers", &kept),
        ("strings", &|layers| {
            kept(layers).fmap(|value| Value::Str(value.show().into()))
        }),
        ("a function of the script", &|layers| {
            // A handle of the function for each hap.
            let function = function.clone();
            kept(layers).fmap(move |value| {
                Value::object([
                    ("n".to_owned(), value.clone()),
                    ("room".to_owned(), function.clone()),
                ])
            })
        }),
    ];
    // The second gain takes the place of the first, and each control
    // has a place of its own, so their order shows.
    let chain = |pattern: Pattern| {
        ["gain", "pan", "gain", "room"].iter().enumerate().fold(
            pattern,
            |pattern, (index, name)| {
                control(name).apply(&pattern, Some(located_value(index as f64 + 0.5)))
            },
        )
    };
    let specs = ["gain", "pan", "gain", "room"].map(control);
    let row = |pattern: Pattern| {
        let numbers = (0..).map(|index| ChainArgument::Number(f64::from(index) + 0.5));
        let calls: Vec<_> = specs.iter().zip(numbers).collect();
        apply_chain(pattern, &calls)
    };
    for (name, layers) in sequences() {
        for (base_name, base) in bases {
            let name = format!("{base_name} of {name}");
            let joined = chain(base(&layers));
            let Node::Constants(node) = joined.node_kind() else {
                panic!("{name} is no run of constants");
            };
            let (under, run) = (&node.0, &node.1);
            assert_eq!(run.len(), 1, "{name}");
            assert!(!matches!(under.node_kind(), Node::Constants(..)), "{name}");
            assert_same_answers(&name, &joined, &unfolded(|| chain(base(&layers))));
            // A row on a stack is one such meeting on the pattern of
            // the stack that takes no control.
            let stacked = || {
                let notes = built(&layers).map_control(&control("n"), false);
                crate::stack(vec![notes, base(&layers)])
            };
            let name = format!("a row on a stack of {name}");
            assert_same_answers(&name, &row(stacked()), &unfolded(|| row(stacked())));
        }
    }
}

/// A chain of controls answers as its controls applied in turn, with
/// no fold: for constants in a row, for a pattern between them, for a
/// control that a union keeps out, and for the name of a wave over the
/// steps. The steps of a sequence, of a pick and under a wave hold the
/// constants, and a pattern that keeps its node holds none.
#[test]
fn a_chain_of_controls_answers_as_each_control_in_turn() {
    use crate::controls::{ChainArgument, apply_chain};
    let number = |name: &'static str, n: f64| (name, ChainArgument::Number(n));
    let pattern = |name: &'static str, values: Pattern| (name, ChainArgument::Pattern(values));
    let two_steps = || {
        built(&[layer(
            (1, 1),
            vec![step((0, 1), (1, 2), 20.0), step((1, 2), (1, 1), 21.0)],
        )])
    };
    type Chain = Vec<(&'static str, ChainArgument)>;
    let chains: [(&str, &dyn Fn() -> Chain); 4] = [
        ("constants in a row", &|| {
            vec![
                number("gain", 0.5),
                pattern("s", located_value(7.0)),
                number("pan", 0.25),
                number("gain", 0.75),
                pattern("room", located_value(3.0)),
            ]
        }),
        ("a pattern between constants", &|| {
            vec![
                number("gain", 0.5),
                number("pan", 0.25),
                pattern("cutoff", two_steps()),
                pattern("room", located_value(3.0)),
                number("gain", 1.5),
            ]
        }),
        ("a control named value", &|| {
            vec![
                number("gain", 0.5),
                number("value", 2.0),
                number("pan", 0.25),
            ]
        }),
        ("the name of the wave", &|| {
            vec![
                number("gain", 0.5),
                number("lpf", 300.0),
                number("pan", 0.25),
            ]
        }),
    ];
    let control = |name: &str| ControlSpec::new([name]);
    let notes = |layers: &[SequenceLayer]| built(layers).map_control(&control("n"), false);
    let kept = |layers: &[SequenceLayer]| notes(layers).filter_haps(|_| true);
    let waved = |layers: &[SequenceLayer]| {
        control("lpf").apply(&notes(layers), Some(crate::signal::sine()))
    };
    let picked = |layers: &[SequenceLayer]| {
        let selector = built(&[layer(
            (2, 1),
            vec![step((0, 1), (1, 2), 0.0), step((1, 2), (1, 1), 1.0)],
        )]);
        let lookup = crate::PickLookup::Array {
            enumerable_len: 2,
            length: 2,
            entries: vec![(0, notes(layers)), (1, waved(layers))],
        };
        let (index, join) = (crate::PickIndexMode::Clamp, crate::JoinMode::Restart);
        crate::pick(selector, lookup, index, join)
    };
    // A run of sets holds each control apart, also those of a chain.
    let run =
        |layers: &[SequenceLayer]| control("gain").apply(&kept(layers), Some(located_value(0.125)));
    // A pattern of the stack that takes no control meets each row as
    // one meeting, which strings meet control by control.
    let stacked = |layers: &[SequenceLayer]| {
        crate::stack(vec![
            notes(layers),
            waved(layers),
            picked(layers),
            kept(layers).fmap(|value| Value::Str(value.show().into())),
        ])
    };
    let segmented =
        |layers: &[SequenceLayer]| crate::combinators::segment(&stacked(layers), frac((4, 1)));
    // Strings meet each control of a run one by one, with the name of
    // the wave over the run at its place.
    let strings = |layers: &[SequenceLayer]| {
        let named = kept(layers).fmap(|value| Value::Str(value.show().into()));
        let quiet = control("gain").apply(&named, Some(located_value(0.125)));
        control("lpf").apply(&quiet, Some(crate::signal::sine()))
    };
    let bases: [Base<'_>; 8] = [
        ("a sequence", &notes),
        ("steps under a wave", &waved),
        ("a pick", &picked),
        ("a stack", &stacked),
        ("a segment of a stack", &segmented),
        ("a pattern that keeps its node", &kept),
        ("a wave over a run of sets on strings", &strings),
        ("a run of sets", &run),
    ];
    for (name, layers) in sequences() {
        let (_, row) = chains[0];
        let specs: Vec<ControlSpec> = row().iter().map(|(name, _)| control(name)).collect();
        let calls: Vec<_> = specs
            .iter()
            .zip(row())
            .map(|(spec, call)| (spec, call.1))
            .collect();
        let set = apply_chain(run(&layers), &calls);
        let Node::Constants(set_node) = set.node_kind() else {
            panic!("{name} is no run of constants");
        };
        assert_eq!(set_node.1.len(), 1, "{name}");
        for (base_name, base) in bases {
            for (chain_name, chain) in chains {
                let name = format!("{chain_name} on {base_name} of {name}");
                let specs: Vec<ControlSpec> =
                    chain().iter().map(|(name, _)| control(name)).collect();
                let calls = || {
                    specs
                        .iter()
                        .zip(chain())
                        .map(|(spec, (_, value))| (spec, value))
                };
                let set = apply_chain(base(&layers), &calls().collect::<Vec<_>>());
                let in_turn = || {
                    calls().fold(base(&layers), |pattern, (spec, value)| match value {
                        ChainArgument::Number(n) => spec.apply_value(pattern, Value::F64(n)),
                        ChainArgument::Constant(value, locations) => {
                            if locations.is_empty() {
                                spec.apply_value(pattern, value)
                            } else {
                                let values = crate::pure(value)
                                    .with_added_context(locations.to_vec())
                                    .with_steps(Some(Fraction::ONE));
                                spec.apply_to(pattern, Some(values))
                            }
                        }
                        ChainArgument::Wave(wave, steps, math_inside, op_flags) => spec
                            .apply_to(pattern, Some(wave.to_pattern(math_inside, steps, op_flags))),
                        ChainArgument::Pattern(values) => spec.apply_to(pattern, Some(values)),
                    })
                };
                assert_same_answers(&name, &set, &unfolded(in_turn));
            }
        }
    }
}

/// A receiver that no other handle holds takes a constant control where
/// it is, and answers as a copy that took it. A query before the control
/// leaves tables and a reading of the constant on the node, which answer
/// no query after it. A receiver that another handle holds stays as it
/// was.
#[test]
fn a_receiver_held_alone_takes_a_control_where_it_is() {
    let control = |name: &str| ControlSpec::new([name]);
    let notes = |layers: &[SequenceLayer]| built(layers).map_control(&control("n"), false);
    let bases: [Base<'_>; 3] = [
        ("a sequence", &notes),
        ("one set", &|layers| {
            let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
            control("gain").apply(&swept, Some(located_value(0.25)))
        }),
        ("a run of sets", &|layers| {
            let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
            let quiet = control("gain").apply(&swept, Some(located_value(0.25)));
            control("room").apply(&quiet, Some(located_value(0.75)))
        }),
    ];
    // The control goes to the steps of each picked pattern.
    let picked = |layers: &[SequenceLayer], mode| {
        // A restart takes a selector whose steps begin on whole cycles.
        let period = match mode {
            crate::JoinMode::Restart => (2, 1),
            _ => (1, 1),
        };
        let selector = built(&[layer(
            period,
            vec![step((0, 1), (1, 2), 0.0), step((1, 2), (1, 1), 1.0)],
        )]);
        let lookup = crate::PickLookup::Array {
            enumerable_len: 2,
            length: 2,
            entries: vec![
                (0, notes(layers)),
                (
                    1,
                    control("gain").apply(&notes(layers), Some(located_value(0.25))),
                ),
            ],
        };
        crate::pick(selector, lookup, crate::PickIndexMode::Clamp, mode)
    };
    let inner = |layers: &[SequenceLayer]| picked(layers, crate::JoinMode::Inner);
    let restart = |layers: &[SequenceLayer]| picked(layers, crate::JoinMode::Restart);
    // The control goes to each pattern of a stack: steps, steps under a
    // wave, a pick and silence.
    let stacked = |layers: &[SequenceLayer]| {
        let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
        let quiet = control("gain").apply(&notes(layers), Some(located_value(0.25)));
        crate::stack(vec![quiet, swept, restart(layers), crate::silence()])
    };
    // A pattern of a stack that takes no control meets a constant of
    // it, and each other pattern takes the control.
    let kept = |layers: &[SequenceLayer]| {
        crate::stack(vec![notes(layers), notes(layers).filter_haps(|_| true)])
    };
    // The control goes to the steps under a structure that gives its
    // haps nothing more: the pulses of a segment, in each cycle.
    let segmented = |layers: &[SequenceLayer]| {
        let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
        crate::combinators::segment(&crate::stack(vec![swept, notes(layers)]), frac((4, 1)))
    };
    // A structure with places in the text keeps the control over it.
    // Pulses that cross the cycles cut each hap of the steps to a part
    // in its cycle, so the control goes to the steps.
    let struck = |layers: &[SequenceLayer]| {
        let structure = built(&[layer(
            (1, 1),
            vec![step((0, 1), (1, 2), 1.0), step((1, 2), (1, 1), 0.0)],
        )]);
        crate::compose::compose(
            &notes(layers),
            &structure,
            crate::compose::ComposeOp::KeepIf,
            crate::compose::Alignment::Out,
        )
    };
    let crossed = |layers: &[SequenceLayer]| {
        let pulse = SequenceStep::new(
            Fraction::ZERO,
            Fraction::ONE,
            Value::Bool(true),
            Locations::new(),
        );
        let structure = built(&[layer((3, 2), vec![pulse])]);
        let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
        crate::compose::compose(
            &crate::stack(vec![swept, notes(layers)]),
            &structure,
            crate::compose::ComposeOp::KeepIf,
            crate::compose::Alignment::Out,
        )
    };
    let bases = bases.into_iter().chain([
        ("a pick", &inner as _),
        ("a pick that restarts", &restart as _),
        ("a stack", &stacked as _),
        ("a stack that keeps a node", &kept as _),
        ("a segment", &segmented as _),
        ("a structure with places", &struck as _),
        ("pulses that cross cycles", &crossed as _),
    ]);
    let pan = |pattern: Pattern| control("pan").apply_to(pattern, Some(located_value(0.5)));
    let ask = |pattern: &Pattern| {
        for (begin, end) in spans() {
            pattern.query_arc(begin, end);
        }
    };
    let mut written = 0;
    for (name, layers) in sequences() {
        for (base_name, base) in bases.clone() {
            let name = format!("{base_name} of {name}");
            let alone = base(&layers);
            let node = Arc::as_ptr(&alone.node);
            ask(&alone);
            let alone = pan(alone);
            written += usize::from(Arc::as_ptr(&alone.node) == node);

            let held = base(&layers);
            ask(&held);
            // A handle of the receiver, which `held` keeps.
            let copied = pan(held.clone());
            assert_same_answers(&name, &alone, &copied);
            assert_same_answers(&name, &held, &base(&layers));
            assert_same_answers(&name, &alone, &unfolded(|| pan(base(&layers))));
        }
    }
    // Each base of each sequence whose steps lie in one cycle.
    assert!(written >= 7 * 4, "written where it is: {written}");
}

/// A control of a number, with no pattern of the number, answers as the
/// control of a `pure` of it: on a sequence, on a set of a constant, and
/// on a pattern that takes no control where it is.
#[test]
fn a_control_of_a_number_answers_as_the_control_of_its_pure() {
    let control = |name: &str| ControlSpec::new([name]);
    let notes = |layers: &[SequenceLayer]| built(layers).map_control(&control("n"), false);
    let bases: [Base<'_>; 3] = [
        ("a sequence", &notes),
        ("a set", &|layers| {
            let swept = control("lpf").apply(&notes(layers), Some(crate::signal::sine()));
            control("room").apply(&swept, Some(located_value(0.25)))
        }),
        ("a filter", &|layers| notes(layers).filter_haps(|_| true)),
    ];
    for (name, layers) in sequences() {
        for (base_name, base) in bases {
            let name = format!("{base_name} of {name}");
            let number = control("gain").apply_value(base(&layers), Value::F64(0.5));
            let reified = control("gain").apply(&base(&layers), Some(pure(Value::F64(0.5))));
            assert_eq!(
                std::mem::discriminant(number.node_kind()),
                std::mem::discriminant(reified.node_kind()),
                "{name}"
            );
            assert_same_answers(&name, &number, &reified);
        }
    }
}

/// A control of a pattern of many steps, on a pattern that is no
/// sequence, asks the steps one time for all the haps of a query. The
/// haps that it then makes are those that each hap makes when it asks
/// the steps over its own part.
#[test]
fn a_control_of_many_steps_asks_them_one_time() {
    let control = |name: &str| ControlSpec::new([name]);
    let kept = |layers: &[SequenceLayer]| {
        built(layers)
            .map_control(&control("n"), false)
            .filter_haps(|_| true)
    };
    let steps = |period: (i128, i128), count: i128| {
        let step = |at: i128| step((at, count), (at + 1, count), at as f64);
        built(&[layer(period, (0..count).map(step).collect())])
    };
    let values: [(&str, &dyn Fn() -> Pattern); 3] = [
        ("eight steps a cycle", &|| steps((1, 1), 8)),
        ("five steps in a cycle and a half", &|| steps((3, 2), 5)),
        ("a chord of steps", &|| {
            stack(vec![steps((1, 1), 8), steps((1, 2), 3)]).filter_haps(|_| true)
        }),
    ];
    let said = |haps: &[Hap]| {
        haps.iter()
            .map(|hap| format!("{hap:?}"))
            .collect::<Vec<_>>()
    };
    let mut asked_one_time = 0;
    for (name, layers) in sequences() {
        for (values_name, values) in values {
            let name = format!("{values_name} on {name}");
            let applied = control("gain").apply(&kept(&layers), Some(values()));
            let side = crate::ValueSide::of(applied.node_kind()).expect("an application");
            for (begin, end) in spans() {
                let state = State::new(TimeSpan::new(begin, end));
                let functions = side.functions.query(&state);
                let mut one_query = Vec::new();
                if !side.met_in_one_query(&mut functions.clone(), &state, &mut one_query) {
                    continue;
                }
                asked_one_time += 1;
                let mut each = Vec::new();
                let mut meetings = crate::Meetings::default();
                for hf in functions {
                    assert!(side.meet(hf, &mut meetings, &state, 0, &mut each));
                }
                assert_eq!(said(&one_query), said(&each), "{name}: {begin:?}..{end:?}");
            }
        }
    }
    assert!(asked_one_time >= 30, "asked one time: {asked_one_time}");
}

/// A union with a map of `value` alone keeps the other side as it is, so
/// a set of such a map joins no run.
#[test]
fn a_set_that_a_union_drops_joins_no_run() {
    let gain = ControlSpec::new(["gain"]);
    let value_alone = ControlSpec::new(["value"]);
    for (name, layers) in sequences() {
        let base = || {
            built(&layers)
                .map_control(&ControlSpec::new(["n"]), false)
                .filter_haps(|_| true)
        };
        let chains: [(&str, [&ControlSpec; 2]); 2] = [
            ("dropped first", [&value_alone, &gain]),
            ("dropped last", [&gain, &value_alone]),
        ];
        for (chain_name, controls) in chains {
            let name = format!("{chain_name} on {name}");
            let chain = || {
                controls.iter().fold(base(), |pattern, control| {
                    control.apply(&pattern, Some(located_value(0.5)))
                })
            };
            assert_same_answers(&name, &chain(), &unfolded(chain));
        }
    }
}

/// A row with the name of a wave, on a stack with a pattern under that
/// wave: the control of that name meets the pattern after the wave, and
/// the other control goes under the wave, into the steps.
#[test]
fn a_row_with_the_name_of_a_wave_sets_the_others_under_it() {
    use crate::controls::{ChainArgument, apply_chain};
    let (gain, lpf) = (ControlSpec::new(["gain"]), ControlSpec::new(["lpf"]));
    for (name, layers) in sequences() {
        let stacked = || {
            let notes = || built(&layers).map_control(&ControlSpec::new(["n"]), false);
            let waved = lpf.apply(&notes(), Some(crate::signal::sine()));
            crate::stack(vec![notes(), waved])
        };
        let row = [
            (&gain, ChainArgument::Number(0.5)),
            (&lpf, ChainArgument::Number(800.0)),
        ];
        let set = apply_chain(stacked(), &row);
        assert_same_answers(name, &set, &unfolded(|| apply_chain(stacked(), &row)));
        // Steps that lie in one cycle take a row where they are.
        if !lies_in_cycles(&built(&layers)) {
            continue;
        }
        let second_kind = match set.node_kind() {
            Node::Stack(patterns) => patterns[1].node_kind(),
            Node::StackPure(cells) => &cells[1].kind,
            _ => panic!("{name} is no stack"),
        };
        let Node::Constants(node) = second_kind else {
            panic!("the wave of {name} meets no constant");
        };
        let run = &node.1;
        assert_eq!(run.len(), 1, "{name}");
    }
}

/// A `set` that gives its hap another lookup than a `set` of controls
/// does joins no run, and no control joins it.
#[test]
fn a_set_of_another_lookup_joins_no_run() {
    let gain = ControlSpec::new(["gain"]);
    for (name, layers) in sequences() {
        let chain = || {
            let notes = built(&layers)
                .map_control(&ControlSpec::new(["n"]), false)
                .filter_haps(|_| true);
            let quiet = gain.pattern(&located_value(0.5));
            let set = notes.app_left_composed(quiet, ComposeOp::Set, LookupFlow::Left);
            ControlSpec::new(["pan"]).apply(&set, Some(located_value(1.5)))
        };
        let set = chain();
        let Node::Constants(node) = set.node_kind() else {
            panic!("{name} is no run of constants");
        };
        let run = &node.1;
        assert_eq!(run.len(), 2, "{name}");
        assert_same_answers(name, &set, &unfolded(chain));
    }
}

/// A pick whose patterns take no control where they are folds a
/// constant control into the steps of each, when the steps lie in one
/// cycle: no run of constants is made over the pick.
#[test]
fn a_pick_folds_a_control_that_its_steps_do_not_take() {
    for (name, layers) in sequences() {
        let chain = || {
            let selector = built(&[layer((1, 1), vec![step((0, 1), (1, 1), 0.0)])]);
            let lookup = crate::PickLookup::Array {
                enumerable_len: 1,
                length: 1,
                // Steps of numbers hold no map for a control to go in.
                entries: vec![(0, built(&layers))],
            };
            let (index, join) = (crate::PickIndexMode::Clamp, crate::JoinMode::Restart);
            let picked = crate::pick(selector, lookup, index, join);
            ControlSpec::new(["gain"]).apply(&picked, Some(located_value(0.5)))
        };
        let folded = matches!(chain().node_kind(), Node::Pick(..));
        assert_eq!(folded, lies_in_cycles(&built(&layers)), "{name}");
        assert_same_answers(name, &chain(), &unfolded(chain));
    }
}

/// A function call gives a handle that binds the runtime of the call.
#[test]
fn a_fold_reads_data_through_the_runtime_that_a_handle_binds() {
    let bound = |pattern: Pattern| {
        pattern.with_runtime_settings(crate::settings::RuntimeSettings::default())
    };
    let changed = |pattern: Pattern| {
        stack(vec![
            pattern,
            bound(located_value(2.0)),
            bound(crate::silence()),
        ])
        .map_control(&ControlSpec::new(["n"]), false)
        .fast(frac((3, 2)))
    };
    for (name, layers) in sequences() {
        let folded = changed(bound(built(&layers)));
        assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
        assert_same_answers(name, &folded, &unfolded(|| changed(bound(built(&layers)))));
    }
    // A list of haps is no data: a query reads it under the runtime.
    let haps = bound(pure(Value::Haps(crate::value::HapList::new(Vec::new()))));
    let kept = stack(vec![built(&sequences()[0].1), haps]);
    assert!(matches!(
        kept.node_kind(),
        Node::Stack(_) | Node::StackPure(_)
    ));
}

#[test]
fn a_stack_and_a_speed_of_sequences_answer_as_their_own_nodes() {
    let all = || sequences().into_iter().map(|(_, layers)| built(&layers));
    let stacked = || stack(all().collect());
    let folded = stacked();
    assert!(matches!(folded.node_kind(), Node::Sequence(_)));
    assert_same_answers("a stack", &folded, &unfolded(stacked));
    for (name, layers) in sequences() {
        for factor in [(2, 1), (1, 3), (3, 2), (5, 7)] {
            let sped = |pattern: Pattern| pattern.fast(frac(factor)).slow(frac((2, 1)));
            let folded = sped(built(&layers));
            assert!(matches!(folded.node_kind(), Node::Sequence(_)), "{name}");
            assert_same_answers(
                &format!("{name} at {factor:?}"),
                &folded,
                &unfolded(|| sped(built(&layers))),
            );
        }
    }
}

#[test]
fn a_fold_leaves_what_it_does_not_know_to_its_node() {
    let steps = || built(&sequences()[0].1);
    // Values of two layers, a factor that is not above zero, a handle
    // that is marked volatile, and a stack with a signal in it.
    let layered = built(&sequences()[1].1);
    let unknown = [
        compose(&steps(), &layered, ComposeOp::Add, Alignment::In),
        steps().fast(frac((-2, 1))),
        compose(
            &steps().mark_volatile(),
            &pure(Value::F64(1.0)),
            ComposeOp::Add,
            Alignment::In,
        ),
        stack(vec![steps(), crate::signal::saw()]),
    ];
    for pattern in unknown {
        assert!(!matches!(pattern.node_kind(), Node::Sequence(_)));
    }
}

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

fn map_of(entries: &[(&str, Value)]) -> Value {
    Value::object(
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone())),
    )
}

/// A layer of `values` that `names` and `set` dress.
fn dressed_layer(values: Vec<Value>, names: Option<&[&str]>, set: Option<Value>) -> Layer {
    let raws: Vec<Raw> = (0..values.len())
        .map(|at| {
            let at = at as i128;
            let whole = TimeSpan::new(frac((at, 4)), frac((at + 1, 4)));
            Raw {
                whole,
                part: None,
                value: at as u32,
                locations: Locations::new(),
            }
        })
        .collect();
    let mut layer = Layer::new(frac((1, 1)), raws, values.into());
    if let Some(names) = names {
        let names: Arc<[Arc<str>]> = names.iter().map(|name| Arc::from(*name)).collect();
        let id = crate::controls::builtin_id_of_names(&names);
        layer.named_id = id;
        layer.change_dress(|dress| {
            if id == 0 {
                dress.set_custom_named(&names);
            } else {
                dress.clear_custom_named();
            }
            dress.assign_set(set);
        });
    } else {
        layer.named_id = 0;
        layer.change_dress(|dress| {
            dress.clear_custom_named();
            dress.assign_set(set);
        });
    }
    layer
}

/// The value of each step as one map of its own: the way a layer made
/// each value before the steps shared a table.
fn made_plain(layer: &Layer) -> Vec<Value> {
    let dress = layer.dress();
    let named = layer.named_names();
    layer
        .values
        .iter()
        .map(|value| {
            let mut value = match named {
                Some(names) => crate::controls::named_value(names, value.clone()),
                None => value.clone(),
            };
            dress.compose_set_into(&mut value);
            value
        })
        .collect()
}

/// Every way to read the two maps agrees.
fn assert_one_map(shared: &crate::OrderedMap, plain: &crate::OrderedMap, what: &str) {
    assert_eq!(shared, plain, "{what}: ==");
    assert_eq!(format!("{shared:?}"), format!("{plain:?}"), "{what}: Debug");
    assert_eq!(
        format!("{shared:#?}"),
        format!("{plain:#?}"),
        "{what}: Debug#"
    );
    assert_eq!(shared.len(), plain.len(), "{what}: len");
    assert_eq!(shared.is_empty(), plain.is_empty(), "{what}: is_empty");
    assert_eq!(shared.data_hash(), plain.data_hash(), "{what}: data_hash");
    assert!(
        shared.same_data(plain) && plain.same_data(shared),
        "{what}: same_data"
    );
    assert_eq!(
        shared.holds_only_data(),
        plain.holds_only_data(),
        "{what}: data"
    );
    assert_eq!(shared.owns_no_heap(), plain.owns_no_heap(), "{what}: heap");
    assert_eq!(
        shared.js_entries(),
        plain.js_entries(),
        "{what}: js_entries"
    );
    assert_eq!(
        shared.iter().collect::<Vec<_>>(),
        plain.iter().collect::<Vec<_>>(),
        "{what}: iter"
    );
    assert_eq!(
        shared.into_iter().collect::<Vec<_>>(),
        plain.into_iter().collect::<Vec<_>>(),
        "{what}: into_iter"
    );
    for key in ["note", "s", "n", "gain", "release", "value", "new", "none"] {
        assert_eq!(shared.get(key), plain.get(key), "{what}: get {key}");
        assert_eq!(
            shared.contains_key(key),
            plain.contains_key(key),
            "{what}: has {key}"
        );
    }
}

#[test]
fn a_shared_table_reads_as_the_plain_maps() {
    let set = map_of(&[
        ("s", text("gm_piano")),
        ("release", Value::F64(0.28)),
        ("gain", Value::F64(0.78)),
    ]);
    let notes = || vec![text("d#3"), text("a2"), Value::F64(7.0), text("d#3")];
    let raw = || {
        vec![
            map_of(&[("note", text("c3")), ("n", Value::F64(1.0))]),
            map_of(&[("n", Value::F64(2.0))]),
            map_of(&[]),
        ]
    };
    let object = |object: Value| Some(object);
    // The name, the layer's values, its set, and whether the table
    // reads them.
    type Case<'a> = (
        &'a str,
        Vec<Value>,
        Option<&'a [&'a str]>,
        Option<Value>,
        bool,
    );
    let cases: Vec<Case> = vec![
        (
            "one name and a set",
            notes(),
            Some(&["note"]),
            object(set.clone()),
            true,
        ),
        ("one name, no set", notes(), Some(&["note"]), None, true),
        (
            "two names, no list",
            notes(),
            Some(&["note", "s"]),
            object(set.clone()),
            true,
        ),
        (
            "a raw map and a set",
            raw(),
            None,
            object(set.clone()),
            true,
        ),
        (
            "a name that the set holds",
            notes(),
            Some(&["s"]),
            object(set.clone()),
            false,
        ),
        (
            "a raw map with a key that the set holds",
            vec![map_of(&[("gain", Value::F64(1.0)), ("n", Value::F64(2.0))])],
            None,
            object(set.clone()),
            false,
        ),
        (
            "a list under two names",
            vec![Value::list(vec![text("c3"), text("gm_piano")])],
            Some(&["note", "s"]),
            object(set.clone()),
            false,
        ),
        (
            "a list under one name",
            vec![Value::list(vec![text("c3"), text("e3")])],
            Some(&["note"]),
            object(set.clone()),
            true,
        ),
        (
            "an object with a value key",
            vec![map_of(&[("value", text("c3")), ("n", Value::F64(2.0))])],
            Some(&["note"]),
            object(set.clone()),
            false,
        ),
        (
            "a value that is no data",
            vec![Value::Haps(crate::value::HapList::new(Vec::new()))],
            Some(&["note"]),
            None,
            false,
        ),
        (
            "a set that a union drops",
            notes(),
            Some(&["note"]),
            object(map_of(&[("value", Value::F64(1.0))])),
            false,
        ),
        (
            "an object under a name",
            vec![map_of(&[("n", Value::F64(1.0))]), text("x")],
            Some(&["dict"]),
            object(set.clone()),
            true,
        ),
    ];
    for (what, values, names, set, shared) in cases {
        let layer = dressed_layer(values, names, set);
        let owner: Arc<[Layer]> = Arc::new([layer]);
        let layer = &owner[0];
        let plain = made_plain(layer);
        assert_eq!(layer.each_value().count(), plain.len(), "{what}: count");
        assert!(layer.value(plain.len()).is_none(), "{what}: past the end");
        let is_shared = layer.is_direct()
            || layer
                .steps
                .cache_cell()
                .and_then(OnceLock::get)
                .is_some_and(|read| {
                    matches!(read.as_layer_read(), (LayerReadView::Shared(_), None))
                });
        assert_eq!(is_shared, shared, "{what}: the form");
        let reader = layer.reader(Some((&owner, 0)));
        for (index, plain) in plain.iter().enumerate() {
            let Some(Value::Object(shared)) = reader.get(index) else {
                panic!("{what}: step {index} is no map");
            };
            let Value::Object(plain) = plain else {
                panic!("{what}: plain step {index} is no map");
            };
            let name = format!("{what}, step {index}");
            assert_one_map(&shared, plain, &name);
            // A compose reads data only.
            if !Value::Object(plain.clone()).is_data(0) {
                continue;
            }
            // A write copies the entries into a plain map first.
            for write in 0..5 {
                let (mut shared, mut plain) = (shared.clone(), plain.clone());
                let change = |map: &mut crate::OrderedMap| match write {
                    0 => {
                        map.insert("gain".into(), Value::F64(9.0));
                    }
                    1 => {
                        map.insert("new".into(), Value::F64(9.0));
                    }
                    2 => {
                        map.remove("s");
                        map.remove("note");
                    }
                    3 => {
                        if let Some(value) = map.get_mut("release") {
                            *value = Value::F64(9.0);
                        }
                    }
                    _ => {
                        let mut over = Value::Object(map.clone());
                        let by = map_of(&[("n", Value::F64(5.0)), ("fresh", text("f"))]);
                        compose_data_in_place(ComposeOp::Set, &mut over, &by);
                        *map = match over {
                            Value::Object(over) => over,
                            _ => unreachable!("a map composed with a map"),
                        };
                    }
                };
                change(&mut shared);
                change(&mut plain);
                assert_one_map(&shared, &plain, &format!("{name}, write {write}"));
            }
            // A map that a dressed map is set onto reads the same.
            let (mut onto, mut onto_plain) = (
                map_of(&[("n", Value::F64(0.0)), ("gain", Value::F64(1.0))]),
                map_of(&[("n", Value::F64(0.0)), ("gain", Value::F64(1.0))]),
            );
            compose_data_in_place(ComposeOp::Set, &mut onto, &Value::Object(shared.clone()));
            compose_data_in_place(
                ComposeOp::Set,
                &mut onto_plain,
                &Value::Object(plain.clone()),
            );
            let (Value::Object(onto), Value::Object(onto_plain)) = (onto, onto_plain) else {
                panic!("{what}: a union is no map");
            };
            assert_one_map(&onto, &onto_plain, &format!("{name}, set onto"));
            // An empty map of data set onto it leaves it data.
            let (mut empty, mut empty_plain) =
                (Value::Object(shared.clone()), Value::Object(plain.clone()));
            compose_data_in_place(ComposeOp::Set, &mut empty, &map_of(&[]));
            compose_data_in_place(ComposeOp::Set, &mut empty_plain, &map_of(&[]));
            let (Value::Object(empty), Value::Object(empty_plain)) = (empty, empty_plain) else {
                panic!("{what}: a union is no map");
            };
            assert_one_map(&empty, &empty_plain, &format!("{name}, empty set onto"));
        }
    }
}

/// The order of the entries is the order of the compose of the set
/// onto the value: the entries of the step, then those of the set.
#[test]
fn a_shared_map_holds_the_entries_of_its_step_before_the_set() {
    let set = map_of(&[("s", text("gm_piano")), ("gain", Value::F64(0.5))]);
    let layer = dressed_layer(vec![text("c3")], Some(&["note"]), Some(set));
    let Some(Value::Object(map)) = layer.value(0) else {
        panic!("a step is a map");
    };
    assert_eq!(
        map.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        ["note", "s", "gain"]
    );
}

/// A map that reads a table is the raw map of another layer, or the
/// set of one, and reads the same as the plain map.
#[test]
fn a_dressed_map_dresses_another_layer() {
    let inner_set = map_of(&[("s", text("gm_piano")), ("gain", Value::F64(0.5))]);
    let inner = dressed_layer(
        vec![text("c3"), text("e3")],
        Some(&["note"]),
        Some(inner_set),
    );
    let maps: Vec<Value> = inner.each_value().collect();
    let outer_set = map_of(&[("release", Value::F64(0.2))]);
    // The dressed maps as raw values, then as the set.
    for (values, set) in [
        (maps.clone(), outer_set.clone()),
        (vec![text("a2")], maps[0].clone()),
    ] {
        let names: Option<&[&str]> = match values[0] {
            Value::Object(_) => None,
            _ => Some(&["n"]),
        };
        let layer = dressed_layer(values, names, Some(set));
        let owner: Arc<[Layer]> = Arc::new([layer]);
        let layer = &owner[0];
        assert!(matches!(
            layer.reader(Some((&owner, 0))),
            Values::Table(_) | Values::SeqLayer(_, _)
        ));
        for (shared, plain) in layer.each_value().zip(made_plain(layer)) {
            let (Value::Object(shared), Value::Object(plain)) = (shared, plain) else {
                panic!("a step is no map");
            };
            assert_one_map(&shared, &plain, "a dressed map under a dress");
        }
    }
}

/// A layer of `cells` steps that fill `[0, 1)` of the period, some cut
/// by a wider whole, with the locations that `variant` picks.
fn grid_layer(period: (i128, i128), cells: i128, variant: usize) -> SequenceLayer {
    let steps = (0..cells)
        .map(|cell| {
            let at = |n: i128| frac((n, cells));
            let mut locations = Locations::new();
            let (begin, end) = match (variant + cell as usize) % 6 {
                0 => (0, 0),
                1 => (cell as usize * 7, cell as usize * 7 + 3),
                // A range of no length is not no range.
                2 => (cell as usize, cell as usize),
                3 => (cell as usize, cell as usize + 1),
                4 => ((1 << 22) + cell as usize, (1 << 22) + cell as usize + 5000),
                _ => (0, 1),
            };
            match (variant + cell as usize) % 6 {
                0 => {}
                3 => locations.extend_from_slice(&[(begin, end), (begin + 9, end + 9)]),
                _ => locations.push((begin, end)),
            }
            let mut step =
                SequenceStep::new(at(cell), at(cell + 1), Value::F64(cell as f64), locations);
            if cell % 3 == 1 {
                // The whole reaches into the cells around the part.
                let whole = (at((cell - 1).max(0)), at((cell + 2).min(cells)));
                step = step.placed(whole, (at(cell), at(cell + 1)));
            }
            step
        })
        .collect();
    layer(period, steps)
}

fn haps_of(layer: &Layer, span: (Fraction, Fraction)) -> Vec<String> {
    let mut out = Vec::new();
    let owner: Arc<[Layer]> = Arc::new([layer.clone()]);
    crate::QUERY_THREAD.with(|thread| {
        query_sequence_onto(
            thread,
            &owner,
            &State::new(TimeSpan::new(span.0, span.1)),
            &mut out,
        );
    });
    out.iter()
        .map(|hap| format!("{hap:?} {:?}", hap.context.to_vec()))
        .collect()
}

/// The same layer with its steps as places.
fn as_places(layer: &Layer) -> Layer {
    let places: Box<[Place]> = layer.places().into();
    layer.with(|layer| {
        layer.set_steps(Steps::Listed(Listing::Places(Arc::new(Listed {
            places,
            small_denominators: true,
        }))));
    })
}

fn with_grid_of<R>(layer: &Layer, f: impl FnOnce(GridView<'_>) -> R) -> R {
    layer
        .listing()
        .with_grid_view(f)
        .expect("the steps are not on a grid")
}

fn assert_on_grid(layer: &Layer) {
    with_grid_of(layer, |_| ());
}

/// The grid of a layer answers as its places do, over spans that are
/// negative, of no width, on a tick or between two, and at the ends of
/// a period.
#[test]
fn a_layer_on_a_grid_answers_as_its_places_do() {
    let mut seen = 0;
    for period in [(1, 1), (3, 2), (2, 1), (1, 3)] {
        for cells in [1, 2, 3, 4, 5, 8, 12] {
            for variant in 0..6 {
                let grid = Layer::of(grid_layer(period, cells, variant)).unwrap();
                assert_on_grid(&grid);
                let places = as_places(&grid);
                for begin in -40..40i128 {
                    for width in [0, 1, 3, 7, 24] {
                        let begin = frac((begin, 8));
                        let span = (begin, begin.add(frac((width, 8))));
                        assert_eq!(
                            haps_of(&grid, span),
                            haps_of(&places, span),
                            "{period:?} {cells} {variant} {span:?}"
                        );
                    }
                }
                // A span between two ticks.
                for span in [(frac((1, 7)), frac((22, 7))), (frac((-5, 3)), frac((2, 9)))] {
                    assert_eq!(haps_of(&grid, span), haps_of(&places, span));
                }
                seen += 1;
            }
        }
    }
    assert_eq!(seen, 168);
}

/// A grid gives back each time, cut and location that made it.
#[test]
fn a_grid_gives_back_its_steps() {
    for cells in [1, 3, 5, 12] {
        for variant in 0..6 {
            let raws = |layer: SequenceLayer| {
                layer
                    .steps
                    .iter()
                    .map(|step| (step.whole(), step.part.is_some(), step.locations().clone()))
                    .collect::<Vec<_>>()
            };
            let made = grid_layer((1, 1), cells, variant);
            let layer = Layer::of(made.clone()).unwrap();
            let listing = layer.listing();
            for (index, (whole, part, locations)) in raws(made).into_iter().enumerate() {
                assert_eq!(listing.whole(index), TimeSpan::new(whole.0, whole.1));
                assert_eq!(listing.is_cut(index), part);
                assert_eq!(listing.locations(index), locations);
            }
        }
    }
}

/// A tick that is zero is the canonical zero, and the tick size is the
/// least common multiple of the denominators.
#[test]
fn a_grid_is_as_fine_as_its_denominators() {
    let layer = Layer::of(grid_layer((1, 1), 4, 0)).unwrap();
    with_grid_of(&layer, |grid| {
        assert_eq!(grid.ticks.get(), 4);
        assert_eq!(grid.time(0), Fraction::ZERO);
        assert_eq!(grid.time(0).denom(), 1);
    });
    let layer = Layer::of(grid_layer((1, 3), 4, 0)).unwrap();
    with_grid_of(&layer, |grid| assert_eq!(grid.ticks.get(), 12));
}

/// A layer whose times do not fit 32 bits of ticks keeps its places,
/// and a denominator past 32 bits refuses before the tick size is made.
#[test]
fn a_layer_off_the_grid_keeps_its_places() {
    let huge = Fraction::new(1, (1 << 40) + 1);
    let none = Locations::new;
    let steps = vec![
        SequenceStep::new(Fraction::ZERO, huge, Value::F64(0.0), none()),
        SequenceStep::new(huge, Fraction::ONE, Value::F64(1.0), none()),
    ];
    let layer = Layer::of(layer((1, 1), steps)).unwrap();
    assert!(matches!(layer.listing(), Listing::Places(_)));
    // The product of the two denominators does not fit 64 bits.
    let wide = Fraction::new((1 << 62) + 1, (1 << 63) + 1);
    let steps = vec![
        SequenceStep::new(Fraction::ZERO, frac((1, 2)), Value::F64(0.0), none()),
        SequenceStep::new(frac((1, 2)), wide, Value::F64(1.0), none()),
    ];
    let layer = Layer::of(self::layer((1, 1), steps)).unwrap();
    assert!(matches!(layer.listing(), Listing::Places(_)));
    // A negative time does not fit unsigned ticks either.
    let steps = vec![SequenceStep::new(
        frac((-1, 2)),
        frac((1, 2)),
        Value::F64(0.0),
        none(),
    )];
    let layer = Layer::of(self::layer((1, 1), steps)).unwrap();
    assert!(matches!(layer.listing(), Listing::Places(_)));
    // The places of a layer off the grid answer.
    assert_eq!(haps_of(&layer, (Fraction::ZERO, Fraction::ONE)).len(), 1);
}

/// A range of locations that is too long for a packed one does not push
/// the short ranges of other steps into the spare list.
#[test]
fn a_long_range_leaves_the_short_ones_packed() {
    let mut long = Locations::new();
    long.push((0, 100_000));
    let mut short = Locations::new();
    short.push((5_000_000, 5_000_003));
    let steps = vec![
        SequenceStep::new(Fraction::ZERO, frac((1, 2)), Value::F64(0.0), long),
        SequenceStep::new(frac((1, 2)), Fraction::ONE, Value::F64(1.0), short),
    ];
    let layer = Layer::of(self::layer((1, 1), steps)).unwrap();
    with_grid_of(&layer, |grid| {
        assert_eq!(grid.steps.get(1).loc.0 & PackedLoc::SPARE, 0);
        assert_eq!(grid.locations(1).to_vec(), [(5_000_000, 5_000_003)]);
        assert_eq!(grid.locations(0).to_vec(), [(0, 100_000)]);
    });
}

/// `same` leaves a source that nobody read unread, and answers the same
/// each time it is asked.
#[test]
fn same_reads_no_source() {
    struct Rest;
    impl StepSource for Rest {
        fn count(&self) -> usize {
            1
        }
        fn steps(
            &self,
            _: Fraction,
            end: Fraction,
            each: &mut dyn FnMut(SourceStep),
        ) -> Option<()> {
            each(SourceStep {
                begin: Fraction::ZERO,
                end,
                value: 0,
                locations: Locations::new(),
            });
            Some(())
        }
    }
    let sourced = |source: &Arc<dyn StepSource>| {
        Layer::of_source(SourcedLayer {
            period: Fraction::ONE,
            values: Arc::new([Value::F64(1.0)]),
            in_cycles: true,
            steps: Arc::clone(source),
        })
    };
    let one: Arc<dyn StepSource> = Arc::new(Rest);
    let (a, b) = (sourced(&one), sourced(&one));
    let other = sourced(&(Arc::new(Rest) as Arc<dyn StepSource>));
    let listed = Layer::of(layer((1, 1), vec![step((0, 1), (1, 1), 1.0)])).unwrap();
    let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
    for _ in 0..2 {
        assert!(same(&a, &b));
        assert!(!same(&a, &other));
        assert!(!same(&a, &listed));
        assert!(!same(&listed, &a));
    }
    for layer in [&a, &b, &other] {
        assert!(layer.source().is_some(), "a source was read");
    }
}

/// Two layers of the same steps are the same layer, and two grids with
/// other steps are not.
#[test]
fn grids_of_the_same_steps_are_the_same_layer() {
    let first = Layer::of(grid_layer((1, 1), 5, 2)).unwrap();
    let second = Layer::of(grid_layer((1, 1), 5, 2)).unwrap();
    let other = Layer::of(grid_layer((1, 1), 5, 3)).unwrap();
    let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
    assert!(same(&first, &second));
    assert!(same(&first, &as_places(&second)));
    assert!(!same(&first, &other));
}

/// A grid says a layer is in order as its places do: a step past the
/// period, out of order, empty, or with a whole inside its part is not.
#[test]
fn a_grid_checks_the_order_as_its_places_do() {
    let at = |a: i128, b: i128, whole: (i128, i128)| {
        let step = step((a, 4), (b, 4), 1.0);
        step.placed(
            (frac((whole.0, 4)), frac((whole.1, 4))),
            (frac((a, 4)), frac((b, 4))),
        )
    };
    let cases = [
        vec![at(0, 2, (0, 2)), at(2, 4, (2, 4))],
        vec![at(0, 2, (0, 2)), at(2, 5, (2, 5))],
        vec![at(2, 4, (2, 4)), at(0, 2, (0, 2))],
        vec![at(0, 0, (0, 0))],
        vec![at(1, 3, (0, 4)), at(2, 4, (1, 4))],
        vec![at(1, 3, (2, 4))],
        vec![at(0, 3, (0, 3)), at(1, 2, (1, 2))],
        vec![at(0, 2, (0, 2)), at(1, 4, (1, 4)), at(1, 4, (0, 4))],
    ];
    for period in [(1, 1), (1, 2), (3, 2)] {
        for steps in &cases {
            let grid = Layer::of(layer(period, steps.clone())).unwrap();
            assert_on_grid(&grid);
            assert_eq!(
                grid.is_in_order(),
                as_places(&grid).is_in_order(),
                "{period:?}"
            );
        }
    }
}

/// A grid makes the reduced time of a tick as exact division does,
/// with a table of reductions or with a shift, for a tick of either
/// sign.
#[test]
fn a_grid_makes_the_reduced_time_of_a_tick() {
    for ticks in [1, 3, 6, 12, 64, 100, 1000, 1024, 1025, 4096, 65_537] {
        let step = |end: i128| {
            SequenceStep::new(
                Fraction::ZERO,
                Fraction::new(1, end),
                Value::F64(0.0),
                Locations::new(),
            )
        };
        let layer = Layer::of(layer((1, 1), vec![step(ticks)])).unwrap();
        with_grid_of(&layer, |grid| {
            assert_eq!(grid.ticks.get() as i128, ticks);
            for tick in (-5000..5000).chain([i128::from(i64::MAX), i128::from(i64::MIN)]) {
                assert_eq!(
                    grid.made(tick),
                    Fraction::new(tick, ticks),
                    "{tick}/{ticks}"
                );
            }
        });
    }
}

/// The floor and the ceiling of a time in ticks are those of exact
/// division, for a time on a tick, between two, and negative.
#[test]
fn a_time_has_its_floor_and_ceiling_in_ticks() {
    let layer = Layer::of(grid_layer((1, 1), 12, 0)).unwrap();
    with_grid_of(&layer, |grid| {
        for denom in [1, 2, 3, 5, 7, 8, 12, 16, 24, 1 << 20] {
            for numer in -300..300 {
                let time = Fraction::new(numer, denom);
                let exact = time * Fraction::int(12);
                let (floor, ceil) = grid.ticks_at(time).unwrap();
                assert_eq!(Fraction::int(i128::from(floor)), exact.floor(), "{time:?}");
                assert_eq!(Fraction::int(i128::from(ceil)), exact.ceil(), "{time:?}");
            }
        }
        assert!(grid.ticks_at(Fraction::int(i128::MAX / 2)).is_none());
    });
}

/// A source that holds one step in each period.
struct OneStep;
impl StepSource for OneStep {
    fn count(&self) -> usize {
        1
    }
    fn steps(&self, _: Fraction, end: Fraction, each: &mut dyn FnMut(SourceStep)) -> Option<()> {
        each(SourceStep {
            begin: Fraction::ZERO,
            end,
            value: 0,
            locations: Locations::new(),
        });
        Some(())
    }
}

fn one_step_layer(source: &Arc<dyn StepSource>, value: f64) -> Layer {
    Layer::of_source(SourcedLayer {
        period: Fraction::ONE,
        values: Arc::new([Value::F64(value)]),
        in_cycles: true,
        steps: Arc::clone(source),
    })
}

/// The answer of `same` for two layers of one source does not change
/// when a query reads the source of one of them.
#[test]
fn same_answers_alike_after_a_source_is_read() {
    let source: Arc<dyn StepSource> = Arc::new(OneStep);
    let (a, b) = (one_step_layer(&source, 1.0), one_step_layer(&source, 1.0));
    let listed = Layer::of(layer((1, 1), vec![step((0, 1), (1, 1), 1.0)])).unwrap();
    let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
    let before = (
        same(&a, &b),
        same(&b, &a),
        same(&a, &listed),
        same(&listed, &a),
    );
    assert_eq!(before, (true, true, false, false));
    // A query reads the steps of `a` into a list.
    assert_eq!(a.listing().len(), 1);
    assert!(a.source().is_none(), "the source of a was read");
    let after = (
        same(&a, &b),
        same(&b, &a),
        same(&a, &listed),
        same(&listed, &a),
    );
    assert_eq!(after, before);
}

/// Two layers of one source with other values are not the same.
#[test]
fn same_tells_two_layers_of_one_source_with_other_values() {
    let source: Arc<dyn StepSource> = Arc::new(OneStep);
    let a = one_step_layer(&source, 1.0);
    let other = a.clone().valued(Arc::new([Value::F64(2.0)]));
    let alike = a.clone().valued(Arc::new([Value::F64(1.0)]));
    let same = |a: &Layer, b: &Layer| a.same(b, |a, b| a.same_data(b));
    assert!(!same(&a, &other));
    assert!(!same(&other, &a));
    assert!(same(&a, &alike));
}

/// A raw map with a `value` key under no name reads as the compose of a
/// set onto it, which keeps every key of the map.
#[test]
fn a_raw_map_with_a_value_key_reads_as_the_plain_map() {
    let set = map_of(&[("s", text("piano")), ("gain", Value::F64(0.5))]);
    let raw = vec![map_of(&[("value", text("c3")), ("n", Value::F64(1.0))])];
    let layer = dressed_layer(raw, None, Some(set));
    let plain = made_plain(&layer);
    for (index, plain) in plain.iter().enumerate() {
        let (Some(Value::Object(shared)), Value::Object(plain)) = (layer.value(index), plain)
        else {
            panic!("step {index} is no map");
        };
        assert_one_map(&shared, plain, "a raw map with a value key");
        assert_eq!(shared.get("value"), Some(&text("c3")));
    }
}

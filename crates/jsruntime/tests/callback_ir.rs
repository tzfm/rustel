//! Differential coverage for the JavaScript-compatible callback-IR tier.
//!
//! Each mode gets a fresh runtime, and the mode is set before any score is
//! evaluated, because a callback takes its representation when it is
//! bridged. A compiled callback must give the same haps as its JavaScript in
//! every window, with every field a hap shows outside its crate, and the same
//! steps.

use std::fmt::Write;

use rustel_core::{Hap, PickLookup, TimeSpan, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, PatternTransformIrMode, PatternTransformIrStats, Slot};
use rustel_transpiler::TranspileOptions;

const AUTO: PatternTransformIrMode = PatternTransformIrMode::Auto;
const COMPATIBILITY: PatternTransformIrMode = PatternTransformIrMode::Compatibility;
const DUAL_RUN: PatternTransformIrMode = PatternTransformIrMode::DualRun;

fn runtime(mode: PatternTransformIrMode) -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime.set_pattern_transform_ir_mode(mode);
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
}

/// Windows from the first cycle, across several cycles, across a cycle
/// boundary off the grid, and late in the timeline.
fn windows() -> [(Fraction, Fraction); 4] {
    [
        (Fraction::ZERO, Fraction::ONE),
        (Fraction::ZERO, Fraction::int(4)),
        (Fraction::new(1, 3), Fraction::new(7, 3)),
        (Fraction::int(7), Fraction::int(9)),
    ]
}

/// A value's debug form with every function shown without its identity. A
/// function's id counts the functions made so far, which differs between
/// runtimes, and how a runtime holds a callback is not what it plays.
fn value_without_identities(value: &Value) -> String {
    const FUNCTION: &str = "Function(";
    let shown = format!("{value:?}");
    let mut out = String::with_capacity(shown.len());
    let mut rest = shown.as_str();
    while let Some(start) = rest.find(FUNCTION) {
        let (before, after) = rest.split_at(start + FUNCTION.len());
        out.push_str(before);
        let mut depth = 1_usize;
        let end = after
            .char_indices()
            .find_map(|(index, character)| {
                match character {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                (depth == 0).then_some(index)
            })
            .unwrap_or(after.len());
        out.push_str("function");
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// One hap with every field it shows outside its crate: whole, part, value,
/// source locations, the context keys, live bindings and lookup shape.
fn describe(hap: &Hap) -> String {
    let lookup = hap.pick_lookup().map(|lookup| match lookup {
        PickLookup::Array {
            enumerable_len,
            length,
            entries,
        } => format!(
            "array {enumerable_len} {length} {:?}",
            entries.iter().map(|(index, _)| index).collect::<Vec<_>>()
        ),
        PickLookup::Object {
            enumerable_len,
            entries,
        } => format!(
            "object {enumerable_len} {:?}",
            entries.iter().map(|(key, _)| key).collect::<Vec<_>>()
        ),
    });
    format!(
        "{:?} {:?} {} {:?} scale={:?} edo={:?} definition={:?} tags={:?} log={:?} live={:?} visuals={} lookup={lookup:?}",
        hap.whole,
        hap.part,
        value_without_identities(&hap.value),
        hap.context,
        hap.scale_context(),
        hap.edo_size_context(),
        hap.scale_definition_context(),
        hap.tags_context(),
        hap.log_line(),
        hap.live_controls,
        hap.ui_visuals_context(),
    )
}

/// Every hap of the active graph in each window, described in full, and the
/// graph's steps.
fn observe(runtime: &JsRuntime) -> String {
    let mut out = String::new();
    for (begin, end) in windows() {
        match runtime.query(Slot::Active, 0, begin, end) {
            Ok(haps) => {
                writeln!(out, "{begin}..{end}:").expect("write to a string");
                for hap in &haps {
                    writeln!(out, "  {}", describe(hap)).expect("write to a string");
                }
            }
            Err(error) => writeln!(out, "{begin}..{end}: {error}").expect("write to a string"),
        }
    }
    let steps = runtime.active_pattern().map(|pattern| pattern.steps);
    writeln!(out, "steps: {steps:?}").expect("write to a string");
    out
}

/// Evaluate `source` in one runtime per mode and require the same result.
fn assert_modes_agree(runtimes: &[JsRuntime; 3], source: &str) {
    let observed: Vec<String> = runtimes
        .iter()
        .map(|runtime| {
            evaluate(runtime, source);
            observe(runtime)
        })
        .collect();
    assert_eq!(observed[0], observed[1], "Auto differs: {source}");
    assert_eq!(observed[2], observed[1], "DualRun differs: {source}");
}

fn stats_after(mode: PatternTransformIrMode, source: &str) -> (JsRuntime, PatternTransformIrStats) {
    let runtime = runtime(mode);
    evaluate(&runtime, source);
    let stats = runtime.pattern_transform_ir_stats();
    (runtime, stats)
}

/// One text a session runs.
#[derive(Clone, Copy, Debug)]
enum Text<'a> {
    Prelude(&'a str),
    Score(&'a str),
    Eval(&'a str),
    /// Query the active graph, which keeps constructions and builds tables.
    Query,
}

/// What one mode makes of a session: each text's outcome, then what the
/// active graph plays, the logs, and the stats.
type Session = (String, Vec<String>, PatternTransformIrStats);

fn session(mode: PatternTransformIrMode, texts: &[Text<'_>]) -> Session {
    let runtime = runtime(mode);
    let mut out = String::new();
    for text in texts {
        let outcome = match text {
            Text::Prelude(source) => runtime
                .evaluate_prelude(
                    source,
                    &TranspileOptions::default(),
                    std::time::Duration::from_secs(5),
                )
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Text::Score(source) => runtime
                .evaluate_score(source, &TranspileOptions::default())
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Text::Eval(source) => runtime.eval(source),
            Text::Query => {
                out.push_str(&observe(&runtime));
                Ok(())
            }
        };
        writeln!(out, "{text:?}: {outcome:?}").expect("write to a string");
    }
    out.push_str(&observe(&runtime));
    (
        out,
        runtime.take_logs(),
        runtime.pattern_transform_ir_stats(),
    )
}

/// Run `texts` in one fresh runtime per mode and require the same outcomes,
/// haps and logs.
fn assert_sessions_agree(texts: &[Text<'_>]) -> [Session; 3] {
    let [auto, compatibility, dual] =
        [AUTO, COMPATIBILITY, DUAL_RUN].map(|mode| session(mode, texts));
    assert_eq!(auto.0, compatibility.0, "Auto plays differently: {texts:?}");
    assert_eq!(auto.1, compatibility.1, "Auto logs differently: {texts:?}");
    assert_eq!(
        dual.0, compatibility.0,
        "DualRun plays differently: {texts:?}"
    );
    assert_eq!(
        dual.1, compatibility.1,
        "DualRun logs differently: {texts:?}"
    );
    assert_eq!(dual.2.mismatches, 0, "{texts:?}: {:?}", dual.2);
    [auto, compatibility, dual]
}

/// The callbacks of the benchmark songs and other compilable chains.
const CALLBACKS: &[&str] = &[
    "x => x.rev()",
    "x => x.fast(2)",
    "y => y",
    "x => x.gain(1)",
    "x => x.add(12)",
    "x => x.add(7)",
    r#"x => x.struct("[- x x]*4")"#,
    r#"x => x.s("supersaw").vib(5).fm(3).fmh(12).detune("<0.9@2 1.5 2>/3")"#,
    "x => x.fast(2).rev().gain(0.8)",
    r#"x => x.add("<0 [3 7]>")"#,
    r#"x => x.fast("<1 2>")"#,
    "x => x.add(-12)",
    "x => x.add(-0).gain(0)",
    "x => x.struct(true)",
    "x => x.s('bd')",
    "x => x.bbexpr('t*2')",
    "x => x.fast(1, 2)",
    r#"x => x.mask("1 0 1 1")"#,
    r#"x => x.squeeze("0 1")"#,
    r#"x => x.clip(0.75).ply("<2 2 2 4>")"#,
    r#"x => x.s("square:1").add(16)"#,
    "x => x.crush(4).speed(2)",
    "x => x.hurry(2).early(0.25)",
    "x => x.degradeBy(0.5)",
    "x => x.tag('a')",
    "x => x.tag('a').tag('b')",
    "x => x.scale('C:major')",
    r#"x => x.scale("C:<major minor>")"#,
    "x => x.color('red')",
    "x => x.sub(1).mul(2)",
    "x => x.add(12).n()",
    // A context key the JavaScript sets, compiled or not.
    "x => x.log()",
];

/// A control receiver, and a receiver of bare numbers that arithmetic
/// changes: `add` leaves a control value such as `{n: 0}` as it is.
const RECEIVERS: &[&str] = &[r#"n("0 1 2 3")"#, r#""0 1 2 3""#];

const COMBINATORS: &[&str] = &[
    "sometimes(F)",
    "sometimesBy(0.5, F)",
    "someCycles(F)",
    "every(2, F)",
    "firstOf(3, F)",
    "lastOf(3, F)",
    "chunk(4, F)",
    "off(1/8, F)",
    "superimpose(F)",
    "layer(F)",
    "jux(F)",
];

#[test]
fn compiled_callbacks_build_what_their_javascript_builds() {
    let runtimes = [runtime(AUTO), runtime(COMPATIBILITY), runtime(DUAL_RUN)];
    for callback in CALLBACKS {
        for combinator in COMBINATORS {
            let call = combinator.replace('F', callback);
            for receiver in RECEIVERS {
                assert_modes_agree(&runtimes, &format!("{receiver}.{call}"));
            }
        }
        // A receiver with a JavaScript callback reaches the program through
        // the host.
        for combinator in ["sometimes(F)", "every(2, F)", "chunk(4, F)", "layer(F)"] {
            let call = combinator.replace('F', callback);
            assert_modes_agree(&runtimes, &format!(r#"n("0 1 2 3").fmap(v => v).{call}"#));
        }
    }
    // Inputs whose values are patterns.
    for callback in [
        "x => x.rev()",
        "x => x.fast(2)",
        "y => y",
        r#"x => x.struct("x ~ x")"#,
    ] {
        assert_modes_agree(
            &runtimes,
            &format!(r#"pure(s("bd sd")).sometimes({callback}).innerJoin()"#),
        );
        assert_modes_agree(
            &runtimes,
            &format!(r#"pure(s("bd sd")).every(2, {callback}).innerJoin()"#),
        );
    }

    let [auto, compatibility, dual] = runtimes
        .each_ref()
        .map(JsRuntime::pattern_transform_ir_stats);
    assert!(auto.lowered > 0, "{auto:?}");
    assert!(auto.native_executions > 0, "{auto:?}");
    assert_eq!(auto.stale, 0, "{auto:?}");
    assert_eq!(auto.compatibility_executions, 0, "{auto:?}");
    assert_eq!(compatibility.native_executions, 0, "{compatibility:?}");
    assert!(dual.dual_runs > 0, "{dual:?}");
    assert_eq!(dual.mismatches, 0, "{dual:?}");
    assert_eq!(dual.stale, 0, "{dual:?}");
}

#[test]
fn a_setup_helper_compiles_with_its_location_free_mini_notation() {
    let setup = TranspileOptions {
        emit_mini_locations: false,
        ..TranspileOptions::default()
    };
    let runtimes = [runtime(AUTO), runtime(COMPATIBILITY), runtime(DUAL_RUN)];
    for runtime in &runtimes {
        runtime
            .evaluate_prelude(
                r#"globalThis.helper = x => x.struct("x ~ x").add("<0 12>")"#,
                &setup,
                std::time::Duration::from_secs(5),
            )
            .expect("setup");
    }
    // Each score compiles every callback it hands over, the helper included.
    for (score, callbacks) in [
        (r#"n("0 1 2 3").sometimes(helper)"#, 1),
        (r#"n("0 1 2 3").every(2, helper)"#, 1),
        (r#"n("0 1 2 3").layer(helper, x => x.rev())"#, 2),
    ] {
        let before = runtimes[0].pattern_transform_ir_stats().lowered;
        assert_modes_agree(&runtimes, score);
        let after = runtimes[0].pattern_transform_ir_stats().lowered;
        assert_eq!(after - before, callbacks, "{score}");
    }
}

#[test]
fn lazily_applied_compiled_callbacks_leave_a_pure_graph() {
    for source in [
        r#"n("0 1 2 3").sometimes(x => x.rev())"#,
        r#"n("0 1 2 3").sometimes(x => x.fast(2))"#,
        r#"s("bd sd hh cp").sometimesBy(0.3, x => x.s("supersaw").vib(5).fm(3))"#,
    ] {
        let (auto, stats) = stats_after(AUTO, source);
        assert!(stats.lowered > 0, "{source}: {stats:?}");
        let graph = auto.active_pattern().expect("active graph");
        let pure = graph
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: the compiled callback left JavaScript behind"));
        // No host is installed: a wrong classification panics here.
        assert!(
            !pure.query_arc(Fraction::ZERO, Fraction::int(4)).is_empty(),
            "{source}"
        );

        let (compatibility, _) = stats_after(COMPATIBILITY, source);
        let graph = compatibility.active_pattern().expect("active graph");
        assert!(
            !graph.is_pure(),
            "{source}: Compatibility must keep the callback"
        );
    }
}

#[test]
fn a_compiled_chunk_callback_answers_from_tables() {
    let (auto, _) = stats_after(AUTO, r#"n("0 1 2 3").chunk(4, x => x.fast(2))"#);
    let graph = auto.active_pattern().expect("active graph");
    assert!(graph.is_pure());
    // The first query of a node answers from the graph and a later one
    // builds its tables.
    for _ in 0..2 {
        let _ = auto.query(Slot::Active, 0, Fraction::ZERO, Fraction::int(16));
    }
    assert!(graph.answers_from_tables(TimeSpan::new(Fraction::ZERO, Fraction::int(16))));
}

#[test]
fn replacements_close_compilation_before_the_text_that_makes_them_runs() {
    let callback = r#"n("0 1 2 3").sometimesBy(1, x => x.rev())"#;
    for (name, source) in [
        (
            "register after the callback",
            format!("const p = {callback}; register('rev', (pat) => pat.fast(2)); p"),
        ),
        (
            "register before the callback",
            format!("register('rev', (pat) => pat.fast(2)); {callback}"),
        ),
        (
            "assignment after the callback",
            format!(
                "const p = {callback}; Pattern.prototype.rev = function () {{ return this.fast(2) }}; p"
            ),
        ),
        (
            "assignment before the callback",
            format!("Pattern.prototype.rev = function () {{ return this.fast(2) }}; {callback}"),
        ),
        (
            "a computed write",
            format!(
                "const key = 'rev'; const p = {callback}; Pattern.prototype[key] = (x) => x; p"
            ),
        ),
        (
            "a declared mini callee",
            r#"const p = n("0 1 2 3").sometimesBy(1, x => x.struct("x x")); function f(m) { return m } p"#
                .to_owned(),
        ),
        (
            "a mini callee parameter",
            r#"n("0 1 2 3").sometimesBy(1, m => m.rev().struct("x x"))"#.to_owned(),
        ),
        (
            "a string parser",
            "const p = n(\"0 1 2 3\").sometimesBy(1, x => x.s('bd sd')); setStringParser(mini); p"
                .to_owned(),
        ),
        (
            "a user-registered method",
            r#"register('myfx', (pat) => pat.fast(2)); n("0 1 2 3").sometimesBy(1, x => x.myfx())"#
                .to_owned(),
        ),
        (
            "a free variable",
            r#"const k = 2; n("0 1 2 3").sometimesBy(1, x => x.fast(k))"#.to_owned(),
        ),
        (
            "a bound callback",
            r#"n("0 1 2 3").sometimesBy(1, (x => x.rev()).bind(null))"#.to_owned(),
        ),
        (
            "a proxied callback",
            r#"n("0 1 2 3").sometimesBy(1, new Proxy(x => x.rev(), {}))"#.to_owned(),
        ),
        (
            "register through the scope alias",
            format!("const p = {callback}; strudelScope.register('rev', (pat) => pat.fast(2)); p"),
        ),
        (
            "register through a computed member of the scope alias",
            format!(
                "const p = {callback}; strudelScope['register']('rev', (pat) => pat.fast(2)); p"
            ),
        ),
        (
            "register through an escaped global object",
            format!(
                "const g = globalThis.valueOf(); const p = {callback}; g.register('rev', (pat) => pat.fast(2)); p"
            ),
        ),
        (
            "register through a destructured alias",
            format!(
                "const {{ register: r }} = strudelScope; const p = {callback}; r('rev', (pat) => pat.fast(2)); p"
            ),
        ),
        (
            "a class field on the prototype",
            format!(
                "function Base(o) {{ return o }} class Stamp extends Base {{ rev = function () {{ return this.fast(2) }} }} const p = {callback}; new Stamp(Pattern.prototype); p"
            ),
        ),
        (
            "a class field that replaces the mini callee",
            r#"function Base(o) { return o } class Stamp extends Base { m = (s, o) => pure(true) } const p = n("0 1 2 3").sometimesBy(1, x => x.struct("x x")); new Stamp(globalThis.valueOf()); p"#
                .to_owned(),
        ),
        (
            "a class field that replaces an intrinsic",
            r#"function Base(o) { return o } class Stamp extends Base { Object = { keys() { throw new Error('keys replaced') } } } const p = n("0 1 2 3").sometimesBy(1, x => x.add(12)); new Stamp(globalThis.valueOf()); p"#
                .to_owned(),
        ),
    ] {
        let [auto, _, _] = assert_sessions_agree(&[Text::Score(&source)]);
        assert_eq!(auto.2.lowered, 0, "{name}: {:?}", auto.2);
    }
}

#[test]
fn every_global_that_holds_the_global_object_is_a_holder() {
    let runtime = runtime(AUTO);
    evaluate(
        &runtime,
        "pure(Object.getOwnPropertyNames(globalThis).filter((k) => { try { return globalThis[k] === globalThis } catch (e) { return false } }).join(' '))",
    );
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query");
    let aliases = haps[0].value.show();
    let aliases: Vec<&str> = aliases.split(' ').collect();
    assert!(aliases.contains(&"globalThis"), "{aliases:?}");
    for alias in aliases {
        let output = rustel_transpiler::transpile(
            &format!("{alias}.register('rev', (pat) => pat)"),
            &TranspileOptions::default(),
        );
        assert_eq!(
            output.surface_writes.member_names,
            ["rev", "_rev"],
            "{alias}: {:?}",
            output.surface_writes
        );
    }
}

#[test]
fn a_setup_that_can_write_any_member_closes_compilation() {
    let setup = "globalThis.swap = (name, f) => { Pattern.prototype[name] = f }";
    let score = r#"const p = n("0 1 2 3").sometimesBy(1, x => x.rev()); swap('rev', function () { return this.fast(2) }); p"#;
    let [auto, compatibility] = [AUTO, COMPATIBILITY].map(|mode| {
        let runtime = runtime(mode);
        runtime
            .evaluate_prelude(
                setup,
                &TranspileOptions::default(),
                std::time::Duration::from_secs(5),
            )
            .expect("setup");
        evaluate(&runtime, score);
        (observe(&runtime), runtime.pattern_transform_ir_stats())
    });
    assert_eq!(auto.0, compatibility.0);
    assert_eq!(auto.1.lowered, 0, "{:?}", auto.1);
}

#[test]
fn an_inert_text_keeps_compilation_open() {
    let (auto, stats) = stats_after(
        AUTO,
        r#"register('myfx', (pat) => pat); globalThis.k = 1; n("0 1 2 3").sometimesBy(1, x => x.rev())"#,
    );
    assert_eq!(stats.lowered, 1, "{stats:?}");
    assert!(auto.active_pattern().expect("active graph").is_pure());
}

#[test]
fn a_callback_that_throws_keeps_throwing_where_and_when_it_does() {
    for callback in [
        "x => x.fast()",
        r#"x => x.struct("[")"#,
        "x => x.rev(1)",
        "x => x.nope()",
        "x => x.add()",
    ] {
        for combinator in ["sometimes(F)", "every(2, F)"] {
            let source = format!(r#"n("0 1 2 3").{}"#, combinator.replace('F', callback));
            let [auto, compatibility, dual] = [AUTO, COMPATIBILITY, DUAL_RUN].map(|mode| {
                let runtime = runtime(mode);
                let evaluated = runtime
                    .evaluate_score(&source, &TranspileOptions::default())
                    .map(|_| ());
                let observed = evaluated.is_ok().then(|| observe(&runtime));
                let lowered = runtime.pattern_transform_ir_stats().lowered;
                (evaluated, observed, runtime.take_logs(), lowered)
            });
            assert_eq!(auto.0, compatibility.0, "{source}");
            assert_eq!(auto.1, compatibility.1, "{source}");
            assert_eq!(auto.2, compatibility.2, "{source}");
            assert_eq!(dual.0, compatibility.0, "{source}");
            assert_eq!(dual.1, compatibility.1, "{source}");
            assert_eq!(auto.3, 0, "{source}");
        }
    }
}

#[test]
fn a_compiled_callback_used_as_a_value_keeps_its_identity() {
    for mode in [AUTO, COMPATIBILITY] {
        let runtime = runtime(mode);
        evaluate(
            &runtime,
            "const f = x => x.rev(); n(f).fmap(v => v.n === f ? 1 : 0)",
        );
        let haps = runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query");
        assert_eq!(haps.len(), 1, "{mode:?}");
        assert_eq!(haps[0].value.show(), "1", "{mode:?}");
    }
}

#[test]
fn a_later_replacement_reaches_graphs_built_before_it() {
    let fast_rev = "Pattern.prototype.rev = function () { return this.fast(2) }";
    let throwing_rev = r#"Pattern.prototype.rev = function () { throw new Error("rev is gone") }"#;
    let failing = format!("{throwing_rev}; null.boom");
    let kept = format!("{fast_rev}; keep");
    let lazy = r#"n("0 1 2 3").sometimesBy(1, x => x.rev())"#;
    let impure = r#"n("0 1 2 3").fmap(v => v).sometimesBy(1, x => x.rev())"#;
    for texts in [
        // A text after the score replaces the built-in.
        vec![Text::Score(lazy), Text::Eval(fast_rev)],
        vec![Text::Score(impure), Text::Eval(fast_rev)],
        // A later score replaces it and fails, so the graph keeps playing.
        vec![Text::Score(lazy), Text::Score(&failing)],
        // A later score reuses a graph a global holds.
        vec![
            Text::Score(r#"globalThis.keep = n("0 1 2 3").sometimesBy(1, x => x.rev()); keep"#),
            Text::Score(&kept),
        ],
        vec![
            Text::Prelude(r#"globalThis.keep = n("0 1 2 3").sometimesBy(1, x => x.rev())"#),
            Text::Score("register('rev', (pat) => pat.fast(2)); keep"),
        ],
        vec![
            Text::Prelude(r#"globalThis.keep = n("0 1 2 3").sometimesBy(1, x => x.rev())"#),
            Text::Score(
                "strudelScope.register('rev', function (pat) { return pat.fast(2) }); keep",
            ),
        ],
        // An eagerly applied callback ran before the change, in every mode.
        vec![
            Text::Score(r#"n("0 1 2 3").superimpose(x => x.rev())"#),
            Text::Eval(fast_rev),
        ],
        // Constructions and tables kept before the change answer no more.
        vec![
            Text::Score(lazy),
            Text::Query,
            Text::Query,
            Text::Eval(fast_rev),
        ],
        vec![
            Text::Score(r#"n("0 1 2 3").chunk(4, x => x.fast(2))"#),
            Text::Query,
            Text::Query,
            Text::Eval("Pattern.prototype.fast = function () { return this.slow(2) }"),
        ],
        // A batch of lazily applied calls.
        vec![
            Text::Score(r#"n("0 1").plyForEach(3, x => x.fast(2))"#),
            Text::Eval("Pattern.prototype.fast = function () { return this.slow(2) }"),
        ],
        // A change of the mini callee reaches a chain that builds
        // mini-notation.
        vec![
            Text::Score(r#"n("0 1 2 3").sometimesBy(1, x => x.struct("x x"))"#),
            Text::Eval("globalThis.m = (source, offset) => pure(true)"),
        ],
        // Answers that fill kept before the change answer no more.
        vec![
            Text::Score(r#"n("0 1 2 3").sometimesBy(1, x => x.rev()).fill()"#),
            Text::Query,
            Text::Eval(fast_rev),
        ],
        vec![
            Text::Score(r#"n("0 1 2 3").sometimesBy(1, x => x.rev()).fill()"#),
            Text::Query,
            Text::Score(&failing),
        ],
    ] {
        let [auto, _, dual] = assert_sessions_agree(&texts);
        assert_eq!(auto.2.lowered, 1, "{texts:?}: {:?}", auto.2);
        assert!(
            dual.1.iter().all(|line| !line.contains("compiled")),
            "{:?}",
            dual.1
        );
    }
}

#[test]
fn a_stale_compiled_callback_routes_queries_through_the_host_while_it_lives() {
    let runtime = runtime(AUTO);
    evaluate(&runtime, r#"n("0 1 2 3").sometimesBy(1, x => x.rev())"#);
    let graph = runtime.active_pattern().expect("active graph");
    assert!(graph.is_pure());
    assert!(!runtime.active_needs_host());
    assert!(!runtime.compiled_callbacks_need_host());
    runtime
        .eval("Pattern.prototype.rev = function () { return this.fast(2) }")
        .expect("replace rev");
    assert!(graph.is_pure(), "a built graph keeps its classification");
    assert!(runtime.compiled_callbacks_need_host());
    assert!(runtime.active_needs_host());

    // A pure graph queried with no host cannot run the callback, and plays
    // nothing rather than the built-in.
    assert!(graph.query_arc(Fraction::ZERO, Fraction::ONE).is_empty());
    // The wrappers that own the graph keep the callback it may need.
    assert!(!graph.reachable_callbacks().is_empty());

    // Once no stale program is alive, nothing needs the host.
    drop(graph);
    evaluate(&runtime, r#"n("0 1 2 3").fast(2)"#);
    evaluate(&runtime, r#"n("0 1 2 3").slow(2)"#);
    runtime.run_gc();
    assert!(!runtime.compiled_callbacks_need_host());
    assert!(!runtime.active_needs_host());
}

#[test]
fn a_predicate_callback_keeps_its_javascript() {
    for source in [
        r#"n("0 1 2 3").filterWhen(t => t.lt(0.5))"#,
        r#"n("0 1 2 3").fast(2).filterWhen(t => t.lt(1))"#,
        r#"n("0 1 2 3").filterWhen(t => t.mod(1).lt(0.5))"#,
        r#"filterWhen(t => t.lt(0.5), n("0 1 2 3"))"#,
        r#"n("0 1 2 3").filter(h => h)"#,
        r#"n("0 1 2 3").filterWhen(t => t)"#,
        r#"n("0 1 2 3").filter(x => x.rev())"#,
    ] {
        let [auto, ..] = assert_sessions_agree(&[Text::Score(source)]);
        assert!(!auto.0.contains("[]\n"), "{source}: {}", auto.0);
    }
}

#[test]
fn a_patterned_mini_argument_keeps_its_javascript() {
    for source in [
        r#"n("0 1 2 3").sometimes(x => x.struct("x(<3 5>,8)"))"#,
        r#"n("0 1 2 3").every(2, x => x.mask("1(3,<8 16>)"))"#,
        r#"n("0 1 2 3").off(0.125, x => x.gain("1(3,8,<0 2>)"))"#,
        r#"n("0 1 2 3").jux(x => x.n("<0 2> .. 3"))"#,
        r#"n("0 1 2 3").sometimes(x => x.s("bd(<3 5>,8)"))"#,
        r#"n("0 1 2 3").sometimes(x => x.add("0 .. <2 4>"))"#,
        r#"n("0 1 2 3").sometimes(x => x.fast("1(<3 5>,8)"))"#,
    ] {
        let [auto, ..] = assert_sessions_agree(&[Text::Score(source)]);
        assert_eq!(auto.2.lowered, 0, "{source}: {:?}", auto.2);
    }
}

/// Hydra's `setFunction` writes a global whose name is data, so no text
/// announces it.
#[cfg(feature = "hydra")]
#[test]
fn a_hydra_source_that_replaces_a_global_closes_compilation() {
    for score in [
        "await initHydra()\nsetFunction({ name: 'Object', type: 'src', glsl: '' })\nnote(\"0 1\").superimpose(x => x.add(12))",
        "await initHydra()\nsetFunction({ name: 'Array', type: 'src', glsl: '' })\nnote(\"0 1\").every(1, x => x.add(12))",
        "await initHydra()\nconst p = n(\"0 1\").sometimesBy(1, x => x.s(\"bd\"))\nsetFunction({ name: 'm', type: 'src', glsl: '' })\nglobalThis.__seen = JSON.stringify(p.queryArc(0, 1).map(h => h.value))\np",
    ] {
        let [auto, compatibility, dual] = [AUTO, COMPATIBILITY, DUAL_RUN].map(|mode| {
            let runtime = runtime(mode);
            let outcome = runtime
                .evaluate_score_with_effects_cancellable(
                    score,
                    &TranspileOptions::default(),
                    std::time::Duration::from_secs(5),
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .map(|_| ())
                .map_err(|error| error.to_string());
            (
                outcome,
                runtime.get_string("__seen"),
                observe(&runtime),
                runtime.take_logs(),
                runtime.pattern_transform_ir_stats(),
            )
        });
        assert_eq!(auto.0, compatibility.0, "{score}");
        assert_eq!(auto.1, compatibility.1, "{score}");
        assert_eq!(auto.2, compatibility.2, "{score}");
        assert_eq!(auto.3, compatibility.3, "{score}");
        assert_eq!(dual.0, compatibility.0, "{score}");
        assert_eq!(dual.1, compatibility.1, "{score}");
        assert_eq!(dual.2, compatibility.2, "{score}");
        assert_eq!(dual.3, compatibility.3, "{score}");
        assert_eq!(dual.4.mismatches, 0, "{score}: {:?}", dual.4);
    }
}

#[test]
fn a_query_error_left_pending_stops_no_compiled_chain() {
    let superimposed = r#"n("0 1 2 3").superimpose(x => x.fast(2))"#;
    let compress =
        r#"const a = n("0").compress(0.5, 0.5); n("0 1 2 3").superimpose(x => x.fast(2))"#;
    let focus = r#"const a = n("0").focus(0, 0); n("0 1 2 3").superimpose(x => x.fast(2))"#;
    let press = r#"n("0 1").pressBy(1).queryArc(0, 1); n("0 1 2 3").superimpose(x => x.fast(2))"#;
    let chained = r#"n("0 1 2 3").every(2, x => x.compress(0.5, 0.5)).superimpose(x => x.fast(2))"#;
    let batched =
        r#"const a = n("0").compress(0.5, 0.5); n("0 1 2 3").plyForEach(3, x => x.add(12))"#;
    for texts in [
        vec![Text::Score(compress)],
        vec![Text::Score(focus)],
        vec![Text::Score(press)],
        vec![Text::Score(chained)],
        vec![Text::Score(batched)],
        vec![
            Text::Score(r#"n("0").compress(0.5, 0.5)"#),
            Text::Score(r#"s("bd")"#),
            Text::Score(r#"n("0 1 2 3").every(2, x => x.fast(2))"#),
        ],
        vec![Text::Score(compress), Text::Score(superimposed)],
    ] {
        let [auto, ..] = assert_sessions_agree(&texts);
        assert!(auto.2.lowered > 0, "{texts:?}: {:?}", auto.2);
    }
}

#[test]
fn identity_arrow_auto_compatibility_and_dual_run_agree() {
    let source = r#"s("bd sd hh cp").sometimesBy(1, pattern => pattern)"#;
    let (auto, auto_stats) = stats_after(AUTO, source);
    let graph = auto.active_pattern().expect("active graph");
    assert!(graph.is_pure(), "the compiled identity needs no host");
    assert_eq!(auto_stats.lowered, 1);
    let native = observe(&auto);

    let (compatibility, _) = stats_after(COMPATIBILITY, source);
    let graph = compatibility.active_pattern().expect("active graph");
    assert!(
        !graph.is_pure(),
        "the retained fallback keeps this graph impure"
    );
    assert!(
        !graph.reachable_callbacks().is_empty() || graph.purity().opaque,
        "the original JavaScript callback must remain owned"
    );
    compatibility.reset_pattern_transform_ir_stats();
    assert_eq!(observe(&compatibility), native);
    let stats = compatibility.pattern_transform_ir_stats();
    assert!(stats.calls > 0);
    assert_eq!(stats.native_executions, 0);
    assert_eq!(stats.compatibility_executions, stats.calls);

    let (dual, _) = stats_after(DUAL_RUN, source);
    dual.reset_pattern_transform_ir_stats();
    assert_eq!(observe(&dual), native);
    let stats = dual.pattern_transform_ir_stats();
    assert!(stats.dual_runs > 0);
    assert_eq!(stats.dual_runs, stats.calls);
    assert_eq!(stats.native_executions, stats.calls);
    assert_eq!(stats.compatibility_executions, stats.calls);
    assert_eq!(stats.mismatches, 0);
}

#[test]
fn nearby_dynamic_arrows_remain_on_the_complete_quickjs_tier() {
    for callback in [
        "pattern => pattern.rev(...[])",
        "pattern => { return pattern.rev(); }",
        "pattern => pattern.fast(two)",
    ] {
        let source = format!(r#"const two = 2; s("bd sd hh cp").sometimesBy(1, {callback})"#);
        let (runtime, stats) = stats_after(AUTO, &source);
        assert_eq!(stats.lowered, 0, "{callback}");
        let graph = runtime.active_pattern().expect("active graph");
        assert!(!graph.is_pure(), "{callback}: callback graph became pure");
        assert!(!observe(&runtime).is_empty(), "{callback}: empty result");
        assert_eq!(
            runtime.pattern_transform_ir_stats().calls,
            0,
            "{callback}: unsupported source entered callback IR"
        );
    }
}

#[test]
fn source_recognition_uses_captured_intrinsics_not_mutable_globals() {
    // Naming `Function` closes compilation for the whole realm, and the
    // callback keeps its JavaScript.
    let spoofing = r#"
          Function.prototype.toString = () => "pattern => pattern";
          s("bd sd hh cp").sometimesBy(1, pattern => pattern.rev())
        "#;
    let (spoofed, stats) = stats_after(AUTO, spoofing);
    assert_eq!(stats.lowered, 0);
    let (control, _) = stats_after(COMPATIBILITY, spoofing);
    assert_eq!(observe(&spoofed), observe(&control));

    let (hidden, stats) = stats_after(
        AUTO,
        r#"
          Function.prototype.toString = () => { throw new Error("user hook"); };
          s("bd sd hh cp").sometimesBy(1, pattern => pattern)
        "#,
    );
    assert_eq!(
        stats.lowered, 1,
        "the captured intrinsic was not isolated from the user replacement"
    );
    assert!(!observe(&hidden).is_empty());
}

#[test]
fn indexed_transformers_keep_their_original_javascript_call_contract() {
    let runtime = runtime(AUTO);
    evaluate(
        &runtime,
        r#"pure('seed').echoWith(fastcat(2, 2), 1/8, pattern => pattern.fast(2))"#,
    );
    runtime.reset_pattern_transform_ir_stats();
    let output = observe(&runtime);
    assert_eq!(
        runtime.pattern_transform_ir_stats().calls,
        0,
        "indexed callbacks must stay behind their batch JavaScript boundary"
    );
    assert!(
        !output.is_empty(),
        "the original callback returned no events"
    );
}

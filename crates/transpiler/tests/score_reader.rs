//! The reader of the score crate reads a text as the transpiler reads it:
//! the same program and the same lanes, for each text it takes.

use rustel_transpiler::{
    CallForm, Held, Row, ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement,
    TranspileOptions, VISUAL_WIDGET_METHODS, call_form, lane_statements, transpile,
};
use std::path::{Path, PathBuf};

/// The options a score evaluation passes, with no lane numbers: each lane
/// call then holds the place of its lane.
fn options() -> TranspileOptions {
    TranspileOptions {
        add_return: false,
        allow_module_syntax: false,
        list_mini_leaves: false,
        widget_methods: VISUAL_WIDGET_METHODS
            .iter()
            .map(|method| (*method).to_owned())
            .collect(),
        ..TranspileOptions::default()
    }
}

fn scores_under(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scores_under(&path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "strudel")
        {
            out.push(path);
        }
    }
}

/// Each expression of `program`, also those that another one holds.
fn each_expression<'p, 's>(
    program: &'p ScoreProgram<'s>,
    visit: &mut dyn FnMut(&'p ScoreExpr<'s>),
) {
    fn function<'p, 's>(
        held: &'p Held<'s>,
        function: &'p ScoreFunction<'s>,
        visit: &mut dyn FnMut(&'p ScoreExpr<'s>),
    ) {
        for (_, value) in &function.bindings {
            expression(held, value, visit);
        }
        expression(held, &function.result, visit);
    }
    fn expression<'p, 's>(
        held: &'p Held<'s>,
        node: &'p ScoreExpr<'s>,
        visit: &mut dyn FnMut(&'p ScoreExpr<'s>),
    ) {
        visit(node);
        let mut row = |row: Row| {
            for node in held.row(row) {
                expression(held, node, visit);
            }
        };
        match node {
            ScoreExpr::Number(_) | ScoreExpr::String(_) | ScoreExpr::Name(_) => {}
            ScoreExpr::Object(properties) => {
                for property in properties {
                    match property {
                        ScoreProperty::Entry { value, .. } | ScoreProperty::Spread(value) => {
                            expression(held, value, visit);
                        }
                    }
                }
            }
            ScoreExpr::Array(elements) => row(*elements),
            ScoreExpr::Member { object, .. } => expression(held, held.at(*object), visit),
            ScoreExpr::Call { arguments, .. } => row(*arguments),
            ScoreExpr::Method {
                receiver,
                arguments,
                ..
            } => {
                row(*arguments);
                expression(held, held.at(*receiver), visit);
            }
            ScoreExpr::Arrow(arrow) => function(held, arrow, visit),
        }
    }
    for statement in &program.statements {
        match statement {
            ScoreStatement::Binding { value, .. } | ScoreStatement::Expression(value) => {
                expression(&program.held, value, visit);
            }
            ScoreStatement::Function { function: body, .. } => {
                function(&program.held, body, visit);
            }
        }
    }
}

/// `program` with an empty id in each widget call. The id names the place of
/// the widget in the text, and the reader writes none.
fn without_widget_ids(mut program: ScoreProgram) -> ScoreProgram {
    let mut widgets = Vec::new();
    each_expression(&program, &mut |node| {
        if let ScoreExpr::Method {
            method, arguments, ..
        } = node
            && matches!(call_form(method), CallForm::Widget { .. })
        {
            widgets.push(*arguments);
        }
    });
    for arguments in widgets {
        if let Some(ScoreExpr::String(id)) = program.held.row_mut(arguments).first_mut() {
            *id = "".into();
        }
    }
    program
}

/// Each call that the reader says is written in `source` is there: its text
/// starts with no space and ends with the `)` of the call.
fn assert_calls_are_where_written(name: &str, source: &str, program: &ScoreProgram) {
    each_expression(program, &mut |node| {
        let (ScoreExpr::Call { written, .. } | ScoreExpr::Method { written, .. }) = node else {
            return;
        };
        if let Some(text) = written.text(source) {
            assert!(
                text.ends_with(')') && !text.starts_with(char::is_whitespace),
                "{name}: {text:?}"
            );
        }
    });
}

/// Whether the reader takes `source`, after the check that it then reads
/// what the transpiler reads.
fn agrees(name: &str, source: &str) -> bool {
    let Some(read) = rustel_score::read(source, call_form) else {
        return false;
    };
    assert_calls_are_where_written(name, source, &read.program);
    let output = transpile(source, &options());
    assert_eq!(output.diagnostics, [], "{name}: {source}");
    assert_eq!(
        output.program.map(without_widget_ids),
        Some(without_widget_ids(read.program)),
        "{name}: {source}"
    );
    assert_eq!(
        lane_statements(source),
        Some(read.lanes.to_statements(source)),
        "{name}: {source}"
    );
    true
}

#[test]
fn the_reader_reads_each_score_of_the_corpus_as_the_transpiler_reads_it() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime");
    let mut scores = Vec::new();
    scores_under(&runtime.join("tests/e2e/scores"), &mut scores);
    scores_under(&runtime.join("examples/songs"), &mut scores);
    scores.sort();
    assert!(scores.len() > 1000, "the corpus is {} scores", scores.len());
    let mut taken = 0;
    for path in &scores {
        let source = std::fs::read_to_string(path).expect("a score");
        taken += usize::from(agrees(&path.display().to_string(), &source));
    }
    // A floor, so a reader that takes fewer texts does not pass in silence.
    assert!(taken >= 1250, "the reader takes {taken} scores");
}

#[test]
fn the_reader_takes_each_example_song() {
    let songs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime/examples/songs");
    let mut scores = Vec::new();
    scores_under(&songs, &mut scores);
    assert!(!scores.is_empty());
    for path in scores {
        let source = std::fs::read_to_string(&path).expect("a score");
        assert!(agrees(&path.display().to_string(), &source), "{path:?}");
    }
}

/// Texts at the edges of what the reader takes.
const EDGES: &[&str] = &[
    // Lanes.
    "$: s(\"bd\")",
    "$: s(\"bd\")\n.fast(2)",
    "$: s(\"bd\").\nfast(2)",
    "$: s(\"bd\")\n  .fast(2) // faster\n  .gain(0.5)\n",
    "kick: s(\"bd\")\n$: s(\"hh\")",
    "_$: s(\"bd\")\n$: s(\"hh\")",
    "$:\n  s(\"bd\")",
    "$: s(\"bd\") // c",
    "$: s(\"bd\"); $: s(\"hh\")",
    "$ : s(\"bd\")",
    "$\n: s(\"bd\")",
    "/* c */ $: s(\"bd\")",
    "$: s(\"bd\")\n/* a\n b */ $: s(\"hh\")",
    "// c\n$: s(\"bd\")",
    "$: ;",
    "$:",
    "$: s(\"bd\")\n$:",
    "$: s(\"bd\")\n$:\n",
    "5: s(\"bd\")",
    "const: s(\"bd\")",
    "a::b",
    "$: const x = s(\"bd\")",
    "$: 5",
    "$: (5)",
    "$: 'x'",
    "$: x => x",
    "$: -1",
    "$: {a: 1}",
    "$: a + b",
    "$: [s(\"bd\")]",
    "$: s(\"bd\")\n+ 1",
    "$: foo: s(\"bd\")",
    "$: s(\"bd\")\r\n.fast(2)\r\n$: s(\"hh\")\r\n",
    "setcpm(120/4)\n\n$: s(\"bd*4\")\n\n// drums\n$: s(\"hh*8\").gain(.5)\n",
    // Statements.
    "const a = s(\"bd\")\n$: a",
    "const a =\n s(\"bd\")\na",
    "const a\n= 1",
    "let a = 1\na",
    "let a\na",
    "var a = 1",
    "const {a} = b",
    "const a = 1, b = 2",
    "const eval = 1",
    "const a = 1\nconst a = 2",
    "function f(a) { return a.fast(2) }\n$: f(s(\"bd\"))",
    "function f(a) {\n  const b = a.fast(2)\n  return b.rev()\n}\nf(s(\"bd\"))",
    "function f(a)\n{ return a }",
    "function f(a) { return a } $: f(1)",
    "function f(a) { return a } f(1)",
    "function f(a) { return a };",
    "function f(a) { return\na }",
    "function f(a) { return a; }\nf(1)",
    "function f(a) { const a = 1; return a }",
    "function f(a, a) { return a }",
    "function f(a) { return a }\nfunction f(b) { return b }",
    "function K(a) { return a }\nK(1)",
    "function* f(a) { return a }",
    "async function f(a) { return a }",
    "a()\nb()",
    "a() b()",
    "a()\n(b)",
    "a()\n[b]",
    "a()\n+1",
    "a();;",
    ";",
    "a()\n;",
    "1\n-1",
    "a()\n.b()",
    "a()\n. b()",
    "a() /* x\n */ b()",
    "a() /* x */\nb()",
    "{ a() }",
    "",
    "// nothing\n",
    "/* x */",
    // Arrows.
    "s(\"bd\").every(4, x => x.fast(2))",
    "s(\"bd\").every(4, (x) => x.fast(2))",
    "s(\"bd\").every(4, x =>\n x.rev())",
    "(x, y) => x",
    "() => 1",
    "x => { const y = x.fast(2); return y }",
    "x => { return x }",
    "x => ({a: x})",
    "x\n=> x",
    "(a, a) => a",
    "async x => x",
    "x => y => x.add(y)",
    "(x = 1) => x",
    "(...x) => x",
    // Numbers.
    "1 + 2 * 3",
    "(1 + 2) * 3",
    "8 / 2 / 2",
    "-1",
    "- -1",
    "--1",
    "+1",
    "-(1 + 2)",
    "-a",
    "1 - -1",
    "2 ** 3",
    ".5",
    "5.",
    "1e3",
    "1.5e-3",
    "1e",
    "0x10",
    "1_000",
    "01",
    "0",
    "0.5",
    "1n",
    "1..x",
    "(1).x",
    "1.x",
    "s(\"bd\").gain(.5)",
    // Strings.
    "'a'.x",
    "\"use strict\"",
    "'use strict'",
    "('use strict')",
    "s('bd')",
    "s(`bd\nsd`)",
    "s(`bd ${x}`)",
    "foo`bar`",
    "foo\n`bar`",
    "s(\"a\\nb\")",
    "s(\"\\u{1F600}\")",
    "s('\\uD800')",
    "s(\"\\uD83D\\uDE00\")",
    "s(\"a\\\nb\")",
    "s(\"a\nb\")",
    "s(\"bd)",
    "\"a\" \"b\"",
    "s(\"\\x41\\0\")",
    "s(\"\\07\")",
    "// mini-off\ns(\"bd\")\n// mini-on\ns(\"sd\")",
    "/* mini-off */ s(\"bd\")",
    "//mini-off-ish\ns(\"bd\")",
    "// note: mini-off\ns(\"bd\")",
    "// mini-off\ns(`bd`)",
    // Objects and arrays.
    "({a: 1, 'b': 2, c})",
    "s(\"0 1\").pick({a: s(\"bd\"), b: s(\"hh\")})",
    "({...a, b: 1})",
    "({__proto__: 1})",
    "({\"a\": 1})",
    "({a() {}})",
    "({get a() { return 1 }})",
    "({1: 2})",
    "({[a]: 1})",
    "({class})",
    "({a: 1,})",
    "[1, 2,]",
    "[,1]",
    "[...a]",
    "[[1, [2]], []]",
    // Calls and members.
    "(foo)(1)",
    "(a.b)(1)",
    "a()()",
    "a?.b",
    "a?.()",
    "new Foo()",
    "a.b.c()",
    "a.class.in()",
    "a.#b",
    "a[0]",
    "a(...b)",
    "a(1,)",
    "a(,)",
    "K(\"x\")",
    "foo.K(1)",
    "slider(1)",
    "s(\"bd\").pianoroll()",
    "s(\"bd\")._scope()",
    "s(\"bd\").scope().pianoroll()\n$: s(\"hh\")._scope()",
    "stack(s(\"bd\")._scope(), s(\"hh\")._scope()).markcss()._punchcard()",
    "s(\"bd\"). scope()",
    "s(\"bd\").scope ()",
    "s(\"bd\").scope\n()",
    "s(\"bd\").scope( )",
    "s(\"bd\").scope(\n)",
    "s(\"bd\").scope/* c */()",
    "s(\"bd\").pianoroll({ labels: 1 })",
    "s(\"bd\").markcss()",
    "s(\"bd\").markcss('x')",
    "(s(\"bd\").scope)()",
    "(s(\"bd\").scope)('x')",
    "s(\"bd\").every(2, x => x.scope())",
    "all(s(\"bd\")._scope())",
    "all(pianoroll(s(\"bd\")._scope()))",
    "function scope(a) { return a }\nscope(1)",
    "scope(1)",
    "s(\"bd\").scope",
    "all(pianoroll)",
    "all(pianoroll({ labels: 1 }))",
    "all(x => x.room(0.5))",
    "samples('x')",
    "s(\"bd\").midi()",
    "arrange([1, s(\"bd\")])",
    "m(\"bd\")",
    "const m = 1\ns(\"bd\")",
    "note(\"c e g\").s(\"sawtooth\").lpf(sine.range(200, 2000).slow(4))",
    // Words.
    "await x",
    "this.x",
    "typeof x",
    "x in y",
    "a()\nin b",
    "undefined",
    "caf\u{e9}()",
    "s(\"\u{e9}\")",
    "// \u{e9}\ns(\"bd\")",
    "a()\u{2028}b()",
    "a\\u0062()",
    // Comments.
    "s(\"bd\") // x",
    "s(/* x */ \"bd\")",
    "s(\"bd\" /* never closed",
    "s(\"bd\") /* x */ .fast(2)",
    "<!-- x\ns(\"bd\")",
];

#[test]
fn the_reader_reads_each_edge_as_the_transpiler_reads_it() {
    let mut taken = 0;
    for source in EDGES {
        taken += usize::from(agrees("an edge", source));
    }
    assert!(taken >= 90, "the reader takes {taken} edges");
}

#[test]
fn the_reader_reads_deep_texts_as_the_transpiler_reads_them() {
    let chain = |calls: usize| format!("s(\"bd\"){}", ".fast(2)".repeat(calls));
    let nested = |levels: usize| format!("{}1{}", "(".repeat(levels), ")".repeat(levels));
    let arrows = |levels: usize| format!("{}x", "x => ".repeat(levels));
    for source in [
        chain(100),
        chain(509),
        chain(510),
        chain(511),
        chain(512),
        chain(600),
        format!("$: {}", chain(509)),
        format!("$: {}", chain(510)),
        format!("$: {}", chain(511)),
        nested(50),
        nested(200),
        arrows(50),
        arrows(200),
    ] {
        agrees("a deep text", &source);
    }
    assert!(agrees("a chain", &chain(100)));
    // The transpiler gives a slot to the first 256 widgets.
    assert!(agrees(
        "many widgets",
        &format!("s(\"bd\"){}", ".scope()".repeat(260))
    ));
}

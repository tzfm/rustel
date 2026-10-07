//! Differential parity between the two fraction.js ports.
//!
//! The engine computes with `rustel_fraction::Fraction` (i128); scripts see the
//! `Fraction` global installed in QuickJS. Both are ports of fraction.js 5.2.1
//! and both must answer the same question the same way: `fast(1/3)` on the
//! engine side (`Fraction::from_f64`) and `Fraction(1/3)` in a score must be
//! the same rational, and so must every string form and every arithmetic
//! result. Nothing else guards that agreement; this test does.
//!
//! The engine side is `i128` by documented policy. Where it refuses a value
//! (`None`), the script side may either throw or produce a rational the engine
//! cannot hold; both count as agreement. Where the engine answers, the script
//! must answer identically.

use std::cmp::Ordering;
use std::fmt::Write as _;

use rustel_fraction::Fraction;
use rustel_jsruntime::JsRuntime;

/// Engine-side result encoding shared with the script: `show()` or `!`.
fn encode(value: Option<Fraction>) -> String {
    value.map_or_else(|| "!".to_string(), |value| value.show())
}

fn ordering(value: Option<Ordering>) -> String {
    match value {
        Some(Ordering::Less) => "-1".to_string(),
        Some(Ordering::Equal) => "0".to_string(),
        Some(Ordering::Greater) => "1".to_string(),
        None => "!".to_string(),
    }
}

/// A JavaScript number literal that parses to exactly `value`.
fn js_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value.is_infinite() {
        (if value > 0.0 { "Infinity" } else { "-Infinity" }).to_string()
    } else {
        format!("{value:?}")
    }
}

/// A JavaScript string literal for `value`; anything outside printable ASCII
/// is escaped so control characters and line terminators survive verbatim.
fn js_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(ch),
            other => write!(out, "\\u{{{:X}}}", other as u32).unwrap(),
        }
    }
    out.push('"');
    out
}

/// xorshift64*: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    fn sign(&mut self) -> f64 {
        if self.below(4) == 0 { -1.0 } else { 1.0 }
    }
}

fn number_cases() -> Vec<f64> {
    let mut cases = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        0.1,
        0.2,
        0.3,
        0.1 + 0.2,
        1.0 / 3.0,
        2.0 / 3.0,
        1.0 / 7.0,
        22.0 / 7.0,
        0.75,
        -0.75,
        1.0 / 1024.0,
        std::f64::consts::PI,
        std::f64::consts::E,
        std::f64::consts::SQRT_2,
        1e-7,
        1e-6,
        1.5e-5,
        123_456.789,
        1e6 + 0.5,
        9_007_199_254_740_992.0,
        9_007_199_254_740_993.0,
        1e15,
        1e18,
        1.7e38,
        -1.7e38,
        // Exactly -2^127, the one value the engine holds but JS reads as a
        // magnitude one past i128::MAX.
        -170_141_183_460_469_231_731_687_303_715_884_105_728.0,
        170_141_183_460_469_231_731_687_303_715_884_105_728.0,
        1.8e38,
        1e39,
        1e300,
        -1e300,
        f64::MIN_POSITIVE,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // Ordinary rationals as a score would write them: `5/16`, `-7/12`.
    for _ in 0..300 {
        let denominator = rng.below(5_000) + 1;
        let numerator = rng.below(denominator * 5) + 1;
        cases.push(rng.sign() * numerator as f64 / denominator as f64);
    }
    for _ in 0..150 {
        cases.push(rng.sign() * rng.unit());
    }
    for _ in 0..150 {
        cases.push(rng.sign() * (1.0 + rng.unit() * 9_999.0));
    }
    for _ in 0..60 {
        cases.push(rng.sign() * rng.below(1_000_000_000_000) as f64);
    }
    // Small magnitudes drive the longest Farey walks; keep the group short.
    for _ in 0..10 {
        cases.push(1e-5 + rng.unit() * 1e-3);
    }
    cases
}

fn string_cases() -> Vec<String> {
    let mut cases: Vec<String> = [
        "0",
        "1",
        "-1",
        "+1",
        "-",
        "+",
        "",
        " ",
        "3/4",
        "-3/4",
        "+3/4",
        "3:4",
        "1.5",
        "-1.5",
        ".5",
        "5.",
        "0.1",
        "1_000/3",
        "1 1/2",
        "-1 1/2",
        "1  1/2",
        " 1/2",
        "1/2 ",
        "0.(3)",
        "0.'3'",
        "1.(6)",
        "-0.1(6)",
        "1/0",
        "0/0",
        "abc",
        "1/2/3",
        "1e3",
        "0x10",
        "١",
        "170141183460469231731687303715884105727",
        "170141183460469231731687303715884105728",
        "-170141183460469231731687303715884105728",
        "1/170141183460469231731687303715884105727",
        "1/170141183460469231731687303715884105728",
        "99999999999999999999999999999999999999999",
        // fraction.js reads each token with `BigInt(token)`, and
        // `BigInt(" ")` is `0n`; its tokeniser also drops line terminators.
        "\t",
        "\u{A0}",
        "\u{FEFF}",
        "\u{3000}",
        "- ",
        "+ ",
        "_ ",
        ". ",
        " .5",
        "1\n",
        "\n1",
        "1\r\n/\u{2028}3",
        "\n",
        " -",
        "1/ ",
        " / ",
        "\u{85}",
        "\u{200B}",
        "\u{FF11}",
        "1e5",
        "Infinity",
        "NaN",
        "..5",
        "1.(",
        "1.2(3",
        "1.'2'3",
        "1.(2)(3)",
        "(3)",
        "1/2/",
        "1 /2",
        "1 1 1/2",
        "1.\u{3000}",
        "\u{1F600}",
        "1\u{1F600}/2",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let digits = |rng: &mut Rng, max_len: u64| -> String {
        let len = rng.below(max_len) + 1;
        (0..len)
            .map(|_| char::from(b'0' + rng.below(10) as u8))
            .collect()
    };
    for _ in 0..400 {
        let sign = match rng.below(6) {
            0 => "-",
            1 => "+",
            _ => "",
        };
        let body = match rng.below(7) {
            0 => digits(&mut rng, 12),
            1 => format!("{}/{}", digits(&mut rng, 9), digits(&mut rng, 9)),
            2 => format!("{}.{}", digits(&mut rng, 6), digits(&mut rng, 8)),
            3 => format!(
                "{} {}/{}",
                digits(&mut rng, 4),
                digits(&mut rng, 4),
                digits(&mut rng, 4)
            ),
            4 => format!(
                "{}.{}({})",
                digits(&mut rng, 3),
                digits(&mut rng, 4),
                digits(&mut rng, 4)
            ),
            5 => format!("{}:{}", digits(&mut rng, 6), digits(&mut rng, 6)),
            _ => format!(
                "{}_{}/{}",
                digits(&mut rng, 3),
                digits(&mut rng, 3),
                digits(&mut rng, 3)
            ),
        };
        let mut text = format!("{sign}{body}");
        // Splice one stray token into a quarter of the forms so the fuzz
        // reaches the tokeniser's whitespace, junk, and line-terminator paths.
        if rng.below(4) == 0 {
            const STRAY: [&str; 16] = [
                " ", "\t", "\n", "\r", "\u{A0}", "\u{FEFF}", "\u{3000}", "x", ".", "(", ")", "'",
                "/", ":", "-", "_",
            ];
            let stray = STRAY[rng.below(STRAY.len() as u64) as usize];
            let boundaries: Vec<usize> = (0..=text.len())
                .filter(|i| text.is_char_boundary(*i))
                .collect();
            let at = boundaries[rng.below(boundaries.len() as u64) as usize];
            text.insert_str(at, stray);
        }
        cases.push(text);
    }
    cases
}

fn arithmetic_values() -> Vec<Fraction> {
    let mut values: Vec<Fraction> = [
        "0/1",
        "1/1",
        "-1/1",
        "1/2",
        "-1/2",
        "1/3",
        "-2/3",
        "3/4",
        "5/4",
        "-7/8",
        "1/7",
        "22/7",
        "-22/7",
        "1/12",
        "1/16",
        "3/16",
        "-9/16",
        "100/1",
        "-100/1",
        "1/1000000",
        "999999/1000000",
        "1/10000000",
        "123456789/1000",
        "-5/3",
        "8/5",
        "13/8",
        "21/13",
        "-1/1024",
        "7/1",
        "-7/1",
        "1/100",
        "3/100",
        "2/1",
        "-3/1",
        "1/6",
        "-1/6",
    ]
    .into_iter()
    .map(|text| text.parse().expect("fixture rational"))
    .collect();
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..24 {
        let denominator = rng.below(1_000_000) as i128 + 1;
        let numerator = rng.below(2_000_000) as i128 - 1_000_000;
        values.push(Fraction::new(numerator, denominator));
    }
    values
}

fn binary_expectation(a: Fraction, b: Fraction) -> String {
    let nonzero = b != Fraction::ZERO;
    [
        encode(a.checked_add(b)),
        encode(a.checked_sub(b)),
        encode(a.checked_mul(b)),
        encode(nonzero.then(|| a.checked_div(b)).flatten()),
        encode(a.checked_rem(b)),
        encode(Some(a.gcd(b))),
        encode(a.checked_lcm(b)),
        ordering(a.checked_cmp(b)),
    ]
    .join("|")
}

fn unary_expectation(a: Fraction) -> String {
    [
        encode(Some(a.floor())),
        encode(Some(a.ceil())),
        encode(a.checked_neg()),
    ]
    .join("|")
}

fn parity_script() -> String {
    let mut script = String::from(
        r#"(function () {
  const LIMIT = BigInt("170141183460469231731687303715884105727");
  const show = (f) => String(f.s * f.n) + "/" + String(f.d);
  // A rational the engine cannot hold counts as refused, matching its `None`.
  const held = (f) => f.d <= LIMIT && (f.s < 0n ? f.n <= LIMIT + 1n : f.n <= LIMIT);
  const attempt = (thunk) => {
    try {
      const f = thunk();
      return held(f) ? show(f) : "!";
    } catch (_) {
      return "!";
    }
  };
  const mismatches = [];
  const check = (label, expected, actual) => {
    if (expected !== actual) mismatches.push(label + ": engine " + expected + " vs script " + actual);
  };
"#,
    );

    script.push_str("  const numbers = [\n");
    for value in number_cases() {
        writeln!(
            script,
            "    [{}, {}],",
            js_number(value),
            js_string(&encode(Fraction::from_f64(value)))
        )
        .unwrap();
    }
    script.push_str(
        "  ];\n  for (const [value, expected] of numbers) {\n    check(\"Fraction(\" + \
         String(value) + \")\", expected, attempt(() => Fraction(value)));\n  }\n",
    );

    script.push_str("  const strings = [\n");
    for value in string_cases() {
        writeln!(
            script,
            "    [{}, {}],",
            js_string(&value),
            js_string(&encode(value.parse().ok()))
        )
        .unwrap();
    }
    script.push_str(
        "  ];\n  for (const [value, expected] of strings) {\n    check(\"Fraction(\" + \
         JSON.stringify(value) + \")\", expected, attempt(() => Fraction(value)));\n  }\n",
    );

    let values = arithmetic_values();
    script.push_str("  const values = [\n");
    for value in &values {
        writeln!(
            script,
            "    [{}, {}],",
            js_string(&value.show()),
            js_string(&unary_expectation(*value))
        )
        .unwrap();
    }
    script.push_str(
        r#"  ].map(([text, unary]) => [Fraction(text), text, unary]);
  for (const [a, text, unary] of values) {
    const actual = [attempt(() => a.floor()), attempt(() => a.ceil()), attempt(() => a.neg())].join("|");
    check(text + " unary", unary, actual);
  }
"#,
    );

    script.push_str("  const pairs = [\n");
    for (i, a) in values.iter().enumerate() {
        for (j, b) in values.iter().enumerate() {
            writeln!(
                script,
                "    [{i}, {j}, {}],",
                js_string(&binary_expectation(*a, *b))
            )
            .unwrap();
        }
    }
    script.push_str(
        r#"  ];
  for (const [i, j, expected] of pairs) {
    const [a, left] = values[i];
    const [b, right] = values[j];
    const actual = [
      attempt(() => a.add(b)), attempt(() => a.sub(b)), attempt(() => a.mul(b)),
      attempt(() => a.div(b)), attempt(() => a.mod(b)), attempt(() => a.gcd(b)),
      attempt(() => a.lcm(b)), String(a.compare(b)),
    ].join("|");
    check(left + " . " + right + " [add|sub|mul|div|mod|gcd|lcm|compare]", expected, actual);
  }
  if (mismatches.length > 0) {
    throw new Error(mismatches.length + " parity mismatches:\n" + mismatches.slice(0, 40).join("\n"));
  }
})()
"#,
    );
    script
}

#[test]
fn engine_and_script_fractions_agree() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    if let Err(error) = runtime.eval(&parity_script()) {
        panic!("{error}");
    }
}

#[test]
fn replaced_bigint_cannot_make_infinite_fraction_panic() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");

    let error = runtime
        .eval("globalThis.__savedBigInt = BigInt; globalThis.BigInt = () => 0n; Fraction(Infinity)")
        .expect_err("an infinite Fraction must be rejected");
    assert!(
        error.contains("RangeError: cannot convert NaN or Infinity to BigInt"),
        "{error}"
    );

    runtime
        .eval("globalThis.BigInt = globalThis.__savedBigInt; Fraction(1n).add(1n)")
        .expect("the runtime must keep evaluating after the rejected input");
}

#[test]
fn replaced_bigint_cannot_make_fraction_method_divide_by_zero() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");

    let error = runtime
        .eval("const f = Fraction(2); globalThis.__savedBigInt = BigInt; globalThis.BigInt = () => 0n; f.pow(0.5)")
        .expect_err("the coerced zero denominator must be rejected");
    assert!(error.contains("Division by Zero"), "{error}");

    runtime
        .eval("globalThis.BigInt = globalThis.__savedBigInt; Fraction(2).pow(0.5)")
        .expect("the runtime must keep evaluating after the rejected input");
}

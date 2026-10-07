//! Manual release benchmark for JavaScript-compatible value rendering.
//!
//! Scheduler onset identity and several diagnostics render arbitrary `Value`
//! trees. These Rust-only workloads separate primitive controls from flat and
//! nested containers without depending on score syntax or a musical recipe.
//! Run with:
//!
//! ```text
//! cargo test --release -p rustel-core --test value_serialization_workloads -- --ignored --nocapture
//! ```

use std::hint::black_box;
use std::time::Instant;

use rustel_core::Value;

#[derive(Clone, Copy)]
enum RenderMode {
    Show,
    Json,
    Compact,
}

impl RenderMode {
    fn render(self, value: &Value) -> String {
        match self {
            Self::Show => value.show(),
            Self::Json => value
                .json_stringify()
                .unwrap_or_else(|| "<no-json-value>".to_owned()),
            Self::Compact => value.compact_json(),
        }
    }
}

struct Workload {
    id: &'static str,
    mode: RenderMode,
    values: Vec<Value>,
}

fn primitive_strings() -> Vec<Value> {
    (0..64)
        .map(|index| Value::Str(format!("text-{index}-comma,quote\"slash\\line\n")))
        .collect()
}

fn primitive_numbers() -> Vec<Value> {
    (0..64)
        .map(|index| {
            let exponent = index % 13 - 6;
            Value::F64((index as f64 + 0.125) * 10_f64.powi(exponent))
        })
        .collect()
}

fn flat_objects() -> Vec<Value> {
    (0..64)
        .map(|index| {
            Value::object([
                ("alpha".into(), Value::Str(format!("item-{}", index % 16))),
                ("beta".into(), Value::F64(index as f64 + 0.25)),
                ("gamma".into(), Value::F64(index as f64 / 7.0)),
                ("delta".into(), Value::Bool(index % 2 == 0)),
                ("epsilon".into(), Value::Null),
                ("zeta".into(), Value::F64(1e-7 * (index + 1) as f64)),
            ])
        })
        .collect()
}

fn nested_values() -> Vec<Value> {
    (0..32)
        .map(|index| {
            Value::object([
                (
                    "tail".into(),
                    Value::Str(format!("tail-{index},\"quoted\"")),
                ),
                ("10".into(), Value::F64(10.0 + index as f64)),
                ("2".into(), Value::F64(2.0 + index as f64)),
                ("omitted".into(), Value::Undefined),
                (
                    "items".into(),
                    Value::List(vec![
                        Value::Undefined,
                        Value::Bool(index % 2 == 0),
                        Value::object([
                            ("inner".into(), Value::Str("line\nslash\\comma,".into())),
                            ("value".into(), Value::F64(index as f64 / 3.0)),
                        ]),
                    ]),
                ),
            ])
        })
        .collect()
}

fn omitted_properties() -> Vec<Value> {
    (0..32)
        .map(|index| {
            Value::object([
                (
                    format!("first-{index}-{}", "escaped\"key\\".repeat(32)),
                    Value::Undefined,
                ),
                ("kept".into(), Value::F64(index as f64)),
                (
                    format!("last-{index}-{}", "escaped\"key\\".repeat(32)),
                    Value::Undefined,
                ),
            ])
        })
        .collect()
}

fn text_containers(text: &str) -> Vec<Value> {
    (0..32)
        .map(|index| {
            Value::object([
                (format!("{text}-{index}"), Value::Str(text.repeat(8))),
                (
                    "items".into(),
                    Value::List(vec![Value::Str(text.into()), Value::F64(index as f64)]),
                ),
            ])
        })
        .collect()
}

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            id: "show-primitive-strings",
            mode: RenderMode::Show,
            values: primitive_strings(),
        },
        Workload {
            id: "show-primitive-numbers",
            mode: RenderMode::Show,
            values: primitive_numbers(),
        },
        Workload {
            id: "json-primitive-strings",
            mode: RenderMode::Json,
            values: primitive_strings(),
        },
        Workload {
            id: "json-primitive-numbers",
            mode: RenderMode::Json,
            values: primitive_numbers(),
        },
        Workload {
            id: "show-flat-objects",
            mode: RenderMode::Show,
            values: flat_objects(),
        },
        Workload {
            id: "show-nested-values",
            mode: RenderMode::Show,
            values: nested_values(),
        },
        Workload {
            id: "json-flat-objects",
            mode: RenderMode::Json,
            values: flat_objects(),
        },
        Workload {
            id: "json-nested-values",
            mode: RenderMode::Json,
            values: nested_values(),
        },
        Workload {
            id: "compact-nested-values",
            mode: RenderMode::Compact,
            values: nested_values(),
        },
        Workload {
            id: "json-omitted-properties",
            mode: RenderMode::Json,
            values: omitted_properties(),
        },
        Workload {
            id: "compact-quoted-text",
            mode: RenderMode::Compact,
            values: text_containers("a,\"b\"\\c\n"),
        },
        Workload {
            id: "json-quoted-text",
            mode: RenderMode::Json,
            values: text_containers("a,\"b\"\\c\n"),
        },
        Workload {
            id: "compact-unicode-text",
            mode: RenderMode::Compact,
            values: text_containers("clé,音符\"é"),
        },
        Workload {
            id: "json-unicode-text",
            mode: RenderMode::Json,
            values: text_containers("clé,音符\"é"),
        },
        Workload {
            id: "compact-numeric-arrays",
            mode: RenderMode::Compact,
            values: primitive_numbers()
                .chunks(8)
                .map(|numbers| Value::List(numbers.to_vec()))
                .collect(),
        },
    ]
}

fn setting(name: &str, default: usize, maximum: usize) -> usize {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    let value = raw
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{name} must be a positive integer"));
    assert!(
        (1..=maximum).contains(&value),
        "{name} must be in 1..={maximum}"
    );
    value
}

fn require_release_build() {
    #[cfg(debug_assertions)]
    panic!("value serialization measurements must use cargo test --release");
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn validation(workload: &Workload) -> (u64, usize) {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut bytes = 0_usize;
    for value in &workload.values {
        let rendered = workload.mode.render(value);
        assert!(
            !rendered.is_empty(),
            "{} rendered an empty value",
            workload.id
        );
        hash_bytes(&mut hash, rendered.as_bytes());
        hash_bytes(&mut hash, &[0xff]);
        bytes = bytes.saturating_add(rendered.len());
    }
    (hash, bytes)
}

fn render_round(workload: &Workload) -> usize {
    let mut bytes = 0_usize;
    for value in &workload.values {
        let rendered = workload.mode.render(black_box(value));
        bytes = bytes.wrapping_add(black_box(rendered.as_bytes()).len());
    }
    bytes
}

#[test]
fn representative_container_rendering_is_pinned() {
    let value = Value::object([
        ("tail".into(), Value::Str("a,\"b\"\\c\n".into())),
        ("10".into(), Value::F64(10.0)),
        ("2".into(), Value::F64(2.0)),
        (
            "items".into(),
            Value::List(vec![Value::Undefined, Value::Bool(true)]),
        ),
        // Pin omission between two emitted string-key properties so a direct
        // writer must also remove its speculative separator and key.
        ("omitted".into(), Value::Undefined),
        ("after".into(), Value::Null),
    ]);

    assert_eq!(
        value.json_stringify().as_deref(),
        Some(r#"{"2":2,"10":10,"tail":"a,\"b\"\\c\n","items":[null,true],"after":null}"#)
    );
    assert_eq!(
        value.compact_json(),
        r#"2:2 10:10 tail:a \b\\\c\n items:[null true] after:null"#
    );
    assert_eq!(value.show(), value.compact_json());
}

#[test]
fn compact_rendering_preserves_unicode_and_opaque_root_delimiters() {
    struct MaterializeHost {
        value: Value,
        calls: std::cell::Cell<usize>,
    }

    impl rustel_core::CallbackHost for MaterializeHost {
        fn call_value(&self, id: rustel_core::CallbackId, _: &Value) -> Result<Value, String> {
            panic!("unexpected value callback {id}")
        }

        fn call_query(
            &self,
            id: rustel_core::CallbackId,
            _: &rustel_core::State,
        ) -> Result<Vec<rustel_core::Hap>, String> {
            panic!("unexpected query callback {id}")
        }

        fn call_materialize_value(&self, id: rustel_core::CallbackId) -> Result<Value, String> {
            assert_eq!(id, 1);
            self.calls.set(self.calls.get() + 1);
            Ok(self.value.clone())
        }
    }

    for (value, native, opaque) in [
        (Value::List(Vec::new()), "", "[]"),
        (Value::object([]), "", "{}"),
        (Value::Str(String::new()), "", ""),
        (Value::List(vec![Value::Str(String::new())]), "", "[]"),
        (
            Value::List(vec![Value::Str("é,音符".into())]),
            "é 音符",
            "[é 音符]",
        ),
        (
            Value::object([("clé".into(), Value::Str("é,音符".into()))]),
            "clé:é 音符",
            "{clé:é 音符}",
        ),
        (Value::Str("é,\"音符\"".into()), "é \\音符\\", "é \\音符\\"),
        (Value::Undefined, "undefined", "undefined"),
    ] {
        assert_eq!(value.compact_json(), native);
        let host = MaterializeHost {
            value,
            calls: std::cell::Cell::new(0),
        };
        rustel_core::with_callback_host(&host, || {
            let value = Value::JsValue(rustel_core::value::JsValueRef::new(1, false));
            assert_eq!(value.compact_json(), opaque);
            assert_eq!(host.calls.get(), 1);
        });
    }
}

#[test]
#[ignore = "manual release benchmark"]
fn value_serialization_workloads() {
    require_release_build();
    // Finish libtest's status line before emitting JSONL records.
    println!();

    let repetitions = setting("RUSTEL_VALUE_BENCH_REPETITIONS", 7, 100);
    let warmup_rounds = setting("RUSTEL_VALUE_BENCH_WARMUP_ROUNDS", 1_024, 100_000);
    let measured_rounds = setting("RUSTEL_VALUE_BENCH_ROUNDS", 8_192, 1_000_000);
    let selected = std::env::var("RUSTEL_VALUE_BENCH_CASE").ok();
    let mut matched = false;

    for workload in workloads() {
        if selected
            .as_deref()
            .is_some_and(|selected| selected != workload.id)
        {
            continue;
        }
        matched = true;
        let (validation_hash, bytes_per_round) = validation(&workload);

        for _ in 0..warmup_rounds {
            black_box(render_round(&workload));
        }

        for repetition in 1..=repetitions {
            let started = Instant::now();
            let mut rendered_bytes = 0_usize;
            for _ in 0..measured_rounds {
                rendered_bytes = rendered_bytes.wrapping_add(render_round(&workload));
            }
            let elapsed_nanos = started.elapsed().as_nanos();
            black_box(rendered_bytes);
            assert_eq!(
                validation(&workload),
                (validation_hash, bytes_per_round),
                "{} changed after measured rendering",
                workload.id
            );

            let renders = measured_rounds.saturating_mul(workload.values.len());
            assert_eq!(
                rendered_bytes,
                bytes_per_round.saturating_mul(measured_rounds),
                "{} rendered a different number of bytes",
                workload.id
            );
            println!(
                "{{\"schema_version\":1,\"benchmark\":\"value-serialization\",\"workload\":\"{}\",\"repetition\":{},\"warmup_rounds\":{},\"measured_rounds\":{},\"values_per_round\":{},\"renders\":{},\"rendered_bytes\":{},\"elapsed_nanos\":{},\"nanos_per_render\":{:.6},\"bytes_per_render\":{:.6},\"validation_hash\":\"{:016x}\"}}",
                workload.id,
                repetition,
                warmup_rounds,
                measured_rounds,
                workload.values.len(),
                renders,
                rendered_bytes,
                elapsed_nanos,
                elapsed_nanos as f64 / renders.max(1) as f64,
                rendered_bytes as f64 / renders.max(1) as f64,
                validation_hash,
            );
        }
    }

    assert!(
        matched,
        "RUSTEL_VALUE_BENCH_CASE did not name a benchmark workload"
    );
}

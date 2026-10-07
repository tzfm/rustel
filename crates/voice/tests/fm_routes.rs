use rustel_audio::{Envelope, FmControls, FmOperator, FmRoute, FmWave};
use serde_json::{Value, json};

fn route_key(source: u8, target: u8) -> String {
    match (source, target) {
        (1, 0) => "fmi".to_owned(),
        (source, target) if source == target + 1 => format!("fmi{source}"),
        (source, target) => format!("fmi{source}{target}"),
    }
}

fn matrix_routes() -> Vec<FmRoute> {
    (1..=8)
        .flat_map(|source| {
            (0..=8).map(move |target| FmRoute {
                source,
                target,
                amount: f32::from(source) + f32::from(target) / 10.0,
                mod_slot: (source == target + 1).then_some(source - 1),
            })
        })
        .collect()
}

fn input(routes: &[FmRoute]) -> Value {
    let mut value = json!({"s": "sine", "note": 60});
    // Reverse insertion does not define the resolver's matrix traversal.
    for route in routes.iter().rev() {
        value[route_key(route.source, route.target)] = json!(route.amount);
    }
    for operator in 1..=8 {
        let suffix = if operator == 1 {
            String::new()
        } else {
            operator.to_string()
        };
        for (prefix, field) in [
            ("fmh", json!(f64::from(operator) + 0.25)),
            ("fmwave", json!("square")),
            ("fmattack", json!(0.02)),
            ("fmdecay", json!(0.03)),
            ("fmsustain", json!(0.4)),
            ("fmrelease", json!(0.05)),
            ("fmenv", json!("linear")),
        ] {
            value[format!("{prefix}{suffix}")] = field;
        }
    }
    value
}

fn expected(routes: &[FmRoute]) -> FmControls {
    let mut controls = FmControls {
        operators: [None; rustel_audio::MAX_FM_OPERATORS],
        routes: [None; rustel_audio::MAX_FM_ROUTES],
    };
    for (slot, route) in routes.iter().enumerate() {
        controls.routes[slot] = Some(*route);
        for operator in [route.source, route.target] {
            if operator == 0 {
                continue;
            }
            controls.operators[usize::from(operator - 1)] = Some(FmOperator {
                harmonicity: f32::from(operator) + 0.25,
                waveform: FmWave::Square,
                env: Some(Envelope {
                    attack_secs: 0.02,
                    decay_secs: 0.03,
                    sustain: 0.4,
                    release_secs: 0.05,
                }),
                env_exponential: false,
            });
        }
    }
    controls
}

fn resolve(value: &Value) -> Result<Option<FmControls>, String> {
    rustel_voice::resolve_voice(value, 7, 0.125, 0.25, 48_000, 2.0).map(|event| event.controls.fm)
}

#[test]
fn every_fm_route_spelling_preserves_the_complete_descriptor() {
    let routes = matrix_routes();
    assert_eq!(routes.len(), 72);
    for route in routes {
        assert_eq!(
            resolve(&input(&[route])),
            Ok(Some(expected(&[route]))),
            "{}",
            route_key(route.source, route.target)
        );
    }
}

#[test]
fn fm_diagonal_aliases_and_empty_amounts_do_not_create_routes() {
    let routes = matrix_routes();
    let mut aliases = input(&[]);
    aliases["fmi1"] = json!("ignored");
    for source in 1..=8 {
        aliases[format!("fmi{source}{}", source - 1)] = json!("ignored");
    }
    assert_eq!(resolve(&aliases), Ok(None));
    for amount in [Value::Null, json!(0), json!(-0.0), json!(false)] {
        let mut value = input(&[]);
        for route in &routes {
            value[route_key(route.source, route.target)] = amount.clone();
        }
        assert_eq!(resolve(&value), Ok(None));
    }
    for (amount, normalized) in [(json!(true), 1.0), (json!(-0.5), -0.5)] {
        let mut route = routes[71];
        route.amount = normalized;
        let mut value = input(&[route]);
        value[route_key(route.source, route.target)] = amount;
        assert_eq!(resolve(&value), Ok(Some(expected(&[route]))));
    }
}

#[test]
fn fm_routes_preserve_source_major_order_without_sorting() {
    for routes in matrix_routes().chunks(16) {
        assert_eq!(resolve(&input(routes)), Ok(Some(expected(routes))));
    }
}

#[test]
fn fm_amount_errors_keep_the_exact_key_and_capacity_precedence() {
    let routes = matrix_routes();
    for route in &routes {
        let key = route_key(route.source, route.target);
        for (amount, reason) in [
            (json!("bad"), "must be a finite number"),
            (json!(f64::MAX), "is outside the finite f32 range"),
        ] {
            let mut value = input(&[]);
            value[key.clone()] = amount;
            assert_eq!(resolve(&value), Err(format!("{key} {reason}")));
        }
    }
    let last_key = route_key(routes[16].source, routes[16].target);
    let full = "FM matrix declares more than 16 connections".to_owned();
    for (amount, error) in [
        (json!(1), full.clone()),
        (json!(f64::MAX), full.clone()),
        (json!("bad"), format!("{last_key} must be a finite number")),
    ] {
        let mut value = input(&routes[..17]);
        value[last_key.clone()] = amount;
        assert_eq!(resolve(&value), Err(error));
    }
    for amount in [Value::Null, json!(0), json!(false)] {
        let mut value = input(&routes[..17]);
        value[last_key.clone()] = amount;
        assert_eq!(resolve(&value), Ok(Some(expected(&routes[..16]))));
    }
    let mut value = input(&routes[..17]);
    value["fmi"] = json!("first");
    value[last_key] = json!("last");
    assert_eq!(
        resolve(&value),
        Err("fmi must be a finite number".to_owned())
    );
    value["fmi"] = json!(1);
    value["fmwave"] = json!("invalid");
    value[route_key(routes[16].source, routes[16].target)] = json!(1);
    assert_eq!(resolve(&value), Err(full));
}

#[test]
fn fm_wave_errors_name_each_used_operator() {
    for source in 1..=8 {
        let route = FmRoute {
            source,
            target: 0,
            amount: 1.0,
            mod_slot: (source == 1).then_some(0),
        };
        let key = if source == 1 {
            "fmwave".to_owned()
        } else {
            format!("fmwave{source}")
        };
        for (wave, error) in [
            (json!(2), format!("{key} must be a waveform name")),
            (json!("invalid"), format!("unsupported {key}: invalid")),
        ] {
            let mut value = input(&[route]);
            value[key.clone()] = wave;
            assert_eq!(resolve(&value), Err(error));
        }
    }
}

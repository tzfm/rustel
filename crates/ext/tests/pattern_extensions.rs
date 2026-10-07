//! Check extensions through the generic registry used by the JavaScript host.

use rustel_core::controls::default_control_registry;
use rustel_core::{Pattern, Value, pure};
use rustel_fraction::Fraction;

fn mini(source: &str) -> Pattern {
    rustel_mini::mini(source).unwrap_or_else(|error| panic!("{source:?}: {error}"))
}

fn control(name: &str, source: &str) -> Pattern {
    default_control_registry()
        .get(name)
        .unwrap_or_else(|| panic!("missing control {name}"))
        .standalone(&mini(source))
}

// The diode list order is amount, volume, algorithm. The broad query sweep
// below does not check these values.
#[test]
fn the_rollers_diode_is_the_distort_control_with_its_shape() {
    use rustel_ext::{PatternCallableBehavior, pattern_callables};

    let receiver = control("s", "bd*4");
    for (name, amount, volume) in [("roller", 1.0, 1.0), ("roller2", 2.5, 0.6)] {
        let callable = pattern_callables()
            .find(|callable| callable.names.contains(&name))
            .unwrap_or_else(|| panic!("{name} is not an extension"));
        let pattern = match callable.behavior {
            PatternCallableBehavior::Stateless(call) => call(&[], Some(&receiver)),
            PatternCallableBehavior::Fallible(call) => {
                call(&[], Some(&receiver)).unwrap_or_else(|error| panic!("{name}: {error}"))
            }
            _ => panic!("{name} is not a plain callable"),
        };
        let haps = pattern.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert!(!haps.is_empty(), "{name}() plays nothing");
        for hap in haps {
            let object = hap.value.as_object().expect("a control object");
            assert_eq!(
                object.get("distort"),
                Some(&Value::F64(amount)),
                "{name}()'s amount"
            );
            assert_eq!(
                object.get("distortvol"),
                Some(&Value::F64(volume)),
                "{name}()'s volume"
            );
            assert_eq!(
                object.get("distorttype"),
                Some(&Value::Str("diode".into())),
                "{name}()'s algorithm"
            );
        }
    }
}

// Call every native body: an invalid control name can panic inside the registry.
#[test]
fn every_native_extension_answers_a_query() {
    use rustel_ext::{PatternCallableBehavior, pattern_callables, pattern_states};

    let state_for = |key: &str| {
        pattern_states()
            .find(|state| state.key == key)
            .map(|state| (state.initial)())
            .unwrap_or_else(|| panic!("extension state {key} is not declared"))
    };
    let receiver = control("s", "bd*4");
    // Enough arguments for the widest body, in a shape every body accepts:
    // a number reads as a number and as a note, and a body that wants fewer
    // ignores the rest.
    let arguments: Vec<Pattern> = vec![pure(Value::F64(1.0)); 3];

    for callable in pattern_callables() {
        for count in 0..=arguments.len() {
            let args = &arguments[..count];
            let name = callable.names.first().copied().unwrap_or("<unnamed>");
            let pattern = match callable.behavior {
                PatternCallableBehavior::Stateless(call) => call(args, Some(&receiver)),
                PatternCallableBehavior::Fallible(call) => match call(args, Some(&receiver)) {
                    Ok(pattern) => pattern,
                    // A refusal is an answer: the host turns it into an error
                    // the player can read.
                    Err(_) => continue,
                },
                PatternCallableBehavior::ReadState { key, call } => {
                    call(&state_for(key), args, Some(&receiver))
                }
                // The host owns the write; there is no body to call.
                PatternCallableBehavior::WriteState { .. } => continue,
            };
            let haps = pattern.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
            assert!(
                haps.iter().all(|hap| hap.part.begin <= hap.part.end),
                "{name} with {count} arguments answered with a backwards span"
            );
        }
    }
}

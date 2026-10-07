use rustel_core::Value;
use rustel_core::combinators as c;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::shared::{binary_in, clip, mini, number, scalar};
use super::trancegate::ribbon_pattern;
use super::{NativeExtensionOperand, ORIGIN, fill_pattern};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "tgate",
    "select a curated trance-gate rhythm",
    "Curated cousin of trancegate: the cycle argument is a preset number that maps to a ribbon seed. Sixteen steps are degraded by 1 − amount, ribboned from that seed across length cycles, their gaps filled, and the receiver is structured with the result and clipped to .7.",
    params: [
        ReferenceParam {
            name: "amount",
            r#type: "number | Pattern",
            description: "gate density, inverted: 0 keeps every step, 1 removes them all.",
        },
        ReferenceParam {
            name: "cycle",
            r#type: "number | Pattern",
            description: "preset number selecting a curated rhythm; the table covers 0-25 and 31-37, any other number acts as its own raw seed.",
        },
        ReferenceParam {
            name: "length",
            r#type: "number | Pattern",
            description: "how many cycles the sixteen-step ribbon spans before repeating.",
        },
    ],
    examples: ["note(\"c e g a\").s(\"sawtooth\").tgate(0.3, 5, 4)"],
    "rhythm"
);

fn substitute_cycle(value: &Value) -> Value {
    // Only a number that is a table key selects a preset. Any other value,
    // a numeric string included, is its own seed, as in her prebake.
    let Some(cycle) = value.as_f64().filter(|cycle| cycle.fract() == 0.0) else {
        return value.clone();
    };
    let substituted = match cycle as i64 {
        0 => 45,
        1 => 116,
        2 => 99,
        3 => 100,
        4 => 107,
        5 => 53,
        6 => 57,
        7 => 58,
        8 => 67,
        9 => 81,
        10 => 89,
        11 => 115,
        12 => 8,
        13 => 118,
        14 => 120,
        15 => 144,
        16 => 149,
        17 => 161,
        18 => 197,
        19 => 206,
        20 => 209,
        21 => 230,
        22 => 269,
        23 => 274,
        24 => 295,
        25 => 308,
        31 => 37,
        32 => 40,
        33 => 59,
        34 => 63,
        35 => 65,
        36 => 68,
        37 => 225,
        _ => return value.clone(),
    };
    Value::F64(substituted as f64)
}

fn apply<P: PatOps + NativeExtensionOperand>(args: &[P], pattern: &P) -> P {
    let Some(amount) = args.first() else {
        return P::pat_silence();
    };
    let Some(cycle) = args.get(1) else {
        return P::pat_silence();
    };
    let Some(length) = args.get(2) else {
        return P::pat_silence();
    };
    let cycle = cycle.fmap(substitute_cycle);
    let base: P = mini("x!16");
    let amount = binary_in(
        &binary_in(amount, &scalar(Value::F64(-1.0)), ComposeOp::Mul),
        &scalar(Value::F64(1.0)),
        ComposeOp::Add,
    );
    let gate = amount.inner_bind(move |amount| c::degrade_by(&base, number(amount)));
    let gate = ribbon_pattern(&gate, &cycle, length);
    clip(&fill_pattern(&c::struct_with(pattern, &gate)), 0.7)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["tgate"],
        REFERENCE,
        4,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| apply(args, &pattern)),
    );
}

#[cfg(test)]
mod tests {
    use super::substitute_cycle;
    use rustel_core::Value;

    /// Only a number that is a table key selects a preset; any other cycle is its
    /// own seed.
    #[test]
    fn only_a_whole_number_in_the_table_picks_a_preset() {
        assert_eq!(substitute_cycle(&Value::F64(5.0)), Value::F64(53.0));
        // -0 is the key 0.
        assert_eq!(substitute_cycle(&Value::F64(-0.0)), Value::F64(45.0));
        for seed in [5.7, -0.5, 26.0, f64::INFINITY] {
            assert_eq!(
                substitute_cycle(&Value::F64(seed)),
                Value::F64(seed),
                "{seed} is no key, so it is its own seed"
            );
        }
        assert!(
            matches!(substitute_cycle(&Value::F64(f64::NAN)), Value::F64(v) if v.is_nan()),
            "a NaN cycle must stay NaN, not fold onto preset 0"
        );
        for seed in [Value::Str("5".into()), Value::Bool(true), Value::Null] {
            assert_eq!(
                substitute_cycle(&seed),
                seed,
                "{seed:?} is no number, so no key either"
            );
        }
    }
}

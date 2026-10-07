use rustel_core::compose::ComposeOp;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{
    apply_control, pattern_argument, pattern_binary, register_func_result, source,
};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "zap",
    "make a falling sine zap",
    "A pitched sine transient whose pitch envelope (amount × 120 semitones) falls from the chosen note, with pdecay and decay both following speed, routed to orbit 8. Called bare it simply plays; as a method it replaces the receiver's values with the zap.",
    params: [
        ReferenceParam {
            name: "amount",
            r#type: "number | Pattern",
            description: "depth of the pitch fall, multiplied by 120 semitones; defaults to .8.",
        },
        ReferenceParam {
            name: "speed",
            r#type: "number | Pattern",
            description: "pdecay and decay of the zap; defaults to .1.",
        },
        ReferenceParam {
            name: "note",
            r#type: "string | number | Pattern",
            description: "the note the zap starts from; defaults to 'c3'.",
        },
    ],
    examples: ["zap()", "zap(0.5, 0.05, \"c2\")"],
    "preset"
);

fn apply(args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let amount = pattern_argument(args, 0, Value::F64(0.8));
    let speed = pattern_argument(args, 1, Value::F64(0.1));
    let note = pattern_argument(args, 2, Value::Str("c3".into()));
    let penv = pattern_binary(
        &amount,
        &rustel_core::pure(Value::F64(120.0)),
        ComposeOp::Mul,
    );
    let mut output = source("sine");
    output = apply_control(&output, "penv", penv);
    output = apply_control(&output, "note", note);
    output = apply_control(&output, "pdecay", speed.clone());
    output = apply_control(&output, "decay", speed);
    register_func_result(
        apply_control(&output, "orbit", rustel_core::pure(Value::F64(8.0))),
        receiver,
    )
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["zap"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::BOTH,
    behavior: PatternCallableBehavior::Stateless(apply),
};

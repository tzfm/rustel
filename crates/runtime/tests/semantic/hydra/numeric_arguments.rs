use super::*;

#[test]
fn non_finite_hydra_numbers_fail_before_wire_serialization() {
    for call in [
        "osc(1/0).out()",
        "osc(-Infinity).out()",
        "osc(NaN).out()",
        "osc([1, NaN]).out()",
    ] {
        let message = evaluate_error(&format!("await initHydra()\n{call}"));
        assert!(
            message.contains("a hydra numeric argument must be finite"),
            "{call}: {message}"
        );
    }
    for mark in ["_speed", "_smooth", "_offset"] {
        let message = evaluate_error(&format!(
            "await initHydra()\nconst values = [1,2]; values.{mark} = Infinity; osc(values).out()"
        ));
        assert!(
            message.contains(&format!("a hydra array {mark} value must be finite")),
            "{message}"
        );
    }
}

#[test]
fn unset_array_marks_keep_their_defaults() {
    let update = program("await initHydra()\nosc([1,2].fast(NaN).smooth(0).offset(0)).out()");
    let HydraStatement::Evaluate {
        node: HydraNode::Chain { args, .. },
    } = &update.program.statements[0]
    else {
        panic!("a drawing chain");
    };
    assert!(matches!(
        &args[0],
        HydraNode::List { speed, smooth, offset, .. }
            if *speed == 1.0 && *smooth == 0.0 && *offset == 0.0
    ));
}

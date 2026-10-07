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

#[test]
fn a_non_finite_argument_preserves_the_committed_score() {
    let mut session = Session::new().expect("session");
    let source = "await initHydra()\nosc(10).out()";
    session.evaluate(source).expect("initial score");
    let previous = session.take_pending_hydra().expect("initial visuals");
    HydraUpdate::from_candidate(&previous).expect("initial visuals read back");

    let error = session
        .evaluate("await initHydra()\nosc([1,2].fast(Infinity)).out()")
        .expect_err("a non-finite speed fails evaluation");
    assert!(
        error
            .to_string()
            .contains("array _speed value must be finite")
    );
    assert_eq!(session.active_source(), Some(source));
    assert!(session.take_pending_hydra().is_none());
}

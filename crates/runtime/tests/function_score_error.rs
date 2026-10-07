use rustel_fraction::Fraction;
use rustel_runtime::Session;
use std::sync::atomic::AtomicBool;

#[test]
fn function_valued_scores_explain_how_to_produce_a_pattern() {
    for source in ["s('bd').noise", "globalThis.helper = () => 42", "() => 42"] {
        let mut session = Session::new().unwrap();
        let message = session.evaluate(source).unwrap_err().to_string();
        assert!(
            message.contains("ends on a function"),
            "{source}: {message}"
        );
        assert!(message.contains(".noise(0.5)"), "{message}");
        assert!(!message.contains("NativePatternWrapper"), "{message}");

        let message = session
            .evaluate_no_fallback_cancellable(source, &AtomicBool::new(false))
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("ends on a function"),
            "{source}: {message}"
        );
    }
}

#[test]
fn a_receiver_conversion_error_keeps_its_type_and_source_location() {
    for source in [
        "pure(1).fast.call(() => {}, 2); pure(1)",
        "pure(1).rev.call(() => {}); pure(1)",
    ] {
        let mut session = Session::new().unwrap();
        for message in [
            session.evaluate(source).unwrap_err().to_string(),
            session
                .evaluate_no_fallback_cancellable(source, &AtomicBool::new(false))
                .unwrap_err()
                .to_string(),
        ] {
            assert!(message.contains("TypeError:"), "{source}: {message}");
            assert!(message.contains("line 1"), "{source}: {message}");
            assert!(
                !message.contains("ends on a function"),
                "{source}: {message}"
            );
        }
    }
}

#[test]
fn a_function_valued_live_reload_keeps_the_playing_pattern() {
    let mut session = Session::new().unwrap();
    let stopped = AtomicBool::new(false);
    session
        .reload_at_cancellable("pure('playing')", false, 0.0, &stopped)
        .unwrap();
    let message = session
        .reload_at_cancellable("s('bd').noise", false, 0.1, &stopped)
        .unwrap_err()
        .to_string();
    assert!(message.contains("ends on a function"), "{message}");
    let events = session.query(Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value, rustel_core::Value::Str("playing".into()));
}

#[test]
fn trailing_register_is_still_valid_definition_only_setup() {
    let mut session = Session::new().unwrap();
    session
        .evaluate("register('identityTail', (pat) => pat)")
        .unwrap();
    assert!(
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    session.evaluate("pure(1).identityTail()").unwrap();
    assert_eq!(
        session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
        1
    );
}

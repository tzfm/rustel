use super::run;

#[test]
fn validate_explains_a_function_valued_score() {
    let output = run(&["validate", "-e", "s('bd').noise"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains("ends on a function"), "{message}");
    assert!(!message.contains("NativePatternWrapper"), "{message}");
}

#[test]
fn validate_accepts_calling_the_trailing_function() {
    let output = run(&["validate", "-e", "s('bd').noise(0.5)"]);
    assert!(output.status.success(), "{output:?}");
}

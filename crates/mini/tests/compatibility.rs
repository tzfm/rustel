use rustel_fraction::Fraction;
use rustel_mini::mini;

#[test]
fn parser_boundaries_and_unicode_tables_are_stable() {
    assert!(mini("").is_err());
    assert!(mini("a'b").is_err());
    assert!(mini("a\u{000b}b").is_err());

    for accepted in ["é", "音", "Ⅻ", "\u{9fef}", "\u{8b6}", "\u{a7b9}"] {
        assert!(mini(accepted).is_ok(), "expected {accepted:?} to parse");
    }
    for rejected in [
        "\u{0345}",
        "\u{05b0}",
        "\u{10400}",
        "\u{9ff0}",
        "\u{8b5}",
        "\u{a7ba}",
    ] {
        assert!(mini(rejected).is_err(), "expected {rejected:?} to fail");
    }
}

#[test]
fn replication_preserves_fractional_step_metadata() {
    let cases = [
        ("a@3", Fraction::int(3)),
        ("a!2.5", Fraction::new(5, 2)),
        ("[^a b]!2.5", Fraction::int(5)),
        ("[^a b]!2.5 c", Fraction::int(7)),
    ];
    for (source, expected) in cases {
        assert_eq!(mini(source).unwrap().steps, Some(expected), "{source}");
    }
}

/// Pins that a feet group reports its foot count as `_steps` like upstream
/// fastcat, ignoring the children's `_steps` but still marking a parent
/// sequence as sourced.
#[test]
fn feet_step_metadata_counts_feet_not_the_childrens_lcm() {
    let cases = [
        ("[^a b] . [c]", Fraction::int(2)),
        ("[^a b] . [c] . [d]", Fraction::int(3)),
        ("[a b] . [c]", Fraction::int(2)),
        ("y [x [^a b] . [c]]", Fraction::int(4)),
    ];
    for (source, expected) in cases {
        assert_eq!(mini(source).unwrap().steps, Some(expected), "{source}");
    }
}

#[test]
fn source_spans_and_error_offsets_remain_available_to_editors() {
    let ast = rustel_mini::parse("bd sd").unwrap();
    assert_eq!(ast.span().start, 0);
    assert_eq!(ast.span().end, 5);

    let error = rustel_mini::parse("[bd sd").unwrap_err();
    assert!(error.offset > 0);
}

use super::*;

#[test]
fn continuous_arithmetic_query_keeps_wholeless_haps_across_cycles() {
    for source in ["s(rand.mul(3))", "s(rand.range(0, 4))", "s(irand(3))"] {
        let runtime = semantic_runtime(source);
        for cycles in [1, 2, 3] {
            let haps = query_active(&runtime, Fraction::ZERO, Fraction::int(cycles))
                .expect("query continuous signal");
            assert_eq!(haps.len(), cycles as usize, "{source} over {cycles} cycles");
            assert!(haps.iter().all(|hap| hap.whole.is_none()));
        }
    }

    let runtime = semantic_runtime("s(rand.mul(3))");
    let haps = runtime
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::int(2),
            &[("_cps", 0.5)],
        )
        .expect("scheduler query continuous signal");
    assert_eq!(haps.len(), 2);
    assert!(haps.iter().all(|hap| hap.whole.is_none()));

    let explicitly_sorted = semantic_runtime("s(rand.mul(3)).sortHapsByPart()");
    assert!(
        query_active(&explicitly_sorted, Fraction::ZERO, Fraction::int(2))
            .expect("explicit comparator throw is caught")
            .is_empty()
    );
    assert!(explicitly_sorted.take_query_throw().is_some());
}

#[test]
fn controls_query_accepts_the_native_active_wrapper() {
    let runtime = semantic_runtime(r#"s("bd")"#);
    let haps = runtime
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
        )
        .expect("query native active Pattern with scheduler controls");
    assert_eq!(haps.len(), 1);
    assert_eq!(haps[0].value.get("s").and_then(Value::as_str), Some("bd"));
}

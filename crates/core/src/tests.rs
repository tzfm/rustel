//! Unit tests for core pattern semantics, limits, and queries.

#[cfg(test)]
mod step_combination_overflow_tests {
    use super::super::*;

    fn stepped(steps: Fraction) -> Pattern {
        pure(Value::Str("bd".into())).with_steps(Some(steps))
    }

    fn refusal(pattern: &Pattern) -> Option<QueryLimit> {
        pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .err()
    }

    #[test]
    fn stacking_and_catenation_refuse_unrepresentable_lcm() {
        let pair = || vec![stepped(Fraction::int(i128::MAX)), stepped(Fraction::int(2))];
        for (pattern, operation) in [(stack(pair()), "stack"), (slowcat(pair()), "slowcat")] {
            assert_eq!(
                refusal(&pattern),
                Some(QueryLimit::NativeFraction { operation }),
                "{operation} should refuse an unrepresentable step LCM"
            );
        }
    }

    /// `fastcat`'s step count is its length, so an unrepresentable LCM of its
    /// inputs' step counts does not refuse it.
    #[test]
    fn fastcat_plays_inputs_whose_step_lcm_is_unrepresentable() {
        let pattern = fastcat(vec![
            stepped(Fraction::int(i128::MAX)),
            stepped(Fraction::int(2)),
        ]);
        assert_eq!(pattern.steps(), Some(Fraction::int(2)));
        let haps = pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("fastcat plays");
        let wholes: Vec<_> = haps.into_iter().map(|hap| hap.whole).collect();
        assert_eq!(
            wholes,
            [
                Some(TimeSpan::new(Fraction::ZERO, Fraction::new(1, 2))),
                Some(TimeSpan::new(Fraction::new(1, 2), Fraction::ONE)),
            ]
        );
    }

    #[test]
    fn app_both_refuses_unrepresentable_lcm() {
        let pattern = stepped(Fraction::int(i128::MAX))
            .app_both_with(stepped(Fraction::int(2)), |a, _| a.clone());
        assert_eq!(
            refusal(&pattern),
            Some(QueryLimit::NativeFraction {
                operation: "appBoth"
            })
        );
    }

    #[test]
    fn poly_join_refuses_unrepresentable_step_ratio() {
        let pattern = stepped(Fraction::int(i128::MAX))
            .fmap_to_pattern(|_| stepped(Fraction::new(1, 2)))
            .poly_join();
        assert_eq!(
            refusal(&pattern),
            Some(QueryLimit::NativeFraction {
                operation: "polyJoin"
            })
        );
    }

    #[test]
    fn small_step_combinations_keep_their_steps() {
        let pair = || vec![stepped(Fraction::int(3)), stepped(Fraction::int(2))];
        assert_eq!(stack(pair()).steps(), Some(Fraction::int(6)));
        assert_eq!(slowcat(pair()).steps(), Some(Fraction::int(6)));
        assert_eq!(fastcat(pair()).steps(), Some(Fraction::int(2)));
        assert_eq!(
            stepped(Fraction::int(3))
                .app_both_with(stepped(Fraction::int(2)), |a, _| a.clone())
                .steps(),
            Some(Fraction::int(6))
        );
    }
}

#[cfg(test)]
mod fastgap_overflow_tests {
    use super::super::*;

    /// `fastGap` divides a span by its factor, so a small factor far from
    /// cycle zero can leave the i128 fraction range. No factor and no span
    /// may panic: the query returns its haps or a refusal.
    #[test]
    fn a_hostile_gap_is_refused_or_returned_but_never_panics() {
        for (numer, denom) in [(1, 1), (1, 1_000_000), (1, i128::MAX / 4), (3, 7)] {
            let factor = Fraction::new(numer, denom);
            let pattern = pure(Value::Str("x".into())).fast_gap(factor);
            for from in [0i128, 1, 1_000_000, i128::MAX / 8] {
                let begin = Fraction::int(from);
                let end = begin.checked_add(Fraction::ONE).unwrap_or(begin);
                // The assertion is that this returns at all.
                let _ = pattern.try_query_arc_sorted(begin, end);
            }
        }
    }
}

#[cfg(test)]
mod focus_overflow_tests {
    use super::super::*;

    /// The two spellings a hostile score can put in `focus("…", "…")`:
    /// `arg_fraction` converts them (each is an in-range `i128` rational),
    /// and their difference is `2^128 − 2`, which no native fraction holds.
    /// The unchecked `e − b` used to panic at score-evaluation time, where
    /// nothing on the producer path catches it.
    #[test]
    fn extreme_bounds_refuse_the_window_subtraction() {
        let b = "-170141183460469231731687303715884105727"
            .parse::<Fraction>()
            .expect("an in-range literal converts");
        let e = "170141183460469231731687303715884105727"
            .parse::<Fraction>()
            .expect("an in-range literal converts");
        let focused = pure(Value::Str("bd".into())).focus(b, e);
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "focus across the whole i128 range must refuse, not panic"
        );
    }

    /// A span of exactly `i128::MIN` is representable, but its reciprocal is
    /// `−1/2^127`, whose denominator no `i128/i128` fraction holds: `div` is
    /// a component swap for a numerator of one, and the swapped denominator
    /// is the one magnitude past `i128::MAX`. The unchecked reciprocal
    /// panicked there.
    #[test]
    fn a_min_width_window_refuses_the_reciprocal() {
        let focused =
            pure(Value::Str("bd".into())).focus(Fraction::int(i128::MAX), Fraction::int(-1));
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "the reciprocal of an i128::MIN-wide window must refuse, not panic"
        );
    }

    /// `b` at `i128::MIN` shifts by a whole cycle of `i128::MIN`. `early(t)`
    /// is `late(t.neg())`, and `i128::MIN` has no `i128` negation. The shift
    /// must not panic.
    #[test]
    fn a_min_whole_cycle_shift_refuses_the_negation() {
        let focused =
            pure(Value::Str("bd".into())).focus(Fraction::int(i128::MIN), Fraction::int(-1));
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "shifting by a whole i128::MIN cycle must refuse, not panic"
        );
    }

    /// The ordinary windows the reference documents keep their events and
    /// their refusal-free queries.
    #[test]
    fn ordinary_windows_still_focus_their_events() {
        let bd = pure(Value::Str("bd".into()));
        let halved = bd
            .focus(Fraction::ZERO, Fraction::new(1, 2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(0, 1/2) is ordinary");
        assert_eq!(halved.len(), 2, "focus(0, 1/2) is fast(2)");
        let focused = bd
            .focus(Fraction::new(1, 4), Fraction::new(3, 4))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(1/4, 3/4) is ordinary");
        // The window is half a cycle wide, so its content repeats twice per
        // cycle, and the copy straddling the query start from the previous
        // cycle covers `[0, 1/4)`: bd sounds across the whole cycle, thrice.
        assert_eq!(focused.len(), 3, "the middle half loops to fill the cycle");
        for hap in halved.iter().chain(focused.iter()) {
            assert_eq!(hap.value, Value::Str("bd".into()));
        }
    }
}

#[cfg(test)]
mod time_mapping_overflow_tests {
    use super::super::*;

    /// One large factor or shift is legal, but a chain of two leaves the
    /// i128 range at query time. Each chain must return the typed
    /// `NativeFraction` refusal instead of a panic.
    #[test]
    fn chained_time_factors_refuse_instead_of_panicking() {
        let bd = || pure(Value::Str("bd".into()));
        let factor = Fraction::from_f64(1e30).expect("1e30 is inside the conversion bound");
        let shift = Fraction::from_f64(1.7e38).expect("1.7e38 is inside the conversion bound");
        let twice_hurried = {
            let once = combinators::hurry(&pure(Value::Str("c".into())), 1e30);
            combinators::hurry(&once, 1e30)
        };
        for (built, operation) in [
            (bd().fast(factor).fast(factor), "fast"),
            // `hurry` is `fast` plus a speed control; the temporal half is
            // what overflows, so the refusal names it.
            (twice_hurried, "fast"),
            (bd().late(shift).late(shift), "late"),
            (bd().early(shift).early(shift), "early"),
            // `slow` is `fast` by the reciprocal; the refusal names the
            // operation the score wrote.
            (
                bd().slow(Fraction::ONE.div(factor))
                    .slow(Fraction::ONE.div(factor)),
                "slow",
            ),
        ] {
            assert!(
                matches!(
                    built.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::NativeFraction {
                        operation: got
                    }) if got == operation
                ),
                "the chained {operation} must refuse through the typed channel"
            );
        }
    }

    /// A zoom slot built from exact string literals at ±(2^127 - 1) has a
    /// width (`2^128 - 2`) no i128 fraction can hold. The `e.sub(s)` at
    /// construction used to panic on it; the slot must instead build and
    /// refuse when queried.
    #[test]
    fn an_extreme_zoom_slot_refuses_instead_of_panicking() {
        let begin: Fraction = "-170141183460469231731687303715884105727"
            .parse()
            .expect("-i128::MAX parses exactly");
        let end: Fraction = "170141183460469231731687303715884105727"
            .parse()
            .expect("i128::MAX parses exactly");
        let zoomed = pure(Value::Str("bd".into())).zoom(begin, end);
        assert!(
            matches!(
                zoomed.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                Err(QueryLimit::NativeFraction { operation: "zoom" })
            ),
            "the unrepresentable zoom slot must refuse through the typed channel"
        );
    }

    /// Single arguments at the i128 edge, which a score reaches from a plain
    /// number (`Fraction::from_f64(-2^127)` is exactly `i128::MIN`), must
    /// build without a panic and refuse when queried.
    #[test]
    fn edge_arguments_refuse_at_query_time_instead_of_panicking() {
        let bd = || pure(Value::Str("bd".into()));
        let min = Fraction::from_f64(-(2f64.powi(127))).expect("-2^127 converts exactly");
        assert_eq!(min, Fraction::int(i128::MIN));
        let max = Fraction::int(i128::MAX);
        for (built, operation) in [
            (bd().early(min), "early"),
            (bd().slow(min), "slow"),
            (bd().focus(min, max), "focus"),
        ] {
            assert!(
                matches!(
                    built.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::NativeFraction {
                        operation: got
                    }) if got == operation
                ),
                "{operation} at the i128 edge must refuse through the typed channel"
            );
        }
        // Ordinary arguments on the same paths are untouched.
        let slowed = bd()
            .slow(Fraction::int(2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(2))
            .expect("slow(2) is legal");
        assert_eq!(slowed.len(), 1, "slow(2) lost or duplicated events");
        let focused = bd()
            .focus(Fraction::ZERO, Fraction::new(1, 2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(0, 1/2) is legal");
        assert_eq!(focused.len(), 2, "focus(0, 1/2) lost or duplicated events");
    }

    /// The single `fast(1e30)` keeps its existing behavior: the span it
    /// requests is refused by the cycle-count guard (`MAX_QUERY_SPAN_CYCLES`),
    /// not by fraction overflow - the mapping itself is representable.
    #[test]
    fn a_single_huge_fast_keeps_its_cycle_span_refusal() {
        let factor = Fraction::from_f64(1e30).expect("1e30 is inside the conversion bound");
        let once = pure(Value::Str("bd".into())).fast(factor);
        assert!(
            matches!(
                once.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                Err(QueryLimit::QuerySpan { .. })
            ),
            "one legal factor must stay the ordinary span refusal, not a new one"
        );
        // An ordinary factor on the same paths is untouched.
        let doubled = pure(Value::Str("bd".into())).fast(Fraction::int(2));
        let haps = doubled
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("fast(2) is legal");
        assert_eq!(haps.len(), 2, "fast(2) lost or duplicated events");
    }
}

#[cfg(test)]
mod query_in_progress_tests {
    //! `query_in_progress` distinguishes ordinary score construction from callbacks
    //! reached by a query or a construction-time probe.

    use super::super::*;
    use std::sync::{Arc, Mutex};

    /// Ordinary construction is outside a query. The `stepJoin` cycle-0 probe
    /// counts as a query because it calls the pattern's function. Its boundary
    /// remains active after the carrier's `Pattern::query` call returns.
    #[test]
    fn a_construction_probe_and_a_query_are_in_progress_and_construction_is_not() {
        assert!(!query_in_progress(), "nothing is being queried");
        let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
        let record = seen.clone();
        let carrier = pure(Value::Str("bd".into())).fmap_to_pattern(move |value| {
            record.lock().expect("record").push(query_in_progress());
            pure(value.clone())
        });

        let joined = carrier.try_step_join().expect("stepJoin");
        assert!(!query_in_progress(), "the probe's boundary was left open");
        assert_eq!(
            *seen.lock().expect("seen"),
            [true],
            "the construction-time probe must count as a query"
        );

        let haps = joined.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert!(!haps.is_empty(), "the joined pattern plays");
        assert!(!query_in_progress(), "the query's boundary was left open");
        let seen = seen.lock().expect("seen");
        assert!(
            seen.len() > 1 && seen.iter().all(|inside| *inside),
            "a query's callbacks run inside it: {seen:?}"
        );
    }
}

#[cfg(test)]
mod slowcat_overflow_tests {
    //! `slowcat` answers every query whose shifted times fit the native fraction
    //! range and refuses the rest as `NativeFraction { operation: "slowcat" }`.

    use super::super::*;

    fn bd_sd() -> Pattern {
        slowcat(vec![
            pure(Value::Str("bd".into())),
            pure(Value::Str("sd".into())),
        ])
    }

    #[test]
    fn a_large_time_denominator_still_gets_its_exact_answer() {
        let denominator = 100_000_000_000_000_000_000_000_000_000_000_000_000i128;
        let begin = Fraction::new(1, denominator);
        let end = Fraction::new(2, denominator);
        let haps = bd_sd()
            .try_query_arc_sorted(begin, end)
            .expect("the exact answer fits");
        assert_eq!(haps.len(), 1);
        assert_eq!(haps[0].value, Value::Str("bd".into()));
        assert_eq!(haps[0].part, TimeSpan::new(begin, end));
        assert_eq!(
            haps[0].whole,
            Some(TimeSpan::new(Fraction::ZERO, Fraction::ONE))
        );
    }

    #[test]
    fn a_hap_shifted_past_the_native_range_refuses() {
        let pat = slowcat(vec![
            pure(Value::Str("bd".into())).slow(Fraction::int(i128::MAX)),
            pure(Value::Str("sd".into())),
        ]);
        assert_eq!(
            pat.try_query_arc_sorted(Fraction::int(2), Fraction::int(3))
                .err(),
            Some(QueryLimit::NativeFraction {
                operation: "slowcat"
            })
        );
    }

    #[test]
    fn ordinary_slowcat_queries_keep_their_cycle_order() {
        let values: Vec<_> = bd_sd()
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(4))
            .expect("ordinary slowcat query")
            .into_iter()
            .map(|hap| hap.value)
            .collect();
        assert_eq!(
            values,
            ["bd", "sd", "bd", "sd"].map(|value| Value::Str(value.into()))
        );
    }
}

#[cfg(test)]
mod pattern_of_join_resolution_tests {
    use super::super::*;

    /// `seqPLoop`'s exact graph shape:
    /// `stack(pure(p).compress(a, b), ...).slow(total).innerJoin()`.
    /// The join must resolve through the stack and fastGap wrappers.
    #[test]
    fn a_join_over_stacked_compressed_patterns_resolves_each_window() {
        let bd = pure(Value::Str("bd".into()));
        let cp = pure(Value::Str("cp".into()));
        let sections = stack(vec![
            pure_pattern(bd).compress(Fraction::ZERO, Fraction::new(2, 3)),
            pure_pattern(cp).compress(Fraction::new(1, 3), Fraction::ONE),
        ]);
        let pattern = sections.slow(Fraction::int(3)).inner_join();
        let haps = pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(4))
            .expect("the seqPLoop shape must not be refused");
        assert!(!haps.is_empty(), "seqPLoop windows must produce haps");
        for hap in &haps {
            assert!(
                matches!(hap.value, Value::Str(_)),
                "a pattern-valued hap leaked through the join: {hap:?}"
            );
        }
        // bd owns [0, 2) and [3, 4) of the four queried cycles; cp owns
        // [1, 3). Both branches must be audible.
        let has_bd = haps.iter().any(|hap| hap.value == Value::Str("bd".into()));
        let has_cp = haps.iter().any(|hap| hap.value == Value::Str("cp".into()));
        assert!(has_bd, "the first seqPLoop section is missing: {haps:?}");
        assert!(has_cp, "the second seqPLoop section is missing: {haps:?}");
    }
}

#[cfg(test)]
mod callback_query_metrics_tests {
    use super::super::*;

    struct NestedHost;

    impl CallbackHost for NestedHost {
        fn call_value(&self, id: CallbackId, value: &Value) -> Result<Value, String> {
            if id == 0 {
                let _ = host_call_value(1, value);
            } else {
                // The innermost call takes a time that every clock reads.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(value.clone())
        }

        fn call_query(&self, _id: CallbackId, _state: &State) -> Result<Vec<Hap>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn callback_metrics_count_nested_crossings_without_losing_scope() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, true, || {
                let value = Value::F64(1.0);
                assert_eq!(host_call_value(0, &value), value);
            });
        });
        assert_eq!(metrics.calls(), 2);
        let kinds = metrics.kind_sample().expect("forced callback-kind sample");
        assert_eq!(kinds.total(), metrics.calls());
        assert_eq!(kinds.get(CallbackQueryKind::Value), 2);
        for kind in CallbackQueryKind::ALL {
            if kind != CallbackQueryKind::Value {
                assert_eq!(kinds.get(kind), 0, "unexpected {kind:?}");
            }
        }
        assert!(metrics.busy_nanos() > 0);
        CALLBACK_QUERY_DEPTH.with(|depth| assert_eq!(depth.get(), 0));
        CALLBACK_QUERY_METRICS.with(|slot| assert!(slot.get().is_none()));
    }

    #[test]
    fn nested_metric_recorders_restore_the_outer_recorder() {
        let host = NestedHost;
        let mut outer = CallbackQueryMetrics::default();
        let mut inner = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut outer, true, || {
                let value = Value::F64(1.0);
                let _ = host_call_value(1, &value);
                with_callback_query_metrics_policy(&mut inner, true, || {
                    let _ = host_call_value(1, &value);
                });
                let _ = host_call_value(1, &value);
            });
        });
        assert_eq!(outer.calls(), 2);
        assert_eq!(inner.calls(), 1);
        assert_eq!(
            outer
                .kind_sample()
                .expect("outer kind sample")
                .get(CallbackQueryKind::Value),
            2
        );
        assert_eq!(
            inner
                .kind_sample()
                .expect("inner kind sample")
                .get(CallbackQueryKind::Value),
            1
        );
    }

    #[test]
    fn an_unsampled_kind_census_keeps_the_exact_aggregate() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, false, || {
                let _ = host_call_value(1, &Value::F64(1.0));
            });
        });
        assert_eq!(metrics.calls(), 1);
        assert!(metrics.kind_sample().is_none());
    }

    #[test]
    fn each_host_call_reads_the_installed_host_and_counts_its_kind() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        let value = Value::F64(1.0);
        let haps = pure(value.clone()).query_arc(Fraction::ZERO, Fraction::ONE);
        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let program = callback_ir::PatternTransformProgram::new([]).unwrap();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, true, || {
                let _ = host_call_value(1, &value);
                let _ = host_call_value_predicate(1, &value);
                let _ = host_call_ref(1);
                let _ = host_call_pick_lookup(1);
                let _ = host_call_pattern(1, pure(value.clone()));
                let _ = host_call_pattern_ir(1, &program, pure(value.clone()));
                let _ = host_call_pattern_indexed_batch(1, vec![(pure(value.clone()), 0)]);
                let _ = host_call_haps(1, &haps);
                let _ = host_call_hap_predicate(1, &haps[0]);
                let _ = host_call_time_predicate(1, Fraction::ZERO);
                let _ = host_call_span_transform(1, span);
                let _ = host_call_bind(1, BindArg::Value(&value));
                let _ = host_call_query(1, &State::new(span));
            });
        });
        let kinds = metrics.kind_sample().expect("forced callback-kind sample");
        for kind in CallbackQueryKind::ALL {
            let expected = match kind {
                // The value call and the value predicate share this kind.
                CallbackQueryKind::Value => 2,
                CallbackQueryKind::MaterializeValue => 0,
                _ => 1,
            };
            assert_eq!(kinds.get(kind), expected, "{kind:?}");
        }
    }
}

#[cfg(test)]
mod set_lookup_tests {
    use super::super::*;

    fn object(entries: &[(&str, Value)]) -> Value {
        Value::object(
            entries
                .iter()
                .map(|(key, value)| ((*key).into(), value.clone())),
        )
    }

    fn entries(lookup: &PickLookup) -> Vec<(String, Value)> {
        let PickLookup::Object { entries, .. } = lookup else {
            panic!("a set lookup is an object");
        };
        entries
            .iter()
            .map(|(key, pattern)| (key.to_string(), pattern.as_pure().expect("a pure entry")))
            .collect()
    }

    #[test]
    fn a_skipped_set_lookup_equals_the_lookup_of_the_merged_value() {
        let members = [
            Value::F64(0.0),
            Value::F64(-0.0),
            Value::F64(1.0),
            Value::Bool(true),
            Value::Str("x".into()),
            Value::list(vec![Value::F64(-0.0)]),
        ];
        let mut values = vec![Value::Undefined, Value::Null, Value::Str("ab".into())];
        values.extend(members.iter().cloned());
        for key in ["value", "0", "a"] {
            for member in &members {
                values.push(object(&[(key, member.clone())]));
            }
        }
        for first in &members {
            for second in &members {
                values.push(object(&[("value", first.clone()), ("a", second.clone())]));
            }
        }

        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let mut skipped = 0;
        for left in &values {
            for right in &values {
                let left_hap = Hap::new(Some(span), span, left.clone());
                let right_hap = Hap::new(Some(span), span, right.clone());
                for merged in [
                    controls::set_value(left, right),
                    compose::compose_op(compose::ComposeOp::Set, left, right),
                ] {
                    if !set_lookup_follows_value(&merged, &left_hap, &right_hap) {
                        continue;
                    }
                    skipped += 1;
                    // The lookup the merge stored before, then the one a pick derives.
                    let Some(eager) = (merged != *right)
                        .then(|| PickLookup::set_object(&left_hap, &right_hap, &merged))
                        .flatten()
                    else {
                        continue;
                    };
                    let derived = PickLookup::from_value(&merged).expect("an object value");
                    assert_eq!(eager.enumerable_len(), derived.enumerable_len());
                    let (eager, derived) = (entries(&eager), entries(&derived));
                    assert_eq!(eager.len(), derived.len());
                    for ((eager_key, eager_value), (key, value)) in eager.iter().zip(&derived) {
                        assert!(
                            eager_key == key && same_key(eager_value, value),
                            "{left:?} set {right:?}: {eager_key} {eager_value:?}, {key} {value:?}"
                        );
                    }
                }
            }
        }
        assert!(skipped > 1_000, "only {skipped} merges skipped the lookup");
    }
}

#[cfg(test)]
mod stable_sort_tests {
    use super::super::*;

    fn hap(begin: i128, end: i128, label: &str) -> Hap {
        let span = TimeSpan::new(Fraction::new(begin, 4), Fraction::new(end, 4));
        Hap::new(Some(span), span, Value::Str(label.into()))
    }

    fn labels(haps: &[Hap]) -> Vec<String> {
        haps.iter().map(|hap| hap.value.show()).collect()
    }

    #[test]
    fn equal_haps_keep_their_order_and_every_hap_moves_once() {
        let haps = vec![
            hap(2, 3, "c"),
            hap(0, 1, "a1"),
            hap(1, 2, "b"),
            hap(0, 1, "a2"),
            hap(0, 2, "long"),
        ];
        let sorted = sort_haps_for_query(haps);
        assert_eq!(labels(&sorted), ["a1", "a2", "long", "b", "c"]);
        let already = sort_haps_for_query(sorted);
        assert_eq!(labels(&already), ["a1", "a2", "long", "b", "c"]);
    }

    #[test]
    fn many_haps_sort_as_their_times_order_them() {
        // Times that floats tell apart, times with the same float, and
        // times past what a float holds exactly.
        let far = 1_i128 << 60;
        let times = |index: i128| match index % 4 {
            0 => Fraction::new(index * 7 % 23, 5),
            1 => Fraction::new(far + index % 3, far),
            2 => Fraction::new(far * 3 + index % 5, 3),
            _ => Fraction::new(index % 9, 4),
        };
        // Two times that their floats put in the other order: the first
        // is the later one, and its float is the lower one.
        let later = Fraction::new(far + 128, far);
        let earlier = Fraction::new(far + 200, far + 100);
        assert!(later > earlier && later.to_f64() < earlier.to_f64());
        let haps: Vec<Hap> = (0..64)
            .map(times)
            .chain([later, earlier])
            .zip(0_i32..)
            .map(|(begin, index)| {
                let width = Fraction::new(i128::from(index % 3 + 1), 7);
                let span = TimeSpan::new(begin, begin + width);
                Hap::new(Some(span), span, Value::F64(f64::from(index)))
            })
            .collect();
        let mut expected = haps.clone();
        expected.sort_by(query_order);
        assert_eq!(labels(&sort_haps_for_query(haps)), labels(&expected));
    }
}

#[cfg(test)]
mod constant_values_tests {
    use super::super::*;

    fn half(n: i128) -> Fraction {
        Fraction::new(n, 2)
    }

    fn span(begin: Fraction, end: Fraction) -> TimeSpan {
        TimeSpan::new(begin, end)
    }

    fn summed(functions: Pattern) -> Pattern {
        let add = |a: &Value, b: &Value| match (a, b) {
            (Value::F64(a), Value::F64(b)) => Value::F64(a + b),
            _ => Value::Undefined,
        };
        functions.app_left_by(
            pure(Value::F64(10.0)),
            Combine::with(Arc::new(add)),
            LookupFlow::Infer,
        )
    }

    fn meetings(
        pattern: &Pattern,
        begin: Fraction,
        end: Fraction,
    ) -> Vec<(TimeSpan, TimeSpan, f64)> {
        pattern
            .query_arc(begin, end)
            .into_iter()
            .map(|hap| {
                (
                    hap.whole.expect("a discrete hap"),
                    hap.part,
                    hap.value.as_f64().expect("a number"),
                )
            })
            .collect()
    }

    #[test]
    fn a_constant_value_side_meets_each_function_hap_once_per_cycle() {
        let inside = summed(pure(Value::F64(1.0)));
        assert_eq!(
            meetings(&inside, Fraction::ZERO, half(1)),
            [(
                span(Fraction::ZERO, Fraction::ONE),
                span(Fraction::ZERO, half(1)),
                11.0
            )]
        );
        // A function hap across cycle 1 meets the value hap of each cycle it
        // covers, and keeps its own whole.
        let across = summed(pure(Value::F64(1.0)).late(half(1)));
        assert_eq!(
            meetings(&across, half(1), half(3)),
            [
                (span(half(1), half(3)), span(half(1), Fraction::ONE), 11.0),
                (span(half(1), half(3)), span(Fraction::ONE, half(3)), 11.0),
            ]
        );
    }
}

#[cfg(test)]
mod constant_run_tests {
    use super::super::*;
    use crate::compose::{Alignment, ComposeOp, compose};

    fn third(n: i128) -> Fraction {
        Fraction::new(n, 3)
    }

    pub(super) fn control(key: &str, value: Value, location: Option<(usize, usize)>) -> Pattern {
        let controls = pure(Value::object([(key.to_string(), value)]));
        match location {
            Some(location) => controls.with_added_context(vec![location]),
            None => controls,
        }
    }

    /// `base` with each of `values` applied to it in turn.
    pub(super) fn chain(base: &Pattern, values: &[(ComposeOp, Pattern)]) -> Pattern {
        values.iter().fold(base.clone(), |pattern, (op, values)| {
            compose(&pattern, values, *op, Alignment::In)
        })
    }

    /// The same chain, with each value behind a stack of one. A stack
    /// answers as its pattern does and is not read as constant, so each
    /// application meets its value through a query, one node at a time.
    pub(super) fn chain_of_queries(base: &Pattern, values: &[(ComposeOp, Pattern)]) -> Pattern {
        let queried: Vec<(ComposeOp, Pattern)> = values
            .iter()
            .map(|(op, values)| (*op, stack(vec![values.clone()])))
            .collect();
        chain(base, &queried)
    }

    /// Everything a meeting decides of each hap `pattern` answers.
    pub(super) fn answers(pattern: &Pattern, begin: Fraction, end: Fraction) -> Vec<String> {
        pattern
            .query_arc(begin, end)
            .iter()
            .map(|hap| {
                format!(
                    "{hap:?} {:?} {} {} {} {}",
                    hap.live_controls(),
                    hap.slider_binding(),
                    hap.ui_visuals_context(),
                    hap.keys.is_empty(),
                    hap.pick_lookup().is_some()
                )
            })
            .collect()
    }

    pub(super) fn bases() -> Vec<(&'static str, Pattern)> {
        let note = |n: f64| pure(Value::object([("note".to_string(), Value::F64(n))]));
        vec![
            ("two notes a cycle", fastcat(vec![note(0.0), note(1.0)])),
            (
                "a note of three cycles",
                note(2.0).slow(Fraction::new(3, 1)),
            ),
            (
                "a note with a key of the chain",
                compose(
                    &note(3.0),
                    &control("gain", Value::F64(0.1), Some((1, 2))),
                    ComposeOp::Set,
                    Alignment::In,
                ),
            ),
            ("a number", pure(Value::F64(5.0))),
            ("a string", pure(Value::Str("bd".into()))),
            ("a signal", crate::signal::saw()),
            ("a tagged note", note(4.0).with_ui_visual_slot(3)),
        ]
    }

    fn spans() -> [(Fraction, Fraction); 4] {
        [
            (Fraction::ZERO, Fraction::ONE),
            (Fraction::ZERO, Fraction::new(4, 1)),
            (third(1), third(5)),
            (third(7), third(8)),
        ]
    }

    /// The constants that a pattern meets are one node, as the two numbers
    /// of a `range` are, and a control after them. Two runs of the same
    /// constants at the same places are the same construction. A constant
    /// with a place of its own in the text keeps the node of its
    /// application.
    #[test]
    fn constants_that_a_pattern_meets_are_one_node() {
        let number = |n: f64| pure(Value::F64(n));
        let ranged = |to: f64| {
            let numbers = [
                (ComposeOp::Mul, number(to - 200.0)),
                (ComposeOp::Add, number(200.0)),
            ];
            chain(&crate::signal::saw(), &numbers)
        };
        let pattern = ranged(800.0);
        let Node::Constants(node) = pattern.node_kind() else {
            panic!("a range of a signal is no run of numbers");
        };
        let (base, run) = (&node.0, &node.1);
        assert!(matches!(base.node_kind(), Node::Signal(_)));
        let numbers: Vec<(ComposeOp, f64)> = run.iter().filter_map(RunMeeting::number).collect();
        assert_eq!(numbers, [(ComposeOp::Mul, 600.0), (ComposeOp::Add, 200.0)]);
        assert!(pattern.same_construction(&ranged(800.0)));
        assert!(!pattern.same_construction(&ranged(801.0)));
        // A `range` makes the run with no pattern of a number, and answers
        // as its two compositions do. A constant folds each number in.
        let range = |pattern: &Pattern| crate::combinators::range(pattern, 200.0, 800.0);
        let direct = range(&crate::signal::saw());
        assert!(pattern.same_construction(&direct));
        let composed = sequence::unfolded(|| range(&crate::signal::saw()));
        assert!(matches!(composed.node_kind(), Node::AppLeft(..)));
        for (begin, end) in spans() {
            assert_eq!(answers(&direct, begin, end), answers(&composed, begin, end));
        }
        let constant = range(&number(0.5));
        assert!(!matches!(constant.node_kind(), Node::Constants(..)));
        assert_eq!(
            answers(&constant, Fraction::ZERO, Fraction::ONE),
            answers(&number(500.0), Fraction::ZERO, Fraction::ONE)
        );

        let located = number(2.0).with_pure_loc((1, 2));
        let applied = compose(
            &crate::signal::saw(),
            &located,
            ComposeOp::Mul,
            Alignment::In,
        );
        assert!(matches!(applied.node_kind(), Node::AppLeft(..)));

        let quiet = |at: (usize, usize)| {
            let gain = control("gain", Value::F64(0.5), Some(at));
            chain(&ranged(800.0), &[(ComposeOp::Set, gain)])
        };
        let set = quiet((3, 6));
        let Node::Constants(node) = set.node_kind() else {
            panic!("a control after a range is no run of constants");
        };
        let (base, run) = (&node.0, &node.1);
        assert!(matches!(base.node_kind(), Node::Signal(_)));
        assert_eq!(run.len(), 3);
        assert!(set.same_construction(&quiet((3, 6))));
        assert!(!set.same_construction(&quiet((4, 7))));
    }

    #[test]
    fn a_chain_of_constants_answers_as_each_application_in_turn() {
        let chains = [
            (
                "controls",
                vec![
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("saw".into()), Some((3, 6))),
                    ),
                    (ComposeOp::Set, control("gain", Value::F64(0.8), None)),
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("sine".into()), Some((10, 14))),
                    ),
                    (
                        ComposeOp::Set,
                        control("cutoff", Value::F64(800.0), Some((20, 23))),
                    ),
                ],
            ),
            (
                "arithmetic",
                vec![
                    (ComposeOp::Mul, pure(Value::F64(600.0))),
                    (ComposeOp::Add, pure(Value::F64(200.0))),
                    (ComposeOp::Sub, pure(Value::F64(1.5))),
                ],
            ),
            (
                "sets on each side of an addition",
                vec![
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("saw".into()), Some((3, 6))),
                    ),
                    (
                        ComposeOp::Set,
                        control("gain", Value::F64(0.5), Some((8, 11))),
                    ),
                    (ComposeOp::Add, control("gain", Value::F64(0.25), None)),
                    (
                        ComposeOp::Set,
                        control("pan", Value::F64(0.5), Some((20, 23))),
                    ),
                    (
                        ComposeOp::Set,
                        control("s", Value::Str("sine".into()), None),
                    ),
                    (
                        ComposeOp::Set,
                        control("room", Value::F64(0.3), Some((30, 33))),
                    ),
                ],
            ),
            (
                "a number among controls",
                vec![
                    (ComposeOp::Set, control("s", Value::Str("saw".into()), None)),
                    // A bare number sets nothing on a control object.
                    (ComposeOp::Set, pure(Value::F64(3.0))),
                    (
                        ComposeOp::Add,
                        control("gain", Value::F64(0.8), Some((4, 7))),
                    ),
                    (ComposeOp::Set, control("pan", Value::F64(0.5), None)),
                ],
            ),
        ];
        for (chain_name, values) in &chains {
            for (base_name, base) in bases() {
                let (run, in_turn) = (chain(&base, values), chain_of_queries(&base, values));
                for (begin, end) in spans() {
                    assert_eq!(
                        answers(&run, begin, end),
                        answers(&in_turn, begin, end),
                        "{chain_name} on {base_name} over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    /// A function call gives a handle that binds the runtime of the call,
    /// and each pattern made of it binds that runtime too.
    #[test]
    fn a_chain_reads_through_the_handles_of_the_runtime_in_force_alone() {
        use crate::rng::{self, RngMode};
        use crate::settings::RuntimeSettings;
        let runtime = |mode: RngMode| {
            let settings = RuntimeSettings::default();
            settings.with(|| rng::use_rng(mode));
            settings
        };
        let (one, other) = (runtime(RngMode::Precise), runtime(RngMode::Legacy));
        // A base that tells the runtime in force when a query asks it.
        let told = || {
            fastcat(vec![pure(Value::F64(0.0)), pure(Value::F64(1.0))]).fmap(|_| {
                let mode = Value::Str(format!("{:?}", rng::rng_mode()).into());
                Value::object([("mode".to_string(), mode)])
            })
        };
        let values = [
            (ComposeOp::Set, control("s", Value::Str("saw".into()), None)),
            (
                ComposeOp::Set,
                control("gain", Value::F64(0.8), Some((4, 7))),
            ),
            (ComposeOp::Set, control("pan", Value::F64(0.5), None)),
            (
                ComposeOp::Set,
                control("room", Value::F64(0.3), Some((9, 12))),
            ),
        ];
        type Chain = fn(&Pattern, &[(ComposeOp, Pattern)]) -> Pattern;
        // Each handle of the chain binds the runtime of the base.
        let of_one = |chain: Chain| chain(&told().with_runtime_settings(one.clone()), &values);
        // A handle in the chain binds another runtime than the outermost.
        let of_two = |chain: Chain| {
            let inner = chain(&told(), &values[..2]).with_runtime_settings(other.clone());
            let mut outer = chain(&inner, &values[2..]);
            outer.force_runtime_settings(one.clone());
            outer
        };
        for (name, mode, run, in_turn) in [
            (
                "one runtime",
                "Precise",
                of_one(chain),
                of_one(chain_of_queries),
            ),
            (
                "two runtimes",
                "Legacy",
                of_two(chain),
                of_two(chain_of_queries),
            ),
        ] {
            for (begin, end) in spans() {
                let answered = answers(&run, begin, end);
                assert!(answered.iter().all(|hap| hap.contains(mode)), "{name}");
                assert_eq!(answered, answers(&in_turn, begin, end), "{name}");
            }
        }
    }

    #[test]
    fn a_chain_under_a_queried_value_still_answers_as_one() {
        // The outer application queries its value, so only the two inner
        // applications make a run, under a node that is none.
        let base = fastcat(vec![pure(Value::F64(1.0)), pure(Value::F64(2.0))]);
        let inner = [
            (ComposeOp::Mul, pure(Value::F64(3.0))),
            (ComposeOp::Add, pure(Value::F64(4.0))),
        ];
        let patterned = fastcat(vec![pure(Value::F64(10.0)), pure(Value::F64(20.0))]);
        let outer = |inner: Pattern| compose(&inner, &patterned, ComposeOp::Add, Alignment::In);
        let (run, in_turn) = (
            outer(chain(&base, &inner)),
            outer(chain_of_queries(&base, &inner)),
        );
        for (begin, end) in spans() {
            assert_eq!(answers(&run, begin, end), answers(&in_turn, begin, end));
        }
    }
}

#[cfg(test)]
mod value_signal_tests {
    use super::super::*;
    use super::constant_run_tests::{answers, bases, chain, chain_of_queries, control};
    use crate::combinators::range;
    use crate::compose::{Alignment, ComposeOp, compose};
    use crate::signal::{saw, sine};

    fn whole(n: i128) -> Fraction {
        Fraction::new(n, 1)
    }

    fn third(n: i128) -> Fraction {
        Fraction::new(n, 3)
    }

    /// The `segment` of a signal answers as the graph of the segment does,
    /// read where it is or under steps of its own, for spans that cut its
    /// pulses and for a rate that is not whole.
    #[test]
    fn a_segmented_signal_walk_answers_as_its_graph() {
        for (name, signal) in signals() {
            for rate in [whole(8), third(8)] {
                let read = combinators::segment(&signal, rate).mark_walked();
                let graph = combinators::segment(&signal.mark_volatile(), rate);
                for (begin, end) in spans() {
                    assert_eq!(
                        answers(&read, begin, end),
                        answers(&graph, begin, end),
                        "{name} at rate {rate} over {begin}..{end}"
                    );
                }
            }
        }
        // A pulse of more cycles than a query may ask refuses as the graph
        // refuses, also under a constant that meets each of its cycles.
        let ranged = range(&sine(), 200.0, 2000.0);
        let slow = Fraction::new(1, MAX_QUERY_SPAN_CYCLES + 5);
        let read = combinators::segment(&ranged, slow).mark_walked();
        let graph = combinators::segment(&ranged.mark_volatile(), slow);
        let span = (whole(0), whole(1));
        assert_eq!(
            format!("{:?}", read.try_query_arc_sorted(span.0, span.1)),
            format!("{:?}", graph.try_query_arc_sorted(span.0, span.1)),
        );
    }

    /// The one walk of the steps of a `segment` answers what the walk of
    /// its graph answers, for each wave under each map, counts that are
    /// powers of two and counts that are not, and spans that are negative,
    /// of no width, between two steps, and in one step or many.
    #[test]
    fn a_fused_segment_walk_answers_as_the_graph_walk() {
        use crate::combinators::rangex;
        type Map = Box<dyn Fn(&Pattern) -> Pattern>;
        let maps: Vec<(&str, Map)> = vec![
            ("bare", Box::new(|signal| signal.clone())),
            ("range", Box::new(|signal| range(signal, 200.0, 2000.0))),
            ("rangex", Box::new(|signal| rangex(signal, 200.0, 2000.0))),
            ("arithmetic", Box::new(arithmetic)),
            ("fast", Box::new(|signal| signal.fast(whole(3)))),
            (
                "slow",
                Box::new(|signal| range(signal, 0.0, 4.0).slow(third(5))),
            ),
            ("late", Box::new(|signal| signal.late(third(1)))),
        ];
        let spans: Vec<(Fraction, Fraction)> = {
            let sixteenth = |n: i128| Fraction::new(n, 16);
            vec![
                (whole(0), whole(1)),
                (whole(0), whole(3)),
                (whole(-2), whole(1)),
                (third(-4), third(-1)),
                (third(-5), third(5)),
                (third(1), third(5)),
                (third(7), third(8)),
                (third(2), third(2)),
                (whole(0), whole(0)),
                (whole(-1), whole(-1)),
                (sixteenth(3), sixteenth(3)),
                (sixteenth(2), sixteenth(2)),
                (sixteenth(5), sixteenth(6)),
                (sixteenth(1), sixteenth(9)),
                (Fraction::new(1, 1000), Fraction::new(3, 1000)),
                (whole(7), Fraction::new(15, 2)),
            ]
        };
        use std::ops::AddAssign;
        let mut ran = std::collections::BTreeMap::new();
        for (wave, signal) in crate::signal::signals() {
            for (map, apply) in &maps {
                let signal = apply(&signal);
                for count in [1, 2, 3, 5, 8, 12, 48, 64, 100] {
                    let rate = whole(count);
                    let read = combinators::segment(&signal, rate).mark_walked();
                    let graph = combinators::segment(&signal.mark_volatile(), rate);
                    for (begin, end) in &spans {
                        let (walked, none) =
                            fused::counting(|| fused::unfused(|| answers(&graph, *begin, *end)));
                        let (fused, hits) = fused::counting(|| answers(&read, *begin, *end));
                        let case = format!("{wave} {map} in {count} steps over {begin}..{end}");
                        assert_eq!(fused, walked, "{case}");
                        assert_eq!(none, fused::Hits::default(), "{case}: the graph ran fused");
                        ran.entry(wave).or_insert(0).add_assign(hits.segment);
                    }
                }
            }
        }
        // The triangle waves are built from a pattern of two ramps, and no
        // signal: the graph answers them.
        let never: Vec<_> = ran
            .iter()
            .filter(|(wave, hits)| **hits == 0 && !wave.contains("tri"))
            .collect();
        assert!(
            never.is_empty(),
            "the walk of the steps never ran for {never:?}"
        );
    }

    /// A segment pattern, made again for each case. A control of a segment
    /// that is to be read as a wave is made again until it is: the name of a
    /// control comes from a pool that another test can leave out of reach.
    fn built(make: &dyn Fn() -> Pattern, as_steps: bool) -> Pattern {
        let sets = Combine::Composed(ComposeOp::Set);
        for _ in 0..200 {
            let pattern = make();
            if !as_steps || fused::StepWave::of(&pattern, &sets, LookupFlow::Set).is_some() {
                return pattern;
            }
            std::thread::yield_now();
        }
        panic!("a control of a segment is not read as a control of a wave");
    }

    /// A segment on the value side of a meeting answers as a query of it
    /// for each hap of the other side does: the haps of the steps that the
    /// function hap meets, cut to it, under each control and map.
    #[test]
    fn a_segmented_value_side_answers_as_a_query_of_it_for_each_hap() {
        type Make = Box<dyn Fn() -> Pattern>;
        let rand = crate::signal::rand;
        let segments: Vec<(&str, bool, Make)> = vec![
            (
                "a segment",
                false,
                Box::new(|| combinators::segment(&sine(), whole(8))),
            ),
            (
                "a ranged segment",
                false,
                Box::new(|| range(&combinators::segment(&saw(), whole(5)), 200.0, 2000.0)),
            ),
            (
                "a segment of a ranged signal",
                false,
                Box::new(|| {
                    combinators::segment(&range(&sine(), 0.0, 4.0).slow(third(4)), whole(64))
                }),
            ),
            (
                "a segment of a random signal",
                false,
                Box::new(move || combinators::segment(&rand(), whole(8))),
            ),
            (
                "a slow segment",
                false,
                Box::new(|| combinators::segment(&saw(), whole(3)).slow(whole(2))),
            ),
            (
                "a named segment",
                true,
                Box::new(|| named("cutoff", combinators::segment(&sine(), whole(16)))),
            ),
            (
                "a named ranged segment",
                true,
                Box::new(move || {
                    named(
                        "gain",
                        range(&combinators::segment(&rand(), whole(8)), 0.2, 0.9),
                    )
                }),
            ),
        ];
        let mut total = fused::Hits::default();
        for op in [ComposeOp::Set, ComposeOp::Add, ComposeOp::Mul] {
            for (begin, end) in spans() {
                for ((structure_name, structure), (name, as_steps, make)) in structures()
                    .into_iter()
                    .flat_map(|structure| segments.iter().map(move |s| (structure.clone(), s)))
                {
                    let signal = built(make, *as_steps && op == ComposeOp::Set);
                    let read = compose(&structure, &signal, op, Alignment::In);
                    let queried =
                        compose(&structure, &stack(vec![signal.clone()]), op, Alignment::In);
                    let (graph, none) =
                        fused::counting(|| fused::unfused(|| answers(&queried, begin, end)));
                    let case =
                        format!("{op:?} of {name} on {structure_name} over {begin:?}..{end:?}");
                    assert_eq!(none, fused::Hits::default(), "{case}: the graph ran fused");
                    for read in [read.mark_walked(), read] {
                        let (fused, hits) = fused::counting(|| answers(&read, begin, end));
                        assert_eq!(fused, graph, "{case}");
                        total.segment += hits.segment;
                        total.step_one += hits.step_one;
                        total.step_many += hits.step_many;
                    }
                }
            }
        }
        // The walks ran: each kind of answer was made in some case.
        assert!(total.segment > 0, "{total:?}");
        assert!(total.step_one > 0, "{total:?}");
        assert!(total.step_many > 0, "{total:?}");
    }

    /// A segment charges the hap budget one hap at a time, as the graph
    /// does, also for a signal that makes several haps for a step.
    #[test]
    fn a_segment_refuses_the_hap_budget_as_the_graph_does() {
        let many = range(&sine(), 0.0, 1.0).fast(whole(256));
        let read = combinators::segment(&many, whole(64)).mark_walked();
        let graph = combinators::segment(&many.mark_volatile(), whole(64));
        let ask = |pattern: &Pattern, budget: u64| {
            format!(
                "{:?}",
                pattern.try_query_arc_sorted_with_budget(whole(0), whole(1), budget)
            )
        };
        for budget in [0, 1, 10, 63, 64, 100, 255, 256, 257, 10_000] {
            let (walked, none) = fused::counting(|| fused::unfused(|| ask(&graph, budget)));
            let (fused, hits) = fused::counting(|| ask(&read, budget));
            assert_eq!(fused, walked, "a budget of {budget}");
            assert_eq!(none, fused::Hits::default());
            // A budget under the steps is the graph's, and one over them is
            // the walk's.
            assert_eq!(hits.segment > 0, budget >= 64, "a budget of {budget}");
        }
    }

    /// The walk of the steps stops at the hap that passes the budget, and
    /// leaves no hap, as the graph path of the application does. The walk
    /// is asked as it is, with no query around it that would charge the
    /// haps it holds.
    #[test]
    fn a_segment_walk_charges_each_hap_of_a_step_to_the_budget() {
        let many = range(&sine(), 0.0, 1.0).fast(whole(256));
        let segment = combinators::segment(&many, whole(64));
        let Node::FilterValues(boxed) = segment.node_kind() else {
            panic!("a segment is a filter of an application");
        };
        let structured = &boxed.0;
        let Node::AppRight(node) = structured.node_kind() else {
            panic!("a segment is an application");
        };
        let state = State::new(TimeSpan::new(whole(0), whole(1)));
        for (budget, refused) in [(100, true), (255, true), (256, false), (1000, false)] {
            let mut out = Vec::new();
            let (_, refusal) =
                with_hap_budget(budget, || fused::app_right_onto(node, &state, &mut out));
            assert_eq!(refusal.is_some(), refused, "a budget of {budget}");
            assert_eq!(
                out.len(),
                if refused { 0 } else { 256 },
                "a budget of {budget}"
            );
        }
    }

    /// A segment stops at a requested stop as the graph does, for a span
    /// that holds no multiple of sixteen steps.
    #[test]
    fn a_segment_stops_as_the_graph_does() {
        let read = combinators::segment(&range(&sine(), 200.0, 2000.0), whole(16)).mark_walked();
        let graph = combinators::segment(&range(&sine(), 200.0, 2000.0).mark_volatile(), whole(16));
        let stopped = std::sync::atomic::AtomicBool::new(true);
        for (begin, end) in [
            (Fraction::new(1, 16), whole(1)),
            (Fraction::new(-15, 16), whole(0)),
        ] {
            let ask = |pattern: &Pattern| {
                with_cancellation(&stopped, || {
                    format!("{:?}", pattern.try_query_arc_sorted(begin, end))
                })
            };
            let walked = fused::unfused(|| ask(&graph));
            assert_eq!(ask(&read), walked, "over {begin}..{end}");
        }
    }

    fn signals() -> Vec<(&'static str, Pattern)> {
        let cutoff = |signal: Pattern| {
            signal.fmap(|value| Value::object([("cutoff".to_string(), value.clone())]))
        };
        vec![
            ("a signal", saw()),
            ("a ranged signal", range(&sine(), 200.0, 2000.0)),
            (
                "a slow ranged signal",
                range(&sine(), 200.0, 2000.0).slow(whole(4)),
            ),
            ("a fast late signal", saw().fast(whole(3)).late(third(1))),
            (
                "a control of a slow signal",
                cutoff(range(&saw(), 0.0, 1.0).slow(whole(2))),
            ),
            (
                "a signal with a place in the text",
                cutoff(sine()).with_added_context(vec![(4, 8)]),
            ),
            // A control by its name sets its value where the hap is.
            (
                "a named control",
                named("cutoff", range(&sine(), 200.0, 2000.0)),
            ),
            ("a named gain", named("gain", saw().slow(whole(3)))),
            (
                "a named control with a place in the text",
                named("pan", sine().with_added_context(vec![(4, 8)])),
            ),
            ("a named note", named("note", saw().fast(whole(2)))),
            // A control of several names reads a number as its first name.
            (
                "a named control of several names",
                crate::controls::ControlSpec::new(["cutoff", "resonance"].map(Arc::from))
                    .pattern(&range(&sine(), 200.0, 2000.0)),
            ),
            ("a control named value", named("value", saw())),
            // A signal of numbers gives each number with no hap of its own.
            (
                "a named ranged signal, slowed",
                named("cutoff", range(&sine(), 200.0, 2000.0).slow(whole(4))),
            ),
            (
                "a named slow signal, ranged",
                named("cutoff", range(&saw().slow(third(4)), 0.5, 4.0)),
            ),
            (
                "a named late signal of arithmetic",
                named("gain", arithmetic(&saw().late(whole(2)).fast(third(1)))),
            ),
            (
                "a named random signal",
                named(
                    "pan",
                    range(&crate::signal::rand(), -1.0, 1.0).late(whole(1)),
                ),
            ),
            // A number of these may cut the hap of the signal inside a
            // cycle of the query: the graph answers a part that it cuts.
            (
                "a named ranged signal, slowed by a part of a cycle",
                named("cutoff", range(&sine(), 200.0, 2000.0).slow(third(4))),
            ),
            (
                "a named ranged signal, sped up",
                named("cutoff", range(&saw(), 0.0, 8.0).fast(whole(3))),
            ),
            (
                "a named ranged signal, late by a part of a cycle",
                named("gain", range(&saw(), 0.0, 8.0).late(third(1))),
            ),
            (
                "a named signal with a number at a place in the text",
                named(
                    "gain",
                    compose(
                        &sine(),
                        &pure(Value::F64(3.0)).with_added_context(vec![(2, 3)]),
                        ComposeOp::Mul,
                        Alignment::In,
                    ),
                ),
            ),
        ]
    }

    /// `signal` under each op of arithmetic, with a number for each.
    fn arithmetic(signal: &Pattern) -> Pattern {
        use ComposeOp::{Add, Band, Blshift, Bor, Brshift, Bxor, Div, Mod, Mul, Pow, Sub};
        [
            Mul, Add, Sub, Div, Pow, Mod, Blshift, Bor, Bxor, Band, Brshift,
        ]
        .into_iter()
        .zip([9.5, 1.25, 0.75, 0.5, 1.5, 7.0, 2.0, 5.0, 9.0, 14.0, 1.0])
        .fold(signal.clone(), |signal, (op, number)| {
            compose(&signal, &pure(Value::F64(number)), op, Alignment::In)
        })
    }

    /// `signal` as the values of the control `name`.
    fn named(name: &str, signal: Pattern) -> Pattern {
        crate::controls::ControlSpec::new([Arc::from(name)]).pattern(&signal)
    }

    fn structures() -> Vec<(&'static str, Pattern)> {
        let note = |n: f64| pure(Value::object([("note".to_string(), Value::F64(n))]));
        // Notes that sliders drive, as a control of a slider makes them.
        let driven = Pattern::of(Node::MapHapsOwned(Box::new((
            fastcat(vec![note(3.0), note(4.0), note(5.0)]),
            Arc::new(|mut hap: Hap| {
                hap.set_live_bindings([9, 8, 7, 6], 5);
                Some(hap)
            }),
            SpanUse::Ignores,
        ))));
        // A number that equals no number, with a lookup: a meeting keeps
        // the lookup of a value that equals itself.
        let looked_up = Pattern::of(Node::MapHapsOwned(Box::new((
            fastcat(vec![pure(Value::F64(f64::NAN)), pure(Value::F64(2.0))]),
            Arc::new(|hap: Hap| {
                let lookup = PickLookup::Array {
                    enumerable_len: 0,
                    length: 0,
                    entries: Vec::new(),
                };
                Some(hap.with_pick_lookup(Arc::new(lookup)))
            }),
            SpanUse::Ignores,
        ))));
        // Notes whose wholes begin after their parts: a meeting cuts each
        // part to the time that the whole holds too.
        let late = Pattern::of(Node::MapHapsOwned(Box::new((
            fastcat(vec![note(6.0), note(7.0), note(8.0)]),
            Arc::new(|mut hap: Hap| {
                let whole = hap.whole?;
                let quarter = (whole.end - whole.begin) * Fraction::new(1, 4);
                hap.whole = Some(TimeSpan::new(whole.begin + quarter, whole.end));
                Some(hap)
            }),
            SpanUse::Reads,
        ))));
        vec![
            ("two notes a cycle", fastcat(vec![note(0.0), note(1.0)])),
            ("notes that sliders drive", driven),
            ("numbers with a lookup", looked_up),
            ("notes with late wholes", late),
            (
                "eight notes a cycle",
                fastcat((0..8).map(|n| note(f64::from(n))).collect()),
            ),
            ("a note of three cycles", note(2.0).slow(whole(3))),
            ("a number", pure(Value::F64(5.0))),
            ("a signal", saw()),
        ]
    }

    fn spans() -> [(Fraction, Fraction); 6] {
        [
            (Fraction::ZERO, Fraction::ONE),
            (Fraction::ZERO, whole(4)),
            (third(1), third(5)),
            (third(7), third(8)),
            (third(2), third(2)),
            (third(-4), third(-1)),
        ]
    }

    #[test]
    fn a_signal_value_side_answers_as_a_query_of_it_for_each_hap() {
        for op in [ComposeOp::Set, ComposeOp::Add, ComposeOp::Mul] {
            for (begin, end) in spans() {
                // Each span asks new graphs, so that the walk answers it and
                // no table does.
                for ((structure_name, structure), (signal_name, signal)) in
                    structures().into_iter().flat_map(|structure| {
                        signals().into_iter().map(move |s| (structure.clone(), s))
                    })
                {
                    let read = compose(&structure, &signal, op, Alignment::In);
                    // A stack answers as its pattern does and is no signal,
                    // so the application queries it for each hap.
                    let queried = compose(&structure, &stack(vec![signal]), op, Alignment::In);
                    // A graph that is marked as walked reads the signal in
                    // its walk. Any other graph may read it in a table.
                    for read in [read.mark_walked(), read] {
                        assert_eq!(
                            answers(&read, begin, end),
                            answers(&queried, begin, end),
                            "{op:?} of {signal_name} on {structure_name} over {begin:?}..{end:?}"
                        );
                    }
                }
            }
        }
    }

    /// The gate of a degrade says whether each hap stays, as a query of
    /// the gate for the hap says it: for the coin of `degradeBy` and of
    /// `undegradeBy`, for a wave of other speeds and numbers, and for a
    /// signal that is no wave.
    #[test]
    fn a_degrade_keeps_the_haps_that_a_query_of_its_gate_passes() {
        use crate::combinators::{degrade_by, degrade_by_with, undegrade_by};
        use crate::signal::{rand, rand_inverted};
        let coins: Vec<(&str, Pattern)> = vec![
            ("the coin", rand()),
            ("the other side of the coin", rand_inverted()),
            ("a slow late coin", rand().slow(whole(3)).late(third(1))),
            ("a ranged coin", range(&rand(), 0.2, 0.9).fast(whole(2))),
            ("a sine", sine()),
            (
                "a map of a coin",
                rand().fmap(|value| Value::F64(value.as_f64().unwrap_or(0.0) * 0.75)),
            ),
        ];
        let seeds = [None, Some(3.5)];
        for (structure_name, structure) in structures() {
            for (coin_name, coin) in &coins {
                for amount in [0.0, 0.3, 0.5, 0.9] {
                    for ((begin, end), seed) in spans().into_iter().zip(seeds.into_iter().cycle()) {
                        let read = degrade_by_with(&structure, coin, amount);
                        // A stack of the coin is no signal, so the degrade
                        // queries it for each hap.
                        let queried =
                            degrade_by_with(&structure, &stack(vec![coin.clone()]), amount);
                        let asked = |pattern: &Pattern| {
                            let mut state = State::new(TimeSpan::new(begin, end));
                            if let Some(seed) = seed {
                                state.controls.insert("randSeed".into(), Value::F64(seed));
                            }
                            let haps = pattern.query_state(&state);
                            let bound = |hap: &Hap| {
                                let lookup = hap.pick_lookup().is_some();
                                format!("{hap:?} {:?} {lookup}", hap.live_controls())
                            };
                            haps.iter().map(bound).collect::<Vec<_>>()
                        };
                        let name = format!(
                            "{coin_name} above {amount} on {structure_name} over \
                             {begin:?}..{end:?}"
                        );
                        for read in [read.mark_walked(), read] {
                            assert_eq!(asked(&read), asked(&queried), "{name}");
                        }
                        // The gate under another meeting gives its number.
                        let gate_of = |degrade: &Pattern| match degrade.node_kind() {
                            Node::AppLeft(pair, ..) => pair.1.clone(),
                            _ => panic!("a degrade is an application"),
                        };
                        let added = |degrade: &Pattern| {
                            let gate = gate_of(degrade);
                            compose(&structure, &gate, ComposeOp::Add, Alignment::In).mark_walked()
                        };
                        let again = degrade_by_with(&structure, coin, amount);
                        assert_eq!(asked(&added(&again)), asked(&added(&queried)), "{name}");
                    }
                }
            }
            // The two degrades of a score are the two sides of one coin.
            for (begin, end) in spans() {
                let kept = answers(&degrade_by(&structure, 0.4).mark_walked(), begin, end);
                let dropped = answers(&undegrade_by(&structure, 0.6).mark_walked(), begin, end);
                // Each hap that a degrade can keep: a coin is above -1.
                let all = answers(&degrade_by(&structure, -1.0).mark_walked(), begin, end);
                assert_eq!(kept.len() + dropped.len(), all.len(), "{structure_name}");
                assert!(
                    kept.iter().all(|hap| !dropped.contains(hap)),
                    "{structure_name}"
                );
            }
        }
    }

    /// A function call gives a handle that binds the runtime of the call.
    /// A constant after a wave of such a handle goes under the wave, and
    /// the application keeps its runtime.
    #[test]
    fn a_constant_under_a_wave_keeps_the_runtime_that_the_wave_binds() {
        use crate::rng::{self, RngMode};
        use crate::settings::RuntimeSettings;
        let runtime = |mode: RngMode| {
            let settings = RuntimeSettings::default();
            settings.with(|| rng::use_rng(mode));
            settings
        };
        let gain = || {
            [(
                ComposeOp::Set,
                control("gain", Value::F64(0.5), Some((4, 7))),
            )]
        };
        // The coin of a runtime is the coin of its random numbers.
        let coin = || [(ComposeOp::Set, named("pan", crate::signal::rand()))];
        type Chain = fn(&Pattern, &[(ComposeOp, Pattern)]) -> Pattern;
        let of = |settings: &RuntimeSettings, chain: Chain| {
            let swept = chain(&steps_of_other_names(), &coin());
            chain(&swept.with_runtime_settings(settings.clone()), &gain())
        };
        let runtimes = [runtime(RngMode::Precise), runtime(RngMode::Legacy)];
        for settings in &runtimes {
            let set = of(settings, chain);
            let inner = match set.node_kind() {
                Node::AppLeft(pair, ..) => &pair.0,
                Node::AppWave(app) => &app.inner,
                _ => panic!("the application of the wave is the last one"),
            };
            assert!(matches!(inner.node_kind(), Node::Sequence(_)));
            for (begin, end) in spans() {
                assert_eq!(
                    answers(&set, begin, end),
                    answers(&of(settings, chain_of_queries), begin, end),
                    "{begin:?}..{end:?}"
                );
            }
        }
        // Steps of another runtime than the wave over them keep the
        // constant over the wave, which reads its coin under its own.
        let [one, other] = &runtimes;
        let of_two = |chain: Chain| {
            let steps = steps_of_other_names().with_runtime_settings(other.clone());
            let mut swept = chain(&steps, &coin());
            swept.force_runtime_settings(one.clone());
            chain(&swept, &gain())
        };
        for (begin, end) in spans() {
            assert_eq!(
                answers(&of_two(chain), begin, end),
                answers(&of_two(chain_of_queries), begin, end),
                "{begin:?}..{end:?}"
            );
        }
        let [one, other] =
            runtimes.map(|settings| answers(&of(&settings, chain), whole(0), whole(1)));
        assert_ne!(one, other, "each runtime has its own coin");
    }

    /// Chains of controls with a wave among them.
    fn chains_with_waves() -> Vec<(&'static str, Vec<(ComposeOp, Pattern)>)> {
        let wave = |name: &str, signal: Pattern| (ComposeOp::Set, named(name, signal));
        let number = |key: &str, n: f64| (ComposeOp::Set, control(key, Value::F64(n), None));
        let sound = |name: &str, at: (usize, usize)| {
            (
                ComposeOp::Set,
                control("s", Value::Str(name.into()), Some(at)),
            )
        };
        let cutoff = || range(&sine(), 200.0, 2000.0).slow(whole(4));
        vec![
            (
                "constants after a wave",
                vec![
                    wave("cutoff", cutoff()),
                    number("resonance", 8.0),
                    sound("saw", (3, 6)),
                    number("gain", 0.8),
                ],
            ),
            (
                "a constant of the name of the wave",
                vec![
                    wave("cutoff", cutoff()),
                    number("resonance", 8.0),
                    number("cutoff", 300.0),
                    number("gain", 0.8),
                ],
            ),
            (
                "two waves among constants",
                vec![
                    sound("saw", (3, 6)),
                    wave("cutoff", cutoff()),
                    number("note", 7.0),
                    wave("gain", saw().late(whole(1))),
                    number("pan", 0.25),
                    sound("sine", (10, 14)),
                    number("cutoff", 500.0),
                    number("room", 0.5),
                ],
            ),
            (
                "a wave of a name of the base",
                vec![
                    wave("note", saw()),
                    number("gain", 0.5),
                    wave("gain", cutoff()),
                    number("note", 9.0),
                ],
            ),
            (
                "an addition after a wave",
                vec![
                    wave("gain", saw()),
                    (ComposeOp::Add, control("gain", Value::F64(0.25), None)),
                    number("pan", 0.5),
                ],
            ),
            (
                "a constant after a wave that may cut a part",
                vec![
                    wave("cutoff", range(&saw(), 0.0, 8.0).fast(whole(3))),
                    number("resonance", 8.0),
                ],
            ),
            (
                "a value alone after a wave",
                vec![
                    wave("cutoff", cutoff()),
                    number("gain", 0.8),
                    number("value", 2.0),
                    number("pan", 0.25),
                ],
            ),
        ]
    }

    /// A sequence of steps that hold other names, so that a new name finds
    /// its place in each map.
    fn steps_of_other_names() -> Pattern {
        let steps = [
            vec![("note", 0.0), ("gain", 0.3)],
            vec![("cutoff", 90.0), ("note", 1.0)],
            vec![("note", 2.0)],
        ];
        let steps = steps
            .into_iter()
            .zip(0..)
            .map(|(entries, index)| {
                let controls = entries
                    .into_iter()
                    .map(|(key, n)| (key.to_string(), Value::F64(n)));
                SequenceStep::new(
                    third(index),
                    third(index + 1),
                    Value::object(controls),
                    Locations::from(vec![(index as usize, index as usize + 2)]),
                )
            })
            .collect();
        sequence(vec![SequenceLayer {
            period: Fraction::ONE,
            steps,
        }])
        .expect("steps in order")
    }

    #[test]
    fn a_chain_with_a_wave_answers_as_each_application_in_turn() {
        let bases = bases()
            .into_iter()
            .chain([("steps of other names", steps_of_other_names())]);
        for (base_name, base) in bases {
            for (chain_name, values) in chains_with_waves() {
                for (begin, end) in spans() {
                    let queried = chain_of_queries(&base, &values);
                    // Each span asks a new graph, so that the walk answers.
                    let read = chain(&base, &values);
                    for read in [read.mark_walked(), read] {
                        assert_eq!(
                            answers(&read, begin, end),
                            answers(&queried, begin, end),
                            "{chain_name} on {base_name} over {begin:?}..{end:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn constants_after_a_wave_are_set_under_its_application() {
        let (_, values) = chains_with_waves().swap_remove(0);
        let set = chain(&steps_of_other_names(), &values);
        let inner = match set.node_kind() {
            Node::AppLeft(pair) => {
                assert!(matches!(pair.3, Combine::Composed(ComposeOp::Set)));
                &pair.0
            }
            Node::AppWave(app) => &app.inner,
            _ => panic!("the application of the wave is the last one"),
        };
        assert!(
            matches!(inner.node_kind(), Node::Sequence(_)),
            "the steps hold each constant"
        );
    }
}

#[cfg(test)]
mod restart_tests {
    use super::super::*;

    fn whole(n: i128) -> Fraction {
        Fraction::new(n, 1)
    }

    fn third(n: i128) -> Fraction {
        Fraction::new(n, 3)
    }

    fn note(n: f64) -> Pattern {
        pure(Value::object([("note".to_string(), Value::F64(n))]))
            .with_added_context(vec![(n as usize, n as usize + 1)])
    }

    fn steps(from: i32, count: i32) -> Pattern {
        fastcat((0..count).map(|n| note(f64::from(from + n))).collect())
    }

    #[test]
    fn cpm_keeps_exact_step_times_when_the_scheduler_tempo_changes() {
        let source = stack(vec![steps(0, 16), steps(20, 3), crate::signal::saw()]);
        for cpm in [32, 33] {
            let tempo = source.cpm(whole(cpm)).mark_walked();
            for (cps, denominator) in [(0.5, 30), (0.75, 45), (0.5, 30)] {
                let speed = Fraction::new(cpm, denominator);
                let walked = source
                    .with_query_time(move |t| t * speed)
                    .with_hap_time(move |t| t / speed)
                    .mark_walked();
                for tick in -24..48 {
                    for width in [0, 1, 5, 11] {
                        let span =
                            TimeSpan::new(Fraction::new(tick, 13), Fraction::new(tick + width, 13));
                        let mut state = State::new(span);
                        state.controls.insert("_cps".into(), Value::F64(cps));
                        assert_eq!(
                            format!("{:?}", tempo.query(&state)),
                            format!("{:?}", walked.query(&state)),
                            "{span:?} at {cpm} cpm and {cps} cps"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn speed_and_shift_restore_shared_edges_of_cut_haps() {
        let source = stack(vec![steps(0, 4), steps(10, 3), crate::signal::saw()]);
        for speed in [Fraction::ONE, Fraction::new(11, 10), Fraction::new(2, 3)] {
            for shift in [Fraction::ZERO, whole(-2), Fraction::new(3, 5)] {
                let mapped = source.late(shift).fast(speed).mark_walked();
                let walked = source
                    .with_query_time(move |t| t - shift)
                    .with_hap_time(move |t| t + shift)
                    .with_query_time(move |t| t * speed)
                    .with_hap_time(move |t| t / speed)
                    .mark_walked();
                for tick in -16..32 {
                    for width in [0, 1, 5, 13] {
                        let begin = Fraction::new(tick, 8);
                        let end = Fraction::new(tick + width, 8);
                        assert_eq!(
                            format!("{:?}", mapped.query_arc(begin, end)),
                            format!("{:?}", walked.query_arc(begin, end)),
                            "{begin:?}..{end:?} at {speed:?} shifted by {shift:?}"
                        );
                    }
                }
            }
        }
    }

    /// Sections of discrete haps: steps, stacks, alternations, structures,
    /// speed changes, a coin and controls, with a signal as the value of one.
    fn discrete_sections() -> Vec<Pattern> {
        use crate::compose::{Alignment, ComposeOp, compose};
        let cutoff = crate::signal::saw()
            .slow(whole(3))
            .fmap(|value| Value::object([("cutoff".to_string(), value.clone())]));
        vec![
            steps(0, 4),
            stack(vec![steps(10, 3), steps(20, 2).slow(whole(2))]),
            slowcat(vec![steps(30, 2), note(33.0), steps(34, 3)]).fast(third(2)),
            compose(&steps(40, 8), &cutoff, ComposeOp::Set, Alignment::In),
            crate::combinators::struct_with(
                &steps(50, 2).slow(whole(3)),
                &fastcat(
                    [true, false, true, true]
                        .map(|on| pure(Value::Bool(on)))
                        .to_vec(),
                ),
            )
            .late(third(1)),
            steps(60, 5).degrade_by_seeded(0.5, 7),
            crate::combinators::sometimes_by(&steps(70, 6), 0.5, None),
            picked(
                JoinMode::Restart,
                vec![steps(80, 3), steps(90, 2).slow(whole(2))],
                third(2),
            ),
        ]
    }

    /// Sections with a layer that reads the span: a stack of steps and a
    /// signal, alone and in another stack.
    fn sections_with_a_signal() -> Vec<Pattern> {
        let mixed = stack(vec![steps(100, 4), crate::signal::saw(), steps(110, 3)]);
        vec![mixed.clone(), stack(vec![steps(120, 2), mixed])]
    }

    /// A pick of `sections` in turn, each for `cycles` cycles.
    fn picked(mode: JoinMode, sections: Vec<Pattern>, cycles: Fraction) -> Pattern {
        let count = sections.len();
        let selector = fastcat((0..count).map(|n| pure(Value::F64(n as f64))).collect())
            .slow(cycles * Fraction::int(count as i128));
        pick(
            selector,
            PickLookup::Array {
                enumerable_len: count,
                length: count,
                entries: sections.into_iter().enumerate().collect(),
            },
            PickIndexMode::Clamp,
            mode,
        )
    }

    /// `section` behind a time map that changes no time. The join does not
    /// know the map, so it asks such a section over the span of the query.
    fn asked_over_the_span(section: Pattern) -> Pattern {
        section.with_query_time(|time| time)
    }

    #[test]
    fn restarted_sequence_steps_keep_their_parts_and_clip_their_wholes() {
        let section = || {
            let at = |n| Fraction::new(n, 8);
            let step = SequenceStep::new(at(0), at(8), Value::F64(1.0), vec![(2, 5)].into());
            sequence(vec![SequenceLayer {
                period: whole(2),
                steps: vec![
                    step.placed((at(0), at(6)), (at(1), at(4))),
                    step.placed((at(3), at(8)), (at(4), at(7))),
                ],
            }])
            .expect("ordered steps")
        };
        for mode in [JoinMode::Reset, JoinMode::Restart] {
            let pick = |choice| picked(mode, vec![choice], third(4)).mark_walked();
            let direct = pick(section());
            let graph = pick(asked_over_the_span(section()));
            for start in -36..36 {
                for width in [0, 1, 3, 9, 19] {
                    let state = State::new(TimeSpan::new(
                        Fraction::new(start, 9),
                        Fraction::new(start + width, 9),
                    ));
                    assert_eq!(
                        format!("{:?}", direct.query(&state)),
                        format!("{:?}", graph.query(&state)),
                        "{mode:?} at {start}/9 for {width}/9"
                    );
                }
            }
        }
    }

    #[test]
    fn a_restart_repeats_every_layer_only_at_a_shared_period() {
        let section = || {
            let layer = |period, value| SequenceLayer {
                period,
                steps: vec![SequenceStep::new(
                    Fraction::ZERO,
                    third(2),
                    Value::F64(value),
                    Locations::new(),
                )],
            };
            sequence(vec![layer(whole(2), 1.0), layer(third(2), 2.0)])
                .expect("two layers of different periods")
        };
        for cycles in [whole(2), whole(3), third(5)] {
            let pick = |choice| picked(JoinMode::Restart, vec![choice], cycles).mark_walked();
            let repeated = pick(section());
            let moved = pick(asked_over_the_span(section()));
            for tick in -30..45 {
                for width in [0, 1, 4, 13] {
                    let span = TimeSpan::new(third(tick), third(tick + width));
                    let state = State::new(span);
                    assert_eq!(
                        format!("{:?}", repeated.query(&state)),
                        format!("{:?}", moved.query(&state)),
                        "{span:?} with a restart every {cycles:?} cycles"
                    );
                }
            }
        }
    }

    #[test]
    fn a_restart_asks_discrete_haps_over_their_part_alone() {
        let spans = [
            (Fraction::ZERO, whole(16)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (whole(2), third(7)),
            (third(13), third(29)),
            (third(7), third(7)),
            (whole(14), whole(14)),
        ];
        let sections = || {
            let mut sections = discrete_sections();
            sections.extend(sections_with_a_signal());
            sections
        };
        for mode in [JoinMode::Restart, JoinMode::Reset] {
            for (begin, end) in spans {
                // Each span asks new graphs, so that the walk answers it
                // and no table does. Each section holds two cycles and a
                // third of one.
                let cut = picked(mode, sections(), third(7));
                let whole_span = picked(
                    mode,
                    sections().into_iter().map(asked_over_the_span).collect(),
                    third(7),
                );
                assert_eq!(
                    format!("{:?}", cut.query_arc(begin, end)),
                    format!("{:?}", whole_span.query_arc(begin, end)),
                    "{mode:?} over {begin:?}..{end:?}"
                );
            }
        }
    }

    /// A sequence of `values`, one a cycle.
    fn each_cycle(values: &[f64]) -> Pattern {
        let count = values.len() as i128;
        let steps = values
            .iter()
            .zip(0..)
            .map(|(value, index)| {
                SequenceStep::new(
                    Fraction::new(index, count),
                    Fraction::new(index + 1, count),
                    Value::F64(*value),
                    Locations::new(),
                )
            })
            .collect();
        sequence(vec![SequenceLayer {
            period: Fraction::int(count),
            steps,
        }])
        .expect("steps in order")
    }

    fn transformer(
        name: &str,
        apply: impl Fn(Pattern) -> Pattern + Send + Sync + 'static,
    ) -> Pattern {
        pure(Value::Function(std::sync::Arc::new(
            crate::value::FunctionRef::registered(name, Arc::new(apply), true),
        )))
    }

    /// `every` of a count for each cycle, with `apply`, over `receiver`: a
    /// join that makes its patterns at the query.
    fn every_cycle(
        counts: &[f64],
        name: &str,
        apply: impl Fn(Pattern) -> Pattern + Send + Sync + 'static,
        receiver: Pattern,
    ) -> Pattern {
        crate::register::default_registry()
            .get("every")
            .expect("registered every")
            .call(&[each_cycle(counts), transformer(name, apply)], receiver)
    }

    /// Sections with a join that makes patterns of discrete haps at the
    /// query: a count with a transformer, and a count of repeats.
    fn sections_made_at_the_query() -> Vec<Pattern> {
        let registry = crate::register::default_registry();
        let ply = registry.get("ply").expect("registered ply");
        vec![
            every_cycle(&[2.0, 3.0], "rev", |pattern| pattern.rev(), steps(130, 4)),
            ply.call(&[each_cycle(&[2.0, 1.0, 3.0])], steps(140, 3)),
            stack(vec![
                steps(150, 2),
                ply.call(&[each_cycle(&[1.0, 2.0])], steps(160, 2)),
            ]),
        ]
    }

    /// A section with a join that makes a pattern that reads the span.
    fn section_that_makes_a_signal() -> Pattern {
        every_cycle(
            &[1.0, 2.0],
            "with a signal",
            |pattern| stack(vec![pattern, crate::signal::saw()]),
            steps(170, 3),
        )
    }

    #[test]
    fn a_restart_asks_patterns_made_at_the_query_over_their_part() {
        let spans = [
            (Fraction::ZERO, whole(16)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(13), third(29)),
            (third(7), third(7)),
        ];
        let sections = || {
            let mut sections = sections_made_at_the_query();
            sections.push(section_that_makes_a_signal());
            sections
        };
        for mode in [JoinMode::Restart, JoinMode::Reset] {
            for (begin, end) in spans {
                // A walk asks over the part, and falls back for the section
                // that makes a signal. A query that may read a table asks
                // over the span.
                let whole_span = picked(
                    mode,
                    sections().into_iter().map(asked_over_the_span).collect(),
                    third(7),
                );
                for cut in [
                    picked(mode, sections(), third(7)).mark_walked(),
                    picked(mode, sections(), third(7)),
                ] {
                    assert_eq!(
                        format!("{:?}", cut.query_arc(begin, end)),
                        format!("{:?}", whole_span.query_arc(begin, end)),
                        "{mode:?} over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_join_says_at_the_query_when_a_pattern_that_it_made_reads_the_span() {
        use purity::SpanRead;
        let state = State::new(TimeSpan::new(third(1), third(8)));
        for section in sections_made_at_the_query() {
            assert_eq!(section.purity().span_read, SpanRead::Made);
            assert!(asked_over_part(|| section.query(&state)).is_some());
        }
        let signal = section_that_makes_a_signal();
        assert_eq!(signal.purity().span_read, SpanRead::Made);
        assert!(asked_over_part(|| signal.query(&state)).is_none());
        // A query over a part around the refused one is refused too, and
        // no query asks over a part after them.
        let around = asked_over_part(|| asked_over_part(|| signal.query(&state)).is_none());
        assert_eq!(around, None);
        assert!(!part_is_asked());
        assert!(!signal.query(&state).is_empty());
    }

    #[test]
    fn a_part_query_asks_no_volatile_pattern_that_a_join_made() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let asked = Arc::new(AtomicUsize::new(0));
        let section = || {
            let asked = Arc::clone(&asked);
            every_cycle(
                &[1.0, 1.0],
                "counted",
                move |pattern| {
                    let asked = Arc::clone(&asked);
                    pattern
                        .with_query_span(move |span| {
                            asked.fetch_add(1, Ordering::Relaxed);
                            *span
                        })
                        .mark_volatile()
                },
                steps(180, 2),
            )
        };
        let sections = |section: Pattern| {
            picked(JoinMode::Restart, vec![section, steps(190, 2)], whole(2)).mark_walked()
        };
        let over_the_span = sections(asked_over_the_span(section()));
        let expected = format!("{:?}", over_the_span.query_arc(Fraction::ZERO, whole(8)));
        let asks = asked.swap(0, Ordering::Relaxed);
        assert!(asks > 0);
        let cut = sections(section());
        assert_eq!(
            format!("{:?}", cut.query_arc(Fraction::ZERO, whole(8))),
            expected
        );
        assert_eq!(asked.load(Ordering::Relaxed), asks);
    }

    /// What `pattern` answers to a top-level query of `span` as `state`
    /// says, in the order of the graph and sorted.
    fn asked(pattern: &Pattern, span: TimeSpan, state: &State) -> String {
        let state = state.set_span(span);
        let graph = pattern.query_arc_outcome_with_budget(&state, DEFAULT_HAP_BUDGET);
        let contained = take_query_contained_throw();
        let sorted = pattern.query_arc_outcome_sorted_with_budget(&state, DEFAULT_HAP_BUDGET);
        format!(
            "{graph:?} {contained:?} {sorted:?} {:?}",
            take_query_contained_throw()
        )
    }

    /// `count` spans of `width` in a row from `from`.
    fn in_a_row(from: Fraction, width: Fraction, count: i128) -> Vec<TimeSpan> {
        (0..count)
            .map(|index| {
                let begin = from + width * Fraction::int(index);
                TimeSpan::new(begin, begin + width)
            })
            .collect()
    }

    /// Graphs of discrete haps, whole songs in small: sections one after
    /// the other, with joins that make patterns at the query, at a tempo.
    fn songs() -> Vec<(&'static str, Pattern)> {
        let made = || picked(JoinMode::Restart, sections_made_at_the_query(), third(4));
        vec![
            ("steps", steps(0, 4)),
            (
                "sections",
                picked(JoinMode::Restart, discrete_sections(), third(7)),
            ),
            (
                "sections that reset",
                picked(JoinMode::Reset, discrete_sections(), third(2)),
            ),
            ("sections made at the query", made()),
            (
                "a song at a tempo",
                stack(vec![made(), steps(200, 3)]).cpm(Fraction::int(45)),
            ),
            // A long step sounds on when a short one begins and ends in a
            // query: the cut parts say which comes first.
            (
                "long steps over short ones",
                stack(vec![steps(210, 2), steps(220, 16)]),
            ),
            // A cycle of the song is more than a cycle of its patterns:
            // one that asks for each cycle, and one that does not.
            (
                "patterns in turn at a tempo",
                stack(vec![
                    in_turn(300).split_queries(),
                    in_turn(340),
                    Pattern::of(Node::SlowCatPrime(
                        vec![in_turn(360), steps(370, 3)].into_boxed_slice(),
                    ))
                    .split_queries(),
                    steps(380, 2),
                ])
                .cpm(Fraction::int(35)),
            ),
        ]
    }

    /// Three patterns, one a cycle: steps, lanes, and steps of two cycles.
    fn in_turn(from: i32) -> Pattern {
        slowcat(vec![
            steps(from, 2),
            stack(vec![steps(from + 10, 3), note(f64::from(from + 15))]),
            steps(from + 20, 4).slow(whole(2)),
        ])
    }

    /// A map of the haps of a list from a place on keeps the haps before
    /// the place and the order of the others, and a hap that the map drops
    /// leaves. A map that unwinds drops no hap two times.
    #[test]
    fn a_map_of_the_haps_of_a_list_writes_each_where_it_is() {
        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let shared: Arc<str> = "x".into();
        let hap = |n: i32| Hap::new(Some(span), span, Value::F64(f64::from(n)));
        let numbers = |haps: &[Hap]| -> Vec<f64> {
            haps.iter().filter_map(|hap| hap.value.as_f64()).collect()
        };
        let mut haps: Vec<Hap> = (0..6).map(hap).collect();
        crate::map_from(&mut haps, 2, |hap| match hap.value.as_f64() {
            Some(3.0 | 5.0) => None,
            number => Some(hap.with_value(|_| Value::F64(number.unwrap_or(0.0) * 10.0))),
        });
        assert_eq!(numbers(&haps), [0.0, 1.0, 20.0, 40.0]);
        crate::map_from(&mut haps, 4, |_| None);
        assert_eq!(numbers(&haps), [0.0, 1.0, 20.0, 40.0]);

        // Five haps hold the text. The map takes the fourth and unwinds:
        // that hap drops, and the fifth is not read.
        let held = || {
            let mut hap = Hap::new(Some(span), span, Value::F64(0.0));
            hap.keys.edit().scale = Some(Arc::clone(&shared));
            hap
        };
        let mut haps: Vec<Hap> = (0..5).map(|_| held()).collect();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut taken = 0;
            crate::map_from(&mut haps, 1, |hap| {
                taken += 1;
                assert!(taken < 3, "the map stops");
                Some(hap)
            });
        }));
        assert!(unwound.is_err());
        assert_eq!(haps.len(), 3);
        drop(haps);
        assert_eq!(Arc::strong_count(&shared), 2);
    }

    #[test]
    fn a_run_of_short_queries_matches_fresh_walks() {
        let widths = [Fraction::new(1, 8), Fraction::new(5, 48), third(1)];
        let tempo = |cps: f64| {
            let mut state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
            state.controls.insert("_cps".to_string(), Value::F64(cps));
            state
        };
        for (index, (name, _)) in songs().into_iter().enumerate() {
            for width in widths {
                for state in [
                    State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE)),
                    tempo(0.75),
                ] {
                    for start in [third(-4), Fraction::ZERO] {
                        let song = || songs().swap_remove(index).1.mark_walked();
                        let run = song();
                        for span in in_a_row(start, width, 24) {
                            assert_eq!(
                                asked(&run, span, &state),
                                asked(&song(), span, &state),
                                "{name} over {span:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_tempo_change_keeps_what_its_pattern_says_of_the_span() {
        use purity::SpanRead;
        let read = |pattern: Pattern| pattern.purity().span_read;
        assert_eq!(read(steps(0, 4).cpm(Fraction::int(30))), SpanRead::No);
        let made = sections_made_at_the_query().swap_remove(0);
        assert_eq!(read(made.cpm(Fraction::int(30))), SpanRead::Made);
        assert_eq!(
            read(crate::signal::saw().cpm(Fraction::int(30))),
            SpanRead::Yes
        );
        assert_eq!(read(steps(0, 4).cpm(Fraction::int(-30))), SpanRead::Yes);
    }

    /// A sequence of control maps `{n}`, each at its location: `count`
    /// steps in each of `cycles` cycles, from `from`.
    fn degrees(from: i32, count: i128, cycles: i128) -> Pattern {
        let total = count * cycles;
        let steps = (0..total)
            .map(|index| {
                let degree = f64::from(from) + (index % 7) as f64;
                let mut locations = Locations::new();
                locations.push((
                    from as usize + index as usize,
                    from as usize + index as usize + 1,
                ));
                SequenceStep::new(
                    Fraction::new(index, total),
                    Fraction::new(index + 1, total),
                    Value::object([("n".to_string(), Value::F64(degree))]),
                    locations,
                )
            })
            .collect();
        sequence(vec![SequenceLayer {
            period: Fraction::int(cycles),
            steps,
        }])
        .expect("steps in order")
    }

    /// A pick of `sections` in turn by a sequence, each for `length` cycles.
    fn picked_by_a_sequence(mode: JoinMode, sections: Vec<Pattern>, length: Fraction) -> Pattern {
        let count = sections.len() as i128;
        let steps = (0..count)
            .map(|index| {
                let mut locations = Locations::new();
                locations.push((900 + index as usize, 901 + index as usize));
                SequenceStep::new(
                    Fraction::new(index, count),
                    Fraction::new(index + 1, count),
                    Value::F64(index as f64),
                    locations,
                )
            })
            .collect();
        let selector = sequence(vec![SequenceLayer {
            period: length * Fraction::int(count),
            steps,
        }])
        .expect("steps in order");
        pick(
            selector,
            PickLookup::Array {
                enumerable_len: sections.len(),
                length: sections.len(),
                entries: sections.into_iter().enumerate().collect(),
            },
            PickIndexMode::Clamp,
            mode,
        )
    }

    #[test]
    fn a_change_after_a_pick_answers_as_the_change_of_what_it_picks() {
        use crate::compose::{Alignment, ComposeOp, compose};
        use crate::controls::ControlSpec;
        let spans = [
            (Fraction::ZERO, whole(12)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(13), third(29)),
            (third(7), third(7)),
        ];
        let control = |key: &str, value: f64, at: usize| {
            pure(Value::F64(value))
                .with_added_context(vec![(at, at + 2)])
                .map_control(&ControlSpec::new([key]), false)
        };
        // A scale, then two controls: what `n(..).scale(..).gain(..)` is
        // after a pick.
        let changed = |pattern: Pattern| {
            let scaled = crate::combinators::scale(&pattern, Value::Str("C:minor".into()));
            let quiet = compose(
                &scaled,
                &control("gain", 0.5, 700),
                ComposeOp::Set,
                Alignment::In,
            );
            compose(
                &quiet,
                &control("room", 0.2, 710),
                ComposeOp::Set,
                Alignment::In,
            )
        };
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        // Steps in one cycle each, a rest, a control of a rest as `n("~")`
        // is, and a step of three cycles.
        let rest = || silence().map_control(&ControlSpec::new(["n"]), false);
        assert!(rest().is_rest() && changed(rest()).is_rest());
        let short = || vec![degrees(0, 4, 1), silence(), rest(), degrees(20, 3, 2)];
        let long = || vec![degrees(0, 4, 1), degrees(40, 1, 3).slow(whole(3))];
        for mode in [
            JoinMode::Inner,
            JoinMode::Outer,
            JoinMode::Mix,
            JoinMode::Restart,
            JoinMode::Reset,
        ] {
            let cases: [(&str, &dyn Fn() -> Pattern, bool); 3] = [
                // The picked patterns take the scale and the controls.
                (
                    "whole cycles",
                    &|| picked_by_a_sequence(mode, short(), whole(2)),
                    true,
                ),
                // A restart moves a section by a part of a cycle, where
                // the constant of the pick cuts another way.
                (
                    "parts of a cycle",
                    &|| picked_by_a_sequence(mode, short(), third(7)),
                    !matches!(mode, JoinMode::Restart | JoinMode::Reset),
                ),
                // A step of three cycles is cut by the constant.
                (
                    "a long step",
                    &|| picked_by_a_sequence(mode, long(), whole(2)),
                    false,
                ),
            ];
            for (name, build, all_inside) in cases {
                let inside = changed(build());
                assert_eq!(
                    matches!(inside.node_kind(), Node::Pick { .. }),
                    all_inside,
                    "{mode:?}, {name}"
                );
                let outside = sequence::unfolded(|| changed(build()));
                assert!(!matches!(outside.node_kind(), Node::Pick { .. }));
                for (begin, end) in spans {
                    assert_eq!(
                        answers(&inside, begin, end),
                        answers(&outside, begin, end),
                        "{mode:?}, {name}, over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_structure_asks_discrete_functions_in_one_query() {
        use crate::compose::{Alignment, ComposeOp, compose};
        let spans = [
            (Fraction::ZERO, whole(4)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(7), third(7)),
        ];
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        // Steps that fill the cycle, steps that overlap, and long steps.
        let structures = || {
            let on = |count: i32| fastcat((0..count).map(|_| pure(Value::Bool(true))).collect());
            vec![
                on(12),
                stack(vec![on(3), on(4).late(third(1))]),
                on(5).slow(whole(3)),
            ]
        };
        for (index, functions) in discrete_sections().into_iter().enumerate() {
            for (kind, structure) in structures().into_iter().enumerate() {
                assert_eq!(functions.purity().span_read, purity::SpanRead::No);
                let structured = |functions: &Pattern| {
                    compose(functions, &structure, ComposeOp::KeepIf, Alignment::Out)
                };
                // Behind a time map that changes no time, the functions are
                // asked for each step of the structure.
                let (one_query, each_step) = (
                    structured(&functions),
                    structured(&asked_over_the_span(functions.clone())),
                );
                for (begin, end) in spans {
                    assert_eq!(
                        answers(&one_query, begin, end),
                        answers(&each_step, begin, end),
                        "section {index}, structure {kind}, over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_split_of_queries_asks_a_discrete_pattern_once_for_all_the_cycles() {
        let spans = [
            (Fraction::ZERO, whole(4)),
            (third(-5), third(22)),
            (third(7), third(8)),
            (third(7), third(7)),
        ];
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        for (index, section) in discrete_sections().into_iter().enumerate() {
            assert_eq!(section.purity().span_read, purity::SpanRead::No);
            // Behind a time map that changes no time, the section is asked
            // for each cycle.
            let (one_query, each_cycle) = (
                section.split_queries(),
                asked_over_the_span(section.clone()).split_queries(),
            );
            // The bind of a value to the section, which `sometimes` was
            // before it was a split: each cycle of the value asks the
            // section and adds nothing to its haps.
            let bound = pure(Value::F64(0.5))
                .fmap_to_pattern(move |_| section.clone())
                .inner_join();
            for (begin, end) in spans {
                let split = answers(&one_query, begin, end);
                assert_eq!(
                    split,
                    answers(&each_cycle, begin, end),
                    "section {index}, each cycle, over {begin:?}..{end:?}"
                );
                assert_eq!(
                    split,
                    answers(&bound, begin, end),
                    "section {index}, the bind, over {begin:?}..{end:?}"
                );
            }
        }
    }

    #[test]
    fn an_application_asks_a_discrete_side_over_the_part_of_a_hap() {
        use crate::compose::{Alignment, ComposeOp, compose};
        // Windows inside a note of three cycles, a window of no width, and
        // spans that hold the note.
        let spans = [
            (third(1), third(2)),
            (third(4), third(5)),
            (third(7), third(8)),
            (third(7), third(7)),
            (Fraction::ZERO, whole(4)),
            (third(-5), third(22)),
        ];
        let answers = |pattern: &Pattern, begin: Fraction, end: Fraction| -> Vec<String> {
            pattern
                .query_arc(begin, end)
                .iter()
                .map(|hap| format!("{hap:?} {:?} {:?}", hap.keys, hap.live_controls()))
                .collect()
        };
        let long = || note(300.0).slow(whole(3));
        for (index, section) in discrete_sections().into_iter().enumerate() {
            // The long note gives the structure, on each side in turn.
            for alignment in [Alignment::In, Alignment::Out] {
                let met = |section: &Pattern| match alignment {
                    Alignment::Out => compose(section, &long(), ComposeOp::Set, alignment),
                    _ => compose(&long(), section, ComposeOp::Set, alignment),
                };
                // Behind a time map that changes no time, the section is
                // asked over the whole of the note.
                let (over_part, over_whole) =
                    (met(&section), met(&asked_over_the_span(section.clone())));
                for (begin, end) in spans {
                    assert_eq!(
                        answers(&over_part, begin, end),
                        answers(&over_whole, begin, end),
                        "section {index}, {alignment:?}, over {begin:?}..{end:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_graph_that_reads_the_span_is_no_graph_of_discrete_haps() {
        let discrete = |pattern: &Pattern| pattern.purity().span_read == purity::SpanRead::No;
        for section in discrete_sections() {
            assert!(discrete(&section));
            assert!(!discrete(&asked_over_the_span(section)));
        }
        for section in sections_with_a_signal() {
            assert!(!discrete(&section));
        }
        assert!(!discrete(&crate::signal::saw()));
    }

    #[test]
    fn met_bounds_turns_for_large_coprime_periods() {
        use crate::compose::{Alignment, ComposeOp, compose};
        let a = fastcat(vec![note(1.0), note(2.0)]).fast(Fraction::int(1_048_576));
        let b = fastcat(vec![silence(), note(1.0)]).slow(Fraction::int(1_048_575));
        let combined = compose(&a, &b, ComposeOp::Add, Alignment::In);
        // Must return promptly without folding 10^12 turns and query cleanly over a small window.
        let haps = combined.query_arc(Fraction::ZERO, Fraction::new(1, 1024));
        assert!(haps.len() <= 4096);
    }

    #[test]
    fn split_queries_fast_path_charges_hap_budget() {
        let long = note(60.0).slow(Fraction::int(1000)).split_queries();
        let (haps, limit) =
            with_hap_budget(10, || long.query_arc(Fraction::ZERO, Fraction::int(100)));
        assert!(
            matches!(limit, Some(QueryLimit::HapBudget { budget: 10 })),
            "{limit:?}"
        );
        assert!(haps.is_empty());
    }

    #[test]
    fn degrade_by_nan_silences_all_haps() {
        use crate::combinators::{degrade_by, undegrade_by};
        let pat = fastcat(vec![note(1.0), note(2.0), note(3.0)]);
        assert!(
            degrade_by(&pat, f64::NAN)
                .query_arc(Fraction::ZERO, whole(4))
                .is_empty()
        );
        assert!(
            undegrade_by(&pat, f64::NAN)
                .query_arc(Fraction::ZERO, whole(4))
                .is_empty()
        );
    }

    #[test]
    fn shift_does_not_truncate_ticks_above_u16_max() {
        let base = fastcat(vec![note(1.0), silence()]).fast(Fraction::int(65_536));
        let shifted = base.late(Fraction::new(1, 131_072));
        let orig = base.query_arc(Fraction::ZERO, Fraction::new(1, 65_536));
        let moved = shifted.query_arc(Fraction::ZERO, Fraction::new(1, 65_536));
        assert_eq!(orig.len(), 1);
        assert_eq!(moved.len(), 1);
        assert_ne!(
            orig[0].part, moved[0].part,
            "shift by 1/131072 must not wrap to 0"
        );
    }

    #[test]
    fn reversed_multi_step_sequence_preserves_graph_order() {
        let pat = fastcat(vec![note(1.0), note(2.0)]);
        let folded_rev = pat.rev();
        let graph_rev = Pattern::of(Node::Rev(Box::new(pat.clone())));
        let state = State::new(TimeSpan::new(Fraction::ZERO, whole(1)));
        let show =
            |haps: Vec<Hap>| -> Vec<String> { haps.iter().map(|hap| format!("{hap:?}")).collect() };
        assert_eq!(
            show(folded_rev.query(&state)),
            show(graph_rev.query(&state))
        );
    }

    #[test]
    fn sorted_sequence_enforces_max_query_span_cycles() {
        let single = fastcat(vec![note(1.0), note(2.0)]);
        let stacked = stack(vec![note(1.0), note(2.0)]);
        let wide = MAX_QUERY_SPAN_CYCLES + 5;
        let (_, limit_single) = with_hap_budget(u64::MAX, || {
            single.query_arc(Fraction::ZERO, Fraction::int(wide))
        });
        assert!(
            matches!(limit_single, Some(QueryLimit::QuerySpan { .. })),
            "single-layer sorted query must enforce MAX_QUERY_SPAN_CYCLES: {limit_single:?}"
        );
        let (_, limit_stack) = with_hap_budget(u64::MAX, || {
            stacked.query_arc(Fraction::ZERO, Fraction::int(wide))
        });
        assert!(
            matches!(limit_stack, Some(QueryLimit::QuerySpan { .. })),
            "multi-layer sorted query must enforce MAX_QUERY_SPAN_CYCLES: {limit_stack:?}"
        );
    }

    #[test]
    fn same_construction_distinguishes_app_wave_segment_steps() {
        use crate::combinators::segment;
        use crate::compose::{Alignment, ComposeOp, compose};
        use crate::controls::ControlSpec;
        let lpf = ControlSpec::new([Arc::from("lpf")]);
        let base = fastcat(vec![note(1.0), note(2.0)]);
        let w4 = compose(
            &base,
            &lpf.pattern(&segment(&crate::signal::sine(), Fraction::int(4))),
            ComposeOp::Set,
            Alignment::In,
        );
        let w8 = compose(
            &base,
            &lpf.pattern(&segment(&crate::signal::sine(), Fraction::int(8))),
            ComposeOp::Set,
            Alignment::In,
        );
        assert!(
            !w4.same_construction(&w8),
            "AppWave with different segment step counts must not compare equal"
        );
    }

    #[test]
    fn order_key_handles_i128_min_without_panicking() {
        assert!(crate::order_key(Fraction::int(i128::MIN)).is_nan());
    }
}

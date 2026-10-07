//! The QuickJS boundary.
//!
//! # Ownership model
//!
//! ```text
//!   JS heap                                  Rust
//!   ───────                                  ────
//!   runtime userdata.active[0] ──┐
//!   runtime userdata.held[i]   ──┼──▶ PatternWrapper ──▶ Arc<PatternNode>
//!                                │         │                    │
//!                                │         │ owns               │ holds
//!                                │         ▼                    ▼
//!                                └───▶ Vec<CallbackCell> ◀── CallbackId (opaque)
//!                                             │
//!                                             ▼
//!                                        JS Function ──▶ (may capture the wrapper)
//! ```
//!
//! A callback is owned by every wrapper whose graph reaches it. Roots are
//! explicit and finite: one active slot plus patterns deliberately held by the
//! caller. Replacing a root makes its wrapper, callback cells, functions, and
//! Rust nodes collectable together. Rust pattern nodes store opaque callback
//! ids, never JavaScript values.
//!
//! Callback ids are globally unique for the runtime lifetime. A derived graph
//! imports the cells of every contributing wrapper, so it remains independent
//! if its sources are collected. A missing reachable cell is a hard error.
//!
//! Queries can nest, so the current callback owner is an RAII stack. Opaque
//! graphs conservatively mark every owned cell because their complete reachable
//! set cannot be known safely.

use rquickjs::{
    Context, Ctx, FromJs, Function, JsLifetime, Runtime,
    class::{Trace, Tracer},
};
use rustel_core::{BindArg, BindResult, CallbackHost, CallbackId, Hap, Pattern, State, Value};
use rustel_fraction::Fraction;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;

/// Maximum JS container nesting copied into a host Value. The cap stops a
/// cycle or a long chain before the walk exhausts the Rust stack.
const MAX_JS_VALUE_DEPTH: usize = 64;

mod alloc;
mod bridge;
mod constants;
mod fraction;
mod host;
mod install;
#[cfg(feature = "hydra")]
pub use install::{HYDRA_SCOPE_POLICY, HydraCandidate};
mod pattern;
mod runtime;
mod surface;
mod thread_state;
mod value;

pub use bridge::*;
pub use constants::supported_global_names;
use constants::*;
use host::*;
pub use install::reference_entries;
use pattern::*;
pub use runtime::*;
/// What the engine says when setup asks for something only a score may do.
/// Published so a checker can refuse it in the same words rather than
/// inventing its own.
pub use surface::effects::{MIDI_INPUT_SCOPE_POLICY, TEMPO_SCOPE_POLICY};
use surface::*;
use thread_state::*;
use value::*;

pub use rustel_core::purity::PurePattern;

#[cfg(test)]
mod ownership_cap_tests {
    use super::*;

    fn callback_ids(end: usize) -> HashSet<CallbackId> {
        (0..end).collect()
    }

    #[test]
    fn second_candidate_root_failure_rolls_both_publications_back() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        runtime
            .install_voicings_prebake()
            .expect("voicings bindings");
        runtime
            .evaluate_score(
                "registerVoicings('mine', {'7':['0 4 7']}); chord('C7').voicings('mine')",
                &rustel_transpiler::TranspileOptions::default(),
            )
            .expect("last-good graph");
        let notes = |runtime: &JsRuntime| {
            runtime
                .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("query active graph")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>()
        };
        let before = notes(&runtime);

        runtime.begin_slider_candidate().expect("slider candidate");
        runtime
            .begin_voicing_candidate()
            .expect("voicing candidate");
        runtime
            .eval("registerVoicings('mine', {'7':['0 3 7']})")
            .expect("mutate only candidate registry");
        let error = runtime
            .commit_score_candidates_with_hook(|| Err("injected second publication".into()))
            .expect_err("injected publication failure was accepted");
        assert!(error.contains("injected second publication"), "{error}");
        assert_eq!(
            notes(&runtime),
            before,
            "rollback left the active graph reading the candidate registry"
        );
        with_ctx(&runtime.ctx, |ctx| {
            let same: Function = ctx.eval("(a, b) => a === b").expect("identity helper");
            for sets in [host_slider_sets(&ctx), host_voicing_sets(&ctx)] {
                let sets = sets.expect("private candidate roots");
                let active: rquickjs::Value = sets.get(0).expect("active root");
                let candidate: rquickjs::Value = sets.get(1).expect("candidate root");
                assert!(
                    same.call::<_, bool>((active, candidate))
                        .expect("compare roots"),
                    "rollback left distinct active/candidate roots"
                );
            }
        });
    }

    #[test]
    fn capped_exclusion_planner_accepts_exact_and_refuses_plus_one_atomically() {
        let exact = callback_ids(MAX_OWNERSHIP_EXCLUSIONS);
        let exact_before = exact.clone();
        let planned = plan_capped_exclusions(&[&exact], &HashSet::new())
            .expect("the exact ownership cap must be accepted");
        assert_eq!(planned.len(), MAX_OWNERSHIP_EXCLUSIONS);
        assert_eq!(exact, exact_before, "accepted planning mutated its input");

        let over = callback_ids(MAX_OWNERSHIP_EXCLUSIONS + 1);
        let over_before = over.clone();
        assert!(matches!(
            plan_capped_exclusions(&[&over], &HashSet::new()),
            Err(OwnershipSetError::Limit(
                rustel_core::QueryLimit::StepwiseExpansion {
                    operation: STEPALT_OWNERSHIP_OPERATION,
                    minimum_entries: 16_385,
                    limit: 16_384,
                }
            ))
        ));
        assert_eq!(over, over_before, "refused planning mutated its input");
    }

    #[test]
    fn capped_exclusion_planner_applies_owner_override_before_counting() {
        let exact = callback_ids(MAX_OWNERSHIP_EXCLUSIONS);
        let extra = HashSet::from([MAX_OWNERSHIP_EXCLUSIONS]);
        let protected = HashSet::from([0]);

        for sets in [[&exact, &extra], [&extra, &exact]] {
            let planned = plan_capped_exclusions(&sets, &protected)
                .expect("one explicit owner frees the cap+1 slot");
            assert_eq!(planned.len(), MAX_OWNERSHIP_EXCLUSIONS);
            assert!(!planned.contains(&0));
            assert!(planned.contains(&MAX_OWNERSHIP_EXCLUSIONS));
        }
    }

    #[test]
    fn sidecar_batch_plans_all_owners_before_capping_exclusions() {
        fn exclusions(ids: HashSet<CallbackId>) -> Sidecar<'static> {
            Sidecar {
                ids: Vec::new(),
                cells: Vec::new(),
                excluded_frame_ids: Rc::new(ids),
            }
        }
        fn owner(id: CallbackId) -> Sidecar<'static> {
            Sidecar {
                ids: vec![id],
                cells: Vec::new(),
                excluded_frame_ids: Rc::new(HashSet::new()),
            }
        }

        for owner_first in [false, true] {
            let exact = exclusions(callback_ids(MAX_OWNERSHIP_EXCLUSIONS));
            let extra = exclusions(HashSet::from([MAX_OWNERSHIP_EXCLUSIONS]));
            let owner = owner(0);
            let batch = if owner_first {
                [owner, exact, extra]
            } else {
                [exact, extra, owner]
            };
            let merged = Sidecar::merge_all(batch)
                .expect("the complete batch owner set must override before the cap check");
            assert_eq!(merged.excluded_frame_ids.len(), MAX_OWNERSHIP_EXCLUSIONS);
            assert!(!merged.excluded_frame_ids.contains(&0));
            assert!(
                merged
                    .excluded_frame_ids
                    .contains(&MAX_OWNERSHIP_EXCLUSIONS)
            );
        }
    }

    #[test]
    fn indexed_callback_output_harvests_owner_last_at_the_exact_cap() {
        const BASE: CallbackId = 1_000_000;
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        with_ctx(&runtime.ctx, |ctx| -> Result<(), String> {
            let wrapper = |ids: Vec<CallbackId>, excluded: HashSet<CallbackId>| {
                rquickjs::Class::instance(
                    ctx.clone(),
                    NativePatternWrapper {
                        pattern: rustel_core::silence(),
                        native_query: None,
                        ids,
                        cells: Vec::new(),
                        explicit_ownership_complete: false,
                        excluded_frame_ids: Rc::new(excluded),
                    },
                )
                .map_err(|error| error.to_string())
            };
            let exact = (BASE..BASE + MAX_OWNERSHIP_EXCLUSIONS).collect();
            let results = rquickjs::Array::new(ctx.clone()).map_err(|error| error.to_string())?;
            results
                .set(0, wrapper(Vec::new(), exact)?)
                .map_err(|error| error.to_string())?;
            results
                .set(
                    1,
                    wrapper(Vec::new(), HashSet::from([BASE + MAX_OWNERSHIP_EXCLUSIONS]))?,
                )
                .map_err(|error| error.to_string())?;
            results
                .set(2, wrapper(vec![BASE], HashSet::new())?)
                .map_err(|error| error.to_string())?;
            let factory: Function = ctx
                .eval("(results => { let i = 0; return () => results[i++]; })")
                .map_err(|error| error.to_string())?;
            let callback: Function = factory
                .call((results,))
                .map_err(|error: rquickjs::Error| error.to_string())?;

            let frame = BridgeFrame::new(runtime.ids.clone());
            let _scope = BridgeScope::push(&frame);
            let callback_id = frame.next_id();
            frame.pending.borrow_mut().push((callback_id, callback));
            let output = runtime.call_pattern_indexed_batch(
                callback_id,
                vec![
                    (rustel_core::silence(), 0),
                    (rustel_core::silence(), 1),
                    (rustel_core::silence(), 2),
                ],
            )?;
            assert_eq!(output.len(), 3);
            let suppressed = frame.suppressed.borrow();
            assert_eq!(suppressed.len(), MAX_OWNERSHIP_EXCLUSIONS);
            assert!(!suppressed.contains(&BASE));
            assert!(suppressed.contains(&(BASE + MAX_OWNERSHIP_EXCLUSIONS)));
            Ok(())
        })
        .expect("owner-last indexed callback output must reconcile as one batch");
    }

    /// Install a global JavaScript function `name` that records the typed
    /// ownership refusal (`ownership_limit()`) and throws, as a capped harvest
    /// does.
    pub(crate) fn install_ownership_refusal(runtime: &JsRuntime, name: &str) {
        with_ctx(&runtime.ctx, |ctx| {
            let refuse = Function::new(ctx.clone(), || -> rquickjs::Result<()> {
                Err(ownership_set_error_to_js(OwnershipSetError::Limit(
                    ownership_limit(),
                )))
            })
            .expect("test refusal function");
            ctx.globals()
                .set(name, refuse)
                .expect("install test refusal function");
        });
    }

    /// The active graph is the typed ownership refusal `ownership_limit()`
    /// records, not a plain message and not a score.
    pub(crate) fn assert_active_is_the_ownership_refusal(runtime: &JsRuntime) {
        assert!(matches!(
            runtime.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE),
            Err(QueryError::Limit(
                rustel_core::QueryLimit::StepwiseExpansion {
                    operation: STEPALT_OWNERSHIP_OPERATION,
                    minimum_entries: 16_385,
                    limit: 16_384,
                }
            ))
        ));
    }

    #[test]
    fn scalar_transport_publishes_the_typed_ownership_refusal_graph() {
        let runtime = JsRuntime::new().expect("runtime");
        install_ownership_refusal(&runtime, "__ownership_cap_test");
        let _settings = runtime.core_settings.bind();
        runtime
            .execute_score(
                "__ownership_cap_test()",
                &rustel_transpiler::LineMap::default(),
                false,
            )
            .expect("scalar ownership refusal becomes a durable graph");
        assert_active_is_the_ownership_refusal(&runtime);
        runtime.clear_active();
    }

    /// An ownership refusal keeps its TYPE when an eager transformer callback
    /// throws later in the same evaluation: the throw is an ordinary exception,
    /// and the recorded refusal outranks it, exactly as it outranks a score's own
    /// `throw`. The refusal graph installs; a plain message would not.
    #[test]
    fn an_eager_callback_throw_keeps_the_ownership_refusal_typed() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        install_ownership_refusal(&runtime, "__ownership_cap_test");
        let _settings = runtime.core_settings.bind();
        runtime
            .execute_score(
                "try { __ownership_cap_test(); } catch (error) {}\n\
             s('bd sd').every(2, x => { globalThis.__eagerRan = 1; throw new Error('eager'); })",
                &rustel_transpiler::LineMap::default(),
                false,
            )
            .expect("the ownership refusal becomes a durable graph");
        assert_eq!(
            runtime.get_number("__eagerRan"),
            Some(1.0),
            "vacuous: the eager callback never ran"
        );
        assert_active_is_the_ownership_refusal(&runtime);
        runtime.clear_active();
    }

    #[test]
    fn slider_host_update_is_private_query_time_and_generation_bounded() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        let options = rustel_transpiler::TranspileOptions::default();

        let output = runtime
            .evaluate_score("slider(.25, 0, 1, .05)", &options)
            .expect("initial slider score");
        let id = output
            .widgets
            .iter()
            .find(|widget| widget.widget_type == "slider")
            .expect("slider widget")
            .id
            .clone();
        let value = |runtime: &JsRuntime| {
            runtime
                .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("slider query")[0]
                .value
                .as_f64()
                .expect("numeric slider")
        };
        assert_eq!(value(&runtime), 0.25);
        assert_eq!(runtime.slider_value(&id).expect("host read"), Some(0.25));
        assert!(runtime.set_slider_value(&id, 0.75).expect("host update"));
        assert_eq!(value(&runtime), 0.75);
        assert_eq!(runtime.slider_value(&id).expect("updated read"), Some(0.75));

        runtime
            .eval("delete globalThis.sliderValues; globalThis.sliderValues = new Proxy({}, { set() { throw new Error('redirected') } })")
            .expect("replace compatibility global");
        assert!(
            runtime
                .set_slider_value(&id, 0.5)
                .expect("private host update")
        );
        assert_eq!(value(&runtime), 0.5);
        assert!(
            !runtime
                .set_slider_value("999:1000", 0.5)
                .expect("unknown id is ordinary")
        );
        assert!(runtime.set_slider_value(&id, f64::NAN).is_err());
        assert!(
            runtime
                .evaluate_score(
                    "slider(.9, 0, 1); (() => { throw new Error('reject') })()",
                    &options,
                )
                .is_err()
        );
        assert_eq!(
            value(&runtime),
            0.5,
            "a failed candidate changed the active slider cell"
        );
        runtime.snapshot_active_as_last_good();
        runtime
            .evaluate_score("slider(.8, 0, 1, .05)", &options)
            .expect("replacement candidate");
        assert_eq!(value(&runtime), 0.8);
        runtime
            .restore_last_good_active()
            .expect("restore last-good slider graph and cells");
        assert_eq!(value(&runtime), 0.5);

        for padding in 1..32 {
            let source = format!("{}slider(.4, 0, 1)", " ".repeat(padding));
            runtime
                .evaluate_score(&source, &options)
                .expect("replacement slider score");
            let count = with_ctx(&runtime.ctx, |ctx| {
                let sets = host_slider_sets(&ctx).expect("slider sets");
                let active: rquickjs::Object = sets.get(0).expect("active sliders");
                active
                    .props::<String, rquickjs::Value>()
                    .filter(Result::is_ok)
                    .count()
            });
            assert_eq!(count, 1, "slider ids accumulated across replacements");
        }
        runtime.clear_active();
    }
}

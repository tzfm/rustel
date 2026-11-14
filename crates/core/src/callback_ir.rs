//! Bounded native execution plans for proven pattern-transform callbacks.
//!
//! A program is a straight-line sequence of existing [`FunctionRef`]
//! transforms. Construction accepts only transforms that already carry a
//! native purity proof, so the interpreter cannot enter a callback host or
//! invent a second implementation of a score operation.
//!
//! JavaScript lowering and fallback policy belong to the runtime that owns the
//! original callable. This module only owns the immutable, `Send + Sync`
//! program, the standings its runtime issued, and the bounded interpreter.
//! Instructions describe reusable native operations. Complete builder chains
//! and hard-coded musical arguments do not belong in this layer.

use std::cell::RefCell;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Pattern;
use crate::purity::PurePattern;
use crate::value::FunctionRef;

/// Invalidates cached programs if the instruction contract changes.
pub const PATTERN_TRANSFORM_IR_VERSION: u16 = 13;

/// Maximum work one callback-IR invocation may perform.
///
/// The initial IR has no branches or loops, so this is both a storage bound
/// and the exact worst-case dispatch count.
pub const MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS: usize = 64;

/// One type-checked operation in a [`PatternTransformProgram`].
#[derive(Clone)]
pub enum PatternTransformInstruction {
    /// Apply an existing native pattern transformer to the current value.
    Apply(FunctionRef),
    /// Apply a registered combinator to the current value with literal
    /// leading arguments, as the JavaScript method call passes them. One
    /// argument may hold the one lowered callback in the registration's
    /// function slot.
    ///
    /// Executing delegates to
    /// [`Registration::call`](crate::register::Registration::call), so the
    /// program builds exactly the graph the method call builds, including the
    /// patternified general path a patterned argument takes. This
    /// instruction runs through its ordinary view only. A call that keeps
    /// the operand pure is its own pure view. Any other call is impure by
    /// construction - the same call in JavaScript is impure the same way -
    /// and its pure view wraps that result as host-built.
    ApplyRegistered {
        registration: Box<crate::register::Registration>,
        arguments: Vec<Pattern>,
    },
}

impl fmt::Debug for PatternTransformInstruction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Apply(function) => formatter.debug_tuple("Apply").field(function).finish(),
            Self::ApplyRegistered {
                registration,
                arguments,
            } => formatter
                .debug_struct("ApplyRegistered")
                .field("registration", &registration.names)
                .field("arguments", &arguments.len())
                .finish(),
        }
    }
}

impl PartialEq for PatternTransformInstruction {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Apply(left), Self::Apply(right)) => left == right,
            (
                Self::ApplyRegistered {
                    registration: left,
                    arguments: left_arguments,
                },
                Self::ApplyRegistered {
                    registration: right,
                    arguments: right_arguments,
                },
            ) => {
                left.same_definition(right)
                    && left_arguments.len() == right_arguments.len()
                    && left_arguments
                        .iter()
                        .zip(right_arguments)
                        .all(|(left, right)| left.same_construction(right))
            }
            _ => false,
        }
    }
}

/// Whether an [`PatternTransformInstruction::ApplyRegistered`] instruction is
/// one the interpreter can run: a native-body registration called with the
/// number of leading arguments its method call passes, every argument a
/// pure literal pattern, and at most the one lowered callback, in the
/// registration's function slot.
fn compilable_registered_arguments(
    registration: &crate::register::Registration,
    arguments: &[Pattern],
) -> bool {
    use crate::register::CombinatorFn;
    if !matches!(registration.func, CombinatorFn::Native(_)) {
        return false;
    }
    if arguments.len() != registration.arity.saturating_sub(1) {
        return false;
    }
    let mut functions = 0usize;
    for argument in arguments {
        match argument.as_pure() {
            // A literal scalar or string carries nothing the host owns.
            Some(value) if !value.has_js_function() => {}
            // The function slot may hold exactly one callback with its
            // compiled program, whose host owns the choice between the
            // program and the callback.
            Some(crate::Value::Function(function)) if function.carries_pattern_program() => {
                functions += 1;
            }
            // A patterned literal such as mini-notation holds scalar values
            // across many wholes, so it reaches no callback.
            None if argument.is_pure() && argument.reachable_callbacks().is_empty() => {}
            _ => return false,
        }
    }
    functions <= usize::from(registration.takes_function())
}

/// Why a proposed native callback program was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternTransformBuildError {
    /// The program would exceed its fixed dispatch bound.
    InstructionLimit { requested: usize, limit: usize },
    /// This instruction could reach JavaScript or cannot preserve a pure
    /// input's purity proof.
    NonNativeInstruction { index: usize },
    /// Reserving the bounded instruction storage failed.
    Allocation,
}

impl fmt::Display for PatternTransformBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstructionLimit { requested, limit } => write!(
                formatter,
                "callback IR needs {requested} instructions, past the limit of {limit}"
            ),
            Self::NonNativeInstruction { index } => write!(
                formatter,
                "callback IR instruction {index} does not have a native purity proof"
            ),
            Self::Allocation => formatter.write_str("callback IR instruction allocation failed"),
        }
    }
}

impl std::error::Error for PatternTransformBuildError {}

/// Whether one fact that compiled programs read still holds.
///
/// The runtime that compiles a program hands it the standing of each fact
/// the program reads, such as a method slot or the callee of mini-notation.
/// The runtime marks a standing stale before the fact can change. A stale
/// standing never becomes current again, and a program that holds one no
/// longer stands for its callback.
#[derive(Debug, Default)]
pub struct Standing {
    stale: AtomicBool,
}

impl Standing {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_stale(&self) -> bool {
        self.stale.load(Ordering::Acquire)
    }

    /// Mark the fact as about to change. Returns whether a program still
    /// holds this standing, besides the caller's own handle.
    pub fn mark_stale(self: &Arc<Self>) -> bool {
        self.stale.store(true, Ordering::Release);
        Arc::strong_count(self) > 1
    }
}

/// Two standings are the same only when they are the same fact.
impl PartialEq for Standing {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

thread_local! {
    /// The standings each kept construction or table build in progress on
    /// this thread has read so far, innermost last.
    static READS: RefCell<Vec<Vec<Arc<Standing>>>> = const { RefCell::new(Vec::new()) };
}

/// Note that the construction or table build in progress read `standings`:
/// a program it applied through its pure view, or a kept construction or
/// table it used.
pub(crate) fn note_read(standings: &[Arc<Standing>]) {
    if standings.is_empty() {
        return;
    }
    READS.with(|reads| {
        if let Some(innermost) = reads.borrow_mut().last_mut() {
            innermost.extend(standings.iter().cloned());
        }
    });
}

/// Run `build` and return what it built with every standing it read. The
/// enclosing construction or build reads them too.
///
/// A kept construction or table answers only while all of them hold: what a
/// stale program built is no longer what its callback builds.
pub(crate) fn recording_reads<R>(build: impl FnOnce() -> R) -> (R, Box<[Arc<Standing>]>) {
    /// Ends the recording, also on unwind.
    struct Recording;
    impl Drop for Recording {
        fn drop(&mut self) {
            let _ = READS.try_with(|reads| reads.borrow_mut().pop());
        }
    }
    READS.with(|reads| reads.borrow_mut().push(Vec::new()));
    let recording = Recording;
    let built = build();
    let mut read = READS.with(|reads| {
        std::mem::take(
            reads
                .borrow_mut()
                .last_mut()
                .expect("the recording in progress"),
        )
    });
    drop(recording);
    read.sort_unstable_by_key(Arc::as_ptr);
    read.dedup_by(|a, b| Arc::ptr_eq(a, b));
    note_read(&read);
    (built, read.into_boxed_slice())
}

/// Whether every standing in `read` still holds.
pub(crate) fn reads_hold(read: &[Arc<Standing>]) -> bool {
    read.iter().all(|standing| !standing.is_stale())
}

/// What one answer that a result cache keeps has read: the standing of
/// every program the answer applied, directly or through a kept construction
/// or table.
///
/// A kept answer stands only while all of them hold, as a kept construction
/// does. A cache outside this crate records the reads with each answer and
/// reuses the answer only while they hold.
#[derive(Clone, Debug, Default)]
pub struct AnswerReads(Box<[Arc<Standing>]>);

impl AnswerReads {
    /// Run `answer` and return what it answered with every standing it read.
    /// The construction, build or answer in progress around it reads them
    /// too.
    pub fn record<R>(answer: impl FnOnce() -> R) -> (R, Self) {
        let (answered, read) = recording_reads(answer);
        (answered, Self(read))
    }

    /// Whether every standing the answer read still holds.
    pub fn hold(&self) -> bool {
        reads_hold(&self.0)
    }

    /// Note that the construction, build or answer in progress reuses the
    /// kept answer, so it reads what the answer read.
    pub fn reuse(&self) {
        note_read(&self.0);
    }
}

/// Immutable, bounded program for a unary `Pattern -> Pattern` callback.
///
/// An empty program is the identity callback. Programs are compiled outside a
/// query and can then be shared between producer threads like the pattern
/// graph and native [`FunctionRef`] values they contain.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternTransformProgram {
    instructions: Box<[PatternTransformInstruction]>,
    /// The standing of every fact the program reads. The identity program
    /// reads nothing.
    standings: Box<[Arc<Standing>]>,
}

impl PatternTransformProgram {
    /// Validate and own a sequence of already-lowered instructions.
    pub fn new(
        instructions: impl IntoIterator<Item = PatternTransformInstruction>,
    ) -> Result<Self, PatternTransformBuildError> {
        let mut owned = Vec::new();
        for instruction in instructions {
            if owned.len() == MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS {
                return Err(PatternTransformBuildError::InstructionLimit {
                    requested: owned.len().saturating_add(1),
                    limit: MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS,
                });
            }
            let native = match &instruction {
                PatternTransformInstruction::Apply(function) => function.is_native(),
                PatternTransformInstruction::ApplyRegistered {
                    registration,
                    arguments,
                } => compilable_registered_arguments(registration, arguments),
            };
            if !native {
                return Err(PatternTransformBuildError::NonNativeInstruction {
                    index: owned.len(),
                });
            }
            if owned.len() == owned.capacity() {
                owned
                    .try_reserve(1)
                    .map_err(|_| PatternTransformBuildError::Allocation)?;
            }
            owned.push(instruction);
        }
        Ok(Self {
            instructions: owned.into_boxed_slice(),
            standings: Box::new([]),
        })
    }

    /// The same program, holding the standings its runtime issued for it.
    pub fn with_standings(mut self, standings: impl IntoIterator<Item = Arc<Standing>>) -> Self {
        self.standings = standings.into_iter().collect();
        self
    }

    /// Whether every fact the program reads still holds, so that the program
    /// stands for its callback.
    pub fn is_current(&self) -> bool {
        reads_hold(&self.standings)
    }

    /// The standings of the facts the program reads.
    pub(crate) fn standings(&self) -> &[Arc<Standing>] {
        &self.standings
    }

    /// Every script callback a function holding this program must keep alive:
    /// the callbacks of the function values its registered calls take as
    /// arguments, which apply per query and can go stale while this program
    /// stands. See [`crate::value::FunctionRef::retained_callbacks`].
    pub(crate) fn retained_callback_ids(&self) -> Vec<crate::CallbackId> {
        let mut ids = Vec::new();
        for instruction in &self.instructions[..] {
            if let PatternTransformInstruction::ApplyRegistered { arguments, .. } = instruction {
                for argument in arguments.iter() {
                    if let Some(crate::Value::Function(function)) = argument.as_pure() {
                        ids.extend(function.retained_callbacks());
                    }
                }
            }
        }
        ids
    }

    /// Whether the program reads any fact, so that it can stop standing for
    /// its callback.
    pub fn can_go_stale(&self) -> bool {
        !self.standings.is_empty()
    }

    pub fn version(&self) -> u16 {
        PATTERN_TRANSFORM_IR_VERSION
    }

    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    pub fn instructions(&self) -> &[PatternTransformInstruction] {
        &self.instructions
    }

    /// Execute against an arbitrary native pattern graph.
    ///
    /// Each instruction runs unbound, as a JavaScript method chain builds its
    /// intermediate patterns. The caller selects the runtime settings for the
    /// whole program and binds them to the final result. A pending query
    /// error stops nothing, because the method chain does not look at it
    /// either.
    pub fn apply(&self, mut pattern: Pattern) -> Pattern {
        for instruction in &self.instructions {
            pattern = match instruction {
                PatternTransformInstruction::Apply(function) => function.apply_unbound(pattern),
                PatternTransformInstruction::ApplyRegistered {
                    registration,
                    arguments,
                } => registration.call(arguments, pattern),
            };
        }
        pattern
    }

    /// Execute while retaining the representation-level purity proof.
    pub fn apply_pure(&self, mut pattern: PurePattern) -> PurePattern {
        for instruction in &self.instructions {
            pattern = match instruction {
                PatternTransformInstruction::Apply(function) => function
                    .apply_pure_unbound(pattern)
                    .expect("validated callback IR instruction lost its native purity proof"),
                // The pure view runs the ordinary view. A call that kept
                // the operand pure answers as its graph does. Any other is
                // impure by construction, as its JavaScript method call is,
                // and is wrapped as host-built, the way the host arm of a
                // stale lowered callback does.
                PatternTransformInstruction::ApplyRegistered {
                    registration,
                    arguments,
                } => {
                    let built = registration.call(arguments, pattern.pattern().clone());
                    match built.as_pure_pattern() {
                        Some(pure) => pure,
                        None => PurePattern::host_built(built),
                    }
                }
            };
        }
        pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use rustel_fraction::Fraction;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn increment() -> FunctionRef {
        crate::native_function!("increment", |pattern| pattern
            .fmap(|value| { Value::F64(value.as_f64().unwrap_or(f64::NAN) + 1.0) }))
    }

    fn apply(function: FunctionRef) -> PatternTransformInstruction {
        PatternTransformInstruction::Apply(function)
    }

    #[test]
    fn empty_program_is_identity_for_both_pattern_views() {
        let program = PatternTransformProgram::new([]).unwrap();
        assert_eq!(program.version(), PATTERN_TRANSFORM_IR_VERSION);
        assert!(program.is_empty());

        let pattern = crate::pure(Value::F64(7.0));
        let haps = program
            .apply(pattern.clone())
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps[0].value, Value::F64(7.0));

        let pure = PurePattern::assert_pure(pattern);
        let haps = program
            .apply_pure(pure)
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps[0].value, Value::F64(7.0));
    }

    #[test]
    fn instructions_execute_once_in_source_order() {
        let program = PatternTransformProgram::new([apply(increment()), apply(increment())])
            .expect("two native instructions");
        assert_eq!(program.len(), 2);
        let haps = program
            .apply(crate::pure(Value::F64(3.0)))
            .query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps[0].value, Value::F64(5.0));
    }

    #[test]
    fn pure_execution_preserves_its_typed_proof() {
        let program = PatternTransformProgram::new([apply(increment())]).unwrap();
        let input = PurePattern::assert_pure(crate::pure(Value::F64(9.0)));
        let output = program.apply_pure(input);
        assert!(output.pattern().is_pure());
        assert_eq!(
            output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(10.0)
        );
    }

    #[test]
    fn a_query_error_stops_no_instruction() {
        let _ = crate::take_query_error();
        let calls = Arc::new(AtomicUsize::new(0));
        let any_calls = calls.clone();
        let failing = FunctionRef::native(
            Some("failing"),
            Arc::new(|pattern| pattern),
            Arc::new(move |pattern| {
                any_calls.fetch_add(1, Ordering::SeqCst);
                crate::signal_query_error(|| "expected callback IR failure".into());
                pattern
            }),
        );
        let later_calls = calls.clone();
        let later = FunctionRef::native(
            Some("later"),
            Arc::new(|pattern| pattern),
            Arc::new(move |pattern| {
                later_calls.fetch_add(100, Ordering::SeqCst);
                pattern
            }),
        );
        let program = PatternTransformProgram::new([apply(failing), apply(later)]).unwrap();

        let _ = program.apply(crate::pure(Value::F64(1.0)));

        // A method chain calls every method, whatever the one before it
        // signaled.
        assert_eq!(calls.load(Ordering::SeqCst), 101);
        assert_eq!(
            crate::take_query_error().as_deref(),
            Some("expected callback IR failure")
        );
    }

    #[test]
    fn a_pending_query_error_from_elsewhere_runs_every_instruction() {
        let _ = crate::take_query_error();
        let program = PatternTransformProgram::new([apply(increment()), apply(increment())])
            .expect("two native instructions");
        crate::signal_query_error(|| "outer failure".into());

        let any = program.apply(crate::pure(Value::F64(1.0)));
        let pure = program.apply_pure(PurePattern::assert_pure(crate::pure(Value::F64(1.0))));

        assert_eq!(crate::take_query_error().as_deref(), Some("outer failure"));
        for output in [any, pure.pattern().clone()] {
            assert_eq!(
                output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
                Value::F64(3.0)
            );
        }
    }

    #[test]
    fn kept_constructions_and_tables_answer_only_while_the_programs_they_applied_stand() {
        let standing = Standing::new();
        let program = PatternTransformProgram::new([apply(increment())])
            .unwrap()
            .with_standings([standing.clone()]);
        let lowered = FunctionRef::lowered_js(7, program);
        let builds = Arc::new(AtomicUsize::new(0));
        let counted = builds.clone();
        // A carrier whose values name the patterns a lowered callback
        // builds, as a lazily applied callback does.
        let graph = crate::fastcat(vec![
            crate::pure(Value::F64(1.0)),
            crate::pure(Value::F64(2.0)),
        ])
        .fmap_to_pure_pattern(move |value| {
            counted.fetch_add(1, Ordering::SeqCst);
            lowered
                .apply_pure(PurePattern::assert_pure(crate::pure(value.clone())))
                .expect("a pure view")
        })
        .inner_join();
        assert!(graph.is_pure());
        let span = crate::TimeSpan::new(Fraction::ZERO, Fraction::int(4));
        let answer = graph.query_arc(Fraction::ZERO, Fraction::int(4));
        assert_eq!(answer.len(), 8);
        assert!(graph.answers_from_tables(span));
        let built = builds.load(Ordering::SeqCst);
        let _ = graph.query_arc(Fraction::ZERO, Fraction::int(4));
        assert_eq!(builds.load(Ordering::SeqCst), built, "the memo keeps both");

        standing.mark_stale();
        assert!(!graph.answers_from_tables(span));
        // With no host, the stale program cannot run its callback, so the
        // graph builds again and answers nothing rather than the program's
        // output.
        let _ = crate::take_query_error();
        assert!(graph.query_arc(Fraction::ZERO, Fraction::int(4)).is_empty());
        assert!(builds.load(Ordering::SeqCst) > built);
    }

    #[test]
    fn javascript_instruction_is_rejected_before_execution() {
        let error = PatternTransformProgram::new([apply(FunctionRef::js(41))]).unwrap_err();
        assert_eq!(
            error,
            PatternTransformBuildError::NonNativeInstruction { index: 0 }
        );
    }

    /// The `when` registration, as a lowered callback's compiled method call
    /// holds it.
    fn when_registration() -> crate::register::Registration {
        crate::register::default_registry()
            .get("when")
            .expect("when is registered")
            .clone()
    }

    fn apply_registered(
        registration: crate::register::Registration,
        arguments: Vec<Pattern>,
    ) -> PatternTransformInstruction {
        PatternTransformInstruction::ApplyRegistered {
            registration: Box::new(registration),
            arguments,
        }
    }

    /// A two-whole condition, as a mini-notation literal lowers to.
    fn two_whole_condition() -> Pattern {
        crate::fastcat(vec![
            crate::pure(Value::Bool(true)),
            crate::pure(Value::Bool(false)),
        ])
    }

    #[test]
    fn a_registered_call_with_a_lowered_callback_is_compilable_and_impure() {
        let inner = FunctionRef::lowered_js(
            9,
            PatternTransformProgram::new([apply(increment())]).expect("a native program"),
        );
        let instruction = apply_registered(
            when_registration(),
            vec![
                two_whole_condition(),
                crate::pure(Value::Function(Box::new(inner))),
            ],
        );
        let program = PatternTransformProgram::new([instruction]).expect("compilable");
        // The patternified general path joins the two wholes, so one cycle
        // holds two haps of the operand the condition chose.
        let output = program.apply(crate::pure(Value::F64(1.0)));
        let haps = output.query_arc(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2, "both wholes answer");
        // The pure view runs the same ordinary view, host-built.
        let pure = program.apply_pure(PurePattern::assert_pure(crate::pure(Value::F64(1.0))));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE).len(),
            2,
            "the pure view answers the same"
        );
    }

    #[test]
    fn a_registered_call_refuses_every_uncompilable_shape() {
        let lowered = FunctionRef::lowered_js(4, PatternTransformProgram::new([]).unwrap());
        let function = crate::pure(Value::Function(Box::new(lowered)));
        let condition = two_whole_condition();
        // The wrong number of leading arguments.
        for arguments in [
            vec![],
            vec![condition.clone()],
            vec![condition.clone(), function.clone(), condition.clone()],
        ] {
            let error =
                PatternTransformProgram::new([apply_registered(when_registration(), arguments)])
                    .unwrap_err();
            assert_eq!(
                error,
                PatternTransformBuildError::NonNativeInstruction { index: 0 },
                "the argument count must be the method call's"
            );
        }
        // A JavaScript callback in the function slot, and a JS-owned value in
        // any slot.
        for arguments in [
            vec![
                condition.clone(),
                crate::pure(Value::Function(Box::new(FunctionRef::js(7)))),
            ],
            vec![
                crate::pure(Value::JsValue(crate::value::JsValueRef::new(7, false))),
                function.clone(),
            ],
        ] {
            let error =
                PatternTransformProgram::new([apply_registered(when_registration(), arguments)])
                    .unwrap_err();
            assert_eq!(
                error,
                PatternTransformBuildError::NonNativeInstruction { index: 0 },
                "only the lowered callback may cross into the program"
            );
        }
        // A non-pure argument.
        let error = PatternTransformProgram::new([apply_registered(
            when_registration(),
            vec![crate::pure(Value::F64(1.0)).fmap_js(7), function],
        )])
        .unwrap_err();
        assert_eq!(
            error,
            PatternTransformBuildError::NonNativeInstruction { index: 0 }
        );
    }

    #[test]
    fn instruction_limit_is_checked_before_materializing_an_unbounded_program() {
        let function = increment();
        let instructions = std::iter::repeat_with(|| apply(function.clone()))
            .take(MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS + 1);
        let error = PatternTransformProgram::new(instructions).unwrap_err();
        assert_eq!(
            error,
            PatternTransformBuildError::InstructionLimit {
                requested: MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS + 1,
                limit: MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS,
            }
        );
    }

    #[test]
    fn a_program_stands_for_its_callback_until_a_standing_it_holds_goes_stale() {
        let program = PatternTransformProgram::new([apply(increment())]).unwrap();
        assert!(program.is_current(), "a program that reads nothing");
        assert!(!program.can_go_stale());
        let read = Standing::new();
        let other = Standing::new();
        let issued = program.with_standings([read.clone()]);
        assert!(issued.is_current());
        assert!(issued.can_go_stale());
        assert!(!other.mark_stale(), "no program holds it");
        assert!(issued.is_current());
        assert!(read.mark_stale(), "the program holds it");
        assert!(!issued.is_current());
        assert!(
            !issued.clone().is_current(),
            "a clone holds the same standing"
        );
    }

    #[test]
    fn a_recording_keeps_each_read_once_and_hands_it_outward() {
        let first = Standing::new();
        let second = Standing::new();
        let ((), outer) = recording_reads(|| {
            note_read(std::slice::from_ref(&first));
            let ((), inner) = recording_reads(|| {
                note_read(&[second.clone(), second.clone()]);
            });
            assert_eq!(inner.len(), 1);
            assert!(Arc::ptr_eq(&inner[0], &second));
        });
        assert_eq!(outer.len(), 2);
        assert!(reads_hold(&outer));
        second.mark_stale();
        assert!(!reads_hold(&outer));
        // Nothing records outside a recording.
        note_read(&[first]);
        READS.with(|reads| assert!(reads.borrow().is_empty()));
    }

    #[test]
    fn a_kept_answer_holds_while_what_it_read_stands_and_its_reuse_is_a_read() {
        let standing = Standing::new();
        let (answer, reads) = AnswerReads::record(|| {
            note_read(std::slice::from_ref(&standing));
            7
        });
        assert_eq!(answer, 7);
        assert!(reads.hold());
        let ((), around) = recording_reads(|| reads.reuse());
        assert_eq!(around.len(), 1);
        assert!(Arc::ptr_eq(&around[0], &standing));
        standing.mark_stale();
        assert!(!reads.hold());
        let ((), nothing) = AnswerReads::record(|| ());
        assert!(nothing.hold());
    }

    #[test]
    fn steps_leave_intermediate_handles_unforced() {
        let unforced = Arc::new(AtomicUsize::new(0));
        let seen = unforced.clone();
        let observe = FunctionRef::native(
            Some("observe"),
            Arc::new(|pattern| pattern),
            Arc::new(move |pattern: Pattern| {
                if pattern.runtime_settings.is_none() {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
                pattern
            }),
        );
        let program = PatternTransformProgram::new([apply(increment()), apply(observe)]).unwrap();
        let callback = FunctionRef::lowered_js(5, program);
        let settings = crate::settings::RuntimeSettings::default();
        let output = settings.with(|| callback.apply(crate::pure(Value::F64(1.0))));
        assert_eq!(unforced.load(Ordering::SeqCst), 1);
        assert!(output.runtime_settings.is_some());
        assert_eq!(
            output.query_arc(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::F64(2.0)
        );
    }

    #[test]
    fn program_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PatternTransformProgram>();
    }
}

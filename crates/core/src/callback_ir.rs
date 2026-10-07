//! Bounded native execution plans for proven pattern-transform callbacks.
//!
//! This is the first, deliberately narrow callback-IR tier. A program is a
//! straight-line sequence of existing [`FunctionRef`] transforms. Construction
//! accepts only transforms that already carry a native purity proof, so the
//! interpreter cannot enter a callback host or invent a second implementation
//! of a score operation.
//!
//! JavaScript lowering and fallback policy belong to the runtime that owns the
//! original callable. This module only owns the immutable, `Send + Sync`
//! program and its bounded interpreter. Instructions describe reusable native
//! operations; complete builder chains and hard-coded musical arguments do not
//! belong in this layer.

use std::fmt;

use crate::Pattern;
use crate::purity::PurePattern;
use crate::value::FunctionRef;

/// Invalidates cached programs if the instruction contract changes.
pub const PATTERN_TRANSFORM_IR_VERSION: u16 = 11;

/// Maximum work one callback-IR invocation may perform.
///
/// The initial IR has no branches or loops, so this is both a storage bound
/// and the exact worst-case dispatch count.
pub const MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS: usize = 64;

/// One type-checked operation in a [`PatternTransformProgram`].
#[derive(Clone, Debug, PartialEq)]
pub enum PatternTransformInstruction {
    /// Apply an existing native pattern transformer to the current value.
    Apply(FunctionRef),
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

/// Immutable, bounded program for a unary `Pattern -> Pattern` callback.
///
/// An empty program is the identity callback. Programs are compiled outside a
/// query and can then be shared between producer threads like the pattern
/// graph and native [`FunctionRef`] values they contain.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternTransformProgram {
    instructions: Box<[PatternTransformInstruction]>,
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
        })
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
    pub fn apply(&self, mut pattern: Pattern) -> Pattern {
        for instruction in &self.instructions {
            if crate::query_error_pending() {
                break;
            }
            pattern = match instruction {
                PatternTransformInstruction::Apply(function) => function.apply(pattern),
            };
        }
        pattern
    }

    /// Execute while retaining the representation-level purity proof.
    pub fn apply_pure(&self, mut pattern: PurePattern) -> PurePattern {
        for instruction in &self.instructions {
            if crate::query_error_pending() {
                break;
            }
            pattern = match instruction {
                PatternTransformInstruction::Apply(function) => function
                    .apply_pure(pattern)
                    .expect("validated callback IR instruction lost its native purity proof"),
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
    fn execution_stops_after_the_first_query_error() {
        let _ = crate::take_query_error();
        let calls = Arc::new(AtomicUsize::new(0));
        let any_calls = calls.clone();
        let failing = FunctionRef::native(
            Some("failing"),
            Arc::new(|pattern| {
                crate::signal_query_error(|| "expected callback IR failure".into());
                pattern
            }),
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

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            crate::take_query_error().as_deref(),
            Some("expected callback IR failure")
        );
    }

    #[test]
    fn a_preexisting_query_error_executes_no_instruction() {
        let _ = crate::take_query_error();
        let calls = Arc::new(AtomicUsize::new(0));
        let any_calls = calls.clone();
        let function = FunctionRef::native(
            Some("must-not-run"),
            Arc::new(|pattern| pattern),
            Arc::new(move |pattern| {
                any_calls.fetch_add(1, Ordering::SeqCst);
                pattern
            }),
        );
        let program = PatternTransformProgram::new([apply(function)]).unwrap();
        crate::signal_query_error(|| "outer failure".into());

        let _ = program.apply(crate::pure(Value::F64(1.0)));

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(crate::take_query_error().as_deref(), Some("outer failure"));
    }

    #[test]
    fn javascript_instruction_is_rejected_before_execution() {
        let error = PatternTransformProgram::new([apply(FunctionRef::js(41))]).unwrap_err();
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
    fn program_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PatternTransformProgram>();
    }
}

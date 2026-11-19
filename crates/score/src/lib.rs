//! A score read without a JavaScript parser.
//!
//! [`read`] reads the text of a score made only of bindings, functions,
//! arrows, objects, arrays, numbers, strings, calls, method chains and
//! lanes and widgets. It gives the [`ScoreProgram`] that the JavaScript
//! route reads from the same text after its rewrites, so a host evaluates
//! one program whichever route read it.
//!
//! The reader is fail-closed. A text with any other form reads as `None`,
//! and the JavaScript route owns it, with every error.

mod lane;
mod program;
mod reader;
mod string;

pub use lane::LaneStatement;
pub use program::{
    At, Held, Row, ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement, Written,
};
pub use reader::{CallForm, MAX_NESTING, Read, left_at, read};
pub use string::{DecodeError, decode, take_hex};

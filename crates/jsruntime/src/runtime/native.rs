//! Scores evaluated without JavaScript.
//!
//! A transpiled score made only of bindings, functions, arrows, objects,
//! arrays, mini-notation, numbers, signals, free controls, list
//! constructors, tempo setters, built-in methods and lanes builds its graph
//! here, from the same native operations the JavaScript surface calls, and
//! stages it as [`JsRuntime::execute_score`] does. Every name is proved
//! against the slot installation recorded for it, so a score that replaced
//! one is evaluated by JavaScript. The native tier never reports an error:
//! anything it does not prove, or that would fail, is left to the JavaScript
//! tier, which owns every message.

use super::*;
use crate::surface::{
    BuiltinGlobal, BuiltinMethod, ListKind, ListedNames, MINI_CALLEE, SPECIAL_REGISTERED_METHODS,
    prove_builtin_global, prove_builtin_method, prove_lane_method, prove_mini_callee,
    prove_surface_intrinsics,
};
use rustel_core::callback_ir::{PatternTransformInstruction, PatternTransformProgram};
use rustel_core::controls::{ChainArgument, ControlSpec};
use rustel_core::register::FunctionArgument;
use rustel_core::value::FunctionRef;
use rustel_transpiler::{
    Held, ScoreExpr, ScoreFunction, ScoreProgram, ScoreProperty, ScoreStatement, Written,
};

mod memo;

pub(super) use memo::Memo;

/// A value of a natively evaluated score.
#[derive(Clone)]
enum Native<'p> {
    Pattern(Pattern),
    Number(f64),
    /// A raw string, which the native tier only passes on unread.
    String,
    /// An object's properties, in the order JavaScript enumerates them.
    Object(Rc<[(&'p str, Native<'p>)]>),
    /// An array's elements.
    Array(Rc<[Native<'p>]>),
    /// An arrow of the score, compiled to a pattern transformer.
    Function(FunctionRef),
    /// The parameter of an arrow that is being compiled, after the method
    /// calls its body made on it so far.
    Chain(Chain),
}

impl Native<'_> {
    /// The value as the surface reifies an argument: a pattern as it is, a
    /// number as a pure number. A raw string would go through a string
    /// parser a score may set, and an object or array would keep its
    /// JavaScript identity, so those are left to JavaScript. A transformer
    /// is an argument only where [`Turn::method_arguments`] takes one, and
    /// an arrow's parameter is no value yet.
    fn reified(&self) -> Option<Pattern> {
        match self {
            Self::Pattern(pattern) => Some(pattern.clone()),
            Self::Number(value) => Some(rustel_core::pure(Value::F64(*value))),
            Self::String
            | Self::Object(_)
            | Self::Array(_)
            | Self::Function(_)
            | Self::Chain(_) => None,
        }
    }
}

/// Inline-optimized instruction buffer for an arrow chain, avoiding a heap
/// `Vec` for zero- and one-step arrows (`x => x.fast(2)`).
#[derive(Clone, Default)]
enum ChainSteps {
    #[default]
    Empty,
    One(PatternTransformInstruction),
    Many(Vec<PatternTransformInstruction>),
}

impl ChainSteps {
    fn push(&mut self, step: PatternTransformInstruction) {
        match std::mem::take(self) {
            Self::Empty => *self = Self::One(step),
            Self::One(first) => *self = Self::Many(vec![first, step]),
            Self::Many(mut vec) => {
                vec.push(step);
                *self = Self::Many(vec);
            }
        }
    }
}

/// The method calls an arrow's body made on its parameter, as the steps
/// that apply them to any pattern.
#[derive(Clone)]
struct Chain {
    /// The arrow whose parameter this is, by the order of compilation.
    arrow: usize,
    steps: ChainSteps,
}

/// One call of a control with one argument, as the score writes it.
#[derive(Clone, Copy)]
struct ControlCall<'p> {
    spec: &'static ControlSpec,
    argument: &'p ScoreExpr,
    written: Written,
}

/// Compact lane name avoiding heap `String` allocation for `$:` lanes.
enum LaneName<'p> {
    Named(&'p str),
    AnonDollar(u32),
    Owned(String),
}

impl LaneName<'_> {
    #[inline]
    fn matches_str(&self, target: &str) -> bool {
        match self {
            Self::Named(s) => *s == target,
            Self::Owned(s) => s == target,
            Self::AnonDollar(n) => {
                target
                    .strip_prefix('$')
                    .and_then(|rest| rest.parse::<u32>().ok())
                    == Some(*n)
            }
        }
    }

    #[inline]
    fn matches_anon(&self, n: u32) -> bool {
        match self {
            Self::AnonDollar(m) => *m == n,
            Self::Named(s) => {
                s.strip_prefix('$')
                    .and_then(|rest| rest.parse::<u32>().ok())
                    == Some(n)
            }
            Self::Owned(s) => {
                s.strip_prefix('$')
                    .and_then(|rest| rest.parse::<u32>().ok())
                    == Some(n)
            }
        }
    }
}

/// One playing lane, by the name it is collected under.
struct Lane<'p> {
    name: LaneName<'p>,
    pattern: Pattern,
}

/// Stack-inline lane list avoiding heap allocation for scores with `<= 8` lanes.
struct SmallLanes<'p> {
    inline: [Option<Lane<'p>>; 8],
    len: usize,
    overflow: Vec<Lane<'p>>,
}

impl<'p> Default for SmallLanes<'p> {
    fn default() -> Self {
        Self {
            inline: [const { None }; 8],
            len: 0,
            overflow: Vec::new(),
        }
    }
}

impl<'p> SmallLanes<'p> {
    fn with_capacity(cap: usize) -> Self {
        Self {
            inline: [const { None }; 8],
            len: 0,
            overflow: if cap > 8 {
                Vec::with_capacity(cap)
            } else {
                Vec::new()
            },
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.len == 0 && self.overflow.is_empty()
    }

    #[inline]
    fn has_named(&self, name: &str) -> bool {
        if self.overflow.is_empty() {
            self.inline[..self.len]
                .iter()
                .any(|slot| slot.as_ref().is_some_and(|l| l.name.matches_str(name)))
        } else {
            self.overflow.iter().any(|l| l.name.matches_str(name))
        }
    }

    #[inline]
    fn has_anon(&self, n: u32) -> bool {
        if self.overflow.is_empty() {
            self.inline[..self.len]
                .iter()
                .any(|slot| slot.as_ref().is_some_and(|l| l.name.matches_anon(n)))
        } else {
            self.overflow.iter().any(|l| l.name.matches_anon(n))
        }
    }

    fn push(&mut self, lane: Lane<'p>) {
        if self.overflow.is_empty() && self.len < self.inline.len() {
            self.inline[self.len] = Some(lane);
            self.len += 1;
        } else {
            if self.overflow.is_empty() {
                self.overflow.reserve(16);
                for slot in &mut self.inline[..self.len] {
                    self.overflow.push(slot.take().expect("lane present"));
                }
                self.len = 0;
            }
            self.overflow.push(lane);
        }
    }

    fn into_pattern(mut self) -> Pattern {
        if self.overflow.is_empty() {
            if self.len == 1 {
                return self.inline[0].take().expect("single lane").pattern;
            }
            let mut pats = Vec::with_capacity(self.len);
            for slot in &mut self.inline[..self.len] {
                pats.push(slot.take().expect("lane present").pattern);
            }
            rustel_core::stack_lanes(pats)
        } else if self.overflow.len() == 1 {
            self.overflow.pop().expect("single lane").pattern
        } else {
            rustel_core::stack_lanes(self.overflow.into_iter().map(|l| l.pattern).collect())
        }
    }
}

/// Compact map of top-level score bindings or declared functions keyed by
/// identifier name. Keeps `<= 4` entries inline on the stack (zero heap
/// allocations) and uses injective 64-bit tags (`<= 7` bytes need no `memcmp`)
/// with an open-addressed `u16` index table for larger scopes.
struct SmallNameMap<'p, V> {
    inline: [Option<(u64, &'p str, V)>; 4],
    inline_len: usize,
    entries: Vec<(u64, &'p str, V)>,
    slots: Vec<u16>,
}

impl<V> Default for SmallNameMap<'_, V> {
    fn default() -> Self {
        Self {
            inline: [const { None }; 4],
            inline_len: 0,
            entries: Vec::new(),
            slots: Vec::new(),
        }
    }
}

impl<'p, V> SmallNameMap<'p, V> {
    const LINEAR_LIMIT: usize = 4;

    fn with_capacity(cap: usize) -> Self {
        if cap <= Self::LINEAR_LIMIT {
            return Self::default();
        }
        let len = (cap.saturating_mul(2).max(16)).next_power_of_two();
        Self {
            inline: [const { None }; 4],
            inline_len: 0,
            entries: Vec::with_capacity(cap),
            slots: vec![u16::MAX; len],
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.inline_len == 0 && self.entries.is_empty()
    }

    #[inline(always)]
    fn tag_matches(entry_tag: u64, entry_name: &str, tag: u64, name: &str) -> bool {
        entry_tag == tag && ((tag >> 56) != 0xff || entry_name == name)
    }

    #[inline(always)]
    fn slot_for(tag: u64, mask: usize) -> usize {
        ((tag.wrapping_mul(0x9e37_79b9_7f4a_7c15)) >> 32) as usize & mask
    }

    fn spill_to_heap(&mut self, new_slots_len: usize) {
        self.entries.reserve(self.inline_len + 4);
        for slot in &mut self.inline[..self.inline_len] {
            self.entries.push(slot.take().expect("inline entry"));
        }
        self.inline_len = 0;
        self.grow_slots(new_slots_len);
    }

    fn grow_slots(&mut self, new_len: usize) {
        let mut slots = vec![u16::MAX; new_len];
        let mask = new_len - 1;
        for (idx, &(tag, _, _)) in self.entries.iter().enumerate() {
            let mut slot = Self::slot_for(tag, mask);
            while slots[slot] != u16::MAX {
                slot = (slot + 1) & mask;
            }
            slots[slot] = idx as u16;
        }
        self.slots = slots;
    }

    #[inline]
    fn find_inline(&self, tag: u64, name: &str) -> Option<usize> {
        for (i, slot) in self.inline.iter().enumerate().take(self.inline_len) {
            let (etag, ename, _) = slot.as_ref().expect("inline entry");
            if Self::tag_matches(*etag, ename, tag, name) {
                return Some(i);
            }
        }
        None
    }

    #[inline]
    fn find_heap(&self, tag: u64, name: &str) -> Option<usize> {
        let mask = self.slots.len() - 1;
        let mut slot = Self::slot_for(tag, mask);
        loop {
            let idx = self.slots[slot];
            if idx == u16::MAX {
                return None;
            }
            let (etag, ename, _) = &self.entries[usize::from(idx)];
            if Self::tag_matches(*etag, ename, tag, name) {
                return Some(usize::from(idx));
            }
            slot = (slot + 1) & mask;
        }
    }

    #[inline]
    fn contains_key(&self, name: &str) -> bool {
        let tag = rustel_core::value::name_tag(name);
        if self.slots.is_empty() {
            self.find_inline(tag, name).is_some()
        } else {
            self.find_heap(tag, name).is_some()
        }
    }

    #[inline]
    fn get(&self, name: &str) -> Option<&V> {
        let tag = rustel_core::value::name_tag(name);
        if self.slots.is_empty() {
            let idx = self.find_inline(tag, name)?;
            Some(&self.inline[idx].as_ref().expect("inline entry").2)
        } else {
            let idx = self.find_heap(tag, name)?;
            Some(&self.entries[idx].2)
        }
    }

    #[inline]
    fn get_mut(&mut self, name: &str) -> Option<&mut V> {
        let tag = rustel_core::value::name_tag(name);
        if self.slots.is_empty() {
            let idx = self.find_inline(tag, name)?;
            Some(&mut self.inline[idx].as_mut().expect("inline entry").2)
        } else {
            let idx = self.find_heap(tag, name)?;
            Some(&mut self.entries[idx].2)
        }
    }

    fn insert(&mut self, name: &'p str, value: V) -> Option<V> {
        let tag = rustel_core::value::name_tag(name);
        if self.slots.is_empty() {
            if let Some(idx) = self.find_inline(tag, name) {
                return Some(std::mem::replace(
                    &mut self.inline[idx].as_mut().expect("inline entry").2,
                    value,
                ));
            }
            if self.inline_len < Self::LINEAR_LIMIT {
                self.inline[self.inline_len] = Some((tag, name, value));
                self.inline_len += 1;
                return None;
            }
            self.spill_to_heap(16);
        } else if let Some(idx) = self.find_heap(tag, name) {
            return Some(std::mem::replace(&mut self.entries[idx].2, value));
        }
        let new_idx = self.entries.len();
        self.entries.push((tag, name, value));
        if self.entries.len() * 2 >= self.slots.len() {
            self.grow_slots(self.slots.len() * 2);
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut slot = Self::slot_for(tag, mask);
        while self.slots[slot] != u16::MAX {
            slot = (slot + 1) & mask;
        }
        self.slots[slot] = new_idx as u16;
        None
    }
}

type Named<'p, T> = SmallNameMap<'p, T>;

/// A score-declared function entry with lazily cached `(single_use, mutated_params)` bitmasks.
struct DeclaredFunction<'p> {
    number: u32,
    function: &'p ScoreFunction,
    uses: Option<(u32, u32)>,
}

/// What evaluating the statements of a score built.
/// Cheap 64-bit Bloom bit for a score identifier, allowing host global/method
/// lookups to bypass `HashMap` probes for bindings and declared functions.
#[inline(always)]
fn name_bloom_bit(name: &str) -> u64 {
    let b = name.as_bytes();
    let len = b.len();
    let b0 = if len > 0 { b[0] as usize } else { 0 };
    let bl = if len > 0 { b[len - 1] as usize } else { 0 };
    1u64 << ((b0.wrapping_mul(31) ^ bl.wrapping_mul(17) ^ len.wrapping_mul(7)) & 63)
}

struct Turn<'p> {
    /// The expressions that the expressions of the program hold.
    held: &'p Held<'p>,
    /// The functions the score declares. JavaScript hoists them, so each is
    /// callable from the first statement.
    functions: Named<'p, DeclaredFunction<'p>>,
    /// 64-bit Bloom mask of declared function names for O(1) negative checks.
    func_mask: u64,
    /// The names the score binds. JavaScript takes each name from the first
    /// statement, and it holds no value until its own statement runs. The `u8`
    /// counts remaining top-level references (capped at 2 when also read inside
    /// a function or arrow) and the `bool` marks whether a single top-level use
    /// is immediately mutated in place so redundant memo pinning can be skipped.
    bindings: Named<'p, (u32, u8, bool, Option<Native<'p>>)>,
    /// 64-bit Bloom mask of bound variable names for O(1) negative checks.
    binding_mask: u64,
    /// The names of each function call and arrow in progress, innermost
    /// last.
    frames: Vec<Frame<'p>>,
    /// How many arrows the turn compiled, which numbers the next one.
    arrows: usize,
    lanes: SmallLanes<'p>,
    /// The next number of an anonymous `$` lane.
    anonymous: u32,
    /// The tempo the score set last, in cycles per second.
    cps: Option<f64>,
    /// How deep the evaluation is, across calls, and how many expressions
    /// it evaluated. Past either bound the score is left to JavaScript, so
    /// a deep or runaway score cannot exhaust the stack or the time here.
    depth: usize,
    steps: usize,
    /// What the build before this one kept, when this build can take from
    /// it. See [`memo`].
    reuse: Option<memo::Reuse<'p>>,
    /// The last byte of the innermost call that the native tier does not
    /// take, in the text of the score: where a host says the score needs
    /// JavaScript.
    left: Option<u32>,
    /// The lists of the chains of control calls before, empty, with their
    /// room for the next chains: a chain inside a chain takes its own.
    spare: TurnScratch<'p>,
    /// The list of the numbers of a call before, with its room for the
    /// next call.
    numbers: Vec<Value>,
    /// Whether any top-level binding or function shadows `MINI_CALLEE` (`"m"`).
    top_shadows_mini: bool,
    /// Per-evaluation cache of parsed mini-notation literals (control_id = 0)
    /// and directly named controls (control_id = spec.id) evaluated inside
    /// function or arrow bodies, dropped when the build ends.
    mini_cache: std::collections::HashMap<
        (u16, usize, usize, i64),
        Pattern,
        std::hash::BuildHasherDefault<rustel_core::WordHasher>,
    >,
}

/// Cleared `'static` scratch buffers reused across native evaluations while
/// retaining zero `Pattern` or `Value` handles.
#[derive(Default)]
struct StaticTurnScratch {
    chains: Vec<Vec<ChainCall>>,
    numbers: Vec<Value>,
    mini_cache: std::collections::HashMap<
        (u16, usize, usize, i64),
        Pattern,
        std::hash::BuildHasherDefault<rustel_core::WordHasher>,
    >,
}

thread_local! {
    static SPARE_TURN: std::cell::RefCell<Option<StaticTurnScratch>> =
        const { std::cell::RefCell::new(None) };
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.numbers.clear();
        self.mini_cache.clear();
        for chain in &mut self.spare.chains {
            chain.clear();
        }
        if self.numbers.capacity() <= 64
            && self.mini_cache.capacity() <= 256
            && self.spare.chains.capacity() <= 8
            && self.spare.chains.iter().all(|c| c.capacity() <= 64)
        {
            let scratch = StaticTurnScratch {
                chains: std::mem::take(&mut self.spare.chains),
                numbers: std::mem::take(&mut self.numbers),
                mini_cache: std::mem::take(&mut self.mini_cache),
            };
            let _ = SPARE_TURN.try_with(|cell| *cell.borrow_mut() = Some(scratch));
        }
    }
}

/// One control call of a chain, as [`rustel_core::controls::apply_chain`]
/// takes it.
type ChainCall = rustel_core::controls::ChainCall<'static>;

/// The deepest evaluation, across function calls, the native tier takes on.
const MAX_DEPTH: usize = 128;

/// The most expressions the native tier evaluates for one score.
const MAX_STEPS: usize = 1 << 16;

#[derive(Default)]
struct TurnScratch<'p> {
    calls: Vec<Vec<ControlCall<'p>>>,
    chains: Vec<Vec<ChainCall>>,
    frame_names: Vec<Vec<(&'p str, Option<Native<'p>>)>>,
    call_values: Vec<Vec<Native<'p>>>,
}

/// Whether `expr` is a plain scalar or single-word mini literal whose control
/// application folds directly into a sequence or stack in place.
fn is_plain_scalar_arg(held: &Held<'_>, expr: &ScoreExpr) -> bool {
    match expr {
        ScoreExpr::Number(_) | ScoreExpr::String(_) => true,
        ScoreExpr::Mini { source, .. } => {
            let source = held.text(*source);
            !source.is_empty()
                && !source.bytes().any(|b| {
                    matches!(
                        b,
                        b' ' | b'\t'
                            | b'\n'
                            | b'\r'
                            | b'['
                            | b']'
                            | b'<'
                            | b'>'
                            | b'{'
                            | b'}'
                            | b'*'
                            | b'/'
                            | b'!'
                            | b'@'
                            | b'_'
                            | b'('
                            | b')'
                            | b'?'
                            | b','
                            | b'|'
                    )
                })
        }
        ScoreExpr::Call {
            callee, arguments, ..
        } if held.text(*callee) == MINI_CALLEE => {
            matches!(
                held.row(*arguments),
                [ScoreExpr::String(source), ScoreExpr::Number(_)]
                    if {
                        let source = held.text(*source);
                        !source.is_empty()
                            && !source.bytes().any(|b| matches!(
                                b,
                                b' ' | b'\t'
                                    | b'\n'
                                    | b'\r'
                                    | b'['
                                    | b']'
                                    | b'<'
                                    | b'>'
                                    | b'{'
                                    | b'}'
                                    | b'*'
                                    | b'/'
                                    | b'!'
                                    | b'@'
                                    | b'_'
                                    | b'('
                                    | b')'
                                    | b'?'
                                    | b','
                                    | b'|'
                            ))
                    }
            )
        }
        _ => false,
    }
}

/// Walk `expr` and report every referenced identifier along with whether the
/// reference lies inside a nested arrow or function body and whether this
/// direct occurrence is immediately mutated in place by a control or `.scale`.
fn walk_expr_names<'p>(
    held: &'p Held<'p>,
    names: &Names<'_, '_>,
    mut expr: &'p ScoreExpr,
    interest_mask: u64,
    in_closure: bool,
    mut direct_mutated: bool,
    on_name: &mut impl FnMut(&'p str, bool, bool),
) {
    loop {
        match expr {
            ScoreExpr::Name(name) => {
                let text = held.text(*name);
                if (interest_mask & name_bloom_bit(text)) != 0 {
                    on_name(text, in_closure, direct_mutated);
                }
                return;
            }
            ScoreExpr::Object(properties) => {
                for prop in held.props(*properties) {
                    match prop {
                        ScoreProperty::Entry { value, .. } | ScoreProperty::Spread(value) => {
                            if !matches!(
                                value,
                                ScoreExpr::Number(_)
                                    | ScoreExpr::String(_)
                                    | ScoreExpr::Mini { .. }
                            ) {
                                walk_expr_names(
                                    held,
                                    names,
                                    value,
                                    interest_mask,
                                    in_closure,
                                    false,
                                    on_name,
                                );
                            }
                        }
                    }
                }
                return;
            }
            ScoreExpr::Array(row) => {
                for elem in held.row(*row) {
                    if !matches!(
                        elem,
                        ScoreExpr::Number(_) | ScoreExpr::String(_) | ScoreExpr::Mini { .. }
                    ) {
                        walk_expr_names(
                            held,
                            names,
                            elem,
                            interest_mask,
                            in_closure,
                            false,
                            on_name,
                        );
                    }
                }
                return;
            }
            ScoreExpr::Member { object, .. } => {
                expr = held.at(*object);
                direct_mutated = false;
            }
            ScoreExpr::Call {
                callee, arguments, ..
            } => {
                let callee = held.text(*callee);
                if (interest_mask & name_bloom_bit(callee)) != 0 {
                    on_name(callee, in_closure, false);
                }
                let args = held.row(*arguments);
                if args.len() == 1 {
                    match &args[0] {
                        ScoreExpr::Name(arg_name) => {
                            let arg_text = held.text(*arg_name);
                            if (interest_mask & name_bloom_bit(arg_text)) != 0 {
                                let mutates_sole_arg =
                                    matches!(names.global(callee), Some(BuiltinGlobal::Control(_)));
                                on_name(arg_text, in_closure, mutates_sole_arg);
                            }
                        }
                        ScoreExpr::Number(_) | ScoreExpr::String(_) | ScoreExpr::Mini { .. } => {}
                        arg => {
                            walk_expr_names(
                                held,
                                names,
                                arg,
                                interest_mask,
                                in_closure,
                                false,
                                on_name,
                            );
                        }
                    }
                } else {
                    for arg in args {
                        if !matches!(
                            arg,
                            ScoreExpr::Number(_) | ScoreExpr::String(_) | ScoreExpr::Mini { .. }
                        ) {
                            walk_expr_names(
                                held,
                                names,
                                arg,
                                interest_mask,
                                in_closure,
                                false,
                                on_name,
                            );
                        }
                    }
                }
                return;
            }
            ScoreExpr::Method {
                receiver,
                method,
                arguments,
                ..
            } => {
                let args = held.row(*arguments);
                for arg in args {
                    if !matches!(
                        arg,
                        ScoreExpr::Number(_) | ScoreExpr::String(_) | ScoreExpr::Mini { .. }
                    ) {
                        walk_expr_names(
                            held,
                            names,
                            arg,
                            interest_mask,
                            in_closure,
                            false,
                            on_name,
                        );
                    }
                }
                let receiver_expr = held.at(*receiver);
                if let ScoreExpr::Name(recv_name) = receiver_expr {
                    let recv_text = held.text(*recv_name);
                    if (interest_mask & name_bloom_bit(recv_text)) != 0 {
                        let mutated = args.len() == 1 && is_plain_scalar_arg(held, &args[0]) && {
                            let method = held.text(*method);
                            method == "scale"
                                || matches!(
                                    builtin_method(names, method),
                                    Some(BuiltinMethod::Control(_))
                                )
                        };
                        on_name(recv_text, in_closure, mutated);
                    }
                    return;
                }
                direct_mutated = false;
                expr = receiver_expr;
            }
            ScoreExpr::Arrow(func) => {
                let func = held.function(*func);
                for (_, val) in &func.bindings {
                    walk_expr_names(held, names, val, interest_mask, true, false, on_name);
                }
                walk_expr_names(
                    held,
                    names,
                    &func.result,
                    interest_mask,
                    true,
                    false,
                    on_name,
                );
                return;
            }
            ScoreExpr::Number(_) | ScoreExpr::String(_) | ScoreExpr::Mini { .. } => return,
        }
    }
}

/// Single-pass analysis of a declared function body returning
/// `(single_use_frame_slots, mutated_single_use_params)`:
/// - `single_use_frame_slots`: bit `i` (`0..32`) is set when frame slot `i`
///   (parameters followed by local bindings) is referenced at most once and
///   never inside a nested arrow, so reading it moves instead of cloning.
/// - `mutated_single_use_params`: bit `i` (`0..32`, `i < parameters.len()`) is
///   set when parameter `i`'s sole occurrence is directly mutated in place.
fn analyze_function_uses<'p>(
    held: &'p Held<'p>,
    names: &Names<'_, '_>,
    function: &'p ScoreFunction,
) -> (u32, u32) {
    let param_count = function.parameters.len();
    if param_count == 0 && function.bindings.is_empty() {
        return (0, 0);
    }
    let mut seen = 0u32;
    let mut multi = 0u32;
    let mut mutated = 0u32;
    let mut mark = |ident: &str, in_closure: bool, direct_mutated: bool| {
        if let Some(idx) = function
            .parameters
            .iter()
            .position(|n| held.text(*n) == ident)
        {
            if idx < 32 {
                let bit = 1u32 << idx;
                if in_closure || (seen & bit) != 0 {
                    multi |= bit;
                } else {
                    seen |= bit;
                    if direct_mutated {
                        mutated |= bit;
                    }
                }
            }
        } else if let Some(bidx) = function
            .bindings
            .iter()
            .position(|(n, _)| held.text(*n) == ident)
        {
            let idx = param_count + bidx;
            if idx < 32 {
                let bit = 1u32 << idx;
                if in_closure || (seen & bit) != 0 {
                    multi |= bit;
                } else {
                    seen |= bit;
                }
            }
        }
    };
    let mut local_mask = 0u64;
    for p in function.parameters.iter() {
        local_mask |= name_bloom_bit(held.text(*p));
    }
    for (b, _) in &function.bindings {
        local_mask |= name_bloom_bit(held.text(*b));
    }
    for (_, val) in &function.bindings {
        walk_expr_names(held, names, val, local_mask, false, false, &mut mark);
    }
    walk_expr_names(
        held,
        names,
        &function.result,
        local_mask,
        false,
        false,
        &mut mark,
    );
    let single_use = seen & !multi;
    (single_use, single_use & mutated)
}

/// Inline-optimized binding list for a function call or arrow frame, avoiding
/// heap allocations for frames with `<= 2` parameters + local bindings.
#[allow(clippy::large_enum_variant)]
enum FrameNames<'p> {
    Inline {
        entries: [(&'p str, Option<Native<'p>>); 2],
        len: u8,
    },
    Heap(Vec<(&'p str, Option<Native<'p>>)>),
}

impl<'p> FrameNames<'p> {
    #[inline(always)]
    fn as_slice(&self) -> &[(&'p str, Option<Native<'p>>)] {
        match self {
            Self::Inline { entries, len } => &entries[..usize::from(*len)],
            Self::Heap(vec) => vec.as_slice(),
        }
    }

    #[inline(always)]
    fn as_mut_slice(&mut self) -> &mut [(&'p str, Option<Native<'p>>)] {
        match self {
            Self::Inline { entries, len } => &mut entries[..usize::from(*len)],
            Self::Heap(vec) => vec.as_mut_slice(),
        }
    }
}

/// The names one function call or arrow binds: its parameters, then its
/// bindings, which hold no value until their statement runs.
struct Frame<'p> {
    names: FrameNames<'p>,
    /// Bit `i` is set when `names[i]` is referenced at most once in the frame
    /// body and never inside a nested arrow, so reading it moves instead of cloning.
    single_use: u32,
    /// Whether this is an arrow's frame: its body also reads the names of
    /// the frame it is written in. A declared function reads its own names
    /// and the score's, never its caller's.
    arrow: bool,
    /// Whether this frame (or an enclosing frame visible through arrows)
    /// shadows `MINI_CALLEE` (`"m"`).
    shadows_mini: bool,
}

impl<'p> Frame<'p> {
    /// The frame of `function` with `values` for its parameters, in order.
    /// `None` for a name declared twice: a JavaScript error, or a parameter
    /// that hides another.
    fn of(
        held: &'p Held<'p>,
        function: &'p ScoreFunction,
        values: impl IntoIterator<Item = Native<'p>>,
        arrow: bool,
        parent_shadows_mini: bool,
        single_use: u32,
        spare_names: &mut Vec<Vec<(&'p str, Option<Native<'p>>)>>,
    ) -> Option<Self> {
        let total = function.parameters.len() + function.bindings.len();
        let bound = function
            .parameters
            .iter()
            .copied()
            .zip(values.into_iter().map(Some))
            .chain(function.bindings.iter().map(|(name, _)| (*name, None)));
        let mut shadows_mini = arrow && parent_shadows_mini;
        let names = if total <= 2 {
            let mut entries: [(&'p str, Option<Native<'p>>); 2] = [("", None), ("", None)];
            let mut len = 0usize;
            for (name_id, value) in bound {
                let name = held.text(name_id);
                if entries[..len].iter().any(|(existing, _)| *existing == name) {
                    return None;
                }
                if name == MINI_CALLEE {
                    shadows_mini = true;
                }
                entries[len] = (name, value);
                len += 1;
            }
            FrameNames::Inline {
                entries,
                len: len as u8,
            }
        } else {
            let mut names = spare_names.pop().unwrap_or_default();
            names.clear();
            names.reserve(total);
            for (name_id, value) in bound {
                let name = held.text(name_id);
                if names.iter().any(|(existing, _)| *existing == name) {
                    names.clear();
                    spare_names.push(names);
                    return None;
                }
                if name == MINI_CALLEE {
                    shadows_mini = true;
                }
                names.push((name, value));
            }
            FrameNames::Heap(names)
        };
        Some(Self {
            names,
            single_use: if arrow { 0 } else { single_use },
            arrow,
            shadows_mini,
        })
    }
}

/// What a name of a score means where it is read as a callee or host symbol.
enum Resolved<'p> {
    /// A value bound in a function call or arrow frame in progress.
    Local,
    /// A value bound by a score statement.
    Bound,
    /// A function the score declares, with the number of its name.
    Function(u32, &'p ScoreFunction),
    /// A name the score does not bind, for the host to resolve.
    Host,
}

/// A natively built score, ready to stage.
pub(super) struct NativeScore {
    pub(super) pattern: Pattern,
    pub(super) cps: Option<f64>,
}

/// Where the native tier reads what a name means.
enum Names<'a, 'js> {
    /// An installed realm: each name must still hold its recorded slot.
    Realm(&'a Ctx<'js>),
    /// A realm with no surface yet, where no score has run: the names a
    /// fresh installation proves.
    Listed(ListedNames),
}

impl Names<'_, '_> {
    fn global(&self, name: &str) -> Option<BuiltinGlobal> {
        match self {
            Self::Realm(ctx) => prove_builtin_global(ctx, name),
            Self::Listed(names) => names.global(name),
        }
    }

    fn is_signal(&self, name: &str) -> bool {
        match self {
            Self::Realm(ctx) => matches!(
                prove_builtin_global(ctx, name),
                Some(BuiltinGlobal::Signal(_))
            ),
            Self::Listed(names) => names.is_signal(name),
        }
    }

    fn mini(&self) -> bool {
        match self {
            Self::Realm(ctx) => prove_mini_callee(ctx),
            Self::Listed(_) => true,
        }
    }

    fn lane(&self) -> bool {
        match self {
            Self::Realm(ctx) => prove_lane_method(ctx),
            Self::Listed(_) => true,
        }
    }

    fn raw_strings(&self) -> bool {
        match self {
            Self::Realm(ctx) => {
                crate::host::host_callback_ir(ctx).is_ok_and(|ir| !ir.string_parser_active.get())
            }
            Self::Listed(_) => true,
        }
    }
}

impl JsRuntime {
    /// Build the transpiled score `program` without JavaScript. `None` when
    /// the score is no plain program or the native tier does not prove all
    /// of it: the JavaScript tier then evaluates it, and owns every error.
    ///
    /// `source` is the text that `program` was read from, when the reader
    /// of the text of a score read it. The build then takes each part that
    /// the build before made of the same text. See [`memo`].
    pub(super) fn native_score(
        &self,
        program: Option<&ScoreProgram>,
        source: Option<&str>,
    ) -> Option<NativeScore> {
        self.native_left.set(None);
        if !self.native_tier.get() {
            return None;
        }
        let program = program?;
        // A name of a realm can change between two builds, so a build in a
        // realm keeps no part, and the parts of a build before it are of no
        // use to a later one.
        let kept = self.native_memo.take();
        let built = if self.bindings_installed.get() {
            with_ctx(self, |ctx| {
                if prove_surface_intrinsics(&ctx) && !gain_curve_set(&ctx) {
                    self.build_native(program, &Names::Realm(&ctx), None)
                } else {
                    Err(None)
                }
            })
        } else {
            let reuse = source.map(|source| memo::Reuse::new(source, kept, program));
            self.build_native(program, &Names::Listed(ListedNames), reuse)
        };
        match built {
            Ok((mut score, reuse)) => {
                let pending_source = self.pending_source.borrow_mut().take();
                if let Some(reuse) = reuse {
                    let (mut memo, reused_nodes) = reuse.finish(score.cps, pending_source);
                    rustel_core::intern::dedup_score_with_reused(
                        &mut score.pattern,
                        &reused_nodes,
                        |remap| {
                            memo.remap_nodes(remap);
                        },
                    );
                    self.native_memo.replace(memo);
                } else {
                    rustel_core::intern::dedup_score(&mut score.pattern, |_| {});
                }
                Some(score)
            }
            Err(left) => {
                self.pending_source.borrow_mut().take();
                // The place is in the text that the program was read from.
                self.native_left.set(left.filter(|_| source.is_some()));
                None
            }
        }
    }

    /// Reuse the active native score when `source` is byte-identical to the
    /// last native build and no script engine has started.
    pub fn try_reuse_identical_native_score(
        &self,
        source: &str,
    ) -> Option<(Pattern, Option<f64>, rustel_core::settings::RuntimeSettings)> {
        if !self.native_tier.get() || self.bindings_installed.get() || self.realm.get().is_some() {
            return None;
        }
        let memo = self.native_memo.borrow();
        if memo.source.as_deref() != Some(source) {
            return None;
        }
        let pattern = self
            .native_active
            .borrow()
            .as_ref()?
            .clone()
            .with_runtime_settings(self.core_settings.clone());
        self.evaluated_natively.set(true);
        self.native_left.set(None);
        Some((pattern, memo.cps, self.core_settings.clone()))
    }

    /// The score that `program` builds and what the build keeps, or where
    /// in the text the native tier stopped, when it knows.
    fn build_native<'p>(
        &self,
        program: &'p ScoreProgram<'p>,
        names: &Names<'_, '_>,
        reuse: Option<memo::Reuse<'p>>,
    ) -> Result<(NativeScore, Option<memo::Reuse<'p>>), Option<u32>> {
        // Composers read the default alignment when they are called, and
        // every score starts from `In`.
        rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
        let mut func_count = 0usize;
        let mut binding_count = 0usize;
        let mut lane_count = 0usize;
        for statement in &program.statements {
            match statement {
                ScoreStatement::Function { .. } => func_count += 1,
                ScoreStatement::Binding { .. } => binding_count += 1,
                ScoreStatement::Expression(ScoreExpr::Method { method, .. })
                    if program.held.text(*method) == LANE_METHOD =>
                {
                    lane_count += 1;
                }
                ScoreStatement::Expression(_) => {}
            }
        }
        let mut static_scratch = SPARE_TURN
            .try_with(|cell| cell.borrow_mut().take())
            .ok()
            .flatten()
            .unwrap_or_default();
        static_scratch.numbers.clear();
        static_scratch.mini_cache.clear();
        let mut turn = Turn {
            held: &program.held,
            functions: if func_count > 0 {
                Named::with_capacity(func_count)
            } else {
                Named::default()
            },
            func_mask: 0,
            bindings: if binding_count > 0 {
                Named::with_capacity(binding_count)
            } else {
                Named::default()
            },
            binding_mask: 0,
            frames: Vec::new(),
            arrows: 0,
            lanes: SmallLanes::with_capacity(lane_count),
            anonymous: 0,
            cps: None,
            depth: 0,
            steps: 0,
            reuse,
            left: None,
            spare: TurnScratch {
                chains: static_scratch.chains,
                ..Default::default()
            },
            numbers: static_scratch.numbers,
            top_shadows_mini: false,
            mini_cache: static_scratch.mini_cache,
        };
        match self.build_statements(program, names, &mut turn) {
            Some(score) => Ok((score, turn.reuse.take())),
            None => Err(turn.left),
        }
    }

    fn build_statements<'p>(
        &self,
        program: &'p ScoreProgram<'p>,
        names: &Names<'_, '_>,
        turn: &mut Turn<'p>,
    ) -> Option<NativeScore> {
        // A name declared twice is a JavaScript error. Each name has the
        // number of its statement among the statements with a name.
        let mut named = 0;
        for statement in &program.statements {
            let declared_once = match statement {
                ScoreStatement::Function { name, function, .. } => {
                    let name = program.held.text(*name);
                    if name == MINI_CALLEE {
                        turn.top_shadows_mini = true;
                    }
                    let bit = name_bloom_bit(name);
                    let inserted = ((turn.binding_mask & bit) == 0
                        || !turn.bindings.contains_key(name))
                        && turn
                            .functions
                            .insert(
                                name,
                                DeclaredFunction {
                                    number: named,
                                    function,
                                    uses: None,
                                },
                            )
                            .is_none();
                    turn.func_mask |= bit;
                    inserted
                }
                ScoreStatement::Binding { name, .. } => {
                    let name = program.held.text(*name);
                    if name == MINI_CALLEE {
                        turn.top_shadows_mini = true;
                    }
                    let bit = name_bloom_bit(name);
                    let inserted = ((turn.func_mask & bit) == 0
                        || !turn.functions.contains_key(name))
                        && turn
                            .bindings
                            .insert(name, (named, 0, false, None))
                            .is_none();
                    turn.binding_mask |= bit;
                    inserted
                }
                ScoreStatement::Expression(_) => continue,
            };
            if !declared_once {
                return None;
            }
            named += 1;
        }
        if !turn.bindings.is_empty() {
            let binding_mask = turn.binding_mask;
            let mut count_ref = |ident: &str, in_closure: bool, direct_mutated: bool| {
                if let Some((_, uses, mutated_only, _)) = turn.bindings.get_mut(ident) {
                    if *uses == 0 && !in_closure {
                        *mutated_only = direct_mutated;
                    } else {
                        *mutated_only = false;
                    }
                    *uses = uses.saturating_add(if in_closure { 2 } else { 1 }).min(2);
                }
            };
            for statement in &program.statements {
                match statement {
                    ScoreStatement::Binding { value, .. } | ScoreStatement::Expression(value) => {
                        walk_expr_names(
                            &program.held,
                            names,
                            value,
                            binding_mask,
                            false,
                            false,
                            &mut count_ref,
                        );
                    }
                    ScoreStatement::Function { function, .. } => {
                        for (_, val) in &function.bindings {
                            walk_expr_names(
                                &program.held,
                                names,
                                val,
                                binding_mask,
                                true,
                                false,
                                &mut count_ref,
                            );
                        }
                        walk_expr_names(
                            &program.held,
                            names,
                            &function.result,
                            binding_mask,
                            true,
                            false,
                            &mut count_ref,
                        );
                    }
                }
            }
        }
        let mut last = None;
        for statement in &program.statements {
            last = match statement {
                ScoreStatement::Binding { name, value, .. } => {
                    let name = program.held.text(*name);
                    let skip_keep = turn.reuse.as_ref().is_some_and(|r| !r.keeps_unshared())
                        && turn
                            .bindings
                            .get(name)
                            .is_some_and(|(_, uses, mutated_only, _)| *uses == 1 && *mutated_only);
                    // The name reads what its statement reads.
                    if let Some(reuse) = &mut turn.reuse {
                        reuse.begin(0);
                    }
                    let value = turn.part_maybe_keep(names, value, self, !skip_keep);
                    let (number, _, _, bound) = turn.bindings.get_mut(name)?;
                    if let Some(reuse) = &mut turn.reuse {
                        let read = reuse.end();
                        reuse.named(*number, read);
                    }
                    *bound = Some(value?);
                    None
                }
                ScoreStatement::Expression(expression) => Some(turn.part(names, expression, self)?),
                ScoreStatement::Function { .. } => None,
            };
        }
        let pattern = if turn.lanes.is_empty() {
            // With no lane, the score plays its last expression.
            let Native::Pattern(pattern) = last? else {
                return None;
            };
            pattern
        } else {
            // Drop any extra handle held in `last` before stacking lanes.
            // Pure native scores never produce `vst`/`vsti` plugin notes or
            // `JsValue` payloads, so per-lane `lane_tagged` wrappers are
            // guaranteed no-ops and can be omitted.
            drop(last);
            std::mem::take(&mut turn.lanes).into_pattern()
        };
        Some(NativeScore {
            pattern,
            cps: turn.cps,
        })
    }

    /// Stage a natively built score as [`Self::execute_score`] stages one:
    /// its tempo with the score's effects, and its pattern where the commit
    /// picks it up.
    ///
    /// The JavaScript tier puts a final rewrite over an impure graph, for
    /// the events a worklet graph rides on. Only JavaScript makes such an
    /// event, so a native graph goes in play without it.
    ///
    /// A native graph is quick to walk, so it goes in play marked as
    /// walked: it keeps no table, and its memory is that of its score. No
    /// wrapper holds it, so the stage runs no step of the realm.
    pub(super) fn stage_native(&self, score: NativeScore) -> Result<(), QueryError> {
        if let Some(cps) = score.cps {
            let mut transaction = self.bridge().effect_transaction.borrow_mut();
            let effects = transaction.effects_mut().ok_or_else(|| {
                QueryError::Message("a native score set its tempo outside a score turn".into())
            })?;
            effects.cps = Some(cps);
        }
        if let Some(offsets) = self
            .timeline_state
            .get()
            .map(rustel_core::TimelineState::take)
            && !offsets.is_empty()
        {
            self.last_good_timeline.borrow_mut().get_or_insert(offsets);
        }
        *self.native_staged.borrow_mut() = Some(score.pattern.mark_walked());
        self.evaluated_natively.set(true);
        Ok(())
    }
}

impl<'p> Turn<'p> {
    fn evaluate(
        &mut self,
        names: &Names<'_, '_>,
        expression: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        self.steps += 1;
        if self.steps > MAX_STEPS || self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let value = self.evaluate_here(names, expression, runtime);
        self.depth -= 1;
        if value.is_none()
            && self.left.is_none()
            && let ScoreExpr::Call { written, .. } | ScoreExpr::Method { written, .. } = expression
            && written.len > 0
        {
            self.left = Some(written.start.saturating_add(written.len - 1));
        }
        value
    }

    #[inline(always)]
    fn mini_is_host(&self) -> bool {
        !self.top_shadows_mini && !self.frames.last().is_some_and(|frame| frame.shadows_mini)
    }

    fn eval_mini_literal(
        &mut self,
        names: &Names<'_, '_>,
        source: &'p str,
        offset: u32,
    ) -> Option<Pattern> {
        if !names.mini() || !self.mini_is_host() {
            return None;
        }
        let offset_i64 = i64::from(offset);
        if !self.frames.is_empty() {
            let key = (0u16, source.as_ptr() as usize, source.len(), offset_i64);
            if let Some(cached) = self.mini_cache.get(&key) {
                return Some(cached.clone());
            }
            let pat = crate::surface::parse_mini_in_room(source, offset_i64).ok()?;
            self.mini_cache.insert(key, pat.clone());
            return Some(pat);
        }
        crate::surface::parse_mini_in_room(source, offset_i64).ok()
    }

    fn evaluate_here(
        &mut self,
        names: &Names<'_, '_>,
        expression: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        Some(match expression {
            ScoreExpr::Number(value) => Native::Number(*value),
            ScoreExpr::String(_) => Native::String,
            ScoreExpr::Mini { source, offset } => {
                let source = self.held.text(*source);
                Native::Pattern(self.eval_mini_literal(names, source, *offset)?)
            }
            ScoreExpr::Name(name) => {
                let name = self.held.text(*name);
                let top_frame = self.frames.len().saturating_sub(1);
                let mut found_local = None;
                for (number, frame) in self.frames.iter_mut().enumerate().rev() {
                    if let Some((slot_idx, (_, slot))) = frame
                        .names
                        .as_mut_slice()
                        .iter_mut()
                        .enumerate()
                        .find(|(_, (bound, _))| *bound == name)
                    {
                        let value = if number == top_frame
                            && slot_idx < 32
                            && (frame.single_use & (1u32 << slot_idx)) != 0
                        {
                            slot.take()
                        } else {
                            slot.clone()
                        };
                        found_local = Some((number, value));
                        break;
                    }
                    if !frame.arrow {
                        break;
                    }
                }
                let bit = if (self.binding_mask | self.func_mask) != 0 {
                    name_bloom_bit(name)
                } else {
                    0
                };
                if let Some((frame, value)) = found_local {
                    let value = value?;
                    if let Some(reuse) = &mut self.reuse {
                        reuse.reads_frame(frame);
                    }
                    value
                } else if (self.binding_mask & bit) != 0
                    && let Some((number, uses, _, slot)) = self.bindings.get_mut(name)
                {
                    let number = *number;
                    let value = if self.frames.is_empty() && *uses == 1 {
                        *uses = 0;
                        slot.take()?
                    } else {
                        slot.clone()?
                    };
                    if let Some(reuse) = &mut self.reuse {
                        reuse.reads(number);
                    }
                    value
                } else if (self.func_mask & bit) != 0 && self.functions.contains_key(name) {
                    return None;
                } else {
                    match names.global(name)? {
                        BuiltinGlobal::Signal(pattern) => Native::Pattern(pattern),
                        _ => return None,
                    }
                }
            }
            ScoreExpr::Object(properties) => {
                self.object(names, self.held.props(*properties), runtime)?
            }
            ScoreExpr::Array(elements) => {
                let mut array = Vec::with_capacity(elements.len());
                for element in self.held.row(*elements) {
                    array.push(self.evaluate(names, element, runtime)?);
                }
                Native::Array(array.into())
            }
            ScoreExpr::Member { object, property } => {
                let object = self.held.at(*object);
                let property = self.held.text(*property);
                let Native::Object(properties) = self.evaluate(names, object, runtime)? else {
                    return None;
                };
                // A property the object does not own reads its prototype.
                let (_, value) = properties.iter().find(|(key, _)| key == &property)?;
                value.clone()
            }
            ScoreExpr::Call {
                callee, arguments, ..
            } => self.call(
                names,
                self.held.text(*callee),
                self.held.row(*arguments),
                runtime,
            )?,
            ScoreExpr::Method {
                receiver,
                method,
                arguments,
                written,
            } => {
                let method = self.held.text(*method);
                let arguments = self.held.row(*arguments);
                let pre_resolved = if let [first_arg] = arguments {
                    let resolved = builtin_method(names, method);
                    if let Some(BuiltinMethod::Control(first_spec)) = resolved {
                        let first = ControlCall {
                            spec: first_spec,
                            argument: first_arg,
                            written: *written,
                        };
                        return self.control_chain_from(
                            names,
                            first,
                            self.held.at(*receiver),
                            runtime,
                        );
                    }
                    Some(resolved)
                } else {
                    None
                };
                match self.receiver(names, self.held.at(*receiver), method, runtime)? {
                    Native::Pattern(receiver) => {
                        self.method(names, receiver, method, arguments, pre_resolved, runtime)?
                    }
                    Native::Chain(chain) => Native::Chain(self.chained(
                        names,
                        chain,
                        method,
                        arguments,
                        pre_resolved,
                        runtime,
                    )?),
                    _ => return None,
                }
            }
            ScoreExpr::Arrow(function) => {
                Native::Function(self.arrow(names, self.held.function(*function), runtime)?)
            }
        })
    }

    /// Compile `(parameter) => result` to a pattern transformer.
    ///
    /// The body runs once, here, with the parameter standing for any
    /// pattern: each method it calls on the parameter becomes one step of
    /// the program. Every other value the body reads is the value it has
    /// here, which is the value it has at every call, as a score assigns to
    /// no name. The body must give its own parameter back, through steps
    /// that each keep a pure pattern pure.
    fn arrow(
        &mut self,
        names: &Names<'_, '_>,
        function: &'p ScoreFunction,
        runtime: &JsRuntime,
    ) -> Option<FunctionRef> {
        let [_] = function.parameters.as_slice() else {
            return None;
        };
        let arrow = self.arrows;
        self.arrows += 1;
        let parameter = Native::Chain(Chain {
            arrow,
            steps: ChainSteps::Empty,
        });
        let parent_shadows = self.frames.last().is_some_and(|frame| frame.shadows_mini);
        self.frames.push(Frame::of(
            self.held,
            function,
            [parameter],
            true,
            parent_shadows,
            0,
            &mut self.spare.frame_names,
        )?);
        let result = self.function_body(names, function, runtime);
        if let Some(popped) = self.frames.pop()
            && let FrameNames::Heap(mut names) = popped.names
        {
            names.clear();
            self.spare.frame_names.push(names);
        }
        let Native::Chain(chain) = result? else {
            return None;
        };
        if chain.arrow != arrow {
            return None;
        }
        match chain.steps {
            ChainSteps::One(PatternTransformInstruction::Apply(mut func)) if func.is_native() => {
                func.clear_name();
                Some(func)
            }
            steps => {
                let program = match steps {
                    ChainSteps::Empty => PatternTransformProgram::new(std::iter::empty()).ok()?,
                    ChainSteps::One(step) => PatternTransformProgram::new([step]).ok()?,
                    ChainSteps::Many(vec) => PatternTransformProgram::new(vec).ok()?,
                };
                Some(FunctionRef::native_program(program))
            }
        }
    }

    /// Whether an arrow is being compiled. Its body runs again at every
    /// application of the transformer, so it can have no effect of its own.
    fn compiling(&self) -> bool {
        self.frames.iter().any(|frame| frame.arrow)
    }

    /// An object literal's properties, as JavaScript enumerates them: in
    /// the order written, a spread's own properties in their order, and a
    /// repeated key at its first place with its last value. JavaScript
    /// enumerates integer-like keys first, so an object with one is left to
    /// JavaScript.
    fn object(
        &mut self,
        names: &Names<'_, '_>,
        properties: &'p [ScoreProperty],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        fn set<'p>(object: &mut Vec<(&'p str, Native<'p>)>, key: &'p str, value: Native<'p>) {
            match object.iter_mut().find(|(held, _)| *held == key) {
                Some((_, held)) => *held = value,
                None => object.push((key, value)),
            }
        }
        let mut object = Vec::with_capacity(properties.len());
        for property in properties {
            match property {
                ScoreProperty::Entry { key, value } => {
                    let key = self.held.text(*key);
                    if !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit()) {
                        return None;
                    }
                    let value = self.part(names, value, runtime)?;
                    set(&mut object, key, value);
                }
                ScoreProperty::Spread(source) => {
                    let Native::Object(source) = self.evaluate(names, source, runtime)? else {
                        return None;
                    };
                    for (key, value) in source.iter() {
                        set(&mut object, key, value.clone());
                    }
                }
            }
        }
        Some(Native::Object(object.into()))
    }

    /// What `name` means here: a name of the innermost frame, of each frame
    /// an arrow is written in, then of the score.
    #[inline]
    fn resolve(&self, name: &str) -> Resolved<'p> {
        for frame in self.frames.iter().rev() {
            if frame
                .names
                .as_slice()
                .iter()
                .any(|(bound, _)| *bound == name)
            {
                return Resolved::Local;
            }
            if !frame.arrow {
                break;
            }
        }
        if (self.binding_mask | self.func_mask) == 0 {
            return Resolved::Host;
        }
        let bit = name_bloom_bit(name);
        if (self.binding_mask & bit) != 0 && self.bindings.contains_key(name) {
            return Resolved::Bound;
        }
        if (self.func_mask & bit) == 0 {
            return Resolved::Host;
        }
        self.functions.get(name).map_or(Resolved::Host, |entry| {
            Resolved::Function(entry.number, entry.function)
        })
    }

    fn call(
        &mut self,
        names: &Names<'_, '_>,
        callee: &'p str,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        match self.resolve(callee) {
            Resolved::Local | Resolved::Bound => return None,
            Resolved::Function(number, function) => {
                return self.call_function(names, callee, number, function, arguments, runtime);
            }
            Resolved::Host => {}
        }
        if callee == MINI_CALLEE {
            let [ScoreExpr::String(source), ScoreExpr::Number(offset)] = arguments else {
                return None;
            };
            if !names.mini() || offset.fract() != 0.0 || !offset.is_finite() {
                return None;
            }
            let source = self.held.text(*source);
            let offset = *offset as i64;
            if !self.frames.is_empty() {
                let key = (0u16, source.as_ptr() as usize, source.len(), offset);
                if let Some(cached) = self.mini_cache.get(&key) {
                    return Some(Native::Pattern(cached.clone()));
                }
                let pat = crate::surface::parse_mini_in_room(source, offset).ok()?;
                self.mini_cache.insert(key, pat.clone());
                return Some(Native::Pattern(pat));
            }
            return crate::surface::parse_mini_in_room(source, offset)
                .ok()
                .map(Native::Pattern);
        }
        match names.global(callee)? {
            BuiltinGlobal::Control(spec) => {
                let [argument] = arguments else {
                    return None;
                };
                if let ScoreExpr::Mini { source, offset } = argument
                    && names.mini()
                    && self.mini_is_host()
                {
                    if self.steps >= MAX_STEPS || self.depth >= MAX_DEPTH {
                        return None;
                    }
                    self.steps += 1;
                    let source = self.held.text(*source);
                    let at = *offset as usize;
                    let offset = i64::from(*offset);
                    let spec_id = spec.builtin_id();
                    if !self.frames.is_empty() && spec_id != 0 {
                        let key = (spec_id, source.as_ptr() as usize, source.len(), offset);
                        if let Some(cached) = self.mini_cache.get(&key) {
                            return Some(Native::Pattern(cached.clone()));
                        }
                        let named = if let Some((constant, locations)) =
                            rustel_mini::word_constant_parts(source, at)
                        {
                            match constant {
                                rustel_mini::WordConstant::One(value) => {
                                    spec.standalone_word(value, locations)
                                }
                                rustel_mini::WordConstant::Pair(a, b) => {
                                    spec.standalone_word_pair(a, b, locations)
                                }
                                rustel_mini::WordConstant::Many(list) => {
                                    spec.standalone_word(Value::List(list), locations)
                                }
                            }
                        } else {
                            let pat =
                                crate::surface::parse_mini_non_word_in_room(source, offset).ok()?;
                            spec.standalone_owned(pat)
                        };
                        self.mini_cache.insert(key, named.clone());
                        return Some(Native::Pattern(named));
                    }
                    if let Some((constant, locations)) =
                        rustel_mini::word_constant_parts(source, at)
                    {
                        let named = match constant {
                            rustel_mini::WordConstant::One(value) => {
                                spec.standalone_word(value, locations)
                            }
                            rustel_mini::WordConstant::Pair(a, b) => {
                                spec.standalone_word_pair(a, b, locations)
                            }
                            rustel_mini::WordConstant::Many(list) => {
                                spec.standalone_word(Value::List(list), locations)
                            }
                        };
                        return Some(Native::Pattern(named));
                    }
                    let pat = crate::surface::parse_mini_non_word_in_room(source, offset).ok()?;
                    return Some(Native::Pattern(spec.standalone_owned(pat)));
                }
                if let ScoreExpr::Call {
                    callee: inner_callee,
                    arguments: inner_args,
                    ..
                } = argument
                    && self.held.text(*inner_callee) == MINI_CALLEE
                    && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] =
                        self.held.row(*inner_args)
                    && names.mini()
                    && self.mini_is_host()
                    && offset.fract() == 0.0
                    && offset.is_finite()
                {
                    // Account for the inner `m(...)` step.
                    if self.steps >= MAX_STEPS || self.depth >= MAX_DEPTH {
                        return None;
                    }
                    self.steps += 1;
                    let source = self.held.text(*source);
                    let offset = *offset as i64;
                    let spec_id = spec.builtin_id();
                    if !self.frames.is_empty() && spec_id != 0 {
                        let key = (spec_id, source.as_ptr() as usize, source.len(), offset);
                        if let Some(cached) = self.mini_cache.get(&key) {
                            return Some(Native::Pattern(cached.clone()));
                        }
                        let named = if let Ok(at) = usize::try_from(offset)
                            && let Some((constant, locations)) =
                                rustel_mini::word_constant_parts(source, at)
                        {
                            match constant {
                                rustel_mini::WordConstant::One(value) => {
                                    spec.standalone_word(value, locations)
                                }
                                rustel_mini::WordConstant::Pair(a, b) => {
                                    spec.standalone_word_pair(a, b, locations)
                                }
                                rustel_mini::WordConstant::Many(list) => {
                                    spec.standalone_word(Value::List(list), locations)
                                }
                            }
                        } else {
                            let pat =
                                crate::surface::parse_mini_non_word_in_room(source, offset).ok()?;
                            spec.standalone_owned(pat)
                        };
                        self.mini_cache.insert(key, named.clone());
                        return Some(Native::Pattern(named));
                    }
                    if let Ok(at) = usize::try_from(offset)
                        && let Some((constant, locations)) =
                            rustel_mini::word_constant_parts(source, at)
                    {
                        let named = match constant {
                            rustel_mini::WordConstant::One(value) => {
                                spec.standalone_word(value, locations)
                            }
                            rustel_mini::WordConstant::Pair(a, b) => {
                                spec.standalone_word_pair(a, b, locations)
                            }
                            rustel_mini::WordConstant::Many(list) => {
                                spec.standalone_word(Value::List(list), locations)
                            }
                        };
                        return Some(Native::Pattern(named));
                    }
                    let pat = crate::surface::parse_mini_non_word_in_room(source, offset).ok()?;
                    return Some(Native::Pattern(spec.standalone_owned(pat)));
                }
                let value = self.pattern_argument(names, argument, runtime)?;
                Some(Native::Pattern(spec.standalone_owned(value)))
            }
            BuiltinGlobal::List(kind) => {
                // On non-tiny scores `Reuse::finish` drops unshared folded parts
                // anyway. Release any `stack` children flattened into a new
                // `Sequence` immediately so following `.scale` or control chains
                // see unique `Arc` handles and mutate layers in place.
                let release_mark = if matches!(kind, ListKind::Stack) {
                    self.reuse
                        .as_ref()
                        .filter(|reuse| !reuse.keeps_unshared())
                        .map(memo::Reuse::mark)
                } else {
                    None
                };
                let built = if arguments.len() <= 8 {
                    let mut buf: [Option<Pattern>; 8] = [const { None }; 8];
                    for (slot, argument) in buf.iter_mut().zip(arguments) {
                        let Native::Pattern(pattern) = self.part(names, argument, runtime)? else {
                            return None;
                        };
                        *slot = Some(pattern);
                    }
                    list_slice(kind, &mut buf[..arguments.len()])?
                } else {
                    let mut patterns = Vec::with_capacity(arguments.len());
                    for argument in arguments {
                        let Native::Pattern(pattern) = self.part(names, argument, runtime)? else {
                            return None;
                        };
                        patterns.push(pattern);
                    }
                    list(kind, patterns)?
                };
                if let Some(mark) = release_mark
                    && let Some(reuse) = &mut self.reuse
                {
                    reuse.release(mark);
                }
                Some(Native::Pattern(built))
            }
            BuiltinGlobal::Tempo { per_minute } => {
                let [ScoreExpr::Number(value)] = arguments else {
                    return None;
                };
                let cps = if per_minute { value / 60.0 } else { *value };
                if self.compiling()
                    || !runtime.effect_policy().allows(EffectPolicy::TEMPO)
                    || !cps.is_finite()
                    || cps <= 0.0
                {
                    return None;
                }
                self.cps = Some(cps);
                self.effect();
                Some(Native::Pattern(rustel_core::silence()))
            }
            // A signal is a value: calling it throws.
            BuiltinGlobal::Signal(_) => None,
        }
    }

    /// Call a function of the score as JavaScript does: every argument
    /// evaluated where the call is, then the body with its parameters bound.
    fn call_function(
        &mut self,
        names: &Names<'_, '_>,
        callee: &'p str,
        number: u32,
        function: &'p ScoreFunction,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let kept = self.reuse.as_ref().map(memo::Reuse::mark);
        let (single_use, mutated_params) = if let Some(entry) = self.functions.get(callee)
            && let Some(cached) = entry.uses
        {
            cached
        } else {
            let computed = analyze_function_uses(self.held, names, function);
            if let Some(entry) = self.functions.get_mut(callee) {
                entry.uses = Some(computed);
            }
            computed
        };
        let skip_param_keep = if self.reuse.as_ref().is_some_and(|r| !r.keeps_unshared()) {
            mutated_params
        } else {
            0
        };
        let mut values = self.spare.call_values.pop().unwrap_or_default();
        values.clear();
        values.reserve(arguments.len());
        for (arg_idx, argument) in arguments.iter().enumerate() {
            let keep = !(arg_idx < 32 && (skip_param_keep & (1u32 << arg_idx)) != 0);
            let Some(val) = self.part_maybe_keep(names, argument, runtime, keep) else {
                values.clear();
                self.spare.call_values.push(values);
                return None;
            };
            values.push(val);
        }
        // A missing argument is `undefined`, which the native tier does not
        // hold. An extra one is evaluated and not bound.
        if values.len() < function.parameters.len() {
            values.clear();
            self.spare.call_values.push(values);
            return None;
        }
        let frame = Frame::of(
            self.held,
            function,
            values.drain(..),
            false,
            false,
            single_use,
            &mut self.spare.frame_names,
        );
        self.spare.call_values.push(values);
        let frame = frame?;
        // The call reads the function, and the function what its body
        // reads.
        if let Some(reuse) = &mut self.reuse {
            reuse.reads(number);
            reuse.begin(self.frames.len());
        }
        self.frames.push(frame);
        let result = self.function_body(names, function, runtime);
        if let Some(popped) = self.frames.pop()
            && let FrameNames::Heap(mut names) = popped.names
        {
            names.clear();
            self.spare.frame_names.push(names);
        }
        if let Some(reuse) = &mut self.reuse {
            let read = reuse.end();
            reuse.named(number, read);
            // The result holds each argument, or the body folded it.
            if let Some(kept) = kept {
                reuse.release(kept);
            }
        }
        result
    }

    fn function_body(
        &mut self,
        names: &Names<'_, '_>,
        function: &'p ScoreFunction,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let parameters = function.parameters.len();
        for (index, (_, value)) in function.bindings.iter().enumerate() {
            let value = self.part(names, value, runtime)?;
            self.frames.last_mut()?.names.as_mut_slice()[parameters + index].1 = Some(value);
        }
        self.evaluate(names, &function.result, runtime)
    }

    /// The receiver of a call of `method`. The receiver of the lane method
    /// is the code of a statement with a label, so it is a part.
    fn receiver(
        &mut self,
        names: &Names<'_, '_>,
        receiver: &'p ScoreExpr,
        method: &str,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        if method == LANE_METHOD {
            self.part(names, receiver, runtime)
        } else {
            self.evaluate(names, receiver, runtime)
        }
    }

    /// Collect a chain of control calls starting from `first` and apply them,
    /// keeping chains of up to 20 calls entirely on the stack.
    fn control_chain_from(
        &mut self,
        names: &Names<'_, '_>,
        first: ControlCall<'p>,
        mut under: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        const INLINE: usize = 20;
        let held = self.held;
        let mut inline = [first; INLINE];
        let mut len = 1usize;
        let mut spilled: Option<Vec<ControlCall<'p>>> = None;
        while let ScoreExpr::Method {
            receiver,
            method,
            arguments,
            written,
        } = under
            && let [argument] = held.row(*arguments)
            && let Some(BuiltinMethod::Control(spec)) = builtin_method(names, held.text(*method))
        {
            let call = ControlCall {
                spec,
                argument,
                written: *written,
            };
            if let Some(vec) = &mut spilled {
                vec.push(call);
            } else if len < INLINE {
                inline[len] = call;
                len += 1;
            } else {
                let mut vec = self.spare.calls.pop().unwrap_or_default();
                vec.extend_from_slice(&inline);
                vec.push(call);
                spilled = Some(vec);
            }
            under = held.at(*receiver);
        }
        if let Some(mut calls) = spilled {
            let made = self.control_chain_slice(names, &calls, under, runtime);
            calls.clear();
            self.spare.calls.push(calls);
            made
        } else {
            self.control_chain_slice(names, &inline[..len], under, runtime)
        }
    }

    fn eval_control_arg(
        &mut self,
        names: &Names<'_, '_>,
        call: &ControlCall<'p>,
        runtime: &JsRuntime,
    ) -> Option<ChainArgument> {
        Some(match call.argument {
            ScoreExpr::Number(number) => ChainArgument::Number(*number),
            ScoreExpr::String(text) if names.raw_strings() => ChainArgument::Constant(
                rustel_core::Value::Str(self.held.text(*text).into()),
                rustel_core::Locations::new(),
            ),
            ScoreExpr::Mini { source, offset } if names.mini() && self.mini_is_host() => {
                if self.steps >= MAX_STEPS || self.depth >= MAX_DEPTH {
                    return None;
                }
                self.steps += 1;
                let source = self.held.text(*source);
                let at = *offset as usize;
                let offset_i64 = i64::from(*offset);
                if let Some((constant, locations)) = rustel_mini::word_constant_parts(source, at) {
                    match constant {
                        rustel_mini::WordConstant::One(value) => {
                            ChainArgument::Constant(value, locations)
                        }
                        rustel_mini::WordConstant::Pair(a, b) => {
                            ChainArgument::ConstantPair(a, b, locations)
                        }
                        rustel_mini::WordConstant::Many(list) => {
                            ChainArgument::Constant(rustel_core::Value::List(list), locations)
                        }
                    }
                } else {
                    let parsed = if !self.frames.is_empty() {
                        let key = (0u16, source.as_ptr() as usize, source.len(), offset_i64);
                        if let Some(cached) = self.mini_cache.get(&key) {
                            Ok(cached.clone())
                        } else {
                            let res =
                                crate::surface::parse_mini_non_word_in_room(source, offset_i64);
                            if let Ok(pat) = &res {
                                self.mini_cache.insert(key, pat.clone());
                            }
                            res
                        }
                    } else {
                        crate::surface::parse_mini_non_word_in_room(source, offset_i64)
                    };
                    match parsed {
                        Ok(pat) => ChainArgument::Pattern(pat),
                        Err(_) => {
                            if self.left.is_none() && call.written.len > 0 {
                                self.left =
                                    Some(call.written.start.saturating_add(call.written.len - 1));
                            }
                            return None;
                        }
                    }
                }
            }
            ScoreExpr::Call {
                callee,
                arguments,
                written: inner_written,
            } if self.held.text(*callee) == MINI_CALLEE
                && names.mini()
                && self.mini_is_host()
                && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] =
                    self.held.row(*arguments)
                && offset.fract() == 0.0
                && offset.is_finite() =>
            {
                if self.steps >= MAX_STEPS || self.depth >= MAX_DEPTH {
                    return None;
                }
                self.steps += 1;
                let source = self.held.text(*source);
                let offset = *offset as i64;
                if let Ok(at) = usize::try_from(offset)
                    && let Some((constant, locations)) =
                        rustel_mini::word_constant_parts(source, at)
                {
                    match constant {
                        rustel_mini::WordConstant::One(value) => {
                            ChainArgument::Constant(value, locations)
                        }
                        rustel_mini::WordConstant::Pair(a, b) => {
                            ChainArgument::ConstantPair(a, b, locations)
                        }
                        rustel_mini::WordConstant::Many(list) => {
                            ChainArgument::Constant(rustel_core::Value::List(list), locations)
                        }
                    }
                } else {
                    let parsed = if !self.frames.is_empty() {
                        let key = (0u16, source.as_ptr() as usize, source.len(), offset);
                        if let Some(cached) = self.mini_cache.get(&key) {
                            Ok(cached.clone())
                        } else {
                            let res = crate::surface::parse_mini_non_word_in_room(source, offset);
                            if let Ok(pat) = &res {
                                self.mini_cache.insert(key, pat.clone());
                            }
                            res
                        }
                    } else {
                        crate::surface::parse_mini_non_word_in_room(source, offset)
                    };
                    match parsed {
                        Ok(pat) => ChainArgument::Pattern(pat),
                        Err(_) => {
                            if self.left.is_none() {
                                if inner_written.len > 0 {
                                    self.left = Some(
                                        inner_written.start.saturating_add(inner_written.len - 1),
                                    );
                                } else if call.written.len > 0 {
                                    self.left = Some(
                                        call.written.start.saturating_add(call.written.len - 1),
                                    );
                                }
                            }
                            return None;
                        }
                    }
                }
            }
            argument => {
                if let Some(wave) = self.try_eval_wave_expr(names, argument) {
                    wave
                } else {
                    match self.pattern_argument(names, argument, runtime) {
                        Some(value) => ChainArgument::Pattern(value),
                        None => {
                            if self.left.is_none() && call.written.len > 0 {
                                self.left =
                                    Some(call.written.start.saturating_add(call.written.len - 1));
                            }
                            return None;
                        }
                    }
                }
            }
        })
    }

    /// The pattern of a chain of control calls on `receiver`: each call as
    /// [`Self::method`] makes it, with the constants in a row set in one
    /// step. See [`rustel_core::controls::apply_chain`].
    fn control_chain_slice(
        &mut self,
        names: &Names<'_, '_>,
        calls: &[ControlCall<'p>],
        receiver: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        // Each call but the last is one step that no evaluation counted.
        self.steps += calls.len() - 1;
        if self.steps > MAX_STEPS {
            return None;
        }
        // A pattern, or the parameter of an arrow that is being compiled.
        enum Receiver {
            Pattern(Pattern),
            Parameter(Chain),
        }
        let receiver = match self.evaluate(names, receiver, runtime) {
            Some(Native::Pattern(pattern)) => Receiver::Pattern(pattern),
            Some(Native::Chain(parameter)) => Receiver::Parameter(parameter),
            _ => {
                if let Some(call) = calls.last()
                    && self.left.is_none()
                    && call.written.len > 0
                {
                    self.left = Some(call.written.start.saturating_add(call.written.len - 1));
                }
                return None;
            }
        };
        let finish_chain = |receiver: Receiver, chain: &mut [ChainCall]| -> Native<'p> {
            match receiver {
                Receiver::Pattern(pattern) => {
                    Native::Pattern(rustel_core::controls::apply_chain_owned(pattern, chain))
                }
                // The arrow applies the chain to each pattern in one step, as
                // a pattern takes it here.
                Receiver::Parameter(mut parameter) => {
                    let step = rustel_core::controls::chain_transformer(
                        "",
                        chain.iter_mut().map(|(spec, arg)| {
                            (*spec, std::mem::replace(arg, ChainArgument::Number(0.0)))
                        }),
                    );
                    parameter
                        .steps
                        .push(PatternTransformInstruction::Apply(step));
                    Native::Chain(parameter)
                }
            }
        };
        if let [single] = calls {
            let argument = self.eval_control_arg(names, single, runtime)?;
            let mut chain = [(single.spec, argument)];
            return Some(finish_chain(receiver, &mut chain));
        }
        if calls.len() <= 4 {
            let dummy = calls[0].spec;
            let mut chain: [ChainCall; 4] =
                std::array::from_fn(|_| (dummy, ChainArgument::Number(0.0)));
            let mut len = 0usize;
            for call in calls.iter().rev() {
                let argument = self.eval_control_arg(names, call, runtime)?;
                chain[len] = (call.spec, argument);
                len += 1;
            }
            return Some(finish_chain(receiver, &mut chain[..len]));
        }
        if calls.len() <= 8 {
            let dummy = calls[0].spec;
            let mut chain: [ChainCall; 8] =
                std::array::from_fn(|_| (dummy, ChainArgument::Number(0.0)));
            let mut len = 0usize;
            for call in calls.iter().rev() {
                let argument = self.eval_control_arg(names, call, runtime)?;
                chain[len] = (call.spec, argument);
                len += 1;
            }
            return Some(finish_chain(receiver, &mut chain[..len]));
        }
        const INLINE_CHAIN: usize = 20;
        if calls.len() <= INLINE_CHAIN {
            let dummy = calls[0].spec;
            let mut chain: [ChainCall; INLINE_CHAIN] =
                std::array::from_fn(|_| (dummy, ChainArgument::Number(0.0)));
            let mut len = 0usize;
            for call in calls.iter().rev() {
                let argument = self.eval_control_arg(names, call, runtime)?;
                chain[len] = (call.spec, argument);
                len += 1;
            }
            return Some(finish_chain(receiver, &mut chain[..len]));
        }
        let mut chain = self.spare.chains.pop().unwrap_or_default();
        chain.reserve(calls.len());
        let mut ok = true;
        for call in calls.iter().rev() {
            let Some(argument) = self.eval_control_arg(names, call, runtime) else {
                ok = false;
                break;
            };
            chain.push((call.spec, argument));
        }
        let made = ok.then(|| finish_chain(receiver, &mut chain));
        chain.clear();
        self.spare.chains.push(chain);
        made
    }

    /// Try to evaluate a control argument directly as an inline [`rustel_core::Wave`]
    /// without constructing intermediate signal, time, math, or segment pattern nodes.
    fn try_eval_wave_expr(
        &mut self,
        names: &Names<'_, '_>,
        expr: &'p ScoreExpr,
    ) -> Option<ChainArgument> {
        use rustel_core::compose::{Alignment, ComposeOp, default_alignment};
        use rustel_fraction::Fraction;

        if default_alignment() != Alignment::In {
            return None;
        }
        let mut times: [(bool, Fraction); 3] = [(false, Fraction::ZERO); 3];
        let mut times_len = 0usize;
        let mut op_flags = 0u8;
        let mut math_outer: [(ComposeOp, f64); 8] = [(ComposeOp::Add, 0.0); 8];
        let mut math_len = 0usize;
        let mut steps = 0u32;
        let mut math_depth: Option<u8> = None;
        let mut nodes = 0usize;
        let mut cur = expr;

        while let ScoreExpr::Method {
            receiver,
            method,
            arguments,
            ..
        } = cur
        {
            nodes += 1;
            let args = self.held.row(*arguments);
            let method_name = self.held.text(*method);
            match method_name {
                "seg" | "segment" => {
                    if steps != 0 || times_len != 0 {
                        return None;
                    }
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("segment") {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    if !n.is_finite() || n.fract() != 0.0 || *n < 2.0 || *n > u32::MAX as f64 {
                        return None;
                    }
                    steps = *n as u32;
                }
                "slow" | "sparsity" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("slow") {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    let f = Fraction::from_f64(*n)?;
                    if f <= Fraction::ZERO {
                        return None;
                    }
                    let inv = Fraction::ONE.checked_div(f)?;
                    if times_len >= times.len() {
                        return None;
                    }
                    times[times_len] = (true, inv);
                    op_flags |= 1u8 << times_len;
                    times_len += 1;
                }
                "fast" | "density" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("fast") {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    let f = Fraction::from_f64(*n)?;
                    if f <= Fraction::ZERO {
                        return None;
                    }
                    if times_len >= times.len() {
                        return None;
                    }
                    times[times_len] = (true, f);
                    times_len += 1;
                }
                "late" | "early" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some(method_name) {
                        return None;
                    }
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    let mut offset = Fraction::from_f64(*n)?;
                    if method_name == "early" {
                        offset = offset.checked_neg()?;
                    }
                    if times_len >= times.len() {
                        return None;
                    }
                    times[times_len] = (false, offset);
                    if method_name == "early" {
                        op_flags |= 1u8 << times_len;
                    }
                    times_len += 1;
                }
                "range" => {
                    let Some(BuiltinMethod::Registered(reg)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    if reg.names.first().map(|s| &**s) != Some("range") {
                        return None;
                    }
                    let [ScoreExpr::Number(min), ScoreExpr::Number(max)] = args else {
                        return None;
                    };
                    if math_len + 2 > math_outer.len() {
                        return None;
                    }
                    let depth = times_len as u8;
                    match math_depth {
                        None => math_depth = Some(depth),
                        Some(prev) if prev == depth => {}
                        _ => return None,
                    }
                    // Outer-to-inner: Add(min) wraps Mul(max - min).
                    math_outer[math_len] = (ComposeOp::Add, *min);
                    math_outer[math_len + 1] = (ComposeOp::Mul, *max - *min);
                    math_len += 2;
                }
                "add" | "sub" | "mul" | "div" => {
                    let Some(BuiltinMethod::Composer(op)) = builtin_method(names, method_name)
                    else {
                        return None;
                    };
                    let [ScoreExpr::Number(n)] = args else {
                        return None;
                    };
                    if math_len >= math_outer.len() {
                        return None;
                    }
                    let depth = times_len as u8;
                    match math_depth {
                        None => math_depth = Some(depth),
                        Some(prev) if prev == depth => {}
                        _ => return None,
                    }
                    math_outer[math_len] = (op, *n);
                    math_len += 1;
                }
                _ => return None,
            }
            cur = self.held.at(*receiver);
        }

        let ScoreExpr::Name(signal_name) = cur else {
            return None;
        };
        let signal_name = self.held.text(*signal_name);
        nodes += 1;
        if !matches!(self.resolve(signal_name), Resolved::Host) {
            return None;
        }
        let body = rustel_core::signal::static_signal_body(signal_name)?;
        if !names.is_signal(signal_name) {
            return None;
        }
        let math_inside = match math_depth {
            None => true,
            Some(d) if usize::from(d) == times_len => true,
            Some(0) => false,
            _ => return None,
        };
        math_outer[..math_len].reverse();
        let wave = rustel_core::Wave::from_inline_parts(
            body,
            &times[..times_len],
            &math_outer[..math_len],
            math_inside,
        )?;
        if steps > 0 && wave.has_cuts() {
            return None;
        }
        if self.steps.saturating_add(nodes) > MAX_STEPS {
            return None;
        }
        self.steps += nodes;
        Some(ChainArgument::Wave(wave, steps, math_inside, op_flags))
    }

    /// Evaluate `expression` where the score writes a part. The build
    /// takes the pattern that the build before made of the same call, or
    /// evaluates the call and keeps its pattern. See [`memo`].
    fn part(
        &mut self,
        names: &Names<'_, '_>,
        expression: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        self.part_maybe_keep(names, expression, runtime, true)
    }

    fn part_maybe_keep(
        &mut self,
        names: &Names<'_, '_>,
        expression: &'p ScoreExpr,
        runtime: &JsRuntime,
        keep: bool,
    ) -> Option<Native<'p>> {
        let (ScoreExpr::Call { written, .. } | ScoreExpr::Method { written, .. }) = expression
        else {
            return self.evaluate(names, expression, runtime);
        };
        let Some(reuse) = self.reuse.as_mut().filter(|_| written.len > 0) else {
            return self.evaluate(names, expression, runtime);
        };
        if self.frames.is_empty()
            && let Some(pattern) = reuse.find(written)
        {
            return Some(Native::Pattern(pattern));
        }
        if !keep {
            return self.evaluate(names, expression, runtime);
        }
        reuse.begin(self.frames.len());
        let value = self.evaluate(names, expression, runtime);
        let reuse = self.reuse.as_mut()?;
        let read = reuse.end();
        let value = value?;
        if let Native::Pattern(pattern) = &value {
            reuse.keep(*written, pattern, read);
        }
        Some(value)
    }

    /// The build ran an effect of the score.
    fn effect(&mut self) {
        if let Some(reuse) = &mut self.reuse {
            reuse.effect();
        }
    }

    fn method(
        &mut self,
        names: &Names<'_, '_>,
        receiver: Pattern,
        method: &str,
        arguments: &'p [ScoreExpr],
        pre_resolved: Option<Option<BuiltinMethod>>,
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        if method == LANE_METHOD {
            return self.lane(names, receiver, arguments);
        }
        let builtin = match pre_resolved {
            Some(resolved) => resolved?,
            None => builtin_method(names, method)?,
        };
        match &builtin {
            BuiltinMethod::Pick { index, join } => {
                return self.pick(names, receiver, (*index, *join), arguments, runtime);
            }
            BuiltinMethod::Visual => return self.visual(names, receiver, arguments, runtime),
            BuiltinMethod::Layer { keep_original } => {
                return self.layer(names, receiver, *keep_original, arguments, runtime);
            }
            _ => {}
        }
        // A registration reads the value of each pure argument. It takes
        // numbers, single-word mini literals, and trailing arrow transformers
        // directly without wrapping each argument in a Pattern.
        if let BuiltinMethod::Composer(op) = &builtin
            && let [only] = arguments
        {
            return match self.evaluate(names, only, runtime)? {
                Native::Number(n) => {
                    Some(Native::Pattern(rustel_core::compose::compose_number_owned(
                        receiver,
                        n,
                        *op,
                        rustel_core::compose::default_alignment(),
                    )))
                }
                other => {
                    let arg = other.reified()?;
                    Some(Native::Pattern(rustel_core::compose::compose_owned(
                        receiver,
                        &arg,
                        *op,
                        rustel_core::compose::default_alignment(),
                    )))
                }
            };
        }
        let mut eval_last: Option<Native<'p>> = None;
        if let BuiltinMethod::Registered(registration) = &builtin
            && arguments.len() == registration.arity.saturating_sub(1)
        {
            if arguments.len() <= 4 {
                let takes_transformer =
                    registration.function_argument == FunctionArgument::Transformer;
                let allow_mini_word = !registration.is_native_patterned();
                let (last, leading) = match arguments.split_last() {
                    Some((last, leading)) => (Some(last), leading),
                    None => (None, arguments),
                };
                let mut stack_vals = [const { Value::Undefined }; 4];
                let mut direct_func: Option<FunctionRef> = None;
                let mut mini_steps = 0usize;
                let mut direct = true;
                let mut try_literal = |argument: &'p ScoreExpr, slot: &mut Value| -> bool {
                    match argument {
                        ScoreExpr::Number(number) => {
                            *slot = Value::F64(*number);
                            true
                        }
                        ScoreExpr::Mini { source, offset }
                            if allow_mini_word
                                && names.mini()
                                && self.mini_is_host()
                                && let Some((constant, _)) = rustel_mini::word_constant_parts(
                                    self.held.text(*source),
                                    *offset as usize,
                                ) =>
                        {
                            *slot = match constant {
                                rustel_mini::WordConstant::One(value) => value,
                                rustel_mini::WordConstant::Pair(Value::Str(a), Value::Str(b))
                                    if method == "scale" && a.len() + 1 + b.len() <= 64 =>
                                {
                                    let mut buf = [0u8; 64];
                                    let al = a.len();
                                    buf[..al].copy_from_slice(a.as_bytes());
                                    buf[al] = b' ';
                                    buf[al + 1..al + 1 + b.len()].copy_from_slice(b.as_bytes());
                                    let joined = std::str::from_utf8(&buf[..al + 1 + b.len()])
                                        .expect("valid utf8");
                                    Value::Str(joined.into())
                                }
                                rustel_mini::WordConstant::Pair(a, b) => {
                                    Value::List(std::sync::Arc::from([a, b]))
                                }
                                rustel_mini::WordConstant::Many(list) => Value::List(list),
                            };
                            mini_steps += 1;
                            true
                        }
                        ScoreExpr::Call {
                            callee, arguments, ..
                        } if allow_mini_word
                            && self.held.text(*callee) == MINI_CALLEE
                            && names.mini()
                            && self.mini_is_host()
                            && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] =
                                self.held.row(*arguments)
                            && offset.fract() == 0.0
                            && offset.is_finite()
                            && let Ok(at) = usize::try_from(*offset as i64)
                            && let Some((constant, _)) =
                                rustel_mini::word_constant_parts(self.held.text(*source), at) =>
                        {
                            *slot = match constant {
                                rustel_mini::WordConstant::One(value) => value,
                                rustel_mini::WordConstant::Pair(Value::Str(a), Value::Str(b))
                                    if method == "scale" && a.len() + 1 + b.len() <= 64 =>
                                {
                                    let mut buf = [0u8; 64];
                                    let al = a.len();
                                    buf[..al].copy_from_slice(a.as_bytes());
                                    buf[al] = b' ';
                                    buf[al + 1..al + 1 + b.len()].copy_from_slice(b.as_bytes());
                                    let joined = std::str::from_utf8(&buf[..al + 1 + b.len()])
                                        .expect("valid utf8");
                                    Value::Str(joined.into())
                                }
                                rustel_mini::WordConstant::Pair(a, b) => {
                                    Value::List(std::sync::Arc::from([a, b]))
                                }
                                rustel_mini::WordConstant::Many(list) => Value::List(list),
                            };
                            mini_steps += 1;
                            true
                        }
                        _ => false,
                    }
                };
                for (slot, argument) in stack_vals.iter_mut().zip(leading) {
                    if !try_literal(argument, slot) {
                        direct = false;
                        break;
                    }
                }
                if direct
                    && let Some(last) = last
                    && !try_literal(last, &mut stack_vals[leading.len()])
                {
                    if takes_transformer {
                        let evaluated = self.evaluate(names, last, runtime)?;
                        if let Native::Function(transformer) = evaluated {
                            direct_func = Some(transformer);
                        } else {
                            eval_last = Some(evaluated);
                            direct = false;
                        }
                    } else {
                        direct = false;
                    }
                }
                if direct {
                    if mini_steps > 0 {
                        self.steps += mini_steps;
                        if self.steps > MAX_STEPS {
                            return None;
                        }
                    }
                    let made = match direct_func {
                        Some(func) => registration.call_with_transformer(
                            &stack_vals[..leading.len()],
                            &func,
                            receiver,
                        ),
                        None => registration
                            .call_unpatternified(&stack_vals[..arguments.len()], receiver),
                    };
                    return Some(Native::Pattern(made));
                }
            } else {
                let mut numbers = std::mem::take(&mut self.numbers);
                numbers.clear();
                numbers.extend(arguments.iter().map_while(|argument| match argument {
                    ScoreExpr::Number(number) => Some(Value::F64(*number)),
                    _ => None,
                }));
                if numbers.len() == arguments.len() {
                    let made = registration.call_unpatternified(&numbers, receiver);
                    self.numbers = numbers;
                    return Some(Native::Pattern(made));
                }
                self.numbers = numbers;
            }
        }
        if arguments.len() <= 4 {
            let takes_transformer = match &builtin {
                BuiltinMethod::Registered(registration) => {
                    if arguments.len() != registration.arity.saturating_sub(1) {
                        return None;
                    }
                    registration.function_argument == FunctionArgument::Transformer
                }
                _ => false,
            };
            let mut buf: [Option<Pattern>; 4] = [None, None, None, None];
            let (last, leading) = match arguments.split_last() {
                Some((last, leading)) => (Some(last), leading),
                None => (None, arguments),
            };
            for (slot, argument) in buf.iter_mut().zip(leading) {
                *slot = Some(self.pattern_argument(names, argument, runtime)?);
            }
            if let Some(last) = last {
                let evaluated = match eval_last {
                    Some(v) => v,
                    None => self.evaluate(names, last, runtime)?,
                };
                buf[leading.len()] = Some(match evaluated {
                    Native::Function(transformer) if takes_transformer => {
                        rustel_core::pure(Value::function(transformer))
                    }
                    value => value.reified()?,
                });
            }
            return builtin
                .apply_small(receiver, &mut buf[..arguments.len()])
                .map(Native::Pattern);
        }
        let (values, _) = self.method_arguments(names, &builtin, arguments, runtime)?;
        builtin.apply(receiver, values).map(Native::Pattern)
    }

    /// `pattern.layer(...transformers)` or `superimpose`: a stack of what
    /// each transformer makes of the pattern. The method throws for no
    /// argument and for one that is no transformer.
    fn layer(
        &mut self,
        names: &Names<'_, '_>,
        pattern: Pattern,
        keep_original: bool,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        if arguments.is_empty() {
            return None;
        }
        let mut transformers = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let Native::Function(transformer) = self.evaluate(names, argument, runtime)? else {
                return None;
            };
            transformers.push(transformer);
        }
        Some(Native::Pattern(if keep_original {
            rustel_core::combinators::superimpose(&pattern, &transformers)
        } else {
            rustel_core::combinators::layer(&pattern, &transformers)
        }))
    }

    /// One more method call on an arrow's parameter: the step that applies
    /// the method to any pattern as [`Self::method`] applies it to one.
    fn chained(
        &mut self,
        names: &Names<'_, '_>,
        mut chain: Chain,
        method: &str,
        arguments: &'p [ScoreExpr],
        pre_resolved: Option<Option<BuiltinMethod>>,
        runtime: &JsRuntime,
    ) -> Option<Chain> {
        let builtin = match pre_resolved {
            Some(resolved) => resolved?,
            None => builtin_method(names, method)?,
        };
        if let BuiltinMethod::Control(spec) = &builtin
            && let [only] = arguments
        {
            match self.evaluate(names, only, runtime)? {
                Native::Number(n) => {
                    chain.steps.push(PatternTransformInstruction::Apply(
                        rustel_core::value::FunctionRef::control_number(method, spec, n),
                    ));
                    return Some(chain);
                }
                other => {
                    let arg = other.reified()?;
                    chain.steps.push(builtin.step_args(
                        method,
                        crate::MethodArgs::One(arg),
                        false,
                    )?);
                    return Some(chain);
                }
            }
        }
        if let BuiltinMethod::Composer(op) = &builtin
            && let [only] = arguments
        {
            match self.evaluate(names, only, runtime)? {
                Native::Number(n) => {
                    chain.steps.push(PatternTransformInstruction::Apply(
                        rustel_core::value::FunctionRef::compose_number(method, *op, n, None),
                    ));
                    return Some(chain);
                }
                other => {
                    let arg = other.reified()?;
                    chain.steps.push(builtin.step_args(
                        method,
                        crate::MethodArgs::One(arg),
                        false,
                    )?);
                    return Some(chain);
                }
            }
        }
        // Fast path for non-patterned 1-arg registered methods called with a
        // scalar or single-word mini literal inside an arrow chain (e.g.
        // `x => x.fast(2)` or `x => x.scale("c:minor")`): avoids wrapping the
        // literal in a temporary `core::pure` `NodeCell`.
        if let BuiltinMethod::Registered(registration) = &builtin
            && !registration.is_native_patterned()
            && registration.arity == 2
            && let [only] = arguments
        {
            let mut mini_step = false;
            let scalar = match only {
                ScoreExpr::Number(n) => Some(Value::F64(*n)),
                ScoreExpr::String(s) if names.raw_strings() => {
                    Some(Value::Str(self.held.text(*s).into()))
                }
                ScoreExpr::Mini { source, offset }
                    if names.mini()
                        && self.mini_is_host()
                        && let Some((constant, _)) = rustel_mini::word_constant_parts(
                            self.held.text(*source),
                            *offset as usize,
                        ) =>
                {
                    mini_step = true;
                    Some(match constant {
                        rustel_mini::WordConstant::One(value) => value,
                        rustel_mini::WordConstant::Pair(Value::Str(a), Value::Str(b))
                            if method == "scale" && a.len() + 1 + b.len() <= 64 =>
                        {
                            let mut buf = [0u8; 64];
                            let al = a.len();
                            buf[..al].copy_from_slice(a.as_bytes());
                            buf[al] = b' ';
                            buf[al + 1..al + 1 + b.len()].copy_from_slice(b.as_bytes());
                            let joined =
                                std::str::from_utf8(&buf[..al + 1 + b.len()]).expect("valid utf8");
                            Value::Str(joined.into())
                        }
                        rustel_mini::WordConstant::Pair(a, b) => {
                            Value::List(std::sync::Arc::from([a, b]))
                        }
                        rustel_mini::WordConstant::Many(list) => Value::List(list),
                    })
                }
                ScoreExpr::Call {
                    callee, arguments, ..
                } if self.held.text(*callee) == MINI_CALLEE
                    && names.mini()
                    && self.mini_is_host()
                    && let [ScoreExpr::String(source), ScoreExpr::Number(offset)] =
                        self.held.row(*arguments)
                    && offset.fract() == 0.0
                    && offset.is_finite()
                    && let Ok(at) = usize::try_from(*offset as i64)
                    && let Some((constant, _)) =
                        rustel_mini::word_constant_parts(self.held.text(*source), at) =>
                {
                    mini_step = true;
                    Some(match constant {
                        rustel_mini::WordConstant::One(value) => value,
                        rustel_mini::WordConstant::Pair(Value::Str(a), Value::Str(b))
                            if method == "scale" && a.len() + 1 + b.len() <= 64 =>
                        {
                            let mut buf = [0u8; 64];
                            let al = a.len();
                            buf[..al].copy_from_slice(a.as_bytes());
                            buf[al] = b' ';
                            buf[al + 1..al + 1 + b.len()].copy_from_slice(b.as_bytes());
                            let joined =
                                std::str::from_utf8(&buf[..al + 1 + b.len()]).expect("valid utf8");
                            Value::Str(joined.into())
                        }
                        rustel_mini::WordConstant::Pair(a, b) => {
                            Value::List(std::sync::Arc::from([a, b]))
                        }
                        rustel_mini::WordConstant::Many(list) => Value::List(list),
                    })
                }
                _ => match self.evaluate(names, only, runtime)? {
                    Native::Number(n) => Some(Value::F64(n)),
                    other => {
                        let arg = other.reified()?;
                        chain.steps.push(builtin.step_args(
                            method,
                            crate::MethodArgs::One(arg),
                            false,
                        )?);
                        return Some(chain);
                    }
                },
            };
            if let Some(val) = scalar {
                if mini_step {
                    self.steps += 1;
                    if self.steps > MAX_STEPS {
                        return None;
                    }
                }
                let func = crate::bound_registered_transformer_scalar("", registration, val);
                chain.steps.push(PatternTransformInstruction::Apply(func));
                return Some(chain);
            }
        }
        let (values, holds_transformer) =
            self.method_arguments(names, &builtin, arguments, runtime)?;
        chain
            .steps
            .push(builtin.step_args(method, values, holds_transformer)?);
        Some(chain)
    }

    /// The arguments of a method call as the method reifies them, and
    /// whether the last one is a transformer.
    ///
    /// A registered method that sequences or refuses its arguments shapes
    /// JavaScript values the native tier does not hold, so it takes its
    /// exact count only. A transformer is an argument only where a
    /// registered method takes one, as its last argument.
    fn method_arguments(
        &mut self,
        names: &Names<'_, '_>,
        builtin: &BuiltinMethod,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<(crate::MethodArgs, bool)> {
        use crate::MethodArgs;
        let takes_transformer = match builtin {
            BuiltinMethod::Registered(registration) => {
                if arguments.len() != registration.arity.saturating_sub(1) {
                    return None;
                }
                registration.function_argument == FunctionArgument::Transformer
            }
            _ => false,
        };
        let eval_last = |turn: &mut Self,
                         expr: &'p ScoreExpr,
                         holds_transformer: &mut bool|
         -> Option<Pattern> {
            Some(match turn.evaluate(names, expr, runtime)? {
                Native::Function(transformer) if takes_transformer => {
                    *holds_transformer = true;
                    rustel_core::pure(Value::function(transformer))
                }
                value => value.reified()?,
            })
        };
        let mut holds_transformer = false;
        match arguments {
            [] => Some((MethodArgs::Zero, false)),
            [only] => {
                let first = eval_last(self, only, &mut holds_transformer)?;
                Some((MethodArgs::One(first), holds_transformer))
            }
            [first, second] => {
                let a = self.pattern_argument(names, first, runtime)?;
                let b = eval_last(self, second, &mut holds_transformer)?;
                Some((MethodArgs::Two([a, b]), holds_transformer))
            }
            _ => {
                let (last, leading) = arguments.split_last()?;
                let mut values = Vec::with_capacity(arguments.len());
                for argument in leading {
                    values.push(self.pattern_argument(names, argument, runtime)?);
                }
                values.push(eval_last(self, last, &mut holds_transformer)?);
                Some((MethodArgs::Many(values), holds_transformer))
            }
        }
    }

    /// `selector.pick(lookup)` with one object or array, as the method
    /// builds it: the properties or elements in order, each value reified.
    fn pick(
        &mut self,
        names: &Names<'_, '_>,
        selector: Pattern,
        (index, join): (rustel_core::PickIndexMode, rustel_core::JoinMode),
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let [lookup] = arguments else {
            return None;
        };
        let pattern = match self.evaluate(names, lookup, runtime)? {
            Native::Object(properties) => {
                let lookup = rustel_core::PickLookup::Object {
                    enumerable_len: properties.len(),
                    entries: properties
                        .iter()
                        .map(|(key, value)| Some(((*key).into(), value.reified()?)))
                        .collect::<Option<_>>()?,
                };
                rustel_core::pick(selector, lookup, index, join)
            }
            Native::Array(elements) => {
                let mut patterns = Vec::with_capacity(elements.len());
                for value in elements.iter() {
                    patterns.push(value.reified()?);
                }
                rustel_core::pick_dense(selector, patterns.into_boxed_slice(), index, join)
            }
            _ => return None,
        };
        Some(Native::Pattern(pattern))
    }

    /// `pattern.pianoroll(id, slot)` and every other visual: the pattern
    /// tagged with the slot the second argument names, or as it is.
    fn visual(
        &mut self,
        names: &Names<'_, '_>,
        pattern: Pattern,
        arguments: &'p [ScoreExpr],
        runtime: &JsRuntime,
    ) -> Option<Native<'p>> {
        let slot = match arguments {
            [] | [ScoreExpr::String(_)] => None,
            [ScoreExpr::String(_), slot] => match self.evaluate(names, slot, runtime)? {
                Native::Number(slot) => crate::install::visual_slot(slot),
                _ => None,
            },
            _ => return None,
        };
        Some(Native::Pattern(match slot {
            Some(slot) => pattern.with_ui_visual_slot_owned(slot),
            None => pattern,
        }))
    }

    /// An argument as the surface reifies it. See [`Native::reified`].
    fn pattern_argument(
        &mut self,
        names: &Names<'_, '_>,
        argument: &'p ScoreExpr,
        runtime: &JsRuntime,
    ) -> Option<Pattern> {
        self.evaluate(names, argument, runtime)?.reified()
    }

    /// `pattern.p(id, number)`: collect a lane as the REPL's lane method
    /// does. A muted lane plays nothing. A solo lane, a name a JavaScript
    /// object would order apart, and a name used twice are left to
    /// JavaScript.
    fn lane(
        &mut self,
        names: &Names<'_, '_>,
        pattern: Pattern,
        arguments: &'p [ScoreExpr],
    ) -> Option<Native<'p>> {
        use std::fmt::Write as _;
        let [ScoreExpr::String(id), ScoreExpr::Number(_)] = arguments else {
            return None;
        };
        let id = self.held.text(*id);
        if self.compiling() || !names.lane() {
            return None;
        }
        if id.starts_with('_') || id.ends_with('_') {
            return Some(Native::Pattern(rustel_core::silence()));
        }
        let name = if id == "$" {
            let n = self.anonymous;
            self.anonymous = self.anonymous.saturating_add(1);
            if self.lanes.has_anon(n) {
                return None;
            }
            LaneName::AnonDollar(n)
        } else if id.contains('$') {
            let mut owned = String::with_capacity(id.len() + 4);
            owned.push_str(id);
            let _ = write!(&mut owned, "{}", self.anonymous);
            self.anonymous = self.anonymous.saturating_add(1);
            let solo = owned.len() > 1 && owned.starts_with('S');
            let integer_like = owned.parse::<u32>().is_ok();
            if solo || integer_like || self.lanes.has_named(&owned) {
                return None;
            }
            LaneName::Owned(owned)
        } else {
            let solo = id.len() > 1 && id.starts_with('S');
            let integer_like = id.parse::<u32>().is_ok();
            if solo || integer_like || self.lanes.has_named(id) {
                return None;
            }
            LaneName::Named(id)
        };
        self.lanes.push(Lane {
            name,
            pattern: pattern.clone(),
        });
        self.effect();
        Some(Native::Pattern(pattern))
    }
}

/// The REPL's lane method on the Pattern prototype.
const LANE_METHOD: &str = "p";

/// Methods that route notes to a plugin. A plugin lane has a shadow and plugin
/// effects that only the JavaScript tier stages.
const PLUGIN_METHODS: &[&str] = &["vst", "vsti"];

/// The built-in method `name` names, unless it takes a route of its own.
/// The inventory lists no method with such a route, so the names of a
/// realm alone are read against those routes.
#[inline]
fn builtin_method(names: &Names<'_, '_>, name: &str) -> Option<BuiltinMethod> {
    match names {
        Names::Listed(listed) => listed.method(name),
        Names::Realm(ctx) => {
            if SPECIAL_REGISTERED_METHODS.contains(&name) || PLUGIN_METHODS.contains(&name) {
                return None;
            }
            prove_builtin_method(ctx, name)
        }
    }
}

/// What a list constructor builds, as the JavaScript constructor builds it.
fn list(kind: ListKind, mut patterns: Vec<Pattern>) -> Option<Pattern> {
    if patterns.is_empty() {
        return None;
    }
    Some(match kind {
        ListKind::Stack => rustel_core::stack(patterns),
        // One value is the value itself.
        ListKind::SlowCat | ListKind::FastCat if patterns.len() == 1 => {
            patterns.pop().expect("one list value")
        }
        ListKind::SlowCat => rustel_core::slowcat(patterns),
        ListKind::FastCat => rustel_core::fastcat(patterns),
    })
}

fn list_slice(kind: ListKind, patterns: &mut [Option<Pattern>]) -> Option<Pattern> {
    if patterns.is_empty() {
        return None;
    }
    Some(match kind {
        ListKind::Stack => rustel_core::stack_slice(patterns),
        ListKind::SlowCat | ListKind::FastCat if patterns.len() == 1 => {
            patterns[0].take().expect("one list value")
        }
        ListKind::SlowCat => rustel_core::slowcat_slice(patterns),
        ListKind::FastCat => rustel_core::fastcat_slice(patterns),
    })
}

/// Whether a score set a gain curve, which every later score's lanes go
/// through in JavaScript.
fn gain_curve_set(ctx: &Ctx<'_>) -> bool {
    ctx.globals()
        .get::<_, rquickjs::Value>(crate::install::native_surface::bindings::GAIN_CURVE)
        .map_or(true, |curve| !curve.is_undefined())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_binding_to_shadow_host_name_does_not_reuse_stale_pattern() {
        let runtime = JsRuntime::new().expect("runtime");
        let opts = rustel_transpiler::TranspileOptions::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let budget = std::time::Duration::from_secs(5);

        runtime
            .evaluate_score_cancellable("const x = 1\n$: s(\"bd\")", &opts, budget, &cancel)
            .expect("initial score evaluates natively");
        assert!(runtime.evaluated_natively());

        let err =
            runtime.evaluate_score_cancellable("const s = 1\n$: s(\"bd\")", &opts, budget, &cancel);
        assert!(
            err.is_err(),
            "shadowing host `s` on edit must fail like cold eval, not reuse stale pattern"
        );

        let rt2 = JsRuntime::new().expect("runtime");
        rt2.evaluate_score_cancellable(
            "function f(x) { return s(\"bd\"); }\n$: f(1)",
            &opts,
            budget,
            &cancel,
        )
        .expect("initial function score evaluates");
        let err2 = rt2.evaluate_score_cancellable(
            "function f(s) { return s(\"bd\"); }\n$: f(1)",
            &opts,
            budget,
            &cancel,
        );
        assert!(
            err2.is_err(),
            "shadowing host `s` in parameter edit must fail like cold eval"
        );
    }

    #[test]
    fn native_tier_respects_set_string_parser_on_subsequent_scores() {
        let runtime = JsRuntime::new().expect("runtime");
        let opts = rustel_transpiler::TranspileOptions::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let budget = std::time::Duration::from_secs(5);

        runtime
            .evaluate_score_cancellable(
                "setStringParser(mini)\n$: s(\"bd\")",
                &opts,
                budget,
                &cancel,
            )
            .expect("setStringParser score evaluates");

        runtime
            .evaluate_score_cancellable("$: s(\"bd\").n('0 1')", &opts, budget, &cancel)
            .expect("subsequent score with single-quoted string evaluates");
        let haps = runtime.active_pattern().expect("active pattern").query_arc(
            rustel_fraction::Fraction::ZERO,
            rustel_fraction::Fraction::ONE,
        );
        assert_eq!(
            haps.len(),
            2,
            "setStringParser(mini) must split `'0 1'` into two events"
        );

        runtime
            .evaluate_score_cancellable(
                "$: s(\"bd\").every(1, x => x.n('0 1'))",
                &opts,
                budget,
                &cancel,
            )
            .expect("arrow callback with single-quoted string evaluates");
        let haps = runtime.active_pattern().expect("active pattern").query_arc(
            rustel_fraction::Fraction::ZERO,
            rustel_fraction::Fraction::ONE,
        );
        assert_eq!(
            haps.len(),
            2,
            "setStringParser(mini) must split `'0 1'` in arrow callbacks too"
        );
    }
}

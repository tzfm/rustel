/*
table.rs - stateless signal walks for value sides and compatibility stubs
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Stateless signal helpers for direct graph walks (`ValueSignal`, `Signal`,
//! `Gate`, `Named`) and zero-cost compatibility stubs for the removed
//! precomputed answer-table API.

use std::sync::Arc;

use rustel_fraction::Fraction;

use crate::{
    Combine, Hap, HapMapFn, LookupFlow, Meeting, Node, OwnedHapMapFn, Pattern, SignalFn, SpanUse,
    State, TimeSpan, ValueFn, ValuePredicate, applied_hap,
};

pub(crate) use crate::fused::{Wave, wave_name};

/// Default byte budget for kept answer tables (compatibility stub; direct
/// walks keep no precomputed tables).
pub const DEFAULT_TABLE_BUDGET: usize = 32 << 20;

/// Compatibility stub; direct walks keep no precomputed tables.
pub fn set_table_budget(_bytes: usize) {}

/// Total bytes held in precomputed answer tables (always zero).
pub fn table_bytes() -> usize {
    0
}

/// The order a query asks for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Graph,
    Sorted,
}

const MAX_RESERVED: usize = 64;
const MAX_SIGNAL_DEPTH: u32 = 128;

thread_local! {
    static HAP_BUFFERS: std::cell::RefCell<Vec<Vec<Hap>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Pooled scratch buffer for signal walks, returned on drop without locks.
pub(super) struct HapBuffer(Vec<Hap>);

impl HapBuffer {
    pub(super) fn take() -> Self {
        let pooled = HAP_BUFFERS.with(|pool| pool.borrow_mut().pop());
        Self(pooled.unwrap_or_default())
    }
}

impl Drop for HapBuffer {
    fn drop(&mut self) {
        const MOST_BUFFERS: usize = 8;
        if self.0.capacity() <= MAX_RESERVED {
            self.0.clear();
            let _ = HAP_BUFFERS.try_with(|pool| {
                let mut pool = pool.borrow_mut();
                if pool.len() < MOST_BUFFERS {
                    pool.push(std::mem::take(&mut self.0));
                }
            });
        }
    }
}

impl std::ops::Deref for HapBuffer {
    type Target = Vec<Hap>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for HapBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Clone, Copy, Debug)]
enum Side {
    /// `appRight`: the signal gives the function values.
    Function,
    /// `appLeft`: the signal gives the argument values.
    Value,
}

/// A per-hap change that does not look at time.
#[derive(Clone)]
enum Step {
    Map(ValueFn),
    Keep(ValuePredicate),
    MapHap(HapMapFn),
    MapOwned(OwnedHapMapFn),
    Control(u16, bool),
    Locations(Arc<[(usize, usize)]>),
}

impl Step {
    #[inline]
    fn run_in_place(&self, hap: &mut Hap) -> bool {
        match self {
            Step::Map(f) => {
                hap.map_value_in_place(|value| f(value));
                true
            }
            Step::Keep(keep) => keep(&hap.value),
            Step::MapHap(f) => Self::replaced(hap, f(hap)),
            Step::MapOwned(f) => Self::run_owned(f, hap),
            Step::Control(id, unnamed_only) => {
                let Some(spec) = crate::controls::builtin_spec_by_id(*id) else {
                    return true;
                };
                let owned =
                    std::mem::replace(hap, Hap::new(None, hap.part, crate::Value::Undefined));
                Self::replaced(
                    hap,
                    crate::controls::apply_control_hap(spec, *unnamed_only, owned),
                )
            }
            Step::Locations(locations) => {
                hap.context.extend_from_slice(locations);
                true
            }
        }
    }

    fn replaced(hap: &mut Hap, mapped: Option<Hap>) -> bool {
        match mapped {
            Some(mapped) => {
                *hap = mapped;
                true
            }
            None => false,
        }
    }

    #[inline(never)]
    fn run_owned(f: &OwnedHapMapFn, hap: &mut Hap) -> bool {
        let owned = std::mem::replace(hap, Hap::new(None, hap.part, crate::Value::Undefined));
        Self::replaced(hap, f(owned))
    }
}

/// One node between a signal and its meeting.
#[derive(Clone)]
enum SignalOp {
    Step(Step),
    Fast(Fraction),
    Shift(Fraction),
    Constant {
        hap: Box<Hap>,
        side: Side,
        apply: Combine,
        flow: LookupFlow,
    },
}

/// A signal and the nodes above it, innermost first, evaluated directly without
/// recursing through the pattern graph.
#[derive(Clone)]
pub(super) struct Signal {
    source: SignalFn,
    ops: Vec<SignalOp>,
}

impl Signal {
    /// Sample this signal over `span` into `out` (cleared first). Returns
    /// `None` when a time leaves the native range or exceeds query limits so
    /// the graph handles the refusal.
    pub(super) fn answer(
        &self,
        span: TimeSpan,
        _part: TimeSpan,
        probe: &mut State,
        out: &mut Vec<Hap>,
    ) -> Option<()> {
        let scaled = |span: TimeSpan, factor: Fraction| {
            Some(TimeSpan::new(
                span.begin.checked_mul(factor)?,
                span.end.checked_mul(factor)?,
            ))
        };
        let moved = |span: TimeSpan, by: Fraction| {
            Some(TimeSpan::new(
                span.begin.checked_sub(by)?,
                span.end.checked_sub(by)?,
            ))
        };
        let mut asked = span;
        for op in self.ops.iter().rev() {
            match op {
                SignalOp::Fast(factor) => asked = scaled(asked, *factor)?,
                SignalOp::Shift(by) => asked = moved(asked, *by)?,
                SignalOp::Step(_) | SignalOp::Constant { .. } => {}
            }
        }
        out.clear();
        probe.span = asked;
        let value = (self.source)(probe);
        out.push(Hap::new(None, asked, value));

        for op in &self.ops {
            match op {
                SignalOp::Step(step) => out.retain_mut(|hap| step.run_in_place(hap)),
                SignalOp::Fast(factor) => {
                    for hap in out.iter_mut() {
                        retime(hap, |t| t.checked_div(*factor))?;
                    }
                }
                SignalOp::Shift(by) => {
                    for hap in out.iter_mut() {
                        retime(hap, |t| t.checked_add(*by))?;
                    }
                }
                SignalOp::Constant {
                    hap: constant,
                    side,
                    apply,
                    flow,
                } => {
                    if out.iter().all(|hap| in_one_cycle(hap.part)) {
                        for hap in out.iter_mut() {
                            let part = hap.part;
                            if !(matches!(side, Side::Value)
                                && Meeting::in_place(hap, constant, part, apply, *flow))
                            {
                                *hap = match side {
                                    Side::Value => {
                                        applied_hap(hap, constant, hap.whole, part, apply, *flow)
                                    }
                                    Side::Function => {
                                        applied_hap(constant, hap, hap.whole, part, apply, *flow)
                                    }
                                };
                            }
                        }
                        continue;
                    }
                    let mut met = Vec::with_capacity(out.len());
                    for hap in out.iter() {
                        let count = hap.part.cycle_count()?;
                        if count > crate::MAX_QUERY_SPAN_CYCLES
                            || crate::hap_budget().is_some_and(|budget| {
                                met.len() as i128 + count > i128::from(budget)
                            })
                        {
                            return None;
                        }
                        let first = hap.part.begin.floor().numer();
                        let last = hap.part.end.ceil().numer().checked_sub(1)?;
                        for cycle in first..=last {
                            let mut other = shifted(constant, cycle)?;
                            let Some(part) = other.part.intersection(&hap.part) else {
                                continue;
                            };
                            other.part = part;
                            met.push(match side {
                                Side::Value => {
                                    applied_hap(hap, &other, hap.whole, part, apply, *flow)
                                }
                                Side::Function => {
                                    applied_hap(&other, hap, hap.whole, part, apply, *flow)
                                }
                            });
                        }
                    }
                    *out = met;
                }
            }
        }
        Some(())
    }
}

/// Extract a stateless [`Signal`] from a pattern made of a signal under
/// span-blind steps, speed changes, shifts, and meetings with constants.
pub(super) fn signal_of(pattern: &Pattern) -> Option<Signal> {
    signal_of_depth(crate::NodeRef::Pat(pattern), 0)
}

fn signal_of_ref(pattern: crate::NodeRef<'_>) -> Option<Signal> {
    signal_of_depth(pattern, 0)
}

fn signal_of_depth(pattern: crate::NodeRef<'_>, depth: u32) -> Option<Signal> {
    if depth >= MAX_SIGNAL_DEPTH || !pattern.tables_may_answer() {
        return None;
    }
    let (inner, op) = match pattern.kind() {
        Node::Signal(signal) => {
            return Some(Signal {
                source: signal.clone(),
                ops: Vec::new(),
            });
        }
        Node::FmapNative(boxed) => (
            crate::NodeRef::Pat(&boxed.0),
            SignalOp::Step(Step::Map(boxed.1.clone())),
        ),
        Node::FilterValues(boxed) => (
            crate::NodeRef::Pat(&boxed.0),
            SignalOp::Step(Step::Keep(boxed.1.clone())),
        ),
        Node::ControlMap(cell, flags, id, unnamed_only) => (
            crate::NodeRef::Plain(cell, *flags),
            SignalOp::Step(Step::Control(*id, *unnamed_only)),
        ),
        Node::ControlMapBoxed(boxed) => (
            crate::NodeRef::Pat(&boxed.0),
            SignalOp::Step(Step::Control(boxed.1, boxed.2)),
        ),
        Node::MapHapsNative(boxed) if boxed.2 == SpanUse::Ignores => (
            crate::NodeRef::Pat(&boxed.0),
            SignalOp::Step(Step::MapHap(boxed.1.clone())),
        ),
        Node::MapHapsOwned(boxed) if boxed.2 == SpanUse::Ignores => (
            crate::NodeRef::Pat(&boxed.0),
            SignalOp::Step(Step::MapOwned(boxed.1.clone())),
        ),
        Node::AddContext(boxed) => (
            crate::NodeRef::Pat(&boxed.0),
            SignalOp::Step(Step::Locations(boxed.1.clone())),
        ),
        Node::Fast(node) if node.1 > Fraction::ZERO => {
            (crate::NodeRef::Pat(&node.0), SignalOp::Fast(node.1))
        }
        Node::FastSmall(cell, flags, numer, denom, _) if *numer > 0 => (
            crate::NodeRef::Plain(cell, *flags),
            SignalOp::Fast(Fraction::new(i128::from(*numer), i128::from(*denom))),
        ),
        Node::Shift(node) => (crate::NodeRef::Pat(&node.0), SignalOp::Shift(node.1)),
        Node::ShiftSmall(cell, flags, numer, denom, _) => (
            crate::NodeRef::Plain(cell, *flags),
            SignalOp::Shift(Fraction::new(i128::from(*numer), i128::from(*denom))),
        ),
        Node::Constants(node) => {
            let mut signal = signal_of_depth(crate::NodeRef::Pat(&node.0), depth + 1)?;
            signal
                .ops
                .extend(node.1.iter().map(|meeting| SignalOp::Constant {
                    hap: Box::new(meeting.as_hap()),
                    side: Side::Value,
                    apply: meeting.combine(),
                    flow: meeting.flow(),
                }));
            return Some(signal);
        }
        Node::AppLeft(node) => {
            let crate::AppLeftNode(functions, values, _, apply, flow) = &**node;
            let hap = constant_of(values)?;
            (
                crate::NodeRef::Pat(functions),
                SignalOp::Constant {
                    hap,
                    side: Side::Value,
                    apply: apply.clone(),
                    flow: *flow,
                },
            )
        }
        Node::AppRight(node) => {
            let crate::AppNode(functions, values, apply, flow) = &**node;
            let hap = constant_of(functions)?;
            (
                crate::NodeRef::Pat(values),
                SignalOp::Constant {
                    hap,
                    side: Side::Function,
                    apply: apply.clone(),
                    flow: *flow,
                },
            )
        }
        _ => return None,
    };
    let mut signal = signal_of_depth(inner, depth + 1)?;
    signal.ops.push(op);
    Some(signal)
}

fn constant_of(pattern: &Pattern) -> Option<Box<Hap>> {
    pattern.constant_levels()?;
    if !pattern.tables_may_answer() {
        return None;
    }
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    let hap = crate::without_query_signals(|| pattern.constant_cycle(&State::new(cycle)))?;
    hap.keys.is_empty().then(|| Box::new(hap))
}

/// A signal on the value side of a meeting, for a direct graph walk.
pub(crate) enum ValueSignal {
    Wave {
        key: crate::Key,
        wave: Wave,
    },
    StepWave(Box<crate::fused::StepWave>),
    Gate(Gate),
    Signal {
        signal: Box<Signal>,
        named: Option<Named>,
    },
}

const _: () = assert!(std::mem::size_of::<ValueSignal>() <= 72);

/// A wave under the gate of a degrade.
pub(crate) struct Gate {
    wave: Wave,
    above: f64,
}

impl Gate {
    fn of(values: &Pattern) -> Option<Self> {
        if !values.tables_may_answer() {
            return None;
        }
        let Node::FilterValues(boxed) = values.node_kind() else {
            return None;
        };
        let (inner, gate) = (&boxed.0, &boxed.1);
        Some(Self {
            above: crate::combinators::gate_threshold(gate)?,
            wave: Wave::of(inner)?,
        })
    }

    pub(crate) fn passes(
        &self,
        met: TimeSpan,
        part: TimeSpan,
        controls: &crate::OrderedMap,
    ) -> Option<bool> {
        Some(self.wave.number(&met, &part, controls)? > self.above)
    }
}

/// The control that names each value of a signal on the value side.
pub(crate) struct Named {
    pub(crate) key: crate::Key,
    pub(crate) map: OwnedHapMapFn,
}

impl ValueSignal {
    pub(crate) fn of(values: &Pattern) -> Option<Self> {
        if let Some(step_wave) = crate::fused::StepWave::of_pattern(values) {
            return Some(Self::StepWave(Box::new(step_wave)));
        }
        if let Some((inner, key)) = Self::named_key(values) {
            if let Some(wave) = Wave::of_ref(inner) {
                return Some(Self::Wave { key, wave });
            }
            if let Some(signal) = signal_of_ref(inner) {
                let named = Self::named(values).map(|(_, named)| named);
                return Some(Self::Signal {
                    signal: Box::new(signal),
                    named,
                });
            }
        }
        if let Some(gate) = Gate::of(values) {
            return Some(Self::Gate(gate));
        }
        signal_of(values).map(|signal| Self::Signal {
            signal: Box::new(signal),
            named: None,
        })
    }

    /// Fast lock-free extraction of `(inner, key)` for built-in control maps,
    /// avoiding closure creation when the value side is a [`Wave`] or [`StepWave`].
    pub(super) fn named_key(values: &Pattern) -> Option<(crate::NodeRef<'_>, crate::Key)> {
        if !values.tables_may_answer() {
            return None;
        }
        match values.node_kind() {
            Node::ControlMap(cell, flags, id, false) => {
                let spec = crate::controls::builtin_spec_by_id(*id)?;
                let key = spec.key();
                (key.as_str() != "value").then_some((crate::NodeRef::Plain(cell, *flags), key))
            }
            Node::ControlMapBoxed(boxed) if !boxed.2 => {
                let spec = crate::controls::builtin_spec_by_id(boxed.1)?;
                let key = spec.key();
                (key.as_str() != "value").then_some((crate::NodeRef::Pat(&boxed.0), key))
            }
            Node::MapHapsOwned(boxed) if boxed.2 == SpanUse::Ignores => {
                let (inner, map) = (&boxed.0, &boxed.1);
                let key = crate::controls::named_by(map)?;
                (key.as_str() != "value").then_some((crate::NodeRef::Pat(inner), key))
            }
            _ => None,
        }
    }

    pub(super) fn named(values: &Pattern) -> Option<(crate::NodeRef<'_>, Named)> {
        if !values.tables_may_answer() {
            return None;
        }
        match values.node_kind() {
            Node::ControlMap(cell, flags, id, false) => {
                let spec = crate::controls::builtin_spec_by_id(*id)?;
                let key = spec.key();
                (key.as_str() != "value").then(|| {
                    let map = crate::controls::interned_control_map(spec, false);
                    (crate::NodeRef::Plain(cell, *flags), Named { key, map })
                })
            }
            Node::ControlMapBoxed(boxed) if !boxed.2 => {
                let spec = crate::controls::builtin_spec_by_id(boxed.1)?;
                let key = spec.key();
                (key.as_str() != "value").then(|| {
                    let map = crate::controls::interned_control_map(spec, false);
                    (crate::NodeRef::Pat(&boxed.0), Named { key, map })
                })
            }
            Node::MapHapsOwned(boxed) if boxed.2 == SpanUse::Ignores => {
                let (inner, map) = (&boxed.0, &boxed.1);
                let key = crate::controls::named_by(map)?;
                (key.as_str() != "value").then(|| {
                    let map = OwnedHapMapFn::clone(map);
                    (crate::NodeRef::Pat(inner), Named { key, map })
                })
            }
            _ => None,
        }
    }

    pub(crate) fn control(&self) -> Option<&Named> {
        match self {
            Self::Signal { named, .. } => named.as_ref(),
            Self::Wave { .. } | Self::StepWave(_) | Self::Gate(_) => None,
        }
    }

    pub(crate) fn wave(&self) -> Option<(&crate::Key, &Wave)> {
        match self {
            Self::Wave { key, wave } => Some((key, wave)),
            Self::StepWave(_) | Self::Gate(_) | Self::Signal { .. } => None,
        }
    }

    pub(crate) fn step_wave(&self) -> Option<&crate::fused::StepWave> {
        match self {
            Self::StepWave(step_wave) if !crate::fused::is_unfused() => Some(step_wave.as_ref()),
            _ => None,
        }
    }

    pub(crate) fn gate(&self) -> Option<&Gate> {
        match self {
            Self::Gate(gate) => Some(gate),
            Self::Wave { .. } | Self::StepWave(_) | Self::Signal { .. } => None,
        }
    }

    pub(crate) fn answer(
        &self,
        span: TimeSpan,
        part: TimeSpan,
        probe: &mut State,
        out: &mut Vec<Hap>,
    ) -> Option<()> {
        match self {
            Self::Signal { signal, .. } => signal.answer(span, part, probe, out),
            Self::Wave { .. } | Self::StepWave(_) | Self::Gate(_) => None,
        }
    }
}

pub(super) fn in_one_cycle(span: TimeSpan) -> bool {
    span.begin
        .floor()
        .checked_add_int(1)
        .is_some_and(|next| span.end <= next)
}

fn shifted(hap: &Hap, cycles: i128) -> Option<Hap> {
    let mut hap = hap.clone();
    hap.part = shifted_span(hap.part, cycles)?;
    hap.whole = match hap.whole {
        Some(whole) => Some(shifted_span(whole, cycles)?),
        None => None,
    };
    Some(hap)
}

fn shifted_span(span: TimeSpan, cycles: i128) -> Option<TimeSpan> {
    Some(TimeSpan::new(
        span.begin.checked_add_int(cycles)?,
        span.end.checked_add_int(cycles)?,
    ))
}

fn retime(hap: &mut Hap, map: impl Fn(Fraction) -> Option<Fraction>) -> Option<()> {
    let span = |span: TimeSpan| Some(TimeSpan::new(map(span.begin)?, map(span.end)?));
    hap.part = span(hap.part)?;
    hap.whole = match hap.whole {
        Some(whole) => Some(span(whole)?),
        None => None,
    };
    Some(())
}

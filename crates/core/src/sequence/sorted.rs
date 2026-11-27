/*
sequence/sorted.rs - direct sorted and scaled-cycle sequence emission
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use super::*;
use crate::locations::PackedLocations;

/// Step count of a single-layer `period == 1` sequence, or `0` otherwise.
#[inline]
pub(crate) fn single_cycle_step_count(layers: &[Layer]) -> usize {
    if layers.len() == 1 && layers[0].period() == Fraction::ONE {
        layers[0].count()
    } else {
        0
    }
}

/// Emit `end_cycle - start_cycle` whole scaled cycles of a repeating `SlowCat`
/// of `<= 64` single-layer `period == 1` contiguous `StaticInt` power-of-two
/// sequences under `fast(2^fast_shift)`. Extracts per-pattern descriptors once
/// and emits all cycles with `dyadic_i64_in_bounds` and `TrustedLen` extend.
pub(crate) fn try_query_fast_pow2_slowcat_static_int(
    thread: &QueryThread,
    entries: &[&Arc<[Layer]>],
    start_cycle: i128,
    end_cycle: i128,
    fast_shift: u32,
    from: usize,
    out: &mut Vec<Hap>,
) -> Option<bool> {
    const MAX_PATS: usize = 64;
    const MAX_STEPS: usize = 64;
    let pat_count = entries.len();
    if pat_count == 0 || pat_count > MAX_PATS || start_cycle >= end_cycle {
        return None;
    }
    let start_i64 = i64::try_from(start_cycle).ok()?;
    let end_i64 = i64::try_from(end_cycle).ok()?;
    let cycle_count = usize::try_from(end_cycle - start_cycle).ok()?;
    if cycle_count < pat_count * 2 {
        return None;
    }

    #[derive(Clone, Copy)]
    struct StaticSeqDesc {
        named_id: u16,
        packed_loc: PackedLocations,
        ticks_shift: u8,
        total_shift: u8,
        len: u8,
        part_begin0: u16,
        uniform_unit: bool,
        part_ends: [u16; MAX_STEPS],
        ints: [i8; MAX_STEPS],
    }

    let empty_desc = StaticSeqDesc {
        named_id: 0,
        packed_loc: PackedLocations::EMPTY,
        ticks_shift: 0,
        total_shift: 0,
        len: 0,
        part_begin0: 0,
        uniform_unit: false,
        part_ends: [0; MAX_STEPS],
        ints: [0; MAX_STEPS],
    };
    let mut descs = [empty_desc; MAX_PATS];
    let mut per_pass_haps = 0usize;
    let mut max_total_shift = 0u32;

    for (dst, &layers) in descs[..pat_count].iter_mut().zip(entries.iter()) {
        if layers.len() != 1 {
            return None;
        }
        let layer = &layers[0];
        if layer.source().is_some() || !layer.in_cycles() || layer.period() != Fraction::ONE {
            return None;
        }
        let maker = layer.maker(Some((layers, 0)));
        if !maker.keys.is_empty() || maker.contexts.is_some() {
            return None;
        }
        let Values::SingleBuiltin(_, _, named_id, values) = maker.values else {
            return None;
        };
        let desc = layer.listing().with_grid_view(|grid| {
            if grid.rare.is_some_and(SpareRare::has_cuts) || !grid.ticks.is_power_of_two() {
                return None;
            }
            let GridStepsRef::Small(steps) = grid.steps else {
                return None;
            };
            let met = steps.len();
            if !(2..=MAX_STEPS).contains(&met) {
                return None;
            }
            let ticks_shift = grid.ticks.trailing_zeros();
            let total_shift = ticks_shift.checked_add(fast_shift)?;
            if total_shift >= 24 {
                return None;
            }
            let ticks_i64 = i64::from(grid.ticks.get());
            let loc0 = steps[0].loc;
            let mut ints = [0i8; MAX_STEPS];
            let mut part_ends = [0u16; MAX_STEPS];
            let mut prev = (0u16, 0u16);
            let mut uniform_unit = met == grid.ticks.get() as usize;
            for (i, &s) in steps.iter().enumerate() {
                if i64::from(s.part_end) > ticks_i64
                    || (i > 0 && (prev > (s.part_begin, s.part_end) || s.part_begin != prev.1))
                    || s.loc != loc0
                {
                    return None;
                }
                if uniform_unit
                    && (usize::from(s.part_begin) != i || usize::from(s.part_end) != i + 1)
                {
                    uniform_unit = false;
                }
                prev = (s.part_begin, s.part_end);
                let n = values
                    .get(usize::from(s.value))
                    .and_then(crate::value::static_int_of)?;
                ints[i] = n;
                part_ends[i] = s.part_end;
            }
            let step_loc = grid.locations(0);
            let dressed = match maker.loc_dress {
                Some(dress) => dress.around_owned(step_loc),
                None => step_loc,
            };
            let packed_loc = dressed.as_packed_copy()?;
            Some(StaticSeqDesc {
                named_id,
                packed_loc,
                ticks_shift: ticks_shift as u8,
                total_shift: total_shift as u8,
                len: met as u8,
                part_begin0: steps[0].part_begin,
                uniform_unit,
                part_ends,
                ints,
            })
        })??;
        max_total_shift = max_total_shift.max(u32::from(desc.total_shift));
        per_pass_haps = per_pass_haps.checked_add(usize::from(desc.len))?;
        *dst = desc;
    }

    // Verify all scaled ticks fit in `dyadic_i64_in_bounds` (`|n| <= 2^38 - 1`).
    let max_abs_cycle = start_i64.unsigned_abs().max(end_i64.unsigned_abs());
    let max_ticks = 1u64 << (max_total_shift.saturating_sub(fast_shift));
    if max_abs_cycle.checked_add(1)?.checked_mul(max_ticks)? > (1u64 << 37) {
        return None;
    }

    let full_passes = cycle_count / pat_count;
    let rem_cycles = cycle_count % pat_count;
    let mut total_haps = per_pass_haps.checked_mul(full_passes)?;
    let start_idx = start_cycle.rem_euclid(pat_count as i128) as usize;
    let mut rem_idx = start_idx;
    for _ in 0..rem_cycles {
        total_haps = total_haps.checked_add(usize::from(descs[rem_idx].len))?;
        rem_idx += 1;
        if rem_idx == pat_count {
            rem_idx = 0;
        }
    }
    if !thread.charge(out.len() - from + total_haps) {
        out.truncate(from);
        return Some(true);
    }
    out.reserve_exact(total_haps);

    let mut idx = start_idx;
    for step_idx in 0..cycle_count {
        if step_idx != 0 && (step_idx & 15) == 0 && thread.stop_requested() {
            out.truncate(from);
            return Some(true);
        }
        let d = &descs[idx];
        let cycle_i64 = start_i64 + step_idx as i64;
        let offset = cycle_i64 << u32::from(d.ticks_shift);
        let total_shift = u32::from(d.total_shift);
        let met = usize::from(d.len);
        let named_id = d.named_id;
        let packed_loc = d.packed_loc;
        if d.uniform_unit {
            let (mut cur_bn, mut cur_bd) = Fraction::dyadic_i64_parts(offset, total_shift);
            out.extend(d.ints[..met].iter().enumerate().map(|(i, &iv)| {
                let (bn, bd) = (cur_bn, cur_bd);
                let (en, ed) = Fraction::dyadic_i64_parts(offset + i as i64 + 1, total_shift);
                cur_bn = en;
                cur_bd = ed;
                Hap::of_whole_parts(
                    bn,
                    bd,
                    en,
                    ed,
                    Value::Object(crate::OrderedMap::from_static_int(named_id, iv, false)),
                    Locations::from_packed_copy(packed_loc),
                    ContextKeys::default(),
                )
            }));
        } else {
            let (mut cur_bn, mut cur_bd) =
                Fraction::dyadic_i64_parts(offset + i64::from(d.part_begin0), total_shift);
            out.extend(
                d.part_ends[..met]
                    .iter()
                    .zip(&d.ints[..met])
                    .map(|(&pe, &iv)| {
                        let (bn, bd) = (cur_bn, cur_bd);
                        let (en, ed) =
                            Fraction::dyadic_i64_parts(offset + i64::from(pe), total_shift);
                        cur_bn = en;
                        cur_bd = ed;
                        Hap::of_whole_parts(
                            bn,
                            bd,
                            en,
                            ed,
                            Value::Object(crate::OrderedMap::from_static_int(named_id, iv, false)),
                            Locations::from_packed_copy(packed_loc),
                            ContextKeys::default(),
                        )
                    }),
            );
        }
        idx += 1;
        if idx == pat_count {
            idx = 0;
        }
    }
    let added = out.len() - from;
    if added > 0 {
        thread.note_accumulated(added);
        thread.note_materialised(added);
    }
    Some(true)
}

/// Emit one whole cycle `cycle` of a single-layer power-of-two `Node::Sequence`
/// directly scaled by `1 / 2^fast_shift` into `out` in a single pass, avoiding
/// per-cycle `query_layers` dispatch and post-pass `Hap::map_times`.
pub(crate) fn push_whole_cycle_scaled_pow2(
    thread: &QueryThread,
    layers: &Arc<[Layer]>,
    cycle: i128,
    fast_shift: u32,
    from: usize,
    out: &mut Vec<Hap>,
) -> Option<Result<(), SequenceStop>> {
    if layers.len() != 1 {
        return None;
    }
    let layer = &layers[0];
    if layer.source().is_some() || !layer.in_cycles() || layer.period() != Fraction::ONE {
        return None;
    }
    let cycle_i64 = i64::try_from(cycle).ok()?;
    let listing = layer.listing();
    listing.with_grid_view(|grid| {
        if grid.rare.is_some_and(SpareRare::has_cuts) || !grid.ticks.is_power_of_two() {
            return None;
        }
        let GridStepsRef::Small(steps) = grid.steps else {
            return None;
        };
        let met = steps.len();
        if met < 2 {
            return None;
        }
        let ticks_i64 = i64::from(grid.ticks.get());
        let total_shift = grid.ticks.trailing_zeros().checked_add(fast_shift)?;
        if total_shift >= 63 {
            return None;
        }
        let offset = cycle_i64.checked_mul(ticks_i64)?;
        offset.checked_add(ticks_i64)?;

        let maker = layer.maker(Some((layers, 0)));
        let empty_keys = maker.keys.is_empty();

        // Fast path for single built-in integer control grids (<= 64 steps)
        // with uniform packed location and empty keys: emits via `TrustedLen`
        // `Vec::extend` with zero per-hap capacity, enum, or refcount work.
        if met <= 64
            && empty_keys
            && maker.contexts.is_none()
            && let Values::SingleBuiltin(_, _, named_id, values) = maker.values
        {
            let loc0 = steps[0].loc;
            let mut ints = [0i8; 64];
            let mut fast_ok = true;
            let mut prev = (0u16, 0u16);
            let mut contiguous = true;
            for (i, &s) in steps.iter().enumerate() {
                if i64::from(s.part_end) > ticks_i64
                    || (i > 0 && prev > (s.part_begin, s.part_end))
                    || s.loc != loc0
                {
                    fast_ok = false;
                    break;
                }
                if i > 0 && s.part_begin != prev.1 {
                    contiguous = false;
                }
                prev = (s.part_begin, s.part_end);
                let Some(n) = values
                    .get(usize::from(s.value))
                    .and_then(crate::value::static_int_of)
                else {
                    fast_ok = false;
                    break;
                };
                ints[i] = n;
            }
            if fast_ok {
                let step_loc = grid.locations(0);
                let dressed = match maker.loc_dress {
                    Some(dress) => dress.around_owned(step_loc),
                    None => step_loc,
                };
                if let Some(packed_loc) = dressed.as_packed_copy() {
                    if !thread.charge(out.len() - from + met) {
                        return Some(Err(SequenceStop::Query));
                    }
                    out.reserve(met);
                    let dyadic_in_bounds = total_shift < 24
                        && offset.abs() <= (1i64 << 36)
                        && ticks_i64 <= (1i64 << 24);
                    let step_pairs = steps.iter().zip(&ints[..met]);
                    if contiguous {
                        if dyadic_in_bounds {
                            let (mut cur_bn, mut cur_bd) = Fraction::dyadic_i64_parts(
                                offset + i64::from(steps[0].part_begin),
                                total_shift,
                            );
                            out.extend(step_pairs.map(|(&s, &iv)| {
                                let (bn, bd) = (cur_bn, cur_bd);
                                let (en, ed) = Fraction::dyadic_i64_parts(
                                    offset + i64::from(s.part_end),
                                    total_shift,
                                );
                                cur_bn = en;
                                cur_bd = ed;
                                Hap::of_whole_parts(
                                    bn,
                                    bd,
                                    en,
                                    ed,
                                    Value::Object(crate::OrderedMap::from_static_int(
                                        named_id, iv, false,
                                    )),
                                    Locations::from_packed_copy(packed_loc),
                                    ContextKeys::default(),
                                )
                            }));
                        } else {
                            let mut cur_b = Fraction::dyadic_i64_small(
                                offset + i64::from(steps[0].part_begin),
                                total_shift,
                            );
                            out.extend(step_pairs.map(|(&s, &iv)| {
                                let b = cur_b;
                                let e = Fraction::dyadic_i64_small(
                                    offset + i64::from(s.part_end),
                                    total_shift,
                                );
                                cur_b = e;
                                let ts = TimeSpan::new(b, e);
                                Hap::of_sequence(
                                    ts,
                                    ts,
                                    Value::Object(crate::OrderedMap::from_static_int(
                                        named_id, iv, false,
                                    )),
                                    Locations::from_packed_copy(packed_loc),
                                    ContextKeys::default(),
                                )
                            }));
                        }
                    } else {
                        let mut last_tick = u16::MAX;
                        let mut last_time = Fraction::ZERO;
                        out.extend(step_pairs.map(|(&s, &iv)| {
                            let b = if s.part_begin == last_tick {
                                last_time
                            } else if dyadic_in_bounds {
                                Fraction::dyadic_i64_in_bounds(
                                    offset + i64::from(s.part_begin),
                                    total_shift,
                                )
                            } else {
                                Fraction::dyadic_i64_small(
                                    offset + i64::from(s.part_begin),
                                    total_shift,
                                )
                            };
                            let e = if dyadic_in_bounds {
                                Fraction::dyadic_i64_in_bounds(
                                    offset + i64::from(s.part_end),
                                    total_shift,
                                )
                            } else {
                                Fraction::dyadic_i64_small(
                                    offset + i64::from(s.part_end),
                                    total_shift,
                                )
                            };
                            last_tick = s.part_end;
                            last_time = e;
                            let ts = TimeSpan::new(b, e);
                            Hap::of_sequence(
                                ts,
                                ts,
                                Value::Object(crate::OrderedMap::from_static_int(
                                    named_id, iv, false,
                                )),
                                Locations::from_packed_copy(packed_loc),
                                ContextKeys::default(),
                            )
                        }));
                    }
                    thread.note_accumulated(out.len() - from);
                    thread.note_materialised(met);
                    return Some(Ok(()));
                }
            }
        }

        if i64::from(steps[met - 1].part_end) > ticks_i64
            || !steps
                .windows(2)
                .all(|w| (w[0].part_begin, w[0].part_end) <= (w[1].part_begin, w[1].part_end))
        {
            return None;
        }
        if !thread.charge(out.len() - from + met) {
            return Some(Err(SequenceStop::Query));
        }
        out.reserve(met);
        let mut last_loc_raw: Option<u16> = None;
        let mut last_loc = Locations::new();
        let mut last_tick = u16::MAX;
        let mut last_time = Fraction::ZERO;
        out.extend(steps.iter().enumerate().map(|(index, &s)| {
            let b = if s.part_begin == last_tick {
                last_time
            } else {
                Fraction::dyadic_i64_small(offset + i64::from(s.part_begin), total_shift)
            };
            let e = Fraction::dyadic_i64_small(offset + i64::from(s.part_end), total_shift);
            last_tick = s.part_end;
            last_time = e;
            let ts = TimeSpan::new(b, e);
            let value = maker
                .values
                .get(usize::from(s.value))
                .expect("a step holds a value of its layer");
            let context = if let Some(contexts) = maker.contexts {
                contexts[index].clone()
            } else if last_loc_raw == Some(s.loc) {
                last_loc.clone()
            } else {
                let step_loc = grid.locations(index);
                let dressed = match maker.loc_dress {
                    Some(dress) => dress.around_owned(step_loc),
                    None => step_loc,
                };
                last_loc_raw = Some(s.loc);
                last_loc = dressed.clone();
                dressed
            };
            let keys = if empty_keys {
                ContextKeys::default()
            } else {
                maker.keys.clone()
            };
            Hap::of_sequence(ts, ts, value, context, keys)
        }));
        thread.note_accumulated(out.len() - from);
        thread.note_materialised(met);
        Some(Ok(()))
    })?
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn emit_single_layer_static_int_contiguous(
    thread: &QueryThread,
    grid: &GridView<'_>,
    steps: &[SmallGridStep],
    named_id: u16,
    values: &[Value],
    packed_loc: PackedLocations,
    p_i64: i64,
    first_shift: i64,
    turn_count: usize,
    total_haps: usize,
    from: usize,
    out: &mut Vec<Hap>,
) -> bool {
    const MAX_STEPS: usize = 64;
    let len = steps.len();
    let ticks_pow2 = grid
        .ticks
        .is_power_of_two()
        .then(|| grid.ticks.trailing_zeros())
        .filter(|&shift| shift < 24);
    let mut pairs = [(0i64, std::num::NonZeroU32::MIN, 0i64, 0i8); MAX_STEPS];
    if let Some(shift) = ticks_pow2 {
        for (dst, &s) in pairs[..len].iter_mut().zip(steps.iter()) {
            let Some(iv) = values
                .get(usize::from(s.value))
                .and_then(crate::value::static_int_of)
            else {
                return false;
            };
            let (bn, bd) = Fraction::dyadic_i64_parts(i64::from(s.part_end), shift);
            let step_n = p_i64 * i64::from(bd.get());
            let cur_n = bn + first_shift * i64::from(bd.get());
            *dst = (cur_n, bd, step_n, iv);
        }
    } else {
        for (dst, &s) in pairs[..len].iter_mut().zip(steps.iter()) {
            let Some(iv) = values
                .get(usize::from(s.value))
                .and_then(crate::value::static_int_of)
            else {
                return false;
            };
            let base_e = grid.made_small_with(i64::from(s.part_end), None);
            let bd = std::num::NonZeroU32::new(base_e.denom() as u32)
                .unwrap_or(std::num::NonZeroU32::MIN);
            let bn = base_e.numer() as i64;
            let step_n = p_i64 * i64::from(bd.get());
            let cur_n = bn + first_shift * i64::from(bd.get());
            *dst = (cur_n, bd, step_n, iv);
        }
    }
    let (mut cur_b0_n, cur_b0_d) = if let Some(shift) = ticks_pow2 {
        let (bn, bd) = Fraction::dyadic_i64_parts(i64::from(steps[0].part_begin), shift);
        (bn + first_shift * i64::from(bd.get()), bd)
    } else {
        let base_b0 = grid.made_small_with(i64::from(steps[0].part_begin), None);
        let bd =
            std::num::NonZeroU32::new(base_b0.denom() as u32).unwrap_or(std::num::NonZeroU32::MIN);
        (
            base_b0.numer() as i64 + first_shift * i64::from(bd.get()),
            bd,
        )
    };
    let b0_inc = p_i64 * i64::from(cur_b0_d.get());

    out.reserve_exact(total_haps);
    let pair_slice = &mut pairs[..len];
    {
        let (mut cur_bn, mut cur_bd) = (cur_b0_n, cur_b0_d);
        out.extend(pair_slice.iter().map(|&(en, ed, _, iv)| {
            let (bn, bd) = (cur_bn, cur_bd);
            cur_bn = en;
            cur_bd = ed;
            Hap::of_whole_parts(
                bn,
                bd,
                en,
                ed,
                Value::Object(crate::OrderedMap::from_static_int(named_id, iv, false)),
                Locations::from_packed_copy(packed_loc),
                ContextKeys::default(),
            )
        }));
    }
    for turn in 1..turn_count {
        if (turn & 3) == 0 && thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        cur_b0_n += b0_inc;
        let (mut cur_bn, mut cur_bd) = (cur_b0_n, cur_b0_d);
        out.extend(pair_slice.iter_mut().map(|p| {
            let (bn, bd) = (cur_bn, cur_bd);
            p.0 += p.2;
            let (en, ed) = (p.0, p.1);
            cur_bn = en;
            cur_bd = ed;
            Hap::of_whole_parts(
                bn,
                bd,
                en,
                ed,
                Value::Object(crate::OrderedMap::from_static_int(named_id, p.3, false)),
                Locations::from_packed_copy(packed_loc),
                ContextKeys::default(),
            )
        }));
    }
    let added = out.len() - from;
    if added > 0 {
        thread.note_accumulated(added);
        thread.note_materialised(added);
    }
    true
}

/// Direct sorted emission for single-layer whole-turn integer-period
/// `Node::Sequence` queries when `Order::Sorted` is requested. Skips post-sort
/// scans, pre-materializes per-step values and spans once, and writes via
/// `TrustedLen` `Vec::extend`.
fn query_single_layer_sorted_onto(
    thread: &QueryThread,
    layers: &Arc<[Layer]>,
    state: &State,
    out: &mut Vec<Hap>,
) -> bool {
    const MAX_STEPS: usize = 64;
    let layer = &layers[0];
    if layer.source().is_some() {
        return false;
    }
    let span = state.span;
    if span.begin >= span.end || span.begin.denom() != 1 || span.end.denom() != 1 {
        return false;
    }
    let period = layer.period();
    if period.denom() != 1 || period.numer() <= 0 {
        return false;
    }
    let p_cycles = period.numer();
    let span_b = span.begin.numer();
    let span_e = span.end.numer();
    if span_b.abs() > (1i128 << 30)
        || span_e.abs() > (1i128 << 30)
        || (span_e - span_b) > MAX_QUERY_SPAN_CYCLES
        || p_cycles > (1i128 << 20)
    {
        return false;
    }
    let start_turn = span_b.div_euclid(p_cycles);
    let start_rem = span_b.rem_euclid(p_cycles);
    let last_turn = (span_e - 1).div_euclid(p_cycles);
    let end_rem = (span_e - 1).rem_euclid(p_cycles) + 1;
    let partial = start_rem != 0 || end_rem != p_cycles;
    if partial && !layer.in_cycles() {
        return false;
    }
    let end_turn = last_turn + 1;
    let Some(turn_count) = usize::try_from(end_turn - start_turn).ok() else {
        return false;
    };
    if turn_count == 0 {
        return true;
    }
    let p_i64 = p_cycles as i64;
    let start_i64 = start_turn as i64;
    let end_i64 = end_turn as i64;
    let first_shift = start_i64.checked_mul(p_i64);
    let last_shift = (end_i64 - 1).checked_mul(p_i64);
    let (Some(first_shift), Some(last_shift)) = (first_shift, last_shift) else {
        return false;
    };

    let from = out.len();
    layer
        .listing()
        .with_grid_view(|grid| {
            if grid.rare.is_some_and(SpareRare::has_cuts) {
                return false;
            }
            let GridStepsRef::Small(steps) = grid.steps else {
                return false;
            };
            let len = steps.len();
            if len == 0 {
                return true;
            }
            if len > MAX_STEPS {
                return false;
            }
            let ticks_u32 = grid.ticks.get();
            let Some(period_ticks) = p_i64.checked_mul(i64::from(ticks_u32)) else {
                return false;
            };
            let mut contiguous = true;
            let mut uniform_loc = true;
            let loc0 = steps[0].loc;
            let mut prev = steps[0];
            if i64::from(prev.part_end) > period_ticks {
                return false;
            }
            for &s in &steps[1..] {
                if i64::from(s.part_end) > period_ticks
                    || (prev.part_begin, prev.part_end) > (s.part_begin, s.part_end)
                {
                    return false;
                }
                if s.part_begin != prev.part_end {
                    contiguous = false;
                }
                if s.loc != loc0 {
                    uniform_loc = false;
                }
                prev = s;
            }
            let first_start = if start_rem == 0 {
                0
            } else {
                let cut = (start_rem as u64) * u64::from(ticks_u32);
                let idx = steps.partition_point(|s| u64::from(s.part_begin) < cut);
                if idx > 0 && u64::from(steps[idx - 1].part_end) > cut {
                    return false;
                }
                idx
            };
            let last_end = if end_rem == p_cycles {
                len
            } else {
                let cut = (end_rem as u64) * u64::from(ticks_u32);
                let idx = steps.partition_point(|s| u64::from(s.part_begin) < cut);
                if idx > 0 && u64::from(steps[idx - 1].part_end) > cut {
                    return false;
                }
                idx
            };
            let total_haps = if turn_count == 1 {
                last_end.saturating_sub(first_start)
            } else {
                let Some(mid) = (turn_count - 2).checked_mul(len) else {
                    return false;
                };
                let Some(t) = (len - first_start)
                    .checked_add(mid)
                    .and_then(|v| v.checked_add(last_end))
                else {
                    return false;
                };
                t
            };
            if total_haps == 0 {
                return true;
            }
            if !thread.charge(total_haps) {
                return true;
            }

            let bounds_ok_by_ticks = ticks_u32 <= 65_536
                && span_b.abs() <= (1i128 << 20)
                && span_e.abs() <= (1i128 << 20)
                && p_cycles <= 1024;

            let maker = layer.maker(Some((layers, 0)));
            let empty_keys = maker.keys.is_empty();

            // Zero-Arc contiguous `StaticInt` + uniform `PackedLocations` + empty
            // keys fast path: fuses time, step-delta, and value setup into one
            // pass with a small stack frame.
            if !partial
                && contiguous
                && bounds_ok_by_ticks
                && empty_keys
                && uniform_loc
                && maker.contexts.is_none()
                && let Values::SingleBuiltin(_, _, named_id, values) = maker.values
            {
                let step_loc = grid.locations(0);
                let dressed = match maker.loc_dress {
                    Some(dress) => dress.around_owned(step_loc),
                    None => step_loc,
                };
                if let Some(packed_loc) = dressed.as_packed_copy()
                    && emit_single_layer_static_int_contiguous(
                        thread,
                        &grid,
                        steps,
                        named_id,
                        values,
                        packed_loc,
                        p_i64,
                        first_shift,
                        turn_count,
                        total_haps,
                        from,
                        out,
                    )
                {
                    return true;
                }
            }

            emit_single_layer_general(
                thread,
                &grid,
                steps,
                maker,
                contiguous,
                uniform_loc,
                empty_keys,
                bounds_ok_by_ticks,
                p_i64,
                start_i64,
                end_i64,
                first_shift,
                last_shift,
                first_start,
                last_end,
                total_haps,
                from,
                out,
            )
        })
        .unwrap_or(false)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn emit_single_layer_general(
    thread: &QueryThread,
    grid: &GridView<'_>,
    steps: &[SmallGridStep],
    maker: Maker<'_>,
    contiguous: bool,
    uniform_loc: bool,
    empty_keys: bool,
    bounds_ok_by_ticks: bool,
    p_i64: i64,
    start_i64: i64,
    end_i64: i64,
    first_shift: i64,
    last_shift: i64,
    first_start: usize,
    last_end: usize,
    total_haps: usize,
    from: usize,
    out: &mut Vec<Hap>,
) -> bool {
    const MAX_STEPS: usize = 64;
    let len = steps.len();
    let partial = first_start != 0 || last_end != len;
    let ticks_pow2 = grid
        .ticks
        .is_power_of_two()
        .then(|| grid.ticks.trailing_zeros());
    let mut last: Option<(u16, Fraction)> = None;
    let mut at0 = |tick: u16| -> Fraction {
        if let Some((made, time)) = last
            && made == tick
        {
            return time;
        }
        let time = if let Some(shift) = ticks_pow2
            && shift < 63
        {
            Fraction::dyadic_i64_small(i64::from(tick), shift)
        } else {
            grid.made_small_with(i64::from(tick), None)
        };
        last = Some((tick, time));
        time
    };

    let mut base_b = [Fraction::ZERO; MAX_STEPS];
    let mut base_e = [Fraction::ZERO; MAX_STEPS];
    if contiguous {
        base_b[0] = at0(steps[0].part_begin);
        if !bounds_ok_by_ticks
            && (base_b[0].checked_add_i64(first_shift).is_none()
                || base_b[0].checked_add_i64(last_shift).is_none())
        {
            return false;
        }
        for i in 0..len {
            let e = at0(steps[i].part_end);
            if !bounds_ok_by_ticks
                && (e.checked_add_i64(first_shift).is_none()
                    || e.checked_add_i64(last_shift).is_none())
            {
                return false;
            }
            base_e[i] = e;
        }
    } else {
        for i in 0..len {
            let b = at0(steps[i].part_begin);
            let e = at0(steps[i].part_end);
            if !bounds_ok_by_ticks
                && (b.checked_add_i64(first_shift).is_none()
                    || b.checked_add_i64(last_shift).is_none()
                    || e.checked_add_i64(first_shift).is_none()
                    || e.checked_add_i64(last_shift).is_none())
            {
                return false;
            }
            base_b[i] = b;
            base_e[i] = e;
        }
    }

    if !partial
        && empty_keys
        && uniform_loc
        && maker.contexts.is_none()
        && let Values::SingleBuiltin(_, _, named_id, values) = maker.values
    {
        let mut ints = [0i8; MAX_STEPS];
        let mut all_ints = true;
        for (dst, &s) in ints[..len].iter_mut().zip(steps.iter()) {
            if let Some(n) = values
                .get(usize::from(s.value))
                .and_then(crate::value::static_int_of)
            {
                *dst = n;
            } else {
                all_ints = false;
                break;
            }
        }
        if all_ints {
            let step_loc = grid.locations(0);
            let dressed = match maker.loc_dress {
                Some(dress) => dress.around_owned(step_loc),
                None => step_loc,
            };
            if let Some(packed_loc) = dressed.as_packed_copy() {
                out.reserve_exact(total_haps);
                if contiguous {
                    let mut pairs = [(Fraction::ZERO, 0i64, 0i8); MAX_STEPS];
                    for i in 0..len {
                        let cur_e = if first_shift == 0 {
                            base_e[i]
                        } else {
                            base_e[i].add_i64_in_bounds(first_shift)
                        };
                        pairs[i] = (cur_e, base_e[i].inline_step_raw(p_i64), ints[i]);
                    }
                    let mut cur_b0 = if first_shift == 0 {
                        base_b[0]
                    } else {
                        base_b[0].add_i64_in_bounds(first_shift)
                    };
                    let b0_inc = base_b[0].inline_step_raw(p_i64);
                    let pair_slice = &mut pairs[..len];
                    let turn_count = (end_i64 - start_i64) as usize;
                    {
                        let mut cur_b = cur_b0;
                        out.extend(pair_slice.iter().map(|&(e, _, iv)| {
                            let b = cur_b;
                            cur_b = e;
                            let ts = TimeSpan::new(b, e);
                            Hap::of_sequence(
                                ts,
                                ts,
                                Value::Object(crate::OrderedMap::from_static_int(
                                    named_id, iv, false,
                                )),
                                Locations::from_packed_copy(packed_loc),
                                ContextKeys::default(),
                            )
                        }));
                    }
                    for turn in 1..turn_count {
                        if (turn & 3) == 0 && thread.stop_requested() {
                            out.truncate(from);
                            return true;
                        }
                        cur_b0 = cur_b0.add_inline_step_raw(b0_inc);
                        let mut cur_b = cur_b0;
                        out.extend(pair_slice.iter_mut().map(|p| {
                            let b = cur_b;
                            let e = p.0.add_inline_step_raw(p.1);
                            p.0 = e;
                            cur_b = e;
                            let ts = TimeSpan::new(b, e);
                            Hap::of_sequence(
                                ts,
                                ts,
                                Value::Object(crate::OrderedMap::from_static_int(
                                    named_id, p.2, false,
                                )),
                                Locations::from_packed_copy(packed_loc),
                                ContextKeys::default(),
                            )
                        }));
                    }
                } else {
                    for turn_idx in start_i64..end_i64 {
                        if turn_idx != start_i64
                            && ((turn_idx - start_i64) & 3) == 0
                            && thread.stop_requested()
                        {
                            out.truncate(from);
                            return true;
                        }
                        let shift = turn_idx * p_i64;
                        out.extend((0..len).map(|i| {
                            let ts = TimeSpan::new(
                                base_b[i].add_i64_in_bounds(shift),
                                base_e[i].add_i64_in_bounds(shift),
                            );
                            Hap::of_sequence(
                                ts,
                                ts,
                                Value::Object(crate::OrderedMap::from_static_int(
                                    named_id, ints[i], false,
                                )),
                                Locations::from_packed_copy(packed_loc),
                                ContextKeys::default(),
                            )
                        }));
                    }
                }
                let added = out.len() - from;
                if added > 0 {
                    thread.note_accumulated(added);
                    thread.note_materialised(added);
                }
                return true;
            }
        }
    }

    let mut base_values: [Value; MAX_STEPS] = [const { Value::Undefined }; MAX_STEPS];
    for (dst, &s) in base_values[..len].iter_mut().zip(steps.iter()) {
        *dst = maker
            .values
            .get(usize::from(s.value))
            .expect("a step holds a value of its layer");
    }
    let single_loc = (uniform_loc && maker.contexts.is_none()).then(|| {
        let step_loc = grid.locations(0);
        match maker.loc_dress {
            Some(dress) => dress.around_owned(step_loc),
            None => step_loc,
        }
    });
    let mut base_locs: [Locations; MAX_STEPS] = [const { Locations::new() }; MAX_STEPS];
    let mut packed_locs = [Locations::new().as_packed_copy().expect("empty packed"); MAX_STEPS];
    let mut all_packed = false;
    if single_loc.is_none() {
        all_packed = true;
        for i in 0..len {
            let loc = if let Some(contexts) = maker.contexts {
                contexts[i].clone()
            } else {
                let step_loc = grid.locations(i);
                match maker.loc_dress {
                    Some(dress) => dress.around_owned(step_loc),
                    None => step_loc,
                }
            };
            if all_packed {
                if let Some(p) = loc.as_packed_copy() {
                    packed_locs[i] = p;
                } else {
                    all_packed = false;
                }
            }
            base_locs[i] = loc;
        }
    }
    let single_packed = single_loc.as_ref().and_then(Locations::as_packed_copy);

    out.reserve_exact(total_haps);
    for turn_idx in start_i64..end_i64 {
        if turn_idx != start_i64 && ((turn_idx - start_i64) & 3) == 0 && thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        let lo = if turn_idx == start_i64 {
            first_start
        } else {
            0
        };
        let hi = if turn_idx == end_i64 - 1 {
            last_end
        } else {
            len
        };
        if lo >= hi {
            continue;
        }
        let shift = turn_idx * p_i64;
        if let Some(packed_loc) = single_packed {
            if contiguous {
                let first_b = if lo == 0 { base_b[0] } else { base_e[lo - 1] };
                let mut cur_b = first_b.add_i64_in_bounds(shift);
                out.extend(
                    base_e[lo..hi]
                        .iter()
                        .zip(&base_values[lo..hi])
                        .map(|(&be, val)| {
                            let b = cur_b;
                            let e = be.add_i64_in_bounds(shift);
                            cur_b = e;
                            let ts = TimeSpan::new(b, e);
                            let keys = if empty_keys {
                                ContextKeys::default()
                            } else {
                                maker.keys.clone()
                            };
                            Hap::of_sequence(
                                ts,
                                ts,
                                val.clone(),
                                Locations::from_packed_copy(packed_loc),
                                keys,
                            )
                        }),
                );
            } else {
                out.extend((lo..hi).map(|i| {
                    let ts = TimeSpan::new(
                        base_b[i].add_i64_in_bounds(shift),
                        base_e[i].add_i64_in_bounds(shift),
                    );
                    let keys = if empty_keys {
                        ContextKeys::default()
                    } else {
                        maker.keys.clone()
                    };
                    Hap::of_sequence(
                        ts,
                        ts,
                        base_values[i].clone(),
                        Locations::from_packed_copy(packed_loc),
                        keys,
                    )
                }));
            }
        } else if all_packed && contiguous {
            let first_b = if lo == 0 { base_b[0] } else { base_e[lo - 1] };
            let mut cur_b = first_b.add_i64_in_bounds(shift);
            out.extend((lo..hi).map(|i| {
                let b = cur_b;
                let e = base_e[i].add_i64_in_bounds(shift);
                cur_b = e;
                let ts = TimeSpan::new(b, e);
                let keys = if empty_keys {
                    ContextKeys::default()
                } else {
                    maker.keys.clone()
                };
                Hap::of_sequence(
                    ts,
                    ts,
                    base_values[i].clone(),
                    Locations::from_packed_copy(packed_locs[i]),
                    keys,
                )
            }));
        } else if contiguous {
            let first_b = if lo == 0 { base_b[0] } else { base_e[lo - 1] };
            let mut cur_b = first_b.add_i64_in_bounds(shift);
            out.extend((lo..hi).map(|i| {
                let b = cur_b;
                let e = base_e[i].add_i64_in_bounds(shift);
                cur_b = e;
                let ts = TimeSpan::new(b, e);
                let loc = match &single_loc {
                    Some(loc) => loc.clone(),
                    None => base_locs[i].clone(),
                };
                let keys = if empty_keys {
                    ContextKeys::default()
                } else {
                    maker.keys.clone()
                };
                Hap::of_sequence(ts, ts, base_values[i].clone(), loc, keys)
            }));
        } else {
            out.extend((lo..hi).map(|i| {
                let ts = TimeSpan::new(
                    base_b[i].add_i64_in_bounds(shift),
                    base_e[i].add_i64_in_bounds(shift),
                );
                let loc = match &single_loc {
                    Some(loc) => loc.clone(),
                    None => base_locs[i].clone(),
                };
                let keys = if empty_keys {
                    ContextKeys::default()
                } else {
                    maker.keys.clone()
                };
                Hap::of_sequence(ts, ts, base_values[i].clone(), loc, keys)
            }));
        }
    }
    let added = out.len() - from;
    if added > 0 {
        thread.note_accumulated(added);
        thread.note_materialised(added);
    }
    true
}

/// Direct sorted emission for `Node::Sequence` queries when `Order::Sorted` is
/// requested. Handles single-layer integer-period grids directly, and for
/// multi-layer (`2..=16`) periodic stacks unrolls small unit-fraction (`1/q`)
/// layers to 1 cycle and emits repeating LCM super-periods linearly from a
/// pre-merged stack template.
pub(crate) fn query_sequence_sorted_onto(
    thread: &QueryThread,
    layers: &Arc<[Layer]>,
    state: &State,
    out: &mut Vec<Hap>,
) -> bool {
    // A reversed span, which a negative speed asks, meets no step.
    if state.span.begin > state.span.end {
        return true;
    }
    let k = layers.len();
    if (1..=16).contains(&k)
        && state.span.begin < state.span.end
        && (state.span.begin.denom() != 1 || state.span.end.denom() != 1)
    {
        return query_multi_window_sorted_onto(thread, layers, state, out);
    }
    if k == 1 {
        return query_single_layer_sorted_onto(thread, layers, state, out);
    }
    if k <= 8 && query_multi_sorted_inner::<8, 32, 64>(thread, layers, state, out) {
        return true;
    }
    if (2..=16).contains(&k) {
        return query_multi_sorted_inner::<16, 128, 256>(thread, layers, state, out);
    }
    false
}

/// Fast single-cycle fractional window path (e.g. `1/8`-cycle playback windows)
/// for `1..=16` periodic small-grid layers. Computes exact tick overlap in `i64`
/// without `i128` division, `Fraction` division, or per-layer `Layer::query_onto`
/// setup.
pub(crate) fn query_multi_window_sorted_onto(
    thread: &QueryThread,
    layers: &Arc<[Layer]>,
    state: &State,
    out: &mut Vec<Hap>,
) -> bool {
    let from = out.len();
    let budget = thread.hap_budget();
    match query_multi_window_sorted_budgeted(layers, state, budget, out) {
        Ok(true) => {
            let added = out.len() - from;
            if added > 0 {
                thread.note_accumulated(added);
                thread.note_materialised(added);
            }
            true
        }
        Ok(false) => false,
        Err(limit) => {
            refuse(limit);
            true
        }
    }
}

/// Ultra-compact direct window emission for a single-layer `period == 1`
/// `CompactGrid::Uniform(count)` sequence (e.g. `n("0 .. 7").scale(...).s(...)`).
/// Computes active step indices in O(1) without binary search, step loads, or
/// multi-layer frame setup.
#[inline(never)]
pub(crate) fn try_query_single_uniform_window(
    layers: &Arc<[Layer]>,
    state: &State,
    hap_budget: u64,
) -> Option<Result<Vec<Hap>, QueryLimit>> {
    let layer = layers.first()?;
    if layers.len() != 1 || layer.small_period_parts() != Some((1, 1)) || !layer.is_direct() {
        return None;
    }
    let dress = layer.dress();
    if dress.tail.is_some() {
        return None;
    }
    let Some(FastSmallGrid::Uniform { count }) = layer.listing_fast_uncut() else {
        return None;
    };
    let n = usize::from(count);
    if n == 0 {
        return Some(Ok(Vec::new()));
    }
    let span = state.span;
    let (Ok(bn_i32), Ok(en_i32), Ok(sbd_u16), Ok(sed_u16)) = (
        i32::try_from(span.begin.numer()),
        i32::try_from(span.end.numer()),
        u16::try_from(span.begin.denom()),
        u16::try_from(span.end.denom()),
    ) else {
        return None;
    };
    let bn = i64::from(bn_i32);
    let en = i64::from(en_i32);
    let sbd = i64::from(sbd_u16);
    let sed = i64::from(sed_u16);
    if bn.abs() > (1i64 << 30)
        || en.abs() > (1i64 << 30)
        || bn * sed >= en * sbd
        || !sbd_u16.is_power_of_two()
        || !sed_u16.is_power_of_two()
    {
        return None;
    }
    let b_sh = sbd_u16.trailing_zeros();
    let e_sh = sed_u16.trailing_zeros();
    let cycle = bn >> b_sh;
    if cycle != ((en - 1) >> e_sh) {
        return None;
    }
    let ticks = u32::from(count);
    if cycle
        .unsigned_abs()
        .saturating_mul(u64::from(ticks))
        .saturating_add(u64::from(ticks))
        > i32::MAX as u64
    {
        return None;
    }
    let sbn = bn - (cycle << b_sh);
    let sen = en - (cycle << e_sh);
    let t = i64::from(ticks);
    let lo_num = sbn * t;
    let hi_num = sen * t;
    let i_lo = (lo_num >> b_sh).max(0) as usize;
    let i_hi = (((hi_num + sed - 1) >> e_sh) as usize).min(n);
    if i_lo >= i_hi {
        return Some(Ok(Vec::new()));
    }
    let emit = i_hi - i_lo;
    if emit as u64 > hap_budget {
        return Some(Err(QueryLimit::HapBudget { budget: hap_budget }));
    }
    let shift_ticks = cycle * t;
    let single_builtin_id =
        if layer.is_direct_keyed() && layer.named_id != 0 && !dress.has_set_map() {
            layer.named_id
        } else {
            0
        };
    let packed_ctx =
        PackedLocations::from_words([dress.before_packed[0], dress.before_packed[1], 0, 0]);
    let dress_keys = dress.keys();
    let keys_empty = dress_keys.is_empty();
    let span_bd_nz = std::num::NonZeroU16::new(sbd_u16).expect("positive span denom <= 65535");
    let span_ed_nz = std::num::NonZeroU16::new(sed_u16).expect("positive span denom <= 65535");
    let make_hap =
        |i: usize, wbn: i64, wbd: std::num::NonZeroU32, wen: i64, wed: std::num::NonZeroU32| {
            let sb = i as i64;
            let se = sb + 1;
            let value = if single_builtin_id != 0
                && let Some(iv) = crate::value::static_int_of(&layer.values[i])
            {
                Value::Object(crate::OrderedMap::from_static_int(
                    single_builtin_id,
                    iv,
                    false,
                ))
            } else {
                Value::Object(crate::OrderedMap::from_seq_layer(layers, 0, i as u32))
            };
            let context = Locations::from_packed_copy(packed_ctx);
            let keys = if keys_empty {
                ContextKeys::default()
            } else {
                dress_keys.clone()
            };
            let clip_lo = (sb << b_sh) < lo_num;
            let clip_hi = (se << e_sh) > hi_num;
            if !clip_lo && !clip_hi {
                Hap::of_whole_parts(wbn, wbd, wen, wed, value, context, keys)
            } else {
                let wbd16 =
                    std::num::NonZeroU16::new(wbd.get() as u16).expect("grid denom <= 65535");
                let wed16 =
                    std::num::NonZeroU16::new(wed.get() as u16).expect("grid denom <= 65535");
                let (pbn32, pbd16) = if clip_lo {
                    (bn_i32, span_bd_nz)
                } else {
                    (wbn as i32, wbd16)
                };
                let (pen32, ped16) = if clip_hi {
                    (en_i32, span_ed_nz)
                } else {
                    (wen as i32, wed16)
                };
                Hap::of_cut_small(
                    pbn32, pbd16, pen32, ped16, wbn as i32, wbd16, wen as i32, wed16, value,
                    context, keys,
                )
            }
        };
    let mut out = Vec::with_capacity(emit);
    if ticks.is_power_of_two() {
        let sh = ticks.trailing_zeros();
        out.extend((i_lo..i_hi).map(|i| {
            let sb = i as i64;
            let (wbn, wbd) = Fraction::dyadic_i64_parts(shift_ticks + sb, sh);
            let (wen, wed) = Fraction::dyadic_i64_parts(shift_ticks + sb + 1, sh);
            make_hap(i, wbn, wbd, wen, wed)
        }));
    } else {
        out.extend((i_lo..i_hi).map(|i| {
            let sb = i as i64;
            let (wbn, wbd) = crate::query::reduce_i64_u32(shift_ticks + sb, ticks);
            let (wen, wed) = crate::query::reduce_i64_u32(shift_ticks + sb + 1, ticks);
            make_hap(i, wbn, wbd, wen, wed)
        }));
    }
    Some(Ok(out))
}

/// Budget-parameterized core of [`query_multi_window_sorted_onto`] so top-level
/// leaf queries check the hap budget in a register without TLS round-trips.
///
/// The hot loop handles power-of-two fractional windows over direct, uncut,
/// small-grid layers with zero `Fraction` construction, zero `GridView` stack
/// spills, and zero `Option<Locations>` drop glue. Any rare layer (cuts,
/// complex dress, spare locations, lazy `Table`, non-pow2 span) delegates to a
/// cold out-of-line helper so this function stays tiny and register-resident.
pub(crate) fn query_multi_window_sorted_budgeted(
    layers: &Arc<[Layer]>,
    state: &State,
    hap_budget: Option<u64>,
    out: &mut Vec<Hap>,
) -> Result<bool, QueryLimit> {
    let span = state.span;
    let (Ok(span_bn_i32), Ok(span_en_i32), Ok(sbd_u16), Ok(sed_u16)) = (
        i32::try_from(span.begin.numer()),
        i32::try_from(span.end.numer()),
        u16::try_from(span.begin.denom()),
        u16::try_from(span.end.denom()),
    ) else {
        return Ok(false);
    };
    let bn = i64::from(span_bn_i32);
    let en = i64::from(span_en_i32);
    let sbd = i64::from(sbd_u16);
    let sed = i64::from(sed_u16);
    if bn.abs() > (1i64 << 30) || en.abs() > (1i64 << 30) || bn * sed >= en * sbd {
        return Ok(false);
    }
    if !sbd_u16.is_power_of_two() || !sed_u16.is_power_of_two() {
        return query_multi_window_sorted_general(layers, state, hap_budget, out);
    }
    let b_sh = sbd_u16.trailing_zeros();
    let e_sh = sed_u16.trailing_zeros();
    let cycle = bn >> b_sh;
    if cycle != ((en - 1) >> e_sh) {
        return Ok(false);
    }
    let sbn = bn - (cycle << b_sh);
    let sen = en - (cycle << e_sh);
    let span_bd_nz = std::num::NonZeroU16::new(sbd_u16).expect("positive span denom <= 65535");
    let span_ed_nz = std::num::NonZeroU16::new(sed_u16).expect("positive span denom <= 65535");
    let single_layer = layers.len() == 1;
    let from = out.len();
    let abs_cycle = cycle.unsigned_abs();

    for (layer_idx, layer) in layers.iter().enumerate() {
        let Some((p_num, p_den)) = layer.small_period_parts() else {
            return query_layer_window_slow_from(layers, layer_idx, state, hap_budget, from, out);
        };
        let (p_cycles, q) = if p_den == 1 && p_num > 0 {
            (p_num, 1i64)
        } else if p_num == 1 && p_den > 1 && p_den <= 64 {
            (1i64, p_den)
        } else {
            out.truncate(from);
            return Ok(false);
        };
        let (shift, turn_off) = if q > 1 || p_cycles == 1 {
            (cycle, 0i64)
        } else if (p_cycles as u64).is_power_of_two() {
            let t = cycle & (p_cycles - 1);
            (cycle - t, t)
        } else {
            let t = cycle.rem_euclid(p_cycles);
            (cycle - t, t)
        };
        let dress = layer.dress();
        if !layer.is_direct() || dress.tail.is_some() {
            match query_layer_window_slow(
                layers,
                layer_idx,
                layer,
                p_cycles,
                q,
                shift,
                turn_off,
                bn,
                en,
                sbd,
                sed,
                Some(b_sh),
                Some(e_sh),
                sbn,
                sen,
                span_bn_i32,
                span_en_i32,
                span_bd_nz,
                span_ed_nz,
                single_layer,
                hap_budget,
                from,
                out,
            )? {
                true => continue,
                false => {
                    out.truncate(from);
                    return Ok(false);
                }
            }
        }
        let Some(fast_grid) = layer.listing_fast_uncut() else {
            match query_layer_window_slow(
                layers,
                layer_idx,
                layer,
                p_cycles,
                q,
                shift,
                turn_off,
                bn,
                en,
                sbd,
                sed,
                Some(b_sh),
                Some(e_sh),
                sbn,
                sen,
                span_bn_i32,
                span_en_i32,
                span_bd_nz,
                span_ed_nz,
                single_layer,
                hap_budget,
                from,
                out,
            )? {
                true => continue,
                false => {
                    out.truncate(from);
                    return Ok(false);
                }
            }
        };
        let single_builtin_id =
            if layer.is_direct_keyed() && layer.named_id != 0 && !dress.has_set_map() {
                layer.named_id
            } else {
                0
            };
        let (before0, before1) = (dress.before_packed[0], dress.before_packed[1]);
        let direct_keys = dress.keys();
        let direct_keys_empty = direct_keys.is_empty();

        match fast_grid {
            FastSmallGrid::Uniform { count } => {
                let n = usize::from(count);
                if n == 0 {
                    continue;
                }
                let ticks_u32 = u32::from(count);
                let t = i64::from(ticks_u32);
                let (bound_ticks, sub_ticks, u_lo, u_hi) = if q > 1 {
                    let st = if t == q {
                        1i64
                    } else if t % q == 0 {
                        t / q
                    } else {
                        out.truncate(from);
                        return Ok(false);
                    };
                    (st, st, (sbn * q) >> b_sh, (sen * q + sed - 1) >> e_sh)
                } else {
                    (p_cycles * t, 0i64, 0i64, 1i64)
                };
                if !layer.in_cycles() && t > bound_ticks {
                    out.truncate(from);
                    return Ok(false);
                }
                let (lo_num, hi_num) = if turn_off == 0 {
                    (sbn * t, sen * t)
                } else {
                    (turn_off * t * sbd + sbn * t, turn_off * t * sed + sen * t)
                };
                let lo_tick = lo_num >> b_sh;
                let hi_tick = (hi_num + sed - 1) >> e_sh;
                if (q == 1 && (t <= lo_tick || hi_tick <= 0)) || u_lo >= u_hi {
                    continue;
                }
                if abs_cycle
                    .saturating_mul(u64::from(ticks_u32))
                    .saturating_add(bound_ticks as u64)
                    > i32::MAX as u64
                {
                    match query_layer_window_slow(
                        layers,
                        layer_idx,
                        layer,
                        p_cycles,
                        q,
                        shift,
                        turn_off,
                        bn,
                        en,
                        sbd,
                        sed,
                        Some(b_sh),
                        Some(e_sh),
                        sbn,
                        sen,
                        span_bn_i32,
                        span_en_i32,
                        span_bd_nz,
                        span_ed_nz,
                        single_layer,
                        hap_budget,
                        from,
                        out,
                    )? {
                        true => continue,
                        false => {
                            out.truncate(from);
                            return Ok(false);
                        }
                    }
                }
                let packed_ctx = PackedLocations::from_words([before0, before1, 0, 0]);
                let ticks_pow2 = ticks_u32
                    .is_power_of_two()
                    .then(|| ticks_u32.trailing_zeros());
                let shift_ticks = shift * t;
                for u in u_lo..u_hi {
                    let tick_off = u * sub_ticks;
                    let i_lo = (lo_tick - tick_off).max(0) as usize;
                    let i_hi = ((hi_tick - tick_off).max(0) as usize).min(n);
                    for i in i_lo..i_hi {
                        if let Some(limit) = hap_budget
                            && (out.len() - from + 1) as u64 > limit
                        {
                            out.truncate(from);
                            return Err(QueryLimit::HapBudget { budget: limit });
                        }
                        if out.capacity() == from {
                            reserve_window_initial(
                                out,
                                from,
                                single_layer,
                                i_hi - i_lo,
                                layers.len(),
                            );
                        }
                        let sb = tick_off + i as i64;
                        let se = sb + 1;
                        let (wbn, wbd) = match ticks_pow2 {
                            Some(sh) => Fraction::dyadic_i64_parts(shift_ticks + sb, sh),
                            None => crate::query::reduce_i64_u32(shift_ticks + sb, ticks_u32),
                        };
                        let (wen, wed) = match ticks_pow2 {
                            Some(sh) => Fraction::dyadic_i64_parts(shift_ticks + se, sh),
                            None => crate::query::reduce_i64_u32(shift_ticks + se, ticks_u32),
                        };
                        let value = if single_builtin_id != 0
                            && let Some(iv) = crate::value::static_int_of(&layer.values[i])
                        {
                            Value::Object(crate::OrderedMap::from_static_int(
                                single_builtin_id,
                                iv,
                                false,
                            ))
                        } else {
                            Value::Object(crate::OrderedMap::from_seq_layer(
                                layers,
                                layer_idx as u16,
                                i as u32,
                            ))
                        };
                        let context = Locations::from_packed_copy(packed_ctx);
                        let keys = if direct_keys_empty {
                            ContextKeys::default()
                        } else {
                            direct_keys.clone()
                        };
                        let clip_lo = (sb << b_sh) < lo_num;
                        let clip_hi = (se << e_sh) > hi_num;
                        let hap = if !clip_lo && !clip_hi {
                            Hap::of_whole_parts(wbn, wbd, wen, wed, value, context, keys)
                        } else {
                            let wbd16 = std::num::NonZeroU16::new(wbd.get() as u16)
                                .expect("grid denom <= 65535");
                            let wed16 = std::num::NonZeroU16::new(wed.get() as u16)
                                .expect("grid denom <= 65535");
                            let (pbn32, pbd16) = if clip_lo {
                                (span_bn_i32, span_bd_nz)
                            } else {
                                (wbn as i32, wbd16)
                            };
                            let (pen32, ped16) = if clip_hi {
                                (span_en_i32, span_ed_nz)
                            } else {
                                (wen as i32, wed16)
                            };
                            Hap::of_cut_small(
                                pbn32, pbd16, pen32, ped16, wbn as i32, wbd16, wen as i32, wed16,
                                value, context, keys,
                            )
                        };
                        out.push(hap);
                    }
                }
            }
            FastSmallGrid::OneStep {
                ticks: ticks_u32,
                base: grid_base,
                part_begin,
                part_end,
                value: step_val,
                loc: step_loc,
            } => {
                let t = i64::from(ticks_u32);
                let (bound_ticks, sub_ticks, u_lo, u_hi) = if q > 1 {
                    let st = if t == q {
                        1i64
                    } else if t % q == 0 {
                        t / q
                    } else {
                        out.truncate(from);
                        return Ok(false);
                    };
                    (st, st, (sbn * q) >> b_sh, (sen * q + sed - 1) >> e_sh)
                } else {
                    (p_cycles * t, 0i64, 0i64, 1i64)
                };
                if !layer.in_cycles() && i64::from(part_end) > bound_ticks {
                    out.truncate(from);
                    return Ok(false);
                }
                let (lo_num, hi_num) = if turn_off == 0 {
                    (sbn * t, sen * t)
                } else {
                    (turn_off * t * sbd + sbn * t, turn_off * t * sed + sen * t)
                };
                let lo_tick = lo_num >> b_sh;
                let hi_tick = (hi_num + sed - 1) >> e_sh;
                if q == 1 && (i64::from(part_begin) >= hi_tick || i64::from(part_end) <= lo_tick) {
                    continue;
                }
                if u_lo >= u_hi {
                    continue;
                }
                if (step_loc & SmallGridStep::SPARE) != 0
                    || abs_cycle
                        .saturating_mul(u64::from(ticks_u32))
                        .saturating_add(bound_ticks as u64)
                        > i32::MAX as u64
                {
                    match query_layer_window_slow(
                        layers,
                        layer_idx,
                        layer,
                        p_cycles,
                        q,
                        shift,
                        turn_off,
                        bn,
                        en,
                        sbd,
                        sed,
                        Some(b_sh),
                        Some(e_sh),
                        sbn,
                        sen,
                        span_bn_i32,
                        span_en_i32,
                        span_bd_nz,
                        span_ed_nz,
                        single_layer,
                        hap_budget,
                        from,
                        out,
                    )? {
                        true => continue,
                        false => {
                            out.truncate(from);
                            return Ok(false);
                        }
                    }
                }
                let packed_ctx = if step_loc == 0 {
                    PackedLocations::from_words([before0, before1, 0, 0])
                } else {
                    let begin = grid_base + u32::from(step_loc >> SmallGridStep::LENGTH_BITS);
                    let length = u32::from((step_loc & SmallGridStep::LENGTH_MASK) - 1);
                    let Some(sp) = Locations::pack_small(begin, length) else {
                        match query_layer_window_slow(
                            layers,
                            layer_idx,
                            layer,
                            p_cycles,
                            q,
                            shift,
                            turn_off,
                            bn,
                            en,
                            sbd,
                            sed,
                            Some(b_sh),
                            Some(e_sh),
                            sbn,
                            sen,
                            span_bn_i32,
                            span_en_i32,
                            span_bd_nz,
                            span_ed_nz,
                            single_layer,
                            hap_budget,
                            from,
                            out,
                        )? {
                            true => continue,
                            false => {
                                out.truncate(from);
                                return Ok(false);
                            }
                        }
                    };
                    if before0 == 0 {
                        PackedLocations::from_words([sp, 0, 0, 0])
                    } else if before1 == 0 {
                        PackedLocations::from_words([before0, sp, 0, 0])
                    } else {
                        PackedLocations::from_words([before0, before1, sp, 0])
                    }
                };
                let static_int = if single_builtin_id != 0 {
                    crate::value::static_int_of(&layer.values[usize::from(step_val)])
                } else {
                    None
                };
                let ticks_pow2 = ticks_u32
                    .is_power_of_two()
                    .then(|| ticks_u32.trailing_zeros());
                let shift_ticks = shift * t;
                let pb = i64::from(part_begin);
                let pe = i64::from(part_end);
                for u in u_lo..u_hi {
                    let tick_off = u * sub_ticks;
                    let sb = tick_off + pb;
                    let se = tick_off + pe;
                    if sb >= hi_tick || se <= lo_tick {
                        continue;
                    }
                    if let Some(limit) = hap_budget
                        && (out.len() - from + 1) as u64 > limit
                    {
                        out.truncate(from);
                        return Err(QueryLimit::HapBudget { budget: limit });
                    }
                    if out.capacity() == from {
                        reserve_window_initial(
                            out,
                            from,
                            single_layer,
                            (u_hi - u_lo) as usize,
                            layers.len(),
                        );
                    }
                    let (wbn, wbd) = match ticks_pow2 {
                        Some(sh) => Fraction::dyadic_i64_parts(shift_ticks + sb, sh),
                        None => crate::query::reduce_i64_u32(shift_ticks + sb, ticks_u32),
                    };
                    let (wen, wed) = match ticks_pow2 {
                        Some(sh) => Fraction::dyadic_i64_parts(shift_ticks + se, sh),
                        None => crate::query::reduce_i64_u32(shift_ticks + se, ticks_u32),
                    };
                    let value = match static_int {
                        Some(n) => Value::Object(crate::OrderedMap::from_static_int(
                            single_builtin_id,
                            n,
                            false,
                        )),
                        None => Value::Object(crate::OrderedMap::from_seq_layer(
                            layers,
                            layer_idx as u16,
                            u32::from(step_val),
                        )),
                    };
                    let context = Locations::from_packed_copy(packed_ctx);
                    let keys = if direct_keys_empty {
                        ContextKeys::default()
                    } else {
                        direct_keys.clone()
                    };
                    let clip_lo = (sb << b_sh) < lo_num;
                    let clip_hi = (se << e_sh) > hi_num;
                    let hap = if !clip_lo && !clip_hi {
                        Hap::of_whole_parts(wbn, wbd, wen, wed, value, context, keys)
                    } else {
                        let wbd16 = std::num::NonZeroU16::new(wbd.get() as u16)
                            .expect("grid denom <= 65535");
                        let wed16 = std::num::NonZeroU16::new(wed.get() as u16)
                            .expect("grid denom <= 65535");
                        let (pbn32, pbd16) = if clip_lo {
                            (span_bn_i32, span_bd_nz)
                        } else {
                            (wbn as i32, wbd16)
                        };
                        let (pen32, ped16) = if clip_hi {
                            (span_en_i32, span_ed_nz)
                        } else {
                            (wen as i32, wed16)
                        };
                        Hap::of_cut_small(
                            pbn32, pbd16, pen32, ped16, wbn as i32, wbd16, wen as i32, wed16,
                            value, context, keys,
                        )
                    };
                    out.push(hap);
                }
            }
            FastSmallGrid::Slice {
                ticks: ticks_u32,
                base: grid_base,
                steps,
            } => {
                if steps.is_empty() {
                    continue;
                }
                if ticks_u32 > 65_535 {
                    out.truncate(from);
                    return Ok(false);
                }
                let t = i64::from(ticks_u32);
                let (bound_ticks, sub_ticks, u_lo, u_hi) = if q > 1 {
                    let st = if t == q {
                        1i64
                    } else if t % q == 0 {
                        t / q
                    } else {
                        out.truncate(from);
                        return Ok(false);
                    };
                    (st, st, (sbn * q) >> b_sh, (sen * q + sed - 1) >> e_sh)
                } else {
                    (p_cycles * t, 0i64, 0i64, 1i64)
                };
                if !layer.in_cycles() && i64::from(steps[steps.len() - 1].part_end) > bound_ticks {
                    out.truncate(from);
                    return Ok(false);
                }
                let (lo_num, hi_num) = if turn_off == 0 {
                    (sbn * t, sen * t)
                } else {
                    (turn_off * t * sbd + sbn * t, turn_off * t * sed + sen * t)
                };
                let lo_tick = lo_num >> b_sh;
                let hi_tick = (hi_num + sed - 1) >> e_sh;
                if q == 1
                    && (i64::from(steps[0].part_begin) >= hi_tick
                        || i64::from(steps[steps.len() - 1].part_end) <= lo_tick)
                {
                    continue;
                }
                if u_lo >= u_hi {
                    continue;
                }
                if abs_cycle
                    .saturating_mul(u64::from(ticks_u32))
                    .saturating_add(bound_ticks as u64)
                    > i32::MAX as u64
                {
                    match query_layer_window_slow(
                        layers,
                        layer_idx,
                        layer,
                        p_cycles,
                        q,
                        shift,
                        turn_off,
                        bn,
                        en,
                        sbd,
                        sed,
                        Some(b_sh),
                        Some(e_sh),
                        sbn,
                        sen,
                        span_bn_i32,
                        span_en_i32,
                        span_bd_nz,
                        span_ed_nz,
                        single_layer,
                        hap_budget,
                        from,
                        out,
                    )? {
                        true => continue,
                        false => {
                            out.truncate(from);
                            return Ok(false);
                        }
                    }
                }
                let ticks_pow2 = ticks_u32
                    .is_power_of_two()
                    .then(|| ticks_u32.trailing_zeros());
                let shift_ticks = shift * t;
                let layer_from = out.len();
                let mut need_slow = false;
                'units: for u in u_lo..u_hi {
                    let tick_off = u * sub_ticks;
                    let local_lo = lo_tick - tick_off;
                    let local_hi = hi_tick - tick_off;
                    let start_idx = if steps.len() > 6 && local_lo > 0 {
                        steps.partition_point(|s| i64::from(s.part_end) <= local_lo)
                    } else {
                        0
                    };
                    for (offset_i, &s) in steps[start_idx..].iter().enumerate() {
                        if i64::from(s.part_begin) >= local_hi {
                            break;
                        }
                        if i64::from(s.part_end) <= local_lo {
                            continue;
                        }
                        if (s.loc & SmallGridStep::SPARE) != 0 {
                            need_slow = true;
                            break 'units;
                        }
                        let context = if s.loc == 0 {
                            if before0 != 0 {
                                Locations::from_packed([before0, before1, 0, 0])
                            } else {
                                Locations::new()
                            }
                        } else {
                            let begin = grid_base + u32::from(s.loc >> SmallGridStep::LENGTH_BITS);
                            let length = u32::from((s.loc & SmallGridStep::LENGTH_MASK) - 1);
                            let Some(sp) = Locations::pack_small(begin, length) else {
                                need_slow = true;
                                break 'units;
                            };
                            if before0 == 0 {
                                Locations::from_packed([sp, 0, 0, 0])
                            } else if before1 == 0 {
                                Locations::from_packed([before0, sp, 0, 0])
                            } else {
                                Locations::from_packed([before0, before1, sp, 0])
                            }
                        };
                        if let Some(limit) = hap_budget
                            && (out.len() - from + 1) as u64 > limit
                        {
                            out.truncate(from);
                            return Err(QueryLimit::HapBudget { budget: limit });
                        }
                        if out.capacity() == from {
                            let want = if single_layer {
                                let i = start_idx + offset_i;
                                let one_only = u + 1 == u_hi
                                    && steps
                                        .get(i + 1)
                                        .is_none_or(|next| i64::from(next.part_begin) >= local_hi);
                                if one_only {
                                    1
                                } else {
                                    ((hi_tick - lo_tick) as usize).clamp(2, 8)
                                }
                            } else {
                                layers.len().min(8)
                            };
                            reserve_window_initial(out, from, single_layer, want, layers.len());
                        }
                        let sb = tick_off + i64::from(s.part_begin);
                        let se = tick_off + i64::from(s.part_end);
                        let (wbn, wbd) = match ticks_pow2 {
                            Some(sh) => Fraction::dyadic_i64_parts(shift_ticks + sb, sh),
                            None => crate::query::reduce_i64_u32(shift_ticks + sb, ticks_u32),
                        };
                        let (wen, wed) = match ticks_pow2 {
                            Some(sh) => Fraction::dyadic_i64_parts(shift_ticks + se, sh),
                            None => crate::query::reduce_i64_u32(shift_ticks + se, ticks_u32),
                        };
                        let value = if single_builtin_id != 0 {
                            if let Some(n) =
                                crate::value::static_int_of(&layer.values[usize::from(s.value)])
                            {
                                Value::Object(crate::OrderedMap::from_static_int(
                                    single_builtin_id,
                                    n,
                                    false,
                                ))
                            } else {
                                Value::Object(crate::OrderedMap::from_seq_layer(
                                    layers,
                                    layer_idx as u16,
                                    u32::from(s.value),
                                ))
                            }
                        } else {
                            Value::Object(crate::OrderedMap::from_seq_layer(
                                layers,
                                layer_idx as u16,
                                u32::from(s.value),
                            ))
                        };
                        let keys = if direct_keys_empty {
                            ContextKeys::default()
                        } else {
                            direct_keys.clone()
                        };
                        let clip_lo = (sb << b_sh) < lo_num;
                        let clip_hi = (se << e_sh) > hi_num;
                        let hap = if !clip_lo && !clip_hi {
                            Hap::of_whole_parts(wbn, wbd, wen, wed, value, context, keys)
                        } else {
                            let wbd16 = std::num::NonZeroU16::new(wbd.get() as u16)
                                .expect("grid denom <= 65535");
                            let wed16 = std::num::NonZeroU16::new(wed.get() as u16)
                                .expect("grid denom <= 65535");
                            let (pbn32, pbd16) = if clip_lo {
                                (span_bn_i32, span_bd_nz)
                            } else {
                                (wbn as i32, wbd16)
                            };
                            let (pen32, ped16) = if clip_hi {
                                (span_en_i32, span_ed_nz)
                            } else {
                                (wen as i32, wed16)
                            };
                            Hap::of_cut_small(
                                pbn32, pbd16, pen32, ped16, wbn as i32, wbd16, wen as i32, wed16,
                                value, context, keys,
                            )
                        };
                        out.push(hap);
                    }
                }
                if need_slow {
                    out.truncate(layer_from);
                    match query_layer_window_slow(
                        layers,
                        layer_idx,
                        layer,
                        p_cycles,
                        q,
                        shift,
                        turn_off,
                        bn,
                        en,
                        sbd,
                        sed,
                        Some(b_sh),
                        Some(e_sh),
                        sbn,
                        sen,
                        span_bn_i32,
                        span_en_i32,
                        span_bd_nz,
                        span_ed_nz,
                        single_layer,
                        hap_budget,
                        from,
                        out,
                    )? {
                        true => {}
                        false => {
                            out.truncate(from);
                            return Ok(false);
                        }
                    }
                }
            }
        }
    }

    let added = out.len() - from;
    if !single_layer && added > 1 {
        crate::hap::sort_small_window_haps(&mut out[from..]);
    }
    Ok(true)
}

#[inline(always)]
fn reserve_window_initial(
    out: &mut Vec<Hap>,
    from: usize,
    single_layer: bool,
    want_single: usize,
    layer_count: usize,
) {
    let want = if single_layer {
        want_single.clamp(1, 8)
    } else {
        layer_count.min(8)
    };
    if out.capacity() - from < want {
        if single_layer && want <= 4 {
            out.reserve_exact(want);
        } else {
            out.reserve(want);
        }
    }
}

#[cold]
#[inline(never)]
fn query_layer_window_slow_from(
    layers: &Arc<[Layer]>,
    _start_layer: usize,
    state: &State,
    hap_budget: Option<u64>,
    from: usize,
    out: &mut Vec<Hap>,
) -> Result<bool, QueryLimit> {
    out.truncate(from);
    query_multi_window_sorted_general(layers, state, hap_budget, out)
}

#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn query_layer_window_slow(
    layers: &Arc<[Layer]>,
    layer_idx: usize,
    layer: &Layer,
    p_cycles: i64,
    q: i64,
    shift: i64,
    turn_off: i64,
    bn: i64,
    en: i64,
    sbd: i64,
    sed: i64,
    sbd_pow2: Option<u32>,
    sed_pow2: Option<u32>,
    sbn: i64,
    sen: i64,
    span_bn_i32: i32,
    span_en_i32: i32,
    span_bd_nz: std::num::NonZeroU16,
    span_ed_nz: std::num::NonZeroU16,
    single_layer: bool,
    hap_budget: Option<u64>,
    from: usize,
    out: &mut Vec<Hap>,
) -> Result<bool, QueryLimit> {
    if layer.source().is_some() {
        return Ok(false);
    }
    let in_cycles = layer.in_cycles();
    let is_direct = layer.is_direct();
    let ok = layer
        .listing()
        .with_grid_view(|grid| {
            let GridStepsRef::Small(steps) = grid.steps else {
                return None;
            };
            if steps.is_empty() {
                return Some(Ok(()));
            }
            let ticks_u32 = grid.ticks.get();
            if ticks_u32 > 65_535 {
                return None;
            }
            let has_cuts = grid.rare.is_some_and(SpareRare::has_cuts);
            let t = i64::from(ticks_u32);
            let (bound_ticks, sub_ticks, u_lo, u_hi) = if q > 1 {
                if t % q != 0 {
                    return None;
                }
                let st = t / q;
                let ul = match sbd_pow2 {
                    Some(sh) => (sbn * q) >> sh,
                    None => (sbn * q) / sbd,
                };
                let uh = match sed_pow2 {
                    Some(sh) => (sen * q + sed - 1) >> sh,
                    None => (sen * q + sed - 1) / sed,
                };
                (st, st, ul, uh)
            } else {
                (p_cycles * t, 0i64, 0i64, 1i64)
            };
            if !in_cycles && i64::from(steps[steps.len() - 1].part_end) > bound_ticks {
                return None;
            }
            let (lo_num, hi_num) = if turn_off == 0 {
                (sbn * t, sen * t)
            } else {
                (turn_off * t * sbd + sbn * t, turn_off * t * sed + sen * t)
            };
            let lo_tick = match sbd_pow2 {
                Some(sh) => lo_num >> sh,
                None => lo_num / sbd,
            };
            let hi_tick = match sed_pow2 {
                Some(sh) => (hi_num + sed - 1) >> sh,
                None => (hi_num + sed - 1) / sed,
            };
            if q == 1
                && (i64::from(steps[0].part_begin) >= hi_tick
                    || i64::from(steps[steps.len() - 1].part_end) <= lo_tick)
            {
                return Some(Ok(()));
            }
            if u_lo >= u_hi {
                return Some(Ok(()));
            }
            let ticks_pow2 = grid
                .ticks
                .is_power_of_two()
                .then(|| grid.ticks.trailing_zeros())
                .filter(|&sh| sh < 24);
            let shift_ticks = shift * t;
            let cut_fits_i32 = !has_cuts
                && shift
                    .unsigned_abs()
                    .saturating_mul(u64::from(ticks_u32))
                    .saturating_add(bound_ticks as u64)
                    <= i32::MAX as u64;
            let dress = layer.dress();
            let single_builtin_id = if is_direct
                && layer.is_direct_keyed()
                && layer.named_id != 0
                && !dress.has_set_map()
            {
                layer.named_id
            } else {
                0
            };
            let direct_keys = dress.keys();
            let direct_keys_empty = direct_keys.is_empty();
            let mut maker_opt = None;
            for u in u_lo..u_hi {
                let tick_off = u * sub_ticks;
                let local_lo = lo_tick - tick_off;
                let local_hi = hi_tick - tick_off;
                let start_idx = if steps.len() > 6 && local_lo > 0 {
                    steps.partition_point(|s| i64::from(s.part_end) <= local_lo)
                } else {
                    0
                };
                let mut cuts = if has_cuts {
                    grid.cuts().starting_at(start_idx)
                } else {
                    CutsSlice::EMPTY
                };
                for (offset_i, &s) in steps[start_idx..].iter().enumerate() {
                    let i = start_idx + offset_i;
                    if i64::from(s.part_begin) >= local_hi {
                        break;
                    }
                    if i64::from(s.part_end) <= local_lo {
                        continue;
                    }
                    if let Some(limit) = hap_budget
                        && (out.len() - from + 1) as u64 > limit
                    {
                        return Some(Err(QueryLimit::HapBudget { budget: limit }));
                    }
                    if out.len() == from {
                        let want = if single_layer {
                            let one_only = u + 1 == u_hi
                                && steps
                                    .get(i + 1)
                                    .is_none_or(|next| i64::from(next.part_begin) >= local_hi);
                            if one_only {
                                1
                            } else {
                                ((hi_tick - lo_tick) as usize).clamp(2, 8)
                            }
                        } else {
                            layers.len().min(8)
                        };
                        reserve_window_initial(out, from, single_layer, want, layers.len());
                    }
                    let sb = tick_off + i64::from(s.part_begin);
                    let se = tick_off + i64::from(s.part_end);
                    let (wb_tick, we_tick, is_cut) = if has_cuts {
                        match cuts.split_first_at(i) {
                            Some(cut) => (
                                tick_off + i64::from(cut.begin),
                                tick_off + i64::from(cut.end),
                                true,
                            ),
                            None => (sb, se, false),
                        }
                    } else {
                        (sb, se, false)
                    };
                    let reduce = |tick: i64| {
                        if let Some(sh) = ticks_pow2 {
                            Fraction::dyadic_i64_parts(shift_ticks + tick, sh)
                        } else {
                            crate::query::reduce_i64_u32(shift_ticks + tick, ticks_u32)
                        }
                    };
                    let (wbn, wbd) = reduce(wb_tick);
                    let (wen, wed) = reduce(we_tick);
                    let (value, context, keys) = if is_direct {
                        let val_idx = usize::from(s.value);
                        let value = if single_builtin_id != 0 {
                            if let Some(n) = crate::value::static_int_of(&layer.values[val_idx]) {
                                Value::Object(crate::OrderedMap::from_static_int(
                                    single_builtin_id,
                                    n,
                                    false,
                                ))
                            } else {
                                Value::Object(crate::OrderedMap::from_seq_layer(
                                    layers,
                                    layer_idx as u16,
                                    u32::from(s.value),
                                ))
                            }
                        } else {
                            Value::Object(crate::OrderedMap::from_seq_layer(
                                layers,
                                layer_idx as u16,
                                u32::from(s.value),
                            ))
                        };
                        let step_loc = grid.locations(i);
                        let context = if dress.locates_alone() {
                            step_loc
                        } else {
                            dress.around_owned(step_loc)
                        };
                        let keys = if direct_keys_empty {
                            ContextKeys::default()
                        } else {
                            direct_keys.clone()
                        };
                        (value, context, keys)
                    } else {
                        let maker = maker_opt
                            .get_or_insert_with(|| layer.maker(Some((layers, layer_idx as u16))));
                        let value = maker
                            .values
                            .get(usize::from(s.value))
                            .expect("a step holds a value of its layer");
                        let context = maker.make_grid_small_context(grid, i, s.loc);
                        let keys = if maker.keys.is_empty() {
                            ContextKeys::default()
                        } else {
                            maker.keys.clone()
                        };
                        (value, context, keys)
                    };
                    let clip_lo = sb * sbd < lo_num;
                    let clip_hi = se * sed > hi_num;
                    if !is_cut && !clip_lo && !clip_hi {
                        out.push(Hap::of_whole_parts(
                            wbn, wbd, wen, wed, value, context, keys,
                        ));
                    } else if cut_fits_i32 {
                        let wbd16 = std::num::NonZeroU16::new(wbd.get() as u16)
                            .expect("grid denom <= 65535");
                        let wed16 = std::num::NonZeroU16::new(wed.get() as u16)
                            .expect("grid denom <= 65535");
                        let (pbn32, pbd16) = if clip_lo {
                            (span_bn_i32, span_bd_nz)
                        } else {
                            (wbn as i32, wbd16)
                        };
                        let (pen32, ped16) = if clip_hi {
                            (span_en_i32, span_ed_nz)
                        } else {
                            (wen as i32, wed16)
                        };
                        out.push(Hap::of_cut_small(
                            pbn32, pbd16, pen32, ped16, wbn as i32, wbd16, wen as i32, wed16,
                            value, context, keys,
                        ));
                    } else {
                        let (pbn, pbd) = if clip_lo {
                            (
                                bn,
                                std::num::NonZeroU32::new(sbd as u32)
                                    .unwrap_or(std::num::NonZeroU32::MIN),
                            )
                        } else if is_cut {
                            reduce(sb)
                        } else {
                            (wbn, wbd)
                        };
                        let (pen, ped) = if clip_hi {
                            (
                                en,
                                std::num::NonZeroU32::new(sed as u32)
                                    .unwrap_or(std::num::NonZeroU32::MIN),
                            )
                        } else if is_cut {
                            reduce(se)
                        } else {
                            (wen, wed)
                        };
                        if let (Ok(pbn32), Ok(pen32), Ok(wbn32), Ok(wen32)) = (
                            i32::try_from(pbn),
                            i32::try_from(pen),
                            i32::try_from(wbn),
                            i32::try_from(wen),
                        ) && let (Some(pbd16), Some(ped16), Some(wbd16), Some(wed16)) = (
                            std::num::NonZeroU16::new(pbd.get() as u16),
                            std::num::NonZeroU16::new(ped.get() as u16),
                            std::num::NonZeroU16::new(wbd.get() as u16),
                            std::num::NonZeroU16::new(wed.get() as u16),
                        ) {
                            out.push(Hap::of_cut_small(
                                pbn32, pbd16, pen32, ped16, wbn32, wbd16, wen32, wed16, value,
                                context, keys,
                            ));
                        } else {
                            let whole = TimeSpan::new(
                                Fraction::from_reduced_parts_i64(wbn, wbd),
                                Fraction::from_reduced_parts_i64(wen, wed),
                            );
                            let part = TimeSpan::new(
                                Fraction::from_reduced_parts_i64(pbn, pbd),
                                Fraction::from_reduced_parts_i64(pen, ped),
                            );
                            out.push(Hap::of_sequence(whole, part, value, context, keys));
                        }
                    }
                }
            }
            Some(Ok(()))
        })
        .flatten();
    match ok {
        Some(Ok(())) => Ok(true),
        Some(Err(limit)) => Err(limit),
        None => Ok(false),
    }
}

/// Cold general path for single-cycle windows with non-power-of-two span
/// denominators or large-period layers.
#[cold]
#[inline(never)]
fn query_multi_window_sorted_general(
    layers: &Arc<[Layer]>,
    state: &State,
    hap_budget: Option<u64>,
    out: &mut Vec<Hap>,
) -> Result<bool, QueryLimit> {
    let span = state.span;
    let bn = span.begin.numer() as i64;
    let en = span.end.numer() as i64;
    let sbd = span.begin.denom() as i64;
    let sed = span.end.denom() as i64;
    let sbd_pow2 = (sbd > 0 && (sbd as u64).is_power_of_two()).then(|| sbd.trailing_zeros());
    let sed_pow2 = (sed > 0 && (sed as u64).is_power_of_two()).then(|| sed.trailing_zeros());
    let b_cycle = match sbd_pow2 {
        Some(sh) => bn >> sh,
        None => bn.div_euclid(sbd),
    };
    let e_last_cycle = match sed_pow2 {
        Some(sh) => (en - 1) >> sh,
        None => (en - 1).div_euclid(sed),
    };
    if b_cycle != e_last_cycle {
        return Ok(false);
    }
    let cycle = b_cycle;
    let sbn = bn - cycle * sbd;
    let sen = en - cycle * sed;
    let span_bn_i32 = bn as i32;
    let span_en_i32 = en as i32;
    let span_bd_nz = std::num::NonZeroU16::new(sbd as u16).expect("positive span denom <= 65535");
    let span_ed_nz = std::num::NonZeroU16::new(sed as u16).expect("positive span denom <= 65535");
    let single_layer = layers.len() == 1;
    let from = out.len();
    for (layer_idx, layer) in layers.iter().enumerate() {
        let period = layer.period();
        let (p_cycles, q) = if period.denom() == 1 && period.numer() > 0 && period.numer() <= 65_535
        {
            (period.numer() as i64, 1i64)
        } else if period.numer() == 1 && period.denom() > 1 && period.denom() <= 64 {
            (1i64, period.denom() as i64)
        } else {
            out.truncate(from);
            return Ok(false);
        };
        let (shift, turn_off) = if q > 1 || p_cycles == 1 {
            (cycle, 0i64)
        } else {
            (
                cycle.div_euclid(p_cycles) * p_cycles,
                cycle.rem_euclid(p_cycles),
            )
        };
        if !query_layer_window_slow(
            layers,
            layer_idx,
            layer,
            p_cycles,
            q,
            shift,
            turn_off,
            bn,
            en,
            sbd,
            sed,
            sbd_pow2,
            sed_pow2,
            sbn,
            sen,
            span_bn_i32,
            span_en_i32,
            span_bd_nz,
            span_ed_nz,
            single_layer,
            hap_budget,
            from,
            out,
        )? {
            out.truncate(from);
            return Ok(false);
        }
    }
    let added = out.len() - from;
    if !single_layer && added > 1 {
        crate::hap::sort_small_window_haps(&mut out[from..]);
    }
    Ok(true)
}

fn query_multi_sorted_inner<
    const MAX_K: usize,
    const MAX_STEPS: usize,
    const MAX_SUPER_HAPS: usize,
>(
    thread: &QueryThread,
    layers: &Arc<[Layer]>,
    state: &State,
    out: &mut Vec<Hap>,
) -> bool {
    const { assert!(MAX_STEPS.is_power_of_two()) };
    const MAX_SUPER_PERIOD: i128 = 8;
    let k = layers.len();
    if !(2..=MAX_K).contains(&k) {
        return false;
    }
    let span = state.span;
    if span.begin >= span.end || span.begin.denom() != 1 || span.end.denom() != 1 {
        return false;
    }
    let span_b = span.begin.numer();
    let span_e = span.end.numer();
    if span_b.abs() > (1i128 << 30)
        || span_e.abs() > (1i128 << 30)
        || (span_e - span_b) > MAX_QUERY_SPAN_CYCLES
    {
        return false;
    }
    #[derive(Clone, Copy)]
    enum FastVal {
        StaticInt(u16, i8),
        SeqLayer(u32, u16),
        Table(u32, u16),
        Held(u32, u16),
    }

    let mut base_spans = [TimeSpan::new(Fraction::ZERO, Fraction::ZERO); MAX_STEPS];
    let mut base_fast_vals = [FastVal::StaticInt(0, 0); MAX_STEPS];
    let mut layer_tables: [Option<&Arc<crate::value::Table>>; MAX_K] = [None; MAX_K];
    let mut layer_held: [Option<&[Value]>; MAX_K] = [None; MAX_K];
    let mut base_values: Option<Vec<Value>> = None;
    let mut all_fast_vals = true;
    let mut all_direct_fast = true;
    let mut base_val_slot = [0u16; MAX_STEPS];
    let mut base_packed_locs: [PackedLocations; MAX_STEPS] = [Locations::new()
        .as_packed_copy()
        .expect("empty locations are packed");
        MAX_STEPS];
    let mut base_locs: Option<Vec<Locations>> = None;
    let mut all_locs_packed = true;
    let mut max_ticks = 0u32;
    let mut layer_keys: Option<Vec<ContextKeys>> = None;
    let mut all_keys_empty = true;
    let mut base_cursor = 0usize;

    #[derive(Clone, Copy)]
    struct LayerCursor {
        layer_idx: u16,
        period_cycles: i128,
        start_turn: i128,
        turn: i128,
        end_turn: i128,
        step_base: u16,
        step_len: u16,
        step_idx: u16,
        cur_b: Fraction,
        cur_e: Fraction,
    }
    let empty_cursor = LayerCursor {
        layer_idx: 0,
        period_cycles: 1,
        start_turn: 0,
        turn: 0,
        end_turn: 0,
        step_base: 0,
        step_len: 0,
        step_idx: 0,
        cur_b: Fraction::ZERO,
        cur_e: Fraction::ZERO,
    };
    let mut cursors = [empty_cursor; MAX_K];
    let mut active_k = 0usize;
    let mut total_haps = 0usize;
    let mut super_period = Some(1i128);

    for (layer_idx, layer) in layers.iter().enumerate() {
        if layer.source().is_some() {
            return false;
        }
        let period = layer.period();
        let (p_cycles, unroll_q) = if period.denom() == 1 && period.numer() > 0 {
            (period.numer(), 1usize)
        } else if period.numer() == 1 && period.denom() > 1 {
            let Some(q) = usize::try_from(period.denom()).ok().filter(|&q| q <= 16) else {
                return false;
            };
            (1i128, q)
        } else {
            return false;
        };
        if span_b.rem_euclid(p_cycles) != 0 || span_e.rem_euclid(p_cycles) != 0 {
            return false;
        }
        let start_turn = span_b.div_euclid(p_cycles);
        let end_turn = span_e.div_euclid(p_cycles);
        let Some(turn_count) = usize::try_from(end_turn - start_turn).ok() else {
            return false;
        };
        if turn_count == 0 {
            continue;
        }
        let maker = layer.maker(Some((layers, layer_idx as u16)));
        match maker.values {
            Values::Table(table) => {
                layer_tables[layer_idx] = Some(table);
                all_direct_fast = false;
            }
            Values::Held(values) => {
                layer_held[layer_idx] = Some(values);
                all_direct_fast = false;
            }
            Values::SingleBuiltin(..) | Values::SeqLayer(..) => {}
            Values::DirectKeyed(_) => {
                all_direct_fast = false;
            }
        }
        let Some(step_len) = layer
            .listing()
            .with_grid_view(|grid| {
                if grid.rare.is_some_and(SpareRare::has_cuts) {
                    return None;
                }
                let GridStepsRef::Small(steps) = grid.steps else {
                    return None;
                };
                let len = steps.len();
                if len == 0 {
                    return Some(0usize);
                }
                let total_len = len.checked_mul(unroll_q)?;
                if (unroll_q > 1 && total_len > 32) || base_cursor + total_len > MAX_STEPS {
                    return None;
                }
                let ticks_u32 = grid.ticks.get();
                max_ticks = max_ticks.max(ticks_u32);
                let sub_ticks_u16 = if unroll_q > 1 {
                    if ticks_u32 % (unroll_q as u32) != 0 {
                        return None;
                    }
                    u16::try_from(ticks_u32 / (unroll_q as u32)).ok()?
                } else {
                    0
                };
                let bound_ticks = if unroll_q > 1 {
                    i64::from(sub_ticks_u16)
                } else {
                    i64::try_from(p_cycles)
                        .ok()?
                        .checked_mul(i64::from(ticks_u32))?
                };
                if !steps.iter().all(|s| i64::from(s.part_end) <= bound_ticks)
                    || !steps.windows(2).all(|w| {
                        (w[0].part_begin, w[0].part_end) <= (w[1].part_begin, w[1].part_end)
                    })
                {
                    return None;
                }
                let ticks_pow2 = grid
                    .ticks
                    .is_power_of_two()
                    .then(|| grid.ticks.trailing_zeros());
                let mut last: Option<(u32, Fraction)> = None;
                let mut at0 = |tick: u32| -> Fraction {
                    if let Some((made, time)) = last
                        && made == tick
                    {
                        return time;
                    }
                    let time = if let Some(shift) = ticks_pow2
                        && shift < 24
                    {
                        Fraction::dyadic_i64_in_bounds(i64::from(tick), shift)
                    } else if let Some(shift) = ticks_pow2
                        && shift < 63
                    {
                        Fraction::dyadic_i64_small(i64::from(tick), shift)
                    } else {
                        grid.made_small_with(i64::from(tick), None)
                    };
                    last = Some((tick, time));
                    time
                };
                for (i, &s) in steps.iter().enumerate() {
                    let dst = base_cursor + i;
                    let b = at0(u32::from(s.part_begin));
                    let e = at0(u32::from(s.part_end));
                    base_spans[dst] = TimeSpan::new(b, e);
                    let v_idx = usize::from(s.value);
                    let fast_v = match maker.values {
                        Values::SingleBuiltin(_, li, named_id, values) => {
                            values.get(v_idx).map(|v| {
                                if let Some(n) = crate::value::static_int_of(v) {
                                    FastVal::StaticInt(named_id, n)
                                } else {
                                    FastVal::SeqLayer(v_idx as u32, li)
                                }
                            })
                        }
                        Values::SeqLayer(own_layers, li) => (v_idx
                            < own_layers[usize::from(li)].values.len())
                        .then_some(FastVal::SeqLayer(v_idx as u32, li)),
                        Values::Table(table) => table
                            .has_row(v_idx)
                            .then_some(FastVal::Table(v_idx as u32, layer_idx as u16)),
                        Values::Held(values) => (v_idx < values.len())
                            .then_some(FastVal::Held(v_idx as u32, layer_idx as u16)),
                        _ => None,
                    };
                    if let Some(fv) = fast_v {
                        base_fast_vals[dst] = fv;
                    } else {
                        all_fast_vals = false;
                        let vec =
                            base_values.get_or_insert_with(|| vec![Value::Undefined; MAX_STEPS]);
                        vec[dst] = maker
                            .values
                            .get(v_idx)
                            .expect("a step holds a value of its layer");
                    }
                    base_val_slot[dst] = dst as u16;
                    let loc = if let Some(contexts) = maker.contexts {
                        contexts[i].clone()
                    } else {
                        let step_loc = grid.locations(i);
                        match maker.loc_dress {
                            Some(dress) => dress.around_owned(step_loc),
                            None => step_loc,
                        }
                    };
                    if let Some(packed) = loc.as_packed_copy() {
                        base_packed_locs[dst] = packed;
                    } else {
                        all_locs_packed = false;
                        let vec =
                            base_locs.get_or_insert_with(|| vec![Locations::new(); MAX_STEPS]);
                        vec[dst] = loc;
                    }
                }
                for copy in 1..unroll_q {
                    let tick_off = (copy as u32) * u32::from(sub_ticks_u16);
                    let copy_base = base_cursor + copy * len;
                    for (i, &s) in steps.iter().enumerate() {
                        let dst = copy_base + i;
                        let src = base_cursor + i;
                        let b = at0(tick_off + u32::from(s.part_begin));
                        let e = at0(tick_off + u32::from(s.part_end));
                        base_spans[dst] = TimeSpan::new(b, e);
                        base_val_slot[dst] = src as u16;
                        base_packed_locs[dst] = base_packed_locs[src];
                        if let Some(vec) = &mut base_locs {
                            vec[dst] = vec[src].clone();
                        }
                    }
                }
                Some(total_len)
            })
            .flatten()
        else {
            return false;
        };
        if step_len == 0 {
            continue;
        }
        if !maker.keys.is_empty() {
            all_keys_empty = false;
            let keys_vec = layer_keys.get_or_insert_with(|| vec![ContextKeys::default(); MAX_K]);
            keys_vec[layer_idx] = maker.keys.clone();
        }
        let Some(layer_haps) = step_len.checked_mul(turn_count) else {
            return false;
        };
        let Some(next_total) = total_haps.checked_add(layer_haps) else {
            return false;
        };
        total_haps = next_total;
        super_period = super_period.and_then(|acc| {
            let (mut x, mut y) = (acc, p_cycles);
            while y != 0 {
                let r = x % y;
                x = y;
                y = r;
            }
            let lcm = (acc / x).checked_mul(p_cycles)?;
            (lcm <= MAX_SUPER_PERIOD).then_some(lcm)
        });
        let first_span = base_spans[base_cursor];
        cursors[active_k] = LayerCursor {
            layer_idx: layer_idx as u16,
            period_cycles: p_cycles,
            start_turn,
            turn: 0,
            end_turn,
            step_base: base_cursor as u16,
            step_len: step_len as u16,
            step_idx: 0,
            cur_b: first_span.begin,
            cur_e: first_span.end,
        };
        active_k += 1;
        base_cursor += step_len;
    }

    let from = out.len();
    if total_haps == 0 {
        return true;
    }
    if !thread.charge(total_haps) {
        return true;
    }
    let materialize_fast = |fv: FastVal| -> Value {
        match fv {
            FastVal::StaticInt(id, iv) => {
                Value::Object(crate::OrderedMap::from_static_int(id, iv, false))
            }
            FastVal::SeqLayer(row, li) => {
                Value::Object(crate::OrderedMap::from_seq_layer(layers, li, row))
            }
            FastVal::Table(row, li) => Value::Object(
                layer_tables[usize::from(li)]
                    .expect("table layer recorded")
                    .map(row as usize)
                    .expect("a step holds a value of its layer"),
            ),
            FastVal::Held(row, li) => {
                layer_held[usize::from(li)].expect("held layer recorded")[row as usize].clone()
            }
        }
    };
    // Materialize `FastVal` slots into `base_values` only when another layer
    // needed general `Value` storage.
    if let Some(vec) = &mut base_values {
        for (i, slot) in vec[..base_cursor].iter_mut().enumerate() {
            if matches!(slot, Value::Undefined) && usize::from(base_val_slot[i]) == i {
                *slot = materialize_fast(base_fast_vals[i]);
            }
        }
    }
    // If any layer had non-packed locations, ensure `base_locs` holds packed
    // entries for the packed slots too so the fallback path reads uniformly.
    if let Some(vec) = &mut base_locs {
        for (slot, packed) in vec[..base_cursor]
            .iter_mut()
            .zip(&base_packed_locs[..base_cursor])
        {
            if slot.is_empty() {
                *slot = Locations::from_packed_copy(*packed);
            }
        }
    }
    out.reserve_exact(total_haps);

    if let Some(sp) = super_period
        && span_b.rem_euclid(sp) == 0
        && span_e.rem_euclid(sp) == 0
    {
        let start_super = span_b.div_euclid(sp);
        let end_super = span_e.div_euclid(sp);
        let haps_per_super = cursors[..active_k].iter().try_fold(0usize, |acc, c| {
            let reps = usize::try_from(sp / c.period_cycles).ok()?;
            acc.checked_add(usize::from(c.step_len).checked_mul(reps)?)
        });
        if end_super - start_super >= 2
            && let Some(per_super) = haps_per_super
            && per_super <= MAX_SUPER_HAPS
        {
            let mut tmpl_cursors = cursors;
            let mut tmpl_k = active_k;
            for c in &mut tmpl_cursors[..tmpl_k] {
                c.end_turn = sp / c.period_cycles;
            }
            let mut template =
                [(TimeSpan::new(Fraction::ZERO, Fraction::ZERO), 0u16, 0u8); MAX_SUPER_HAPS];
            let mut tlen = 0usize;
            while tmpl_k > 0 {
                let mut best = 0usize;
                for j in 1..tmpl_k {
                    let a = &tmpl_cursors[best];
                    let b = &tmpl_cursors[j];
                    let less = match b.cur_b.cmp(&a.cur_b) {
                        std::cmp::Ordering::Less => true,
                        std::cmp::Ordering::Greater => false,
                        std::cmp::Ordering::Equal => b.cur_e < a.cur_e,
                    };
                    if less {
                        best = j;
                    }
                }
                let c = &mut tmpl_cursors[best];
                let slot = usize::from(c.step_base + c.step_idx);
                template[tlen] = (
                    TimeSpan::new(c.cur_b, c.cur_e),
                    base_val_slot[slot],
                    c.layer_idx as u8,
                );
                tlen += 1;
                c.step_idx += 1;
                if c.step_idx == c.step_len {
                    c.step_idx = 0;
                    c.turn += 1;
                    if c.turn == c.end_turn {
                        for m in best..tmpl_k - 1 {
                            tmpl_cursors[m] = tmpl_cursors[m + 1];
                        }
                        tmpl_k -= 1;
                        continue;
                    }
                }
                let next_slot = usize::from(c.step_base) + usize::from(c.step_idx);
                let next_span = base_spans[next_slot];
                if c.turn == 0 {
                    c.cur_b = next_span.begin;
                    c.cur_e = next_span.end;
                } else if max_ticks <= 65_536 {
                    let shift = (c.turn * c.period_cycles) as i64;
                    c.cur_b = next_span.begin.add_i64_in_bounds(shift);
                    c.cur_e = next_span.end.add_i64_in_bounds(shift);
                } else {
                    let shift = c.turn * c.period_cycles;
                    c.cur_b = next_span
                        .begin
                        .checked_add_int(shift)
                        .unwrap_or(next_span.begin);
                    c.cur_e = next_span
                        .end
                        .checked_add_int(shift)
                        .unwrap_or(next_span.end);
                }
            }

            let all_inline_bounds = if max_ticks <= 65_536
                && span_b.abs() <= (1i128 << 20)
                && span_e.abs() <= (1i128 << 20)
            {
                true
            } else {
                let first_shift = (start_super as i64).checked_mul(sp as i64);
                let last_shift = ((end_super - 1) as i64).checked_mul(sp as i64);
                if let (Some(fs), Some(ls)) = (first_shift, last_shift) {
                    template[..tlen].iter().all(|&(bs, _, _)| {
                        bs.begin.checked_add_i64(fs).is_some()
                            && bs.begin.checked_add_i64(ls).is_some()
                            && bs.end.checked_add_i64(fs).is_some()
                            && bs.end.checked_add_i64(ls).is_some()
                    })
                } else {
                    false
                }
            };

            if all_inline_bounds && all_locs_packed && all_keys_empty && all_direct_fast {
                let sp_i64 = sp as i64;
                let start_i64 = start_super as i64;
                let end_i64 = end_super as i64;
                let mut fast_tmpl = [(
                    0i64,
                    std::num::NonZeroU32::MIN,
                    0i64,
                    std::num::NonZeroU32::MIN,
                    0u16,
                ); MAX_SUPER_HAPS];
                for (dst, &(bs, slot, _)) in fast_tmpl[..tlen].iter_mut().zip(&template[..tlen]) {
                    let bd = std::num::NonZeroU32::new(bs.begin.denom() as u32)
                        .unwrap_or(std::num::NonZeroU32::MIN);
                    let ed = std::num::NonZeroU32::new(bs.end.denom() as u32)
                        .unwrap_or(std::num::NonZeroU32::MIN);
                    *dst = (bs.begin.numer() as i64, bd, bs.end.numer() as i64, ed, slot);
                }
                let tmpl_slice = &fast_tmpl[..tlen];
                for st in start_i64..end_i64 {
                    if st != start_i64 && ((st - start_i64) & 3) == 0 && thread.stop_requested() {
                        out.truncate(from);
                        return true;
                    }
                    let shift = st * sp_i64;
                    out.extend(tmpl_slice.iter().map(|&(bn, bd, en, ed, slot)| {
                        let slot_i = usize::from(slot) & (MAX_STEPS - 1);
                        let value = match base_fast_vals[slot_i] {
                            FastVal::StaticInt(id, iv) => {
                                Value::Object(crate::OrderedMap::from_static_int(id, iv, false))
                            }
                            FastVal::SeqLayer(row, li) => {
                                Value::Object(crate::OrderedMap::from_seq_layer(layers, li, row))
                            }
                            FastVal::Table(..) | FastVal::Held(..) => Value::Undefined,
                        };
                        Hap::of_whole_parts(
                            bn + shift * i64::from(bd.get()),
                            bd,
                            en + shift * i64::from(ed.get()),
                            ed,
                            value,
                            Locations::from_packed_copy(base_packed_locs[slot_i]),
                            ContextKeys::default(),
                        )
                    }));
                }
            } else if all_inline_bounds && all_locs_packed && all_keys_empty && all_fast_vals {
                let sp_i64 = sp as i64;
                let start_i64 = start_super as i64;
                let end_i64 = end_super as i64;
                let mut fast_tmpl = [(
                    0i64,
                    std::num::NonZeroU32::MIN,
                    0i64,
                    std::num::NonZeroU32::MIN,
                    0u16,
                ); MAX_SUPER_HAPS];
                for (dst, &(bs, slot, _)) in fast_tmpl[..tlen].iter_mut().zip(&template[..tlen]) {
                    let bd = std::num::NonZeroU32::new(bs.begin.denom() as u32)
                        .unwrap_or(std::num::NonZeroU32::MIN);
                    let ed = std::num::NonZeroU32::new(bs.end.denom() as u32)
                        .unwrap_or(std::num::NonZeroU32::MIN);
                    *dst = (bs.begin.numer() as i64, bd, bs.end.numer() as i64, ed, slot);
                }
                let tmpl_slice = &fast_tmpl[..tlen];
                for st in start_i64..end_i64 {
                    if st != start_i64 && ((st - start_i64) & 3) == 0 && thread.stop_requested() {
                        out.truncate(from);
                        return true;
                    }
                    let shift = st * sp_i64;
                    out.extend(tmpl_slice.iter().map(|&(bn, bd, en, ed, slot)| {
                        let slot_i = usize::from(slot) & (MAX_STEPS - 1);
                        let value = materialize_fast(base_fast_vals[slot_i]);
                        Hap::of_whole_parts(
                            bn + shift * i64::from(bd.get()),
                            bd,
                            en + shift * i64::from(ed.get()),
                            ed,
                            value,
                            Locations::from_packed_copy(base_packed_locs[slot_i]),
                            ContextKeys::default(),
                        )
                    }));
                }
            } else if all_inline_bounds && all_locs_packed && all_keys_empty {
                let sp_i64 = sp as i64;
                let start_i64 = start_super as i64;
                let end_i64 = end_super as i64;
                let mut fast_tmpl = [(
                    0i64,
                    std::num::NonZeroU32::MIN,
                    0i64,
                    std::num::NonZeroU32::MIN,
                    0u16,
                ); MAX_SUPER_HAPS];
                for (dst, &(bs, slot, _)) in fast_tmpl[..tlen].iter_mut().zip(&template[..tlen]) {
                    let bd = std::num::NonZeroU32::new(bs.begin.denom() as u32)
                        .unwrap_or(std::num::NonZeroU32::MIN);
                    let ed = std::num::NonZeroU32::new(bs.end.denom() as u32)
                        .unwrap_or(std::num::NonZeroU32::MIN);
                    *dst = (bs.begin.numer() as i64, bd, bs.end.numer() as i64, ed, slot);
                }
                let tmpl_slice = &fast_tmpl[..tlen];
                let values_vec = base_values.as_deref().expect("fallback values populated");
                for st in start_i64..end_i64 {
                    if st != start_i64 && ((st - start_i64) & 3) == 0 && thread.stop_requested() {
                        out.truncate(from);
                        return true;
                    }
                    let shift = st * sp_i64;
                    out.extend(tmpl_slice.iter().map(|&(bn, bd, en, ed, slot)| {
                        let slot_i = usize::from(slot) & (MAX_STEPS - 1);
                        Hap::of_whole_parts(
                            bn + shift * i64::from(bd.get()),
                            bd,
                            en + shift * i64::from(ed.get()),
                            ed,
                            values_vec[slot_i].clone(),
                            Locations::from_packed_copy(base_packed_locs[slot_i]),
                            ContextKeys::default(),
                        )
                    }));
                }
            } else {
                for st in start_super..end_super {
                    if st != start_super && thread.stop_requested() {
                        out.truncate(from);
                        return true;
                    }
                    let shift = st * sp;
                    for &(base_span, slot, layer_idx) in &template[..tlen] {
                        let (Some(b), Some(e)) = (
                            base_span.begin.checked_add_int(shift),
                            base_span.end.checked_add_int(shift),
                        ) else {
                            out.truncate(from);
                            refuse(QueryLimit::NativeFraction {
                                operation: "sequence",
                            });
                            return true;
                        };
                        let ts = TimeSpan::new(b, e);
                        let slot_i = usize::from(slot);
                        let value = match &base_values {
                            None => materialize_fast(base_fast_vals[slot_i]),
                            Some(vec) => vec[slot_i].clone(),
                        };
                        let loc = match &base_locs {
                            None => Locations::from_packed_copy(base_packed_locs[slot_i]),
                            Some(vec) => vec[slot_i].clone(),
                        };
                        let keys = match &layer_keys {
                            None => ContextKeys::default(),
                            Some(vec) => vec[usize::from(layer_idx)].clone(),
                        };
                        out.push(Hap::of_sequence(ts, ts, value, loc, keys));
                    }
                }
            }
            let added = out.len() - from;
            if added > 0 {
                thread.note_accumulated(added);
                thread.note_materialised(added);
            }
            return true;
        }
    }

    // Shift cursors to their starting turn for the general k-way merge fallback.
    for c in &mut cursors[..active_k] {
        c.turn = c.start_turn;
        let shift = c.start_turn * c.period_cycles;
        if shift != 0 {
            let (Some(nb), Some(ne)) = (
                c.cur_b.checked_add_int(shift),
                c.cur_e.checked_add_int(shift),
            ) else {
                out.truncate(from);
                refuse(QueryLimit::NativeFraction {
                    operation: "sequence",
                });
                return true;
            };
            c.cur_b = nb;
            c.cur_e = ne;
        }
    }

    let mut emitted = 0usize;
    while active_k > 0 {
        if (emitted & 63) == 0 && thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        let mut best = 0usize;
        for j in 1..active_k {
            let a = &cursors[best];
            let b = &cursors[j];
            let less = match b.cur_b.cmp(&a.cur_b) {
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Greater => false,
                std::cmp::Ordering::Equal => b.cur_e < a.cur_e,
            };
            if less {
                best = j;
            }
        }
        let c = &mut cursors[best];
        let slot = usize::from(base_val_slot[usize::from(c.step_base) + usize::from(c.step_idx)]);
        let ts = TimeSpan::new(c.cur_b, c.cur_e);
        let value = match &base_values {
            None => materialize_fast(base_fast_vals[slot]),
            Some(vec) => vec[slot].clone(),
        };
        let loc = match &base_locs {
            None => Locations::from_packed_copy(base_packed_locs[slot]),
            Some(vec) => vec[slot].clone(),
        };
        let keys = match &layer_keys {
            None => ContextKeys::default(),
            Some(vec) => vec[usize::from(c.layer_idx)].clone(),
        };
        out.push(Hap::of_sequence(ts, ts, value, loc, keys));
        emitted += 1;
        c.step_idx += 1;
        if c.step_idx == c.step_len {
            c.step_idx = 0;
            c.turn += 1;
            if c.turn == c.end_turn {
                // Shift left to preserve ascending `layer_idx` stability order.
                for m in best..active_k - 1 {
                    cursors[m] = cursors[m + 1];
                }
                active_k -= 1;
                continue;
            }
        }
        let next_slot = usize::from(c.step_base) + usize::from(c.step_idx);
        let shift = c.turn * c.period_cycles;
        let next_span = base_spans[next_slot];
        if shift == 0 {
            c.cur_b = next_span.begin;
            c.cur_e = next_span.end;
        } else {
            let (Some(nb), Some(ne)) = (
                next_span.begin.checked_add_int(shift),
                next_span.end.checked_add_int(shift),
            ) else {
                out.truncate(from);
                refuse(QueryLimit::NativeFraction {
                    operation: "sequence",
                });
                return true;
            };
            c.cur_b = nb;
            c.cur_e = ne;
        }
    }
    let added = out.len() - from;
    if added > 0 {
        thread.note_accumulated(added);
        thread.note_materialised(added);
    }
    true
}

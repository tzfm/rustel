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
        loc_len: u8,
        loc_first: u32,
        loc_t0: u64,
        loc_t1: u64,
        loc_t2: u64,
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
        loc_len: 0,
        loc_first: 0,
        loc_t0: 0,
        loc_t1: 0,
        loc_t2: 0,
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
            if grid.rare.is_some() || !grid.ticks.is_power_of_two() {
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
            let (loc_len, loc_first, loc_t0, loc_t1, loc_t2) = dressed.as_packed_copy()?.parts();
            Some(StaticSeqDesc {
                named_id,
                loc_len,
                loc_first,
                loc_t0,
                loc_t1,
                loc_t2,
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
        let (ll, lf, lt0, lt1, lt2) = (d.loc_len, d.loc_first, d.loc_t0, d.loc_t1, d.loc_t2);
        if d.uniform_unit {
            let mut cur_b = Fraction::dyadic_i64_in_bounds(offset, total_shift);
            out.extend(d.ints[..met].iter().enumerate().map(|(i, &iv)| {
                let b = cur_b;
                let e = Fraction::dyadic_i64_in_bounds(offset + i as i64 + 1, total_shift);
                cur_b = e;
                let ts = TimeSpan::new(b, e);
                Hap::of_sequence(
                    ts,
                    ts,
                    Value::Object(crate::OrderedMap::from_static_int(named_id, iv, false)),
                    Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
                    ContextKeys::default(),
                )
            }));
        } else {
            let mut cur_b =
                Fraction::dyadic_i64_in_bounds(offset + i64::from(d.part_begin0), total_shift);
            out.extend(
                d.part_ends[..met]
                    .iter()
                    .zip(&d.ints[..met])
                    .map(|(&pe, &iv)| {
                        let b = cur_b;
                        let e = Fraction::dyadic_i64_in_bounds(offset + i64::from(pe), total_shift);
                        cur_b = e;
                        let ts = TimeSpan::new(b, e);
                        Hap::of_sequence(
                            ts,
                            ts,
                            Value::Object(crate::OrderedMap::from_static_int(named_id, iv, false)),
                            Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
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
        if grid.rare.is_some() || !grid.ticks.is_power_of_two() {
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
                    let (ll, lf, lt0, lt1, lt2) = packed_loc.parts();
                    let dyadic_in_bounds = total_shift < 24
                        && offset.abs() <= (1i64 << 36)
                        && ticks_i64 <= (1i64 << 24);
                    let step_pairs = steps.iter().zip(&ints[..met]);
                    if contiguous {
                        if dyadic_in_bounds {
                            let mut cur_b = Fraction::dyadic_i64_in_bounds(
                                offset + i64::from(steps[0].part_begin),
                                total_shift,
                            );
                            out.extend(step_pairs.map(|(&s, &iv)| {
                                let b = cur_b;
                                let e = Fraction::dyadic_i64_in_bounds(
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
                                    Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
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
                                    Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
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
                                Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
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
    let mut pairs = [(Fraction::ZERO, 0i64, 0i8); MAX_STEPS];
    if let Some(shift) = ticks_pow2 {
        for (dst, &s) in pairs[..len].iter_mut().zip(steps.iter()) {
            let Some(iv) = values
                .get(usize::from(s.value))
                .and_then(crate::value::static_int_of)
            else {
                return false;
            };
            let base_e = Fraction::dyadic_i64_in_bounds(i64::from(s.part_end), shift);
            let cur_e = if first_shift == 0 {
                base_e
            } else {
                base_e.add_i64_in_bounds(first_shift)
            };
            *dst = (cur_e, base_e.inline_step_raw(p_i64), iv);
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
            let cur_e = if first_shift == 0 {
                base_e
            } else {
                base_e.add_i64_in_bounds(first_shift)
            };
            *dst = (cur_e, base_e.inline_step_raw(p_i64), iv);
        }
    }
    let base_b0 = if let Some(shift) = ticks_pow2 {
        Fraction::dyadic_i64_in_bounds(i64::from(steps[0].part_begin), shift)
    } else {
        grid.made_small_with(i64::from(steps[0].part_begin), None)
    };
    let mut cur_b0 = if first_shift == 0 {
        base_b0
    } else {
        base_b0.add_i64_in_bounds(first_shift)
    };
    let b0_inc = base_b0.inline_step_raw(p_i64);

    let (ll, lf, lt0, lt1, lt2) = packed_loc.parts();
    out.reserve_exact(total_haps);
    let pair_slice = &mut pairs[..len];
    {
        let mut cur_b = cur_b0;
        out.extend(pair_slice.iter().map(|&(e, _, iv)| {
            let b = cur_b;
            cur_b = e;
            let ts = TimeSpan::new(b, e);
            Hap::of_sequence(
                ts,
                ts,
                Value::Object(crate::OrderedMap::from_static_int(named_id, iv, false)),
                Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
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
                Value::Object(crate::OrderedMap::from_static_int(named_id, p.2, false)),
                Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
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
        || span_b.rem_euclid(p_cycles) != 0
        || span_e.rem_euclid(p_cycles) != 0
    {
        return false;
    }
    let start_turn = span_b.div_euclid(p_cycles);
    let end_turn = span_e.div_euclid(p_cycles);
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
            if grid.rare.is_some() {
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
            let Some(total_haps) = len.checked_mul(turn_count) else {
                return false;
            };
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
            if contiguous
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
    total_haps: usize,
    from: usize,
    out: &mut Vec<Hap>,
) -> bool {
    const MAX_STEPS: usize = 64;
    let len = steps.len();
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

    if empty_keys
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
    let base_locs: Option<Vec<Locations>> = single_loc.is_none().then(|| {
        (0..len)
            .map(|i| {
                if let Some(contexts) = maker.contexts {
                    contexts[i].clone()
                } else {
                    let step_loc = grid.locations(i);
                    match maker.loc_dress {
                        Some(dress) => dress.around_owned(step_loc),
                        None => step_loc,
                    }
                }
            })
            .collect()
    });
    let single_packed = single_loc.as_ref().and_then(Locations::as_packed_copy);

    out.reserve_exact(total_haps);
    for turn_idx in start_i64..end_i64 {
        if turn_idx != start_i64 && ((turn_idx - start_i64) & 3) == 0 && thread.stop_requested() {
            out.truncate(from);
            return true;
        }
        let shift = turn_idx * p_i64;
        if let Some(packed_loc) = single_packed {
            if contiguous {
                let mut cur_b = base_b[0].add_i64_in_bounds(shift);
                out.extend(
                    base_e[..len]
                        .iter()
                        .zip(&base_values[..len])
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
                out.extend((0..len).map(|i| {
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
        } else if contiguous {
            let mut cur_b = base_b[0].add_i64_in_bounds(shift);
            out.extend((0..len).map(|i| {
                let b = cur_b;
                let e = base_e[i].add_i64_in_bounds(shift);
                cur_b = e;
                let ts = TimeSpan::new(b, e);
                let loc = match &single_loc {
                    Some(loc) => loc.clone(),
                    None => base_locs.as_deref().expect("non-uniform locs")[i].clone(),
                };
                let keys = if empty_keys {
                    ContextKeys::default()
                } else {
                    maker.keys.clone()
                };
                Hap::of_sequence(ts, ts, base_values[i].clone(), loc, keys)
            }));
        } else {
            out.extend((0..len).map(|i| {
                let ts = TimeSpan::new(
                    base_b[i].add_i64_in_bounds(shift),
                    base_e[i].add_i64_in_bounds(shift),
                );
                let loc = match &single_loc {
                    Some(loc) => loc.clone(),
                    None => base_locs.as_deref().expect("non-uniform locs")[i].clone(),
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
                if grid.rare.is_some() {
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
                let tmpl_slice = &template[..tlen];
                for st in start_i64..end_i64 {
                    if st != start_i64 && ((st - start_i64) & 3) == 0 && thread.stop_requested() {
                        out.truncate(from);
                        return true;
                    }
                    let shift = st * sp_i64;
                    out.extend(tmpl_slice.iter().map(|&(base_span, slot, _)| {
                        let ts = TimeSpan::new(
                            base_span.begin.add_i64_in_bounds(shift),
                            base_span.end.add_i64_in_bounds(shift),
                        );
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
                        let (ll, lf, lt0, lt1, lt2) = base_packed_locs[slot_i].parts();
                        Hap::of_sequence(
                            ts,
                            ts,
                            value,
                            Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
                            ContextKeys::default(),
                        )
                    }));
                }
            } else if all_inline_bounds && all_locs_packed && all_keys_empty && all_fast_vals {
                let sp_i64 = sp as i64;
                let start_i64 = start_super as i64;
                let end_i64 = end_super as i64;
                let tmpl_slice = &template[..tlen];
                for st in start_i64..end_i64 {
                    if st != start_i64 && ((st - start_i64) & 3) == 0 && thread.stop_requested() {
                        out.truncate(from);
                        return true;
                    }
                    let shift = st * sp_i64;
                    out.extend(tmpl_slice.iter().map(|&(base_span, slot, _)| {
                        let ts = TimeSpan::new(
                            base_span.begin.add_i64_in_bounds(shift),
                            base_span.end.add_i64_in_bounds(shift),
                        );
                        let slot_i = usize::from(slot) & (MAX_STEPS - 1);
                        let value = materialize_fast(base_fast_vals[slot_i]);
                        let (ll, lf, lt0, lt1, lt2) = base_packed_locs[slot_i].parts();
                        Hap::of_sequence(
                            ts,
                            ts,
                            value,
                            Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
                            ContextKeys::default(),
                        )
                    }));
                }
            } else if all_inline_bounds && all_locs_packed && all_keys_empty {
                let sp_i64 = sp as i64;
                let start_i64 = start_super as i64;
                let end_i64 = end_super as i64;
                let tmpl_slice = &template[..tlen];
                let values_vec = base_values.as_deref().expect("fallback values populated");
                for st in start_i64..end_i64 {
                    if st != start_i64 && ((st - start_i64) & 3) == 0 && thread.stop_requested() {
                        out.truncate(from);
                        return true;
                    }
                    let shift = st * sp_i64;
                    out.extend(tmpl_slice.iter().map(|&(base_span, slot, _)| {
                        let ts = TimeSpan::new(
                            base_span.begin.add_i64_in_bounds(shift),
                            base_span.end.add_i64_in_bounds(shift),
                        );
                        let slot_i = usize::from(slot) & (MAX_STEPS - 1);
                        let (ll, lf, lt0, lt1, lt2) = base_packed_locs[slot_i].parts();
                        Hap::of_sequence(
                            ts,
                            ts,
                            values_vec[slot_i].clone(),
                            Locations::from_packed_parts(ll, lf, lt0, lt1, lt2),
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

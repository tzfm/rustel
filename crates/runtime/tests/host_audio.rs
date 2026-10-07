/*
rustel-runtime - audio scheduling for a custom host
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! This path must also compile and render with `--no-default-features`: a
//! native host owns its callback and needs no device backend from Rustel.

use std::sync::atomic::{AtomicBool, AtomicU64};

use rustel_audio::{LiveFlipAtomics, LiveScalarBackend, Ring};
use rustel_runtime::{QUERY_WORKER_STACK_BYTES, Session};

#[test]
fn a_custom_host_can_schedule_and_render_audio() {
    std::thread::Builder::new()
        .stack_size(QUERY_WORKER_STACK_BYTES)
        .spawn(|| {
            let sample_rate = 48_000;
            let mut session = Session::new().expect("session");
            session
                .evaluate(r#"s("sine*8").freq(440).gain(0.25)"#)
                .expect("evaluate score");

            // Eight notes per two-second cycle. Both scheduling entry points
            // advance the same cursor, without repeating the first onset.
            let mut events = session
                .schedule_audio_through(0.0, 0.125, sample_rate)
                .expect("schedule first window");
            events.extend(
                session
                    .schedule_audio_at(0.125, sample_rate)
                    .expect("schedule lookahead window"),
            );
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.target_frame)
                    .collect::<Vec<_>>(),
                [0, 12_000, 24_000]
            );
            assert!(
                events
                    .iter()
                    .all(|event| event.generation == session.generation())
            );

            let ring = Ring::new(8);
            for event in events {
                assert!(ring.push(event), "host queue has room for each onset");
            }
            let generation = AtomicU64::new(session.generation());
            let takeover_frame = AtomicU64::new(0);
            let takeover_cut = AtomicU64::new(0);
            let line_arm = AtomicU64::new(0);
            let stopped = AtomicBool::new(false);
            let flip = LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover_frame,
                takeover_cut: &takeover_cut,
                line_arm: &line_arm,
            };
            let mut backend = LiveScalarBackend::new(sample_rate, 16).expect("host DSP");
            let mut output = [0.0; 512 * 2];
            let mut accepted = 0;
            let mut peak = 0.0_f32;
            for block in 0..48 {
                let report = backend.process_block_with(
                    &mut output,
                    512,
                    block * 512,
                    &ring,
                    flip,
                    &stopped,
                );
                accepted += report.accepted;
                assert_eq!((report.stale, report.refused, report.late), (0, 0, 0));
                assert!(output.iter().all(|sample| sample.is_finite()));
                peak = output
                    .iter()
                    .fold(peak, |peak, sample| peak.max(sample.abs()));
            }
            assert_eq!(accepted, 3, "the callback admitted every scheduled onset");
            assert!(peak > 0.01, "the custom callback rendered audible samples");
        })
        .expect("query worker")
        .join()
        .expect("host audio test");
}

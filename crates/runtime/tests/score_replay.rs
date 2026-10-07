/*
rustel-runtime - replaying a score that loads samples
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `samples(...)` is what puts a score on the async evaluation path: the
//! transpiler awaits a bare call for Strudel compatibility, and only a Session
//! may register sample sources, so this is the one shape that cannot be
//! exercised from the JavaScript runtime alone.
//!
//! It is also the shape that HID the scoping bug - a score loading samples
//! replayed fine while the same score without them failed on the second play.

use rustel_runtime::Session;

/// Evaluate the same source three times in one session.
fn replay(source: &str) -> Result<(), String> {
    let mut session = Session::new().map_err(|error| error.to_string())?;
    for pass in 1..=3 {
        session
            .evaluate(source)
            .map_err(|error| format!("pass {pass}: {error}"))?;
    }
    Ok(())
}

#[test]
fn a_score_that_loads_samples_and_declares_things_replays() {
    // The reported shape, with the sample load that hid the bug.
    replay(
        "samples('github:tidalcycles/dirt-samples')\n\
         const main = stack(s(\"bd*4\"), note(\"c3 e3\"))\n\
         $: main",
    )
    .expect("a sample-loading score must replay");
}

#[test]
fn the_same_score_without_samples_replays_the_same_way() {
    // The two halves of the original inconsistency, side by side.
    replay("const main = stack(s(\"bd*4\"), note(\"c3 e3\"))\n$: main")
        .expect("a score without samples must replay too");
}

#[test]
fn a_map_of_samples_replays() {
    // The object form takes the same awaited path.
    replay("samples({ bd: ['bd.wav'] })\nconst main = s(\"bd\")\n$: main")
        .expect("a sample map must replay");
}

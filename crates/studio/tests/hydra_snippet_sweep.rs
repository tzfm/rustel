//! Every snippet the shelf can show must be one the renderer can draw.
//!
//! Checked across the whole JSON catalogue, because a snippet
//! that will not draw shows as an empty preview box, which is the one failure
//! nobody can diagnose from the outside.
#![cfg(feature = "hydra")]

use rustel_hydra::glsl::{compose, parse_chain};
use rustel_studio::examples::{Kind, SECTIONS, section_of};

fn drawable(code: &str) -> Result<(), String> {
    let node = parse_chain(code).map_err(|error| error.to_string())?;
    compose(&node, "highp")
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Every shipped snippet must reach a lit frame, not merely compile. A
/// feedback sketch needs a few frames, and the probe reads every colour
/// channel because one snippet is almost entirely green.
#[cfg(feature = "hydra")]
#[test]
fn every_shipped_snippet_reaches_a_lit_frame() {
    use rustel_hydra::native::NativeRenderer;
    let Ok(mut renderer) = NativeRenderer::new(64, 64) else {
        eprintln!("skipped: nothing to render on");
        return;
    };
    let mut bad = Vec::new();
    for category in &SECTIONS[section_of(Kind::Hydra).unwrap()].shelves {
        for snippet in &category.snippets {
            let Ok(node) = parse_chain(snippet.code) else {
                bad.push(format!("{}: does not parse", snippet.name));
                continue;
            };
            // "Lit" used to mean sixty-odd pixels above channel 8, which a
            // sketch that renders almost nothing still clears. A picture worth
            // shipping puts real light on a real part of the frame.
            let mut best = 0;
            let mut frames: Vec<Vec<u8>> = Vec::new();
            for step in 0..40 {
                if renderer.draw(&node, step as f32 * 0.25).is_err() {
                    break;
                }
                if let Ok(pixels) = renderer.read(0) {
                    best = best.max(
                        pixels
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .filter(|p| p[0] > 64 || p[1] > 64 || p[2] > 64)
                            .count(),
                    );
                    frames.push(pixels);
                }
            }
            // An audio-reactive sketch is judged by neither measure here:
            // with no sound its `a.fft` bands are all zero, so it renders the
            // dim, still frame it is supposed to render.
            if category.name.to_ascii_lowercase().contains("audio") {
                continue;
            }
            if best * 100 < 64 * 64 * 2 {
                bad.push(format!("{}: only {best} bright pixels", snippet.name));
                continue;
            }
            // And it must still be moving at the end. Three of these shipped
            // frozen: a feedback echo scaled inward converges on a point and
            // stops, and nothing in the sweep noticed.
            // Compared late, and over a long gap. A feedback chain is still
            // building for the first second or so; sampling while it converges
            // reads as motion right up until the moment it stops.
            if frames.len() < 30 {
                continue;
            }
            let (first, last) = (&frames[frames.len() - 12], &frames[frames.len() - 1]);
            let moved = first
                .iter()
                .zip(last)
                .filter(|(a, b)| a.abs_diff(**b) > 2)
                .count();
            if moved * 100 < first.len() {
                bad.push(format!("{}: frozen ({moved} bytes moved)", snippet.name));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn every_shipped_snippet_can_be_drawn() {
    let mut refused = Vec::new();
    let mut counted = 0;
    for category in &SECTIONS[section_of(Kind::Hydra).unwrap()].shelves {
        for snippet in &category.snippets {
            counted += 1;
            if let Err(why) = drawable(snippet.code) {
                refused.push(format!("{}: {why}", snippet.name));
            }
        }
    }
    assert!(counted >= 20, "the catalogue shrank to {counted}");
    assert!(refused.is_empty(), "{refused:#?}");
}

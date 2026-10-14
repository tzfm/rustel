//! Items other crates' tests call and no product code does, behind the
//! `test-support` feature.

#[cfg(feature = "hydra")]
pub use crate::hydra_input::HydraInputs;
pub use crate::render::{encode_mp3, read_pcm16_stereo_wav, scalar_event};

/// The clock of a producer at `now`: the first 2 reads give `now`, and
/// each later read gives a time past the cover of the score in play.
/// So the short wait of an edit at the commit seam ends at once.
#[cfg(all(feature = "vst", feature = "device-audio"))]
pub fn past_the_cover(now: f64) -> impl FnMut() -> f64 {
    let mut reads = 0;
    move || {
        reads += 1;
        if reads <= 2 { now } else { now + 60.0 }
    }
}

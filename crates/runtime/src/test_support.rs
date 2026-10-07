//! Items other crates' tests call and no product code does, behind the
//! `test-support` feature.

#[cfg(feature = "hydra")]
pub use crate::hydra_input::HydraInputs;
pub use crate::render::{encode_mp3, read_pcm16_stereo_wav, scalar_event};

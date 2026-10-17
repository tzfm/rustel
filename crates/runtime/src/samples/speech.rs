//! The `talkit` control: a score's words rendered by the bundled speech
//! synthesizer and seated in the sample bank like any other sound.
//!
//! A phrase renders on the thread that first asks for it, with no queue: a
//! second of speech renders in about a millisecond, and a queued render would
//! skip the trigger that asked. Every later trigger reuses the seated sample
//! until a host reclaims it through [`SampleLibrary::forget_decoded`]; the
//! next ask then renders it again.

use rustel_talkit::lang::Language;
use rustel_talkit::params::{Effort, Quality};

use super::*;

/// A spoken request as the table keys it: the text and every setting it
/// renders with. The pitch keys by its bits, since a note can ask for any
/// cent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct SpeechKey {
    text: Box<str>,
    pitch: u32,
    speed: u32,
    quality: Quality,
    effort: Effort,
    language: Language,
}

impl SpeechKey {
    fn of(request: &SpeechRequest<'_>) -> Self {
        let params = &request.params;
        Self {
            text: request.text.into(),
            pitch: params.pitch.to_bits(),
            speed: params.speed,
            quality: params.quality,
            effort: params.effort,
            language: params.language,
        }
    }
}

/// The sample `request` speaks: the one the table holds, or a fresh render
/// seated in the bank. The render runs under the table's lock, so two threads
/// asking for the same words seat them once.
///
/// `Err` is text with nothing to speak. A render the bank cannot seat fails
/// like a sample that does not decode: reported once, then resting for
/// [`FAILED_RETRY_AFTER`].
pub(super) fn speak(
    shared: &Shared,
    request: &SpeechRequest<'_>,
) -> Result<SampleResolution, String> {
    let found = |id: SampleId, duration_secs: f64| SampleResolution::Found {
        id,
        transpose: 0.0,
        duration_secs,
        loop_secs: None,
        envelope_peak: 1.0,
        soundfont: false,
    };
    let key = SpeechKey::of(request);
    let mut spoken = shared.speech.lock().expect("speech table");
    match spoken.get(&key) {
        Some(UrlState::Ready { id, duration_secs }) => return Ok(found(*id, *duration_secs)),
        Some(UrlState::Failed { at }) if at.elapsed() < FAILED_RETRY_AFTER => {
            return Ok(SampleResolution::Failed);
        }
        _ => {}
    }
    let pcm = rustel_talkit::render::render(request.text, &request.params);
    if pcm.is_empty() {
        return Err(format!(
            "talkit has nothing to speak in \"{}\"",
            request.text
        ));
    }
    let duration_secs = pcm.len() as f64 / f64::from(rustel_talkit::RATE);
    // Every decoded sample reaches the device at the context's rate; speech
    // resamples like any other Context decode.
    let rate = shared.render_rate.load(Ordering::Acquire);
    let seated = DecodedSample::from_parts(rustel_talkit::RATE, 1, pcm)
        .and_then(|decoded| decoded.resampled_to(rate))
        .and_then(|decoded| Ok((reserve_sample_ids(shared, 1)?[0], decoded)));
    match seated {
        Ok((id, decoded)) => {
            shared
                .ready
                .lock()
                .expect("ready samples")
                .push((id, decoded));
            spoken.insert(key, UrlState::Ready { id, duration_secs });
            Ok(found(id, duration_secs))
        }
        Err(error) => {
            spoken.insert(key, UrlState::Failed { at: Instant::now() });
            shared
                .failures
                .lock()
                .expect("sample failures")
                .push(format!("talkit \"{}\": {error}", request.text).into());
            Ok(SampleResolution::Failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_talkit::params::Params;
    use rustel_talkit::personality::Personality;

    fn library() -> SampleLibrary {
        SampleLibrary::empty_without_loading()
    }

    fn request(text: &str) -> SpeechRequest<'_> {
        SpeechRequest {
            text,
            params: Params::default(),
        }
    }

    fn seated(resolution: SampleResolution) -> (SampleId, f64) {
        match resolution {
            SampleResolution::Found {
                id, duration_secs, ..
            } => (id, duration_secs),
            _ => panic!("expected a seated phrase"),
        }
    }

    #[test]
    fn the_same_words_reuse_their_sample_and_new_settings_render_anew() {
        let library = library();
        let (first, _) = seated(speak(&library.shared, &request("hello")).expect("spoken"));
        let (again, _) = seated(speak(&library.shared, &request("hello")).expect("spoken"));
        assert_eq!(first, again);
        let higher = SpeechRequest {
            params: Params {
                pitch: 130.81,
                ..Params::default()
            },
            ..request("hello")
        };
        let (other, _) = seated(speak(&library.shared, &higher).expect("spoken"));
        assert_ne!(first, other);
        assert_eq!(library.take_ready().len(), 2);
    }

    #[test]
    fn a_phrase_lasting_seconds_is_seated_ready() {
        let library = library();
        let (id, duration_secs) =
            seated(speak(&library.shared, &request("hello world")).expect("spoken"));
        let ready = library.take_ready();
        let (_, decoded) = ready
            .iter()
            .find(|(queued, _)| *queued == id)
            .expect("seated");
        assert_eq!(decoded.channels(), 1);
        // The duration matches the sample at its own rate.
        let duration = decoded.frames() as f64 / f64::from(decoded.sample_rate());
        assert!((duration - duration_secs).abs() < 0.01, "{duration}");
    }

    /// A host that reclaims a phrase's memory forgets it first. The next ask
    /// must render it again: a table that still named the emptied slot left
    /// the phrase silent for the rest of the session.
    #[test]
    fn a_forgotten_phrase_renders_again() {
        let library = library();
        let (id, _) = seated(speak(&library.shared, &request("hello")).expect("spoken"));
        library.take_ready();
        assert_eq!(library.forget_decoded(&HashSet::from([id])), vec![id]);
        library.release_ids([id]);
        assert_eq!(library.free_id_count(), 1, "the slot goes back to the bank");
        let (again, _) = seated(speak(&library.shared, &request("hello")).expect("spoken"));
        assert!(
            library
                .take_ready()
                .iter()
                .any(|(queued, _)| *queued == again),
            "the phrase is seated again"
        );
    }

    #[test]
    fn a_full_bank_fails_the_phrase_and_says_so_once() {
        let library = library();
        library
            .shared
            .next_id
            .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);
        for _ in 0..2 {
            assert!(matches!(
                speak(&library.shared, &request("hello")),
                Ok(SampleResolution::Failed)
            ));
        }
        let failures = library.take_failures();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("capacity"), "{failures:?}");
    }

    #[test]
    fn text_with_nothing_to_speak_is_refused() {
        let library = library();
        for text in ["", "?!"] {
            let Err(error) = speak(&library.shared, &request(text)) else {
                panic!("{text:?} is refused");
            };
            assert!(error.contains("nothing to speak"), "{error}");
        }
        assert!(library.take_ready().is_empty());
    }

    #[test]
    fn the_reference_offers_every_voice() {
        // The choice list in the core reference is typed by hand, so it can
        // drift from the voices the synthesizer answers to.
        let set = rustel_core::reference::reference_choices("talkit", "config.personality")
            .expect("the voice choices");
        let offered: Vec<&str> = set.choices.iter().map(|choice| choice.value).collect();
        let voices: Vec<&str> = Personality::ALL.iter().map(|voice| voice.name()).collect();
        assert_eq!(offered, voices);
    }
}

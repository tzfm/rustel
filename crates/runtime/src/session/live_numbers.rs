//! The live numbers a host asked for, by the score text they belong to.
//!
//! A host moves a plain number of the running score the way it moves a
//! slider: it names the number's range before the text is evaluated, and
//! the evaluation turns that number into a live cell. The ranges belong to
//! one text. A text can wait for its launch line or its sounds while a newer
//! text is sent, so each text keeps its own ranges, found by its revision.

use std::sync::Arc;

use crate::ui_events::{LiveNumber, source_revision};

use super::Session;

/// The texts the store keeps ranges for. A text waits behind at most a few
/// newer ones, so the oldest past this are let go.
const KEPT_TEXTS: usize = 8;

#[derive(Debug, Default)]
pub(super) struct LiveNumbers {
    /// Newest last: the revision of a text and its ranges.
    texts: Vec<(String, Arc<[LiveNumber]>)>,
}

impl LiveNumbers {
    #[inline]
    pub(super) fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }

    fn set(&mut self, revision: String, ranges: Arc<[LiveNumber]>) {
        self.texts.retain(|(kept, _)| *kept != revision);
        if ranges.is_empty() {
            return;
        }
        if self.texts.len() == KEPT_TEXTS {
            self.texts.remove(0);
        }
        self.texts.push((revision, ranges));
    }

    fn of(&self, revision: &str) -> Arc<[LiveNumber]> {
        self.texts
            .iter()
            .rev()
            .find(|(kept, _)| kept == revision)
            .map_or_else(|| Arc::from([]), |(_, ranges)| Arc::clone(ranges))
    }
}

static EMPTY_LIVE_NUMBERS: std::sync::LazyLock<Arc<[LiveNumber]>> =
    std::sync::LazyLock::new(|| Arc::from([]));

impl Session {
    /// The numbers of `source` that its evaluations make live. They
    /// replace the numbers given before for the same text. No numbers
    /// evaluates every number as written.
    pub fn set_live_numbers(&mut self, source: &str, numbers: Vec<LiveNumber>) {
        if numbers.is_empty() && self.live_numbers.texts.is_empty() {
            return;
        }
        self.live_numbers
            .set(source_revision(source), Arc::from(numbers));
    }

    /// The live numbers of `source`: the numbers last given for this text.
    pub fn live_numbers(&self, source: &str) -> Arc<[LiveNumber]> {
        if self.live_numbers.texts.is_empty() {
            return Arc::clone(&EMPTY_LIVE_NUMBERS);
        }
        self.live_numbers.of(&source_revision(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One number, as the store keeps it.
    fn one(start: usize) -> Arc<[LiveNumber]> {
        Arc::from([LiveNumber {
            literal: start..start + 1,
            value: None,
        }])
    }

    #[test]
    fn each_text_keeps_its_own_numbers_and_the_newest_wins() {
        let mut store = LiveNumbers::default();
        store.set("a".into(), one(1));
        store.set("b".into(), one(3));
        store.set("a".into(), one(5));
        assert_eq!(store.of("a"), one(5));
        assert_eq!(store.of("b"), one(3));
        store.set("a".into(), Arc::from([]));
        assert!(store.of("a").is_empty());
        for text in 0..=KEPT_TEXTS {
            store.set(text.to_string(), one(text));
        }
        assert!(store.of("b").is_empty(), "the oldest text is let go");
        assert_eq!(store.of(&KEPT_TEXTS.to_string()), one(KEPT_TEXTS));
    }

    /// A number its host moved starts at the moved value, not at the
    /// literal, so a reinstall of the same text plays on with no jump.
    #[test]
    fn a_moved_number_starts_where_its_host_left_it() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let source = "note(48).s('sine').gain(0.5)";
        let at = source.find("0.5").expect("the gain");
        let literal = at..at + 3;
        session.set_live_numbers(
            source,
            vec![LiveNumber {
                literal: literal.clone(),
                value: Some(0.25),
            }],
        );
        session.evaluate(source).expect("score");
        let id = rustel_transpiler::live_number_id(literal);
        assert_eq!(session.slider_value(&id).expect("slider read"), Some(0.25));
    }
}

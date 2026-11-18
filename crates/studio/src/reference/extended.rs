//! The extended search: entries found by what their documentation says,
//! not by their names.
//!
//! The basic search ranks names, synonyms, keywords and summaries. When a
//! reader types the name of an extension, a word from a description or a
//! parameter, the basic search has nothing to give. The extended search
//! answers with the entries whose text holds every typed word. Its
//! results always follow the basic results, so a name match is never
//! pushed down.

use super::Entry;
use crate::fuzzy::Score;

/// The shortest query, in characters, the extended search reads. One or
/// two letters are in almost every description, so they find noise.
const MIN_QUERY_CHARS: usize = 3;

/// Where a word was found, strongest first. A word at the start of a word
/// of the text gets one more point than a word inside one.
const NAME: Score = 16;
const SUMMARY: Score = 14;
const ORIGIN: Score = 12;
const TAG: Score = 10;
const PARAM: Score = 8;
const DESCRIPTION: Score = 6;
const PARAM_DETAIL: Score = 4;
const EXAMPLE: Score = 2;

/// The typed words, lower case, for the extended search.
pub(super) struct ExtendedQuery {
    words: Vec<String>,
}

impl ExtendedQuery {
    /// `None` when the query is too short to read.
    pub(super) fn parse(query: &str) -> Option<Self> {
        let query = query.trim();
        if query.chars().count() < MIN_QUERY_CHARS {
            return None;
        }
        let words = query.split_whitespace().map(str::to_lowercase).collect();
        Some(Self { words })
    }

    /// How well the entry's text answers the query. Every word must be
    /// found. The weakest word sets the score.
    pub(super) fn score(&self, entry: &Entry) -> Option<Score> {
        self.words.iter().try_fold(Score::MAX, |score, word| {
            Some(score.min(mention(entry, word)?))
        })
    }
}

/// The strongest place `word` appears in the entry's text.
fn mention(entry: &Entry, word: &str) -> Option<Score> {
    let names = std::iter::once(&entry.name)
        .chain(&entry.synonyms)
        .chain(&entry.keywords)
        .map(|text| (NAME, text.as_str()));
    let params = entry
        .params
        .iter()
        .map(|param| (PARAM, param.name.as_str()));
    let details = entry.params.iter().flat_map(|param| {
        std::iter::once(param.description.as_str())
            .chain(
                param
                    .choices
                    .iter()
                    .flat_map(|choice| [choice.value.as_str(), choice.description.as_str()]),
            )
            .map(|text| (PARAM_DETAIL, text))
    });
    names
        .chain([
            (SUMMARY, entry.summary.as_str()),
            (ORIGIN, entry.origin.as_str()),
        ])
        .chain(entry.tags.iter().map(|tag| (TAG, tag.as_str())))
        .chain(params)
        .chain([(DESCRIPTION, entry.description.as_str())])
        .chain(details)
        .chain(entry.examples.iter().map(|text| (EXAMPLE, text.as_str())))
        .find_map(|(score, text)| find(text, word).map(|start| score + Score::from(start)))
}

/// Whether `text` holds `word` (already lower case), ignoring case: `None`
/// when not, else whether one match starts a word of the text.
fn find(text: &str, word: &str) -> Option<bool> {
    if word.is_ascii() {
        // No allocation: this runs over every description on each key.
        let (text, word) = (text.as_bytes(), word.as_bytes());
        let last = text.len().checked_sub(word.len())?;
        let mut found = None;
        for at in 0..=last {
            if text[at..at + word.len()].eq_ignore_ascii_case(word) {
                if at == 0 || !text[at - 1].is_ascii_alphanumeric() {
                    return Some(true);
                }
                found = Some(false);
            }
        }
        return found;
    }
    let text = text.to_lowercase();
    text.match_indices(word)
        .map(|(at, _)| {
            !text[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::super::{Choice, Param};
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            synonyms: Vec::new(),
            summary: String::new(),
            description: String::new(),
            params: Vec::new(),
            examples: Vec::new(),
            tags: Vec::new(),
            no_autocomplete: false,
            deprecated: false,
            source: String::new(),
            origin: String::new(),
            snippet: None,
            keywords: Vec::new(),
        }
    }

    fn score(query: &str, entry: &Entry) -> Option<Score> {
        ExtendedQuery::parse(query)?.score(entry)
    }

    #[test]
    fn short_queries_are_not_read() {
        let mut acid = entry("acid");
        acid.description = "a squelchy bass line".to_owned();
        assert_eq!(score("a", &acid), None);
        assert_eq!(score(" sq ", &acid), None);
        assert!(score("squ", &acid).is_some());
    }

    #[test]
    fn every_word_must_be_found_somewhere_in_the_entry() {
        let mut acid = entry("acid");
        acid.origin = "switch_angel".to_owned();
        acid.description = "A squelchy bass line.".to_owned();
        assert!(score("angel bass", &acid).is_some());
        assert!(score("BASS Angel", &acid).is_some(), "case is ignored");
        assert_eq!(score("angel piano", &acid), None);
    }

    #[test]
    fn a_stronger_field_scores_higher_and_the_weakest_word_counts() {
        let mut lpf = entry("lpf");
        lpf.origin = "rustel".to_owned();
        lpf.description = "Cuts the highs above the cutoff.".to_owned();
        lpf.examples = vec!["s(\"bd\").lpf(800)".to_owned()];
        lpf.params = vec![Param {
            name: "frequency".to_owned(),
            r#type: String::new(),
            description: String::new(),
            choices: vec![Choice {
                value: "ladder".to_owned(),
                description: String::new(),
            }],
        }];
        let origin = score("rustel", &lpf).unwrap();
        let param = score("frequency", &lpf).unwrap();
        let description = score("highs", &lpf).unwrap();
        let choice = score("ladder", &lpf).unwrap();
        let example = score("800", &lpf).unwrap();
        assert!(origin > param && param > description);
        assert!(description > choice && choice > example);
        assert_eq!(score("rustel highs", &lpf), Some(description));
    }

    #[test]
    fn the_start_of_a_word_beats_the_inside_of_one() {
        let mut first = entry("first");
        first.description = "makes a delay".to_owned();
        let mut second = entry("second");
        second.description = "unmakes nothing".to_owned();
        assert!(score("make", &first) > score("make", &second));
    }

    #[test]
    fn non_ascii_words_match_ignoring_case() {
        let mut entry = entry("chant");
        entry.description = "Une ÉCHELLE de sons".to_owned();
        assert!(score("échelle", &entry).is_some());
        assert_eq!(score("éché", &entry), None);
    }
}

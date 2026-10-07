//! One way of matching a typed word against a list of names.
//!
//! Every list the studio offers - functions, sounds, chords, scales,
//! colours - is searched with this, so `chr` finds `chord` in all of them
//! and the nearest answer is first. The rules, in the order they win:
//!
//! 1. the name, exactly;
//! 2. the name from its start (`lpf` → `lpfattack`);
//! 3. a part of the name from its start, where a part begins after `_`,
//!    `:`, `-`, a digit boundary or a capital (`sine` → `z_sine`,
//!    `pent` → `major:pentatonic`, `attack` → `lpAttack`);
//! 4. the name's letters in order, gaps allowed (`chr` → `chord`,
//!    `smtms` → `sometimes`), the tighter the better;
//! 5. the name a slip of the fingers away (`lpff` → `lpf`), by edit
//!    distance;
//! 6. the word somewhere in what the name is *about* - a summary - which
//!    is the weakest reason to offer something and is ranked last.
//!
//! A shorter name wins a tie, because `chord` is a likelier answer to
//! `chr` than `chordAlteration` is.

/// How well a name answers a query. Bigger is better; `None` is no match.
/// The scale is arbitrary and only ever compared with itself.
pub type Score = i32;

const EXACT: Score = 1_000;
const PREFIX: Score = 800;
const PART_PREFIX: Score = 600;
const SUBSEQUENCE: Score = 400;
const TYPO: Score = 200;
const SHARED: Score = 150;
const ABOUT: Score = 100;

/// Where a part of a name begins: the start, after a separator, at a
/// capital that follows a lowercase, or at the first digit of a run.
fn part_starts(name: &str) -> Vec<usize> {
    let letters: Vec<char> = name.chars().collect();
    let mut starts = vec![0usize];
    for (index, letter) in letters.iter().enumerate().skip(1) {
        let previous = letters[index - 1];
        let separator = matches!(previous, '_' | ':' | '-' | ' ' | '.' | '/');
        let hump = letter.is_uppercase() && previous.is_lowercase();
        let digit = letter.is_ascii_digit() && !previous.is_ascii_digit();
        if separator || hump || digit {
            starts.push(index);
        }
    }
    starts
}

/// The letters of `query` in order inside `name`, starting at `from`, with
/// a bonus for keeping them together. `None` when a letter is missing or
/// the first one is not at `from`.
fn subsequence_from(query: &[char], name: &[char], from: usize) -> Option<Score> {
    if name.get(from) != query.first() {
        return None;
    }
    let mut at = from + 1;
    let mut runs = 0i32;
    let mut previous = from;
    for letter in &query[1..] {
        let found = name[at..]
            .iter()
            .position(|candidate| candidate == letter)?
            + at;
        if found == previous + 1 {
            runs += 1;
        }
        previous = found;
        at = found + 1;
    }
    let span = previous + 1 - from;
    let tightness = (query.len() as i32 * 4) - (span as i32 - query.len() as i32);
    Some(SUBSEQUENCE + tightness + runs * 2 - from as i32)
}

/// The letters of `query` in order inside `name`, beginning at the start
/// of the name or of one of its parts. Anchoring the first letter is what
/// keeps `mi` from finding `gm_piano` while `chr` still finds `chord`.
fn subsequence(query: &[char], name: &[char], starts: &[usize]) -> Option<Score> {
    starts
        .iter()
        .filter_map(|start| subsequence_from(query, name, *start))
        .max()
}

/// How many characters two words share from their starts.
fn shared_prefix(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

/// The score of one name against one query, both trimmed of case.
pub fn name_score(query: &str, name: &str) -> Option<Score> {
    if query.is_empty() {
        return Some(0);
    }
    let lowered = name.to_lowercase();
    let query_lowered = query.to_lowercase();
    if lowered == query_lowered {
        return Some(EXACT);
    }
    // A shorter name answers a short query better: `chord` before
    // `chordAlteration`.
    let brevity = -(name.chars().count() as i32);
    if lowered.starts_with(&query_lowered) {
        return Some(PREFIX + brevity);
    }
    let starts = part_starts(name);
    for start in &starts {
        let part: String = name.chars().skip(*start).collect::<String>().to_lowercase();
        if part.starts_with(&query_lowered) {
            return Some(PART_PREFIX + brevity - *start as i32);
        }
    }
    let query_letters: Vec<char> = query_lowered.chars().collect();
    let name_letters: Vec<char> = lowered.chars().collect();
    if let Some(score) = subsequence(&query_letters, &name_letters, &starts) {
        return Some(score + brevity / 4);
    }
    // A typo: one edit per three characters typed, at least one.
    let allowed = (query_letters.len() / 3).max(1);
    let distance = rustel_runtime::lint::edit_distance(&query_lowered, &lowered);
    if distance <= allowed {
        return Some(TYPO - distance as Score + brevity / 4);
    }
    // Half a name is still a question: `someto` finds the `sometimes`
    // family, which no rule above reaches - nothing contains it and every
    // name is several edits away.
    let shared = shared_prefix(&query_lowered, &lowered);
    if shared >= 3 {
        return Some(SHARED + shared as Score + brevity / 4);
    }
    None
}

/// The best score across a name and its other names. A match in what the
/// thing is about scores lowest.
pub fn score(query: &str, name: &str, aliases: &[String], about: &str) -> Option<Score> {
    let best = std::iter::once(name)
        .chain(aliases.iter().map(String::as_str))
        .filter_map(|candidate| name_score(query, candidate))
        .max();
    if best.is_some() {
        return best;
    }
    if !query.is_empty() && about.to_lowercase().contains(&query.trim().to_lowercase()) {
        return Some(ABOUT);
    }
    None
}

/// Rank `names` against `query`, best first. Ties keep the order the list
/// was given in, which is how a list that put its common names first
/// keeps them first.
pub fn rank<'a>(query: &str, names: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let mut ranked = names
        .enumerate()
        .filter_map(|(index, name)| name_score(query, name).map(|score| (score, index)))
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    ranked.into_iter().map(|(_, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The letters of the word, in order, with gaps: what a clumsy typist
    /// actually types.
    #[test]
    fn a_few_letters_of_a_word_find_the_word() {
        for (query, wanted) in [
            ("chr", "chord"),
            ("smtms", "sometimes"),
            ("plyhd", "playhead"),
            ("lpf", "lpf"),
            ("sine", "z_sine"),
            ("pent", "major:pentatonic"),
            ("attack", "lpAttack"),
        ] {
            assert!(
                name_score(query, wanted).is_some(),
                "{query:?} did not find {wanted:?}"
            );
        }
        assert!(
            name_score("chr", "channels").is_none(),
            "and not everything"
        );
        // The letters must start a name or one of its parts, or `mi`
        // would find `gm_piano` and every search would be a haystack.
        assert!(name_score("mi", "gm_piano").is_none());
        assert!(name_score("mi", "mine").is_some());
        // Half a name still finds its family.
        assert!(name_score("someto", "sometimesBy").is_some());
    }

    /// The nearest answer is first: exact, then from the start, then a
    /// part of the name, then the letters in order.
    #[test]
    fn the_best_match_comes_first() {
        let names = [
            "channels",
            "chord",
            "chordAlteration",
            "chooseCycles",
            "chunk",
        ];
        let ranked = rank("chr", names.iter().copied());
        assert_eq!(names[ranked[0]], "chord", "{ranked:?}");

        let ranked = rank("ch", names.iter().copied());
        assert_eq!(names[ranked[0]], "chord", "the shortest of the prefixes");

        let ranked = rank("chord", names.iter().copied());
        assert_eq!(names[ranked[0]], "chord", "an exact name wins outright");
        assert_eq!(names[ranked[1]], "chordAlteration");
    }

    /// A slip of one key still finds the word.
    #[test]
    fn a_typo_finds_its_word() {
        assert!(name_score("lpff", "lpf").is_some());
        assert!(name_score("gian", "gain").is_some());
        assert!(name_score("supersw", "supersaw").is_some());
    }

    /// An empty query keeps everything, in the order it was given - a
    /// list that put its common names first keeps them first.
    #[test]
    fn an_empty_query_keeps_the_list_in_its_own_order() {
        let names = ["b", "a", "c"];
        assert_eq!(rank("", names.iter().copied()), vec![0, 1, 2]);
        assert!(name_score("", "anything").is_some());
    }

    /// What a thing is about is the weakest reason to offer it.
    #[test]
    fn a_summary_match_ranks_below_a_name_match() {
        let by_name = score("delay", "delay", &[], "").expect("name");
        let by_about = score("delay", "room", &[], "a delay in the room").expect("about");
        assert!(by_name > by_about);
        assert!(score("delay", "room", &[], "nothing to do with it").is_none());
    }
}

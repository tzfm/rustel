//! The embedded, searchable examples catalogue.
//!
//! The JSON catalogue is the single source for the Examples tree.
//! `include_str!` keeps the shipped catalogue available in minimal and
//! packaged builds without a runtime filesystem lookup.

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

/// Version of the on-disk catalogue contract.
pub const SCHEMA_VERSION: u32 = 1;

/// The catalogue is compiled into the binary.
pub const EMBEDDED_JSON: &str = include_str!("../assets/examples.json");

/// Whether an example is a playable Rustel score or a Hydra visual sketch.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExampleKind {
    Music,
    Hydra,
}

/// One displayable example from the catalogue.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Example {
    /// Stable identifier used by history and saved selections.
    pub id: String,
    pub name: String,
    pub kind: ExampleKind,
    /// Top-level Examples tab section (`parts`, `tracks`, `sound-design`, or
    /// `hydra`).
    pub section: String,
    /// Human-readable shelf/genre name.
    pub category: String,
    /// Searchable labels. The first entries are intentionally broad and
    /// stable; more labels can be added without changing the code field.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// Short teaching note shown beside the code.
    pub explanation: String,
    /// One focused adjustment a learner can try next.
    pub change: String,
    /// Suggested listening tempo. This is descriptive and never changes the
    /// active transport when an example is previewed.
    #[serde(default)]
    pub bpm: Option<u16>,
    /// Named bundled assets required by the example, when applicable.
    #[serde(default)]
    pub assets: Vec<String>,
    /// Whether this example needs the optional extension registry.
    #[serde(default)]
    pub requires_extensions: bool,
    pub code: String,
}

#[derive(Debug, Deserialize)]
struct RawCatalogue {
    schema_version: u32,
    entries: Vec<Example>,
}

/// Why a catalogue could not be loaded or indexed.
#[derive(Debug)]
pub enum ParseError {
    Json(serde_json::Error),
    UnsupportedSchema(u32),
    Empty,
    EmptyField { id: String, field: &'static str },
    DuplicateId(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "invalid examples JSON: {error}"),
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported examples schema version {version}")
            }
            Self::Empty => f.write_str("examples catalogue is empty"),
            Self::EmptyField { id, field } => write!(f, "example {id:?} has an empty {field}"),
            Self::DuplicateId(id) => write!(f, "duplicate example id {id:?}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<serde_json::Error> for ParseError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Parsed examples plus an ID index for cheap selection restoration.
#[derive(Clone, Debug)]
pub struct ExampleCatalogue {
    schema_version: u32,
    entries: Vec<Example>,
    by_id: HashMap<String, usize>,
}

impl ExampleCatalogue {
    /// Parse and validate a schema-v1 catalogue.
    pub fn parse(json: &str) -> Result<Self, ParseError> {
        let raw: RawCatalogue = serde_json::from_str(json)?;
        if raw.schema_version != SCHEMA_VERSION {
            return Err(ParseError::UnsupportedSchema(raw.schema_version));
        }
        if raw.entries.is_empty() {
            return Err(ParseError::Empty);
        }

        let mut by_id = HashMap::with_capacity(raw.entries.len());
        for (index, entry) in raw.entries.iter().enumerate() {
            for (field, value) in [
                ("id", entry.id.as_str()),
                ("name", entry.name.as_str()),
                ("section", entry.section.as_str()),
                ("category", entry.category.as_str()),
                ("explanation", entry.explanation.as_str()),
                ("change", entry.change.as_str()),
                ("code", entry.code.as_str()),
            ] {
                if value.trim().is_empty() {
                    return Err(ParseError::EmptyField {
                        id: entry.id.clone(),
                        field,
                    });
                }
            }
            if by_id.insert(entry.id.clone(), index).is_some() {
                return Err(ParseError::DuplicateId(entry.id.clone()));
            }
        }

        Ok(Self {
            schema_version: raw.schema_version,
            entries: raw.entries,
            by_id,
        })
    }

    /// The schema version this catalogue was parsed from.
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Example> {
        self.entries.iter()
    }

    /// Find an example by its stable ID.
    pub fn get(&self, id: &str) -> Option<&Example> {
        self.by_id
            .get(id)
            .and_then(|index| self.entries.get(*index))
    }

    /// Fuzzy search names, categories and tags, retaining catalogue order for
    /// ties. An empty query returns the common-first catalogue order.
    pub fn search(&self, query: &str) -> Vec<&Example> {
        let query = query.trim();
        let mut ranked = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let score = std::iter::once(entry.name.as_str())
                    .chain(std::iter::once(entry.category.as_str()))
                    .chain(std::iter::once(entry.section.as_str()))
                    .chain(std::iter::once(entry.explanation.as_str()))
                    .chain(std::iter::once(entry.change.as_str()))
                    .chain(entry.tags.iter().map(String::as_str))
                    .filter_map(|candidate| super::fuzzy::name_score(query, candidate))
                    .max()?;
                Some((score, index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, index)| (std::cmp::Reverse(*score), *index));
        ranked
            .into_iter()
            .filter_map(|(_, index)| self.entries.get(index))
            .collect()
    }
}

/// Parse the embedded catalogue once and share it across the Examples tab.
pub fn embedded() -> &'static ExampleCatalogue {
    static CATALOGUE: OnceLock<ExampleCatalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let mut catalogue = ExampleCatalogue::parse(EMBEDDED_JSON)
            .expect("the embedded examples catalogue must be valid schema v1");
        catalogue
            .entries
            .retain(|entry| !entry.requires_extensions || cfg!(feature = "extensions"));
        catalogue.by_id = catalogue
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect();
        catalogue
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_examples_are_only_offered_when_available() {
        let source = ExampleCatalogue::parse(EMBEDDED_JSON).expect("catalogue");
        for id in [
            "music-parts-melody-arpeggios-strummed-chords",
            "music-parts-melody-arpeggios-trance-arpeggio",
        ] {
            let entry = source.get(id).expect("extension example");
            assert!(entry.requires_extensions);
            assert_eq!(embedded().get(id).is_some(), cfg!(feature = "extensions"));
            assert_eq!(
                embedded()
                    .search(&entry.name)
                    .iter()
                    .any(|entry| entry.id == id),
                cfg!(feature = "extensions")
            );
        }
    }

    #[test]
    fn all_music_examples_evaluate_as_lanes_and_as_a_stack() {
        for entry in embedded()
            .iter()
            .filter(|entry| entry.kind == ExampleKind::Music)
        {
            for code in [laned(&entry.code), stacked(&entry.code)] {
                let mut session = rustel_runtime::Session::new().expect("session");
                session
                    .evaluate(&code)
                    .unwrap_or_else(|error| panic!("{}: {error}\n{code}", entry.id));
            }
        }
    }

    #[test]
    fn embedded_catalogue_is_schema_v1_and_contains_every_legacy_entry() {
        let catalogue = ExampleCatalogue::parse(EMBEDDED_JSON).expect("catalogue");
        assert_eq!(catalogue.schema_version(), SCHEMA_VERSION);
        assert!(catalogue.len() >= 210);
        assert!(catalogue.iter().all(|entry| !entry.explanation.is_empty()));
        assert!(catalogue.iter().all(|entry| !entry.change.is_empty()));
        assert!(
            catalogue
                .iter()
                .filter(|entry| entry.kind == ExampleKind::Music)
                .count()
                >= 184
        );
        assert!(
            catalogue
                .iter()
                .filter(|entry| entry.kind == ExampleKind::Hydra)
                .count()
                >= 26
        );
    }

    #[test]
    fn ids_are_indexed_and_unique() {
        let catalogue = embedded();
        for example in catalogue.iter() {
            assert_eq!(catalogue.get(&example.id), Some(example));
        }
        assert_eq!(catalogue.by_id.len(), catalogue.len());
    }

    #[test]
    fn search_finds_categories_and_keeps_exact_names_first() {
        let catalogue = embedded();
        let exact = catalogue.search("Four on the floor");
        assert_eq!(
            exact.first().map(|entry| entry.name.as_str()),
            Some("Four on the floor")
        );

        let hydra = catalogue.search("kaleid");
        assert!(!hydra.is_empty());
        assert!(hydra.iter().any(|entry| entry.category == "Kaleidoscope"));
    }

    #[test]
    fn parser_rejects_unknown_schema_and_duplicate_ids() {
        let unknown = r#"{"schema_version": 2, "entries": [{"id":"x","name":"x","kind":"music","section":"parts","category":"x","source":null,"explanation":"x","change":"x","code":"$: s(\"bd\")"}]}"#;
        assert!(matches!(
            ExampleCatalogue::parse(unknown),
            Err(ParseError::UnsupportedSchema(2))
        ));

        let duplicate = r#"{"schema_version": 1, "entries": [{"id":"x","name":"x","kind":"music","section":"parts","category":"x","source":null,"explanation":"x","change":"x","code":"$: s(\"bd\")"},{"id":"x","name":"y","kind":"music","section":"parts","category":"x","source":null,"explanation":"y","change":"y","code":"$: s(\"sd\")"}]}"#;
        assert!(matches!(
            ExampleCatalogue::parse(duplicate),
            Err(ParseError::DuplicateId(id)) if id == "x"
        ));
    }
}

/// Borrowed views into the embedded JSON, used by the tree and previews.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snippet {
    pub name: &'static str,
    pub source: Option<&'static str>,
    pub code: &'static str,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Category {
    pub name: &'static str,
    pub snippets: Vec<Snippet>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    pub name: &'static str,
    pub kind: ExampleKind,
    pub shelves: Vec<Category>,
}
impl Section {
    pub fn count(&self) -> usize {
        self.shelves.iter().map(|s| s.snippets.len()).sum()
    }
}
pub use ExampleKind as Kind;
pub const DEFAULT_SECTION: usize = 0;
pub static SECTIONS: std::sync::LazyLock<Vec<Section>> = std::sync::LazyLock::new(|| {
    let mut sections = Vec::new();
    for (key, name, kind) in [
        ("parts", "Parts", Kind::Music),
        ("tracks", "Tracks", Kind::Music),
        ("sound-design", "Sound design", Kind::Music),
        ("hydra", "Hydra", Kind::Hydra),
    ] {
        let mut shelves: Vec<Category> = Vec::new();
        for entry in embedded().iter().filter(|entry| entry.section == key) {
            let index = shelves
                .iter()
                .position(|s| s.name == entry.category)
                .unwrap_or_else(|| {
                    shelves.push(Category {
                        name: &entry.category,
                        snippets: Vec::new(),
                    });
                    shelves.len() - 1
                });
            shelves[index].snippets.push(Snippet {
                name: &entry.name,
                source: entry.source.as_deref(),
                code: &entry.code,
            });
        }
        if !shelves.is_empty() {
            sections.push(Section {
                name,
                kind,
                shelves,
            });
        }
    }
    sections
});
pub fn section_of(kind: Kind) -> Option<usize> {
    SECTIONS.iter().position(|s| s.kind == kind)
}
pub fn count() -> usize {
    embedded().len()
}

/// Fold a score's voices into one `$: stack(...)`.
///
/// A composed track arrives as one `$:` a voice, which is how a set is
/// played: each line is muted, soloed and edited on its own. Taken into
/// somewhere that wants a single pattern - a scene slot, a line in a
/// larger chain - the same voices want to be one row instead. This is
/// that reading of the same music; nothing about it is rearranged.
///
/// Lines that are not voices (the tempo, a comment) keep their place
/// above the stack. One voice folds the same as several - the reader
/// asked for a stack - but a voice that is already the `stack(...)` call
/// is the stack itself, and is returned as it is rather than stacked
/// again.
pub fn stacked(code: &str) -> String {
    let mut before: Vec<&str> = Vec::new();
    let mut voices: Vec<String> = Vec::new();
    for line in code.lines() {
        match line.strip_prefix("$:") {
            Some(voice) => voices.push(voice.trim().to_owned()),
            // A chain continued on the next row belongs to the voice above
            // it; anything else before the first voice is a heading.
            None if line.starts_with(char::is_whitespace) && !line.trim().is_empty() => {
                match voices.last_mut() {
                    Some(voice) => {
                        voice.push('\n');
                        voice.push_str("    ");
                        voice.push_str(line.trim_start());
                    }
                    None => before.push(line),
                }
            }
            None if line.trim().is_empty() && !voices.is_empty() => {}
            None => before.push(line),
        }
    }
    if voices.is_empty() || (voices.len() == 1 && voices[0].starts_with("stack(")) {
        return code.to_owned();
    }
    let mut out = String::new();
    for line in before {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("$: stack(\n");
    for (index, voice) in voices.iter().enumerate() {
        out.push_str("  ");
        out.push_str(voice);
        if index + 1 < voices.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push(')');
    out
}

/// Split a `$: stack(...)` back into one `$:` a voice.
///
/// The other reading of the same music, and the inverse of [`stacked`]: a
/// track folded for somewhere that wants one pattern can be taken back to
/// the set's own shape, a voice a line, where each part is muted, soloed
/// and edited on its own. A chain after the call belongs to the pattern the
/// call makes, so it goes on every voice: `stack(a, b).bank("tr909")` reads
/// as `a.bank("tr909")` beside `b.bank("tr909")`.
///
/// Voices that were never folded keep their lines - a genre track is a kit
/// `$: stack(...)` beside its bass and chords, and only the kit is split.
/// A stack of one voice reads back as that one voice, so the fold of a
/// single pattern is undone the same way. A track with no stack call comes
/// back untouched: there is nothing to split; two stack calls is two
/// answers, and is left for a reader.
pub fn laned(code: &str) -> String {
    // Exactly one `$: stack(...)` line, wherever it sits. A track with
    // several `$:` lines is the usual shape - a kit beside its parts -
    // and the voices beside the stack are not the stack's seam.
    let mut seam: Option<(usize, usize)> = None;
    let mut at = 0usize;
    for line in code.split_inclusive('\n') {
        if let Some(body) = line.strip_prefix("$:") {
            let lead = body.len() - body.trim_start().len();
            let body_start = at + 2 + lead;
            if code[body_start..].starts_with("stack(") {
                if seam.is_some() {
                    return code.to_owned();
                }
                seam = Some((at, body_start));
            }
        }
        at += line.len();
    }
    let Some((line_start, body_start)) = seam else {
        return code.to_owned();
    };
    let inside = body_start + "stack(".len();
    let Some(close) = matching_close(code, inside) else {
        return code.to_owned();
    };
    // The rest of the line the call ends on is a chain, or nothing. A
    // comment after it would have to be copied onto every voice, so it
    // leaves the code alone instead.
    let after = code[close + 1..].split('\n').next().unwrap_or("");
    let chain = after.trim();
    if chain.contains("//") {
        return code.to_owned();
    }
    let voices = split_arguments(&code[inside..close]);
    if voices.is_empty() {
        return code.to_owned();
    }
    let mut out = String::from(&code[..line_start]);
    for voice in &voices {
        out.push_str("$: ");
        out.push_str(voice);
        out.push_str(chain);
        out.push('\n');
    }
    // What followed the call's own line keeps its place after the voices,
    // minus the newline the voices already ended with.
    let mut tail = &code[close + 1 + after.len()..];
    if tail.starts_with('\n') {
        tail = &tail[1..];
    }
    out.push_str(tail);
    if !code.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// The byte index of the `)` that closes the call opened just before
/// `from`. Quotes and template literals inside a voice's own calls are
/// skipped: a `)` in a mini string is not the end of the stack.
fn matching_close(code: &str, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut chars = code[from..].char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if let Some(open) = quote {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return Some(from + at),
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The top-level comma-separated pieces of a stack's arguments.
fn split_arguments(inside: &str) -> Vec<String> {
    let mut voices = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let mut chars = inside.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if let Some(open) = quote {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                voices.push(inside[start..at].trim().to_owned());
                start = at + 1;
            }
            _ => {}
        }
    }
    voices.push(inside[start..].trim().to_owned());
    voices.retain(|voice| !voice.is_empty());
    voices
}

#[cfg(test)]
mod formatting_tests {
    use super::*;
    #[test]
    fn a_stacked_track_reads_back_as_voices() {
        let kit = "$: stack(s(\"bd*4\").gain(1), s(\"[~ cp]*2\").gain(.8)).bank(\"RolandTR909\")";
        assert_eq!(
            laned(kit),
            "$: s(\"bd*4\").gain(1).bank(\"RolandTR909\")\n$: s(\"[~ cp]*2\").gain(.8).bank(\"RolandTR909\")"
        );
    }

    /// The fold's own multi-line form reads back into the same voices.
    #[test]
    fn the_folded_form_reads_back_as_voices() {
        let lanes = "$: s(\"bd*4\").gain(1)\n$: s(\"hh*8\").gain(.4)\n";
        let folded = stacked(lanes);
        assert_eq!(
            laned(&folded),
            "$: s(\"bd*4\").gain(1)\n$: s(\"hh*8\").gain(.4)"
        );
    }

    /// One pattern is a score too: the s handoff folds it the same way,
    /// and the next press unfolds it back to the one voice.
    #[test]
    fn a_single_voice_folds_and_unfolds() {
        let single = "$: s(\"bd\").gain(1)";
        let folded = stacked(single);
        assert_eq!(folded, "$: stack(\n  s(\"bd\").gain(1)\n)");
        assert_eq!(laned(&folded), single);
    }

    /// A kit that is already one `$: stack(...)` is the stack, so folding
    /// leaves it as it is instead of stacking the stack.
    #[test]
    fn an_already_stacked_kit_is_not_stacked_again() {
        let kit = "$: stack(s(\"bd\")).bank(\"RolandTR909\")";
        assert_eq!(stacked(kit), kit);
        assert_eq!(laned(kit), "$: s(\"bd\").bank(\"RolandTR909\")");
    }

    /// A kit beside its parts - a genre track's own shape - splits only
    /// the kit; the voices that were never folded keep their lines.
    #[test]
    fn a_kit_beside_its_parts_splits_only_the_kit() {
        let track = "// 123 bpm, house\n$: stack(s(\"bd*4\").gain(1), s(\"hh*8\").gain(.4)).bank(\"RolandTR909\")\n$: n(\"0\").scale(\"C2:major\").s(\"gm_synth_bass_1\")\n$: chord(\"C G\").voicing().s(\"supersaw\")\n";
        assert_eq!(
            laned(track),
            "// 123 bpm, house\n$: s(\"bd*4\").gain(1).bank(\"RolandTR909\")\n$: s(\"hh*8\").gain(.4).bank(\"RolandTR909\")\n$: n(\"0\").scale(\"C2:major\").s(\"gm_synth_bass_1\")\n$: chord(\"C G\").voicing().s(\"supersaw\")\n"
        );
    }

    /// Two stack calls is two answers, and is left alone rather than
    /// guessed at.
    #[test]
    fn two_stack_calls_are_left_alone() {
        let track = "$: stack(s(\"bd\"), s(\"sd\"))\n$: stack(s(\"hh\"), s(\"oh\"))\n";
        assert_eq!(laned(track), track);
    }

    /// A track already a voice a line is the shape the split would make,
    /// so it comes back as it is.
    #[test]
    fn voices_and_single_voices_are_left_alone() {
        let lanes = "$: s(\"bd\")\n$: s(\"sd\")\n";
        assert_eq!(laned(lanes), lanes);
        let single = "$: s(\"bd\")\n";
        assert_eq!(laned(single), single);
    }

    /// A comma inside a voice's own call is not a seam between voices.
    #[test]
    fn commas_inside_calls_do_not_split_voices() {
        let stack = "$: stack(s(\"bd:4*4\"), n(\"0,4\").s(\"saw\"))";
        assert_eq!(laned(stack), "$: s(\"bd:4*4\")\n$: n(\"0,4\").s(\"saw\")");
    }

    /// A comment after the stack would have to be copied onto every
    /// voice, so it is left for a reader instead.
    #[test]
    fn a_comment_after_the_stack_leaves_it_alone() {
        let stack = "$: stack(s(\"bd\"), s(\"sd\")) // the kit\n";
        assert_eq!(laned(stack), stack);
    }
}

//! The reference: this engine's documentation, inside the studio.
//!
//! Every entry lives in the code that installs the name - the controls
//! table, the combinator registry, the painters, the synth voices, the
//! chord/scale catalogues, and the extension and host surfaces - and is
//! assembled here at load time. Prose adapted from the Strudel project's
//! JSDoc keeps its licence notice where it lives. Compatibility names listed
//! in `REFERENCE_HIDDEN` and `COMBINATORS_REFERENCE_HIDDEN` remain callable
//! but are omitted from this panel.
//!
//! Nothing here pops up on its own. Ctrl+D shows the word under the caret;
//! Ctrl+F (or F2, or Ctrl+Space where a platform leaves it alone) opens a
//! searchable list. Enter inserts a function call and keeps its parameter
//! reference beside the editor; value choices insert their names.
//!
//! `catalogue` and `vocabulary` build the searchable entries. `panel` handles
//! navigation, with tab-specific state changes in `samples` and `snippets`.
//! `view`, `text`, and `audition` draw the panel; `geometry` shares its layout
//! with hit-testing, and `selection` maps displayed text back to its source.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use serde::Deserialize;
use unicode_width::UnicodeWidthStr;

use super::syntax;
use super::theme::Theme;
use rustel_runtime::samples::{SoundCategory, SoundEntry, SoundOrigin, SourceState};

mod sound_effect;
pub(crate) use sound_effect::SoundEffect;

mod audition;
mod catalogue;
mod geometry;
mod panel;
mod samples;
mod selection;
#[cfg(feature = "hydra")]
mod snippets;
mod text;
mod view;
mod vocabulary;

#[cfg(test)]
use audition::render_sample_shape;
pub use audition::{
    PREVIEW_CEIL_DB, PREVIEW_FLOOR_DB, audition_peak_db, format_preview_gain, preview_gain_at,
    spinner_glyph, tab_previews,
};
use audition::{keyboard_rows, render_keyboard, render_samples_pulse};
#[cfg(test)]
use catalogue::canonical_spellings;
use catalogue::display_origin;
use geometry::tabs;
pub use geometry::{
    entry_body_area, inner_area, samples_pulse_rows, samples_wave_area, samples_wave_rows, tab_at,
};
#[cfg(feature = "hydra")]
pub use geometry::{generator_rail, preview_area, snippet_layout};
pub(super) use geometry::{sample_pulse_rows, tab_layout};
pub use samples::{section_labels, source_labels};
pub use text::entry_body;
#[cfg(any(feature = "hydra", test))]
use text::render_code_span;
use text::{elide, render_code_line, render_inline_line, strip_inline_ticks};
#[cfg(feature = "hydra")]
use text::{paint_mark, wrap, wrap_code_spans};
#[cfg(test)]
use text::{parse_inline, wrap_code, wrap_inline};
#[cfg(feature = "hydra")]
use view::generator_action_label;
#[cfg(test)]
use view::sample_source_line;
#[cfg(all(test, feature = "hydra"))]
use view::{ALIASES_SHOWN, aliases_text};
use vocabulary::CHORD_PREVIEW_OCTAVE;
pub(crate) use vocabulary::tonic_root;
pub use vocabulary::{
    CHORD_ORDER, CHORD_ROOTS, SCALE_ORDER, SCALE_PREVIEW_OCTAVE, chord_vocabulary,
    color_vocabulary, edo_vocabulary, pretty_notation, scale_notes, scale_vocabulary, tuning_notes,
    tuning_vocabulary,
};

/// Variant rows an expanded bank shows at most; a soundfont with hundreds
/// of files is still one sound to a score.
const MAX_VARIANT_ROWS: usize = 64;
/// How far a snippet falls when the caret is not on a blank line. Larger
/// than any score the ranker gives, so they land after every real match
/// rather than merely below the good ones.
const SNIPPET_OFF_LINE_PENALTY: i32 = 100_000;

/// The most characters a search box holds. The limit is longer than the
/// longest name the studio knows. It also bounds the ranking cost of an
/// accidental long input, such as a dropped path or a wrong paste.
pub(crate) const MAX_QUERY_CHARS: usize = 64;

/// The upstream Strudel revision the ported prose came from.
const UPSTREAM_DOC_REVISION: &str = "ebc25467c0f117a7e7b9b11378e21cac94d730fd";

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Param {
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub description: String,
    /// Finite values this parameter accepts, with the same explanations the
    /// editor's Ctrl+D picker displays.
    #[serde(default)]
    pub choices: Vec<Choice>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Choice {
    pub value: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Entry {
    pub name: String,
    #[serde(default)]
    pub synonyms: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default)]
    pub examples: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub no_autocomplete: bool,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub source: String,
    /// Credit used to group extension entries. Empty for other entries.
    /// Source-table origins can identify documentation authors instead;
    /// those are cleared when ingesting non-extension entries.
    #[serde(default)]
    pub origin: String,
    /// Set for a snippet: not a function but lines to paste, and this is
    /// what Enter pastes. Its examples show the same lines.
    #[serde(default)]
    pub snippet: Option<String>,
    /// What a reader might type looking for it, ranked like another name
    /// and never shown as one: `microphone` finds the audio input.
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// Every documented function this engine plays, indexed by name and synonym.
#[derive(Clone, Debug, Default)]
pub struct Reference {
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
    revision: String,
    /// Documented but filtered out by the `known` predicate; reported,
    /// not shown.
    omitted: usize,
    /// Categories the settings hide from the browse list and the
    /// suggestions. Lookup by name still finds their entries.
    hidden: Vec<Category>,
}

/// A kind of entry the settings hide from the browse list and the editor
/// suggestions. An entry joins a kind through the kind's tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    /// Controls only an OSC receiver such as SuperDirt reads, and `osc`.
    Osc,
    /// Output over a serial port.
    Serial,
    /// The FM matrix cells, `fmi11` to `fmi88`, each routing one operator
    /// into another.
    FmMatrix,
    /// The bind and join family, for writing new pattern functions.
    Bind,
    /// Hap and query plumbing such as `withHap` and `splitQueries`.
    Internals,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Osc,
        Self::Serial,
        Self::FmMatrix,
        Self::Bind,
        Self::Internals,
    ];

    /// The tag an entry carries to belong to this category.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Osc => "osc",
            Self::Serial => "serial",
            Self::FmMatrix => "fm_matrix",
            Self::Bind => "bind",
            Self::Internals => "internals",
        }
    }

    /// Whether `entry` belongs to this category.
    pub fn files(self, entry: &Entry) -> bool {
        entry.tags.iter().any(|tag| tag == self.tag())
    }
}

/// The first `tag:` filter in a query and the text to search alongside it.
/// Tag names match case-insensitive prefixes, so `tag:vis piano` finds
/// visualization entries matching `piano` before the tag is fully typed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TagFilter<'a> {
    /// The name after `tag:`, or `None` without a filter. A bare `tag:` gives
    /// `Some("")` and admits every entry that has at least one tag.
    pub name: Option<&'a str>,
    /// Trimmed text after a leading tag, or before a tag later in the query.
    /// Without a tag, this is the whole trimmed query.
    pub rest: &'a str,
}

/// Which tab of the column is open.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tab {
    #[default]
    Reference,
    Samples,
    /// Every chord the voicings know, the ones music actually uses first,
    /// each opening onto its twelve roots.
    Chords,
    /// Every scale, the ones music actually uses first, each opening onto
    /// its twelve tonics and playable.
    Scales,
    #[cfg(feature = "hydra")]
    Generator,
    #[cfg(feature = "hydra")]
    /// The JSON examples, grouped into sections and shelves.
    Examples,
}

impl Tab {
    #[cfg(feature = "hydra")]
    pub fn is_snippets(self) -> bool {
        matches!(self, Self::Examples | Self::Generator)
    }
}

/// One line of the snippets tab.
///
/// Two levels of fold: a section over its shelves of related examples.
#[cfg(feature = "hydra")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnippetLine {
    /// A section heading, by index into [`super::examples::SECTIONS`].
    Section(usize),
    /// A shelf heading: the section, and the shelf within it.
    Shelf(usize, usize),
    /// A snippet: section, shelf, and its place on the shelf.
    Snippet(usize, usize, usize),
    Generator(super::ideas::Row),
}

/// One row of the browse list: a tag heading, or a result under it.
///
/// Both numbers are positions in `results`, never in the reference, so a
/// row survives a refresh the way the results do. A heading is identified
/// by the first entry in its run. The heading text is that entry's own
/// heading, so the row carries nothing else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowseRow {
    Tag(usize),
    Entry(usize),
}

/// One row of the samples tab: a family of banks sharing a machine name, a
/// bank, or one numbered variant of the bank that is expanded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundRow {
    /// A kind of sound, as an index into
    /// [`ReferencePanel::visible_categories`]: the outermost group, which
    /// opens onto its banks. A search lists the sounds themselves and has
    /// no categories.
    Category(usize),
    /// An index into [`ReferencePanel::sound_families`].
    Family(usize),
    Bank(usize),
    Variant(usize, usize),
}

/// Banks that share the name before their first `_`: `AkaiLinn_bd`,
/// `AkaiLinn_sd` and the `AkaiLinn` alias bank are one machine to a reader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundFamily {
    /// What the row reads: the name every member shares.
    pub label: String,
    /// What "open" is remembered by. Two imports can each hold a `KSHMR`
    /// family and they are two rows that fold on their own, so the section
    /// is part of the identity. A string, because it survives a library
    /// reload where an index would not.
    pub key: String,
    /// Indices into `sounds`, in list order.
    pub members: Vec<usize>,
}

/// A section of the samples list: a kind of sound, or one of the score's
/// `samples(…)` imports - each import a heading of its own, at the same
/// level as the drums or the piano, since a library somebody brought in
/// is a collection like any of the pinned ones.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SoundSection {
    Kind(SoundCategory),
    Import(String),
}

/// What the samples selection rested on, for keeping it across a reload.
#[derive(Clone, Debug, PartialEq)]
enum SelectedSound {
    Category(SoundSection),
    /// A family by its [`SoundFamily::key`], never by the label it shows.
    /// Two imports can each hold a family called `KSHMR`; restoring by the
    /// name walked the cursor to whichever came first, and the next Enter
    /// then folded a group the reader was not standing on.
    Family(String),
    Sound(String),
    Variant(String, usize),
}

/// A word list the browse tab can offer instead of the reference: the
/// names that are valid inside a `scale("…")` or `chord("…")` string.
#[derive(Clone, Debug, PartialEq)]
pub struct Vocabulary {
    /// What these words are, for the header: "scales", "chords".
    pub subject: &'static str,
    pub names: Vec<String>,
    /// A short explanation beside each name. Empty for ordinary name lists;
    /// finite parameter choices fill it from the reference catalogue.
    pub details: Vec<String>,
    /// Draw each name in the colour it names, with a swatch: a list of
    /// colours read as words is a list nobody can pick from.
    pub swatches: bool,
    bank_compatibility: Option<BankCompatibility>,
}

#[derive(Clone, Debug, PartialEq)]
struct BankCompatibility {
    /// The sound pattern at the insertion anchor, retained across catalogue updates.
    sounds: Vec<String>,
    /// Compatible names occupy the beginning of the vocabulary.
    count: usize,
    only_compatible: bool,
}

/// One chord quality: the symbol a score writes after the root, what a
/// musician calls it, and how common it is. The order of [`CHORD_ORDER`]
/// is the order they are listed - a reader looking for a minor chord
/// should not scroll past `-^9` to reach it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChordQuality {
    /// The symbol as the voicings dictionary spells it: `-7`, `^7`, `7b9`.
    /// This is what a score must write, so it is what the panel inserts.
    pub symbol: &'static str,
    /// What it is called: "minor 7", "major 7", "dominant 7 flat 9".
    pub name: &'static str,
    /// The same chord as a lead sheet spells it on C, for a reader who
    /// knows `Cm7` and not `C-7`. Shown, never inserted: the dictionary's
    /// shorthand is what plays.
    pub common: &'static str,
}

/// One row of the scales tab: a scale, or one of its tonics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScaleRow {
    /// A scale, as an index into [`ReferencePanel::scale_names`].
    Scale(usize),
    /// That scale on one of the twelve tonics.
    Tonic(usize, usize),
}

/// One row of the chords tab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChordRow {
    /// A quality, as an index into [`ReferencePanel::chord_qualities`].
    Quality(usize),
    /// A chord: the quality it belongs to, and which of the twelve roots.
    Chord(usize, usize),
}

/// What the panel is showing.
#[derive(Clone, Debug, PartialEq)]
pub enum ReferenceMode {
    /// The searchable list.
    Browse,
    /// One entry, opened from the list or from the word under the caret.
    Entry {
        index: usize,
        scroll: u16,
        /// Whether Esc returns to the list rather than closing.
        from_browse: bool,
    },
}

/// The open reference panel's state. The keyboard belongs to it while it is
/// open; everything it needs to draw is here.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferencePanel {
    /// A drag selection in one of the column's read-only text blocks; see
    /// [`PaneSelection`].
    pub selection: Option<PaneSelection>,
    pub tab: Tab,
    pub mode: ReferenceMode,
    pub query: String,
    pub results: Vec<usize>,
    /// The caret is not on a blank line, so a snippet cannot go where it
    /// is. Snippets sink to the bottom of the list rather than leaving it:
    /// wanting one is a reason to make room for it, not a reason to be
    /// told it does not exist.
    pub snippets_last: bool,
    /// The row the cursor is on - a row of [`ReferencePanel::browse_rows`],
    /// not a position in `results`, because a grouped list has headings
    /// between the names. Where the list is not grouped the two are the
    /// same number, which is what every ungrouped path here relies on.
    pub selected: usize,
    /// `results` with the tag headings folded in, rebuilt whenever the
    /// results are. Kept rather than derived because the callers that read
    /// it - `expand`, `confirm` - have no `&Reference` in hand.
    rows: Vec<BrowseRow>,
    /// The samples tab: what the library can play, filtered and walked.
    pub sounds: Vec<SoundEntry>,
    /// The `samples(…)` imports the score names and where each stands, so
    /// one still on its way - no banks in the library yet - is listed all
    /// the same, with a word about it.
    pub imports: Vec<(String, SourceState)>,
    pub sound_query: String,
    /// The machines of the bank that applies where the panel was opened,
    /// when the library holds them. Their sounds rank first and are
    /// inserted without the machine prefix.
    pub sound_prefix: Vec<String>,
    pub sound_results: Vec<usize>,
    pub sound_selected: usize,
    /// The bank whose variants are listed, as an index into `sounds`.
    pub expanded: Option<usize>,
    /// The family whose member banks are listed, by [`SoundFamily::key`] -
    /// a string, because it survives a library reload where an index would
    /// not, and it carries the section so two imports holding a family of
    /// the same name fold one at a time.
    pub open_family: Option<String>,
    /// The examples tree: open sections, open shelves, and the selected row.
    #[cfg(feature = "hydra")]
    pub section_open: std::collections::BTreeSet<usize>,
    #[cfg(feature = "hydra")]
    pub snippet_open: std::collections::BTreeSet<(usize, usize)>,
    #[cfg(feature = "hydra")]
    pub snippet_selected: usize,
    #[cfg(feature = "hydra")]
    pub generator: super::ideas::Generator,
    #[cfg(feature = "hydra")]
    pub snippet_code_scroll: std::cell::Cell<usize>,
    /// A plain word list standing in for the reference: scale or chord
    /// names offered inside a string, where functions are the wrong answer.
    /// While set, `results` indexes into its names.
    pub vocabulary: Option<Vocabulary>,
    /// The enclosing function's docs when Ctrl+D opened an argument picker.
    /// Backspace from its empty search returns to that entry.
    pub(crate) argument_reference: Option<usize>,
    /// Opened on a word: Enter puts the chosen name straight in its place.
    quick: bool,
    /// What Enter does to a chosen sound: put it in the score (the panel
    /// was opened to complete a word) or on the clipboard (it was opened
    /// to look things up).
    pub intent: PanelIntent,
    /// The kinds of sound whose banks are listed. Everything starts
    /// folded: nine rows to read rather than sixteen hundred.
    pub open_categories: std::collections::BTreeSet<SoundSection>,
    /// The chords tab: what is typed, where the cursor is, and which
    /// quality is open onto its roots.
    pub chord_query: String,
    pub chord_selected: usize,
    pub open_quality: Option<String>,
    /// The scales tab, the same shape as the chords one.
    pub scale_query: String,
    pub scale_selected: usize,
    pub open_scale: Option<String>,
    /// Where each tab's list is scrolled to. Cells, because the scroll is
    /// settled in the geometry pass (the one place the height is known) and
    /// geometry takes `&self`. The scroll keeps a margin of rows around the
    /// selection (see [`super::scroll`]). A click on a row must not move
    /// the list, so a click holds that margin back.
    scroll: std::cell::Cell<usize>,
    sound_scroll: std::cell::Cell<usize>,
    chord_scroll: std::cell::Cell<usize>,
    scale_scroll: std::cell::Cell<usize>,
    #[cfg(feature = "hydra")]
    snippet_scroll: std::cell::Cell<usize>,
    /// The selection was last put somewhere by the pointer. Until a key
    /// moves it, changes the tab or opens or folds a row, or the wheel moves
    /// it, the list scrolls only to keep it inside the window and never to
    /// keep the margin: a clicked row stays under the pointer that clicked it.
    pub hold_scroll: bool,
    /// Alt+T on the samples tab asked once to cut a take's silence: the
    /// row it asked about, by [`Self::sound_selected`] at the time. A
    /// second Alt+T on the same row is the one that cuts; anything else -
    /// moving, searching, Esc, changing tabs - lets it go unanswered rather
    /// than leave a cut armed over a row nobody is looking at any more.
    pub confirm_trim: Option<usize>,
    /// A deletion awaits Enter for this bank, variant and exact local file.
    /// Keep the path so a catalogue refresh cannot change the target.
    pub confirm_delete: Option<(String, usize, std::path::PathBuf)>,
}

/// Why the panel was opened, which decides what Enter does to a sound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanelIntent {
    /// Ctrl+Space inside `s("…")`: Enter puts the name where the caret is.
    Insert,
    /// Ctrl+D, or the browser: Enter copies the name and says so.
    Copy,
}

/// What a key did to the panel, for the app to act on.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelAction {
    Nothing,
    /// Play a chord: every note at once, on the browser's own voice.
    PreviewChord(String),
    /// Play a scale: its notes one after another, fast enough to hear the
    /// shape of it.
    PreviewScale(String),
    /// A tuning, played as a rising run: its name is not readable, so the
    /// row is chosen by ear.
    PreviewTuning(String),
    Close,
    /// Insert this choice. Functions retain their parameter reference.
    Insert(String),
    /// Paste these lines at the caret, as a paste would, and close: a
    /// snippet lands whole, not wrapped in quotes or a call.
    Paste(String),
    /// Play this sound once, outside the score.
    Preview(String),
    /// Show the file behind a sound in the browser: `name`, and the numbered
    /// sample when the row is one. `url` is the address the bank was listed
    /// with, its first file - what to show when the library cannot say
    /// more.
    Reveal {
        name: String,
        variant: Option<usize>,
        url: String,
    },
    /// Put this sound's name on the clipboard.
    Copy(String),
    /// Alt+R on a bank: alias it under a new name,
    /// the same rename Settings ▸ Samples offers a user import, reached
    /// without leaving the browser to find the row again there. `origin`
    /// says whether this bank is the player's to alias at all.
    RenameBank {
        name: String,
        origin: SoundOrigin,
    },
    /// Rename the selected local file, retaining its bank/index address.
    RenameSample {
        name: String,
        variant: usize,
    },
    /// Play this score under the set: a snippet heard where it would go,
    /// at the tempo it would play at.
    #[cfg(feature = "hydra")]
    PreviewScore(String),
    #[cfg(feature = "hydra")]
    GeneratorChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PanelGeometry {
    pub list: Rect,
    pub first_row: usize,
}

/// Where the Snippets tab puts each of its pieces.
///
/// Drawing and hit-testing both read this, so a click lands on the row the
/// reader sees rather than on the row a differently-shaped tab would have had
/// there.
#[cfg(feature = "hydra")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnippetLayout {
    pub preview: Rect,
    pub scope: u16,
    pub list: Rect,
    pub code: Rect,
    pub footer: u16,
}

/// The reference column.
pub struct ReferenceView<'a> {
    pub keybinds: &'a super::keybinds::Keybinds,
    pub reference: &'a Reference,
    pub panel: &'a ReferencePanel,
    pub theme: &'a Theme,
    /// Whether the column holds the keyboard. Unfocused it is a thing being
    /// read past: the search caret goes out and the footer says the way
    /// back, because two lit carets is one too many.
    pub focused: bool,
    /// The samples tab's pulse row: what the mix is doing, right where the
    /// sounds are auditioned. `None` outside the studio's frame loop.
    pub pulse: Option<SamplesPulse<'a>>,
    /// Whether a preview is sounding now. A voicing lit whole: the
    /// piano recolours every member at once, the way chords sound.
    pub sounding_note: Option<usize>,
    /// Whether the studio has a frame for the background preview.
    /// The code panel shows a loading label until one arrives.
    pub picture: bool,
    /// Why the snippet under the cursor cannot be drawn, when it cannot.
    /// Without this a refused snippet is indistinguishable from a slow one:
    /// the loading label would wait on a frame that is never coming.
    pub refused: Option<String>,
    /// A previewed sound whose sample is still loading, by name - its row
    /// says so, because a silent preview with no word reads as broken.
    pub loading: Option<String>,
    /// Files the loader still has in its line. Shown as a countdown rather
    /// than a fraction: a pre-cache learns how many files it is as it goes,
    /// and a bar that jumps backwards while it learns tells you less than a
    /// number that only falls.
    pub caching: usize,
    /// Imported folders and packs still being read. A folder is walked off
    /// the studio's thread, so a big library brings nothing for a while and
    /// the browser would otherwise sit empty with no word about why.
    pub importing: usize,
    /// The library still has manifests on the way - the pinned defaults at
    /// startup, a pack being fetched. With no engine at all this is false
    /// and an empty catalogue is simply empty.
    pub library_loading: bool,
    /// The snippet playing under the score and the marks it is sounding,
    /// as ranges into that snippet's own text.
    #[cfg(feature = "hydra")]
    pub playing: Option<(&'a str, &'a [super::visuals::SourceMark])>,
    /// The row a preview was taken from, where it has got to, and whether
    /// it is sounding yet - absent once it is sounding, which the strip
    /// says instead of a word.
    #[cfg(feature = "hydra")]
    pub preview_note: Option<(Option<SnippetLine>, String, bool)>,
    /// The row the sounding preview was taken from, with or without a
    /// word for it: the playhead strip rides this row on its own.
    #[cfg(feature = "hydra")]
    pub preview_row: Option<Option<SnippetLine>>,
    /// How far through its bar the previewed snippet's clock stands, as a
    /// fraction 0..1: a strip of cells on the row, riding the same clock
    /// the score rides. "Playing" beside silence could be anything; a
    /// strip already halfway across says where the cycle is, and the ear
    /// can go looking for what it missed.
    #[cfg(feature = "hydra")]
    pub preview_progress: Option<f32>,
}

/// A live drag selection in one of the column's read-only text blocks.
///
/// The tag records what was on screen when the drag was made. Paint and copy
/// verify it still describes the screen and treat a mismatch as no selection -
/// so changing entry, changing tab, generating a new snippet or resizing
/// the column all drop it correctly with no clear-call checklist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneSelection {
    pub target: SelectionTarget,
    pub selection: super::textblock::TextSelection,
}

/// What a selection was made in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionTarget {
    /// The open entry's body, wrapped at this width - a resize reshapes the
    /// words, so the width is part of the identity.
    Entry { index: usize, width: u16 },
    /// One example code block. `width` is part of the identity like the
    /// entry's: a resize re-wraps the rows a drag is held in, and a
    /// selection that silently followed them would point at other text.
    #[cfg(feature = "hydra")]
    Snippet { row: usize, width: u16 },
}

/// One line of an entry's body, in draw order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyLine {
    pub text: String,
    pub kind: BodyKind,
    /// Character ranges of inline `code` in `text`. The backticks are not
    /// in the string; these spans are italic, not quoted.
    pub code: Vec<(usize, usize)>,
}

/// What a body line is, which is also how it is styled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyKind {
    Signature,
    Synonyms,
    Deprecated,
    Origin,
    Blank,
    Prose,
    Param,
    ParamDetail,
    /// What this engine does with the entry, or with one of its options.
    Terminal,
    Example,
}

/// What the samples tab's pulse row draws: the preview's own waveform with
/// its level as the ground, and the preview volume at its end - three
/// instruments in one line, because vertical space in a browser is rows of
/// sounds. The audio here is the audition tap's, never the mix's: the
/// browser shows what the browser plays.
#[derive(Clone, Copy)]
pub struct SamplesPulse<'a> {
    /// The preview tap's current peak, dBFS: the meter's level.
    pub peak_db: f32,
    /// The preview's own volume, 1.0 being unity.
    pub preview_gain: f32,
    /// The sounding preview's shape, as peaks 0 through 255, and how far
    /// into it the sound has got, 0 through 1. `None` with nothing being
    /// previewed, or before a first-time sound has finished decoding.
    pub shape: Option<(&'a [u8], f32)>,
}

/// One row of a wrapped chain, and where in the line it came from.
///
/// The rows are what is drawn; the span is what lets a highlight or a
/// selection land on the right characters once the line has been broken.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WrappedRow {
    /// The row as drawn, continuation indent included.
    pub text: String,
    /// The bytes of the source line this row shows, after the leading
    /// space a break leaves behind.
    pub from: usize,
    pub to: usize,
    /// Columns of indent before the first of those bytes.
    pub indent: usize,
}

/// The names this engine plays: the core registry, the control table, the
/// projected globals and the inline painters.
pub fn engine_knows() -> impl Fn(&str) -> bool {
    let known = rustel_runtime::lint::known_names();
    |name| known.contains(name)
}

#[cfg(test)]
mod tests {
    mod catalogue {
        use super::super::*;

        /// Plain and inline painters share their renderer's documentation.
        /// A reader asking about a visualization is told what THIS engine does
        /// with each option: the entry's own paragraph, a mark on every option
        /// that behaves differently here, and the options only we have.
        #[test]
        fn a_painters_entry_says_what_the_terminal_does_with_its_options() {
            use crate::visuals::{painter_terminal_entry, painter_terminal_option};

            let reference = Reference::load_all();
            for painter in rustel_transpiler::VISUAL_WIDGET_METHODS {
                let index = reference.lookup(painter).expect("entry");
                let entry = reference.entry(index).expect("entry");
                assert!(
                    !painter_terminal_entry(&entry.name).is_empty(),
                    "{painter} does not say what the terminal does with it"
                );
            }

            let spectrum = reference
                .entry(reference.lookup("spectrum").expect("spectrum"))
                .expect("entry");
            let param = |name: &str| {
                spectrum
                    .params
                    .iter()
                    .find(|param| param.name == name)
                    .unwrap_or_else(|| panic!("{name} is listed"))
            };
            let note = |name: &str| painter_terminal_option("spectrum", name);
            assert!(
                note(param("thickness").name.as_str()).starts_with("ignored"),
                "an option the terminal cannot honour says so"
            );
            assert!(
                note(param("min").name.as_str()).is_empty(),
                "an honoured option is unmarked"
            );
            // Ours, which upstream does not document.
            assert_eq!(note(param("scroll").name.as_str()), "only here");
            assert!(!param("color").description.is_empty());

            // The page shows all of it.
            let body = entry_body(spectrum, 60);
            let notes = body
                .iter()
                .filter(|line| line.kind == BodyKind::Terminal)
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(notes.contains("analyser"), "{notes}");
            assert!(notes.contains("· ignored"), "{notes}");
            assert!(notes.contains("· only here"), "{notes}");
        }

        #[test]
        fn an_inline_painter_has_its_own_callable_spelling_and_shared_options() {
            let reference = Reference::load(engine_knows());
            let plain = reference.lookup("pianoroll").expect("pianoroll");
            let inline = reference.lookup("_pianoroll").expect("_pianoroll");
            assert_ne!(inline, plain);
            let entry = reference.entry(inline).unwrap();
            assert_eq!(entry.name, "_pianoroll");
            assert_eq!(entry.params, reference.entry(plain).unwrap().params);
            assert!(entry.description.contains("inline below this call"));
            assert!(
                entry
                    .examples
                    .iter()
                    .any(|example| example.contains("._pianoroll("))
            );
            assert_eq!(reference.lookup("_"), None);
            assert_eq!(
                reference.lookup("_gain"),
                None,
                "do not invent inline forms of ordinary functions"
            );
            assert!(reference.documents("_pianoroll"));
            assert_eq!(reference.lookup("_tscope"), reference.lookup("_scope"));
        }

        /// Every painter the layout recognises is documented, under its own
        /// name or as a synonym - and `punchcard` has a page of its own rather
        /// than `pianoroll`'s, because it is fed differently.
        #[test]
        fn every_painter_the_layout_knows_has_a_reference_entry() {
            let reference = Reference::load(engine_knows());
            for method in rustel_transpiler::VISUAL_WIDGET_METHODS {
                if *method == "markcss" {
                    continue;
                }
                let index = reference
                    .lookup(method)
                    .unwrap_or_else(|| panic!("{method} has no reference entry"));
                let entry = reference.entry(index).expect("entry");
                assert!(
                    entry.name == *method || entry.synonyms.iter().any(|synonym| synonym == method),
                    "{method} resolves to {}",
                    entry.name
                );
            }
            let punchcard = reference.lookup("punchcard").expect("punchcard");
            assert_eq!(reference.entry(punchcard).expect("entry").name, "punchcard");
            let scope = reference.lookup("tscope").expect("tscope");
            assert_eq!(reference.entry(scope).expect("entry").name, "scope");
        }

        /// ECMAScript built-ins, and the few web globals QuickJS adds
        /// (`performance`, `queueMicrotask`, `DOMException`, `atob`, `btoa`), that
        /// a score can reach but are not part of the musical surface. They need
        /// an explicit allowlist, not entries: nobody looks up `Math.floor` or
        /// `Array.prototype` in a music reference, and writing a "summary" for
        /// them would be filler. The list exists so the coverage census can tell
        /// a reachable-but-undocumented built-in apart from an engine name that
        /// lost its documentation.
        const JS_BUILTINS: &[&str] = &[
            "Array",
            "ArrayBuffer",
            "Boolean",
            "Date",
            "Error",
            "Function",
            "JSON",
            "Map",
            "Math",
            "Number",
            "Object",
            "Promise",
            "Proxy",
            "RegExp",
            "Reflect",
            "Set",
            "String",
            "Symbol",
            "TypeError",
            "AggregateError",
            "AsyncDisposableStack",
            "Atomics",
            "BigInt",
            "BigInt64Array",
            "BigUint64Array",
            "DOMException",
            "DataView",
            "DisposableStack",
            "EvalError",
            "FinalizationRegistry",
            "Float16Array",
            "Float32Array",
            "Float64Array",
            "Int8Array",
            "Int16Array",
            "Int32Array",
            "InternalError",
            "Iterator",
            "RangeError",
            "ReferenceError",
            "SharedArrayBuffer",
            "SuppressedError",
            "SyntaxError",
            "URIError",
            "Uint8Array",
            "Uint8ClampedArray",
            "Uint16Array",
            "Uint32Array",
            "WeakMap",
            "WeakRef",
            "WeakSet",
            "console",
            "globalThis",
            "Infinity",
            "isFinite",
            "isNaN",
            "NaN",
            "parseFloat",
            "parseInt",
            "undefined",
            "atob",
            "btoa",
            "decodeURI",
            "decodeURIComponent",
            "encodeURI",
            "encodeURIComponent",
            "escape",
            "unescape",
            "eval",
            "performance",
            "queueMicrotask",
        ];

        /// Installed names without reference entries. The test checks that each
        /// name remains installed and undocumented; remove it when either changes.
        const ENGINE_NAMES_WITHOUT_ENTRIES: &[&str] = &[
            // Value types exposed to scores.
            "Fraction",
            "Hap",
            "Pattern",
            "State",
            "TimeSpan",
            // Scope, logging, key mapping and parser setup.
            "rustelScope",
            "strudelScope",
            "window",
            "logger",
            "userDefinedKeys",
            "packageName",
            "keyAlias",
            "clearScope",
            "setStringParser",
            // Parsing, value conversion and collection helpers.
            "sol2note",
            "tokenizeNote",
            "parseNumeral",
            "parseFractional",
            "valueToMidi",
            "isNote",
            "isNoteWithOctave",
            "getAccidentalsOffset",
            "getPlayableNoteValue",
            "getFreq",
            "getFrequency",
            "getSoundIndex",
            "getEventOffsetMs",
            "getControlName",
            "mapArgs",
            "fractionalArgs",
            "numeralArgs",
            "objectMap",
            "averageArray",
            "listRange",
            "stringifyValues",
            "nanFallback",
            "pairs",
            "splitAt",
            // The voicing registry underneath voicings()/addVoicings().
            "registerVoicings",
            "setDefaultVoicings",
            "resetVoicings",
            "setVoicingRange",
            "voicingAlias",
            "voicingRegistry",
            // The REPL's control defaults, set, read and reset.
            "setDefault",
            "setDefaultValue",
            "setDefaultValues",
            "setVersionDefaults",
            "getDefaultValue",
            "resetDefaults",
            "resetDefaultValues",
            // Score functions with missing reference entries.
            "and",
            "or",
            "eq",
            "ne",
            "gt",
            "gte",
            "lt",
            "lte",
            "mod",
            "pow",
            "band",
            "bor",
            "bxor",
            "blshift",
            "brshift",
            "eqt",
            "net",
            "keepif",
            "squeezeout",
            "mix",
            "poly",
            "out",
            "chooseIn",
            "chooseOut",
            "uniq",
            "uniqsort",
            "uniqsortr",
            "rotate",
            "flatten",
            "collect",
            "zipWith",
            "clamp",
            "constant",
            "id",
            "pipe",
            "compose",
            "curry",
            "cycleToSeconds",
            "freqToMidi",
            "midiToFreq",
            "midi2note",
            "noteToMidi",
            "modulate",
            "randrun",
            "timeline",
            "piano",
            "q",
            // `.p`, which names a lane. With extensions, Switch Angel's global `p`
            // has a page under the same name.
            #[cfg(not(feature = "extensions"))]
            "p",
            "setSteps",
            "withSteps",
            "hasSteps",
            "unjoin",
            "mini",
            "growlist",
            "shrinklist",
            "reify",
            "drawLine",
            // Every pattern's own fields: its query function and its poly join.
            "query",
            "polyJoin",
        ];

        /// Every installed musical name needs a reference entry. The per-table
        /// rows count through [`Reference::lookup`], which forgives case; the
        /// engine's names, chord symbols, scales and sounds must match an entry's
        /// name or synonym exactly.
        #[test]
        fn reference_census_every_installed_name_has_an_entry() {
            let reference = Reference::load_all();
            let has_entry = |name: &str| reference.lookup(name).is_some();

            // Each installed surface and the names it puts in a score's reach.
            let mut surfaces: Vec<(&'static str, Vec<String>, bool)> = Vec::new();

            // Hidden names stay registered but are exempt from the reference census.
            let registry = rustel_core::register::default_registry();
            let hidden_combinators: std::collections::BTreeSet<&str> =
                rustel_core::register::COMBINATORS_REFERENCE_HIDDEN
                    .iter()
                    .copied()
                    .collect();
            surfaces.push((
                "registry combinators",
                registry
                    .names()
                    .into_iter()
                    .filter(|name| !hidden_combinators.contains(*name))
                    .map(|name| name.to_owned())
                    .collect(),
                false,
            ));

            // Include positional control names and aliases unless their row is hidden.
            let hidden_spellings: std::collections::BTreeSet<String> =
                rustel_core::controls_generated::CONTROLS
                    .iter()
                    .filter(|row| {
                        !rustel_core::controls_generated::reference_shows(row.reference.name)
                    })
                    .flat_map(|row| row.names.iter().chain(row.aliases.iter()))
                    .map(|name| name.to_string())
                    .collect();
            let mut controls: Vec<String> = Vec::new();
            for row in rustel_core::controls_generated::CONTROLS {
                for name in row.names.iter().chain(row.aliases.iter()) {
                    if !hidden_spellings.contains(*name) {
                        controls.push((*name).to_string());
                    }
                }
            }
            surfaces.push(("controls", controls, false));

            // The visual widgets - page-level and inline painter spellings.
            surfaces.push((
                "visual widgets",
                rustel_transpiler::VISUAL_WIDGET_METHODS
                    .iter()
                    .map(|name| name.to_string())
                    .collect(),
                false,
            ));

            // The credited extensions - every installed name they own.
            #[cfg(feature = "extensions")]
            {
                let mut extension_names: Vec<String> = Vec::new();
                let combined = rustel_ext::default_registry();
                for name in combined.names() {
                    if combined
                        .get(name)
                        .is_some_and(|registration| registration.declared_in.is_extension())
                    {
                        extension_names.push(name.to_string());
                    }
                }
                for callable in rustel_ext::pattern_callables() {
                    extension_names.extend(callable.names.iter().map(|name| name.to_string()));
                }
                for callable in rustel_ext::value_callables() {
                    extension_names.extend(callable.names.iter().map(|name| name.to_string()));
                }
                surfaces.push(("credited extensions", extension_names, false));
            }

            // The chord vocabulary - every symbol any voicing dictionary knows.
            // Exact names: punctuation is part of a chord symbol's spelling,
            // and lookup() trims it away.
            surfaces.push(("chord symbols", chord_vocabulary(), true));

            // The scale table - every name and alias `scale("C:…")` accepts.
            let mut scales: Vec<String> = Vec::new();
            for (name, aliases, _intervals) in rustel_core::tonaljs_scales::SCALE_DICTIONARY {
                scales.push(name.to_string());
                scales.extend(aliases.iter().map(|alias| alias.to_string()));
            }
            surfaces.push(("scales", scales, true));

            // The native synth sounds - what `s("…")` makes without a sample bank.
            surfaces.push((
                "native synth sounds",
                rustel_voice::NATIVE_SYNTH_SOUNDS
                    .iter()
                    .map(|name| name.to_string())
                    .collect(),
                true,
            ));

            // Installed globals and pattern methods require exact-name coverage.
            // Raw `_` methods and `s_` stepwise aliases are covered by their public
            // names. Built-ins, undocumented names and hidden entries are exempt.
            let known = rustel_runtime::lint::known_names();
            surfaces.push((
                "engine names",
                known
                    .iter()
                    .map(String::as_str)
                    .filter(|&name| {
                        !name.starts_with('_')
                            && !name.starts_with("s_")
                            && !JS_BUILTINS.contains(&name)
                            && !ENGINE_NAMES_WITHOUT_ENTRIES.contains(&name)
                            && !hidden_combinators.contains(name)
                            && !hidden_spellings.contains(name)
                    })
                    .map(str::to_owned)
                    .collect(),
                true,
            ));

            // Count the gap per surface, printing as we go so the numbers land in
            // the repo even though the assertion below fails.
            let mut total_installed = 0usize;
            let mut total_missing = 0usize;
            let mut all_missing: Vec<String> = Vec::new();
            println!("reference census - installed names missing a reference entry");
            for (label, names, exact) in &surfaces {
                let mut seen = std::collections::BTreeSet::new();
                let mut missing: Vec<String> = Vec::new();
                for name in names {
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    let covered = if *exact {
                        reference.documents(name)
                    } else {
                        has_entry(name)
                    };
                    if !covered {
                        missing.push(name.clone());
                    }
                }
                total_installed += seen.len();
                total_missing += missing.len();
                println!(
                    "  {label:<22} {:>5} installed  {:>5} undocumented",
                    seen.len(),
                    missing.len()
                );
                all_missing.extend(missing);
            }
            println!(
                "  {:<22} {:>5} installed  {:>5} undocumented",
                "TOTAL", total_installed, total_missing
            );

            // The allowlist is honest: every built-in is reachable in a score, and
            // none of them is a surface name the census already walks.
            let walked: std::collections::BTreeSet<String> = surfaces
                .iter()
                .flat_map(|(_, names, _)| names.iter().cloned())
                .collect();
            for builtin in JS_BUILTINS {
                assert!(
                    !walked.contains(*builtin),
                    "{builtin} is allowlisted as a JS built-in but is also a Strudel surface name"
                );
                assert!(
                    known.contains(*builtin),
                    "{builtin} is allowlisted but the engine does not actually reach it"
                );
            }

            // The undocumented list is honest the same way: every name on it is
            // installed, and none of them has an entry.
            for name in ENGINE_NAMES_WITHOUT_ENTRIES {
                assert!(
                    known.contains(*name),
                    "{name} is allowlisted as undocumented but the engine does not install it"
                );
                assert!(
                    !reference.documents(name),
                    "{name} is allowlisted as undocumented but the reference has a page for it"
                );
            }

            // Print the missing names so the gap is actionable, not just a number.
            all_missing.sort();
            println!("undocumented names ({}):", all_missing.len());
            for name in &all_missing {
                println!("  {name}");
            }

            assert!(
                total_missing == 0,
                "{total_missing} of {total_installed} installed names have no reference entry; \
         run with `--ignored --nocapture` to see them"
            );
        }

        /// The census's other direction: every entry the reference assembles
        /// names something the engine actually installs. A documentation entry
        /// for a name nothing provides would be a promise the score cannot keep.
        #[test]
        fn every_entry_names_something_the_engine_installs() {
            let reference = Reference::load_all();
            let mut installed: std::collections::BTreeSet<String> =
                rustel_runtime::lint::known_names()
                    .iter()
                    .cloned()
                    .collect();
            for sound in rustel_voice::NATIVE_SYNTH_SOUNDS {
                installed.insert((*sound).to_string());
            }
            for alias in ["sin", "tri", "sqr", "saw", "user", "in", "bus"] {
                installed.insert(alias.to_string());
            }
            for chord in chord_vocabulary() {
                installed.insert(chord);
            }
            for (name, aliases, _) in rustel_core::tonaljs_scales::SCALE_DICTIONARY {
                installed.insert((*name).to_string());
                for alias in *aliases {
                    installed.insert((*alias).to_string());
                }
            }
            let mut stray: Vec<&str> = Vec::new();
            for index in 0..reference.len() {
                let entry = reference.entry(index).expect("entry");
                // A snippet is lines to paste, not a name the engine installs.
                if entry.snippet.is_some() {
                    continue;
                }
                if !installed.contains(&entry.name) {
                    stray.push(&entry.name);
                }
            }
            assert!(
                stray.is_empty(),
                "entries name nothing installed: {stray:?}"
            );
        }

        /// The panel's catalogues are complete: every chord symbol, every scale
        /// and every native synth the engine knows is on offer in its tab, so the
        /// reference and the lists a reader browses cannot drift apart.
        #[test]
        fn the_panel_catalogues_list_every_chord_scale_and_sound() {
            let reference = Reference::load_all();
            let panel = ReferencePanel::open(&reference, 0);
            let qualities = panel.chord_qualities();
            // The Chords tab lists the active voicing dictionary, not every
            // dictionary the engine ships, so completeness is measured against
            // the symbols a score can actually voice right now.
            for symbol in rustel_core::voicings::dictionary_symbols(None) {
                // The empty symbol voices "no chord"; the panel lists it nowhere.
                if symbol.is_empty() {
                    continue;
                }
                assert!(
                    qualities.iter().any(|quality| quality.symbol == symbol),
                    "chord {symbol} is missing from the panel"
                );
            }
            let scales = panel.scale_names();
            for name in scale_vocabulary() {
                assert!(
                    scales.iter().any(|scale| scale == &name),
                    "scale {name} is missing from the panel"
                );
            }
            for sound in rustel_voice::NATIVE_SYNTH_SOUNDS {
                assert!(
                    rustel_voice::is_native_synth_sound(sound),
                    "sound {sound} is missing from the panel"
                );
            }
        }

        /// The aliases shown are the other NAMES a thing answers to, not the
        /// same name in another case.
        #[test]
        fn an_entry_lists_only_aliases_that_are_a_different_name() {
            let reference = Reference::load_all();
            let body = |name: &str| {
                let index = *reference.by_name.get(name).expect("documented");
                let entry = reference.entry(index).expect("entry");
                entry_body(entry, 60)
                    .into_iter()
                    .filter(|line| line.kind == BodyKind::Synonyms)
                    .map(|line| line.text)
                    .collect::<Vec<_>>()
            };
            // `setcpm` is `setCpm` typed differently; upstream documents it
            // under the lowercase fold, so the entry carries both.
            assert!(
                body("setCpm").is_empty(),
                "the lowercase spelling was listed as an alias: {:?}",
                body("setCpm")
            );
            // Real aliases stay.
            assert_eq!(body("stack"), vec!["also polyrhythm, pr".to_owned()]);
            // And the lowercase spelling still finds the entry.
            assert_eq!(
                reference.by_name.get("setcpm"),
                reference.by_name.get("setCpm")
            );
        }

        #[test]
        fn every_finite_choice_is_documented_once_and_reaches_the_entry_body() {
            let reference = Reference::load_all();
            for set in rustel_core::reference::REFERENCE_CHOICE_SETS {
                assert!(!set.choices.is_empty(), "{} has no choices", set.entry);
                let index = reference
                    .resolve(set.entry)
                    .unwrap_or_else(|| panic!("{} is not in the reference", set.entry));
                let entry = reference.entry(index).expect("resolved entry");
                let param = entry
                    .params
                    .iter()
                    .find(|param| param.name == set.parameter)
                    .unwrap_or_else(|| panic!("{}.{} is not documented", set.entry, set.parameter));
                assert_eq!(
                    param.choices.len(),
                    set.choices.len(),
                    "{}.{}",
                    set.entry,
                    set.parameter
                );
                for (documented, source) in param.choices.iter().zip(set.choices) {
                    assert_eq!(
                        documented.value, source.value,
                        "{}.{}",
                        set.entry, set.parameter
                    );
                    assert_eq!(
                        documented.description, source.description,
                        "{}.{}:{}",
                        set.entry, set.parameter, source.value
                    );
                }

                let mut values = std::collections::HashSet::new();
                for choice in set.choices {
                    assert!(
                        !choice.value.trim().is_empty(),
                        "{}.{} has a blank value",
                        set.entry,
                        set.parameter
                    );
                    assert!(
                        !choice.description.trim().is_empty(),
                        "{}.{}:{} has no explanation",
                        set.entry,
                        set.parameter,
                        choice.value
                    );
                    assert!(
                        values.insert(choice.value),
                        "{}.{} repeats {}",
                        set.entry,
                        set.parameter,
                        choice.value
                    );
                }

                let body = entry_body(entry, 240)
                    .into_iter()
                    .map(|line| line.text)
                    .collect::<Vec<_>>()
                    .join("\n");
                for choice in set.choices {
                    assert!(
                        body.contains(choice.value),
                        "{}.{} omits {}",
                        set.entry,
                        set.parameter,
                        choice.value
                    );
                    assert!(
                        body.contains(&format!("  {} -", choice.value)),
                        "{}.{} omits the explained choice {}",
                        set.entry,
                        set.parameter,
                        choice.value
                    );
                }
            }
        }

        /// A heading is always a word the entries under it actually carry. The
        /// tags come from upstream, so this is the test that notices a
        /// regenerated table quietly inventing one.
        #[test]
        fn every_entry_is_filed_under_a_tag_it_actually_carries() {
            let reference = Reference::load_all();
            for index in 0..reference.len() {
                let entry = reference.entry(index).expect("every entry");
                if entry.tags.is_empty() {
                    assert_eq!(entry.heading(), "other", "{}", entry.name);
                } else {
                    assert!(
                        entry.tags.iter().any(|tag| tag == entry.heading()),
                        "{} is filed under {}, which it does not carry: {:?}",
                        entry.name,
                        entry.heading(),
                        entry.tags
                    );
                }
            }
        }

        /// `limit` is credited to rustel, because strudel.cc has no such
        /// control. The panel clears a control's `origin`, so the extension
        /// registry must list `limit` for the credit to stay.
        #[cfg(feature = "extensions")]
        #[test]
        fn limit_is_credited_to_this_port_and_slider_is_not() {
            let reference = Reference::load(engine_knows());
            let entry = |name: &str| {
                reference
                    .entry(*reference.by_name.get(name).expect("documented"))
                    .expect("entry")
            };
            assert_eq!(
                entry("limit").origin,
                "rustel",
                "limit is ours; strudel.cc has no such control"
            );
            assert!(
                reference.origins().iter().any(|origin| origin == "Rustel"),
                "and it shows up in the origins a reader can filter by: {:?}",
                reference.origins()
            );
            // The counter-example, so this is not asserting that everything is
            // an extension: `slider` is upstream's and stays uncredited.
            assert!(entry("slider").origin.is_empty());
            // And the text is the control table's own, not a second copy that
            // could drift from the row that installs it.
            assert_eq!(
                entry("limit").summary,
                rustel_core::controls_generated::LIMIT_REFERENCE.summary
            );
        }

        #[test]
        fn slider_is_in_the_reference_with_its_argument_order() {
            // The case that put it here: `slider(0, 0.1, 0.1, 1)`, written as
            // though the order were min-first, then searched for and not found.
            let reference = Reference::load(engine_knows());
            let entry = reference
                .entry(
                    *reference
                        .by_name
                        .get("slider")
                        .expect("slider is documented"),
                )
                .expect("entry");
            assert!(
                entry.origin.is_empty(),
                "slider is upstream's, not an extension of ours"
            );
            assert_eq!(entry.signature(), "slider(value, min, max, step)");
            assert!(!entry.examples.is_empty(), "an example to copy");
            assert!(engine_knows()("slider"));
        }

        #[cfg(feature = "extensions")]
        #[test]
        fn extension_metadata_projects_into_the_reference_without_a_studio_catalogue() {
            let reference = Reference::load(engine_knows());
            for documented in rustel_ext::reference_entries() {
                let index = reference
                    .by_name
                    .get(documented.name)
                    .unwrap_or_else(|| panic!("extension `{}` was not projected", documented.name));
                let projected = reference.entry(*index).expect("entry");
                assert_eq!(
                    projected.origin, documented.origin,
                    "`{}` was projected under the wrong origin",
                    documented.name
                );
            }

            // Only an extension has an origin a reader should see. The other
            // tables' `origin` says who wrote the documentation, which is not
            // a claim about the name.
            let mut expected_origins = rustel_ext::reference_entries()
                .map(|entry| display_origin(entry.origin))
                .collect::<Vec<_>>();
            expected_origins.sort();
            expected_origins.dedup();
            assert_eq!(reference.origins(), expected_origins);
            assert!(reference.from_origin("").is_empty());
        }

        #[test]
        fn the_engine_filter_keeps_what_the_engine_plays_and_drops_the_rest() {
            let reference = Reference::load(engine_knows());
            assert!(reference.len() > 100, "{} entries", reference.len());
            assert!(reference.lookup("fast").is_some());
            assert!(reference.lookup("lpf").is_some());
            assert!(reference.lookup("s").is_some());
            assert!(reference.omitted() > 0, "documented but not here yet");
            assert!(reference.len() + reference.omitted() == Reference::load_all().len());
        }

        /// Typing `i` or `in` inside `s("…")` offers the audio input first:
        /// it is a sound `s()` plays, so the browser lists it with the synths,
        /// labelled as the input, and its channels as `in:n` once a device
        /// has said how many there are.
        #[test]
        fn the_audio_input_is_offered_as_a_sound() {
            let reference = Reference::load_all();
            let library = rustel_runtime::samples::SampleLibrary::empty();
            let mut sounds = library.catalogue();
            sounds.push(SoundEntry {
                name: "insect".into(),
                variants: 3,
                variant_names: Vec::new(),
                origin: SoundOrigin::Default,
                category: SoundCategory::Other,
                location: None,
                import: None,
            });
            let input = sounds
                .iter_mut()
                .find(|sound| sound.name == "in")
                .expect("in");
            input.variants = 2;
            let mut panel = ReferencePanel::samples(&reference, sounds);
            for query in ["i", "in"] {
                panel.sound_query = query.into();
                panel.refresh_sounds();
                let first = panel
                    .sound_results
                    .first()
                    .map(|&index| panel.sounds[index].name.as_str());
                assert_eq!(first, Some("in"), "{query:?} offers the input first");
            }
            // Opened, the input lists a row a channel.
            panel.sound_selected = 0;
            assert!(panel.expand(), "the input opens onto its channels");
            let rows = panel.sound_rows();
            let variants = rows
                .iter()
                .filter(
                    |row| matches!(row, SoundRow::Variant(index, _) if panel.sounds[*index].name == "in"),
                )
                .count();
            assert_eq!(variants, 2, "one row a channel: in:0 and in:1 - {rows:?}");
            assert!(
                reference.lookup("in").is_some(),
                "and ^D on it opens a page saying what it is"
            );
        }

        /// Every reference example evaluates and passes the linter with a real
        /// sample library, the two checks a score passes before it plays. The
        /// known exceptions are listed by name, and the list must not grow.
        /// A new entry with a broken example fails here.
        #[test]
        fn every_reference_example_evaluates_and_checks_out() {
            /// Entries whose examples the engine cannot yet play, with why.
            /// Each is a gap to close, not a licence: the example is right and
            /// we are missing what it needs.
            const KNOWN_GAPS: &[(&str, &str)] = &[(
                "bmod",
                "`s(\"one\")` is a constant-1 source - a DC signal whose \
             only purpose is to be modulated, which is why the example puts a \
             slider on its gain. We have no such source, so the modulator voice \
             is refused and bmod's only example demonstrates nothing.",
            )];

            let reference = Reference::load_all();
            let manifest_cache = crate::config::sample_cache_before_test_isolation();
            let library =
                rustel_runtime::samples::SampleLibrary::load_default_with_manifest_cache_for_tests(
                    manifest_cache.clone(),
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "load pinned sample banks from {}: {error}",
                        manifest_cache.display()
                    )
                });
            let mut checked = 0usize;
            let mut snippets = 0usize;
            let mut broken: Vec<String> = Vec::new();
            for index in 0..reference.len() {
                let Some(entry) = reference.entry(index) else {
                    continue;
                };
                if KNOWN_GAPS.iter().any(|(name, _)| *name == entry.name) {
                    continue;
                }
                // A snippet is pasted into a score, so it must pass the same
                // two checks as an example.
                snippets += usize::from(entry.snippet.is_some());
                for example in entry.examples.iter().chain(entry.snippet.as_ref()) {
                    // Without the visuals window `initHydra` is a stub. The
                    // names the real one installs (`osc`, `shape`, `o0`, `s0`)
                    // do not exist, so the example cannot evaluate.
                    // `clearHydra()` is not skipped: its stub is real.
                    #[cfg(not(feature = "hydra"))]
                    if example.contains("initHydra") {
                        continue;
                    }
                    checked += 1;
                    let mut session = rustel_runtime::Session::new().expect("a session");
                    if let Err(error) = session.evaluate(example) {
                        broken.push(format!(
                            "{}: {} - in {example:?}",
                            entry.name,
                            format!("{error}").replace('\n', " ")
                        ));
                        continue;
                    }
                    if let Some(first) = rustel_runtime::lint::lint(example, false, Some(&library))
                        .into_iter()
                        .find(|found| found.level != rustel_runtime::lint::Level::Note)
                    {
                        broken.push(format!(
                            "{}: {:?} {} - in {example:?}",
                            entry.name, first.level, first.message
                        ));
                    }
                }
            }
            // The reference should have examples even without extensions or Hydra.
            // Core tests check exact entry counts beside reference_shows.
            assert!(
                checked > 600,
                "the reference should have hundreds of examples, found {checked}"
            );
            // Not vacuous: the snippets really were among them. A chain that
            // silently yielded nothing would leave this passing while the
            // things people press Enter on went unchecked.
            assert_eq!(
                snippets,
                super::super::super::snippets::SNIPPETS.len(),
                "every snippet is checked"
            );
            assert!(
                broken.is_empty(),
                "{} of {checked} reference examples do not work:\n{}",
                broken.len(),
                broken.join("\n")
            );
        }
    }
    mod entry_text {
        use super::super::*;

        /// A list row is shortened visibly, because it cannot wrap.
        ///
        /// Rows have to stay one line each or the list stops being navigable, so
        /// a long summary must lose its tail either way. The question is whether
        /// the reader can tell: clipped at the edge, "…to create a" reads as a
        /// finished thought.
        #[test]
        fn a_summary_too_long_for_its_row_says_so() {
            let summary = "Modulate the amplitude of an orbit to create a sidechain effect";
            let short = super::super::elide(summary, 30);
            assert!(
                UnicodeWidthStr::width(short.as_str()) <= 30,
                "{short:?} does not fit"
            );
            assert!(short.ends_with('\u{2026}'), "no sign it was cut: {short:?}");
            assert!(
                !short.trim_end_matches('\u{2026}').ends_with(' '),
                "a space before the mark: {short:?}"
            );
            // Cut at a word boundary rather than mid-word.
            assert!(
                summary.starts_with(short.trim_end_matches('\u{2026}')),
                "{short:?} is not a prefix of the summary"
            );

            // Something that fits is returned untouched, mark and all absent.
            assert_eq!(super::super::elide(summary, 200), summary);
            // And a room of nothing does not panic.
            assert_eq!(super::super::elide(summary, 0), summary);
        }

        /// An example is wrapped at its chain seams, never clipped.
        ///
        /// Clipping was the old behaviour and the worst of the options: a reader
        /// cannot tell a short example from a truncated one, so the text was gone
        /// with no sign that it had been.
        #[test]
        fn a_long_example_wraps_at_its_chain_and_keeps_every_character() {
            let code =
                r#"$: n(run(16)).scale("c:minor:pentatonic").s("sawtooth").delay(.7).orbit(2)"#;
            let wrapped = super::super::wrap_code(code, 40);
            assert!(wrapped.len() > 1, "a long chain must wrap: {wrapped:?}");
            for line in &wrapped {
                assert!(
                    UnicodeWidthStr::width(line.as_str()) <= 40,
                    "{line:?} is wider than the panel"
                );
            }
            // Nothing is lost, and the continuation is indented so the whole
            // thing still reads as one statement.
            let rejoined: String = wrapped
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    if index == 0 {
                        line.clone()
                    } else {
                        line.trim_start().to_owned()
                    }
                })
                .collect();
            assert_eq!(rejoined, code, "wrapping lost or added characters");
            assert!(wrapped[1].starts_with("  ."), "{wrapped:?}");

            // A dot inside a string or a number is not a seam.
            let tricky = r#"$: s("bd.sd").gain(0.75).room(0.5)"#;
            for line in super::super::wrap_code(tricky, 16) {
                assert!(!line.trim_start().starts_with(".75"), "broke a number");
                assert!(
                    !line.trim_start().starts_with(".sd"),
                    "broke inside a string"
                );
            }

            // The case that shipped broken: every seam is inside the call, so
            // breaking only at the top level found none and handed back one long
            // line to be clipped. Width is a guarantee, not a preference.
            let nested =
                r#"stack( n(run(8)).scale("c:minor").s("sawtooth").delay(.7).orbit(2), s("hh*8"))"#;
            for column in [24usize, 40, 60] {
                let wrapped = super::super::wrap_code(nested, column);
                for line in &wrapped {
                    assert!(
                        UnicodeWidthStr::width(line.as_str()) <= column,
                        "at {column} columns {line:?} is still too wide"
                    );
                }
                // Compared without whitespace: a wrap may drop the space it broke
                // at, the way word wrap always has. Nothing else may go.
                let strip = |text: &str| -> String {
                    text.chars().filter(|c| !c.is_whitespace()).collect()
                };
                assert_eq!(
                    strip(&wrapped.join("")),
                    strip(nested),
                    "wrapping lost characters at {column}"
                );
            }

            // A line with no seam at all is broken rather than clipped.
            let unbroken = "x".repeat(120);
            for line in super::super::wrap_code(&unbroken, 30) {
                assert!(UnicodeWidthStr::width(line.as_str()) <= 30, "{line:?}");
            }

            // Something that already fits is returned untouched.
            assert_eq!(
                super::super::wrap_code("s(\"bd\")", 40),
                vec!["s(\"bd\")".to_owned()]
            );
        }

        #[test]
        fn the_entry_body_the_pane_draws_is_the_text_it_copies() {
            // The prior art for pane selection kept a second, differently shaped
            // line list and put its band two rows from the text it copied. One
            // producer for both is the fix, and this is the regression test: at
            // every scroll, each drawn row is the body line the producer says.
            let reference = Reference::load(engine_knows());
            let index = *reference.by_name.get("lpf").expect("lpf");
            let entry = reference.entry(index).expect("entry");
            let area = Rect::new(0, 0, 44, 14);
            let inner = inner_area(area);
            let body = entry_body_area(inner);
            let lines = entry_body(entry, usize::from(inner.width));
            assert!(
                lines.len() > usize::from(body.height),
                "long enough to scroll"
            );

            for scroll in [0_u16, 3] {
                let mut panel = ReferencePanel::open(&reference, index);
                panel.mode = ReferenceMode::Entry {
                    index,
                    scroll,
                    from_browse: false,
                };
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    #[cfg(feature = "hydra")]
                    playing: None,
                    #[cfg(feature = "hydra")]
                    preview_note: None,
                    #[cfg(feature = "hydra")]
                    preview_progress: None,
                    #[cfg(feature = "hydra")]
                    preview_row: None,
                    picture: false,
                    refused: None,
                    reference: &reference,
                    panel: &panel,
                    theme: &Theme::built_in_default(),
                }
                .render(area, &mut buffer);

                for row in 0..body.height {
                    let line = &lines[usize::from(scroll + row)];
                    let drawn: String = (0..body.width)
                        .map(|column| {
                            buffer
                                .cell((body.x + column, body.y + row))
                                .unwrap()
                                .symbol()
                                .to_owned()
                        })
                        .collect();
                    // A line wider than the column is truncated on screen and
                    // carried whole by the producer - the deliberate asymmetry
                    // that lets a copy take what the column cannot show.
                    if line.text.chars().count() <= usize::from(body.width) {
                        assert_eq!(
                            drawn.trim_end(),
                            line.text.trim_end(),
                            "scroll {scroll}, row {row}"
                        );
                    } else {
                        let shown: String =
                            line.text.chars().take(usize::from(body.width)).collect();
                        assert_eq!(
                            drawn.trim_end(),
                            shown.trim_end(),
                            "scroll {scroll}, row {row}"
                        );
                    }
                }
            }
        }

        #[test]
        fn a_drag_through_an_example_copies_exactly_those_characters() {
            use crate::textblock::{TextPoint, TextSelection};
            let reference = Reference::load(engine_knows());
            let index = *reference.by_name.get("jux").expect("jux");
            let entry = reference.entry(index).expect("entry");
            let area = Rect::new(0, 0, 60, 20);
            let inner = inner_area(area);
            let mut panel = ReferencePanel::open(&reference, index);

            // A drag through the middle of the first example line.
            let lines: Vec<String> = entry_body(entry, usize::from(inner.width))
                .into_iter()
                .map(|line| line.text)
                .collect();
            let example = lines
                .iter()
                .position(|line| line.contains(".jux(rev)"))
                .expect("an example line");
            panel.selection = Some(PaneSelection {
                target: SelectionTarget::Entry {
                    index,
                    width: inner.width,
                },
                selection: TextSelection {
                    anchor: TextPoint {
                        line: example,
                        column: 0,
                    },
                    head: TextPoint {
                        line: example,
                        column: 5,
                    },
                },
            });
            let copied = panel
                .live_selection_text(&reference, inner)
                .expect("a copyable selection");
            assert_eq!(copied, lines[example].chars().take(5).collect::<String>());

            // The band survives scrolling - it is anchored to the content.
            panel.mode = ReferenceMode::Entry {
                index,
                scroll: 2,
                from_browse: false,
            };
            assert_eq!(
                panel.live_selection_text(&reference, inner).as_deref(),
                Some(copied.as_str())
            );

            // A different entry, or a different width, is a different screen:
            // the selection no longer exists.
            let other = *reference.by_name.get("lpf").expect("lpf");
            panel.mode = ReferenceMode::Entry {
                index: other,
                scroll: 0,
                from_browse: false,
            };
            assert_eq!(panel.live_selection_text(&reference, inner), None);
            panel.mode = ReferenceMode::Entry {
                index,
                scroll: 0,
                from_browse: false,
            };
            let narrower = Rect::new(inner.x, inner.y, inner.width - 4, inner.height);
            assert_eq!(panel.live_selection_text(&reference, narrower), None);
        }

        #[test]
        fn the_band_is_painted_only_over_the_selected_cells() {
            use crate::textblock::{TextPoint, TextSelection};
            let reference = Reference::load(engine_knows());
            let index = *reference.by_name.get("jux").expect("jux");
            let entry = reference.entry(index).expect("entry");
            let area = Rect::new(0, 0, 60, 20);
            let inner = inner_area(area);
            let theme = Theme::built_in_default();
            let mut panel = ReferencePanel::open(&reference, index);
            let lines = entry_body(entry, usize::from(inner.width));
            let example = lines
                .iter()
                .position(|line| line.kind == BodyKind::Example)
                .expect("an example");
            panel.selection = Some(PaneSelection {
                target: SelectionTarget::Entry {
                    index,
                    width: inner.width,
                },
                selection: TextSelection {
                    anchor: TextPoint {
                        line: example,
                        column: 2,
                    },
                    head: TextPoint {
                        line: example,
                        column: 6,
                    },
                },
            });

            let mut plain = Buffer::empty(area);
            let mut banded = Buffer::empty(area);
            let view = |panel: &ReferencePanel, buffer: &mut Buffer| {
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    #[cfg(feature = "hydra")]
                    playing: None,
                    #[cfg(feature = "hydra")]
                    preview_note: None,
                    #[cfg(feature = "hydra")]
                    preview_progress: None,
                    #[cfg(feature = "hydra")]
                    preview_row: None,
                    picture: false,
                    refused: None,
                    reference: &reference,
                    panel,
                    theme: &theme,
                }
                .render(area, buffer);
            };
            let bare = ReferencePanel::open(&reference, index);
            view(&bare, &mut plain);
            view(&panel, &mut banded);

            let body = entry_body_area(inner);
            let y = body.y + example as u16;
            for column in 0..12u16 {
                let cell = banded.cell((body.x + column, y)).unwrap();
                let before = plain.cell((body.x + column, y)).unwrap();
                if (2..6).contains(&column) {
                    assert_eq!(cell.bg, theme.selection, "column {column} is banded");
                } else {
                    assert_eq!(cell.bg, before.bg, "column {column} is not");
                }
                // Background only: the syntax colouring keeps its say.
                assert_eq!(cell.fg, before.fg, "column {column} keeps its colour");
            }
        }

        #[test]
        fn code_colours_include_calls_and_keywords_and_survive_wrapping() {
            use ratatui::style::Color;

            let mut theme = Theme::built_in_default();
            theme.syntax.keyword = Some(Color::Yellow);
            theme.syntax.function = Some(Color::Magenta);
            let line = "const beat = s(\"bd hh oh\").slow(2) // a long comment";
            let area = Rect::new(0, 0, 60, 1);
            for (text, colour) in [
                ("const", Color::Yellow),
                ("s(", Color::Magenta),
                ("slow", Color::Magenta),
                ("hh oh", theme.syntax.string),
                ("long comment", theme.syntax.comment),
            ] {
                let mut buffer = Buffer::empty(area);
                let from = line.find(text).unwrap();
                render_code_span(&mut buffer, area, line, from..line.len(), &theme);
                let cell = buffer.cell((0, 0)).unwrap();
                assert_eq!(cell.symbol(), &text[..1]);
                assert_eq!(cell.fg, colour, "{text}");
            }
        }

        #[test]
        fn the_view_draws_both_modes_without_panicking() {
            let reference = Reference::load_all();
            let theme = Theme::built_in_default();
            for size in [(12, 4), (40, 20), (70, 40)] {
                let area = Rect::new(0, 0, size.0, size.1);
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    #[cfg(feature = "hydra")]
                    playing: None,
                    #[cfg(feature = "hydra")]
                    preview_note: None,
                    #[cfg(feature = "hydra")]
                    preview_progress: None,
                    #[cfg(feature = "hydra")]
                    preview_row: None,
                    picture: false,
                    refused: None,
                    reference: &reference,
                    panel: &ReferencePanel::browse(&reference),
                    theme: &theme,
                }
                .render(area, &mut buffer);
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    #[cfg(feature = "hydra")]
                    playing: None,
                    #[cfg(feature = "hydra")]
                    preview_note: None,
                    #[cfg(feature = "hydra")]
                    preview_progress: None,
                    #[cfg(feature = "hydra")]
                    preview_row: None,
                    picture: false,
                    refused: None,
                    reference: &reference,
                    panel: &ReferencePanel::open(&reference, reference.lookup("lpf").unwrap()),
                    theme: &theme,
                }
                .render(area, &mut buffer);
            }
            let area = Rect::new(0, 0, 60, 30);
            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                reference: &reference,
                panel: &ReferencePanel::open(&reference, reference.lookup("lpf").unwrap()),
                theme: &theme,
            }
            .render(area, &mut buffer);
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("lpf(frequency)"), "{text}");
            assert!(text.contains("also cutoff"), "{text}");
        }

        /// A colour list is a picker: every row carries a swatch and wears the
        /// colour it names.
        #[test]
        fn a_colour_list_draws_its_swatches() {
            let reference = Reference::load_all();
            let panel = ReferencePanel::color_vocabulary_for(&reference, "cy");
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 40, 12);
            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                panel: &panel,
                reference: &reference,
                theme: &theme,
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
            }
            .render(area, &mut buffer);
            let cyan = super::super::super::theme::parse_color("cyan").expect("cyan");
            let row = (0..area.height)
                .find(|y| {
                    (0..area.width).any(|x| {
                        buffer
                            .cell((x, *y))
                            .is_some_and(|cell| cell.symbol() == "█" && cell.fg == cyan)
                    })
                })
                .expect("a cyan swatch");
            let text: String = (0..area.width)
                .filter_map(|x| buffer.cell((x, row)))
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("cyan"), "{text:?}");
        }

        /// The reference tab says which name it is answering about, so the
        /// chord can tell "close this" from "ask about that instead".
        #[test]
        fn the_panel_says_which_name_it_is_showing() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            assert_eq!(panel.showing(&reference), None);
            panel.query = "pianoroll".into();
            panel.refresh(&reference);
            assert_eq!(panel.showing(&reference).as_deref(), Some("pianoroll"));
            let index = reference.lookup("spiral").expect("spiral");
            let opened = ReferencePanel::open(&reference, index);
            assert_eq!(opened.showing(&reference).as_deref(), Some("spiral"));
            // The samples tab answers about sounds, not names.
            let sounds = ReferencePanel::samples(&reference, Vec::new());
            assert_eq!(sounds.showing(&reference), None);
        }

        #[test]
        fn inline_code_ticks_are_stripped_and_the_span_is_marked() {
            let (visible, marks) = super::super::parse_inline("`pattern.log()` is a method");
            assert_eq!(visible, "pattern.log() is a method");
            assert!(!visible.contains('`'));
            let code: String = visible
                .chars()
                .zip(marks.iter().copied())
                .filter(|(_, mark)| *mark)
                .map(|(character, _)| character)
                .collect();
            assert_eq!(code, "pattern.log()");

            let lines = super::super::wrap_inline("Call `pattern.log()` on the pattern", 40);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(lines[0].0, "Call pattern.log() on the pattern");
            assert_eq!(
                &lines[0].0[lines[0].1[0].0..lines[0].1[0].1],
                "pattern.log()"
            );

            let unmatched = super::super::parse_inline("see `oops");
            assert_eq!(unmatched.0, "see `oops");
            assert!(unmatched.1.iter().all(|mark| !*mark));

            let reference = Reference::load(engine_knows());
            let log = reference
                .entry(*reference.by_name.get("log").expect("log"))
                .expect("entry");
            let prose: String = entry_body(log, 60)
                .iter()
                .filter(|line| line.kind == BodyKind::Prose)
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                !prose.contains('`'),
                "ticks leaked into the drawn body: {prose}"
            );
            assert!(
                prose.contains("pattern.log()"),
                "the code itself is missing: {prose}"
            );
            assert!(
                entry_body(log, 60).iter().any(|line| !line.code.is_empty()),
                "the code span was not marked"
            );

            let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 1));
            super::super::render_inline_line(
                &mut buffer,
                0,
                0,
                40,
                "Call pattern.log() now",
                &[(5, 18)],
                Style::default(),
            );
            assert!(
                buffer
                    .cell((5, 0))
                    .expect("code cell")
                    .modifier
                    .contains(Modifier::ITALIC),
                "inline code should lean"
            );
            assert!(
                !buffer
                    .cell((0, 0))
                    .expect("prose cell")
                    .modifier
                    .contains(Modifier::ITALIC),
                "the rest of the sentence stays upright"
            );
        }
    }
    mod music {
        use super::super::*;

        /// The scales tab: the ones music uses most first, each opening onto
        /// its twelve tonics, every row spelled out in notes and playable.
        #[test]
        fn the_scales_tab_lists_by_use_and_opens_each_onto_its_tonics() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Scales;
            let names = panel.scale_names();
            assert_eq!(
                names.iter().take(4).map(String::as_str).collect::<Vec<_>>(),
                ["major", "minor", "major:pentatonic", "minor:pentatonic"],
                "what music is written in comes first"
            );
            assert!(names.iter().any(|name| name == "neapolitan:major"));
            assert_eq!(panel.scale_rows().len(), names.len());

            // Enter opens a scale onto its tonics and stays on it.
            assert_eq!(panel.confirm(&reference), PanelAction::Nothing);
            assert_eq!(panel.scale_rows().len(), names.len() + 12);
            assert_eq!(panel.scale_rows()[1], ScaleRow::Tonic(0, 0));

            // A tonic row is the scale a score writes, with notes to hear.
            panel.move_by(1);
            assert_eq!(panel.selected_scale().as_deref(), Some("C:major"));
            let notes = panel.selected_scale_notes();
            assert_eq!(notes.len(), 8, "seven notes and the octave: {notes:?}");
            assert_eq!(notes[0], 60.0, "from C4");
            assert_eq!(notes[7], 72.0, "and home again");
            assert_eq!(panel.preview(), PanelAction::PreviewScale("C:major".into()));
            assert_eq!(
                panel.confirm(&reference),
                PanelAction::Copy("C:major".into())
            );

            // The search takes a name or a scale written out.
            panel.scale_query = "Eb:dor".into();
            assert_eq!(
                panel.scale_names().first().map(String::as_str),
                Some("dorian")
            );
        }

        /// A written-out tonic names a row the list can select: `Gb1` is
        /// `Gb`, the octave is read and dropped, and a sharp reads as its
        /// flat spelling.
        #[test]
        fn a_written_tonic_reads_as_one_of_the_twelve_roots() {
            for (tonic, root) in [
                ("C", "C"),
                ("C4", "C"),
                ("C-1", "C"),
                ("Gb1", "Gb"),
                ("F#2", "Gb"),
                ("Bb1", "Bb"),
                ("a#", "Bb"),
                ("d#4", "Eb"),
            ] {
                let index = tonic_root(tonic).unwrap_or_else(|| panic!("{tonic} names nothing"));
                assert_eq!(CHORD_ROOTS[index], root, "{tonic} is not {root}");
            }
            assert_eq!(tonic_root("bogus"), None, "a word is not a tonic");
            assert_eq!(tonic_root(""), None, "nothing is not a tonic");
            assert_eq!(tonic_root("9"), None, "an octave alone is not a tonic");
        }

        /// Every scale reads and plays from low to high on every tonic. Scale
        /// note names carry no octave, so one fixed octave would put the `C`
        /// of `Db:major` and the `Cb` of `Gb:major` below the degree before.
        #[test]
        fn every_scale_reads_from_low_to_high() {
            // The two that fold: a seventh written `C`, a fourth written `Cb`.
            assert_eq!(
                scale_notes("Db:major"),
                vec![61.0, 63.0, 65.0, 66.0, 68.0, 70.0, 72.0, 73.0],
                "Db major climbs to its C, an octave above where the letter alone puts it"
            );
            assert_eq!(
                scale_notes("Gb:major"),
                vec![66.0, 68.0, 70.0, 71.0, 73.0, 75.0, 77.0, 78.0],
                "Gb major's Cb is its fourth, not a note below its own tonic"
            );
            // C major is untouched: it never folded.
            assert_eq!(
                scale_notes("C:major"),
                vec![60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0]
            );

            // And nothing the browser offers reads backwards.
            for name in scale_vocabulary() {
                for tonic in CHORD_ROOTS {
                    let scale = format!("{tonic}:{name}");
                    let notes = scale_notes(&scale);
                    assert!(
                        notes.windows(2).all(|pair| pair[0] <= pair[1]),
                        "{scale} does not climb: {notes:?}"
                    );
                    if let (Some(first), Some(last)) = (notes.first(), notes.last()) {
                        assert!(
                            (last - first) % 12.0 == 0.0,
                            "{scale} does not land on its own tonic: {notes:?}"
                        );
                    }
                }
            }
        }

        /// Sharps and flats are drawn as sharps and flats; what a score types
        /// is unchanged.
        #[test]
        fn the_panel_spells_accidentals_the_way_music_does() {
            assert_eq!(pretty_notation("C7b9"), "C7♭9");
            assert_eq!(pretty_notation("Eb:major"), "E♭:major");
            assert_eq!(pretty_notation("7#11"), "7♯11");
            assert_eq!(pretty_notation("Bb4 Db5"), "B♭4 D♭5");
            // A `b` that is a letter of a word stays a letter.
            assert_eq!(pretty_notation("bebop:major"), "bebop:major");
            assert_eq!(pretty_notation("blues"), "blues");
        }

        /// The chords tab: the qualities music uses most first, each opening
        /// onto its twelve roots, every chord spelled out in notes, playable,
        /// and takeable.
        #[test]
        fn the_chords_tab_lists_qualities_by_use_and_opens_each_onto_its_roots() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Chords;
            let qualities = panel.chord_qualities();
            assert_eq!(
                qualities
                    .iter()
                    .take(5)
                    .map(|quality| quality.name)
                    .collect::<Vec<_>>(),
                ["major", "minor", "dominant 7", "major 7", "minor 7"],
                "what music uses most comes first"
            );
            assert!(
                qualities.iter().any(|quality| quality.symbol == "7b9"),
                "and the obscure ones are still there"
            );
            // Folded, one row per quality.
            assert_eq!(panel.chord_rows().len(), qualities.len());
            assert_eq!(panel.chord_rows()[0], ChordRow::Quality(0));

            // Enter opens a quality onto twelve roots and stays on it.
            assert_eq!(panel.confirm(&reference), PanelAction::Nothing);
            assert_eq!(panel.chord_rows().len(), qualities.len() + 12);
            assert_eq!(panel.chord_selected, 0);
            assert_eq!(panel.chord_rows()[1], ChordRow::Chord(0, 0));

            // The roots are the chords a score writes, and each has notes.
            panel.move_by(1);
            assert_eq!(panel.selected_chord().as_deref(), Some("C^"));
            // The dictionary's own voicing, spread as it wrote it.
            let notes = panel.selected_chord_notes();
            assert!(notes.len() >= 3, "a chord is several notes: {notes:?}");
            assert_eq!(notes[0], 48.0, "the root is C3");
            assert!(
                notes.windows(2).all(|pair| pair[0] < pair[1]),
                "the voicing rises: {notes:?}"
            );
            assert_eq!(panel.preview(), PanelAction::PreviewChord("C^".into()));
            // Enter takes it; the intent decides how, as everywhere else.
            assert_eq!(panel.confirm(&reference), PanelAction::Copy("C^".into()));
            panel.intent = PanelIntent::Insert;
            assert_eq!(panel.confirm(&reference), PanelAction::Insert("C^".into()));

            // ← folds the quality from inside it and leaves the reader on it.
            assert!(panel.collapse());
            assert_eq!(panel.chord_rows().len(), qualities.len());

            // The search finds a quality by name, by symbol, or by a whole
            // chord written as a score writes it.
            panel.chord_query = "minor 7".into();
            assert!(
                panel
                    .chord_qualities()
                    .iter()
                    .any(|quality| quality.symbol == "-7")
            );
            panel.chord_query = "Ab-7".into();
            let matched = panel
                .chord_qualities()
                .iter()
                .map(|quality| quality.symbol)
                .collect::<Vec<_>>();
            assert_eq!(matched.first().copied(), Some("-7"), "{matched:?}");
        }

        /// Walking a list by keys keeps a margin of rows between the selection
        /// and the edge it walks toward, so what comes next is in sight before
        /// it is selected. A click selects without scrolling - even when the
        /// click opens the row - and the next key brings the margin back.
        #[test]
        fn keys_keep_a_scroll_margin_and_a_click_holds_the_list_still() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Scales;
            let inner = inner_area(Rect::new(0, 0, 64, 30));
            let height = usize::from(panel.geometry(inner).list.height);
            let margin = crate::scroll::margin(height);
            assert_eq!(margin, 2, "a list this tall keeps two rows");
            let total = panel.scale_rows().len();
            assert!(total > height + 10, "the scales outnumber the window");

            // Down the list: the selection never comes nearer the bottom edge
            // than the margin, and the window does not move until it has to.
            for step in 1..=height + 4 {
                panel.move_by(1);
                let first = panel.geometry(inner).first_row;
                let from_bottom = first + height - 1 - panel.scale_selected;
                assert!(
                    from_bottom >= margin,
                    "step {step}: {from_bottom} rows below"
                );
                if panel.scale_selected + margin < height {
                    assert_eq!(first, 0, "step {step}: nothing scrolls before the margin");
                }
            }
            // Back up: the same margin toward the top.
            for step in 1..=6 {
                panel.move_by(-1);
                let first = panel.geometry(inner).first_row;
                assert!(
                    panel.scale_selected >= first + margin,
                    "step {step}: the selection keeps its rows above"
                );
            }

            // A click on the bottom row selects it and opens the scale, and the
            // list stays exactly where it was under the pointer.
            let geometry = panel.geometry(inner);
            let bottom = geometry.list.bottom() - 1;
            panel.click(&reference, geometry, bottom);
            assert_eq!(panel.scale_selected, geometry.first_row + height - 1);
            assert!(
                panel.scale_rows().len() > total,
                "the click opened the scale onto its tonics"
            );
            assert_eq!(
                panel.geometry(inner).first_row,
                geometry.first_row,
                "the clicked row stays under the pointer"
            );

            // The next key walks into the opened tonics with the margin kept:
            // the first of them is in sight, two rows up from the bottom.
            panel.move_by(1);
            let first = panel.geometry(inner).first_row;
            assert_eq!(first + height - 1 - panel.scale_selected, margin);
            assert!(first > geometry.first_row, "the list moved for the key");
        }

        /// A click that opens a scale while another one above it is open folds
        /// that one, which moves every row above the click up; the list moves
        /// with them, so the scale clicked stays on the screen row it was
        /// clicked on.
        #[test]
        fn a_click_opening_a_row_below_an_open_one_keeps_its_screen_row() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Scales;
            let inner = inner_area(Rect::new(0, 0, 64, 30));
            let geometry = panel.geometry(inner);
            // Open the first scale by clicking its row.
            panel.click(&reference, geometry, geometry.list.y);
            let opened = panel.scale_rows().len();
            // Walk down past its tonics until the window has scrolled.
            for _ in 0..30 {
                panel.move_by(1);
            }
            let geometry = panel.geometry(inner);
            assert!(geometry.first_row > 0, "the list scrolled");
            let row: u16 = 5;
            let clicked = panel.scale_rows()[geometry.first_row + usize::from(row)];
            assert!(
                matches!(clicked, ScaleRow::Scale(_)),
                "a scale row to click"
            );
            panel.click(&reference, geometry, geometry.list.y + row);
            let _ = opened;
            let after = panel.geometry(inner);
            assert_eq!(
                panel.scale_rows()[after.first_row + usize::from(row)],
                clicked,
                "the clicked scale is still under the pointer"
            );
        }

        /// An expanded musical tree can fill its viewport, but its last row must
        /// stop before the selected notes, scope, meter and footer at the foot of
        /// the panel. The samples tab already reserved its pulse rows; chords and
        /// scales once used the full default list and painted directly over them.
        #[test]
        fn expanded_chord_and_scale_lists_stop_before_their_visualization() {
            let reference = Reference::load_all();
            let theme = Theme::built_in_default();
            let area = Rect::new(4, 6, 64, 12);
            let inner = inner_area(area);
            let (scope_y, meter_y) = samples_pulse_rows(inner);
            let detail_y = scope_y - 1;
            let pulse = SamplesPulse {
                peak_db: -18.0,
                preview_gain: 1.0,
                shape: None,
            };
            let row_text = |buffer: &Buffer, y: u16| {
                (inner.x..inner.right())
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            };
            let draw = |panel: &ReferencePanel| {
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: Some(pulse),
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    #[cfg(feature = "hydra")]
                    playing: None,
                    #[cfg(feature = "hydra")]
                    preview_note: None,
                    #[cfg(feature = "hydra")]
                    preview_progress: None,
                    #[cfg(feature = "hydra")]
                    preview_row: None,
                    picture: false,
                    refused: None,
                    reference: &reference,
                    panel,
                    theme: &theme,
                }
                .render(area, &mut buffer);
                buffer
            };

            let mut scales = ReferencePanel::browse(&reference);
            scales.tab = Tab::Scales;
            assert_eq!(scales.confirm(&reference), PanelAction::Nothing);
            scales.scale_selected = 12; // B:major, the last tonic in the open tree.
            let scale_geometry = scales.geometry(inner);
            assert_eq!(scale_geometry.list.bottom(), detail_y);
            let scale_buffer = draw(&scales);
            // The selection keeps its scroll margin above the bottom edge, with
            // the next rows of the tree in sight below it.
            let margin = crate::scroll::margin(usize::from(scale_geometry.list.height)) as u16;
            assert!(
                row_text(&scale_buffer, scale_geometry.list.bottom() - 1 - margin)
                    .contains("B:major"),
                "the selected list row keeps its margin above the bottom of its viewport"
            );
            let keys = row_text(&scale_buffer, detail_y);
            assert!(
                keys.contains('█') && keys.contains('▀'),
                "a scale is drawn on the same keys a chord is: {keys:?}"
            );
            assert!(
                !row_text(&scale_buffer, scope_y).contains(":major"),
                "a scale row reached the scope"
            );
            assert!(row_text(&scale_buffer, meter_y).contains("0.0dB"));
            assert!(row_text(&scale_buffer, inner.bottom() - 1).contains("move"));

            let mut chords = ReferencePanel::browse(&reference);
            chords.tab = Tab::Chords;
            assert_eq!(chords.confirm(&reference), PanelAction::Nothing);
            chords.chord_selected = 12; // B major, the last root in the open tree.
            let chord_geometry = chords.geometry(inner);
            assert_eq!(chord_geometry.list.bottom(), detail_y);
            let chord_buffer = draw(&chords);
            let margin = crate::scroll::margin(usize::from(chord_geometry.list.height)) as u16;
            assert!(
                row_text(&chord_buffer, chord_geometry.list.bottom() - 1 - margin).contains("B^"),
                "the selected chord keeps its margin above the bottom of its viewport"
            );
            let keyboard = row_text(&chord_buffer, detail_y);
            assert!(
                keyboard.contains('█') && keyboard.contains('▀'),
                "the keyboard owns its reserved row: {keyboard:?}"
            );
            assert!(
                keyboard.trim_end().ends_with('▕'),
                "a short panel gets the bare strip, against the right edge: {keyboard:?}"
            );
            assert!(
                !row_text(&chord_buffer, scope_y).contains("B^"),
                "a chord row reached the scope"
            );
            assert!(row_text(&chord_buffer, meter_y).contains("0.0dB"));
            assert!(row_text(&chord_buffer, inner.bottom() - 1).contains("move"));
        }

        #[test]
        fn a_tall_chords_tab_draws_a_piano_with_names_on_the_keys() {
            let reference = Reference::load_all();
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 64, 26);
            let inner = inner_area(area);
            let (scope_y, _) = samples_pulse_rows(inner);
            let row_text = |buffer: &Buffer, y: u16| {
                (inner.x..inner.right())
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            };
            let mut chords = ReferencePanel::browse(&reference);
            chords.tab = Tab::Chords;
            assert_eq!(chords.confirm(&reference), PanelAction::Nothing);
            chords.chord_selected = 12; // B major.
            let geometry = chords.geometry(inner);
            assert_eq!(
                geometry.list.bottom() + 3,
                scope_y,
                "three rows for the piano"
            );

            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                reference: &reference,
                panel: &chords,
                theme: &theme,
            }
            .render(area, &mut buffer);

            let blacks = row_text(&buffer, geometry.list.bottom());
            let stems = row_text(&buffer, geometry.list.bottom() + 1);
            let names = row_text(&buffer, geometry.list.bottom() + 2);
            assert!(
                blacks.contains('█'),
                "black keys hang from the top row: {blacks:?}"
            );
            assert!(
                stems.contains('│') && !stems.contains('█'),
                "pipes run between the white keys: {stems:?}"
            );
            assert!(
                names.contains('C') && names.contains('B') && names.contains('D'),
                "letters sit on the white keys: {names:?}"
            );
            let letters: String = names.chars().filter(|c| c.is_ascii_uppercase()).collect();
            assert!(
                letters.contains("CDEFGAB"),
                "C through B reads left to right: {letters:?}"
            );

            // The sounding note is a colour change, not a filled chip.
            let mut live = Buffer::empty(area);
            let notes = chords.selected_chord_notes();
            super::super::render_keyboard(
                &mut live,
                inner,
                geometry.list.bottom(),
                3,
                None,
                &notes,
                Some(usize::MAX),
                &theme,
            );
            let names = row_text(&live, geometry.list.bottom() + 2);
            let recoloured = (inner.x..inner.right()).any(|x| {
                live.cell((x, geometry.list.bottom() + 2))
                    .is_some_and(|cell| cell.fg == theme.ok && cell.bg != theme.accent)
            });
            assert!(
                recoloured,
                "a sounding voicing recolours its letters, without a chip: {names:?}"
            );
        }

        #[test]
        fn musical_list_geometry_stays_above_the_scope_in_short_narrow_panels() {
            let reference = Reference::load_all();
            let theme = Theme::built_in_default();
            let pulse = SamplesPulse {
                peak_db: f32::NEG_INFINITY,
                preview_gain: 1.0,
                shape: None,
            };

            for width in [12, 15, 24] {
                for height in 6..=9 {
                    let area = Rect::new(3, 5, width, height);
                    let inner = inner_area(area);
                    let (scope_y, meter_y) = samples_pulse_rows(inner);
                    assert!(meter_y < inner.bottom());
                    for tab in [Tab::Chords, Tab::Scales] {
                        let mut panel = ReferencePanel::browse(&reference);
                        panel.tab = tab;
                        let geometry = panel.geometry(inner);
                        assert!(
                            geometry.list.bottom() <= scope_y,
                            "{tab:?} at {width}x{height}: list {:?}, scope {scope_y}",
                            geometry.list
                        );

                        // Rendering the minimum supported widths and heights is
                        // part of the contract: absent room means fewer list or
                        // detail rows, never drawing into the pulse/footer.
                        let mut buffer = Buffer::empty(area);
                        ReferenceView {
                            keybinds: &crate::keybinds::Keybinds::default(),
                            focused: true,
                            pulse: Some(pulse),
                            sounding_note: None,
                            loading: None,
                            caching: 0,
                            importing: 0,
                            library_loading: false,
                            #[cfg(feature = "hydra")]
                            playing: None,
                            #[cfg(feature = "hydra")]
                            preview_note: None,
                            #[cfg(feature = "hydra")]
                            preview_progress: None,
                            #[cfg(feature = "hydra")]
                            preview_row: None,
                            picture: false,
                            refused: None,
                            reference: &reference,
                            panel: &panel,
                            theme: &theme,
                        }
                        .render(area, &mut buffer);
                    }
                }
            }
        }
    }
    mod navigation {
        use super::super::*;

        #[test]
        fn the_header_tabs_are_hit_where_their_labels_are_drawn() {
            let inner = inner_area(Rect::new(0, 0, 60, 20));
            let y = inner.y;
            let showing = Tab::Reference;
            let (placed, hidden) = tab_layout(inner, showing);
            assert_eq!(hidden, 0, "a wide panel hides nothing");
            // Every label is hit across its whole width and nowhere else, and
            // the two are the same table rather than two kept in step by hand.
            for (tab, label, at) in &placed {
                assert_eq!(
                    tab_at(inner, showing, *at, y),
                    Some(*tab),
                    "{label} at its start"
                );
                assert_eq!(
                    tab_at(inner, showing, at + label.len() as u16 - 1, y),
                    Some(*tab),
                    "{label} at its end"
                );
                assert_eq!(
                    tab_at(inner, showing, at + label.len() as u16, y),
                    None,
                    "the gap after {label} is nothing"
                );
            }
            assert_eq!(
                tab_at(inner, showing, inner.x + 2, y + 1),
                None,
                "another row"
            );
        }

        /// The panel takes 38% of the body, at least 36 columns. The full tab
        /// labels do not fit that width on an 80-column terminal, so the
        /// header shortens the labels and still shows every tab.
        #[test]
        fn a_default_width_header_shortens_the_labels_rather_than_dropping_them() {
            for terminal in [80u16, 100, 120, 160] {
                // The panel's own sizing, from `view::regions`.
                let body = terminal - 2;
                let want = ((u32::from(body) * 38 / 100) as u16)
                    .clamp(36, 72)
                    .min(body);
                let inner = inner_area(Rect::new(body - want, 0, want, 20));
                let (placed, hidden) = tab_layout(inner, Tab::Reference);
                assert_eq!(hidden, 0, "a {terminal}-column terminal hides a tab");
                assert_eq!(placed.len(), tabs().len(), "{terminal} columns");
                // And every label is still hit across its own width.
                for (tab, label, at) in &placed {
                    assert_eq!(
                        tab_at(inner, Tab::Reference, *at, inner.y),
                        Some(*tab),
                        "{label}"
                    );
                }
            }
        }

        #[test]
        fn a_narrow_header_says_how_many_tabs_are_behind_the_edge() {
            let all = tabs().len();
            // Narrower than the tightest tier can seat, which takes 23.
            let narrow = inner_area(Rect::new(0, 0, 18, 20));
            let showing = Tab::Reference;
            let (placed, hidden) = tab_layout(narrow, showing);
            assert!(hidden > 0, "26 columns cannot hold them all");
            assert_eq!(placed.len() + hidden, all, "every tab is placed or counted");
            // Nothing is hit past the last label that was actually drawn.
            let last = placed.last().expect("at least one tab fits");
            let past = last.2 + last.1.len() as u16;
            for x in past..narrow.right() {
                assert_eq!(
                    tab_at(narrow, showing, x, narrow.y),
                    None,
                    "nothing is hit at {x}"
                );
            }
            // And the panel still reaches them: Tab cycles every tab, drawn or
            // not, which is why a count is enough and a scroll is not needed.
            assert!(placed.len() < all);
        }

        /// Tab reaches a tab that did not fit, and the strip goes with it.
        #[test]
        fn the_tab_being_shown_is_on_the_strip_however_narrow_it_is() {
            let all = tabs();
            let narrow = inner_area(Rect::new(0, 0, 18, 20));
            for (tab, _) in all {
                let (placed, hidden) = tab_layout(narrow, *tab);
                assert!(
                    placed.iter().any(|(seated, _, _)| seated == tab),
                    "{tab:?} is being shown and is not on the strip"
                );
                assert_eq!(
                    placed.len() + hidden,
                    all.len(),
                    "every tab is placed or counted"
                );
                // The label under the pointer is still the label that was
                // drawn there, whichever window the strip slid to.
                for (seated, label, at) in &placed {
                    assert_eq!(
                        tab_at(narrow, *tab, *at, narrow.y),
                        Some(*seated),
                        "{label}"
                    );
                }
            }
        }

        #[test]
        fn the_right_arrow_reads_an_entry_and_the_left_arrow_comes_back() {
            let reference = Reference::load(engine_knows());
            let mut panel = ReferencePanel::browse(&reference);
            panel.move_by(2);
            let picked = panel.selected;
            assert!(panel.expand(), "→ opens the entry under the cursor");
            let ReferenceMode::Entry { from_browse, .. } = panel.mode else {
                panic!("→ opens the entry");
            };
            assert!(from_browse);
            assert!(panel.collapse(), "← steps back out");
            assert!(matches!(panel.mode, ReferenceMode::Browse));
            assert_eq!(panel.selected, picked, "the cursor is where it was");

            // With nothing to do, neither arrow is claimed: `→` inside an entry
            // and `←` on the list fall through to the score.
            assert!(!panel.collapse());
            assert!(panel.expand());
            assert!(!panel.expand());
        }

        #[test]
        fn the_arrows_read_an_entry_even_where_enter_would_replace_a_word() {
            let reference = Reference::load(engine_knows());
            let mut panel = ReferencePanel::browse_for(&reference, "lpf");
            assert!(panel.expand(), "→ reads the entry");
            assert!(matches!(panel.mode, ReferenceMode::Entry { .. }));
            // Enter still means what it meant: from the entry it inserts.
            assert!(matches!(
                panel.confirm(&reference),
                PanelAction::Insert(name) if name == "lpf"
            ));
        }

        /// The browse list gathers its results under the tag each entry is
        /// filed by. An empty search puts the most used heading first. A
        /// search puts the best answer's heading first. Every result appears
        /// once, in search order within its run.
        #[test]
        fn the_list_gathers_its_results_under_the_tag_each_is_filed_by() {
            let reference = Reference::load_all();
            let searched = ReferencePanel::browse_for(&reference, "lpf");
            let first_name = searched
                .browse_rows()
                .iter()
                .find(|row| matches!(row, BrowseRow::Entry(_)));
            assert!(
                matches!(first_name, Some(BrowseRow::Entry(0))),
                "a search opens on the best answer"
            );
            let panel = ReferencePanel::browse(&reference);
            let rows = panel.browse_rows();
            assert!(rows.len() > panel.results.len(), "headings were added");
            let mut seen: Vec<usize> = Vec::new();
            let mut headings: Vec<&str> = Vec::new();
            let mut under: Option<&str> = None;
            for row in rows {
                match *row {
                    BrowseRow::Tag(position) => {
                        let entry = reference
                            .entry(panel.results[position])
                            .expect("the entry a heading names");
                        assert!(
                            !headings.contains(&entry.heading()),
                            "a tag heads one run and no more: {}",
                            entry.heading()
                        );
                        headings.push(entry.heading());
                        under = Some(entry.heading());
                    }
                    BrowseRow::Entry(position) => {
                        let entry = reference
                            .entry(panel.results[position])
                            .expect("the entry a row draws");
                        assert_eq!(
                            Some(entry.heading()),
                            under,
                            "{} sits under its own tag",
                            entry.name
                        );
                        seen.push(position);
                    }
                }
            }
            assert_eq!(headings[..3], ["temporal", "tonal", "audio"]);
            let mut order: Vec<usize> = seen.clone();
            order.sort_unstable();
            order.dedup();
            assert_eq!(order.len(), seen.len(), "no result is listed twice");
            assert_eq!(order.len(), panel.results.len(), "and none is dropped");
        }

        /// A `tag:` search lists its results flat. One heading over the whole
        /// list would repeat the search word, and one tag is no grouping.
        #[test]
        fn a_tag_word_lists_its_kind_flat_because_one_heading_is_no_grouping() {
            let reference = Reference::load_all();
            for query in ["tag:visualization", "tag:vis"] {
                let panel = ReferencePanel::browse_for(&reference, query);
                assert!(!panel.results.is_empty(), "{query} found something");
                let flat: Vec<BrowseRow> = (0..panel.results.len()).map(BrowseRow::Entry).collect();
                assert_eq!(panel.browse_rows(), flat, "{query} is not grouped");
                assert_eq!(
                    panel.selected_result(),
                    panel.results.first().copied(),
                    "{query} still starts on its best answer"
                );
            }
        }

        /// A heading is a label with nothing behind it, so the cursor carries
        /// on past it the way it was going. At the very top of a grouped list
        /// there is nowhere further up to go, so it turns round rather than
        /// stalling on the heading.
        #[test]
        fn the_cursor_steps_over_a_heading_and_lands_on_a_name() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            assert!(
                panel.browse_rows().len() > 20,
                "the whole reference, with headings"
            );
            for _ in 0..40 {
                assert!(
                    panel.selected_result().is_some(),
                    "row {} is a name, not a heading",
                    panel.selected
                );
                panel.move_by(1);
            }
            for _ in 0..60 {
                assert!(panel.selected_result().is_some(), "and the same going up");
                panel.move_by(-1);
            }
            panel.move_by(-5);
            assert!(
                panel.selected_result().is_some(),
                "↑ off the top turns round onto the first name"
            );
        }

        /// A click on a heading does nothing at all - the cursor does not even
        /// move to it - while a click on the name under it opens that entry.
        #[test]
        fn a_click_on_a_heading_does_nothing_and_a_click_on_a_name_opens_it() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            let geometry = PanelGeometry {
                list: Rect::new(0, 4, 40, 30),
                first_row: 0,
            };
            let heading = panel
                .browse_rows()
                .iter()
                .position(|row| matches!(row, BrowseRow::Tag(_)))
                .expect("a grouped list has one");
            let was = panel.selected;
            assert_eq!(
                panel.click(&reference, geometry, geometry.list.y + heading as u16),
                PanelAction::Nothing
            );
            assert_eq!(panel.selected, was, "and the cursor stayed where it was");
            assert_eq!(panel.mode, ReferenceMode::Browse, "nothing opened");
            let name = heading + 1;
            assert_eq!(
                panel.click(&reference, geometry, geometry.list.y + name as u16),
                PanelAction::Nothing,
                "opening an entry is not an action for the score"
            );
            assert_eq!(panel.selected, name);
            assert!(
                matches!(panel.mode, ReferenceMode::Entry { .. }),
                "the name under the heading opened"
            );
        }

        #[test]
        fn a_direct_entry_steps_out_to_a_list_seeded_with_its_name() {
            let reference = Reference::load(engine_knows());
            let index = *reference.by_name.get("lpf").expect("lpf");
            let mut panel = ReferencePanel::open(&reference, index);
            // Esc still closes a Ctrl+D entry in one press…
            assert!(matches!(
                ReferencePanel::open(&reference, index).escape(),
                PanelAction::Close
            ));
            // …while ← steps sideways into the list, landed on the entry.
            assert!(panel.collapse());
            assert_eq!(panel.selected_result(), Some(index));
        }

        #[test]
        fn the_column_only_takes_letters_where_there_is_a_search_box() {
            let reference = Reference::load(engine_knows());
            let mut panel = ReferencePanel::browse(&reference);
            assert!(panel.wants_text());
            assert!(panel.type_char(&reference, 'l'));
            assert_eq!(panel.query, "l");

            // Reading an entry, letters belong to the score.
            assert!(panel.expand());
            assert!(!panel.wants_text());
            assert!(!panel.type_char(&reference, 'x'));
            assert!(!panel.backspace(&reference));
            assert_eq!(panel.query, "l", "the box is untouched");
            // And that is the one view where they do.
            assert!(panel.types_through(), "an entry's page is the exception");
        }

        #[test]
        fn the_panel_walks_the_list_opens_an_entry_and_inserts_its_name() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            for character in "jux".chars() {
                panel.type_char(&reference, character);
            }
            assert_eq!(reference.entry(panel.results[0]).unwrap().name, "jux");
            panel.move_by(1);
            panel.move_by(-5);
            assert_eq!(
                panel
                    .selected_result()
                    .and_then(|index| reference.entry(index))
                    .map(|entry| entry.name.as_str()),
                Some("jux"),
                "↑ off the top of the list lands on the best answer, not on the heading over it"
            );
            assert_eq!(panel.confirm(&reference), PanelAction::Nothing);
            assert!(matches!(panel.mode, ReferenceMode::Entry { .. }));
            assert_eq!(panel.confirm(&reference), PanelAction::Insert("jux".into()));
            assert_eq!(panel.escape(), PanelAction::Nothing, "back to the list");
            assert_eq!(panel.mode, ReferenceMode::Browse);
            assert_eq!(panel.escape(), PanelAction::Close);

            let direct = ReferencePanel::open(&reference, reference.lookup("lpf").unwrap());
            let mut direct = direct;
            assert_eq!(
                direct.escape(),
                PanelAction::Close,
                "opened from the caret closes at once"
            );
        }
    }
    mod sample_sources {
        use super::super::*;

        /// The audio a set's own folder brought is a kind of its own in the
        /// browser, listed beside the pinned ones under the word a player reads
        /// there - which is the only thing that shows which `bd` is sounding.
        #[test]
        fn the_samples_a_set_brought_are_listed_under_their_own_kind() {
            let reference = Reference::load_all();
            let bank = |name: &str, category: SoundCategory, origin: SoundOrigin| SoundEntry {
                name: name.into(),
                variants: 2,
                variant_names: Vec::new(),
                origin,
                category,
                location: None,
                import: None,
            };
            let panel = ReferencePanel::samples(
                &reference,
                vec![
                    bank("bd", SoundCategory::Drums, SoundOrigin::Default),
                    bank("kicks", SoundCategory::Set, SoundOrigin::Set),
                ],
            );
            assert_eq!(
                SoundCategory::Set.label(),
                "set",
                "the word the browser files the set's own audio under"
            );
            assert!(
                panel
                    .visible_categories()
                    .contains(&(SoundSection::Kind(SoundCategory::Set), 1)),
                "the set's banks are a listed kind: {:?}",
                panel.visible_categories()
            );
        }

        /// An import the score names is listed as soon as it is named, with
        /// a word about where it stands - on its way, or refused - until its
        /// banks are in and it reads like any other section.
        #[test]
        fn an_import_still_loading_is_listed_with_a_word_about_it() {
            let reference = Reference::load_all();
            let bank = |name: &str, category: SoundCategory, import: Option<&str>| SoundEntry {
                name: name.into(),
                variants: 2,
                variant_names: Vec::new(),
                origin: SoundOrigin::Default,
                category,
                location: None,
                import: import.map(str::to_owned),
            };
            let mut panel =
                ReferencePanel::samples(&reference, vec![bank("bd", SoundCategory::Drums, None)]);
            assert!(panel.set_imports(vec![
                ("github:me/slow".into(), SourceState::Loading),
                (
                    "github:me/gone".into(),
                    SourceState::Failed("no strudel.json".into())
                ),
            ]));
            assert_eq!(
                panel.visible_categories(),
                [
                    (SoundSection::Kind(SoundCategory::Drums), 1),
                    (SoundSection::Import("github:me/gone".into()), 0),
                    (SoundSection::Import("github:me/slow".into()), 0),
                ]
            );
            assert_eq!(
                panel.import_state("github:me/slow"),
                Some(&SourceState::Loading)
            );
            // Nothing to open yet.
            panel.sound_selected = 2;
            panel.expand();
            assert_eq!(panel.sound_rows().len(), 3);

            // The banks land: the section is an ordinary one, and the reader
            // is still on it.
            assert!(panel.set_sounds(vec![
                bank("bd", SoundCategory::Drums, None),
                bank("slow_pad", SoundCategory::Score, Some("github:me/slow")),
            ]));
            assert!(panel.set_imports(vec![
                ("github:me/slow".into(), SourceState::Ready),
                (
                    "github:me/gone".into(),
                    SourceState::Failed("no strudel.json".into())
                ),
            ]));
            assert_eq!(
                panel.visible_categories()[2],
                (SoundSection::Import("github:me/slow".into()), 1)
            );
            assert_eq!(panel.sound_selected, 2, "still on the import");
            assert_eq!(panel.sound_rows().len(), 4, "its bank is listed under it");
        }

        /// A folder holding one bank named as the folder is - the recordings
        /// folder, its `recordings` bank - is that bank: one row, not a header
        /// opening onto the same name. A folder whose one bank is named
        /// otherwise keeps its header, which is the only place its name shows.
        #[test]
        fn a_source_of_one_bank_named_like_it_is_that_bank() {
            let reference = Reference::load_all();
            let recordings = folder(&["Users", "me", ".rustel", "recordings"]);
            let pads = folder(&["Users", "me", "pads"]);
            let entry = |name: &str, import: &str, variants: usize| SoundEntry {
                name: name.into(),
                variants,
                variant_names: Vec::new(),
                origin: SoundOrigin::Score,
                category: SoundCategory::Score,
                location: None,
                import: Some(import.to_owned()),
            };
            let sounds = vec![entry("recordings", &recordings, 4), entry("warm", &pads, 3)];
            let mut panel = ReferencePanel::samples(&reference, sounds);
            let rows = panel.sound_rows();
            assert_eq!(
                rows,
                [SoundRow::Bank(0), SoundRow::Category(1)],
                "the recordings bank stands in its folder's place; the pads folder keeps its header"
            );
            assert_eq!(panel.lone_banks(), [0]);

            // It opens onto its samples as any bank does, and plays as one.
            panel.sound_selected = 0;
            assert!(panel.expand());
            assert_eq!(
                panel.sound_rows(),
                [
                    SoundRow::Bank(0),
                    SoundRow::Variant(0, 0),
                    SoundRow::Variant(0, 1),
                    SoundRow::Variant(0, 2),
                    SoundRow::Variant(0, 3),
                    SoundRow::Category(1),
                ]
            );
            panel.move_by(2);
            assert_eq!(panel.preview(), PanelAction::Preview("recordings:1".into()));
        }

        /// Each `samples(…)` import is a section of its own, at the same level
        /// as the kinds - a library somebody brought in, next to the pinned
        /// ones - opening onto its banks like any of them.
        #[test]
        fn each_import_is_a_section_of_its_own() {
            let reference = Reference::load_all();
            let entry = |name: &str, category: SoundCategory, import: Option<&str>| SoundEntry {
                name: name.into(),
                variants: 2,
                variant_names: Vec::new(),
                origin: if import.is_some() {
                    SoundOrigin::Score
                } else {
                    SoundOrigin::Default
                },
                category,
                location: None,
                import: import.map(str::to_owned),
            };
            let sounds = vec![
                entry("bd", SoundCategory::Drums, None),
                entry(
                    "brk",
                    SoundCategory::Score,
                    Some("github:yaxu/clean-breaks"),
                ),
                entry("kit_bd", SoundCategory::Score, Some("github:me/kit")),
                entry("kit_sd", SoundCategory::Score, Some("github:me/kit")),
            ];
            let mut panel = ReferencePanel::samples(&reference, sounds);
            assert_eq!(
                panel.visible_categories(),
                [
                    (SoundSection::Kind(SoundCategory::Drums), 1),
                    (SoundSection::Import("github:me/kit".into()), 2),
                    (SoundSection::Import("github:yaxu/clean-breaks".into()), 1),
                ],
                "the kinds first, then the imports"
            );
            assert_eq!(
                panel.sound_rows(),
                [
                    SoundRow::Category(0),
                    SoundRow::Category(1),
                    SoundRow::Category(2),
                ]
            );
            // An import opens onto its banks - grouped like anyone else's - a
            // bank onto its samples.
            panel.sound_selected = 1;
            panel.expand();
            assert_eq!(
                panel.sound_rows(),
                [
                    SoundRow::Category(0),
                    SoundRow::Category(1),
                    SoundRow::Family(0),
                    SoundRow::Category(2),
                ]
            );
            panel.move_by(1);
            panel.expand();
            assert_eq!(
                panel.sound_rows(),
                [
                    SoundRow::Category(0),
                    SoundRow::Category(1),
                    SoundRow::Family(0),
                    SoundRow::Bank(2),
                    SoundRow::Bank(3),
                    SoundRow::Category(2),
                ]
            );
            panel.move_by(1);
            assert_eq!(panel.preview(), PanelAction::Preview("kit_bd:0".into()));
            assert_eq!(panel.escape(), PanelAction::Close, "Esc closes the browser");
        }

        /// Two imported folders of one pack hold banks with the same prefix.
        /// Each folder keeps its own family: grouping on the prefix alone
        /// leaves one folder with a heading, a count and nothing under it.
        #[test]
        fn two_imports_of_one_pack_each_keep_their_own_rows() {
            let reference = Reference::load_all();
            let entry = |name: &str, import: &str| SoundEntry {
                name: name.into(),
                variants: 4,
                variant_names: Vec::new(),
                origin: SoundOrigin::Global,
                category: SoundCategory::Mine,
                location: None,
                import: Some(import.into()),
            };
            let first_import = "/packs/Sounds of KSHMR/KSHMR_Drum_Enhancers";
            let second_import = "/packs/Sounds of KSHMR/percussions";
            let sounds = vec![
                entry("KSHMR_Kick_Enhancer_01", first_import),
                entry("KSHMR_Kick_Enhancer_02", first_import),
                entry("KSHMR_Orchestral_Drums", second_import),
                entry("KSHMR_Orchestral_Taiko", second_import),
            ];
            let mut panel = ReferencePanel::samples(&reference, sounds);
            let sections = panel.section_categories();
            assert_eq!(sections.len(), 2, "two imports, two headings");
            for section in &sections {
                panel.open_categories.insert(section.clone());
            }

            // A family row under each heading, not two under one and none
            // under the other.
            let rows = panel.sound_rows();
            assert_eq!(
                rows,
                [
                    SoundRow::Category(0),
                    SoundRow::Family(0),
                    SoundRow::Category(1),
                    SoundRow::Family(1),
                ],
                "each import keeps the banks its heading counts"
            );
            let families = panel.sound_families();
            assert_eq!(families[0].label, "KSHMR");
            assert_eq!(families[1].label, "KSHMR");
            assert_eq!(families[0].members.len(), 2);
            assert_eq!(families[1].members.len(), 2);
            assert_ne!(
                families[0].key, families[1].key,
                "two families of the same name are still two families"
            );

            // And they fold one at a time: opening one must not open the
            // other, which sharing a label by itself would have done.
            panel.open_family = Some(families[0].key.clone());
            assert_eq!(
                panel.sound_rows(),
                [
                    SoundRow::Category(0),
                    SoundRow::Family(0),
                    SoundRow::Bank(0),
                    SoundRow::Bank(1),
                    SoundRow::Category(1),
                    SoundRow::Family(1),
                ]
            );

            // Enter on the second family opens it and leaves the cursor on
            // it, although both families have the same label.
            panel.open_family = None;
            let second = panel
                .sound_rows()
                .iter()
                .position(|row| *row == SoundRow::Family(1))
                .expect("the second family has a row");
            panel.sound_selected = second;
            assert_eq!(panel.confirm(&reference), PanelAction::Nothing);
            assert_eq!(panel.open_family.as_deref(), Some(families[1].key.as_str()));
            assert_eq!(
                panel.sound_rows().get(panel.sound_selected),
                Some(&SoundRow::Family(1)),
                "and the cursor is still on the one that opened"
            );

            // A reload keeps it there too - the catalogue is re-read every
            // second while the browser is open, so this runs constantly.
            let mut sounds = panel.sounds.clone();
            sounds.push(entry("KSHMR_Orchestral_Strings", second_import));
            assert!(panel.set_sounds(sounds));
            assert_eq!(
                panel.sound_rows().get(panel.sound_selected),
                Some(&SoundRow::Family(1)),
                "a reload puts the reader back on the family they were on"
            );
        }

        /// An absolute folder path, spelled the way this platform spells one.
        ///
        /// `import_folder` uses `Path::is_absolute`, which needs a drive prefix
        /// on Windows, so a Unix fixture is not a folder there. The labels do
        /// not change with the platform: `section_labels` keeps only
        /// `Component::Normal` and joins the parts with `/`.
        fn folder(components: &[&str]) -> String {
            let mut path = std::path::PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" });
            for component in components {
                path.push(component);
            }
            path.display().to_string()
        }

        /// An import heading is the folder's own name, not its path. Imports
        /// that end in the same name each add one parent, and only those do.
        #[test]
        fn import_headings_read_as_folder_names_and_stay_distinct() {
            let plain = [
                SoundSection::Import(folder(&[
                    "Users",
                    "a",
                    "Dropbox",
                    "Samples",
                    "KSHMR_Drum_Enhancers",
                ])),
                SoundSection::Import(format!(
                    "local:{}",
                    folder(&["Users", "a", "Dropbox", "Samples", "percussions"])
                )),
                SoundSection::Import("github:tidalcycles/dirt-samples".into()),
                SoundSection::Kind(SoundCategory::Drums),
            ];
            assert_eq!(
                section_labels(&plain),
                [
                    "KSHMR_Drum_Enhancers",
                    "percussions",
                    "github:tidalcycles/dirt-samples",
                    SoundCategory::Drums.label(),
                ]
            );

            let clashing = [
                SoundSection::Import(folder(&["packs", "Vol.1", "kicks"])),
                SoundSection::Import(folder(&["packs", "Vol.2", "kicks"])),
                SoundSection::Import(folder(&["packs", "Vol.1", "snares"])),
            ];
            assert_eq!(
                section_labels(&clashing),
                ["Vol.1/kicks", "Vol.2/kicks", "snares"],
                "only the rows that clash grow"
            );

            // Two spellings of one folder - imported here, and named by a
            // score - are two rows that no amount of parent will separate.
            // They say the thing that actually differs: what was written.
            let kicks = folder(&["packs", "kicks"]);
            let same_folder = [
                SoundSection::Import(kicks.clone()),
                SoundSection::Import(format!("local:{kicks}")),
            ];
            assert_eq!(
                section_labels(&same_folder),
                [kicks.clone(), format!("local:{kicks}")]
            );
        }

        #[test]
        fn banks_sharing_a_machine_name_group_into_one_family_row() {
            let reference = Reference::load_all();
            let entry = |name: &str, variants: usize| SoundEntry {
                name: name.into(),
                variants,
                variant_names: Vec::new(),
                origin: SoundOrigin::Default,

                category: SoundCategory::Other,
                location: None,
                import: None,
            };
            let sounds = vec![
                entry("AkaiLinn", 12),
                entry("AkaiLinn_bd", 2),
                entry("AkaiLinn_sd", 3),
                entry("bd", 3),
            ];
            let mut panel = ReferencePanel::samples(&reference, sounds);
            // 683 drum-machine banks were 683 rows; one machine is one row.
            assert_eq!(panel.sound_rows(), [SoundRow::Family(0), SoundRow::Bank(3)]);

            // → opens the family and its members list under it.
            panel.expand();
            assert_eq!(
                panel.sound_rows(),
                [
                    SoundRow::Family(0),
                    SoundRow::Bank(0),
                    SoundRow::Bank(1),
                    SoundRow::Bank(2),
                    SoundRow::Bank(3),
                ]
            );
            // A member bank still expands to its variants, one level deeper.
            panel.move_by(2);
            panel.expand();
            assert_eq!(
                panel.preview(),
                PanelAction::Preview("AkaiLinn_bd:0".into())
            );
            assert_eq!(panel.sound_rows().len(), 7);
            panel.collapse();
            assert_eq!(panel.sound_rows().len(), 5);
            // ← again folds the family and lands back on its header.
            panel.collapse();
            assert_eq!(panel.sound_rows(), [SoundRow::Family(0), SoundRow::Bank(3)]);
            assert_eq!(panel.sound_selected, 0);

            // Enter on the header is the fold, like a shelf's.
            assert_eq!(panel.confirm(&reference), PanelAction::Nothing);
            assert_eq!(panel.sound_rows().len(), 5);
            // Esc leaves the browser, open family or not; ← is what folds.
            assert_eq!(panel.escape(), PanelAction::Close);
            assert_eq!(panel.sound_rows().len(), 5);
            panel.collapse();
            assert_eq!(panel.sound_rows().len(), 2);

            // A search flattens: it shows exactly the banks it matched.
            for character in "akai".chars() {
                panel.type_char(&reference, character);
            }
            let rows = panel.sound_rows();
            assert_eq!(rows.len(), 3, "{rows:?}");
            assert!(rows.iter().all(|row| matches!(row, SoundRow::Bank(_))));
        }

        #[test]
        fn the_samples_tab_knows_where_a_bank_came_from() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            panel.set_sounds(vec![
                SoundEntry {
                    name: "bd".into(),
                    variants: 2,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,

                    category: SoundCategory::Other,
                    location: Some("https://strudel.b-cdn.net/Dirt-Samples/bd/BT0A0A7.wav".into()),
                    import: None,
                },
                SoundEntry {
                    name: "gm_piano".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Font,
                    category: SoundCategory::Other,
                    location: None,
                    import: None,
                },
            ]);
            assert_eq!(
                panel.location(),
                PanelAction::Nothing,
                "not on the samples tab yet"
            );
            panel.toggle_tab();
            let url = "https://strudel.b-cdn.net/Dirt-Samples/bd/BT0A0A7.wav";
            assert_eq!(
                panel.location(),
                PanelAction::Reveal {
                    name: "bd".into(),
                    variant: None,
                    url: url.into(),
                },
                "a bank row shows the bank"
            );
            assert_eq!(
                panel.selected_sample_source().as_deref(),
                Some("Dirt-Samples"),
                "the sample browser names the shipped pack"
            );
            // Opened, the bank's numbered samples each name their own file.
            assert!(panel.expand(), "bd opens onto its samples");
            panel.move_by(2);
            assert_eq!(
                panel.location(),
                PanelAction::Reveal {
                    name: "bd".into(),
                    variant: Some(1),
                    url: url.into(),
                },
                "a sample row asks for that sample, not the bank's first file"
            );
            panel.move_by(1);
            assert_eq!(
                panel.selected_sample_source().as_deref(),
                Some("gm soundfonts")
            );
            assert_eq!(
                panel.location(),
                PanelAction::Nothing,
                "a font has no folder"
            );
        }

        #[test]
        fn sample_source_labels_preserve_import_specs_and_compact_local_folders() {
            let reference = Reference::load_all();
            let empty = ReferencePanel::samples(&reference, Vec::new());
            assert_eq!(sample_source_line(&empty), "source: select a sound");
            assert!(!sample_source_line(&empty).contains('-'));

            let mut panel = ReferencePanel::samples(
                &reference,
                vec![SoundEntry {
                    name: "remote".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Score,
                    category: SoundCategory::Other,
                    location: None,
                    import: Some("github:algorave-dave/samples".into()),
                }],
            );
            assert_eq!(
                panel.selected_sample_source().as_deref(),
                Some("github:algorave-dave/samples")
            );

            panel.set_sounds(vec![SoundEntry {
                name: "take".into(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Global,
                category: SoundCategory::Other,
                location: None,
                import: Some(folder(&["Users", "player", "Music", "sessions"])),
            }]);
            assert_eq!(panel.selected_sample_source().as_deref(), Some("sessions"));
        }
    }
    mod samples {
        use super::super::*;

        #[test]
        fn an_empty_bank_filter_explains_backspace_and_updates_compatibility() {
            let reference = Reference::load_all();
            let names = vec!["Short".to_owned()];
            let mut panel = ReferencePanel::bank_vocabulary_for(
                &reference,
                names.clone(),
                vec!["bd:3".to_owned()],
                0,
                "",
            );
            assert!(panel.results.is_empty());
            assert_eq!(
                panel.banks_miss_line().as_deref(),
                Some("no compatible banks")
            );

            let area = Rect::new(0, 0, 50, 18);
            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                reference: &reference,
                panel: &panel,
                theme: &Theme::built_in_default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                caching: 0,
                importing: 0,
                library_loading: false,
            }
            .render(area, &mut buffer);
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("no compatible banks"), "{text}");
            assert!(text.contains("Backspace shows all"), "{text}");

            // A newly available variant can change compatibility without changing
            // the list of bank names. Refresh must still update the visible results.
            assert!(panel.set_bank_names(&reference, names.clone(), 1));
            assert_eq!(panel.results, [0]);
            assert!(panel.backspace(&reference));
            assert!(!panel.compatible_banks_only());
            assert!(panel.set_bank_names(&reference, names, 0));
            assert_eq!(
                panel.results,
                [0],
                "refresh preserves the request for all banks"
            );
        }

        /// Ctrl+Backspace empties a bank search the way Backspace does one letter
        /// at a time: the text first, keeping the filter, and from an empty search
        /// every bank.
        #[test]
        fn ctrl_backspace_clears_a_bank_search_then_shows_every_bank() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::bank_vocabulary_for(
                &reference,
                vec!["Ready".to_owned(), "Short".to_owned()],
                vec!["bd:3".to_owned()],
                1,
                "",
            );
            assert_eq!(panel.results, [0]);
            assert!(panel.type_char(&reference, 'r'));
            assert!(panel.clear_query(&reference));
            assert_eq!(panel.query, "");
            assert!(
                panel.compatible_banks_only(),
                "the text went, not the filter"
            );
            assert_eq!(panel.results, [0]);
            assert!(panel.clear_query(&reference));
            assert!(!panel.compatible_banks_only());
            assert_eq!(panel.results.len(), 2);
            assert!(!panel.clear_query(&reference), "nothing left to clear");
        }

        /// An empty list names what the studio still reads: the library or
        /// the imports. It says "no sounds" only when nothing is loading.
        #[test]
        fn an_empty_catalogue_says_what_it_is_waiting_for() {
            let reference = Reference::load_all();
            let theme = Theme::default();
            let area = Rect::new(0, 0, 44, 20);
            let count_row = |importing: usize, library_loading: bool, sounds: Vec<SoundEntry>| {
                let panel = ReferencePanel::samples(&reference, sounds);
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    reference: &reference,
                    panel: &panel,
                    theme: &theme,
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    picture: false,
                    refused: None,
                    loading: None,
                    caching: 0,
                    importing,
                    library_loading,
                    #[cfg(feature = "hydra")]
                    playing: None,
                    #[cfg(feature = "hydra")]
                    preview_note: None,
                    #[cfg(feature = "hydra")]
                    preview_progress: None,
                    #[cfg(feature = "hydra")]
                    preview_row: None,
                }
                .render(area, &mut buffer);
                let inner = inner_area(area);
                (0..inner.width)
                    .map(|x| buffer[(inner.x + x, inner.y + 2)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            };

            assert_eq!(count_row(0, true, Vec::new()), "reading the library…");
            assert_eq!(count_row(1, true, Vec::new()), "reading 1 import…");
            assert_eq!(count_row(3, false, Vec::new()), "reading 3 imports…");
            // Nothing on its way and nothing in the list is the one case where
            // "no sounds" is the truth.
            assert_eq!(count_row(0, false, Vec::new()), "no sounds");

            // With sounds in it the row counts them, and says what is still
            // arriving beside the count rather than instead of it.
            let sounds = vec![SoundEntry {
                name: "kick".into(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Global,
                category: SoundCategory::Mine,
                location: None,
                import: None,
            }];
            assert_eq!(count_row(0, false, sounds.clone()), "1 of 1 sounds");
            assert_eq!(
                count_row(2, true, sounds),
                "1 of 1 sounds · reading 2 imports…"
            );
        }

        /// The preview row shows the sample's own shape, filling in as the
        /// sound advances. A scope shows the last few milliseconds and says
        /// nothing about where in a sound you are, which for a browser whose
        /// whole job is auditioning is the thing worth knowing.
        #[test]
        fn the_preview_row_draws_the_sample_and_fills_as_it_plays() {
            let theme = Theme::default();
            let area = Rect::new(0, 0, 24, 3);
            // Quiet at the edges, loud in the middle: a shape with somewhere
            // for the envelope to go.
            let shape: Vec<u8> = (0..256)
                .map(|at: i32| (255 - (at - 128).abs() * 2).clamp(0, 255) as u8)
                .collect();
            let drawn = |played: f32| {
                let mut buffer = Buffer::empty(area);
                render_sample_shape(&shape, played, area, &mut buffer, &theme);
                buffer
            };
            let marks = |buffer: &Buffer, colour| {
                (0..area.width)
                    .filter(|x| {
                        (0..area.height).any(|y| {
                            let cell = &buffer[(*x, y)];
                            cell.fg == colour && cell.symbol() != " " && cell.symbol() != "\u{2800}"
                        })
                    })
                    .collect::<Vec<_>>()
            };

            let start = drawn(0.0);
            assert!(
                marks(&start, theme.accent).is_empty(),
                "nothing is played yet"
            );
            let whole = marks(&start, theme.rule);
            assert!(
                whole.len() > usize::from(area.width) / 2,
                "and the whole shape is drawn all the same: {whole:?}"
            );

            // It is mirrored: the middle column reaches both above and below
            // the centre row, which a bar chart growing from the floor does
            // not.
            let middle_column = area.width / 2;
            let drawn_at = |buffer: &Buffer, x: u16, y: u16| {
                !matches!(buffer[(x, y)].symbol(), " " | "\u{2800}")
            };
            let rows: Vec<u16> = (0..area.height)
                .filter(|y| drawn_at(&start, middle_column, *y))
                .collect();
            assert!(
                rows.contains(&0) && rows.contains(&(area.height - 1)),
                "the loudest column reaches both edges: {rows:?}"
            );
            // And a quiet column does not.
            let quiet = (0..area.width)
                .find(|x| {
                    (0..area.height)
                        .filter(|y| drawn_at(&start, *x, *y))
                        .count()
                        == 1
                })
                .expect("a column near the edge that only reaches the middle row");
            assert!(
                quiet < middle_column,
                "the quiet columns are the outer ones: {quiet}"
            );

            // Half way through, the left half is lit and the right is not, and
            // the shape itself has not moved.
            let middle = drawn(0.5);
            let played = marks(&middle, theme.accent);
            let waiting = marks(&middle, theme.rule);
            assert!(!played.is_empty() && !waiting.is_empty());
            assert!(
                played.iter().max() < waiting.iter().min(),
                "filled from the left: {played:?} then {waiting:?}"
            );
            assert_eq!(
                marks(&drawn(1.0), theme.rule),
                Vec::<u16>::new(),
                "and lit the whole way at the end"
            );

            // A silent sample draws nothing rather than a line across.
            let mut buffer = Buffer::empty(area);
            render_sample_shape(&[0; 64], 0.5, area, &mut buffer, &theme);
            assert!(
                (0..area.width).all(|x| (0..area.height)
                    .all(|y| matches!(buffer[(x, y)].symbol(), " " | "\u{2800}"))),
                "silence has no envelope"
            );
        }

        #[test]
        fn the_preview_fader_can_land_exactly_on_unity() {
            let inner = Rect::new(0, 0, 40, 10);
            let exact = (inner.x..inner.right())
                .map(|x| preview_gain_at(inner, x))
                .filter(|gain| *gain == 1.0)
                .count();
            assert!(exact >= 1, "some column must mean exactly 0 dB");
        }

        /// The whole library is grouped by category, synths first, with a
        /// blank row before each heading; Ctrl+Space inside `s("…")` finds a
        /// synth like any bank, and a search drops the headings.
        #[test]
        fn the_samples_tab_lists_the_synths_under_their_own_heading_and_finds_them() {
            let reference = Reference::load_all();
            let synth = |name: &str| SoundEntry {
                name: name.into(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Synth,

                category: SoundCategory::Synth,
                location: None,
                import: None,
            };
            let sounds = vec![
                synth("sbd"),
                synth("supersaw"),
                SoundEntry {
                    name: "bd".into(),
                    variants: 3,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,

                    category: SoundCategory::Other,
                    location: None,
                    import: None,
                },
            ];
            let mut panel = ReferencePanel::samples(&reference, sounds);
            assert_eq!(
                panel.visible_categories(),
                vec![
                    (SoundSection::Kind(SoundCategory::Synth), 2),
                    (SoundSection::Kind(SoundCategory::Other), 1)
                ]
            );
            // Everything starts folded: two rows, not three sounds.
            assert_eq!(
                panel.sound_rows(),
                vec![SoundRow::Category(0), SoundRow::Category(1)]
            );
            // Enter opens a kind onto its banks, and folds it again.
            assert_eq!(panel.confirm(&reference), PanelAction::Nothing);
            assert_eq!(
                panel.sound_rows(),
                vec![
                    SoundRow::Category(0),
                    SoundRow::Bank(0),
                    SoundRow::Bank(1),
                    SoundRow::Category(1),
                ],
                "the kind the reader opened holds its own banks"
            );
            // → opens, ← folds, on the row the reader stands on.
            assert!(!panel.expand(), "already open");
            assert!(panel.collapse());
            assert_eq!(
                panel.sound_rows(),
                vec![SoundRow::Category(0), SoundRow::Category(1)]
            );
            assert!(panel.expand());
            // A sound inside an open kind is taken as ever.
            panel.move_by(2);
            assert_eq!(panel.preview(), PanelAction::Preview("supersaw".into()));

            panel.sound_query = "sup".into();
            panel.refresh_sounds();
            assert_eq!(
                panel.sound_rows(),
                vec![SoundRow::Bank(1)],
                "a search answers with the sounds themselves"
            );
            assert_eq!(panel.preview(), PanelAction::Preview("supersaw".into()));
            assert_eq!(
                panel.confirm(&reference),
                PanelAction::Insert("supersaw".into())
            );
            panel.expand();
            assert_eq!(
                panel.sound_rows(),
                vec![SoundRow::Bank(1)],
                "a synth has no variants to open"
            );
        }

        /// Opening a bank folds whatever else was open - rows above the
        /// cursor disappear - and the reader must still be on the row they
        /// pressed, not wherever that row number now lands.
        #[test]
        fn opening_a_bank_leaves_the_reader_on_the_bank_they_opened() {
            let reference = Reference::load_all();
            let bank = |name: &str, variants: usize| SoundEntry {
                name: name.into(),
                variants,
                variant_names: Vec::new(),
                origin: SoundOrigin::Font,
                category: SoundCategory::Font,
                location: None,
                import: None,
            };
            let mut panel = ReferencePanel::samples(
                &reference,
                // Plain names, so no machine groups them: this is about the
                // cursor, not the families.
                vec![
                    bank("strings", 7),
                    bank("bass", 9),
                    bank("brass", 4),
                    bank("drum", 6),
                ],
            );
            // One kind, so the banks are the list. Open the first.
            assert_eq!(panel.sound_selected, 0);
            assert!(panel.expand());
            assert_eq!(panel.sound_rows().len(), 4 + 7);

            // Walk down to a later bank and open that one: the seven samples
            // above close, and the reader stays on the bank they pressed.
            let brass = panel
                .sound_rows()
                .iter()
                .position(
                    |row| matches!(row, SoundRow::Bank(index) if panel.sounds[*index].name == "brass"),
                )
                .expect("the brass bank");
            panel.sound_selected = brass;
            assert!(panel.expand());
            assert_eq!(
                panel.selected_sound(),
                Some(SelectedSound::Sound("brass".into())),
                "the cursor did not slide when the rows above it closed"
            );
            assert_eq!(panel.preview(), PanelAction::Preview("brass:0".into()));
            // ← folds it and leaves the reader on it.
            assert!(panel.collapse());
            assert_eq!(
                panel.selected_sound(),
                Some(SelectedSound::Sound("brass".into()))
            );
        }

        /// A search cuts through the tree: the sounds themselves, whatever
        /// kind they are, and folding is forgotten until the search is.
        #[test]
        fn a_search_answers_with_the_sounds_and_not_the_groups() {
            let reference = Reference::load_all();
            let entry = |name: &str, category: SoundCategory| SoundEntry {
                name: name.into(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Default,
                category,
                location: None,
                import: None,
            };
            let mut panel = ReferencePanel::samples(
                &reference,
                vec![
                    entry("supersaw", SoundCategory::Synth),
                    entry("bd", SoundCategory::Drums),
                    entry("sd", SoundCategory::Drums),
                    entry("wt_sine", SoundCategory::Wavetable),
                ],
            );
            assert_eq!(
                panel.sound_rows(),
                vec![
                    SoundRow::Category(0),
                    SoundRow::Category(1),
                    SoundRow::Category(2)
                ],
                "three kinds, all folded"
            );
            assert!(panel.type_char(&reference, 'd'));
            assert_eq!(
                panel.sound_rows(),
                vec![SoundRow::Bank(1), SoundRow::Bank(2)],
                "the sounds that match, ungrouped"
            );
            assert!(panel.visible_categories().is_empty());
            panel.backspace(&reference);
            assert_eq!(
                panel.sound_rows(),
                vec![
                    SoundRow::Category(0),
                    SoundRow::Category(1),
                    SoundRow::Category(2)
                ],
                "the tree comes back with the search cleared"
            );
        }

        #[test]
        fn the_samples_tab_lists_banks_expands_variants_and_previews() {
            let reference = Reference::load_all();
            let sounds = vec![
                SoundEntry {
                    name: "bd".into(),
                    variants: 3,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,

                    category: SoundCategory::Other,
                    location: None,
                    import: None,
                },
                SoundEntry {
                    name: "mine".into(),
                    variants: 2,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Score,
                    // One category throughout: this test is about banks and
                    // variants, not the headings.
                    category: SoundCategory::Other,
                    location: None,
                    import: None,
                },
                SoundEntry {
                    name: "gm_piano".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Font,
                    category: SoundCategory::Other,
                    location: None,
                    import: None,
                },
            ];
            let mut panel = ReferencePanel::samples(&reference, sounds.clone());
            assert_eq!(panel.tab, Tab::Samples);
            assert_eq!(panel.sound_rows().len(), 3);
            assert_eq!(panel.preview(), PanelAction::Preview("bd:0".into()));
            assert_eq!(panel.confirm(&reference), PanelAction::Insert("bd".into()));

            panel.expand();
            let rows = panel.sound_rows();
            assert_eq!(rows.len(), 6, "{rows:?}");
            assert_eq!(rows[1], SoundRow::Variant(0, 0));
            panel.move_by(2);
            assert_eq!(panel.preview(), PanelAction::Preview("bd:1".into()));
            assert_eq!(
                panel.confirm(&reference),
                PanelAction::Insert("bd:1".into())
            );
            panel.collapse();
            assert_eq!(panel.sound_rows().len(), 3);
            assert_eq!(panel.sound_selected, 0);

            for character in "mi".chars() {
                panel.type_char(&reference, character);
            }
            assert_eq!(panel.sound_rows(), [SoundRow::Bank(1)]);
            assert_eq!(panel.preview(), PanelAction::Preview("mine:0".into()));
            assert_eq!(panel.escape(), PanelAction::Close);

            // A refreshed catalogue keeps the selection on the same bank.
            panel.backspace(&reference);
            panel.backspace(&reference);
            panel.move_by(2);
            let mut more = sounds.clone();
            more.insert(
                0,
                SoundEntry {
                    name: "aa".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,

                    category: SoundCategory::Other,
                    location: None,
                    import: None,
                },
            );
            assert!(panel.set_sounds(more));
            assert_eq!(panel.sound_rows()[panel.sound_selected], SoundRow::Bank(3));
            assert!(
                !panel.set_sounds(panel.sounds.clone()),
                "unchanged is a no-op"
            );

            // Tab walks the column: reference, samples, chords, and - in a
            // build with visuals - the snippet shelf, then round again.
            panel.toggle_tab();
            assert_eq!(panel.tab, Tab::Chords);
            panel.toggle_tab();
            assert_eq!(panel.tab, Tab::Scales);
            panel.toggle_tab();
            #[cfg(feature = "hydra")]
            {
                assert_eq!(panel.tab, Tab::Generator);
                panel.toggle_tab();
                assert_eq!(panel.tab, Tab::Examples);
                panel.toggle_tab();
            }
            assert_eq!(panel.tab, Tab::Reference);

            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 50, 12);
            let mut buffer = Buffer::empty(area);
            let mut drawn = ReferencePanel::samples(&reference, sounds);
            drawn.expand();
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                reference: &reference,
                panel: &drawn,
                theme: &theme,
            }
            .render(area, &mut buffer);
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("samples"), "{text}");
            assert!(text.contains("▾ bd (3)"), "{text}");
            assert!(text.contains("bd:2"), "{text}");
            assert!(text.contains("1 from this score"), "{text}");
        }

        /// Conhost draws `▾` as tofu, so a terminal without the capability
        /// gets the stand-in instead - and never the fancy glyph, which is
        /// what would show up as tofu on the very console this is guarding.
        #[test]
        fn the_samples_tab_open_marker_falls_back_without_the_capability() {
            let _forced = super::super::super::terminal::ForceSymbolsForTest::set(false);
            let reference = Reference::load_all();
            let sounds = vec![SoundEntry {
                name: "bd".into(),
                variants: 3,
                variant_names: Vec::new(),
                origin: SoundOrigin::Default,
                category: SoundCategory::Other,
                location: None,
                import: None,
            }];
            let mut drawn = ReferencePanel::samples(&reference, sounds);
            drawn.expand();
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 50, 12);
            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                reference: &reference,
                panel: &drawn,
                theme: &theme,
            }
            .render(area, &mut buffer);
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("v bd (3)"), "{text}");
            assert!(!text.contains('▾'), "{text}");
        }

        /// Bank rows alias the bank; remote variants and headings cannot
        /// rename a local file.
        #[test]
        fn a_single_local_sample_expands_and_renames_the_file_instead_of_its_bank() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::samples(
                &reference,
                vec![SoundEntry {
                    name: "takes".into(),
                    variants: 1,
                    variant_names: vec!["take001".into()],
                    origin: SoundOrigin::Global,
                    category: SoundCategory::Mine,
                    location: Some("file:///music/takes/take001.wav".into()),
                    import: Some("/music/takes".into()),
                }],
            );
            for (section, _) in panel.visible_categories() {
                panel.open_categories.insert(section);
            }
            panel.sound_selected = panel
                .sound_rows()
                .iter()
                .position(|row| matches!(row, SoundRow::Bank(_)))
                .unwrap();
            assert!(matches!(panel.rename(), PanelAction::RenameBank { .. }));
            assert!(
                panel.expand(),
                "a singleton local bank exposes its file row"
            );
            panel.move_by(1);
            assert_eq!(
                panel.rename(),
                PanelAction::RenameSample {
                    name: "takes".into(),
                    variant: 0
                }
            );
            panel.sound_query = "take001".into();
            panel.prepare_sample_rename("takes", 0, "vox1", "file:///music/takes/vox1.wav");
            assert_eq!(
                panel.sound_query, "vox1",
                "an exact filename search follows its rename"
            );
            assert_eq!(panel.sounds[0].variant_label(0), "takes:0 (vox1)");
            panel.sound_query = "vox1".into();
            panel.refresh_sounds();
            assert_eq!(panel.sound_results, [0], "the filename also finds its bank");
            assert_eq!(
                panel.sounds.len(),
                1,
                "no extra filename bank is introduced"
            );
        }

        #[test]
        fn alt_r_names_the_bank_under_the_cursor_and_nothing_else() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            panel.set_sounds(vec![
                SoundEntry {
                    name: "bd".into(),
                    variants: 2,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Global,
                    category: SoundCategory::Drums,
                    location: None,
                    import: None,
                },
                SoundEntry {
                    name: "shaker".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,
                    category: SoundCategory::Percussion,
                    location: None,
                    import: None,
                },
            ]);
            assert_eq!(
                panel.rename(),
                PanelAction::Nothing,
                "not on the samples tab yet"
            );
            panel.toggle_tab();
            // Two kinds of sound group into headings, folded until opened: the
            // row under the cursor at first is a heading, not a bank.
            assert_eq!(
                panel.rename(),
                PanelAction::Nothing,
                "a heading is not a bank"
            );
            panel
                .open_categories
                .insert(SoundSection::Kind(SoundCategory::Drums));
            panel.sound_selected = 1;
            assert_eq!(
                panel.rename(),
                PanelAction::RenameBank {
                    name: "bd".into(),
                    origin: SoundOrigin::Global,
                },
                "a bank row hands back its name and where it came from"
            );
            assert!(panel.expand(), "bd opens onto its variants");
            panel.move_by(1);
            assert_eq!(
                panel.rename(),
                PanelAction::Nothing,
                "a remote variant cannot rename a local file"
            );

            panel.prepare_sound_rename("bd", "vox1");
            panel.set_sounds(vec![
                SoundEntry {
                    name: "vox1".into(),
                    variants: 2,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Global,
                    category: SoundCategory::Drums,
                    location: None,
                    import: None,
                },
                SoundEntry {
                    name: "shaker".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,
                    category: SoundCategory::Percussion,
                    location: None,
                    import: None,
                },
            ]);
            assert_eq!(
                panel.selected_bank().map(|(name, _)| name),
                Some("vox1".to_owned())
            );
            assert!(panel.expanded.is_some(), "the renamed bank stays expanded");

            panel.prepare_sound_rename("vox1", "vox2");
            panel.set_sounds(vec![
                SoundEntry {
                    name: "vox2".into(),
                    variants: 2,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Global,
                    category: SoundCategory::Drums,
                    location: None,
                    import: None,
                },
                SoundEntry {
                    name: "shaker".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,
                    category: SoundCategory::Percussion,
                    location: None,
                    import: None,
                },
            ]);
            assert_eq!(
                panel.selected_bank().map(|(name, _)| name),
                Some("vox2".to_owned())
            );
            assert!(panel.expanded.is_some(), "the second alias stays expanded");
        }
    }
    mod search {
        use super::super::*;

        /// `resolve` spells a name exactly first, so a chord symbol survives -
        /// `+`, `-`, `^7` - before falling back to the trimmed lookup for a
        /// prose-like word. The CLI's `doc` command resolves the same way.
        #[test]
        fn resolve_reads_a_chord_symbol_exactly_and_a_word_leniently() {
            let reference = Reference::load_all();
            // Exact symbols survive: lookup's trimming would erase them.
            assert!(reference.lookup("+").is_none(), "lookup trims `+` away");
            assert!(reference.resolve("+").is_some(), "resolve keeps `+`");
            assert!(reference.resolve("^7").is_some());
            assert!(reference.resolve("-").is_some());
            // Words fall back to the lenient lookup, case and synonyms included.
            let lpf = reference.lookup("lpf").expect("lpf");
            assert_eq!(reference.resolve("lpf"), Some(lpf));
            assert_eq!(reference.resolve("LPF"), Some(lpf));
            assert_eq!(reference.resolve("cutoff"), Some(lpf), "a synonym resolves");
            assert_eq!(reference.resolve("definitely_not_a_function"), None);
        }

        /// The name of the entry a search ranks first.
        fn first_result<'a>(reference: &'a Reference, query: &str) -> &'a str {
            &reference.entry(reference.search(query)[0]).unwrap().name
        }

        #[test]
        fn a_misspelt_name_searches_to_its_nearest_real_one() {
            let reference = Reference::load_all();
            assert_eq!(first_result(&reference, "lpff"), "lpf");
            assert_eq!(first_result(&reference, "gian"), "gain");
            assert_eq!(first_result(&reference, "gain"), "gain");
            let panel = ReferencePanel::browse_for(&reference, "lpff");
            assert_eq!(panel.query, "lpff");
            assert_eq!(reference.entry(panel.results[0]).unwrap().name, "lpf");
        }

        #[test]
        fn mouse_signal_search_explains_the_missing_input() {
            let reference = Reference::load(engine_knows());
            for name in ["mousex", "mouseX", "mousey", "mouseY"] {
                let index = reference.lookup(name).expect("mouse signal entry");
                assert_eq!(reference.search(name).first(), Some(&index), "{name}");
                let entry = reference.entry(index).unwrap();
                assert!(entry.summary.contains("not supported"), "{name}");
                let body = entry_body(entry, 80)
                    .into_iter()
                    .map(|line| line.text)
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(body.contains("always returns 0"), "{name}: {body}");
                assert!(
                    body.contains("does not track the pointer"),
                    "{name}: {body}"
                );
            }
        }

        /// Scoring folds case, so `s` and `S` tie; the spelling typed wins.
        #[test]
        fn a_case_only_tie_ranks_the_spelling_typed_first() {
            let reference = Reference::load_all();
            assert_eq!(first_result(&reference, "s"), "s");
            assert_eq!(first_result(&reference, "S"), "S");
        }

        #[test]
        fn the_compiled_reference_parses_and_finds_names_and_synonyms() {
            let reference = Reference::load_all();
            assert!(reference.len() > 300, "{} entries", reference.len());
            let lpf = reference.lookup("lpf").expect("lpf");
            assert_eq!(
                reference.lookup("cutoff"),
                Some(lpf),
                "a synonym finds the entry"
            );
            assert_eq!(
                reference.lookup("LPF"),
                Some(lpf),
                "case-insensitive fallback"
            );
            assert_eq!(reference.lookup("\"lpf\""), Some(lpf), "quotes are trimmed");
            assert!(reference.lookup("definitely_not_a_function").is_none());
            let entry = reference.entry(lpf).unwrap();
            assert!(!entry.examples.is_empty());
            assert!(entry.summary.contains("low-pass") || entry.summary.contains("l**ow"));
        }

        /// "Show me every visualizer" is a set, not a ranking: `pianoroll` and
        /// `spiral` share no letters, so no amount of fuzzy scoring gathers them.
        /// A `tag:` word is how the question gets asked.
        #[test]
        fn a_tag_word_filters_the_list_to_one_kind_and_composes_with_a_search() {
            let reference = Reference::load(engine_knows());
            let names = |query: &str| {
                reference
                    .search(query)
                    .into_iter()
                    .filter_map(|index| reference.entry(index))
                    .map(|entry| entry.name.clone())
                    .collect::<Vec<_>>()
            };

            let visualizers = names("tag:visualization");
            assert!(
                visualizers.contains(&"pianoroll".to_owned()),
                "the tag must gather the visualizers: {visualizers:?}"
            );
            assert!(
                visualizers.len() >= 8 && visualizers.len() < reference.len() / 4,
                "a filter that returns everything or nothing is not a filter: {}",
                visualizers.len()
            );
            for name in &visualizers {
                let entry = reference
                    .entry(reference.lookup(name).expect("a listed name resolves"))
                    .expect("entry");
                assert!(
                    entry.tags.iter().any(|tag| tag == "visualization"),
                    "{name} is not tagged visualization"
                );
            }

            // Typed one letter at a time, so a prefix has to work before the
            // whole word does.
            assert_eq!(
                names("tag:visualiz"),
                visualizers,
                "a prefix names the same tag"
            );
            assert_eq!(
                names("tag:VISUALIZATION"),
                visualizers,
                "the tag is not case-sensitive"
            );

            // A prefix short enough to reach two tags reaches both, on purpose:
            // strudel.cc groups its visualizers under `visualization` and this
            // engine tags its own Hydra additions `visuals`, and a reader typing
            // "vis" wants the pictures, not one vocabulary's word for them.
            let short = names("tag:vis");
            assert!(
                visualizers.iter().all(|name| short.contains(name))
                    && short.len() > visualizers.len(),
                "a shorter prefix must widen the field, not change it: {short:?}"
            );

            // A tag and a word compose: the visualizers that look like `piano`.
            let both = names("tag:vis piano");
            assert!(
                both.contains(&"pianoroll".to_owned()) && both.len() < visualizers.len(),
                "a word must narrow the tag rather than replace it: {both:?}"
            );

            // A tag nothing carries is empty rather than unfiltered - the
            // failure a reader can see and correct.
            assert!(
                names("tag:nosuchtag").is_empty(),
                "an unknown tag must not fall back to matching everything"
            );

            // And a query with no tag word is untouched.
            assert_eq!(
                names("pianoroll").first().map(String::as_str),
                Some("pianoroll"),
                "an ordinary search still ranks the exact name first"
            );

            let tags = reference.tags();
            assert!(
                tags.iter()
                    .any(|(tag, count)| *tag == "visualization" && *count > 0),
                "the tag menu is read off the entries: {tags:?}"
            );
            assert!(
                tags.windows(2).all(|pair| pair[0].1 >= pair[1].1),
                "most-used first, so the menu leads with what is worth filtering"
            );
        }

        #[test]
        fn search_ranks_prefix_matches_first_and_hides_nothing_from_the_list() {
            let reference = Reference::load_all();
            let names_for = |query: &str| {
                reference
                    .search(query)
                    .iter()
                    .map(|index| reference.entry(*index).unwrap().name.clone())
                    .collect::<Vec<_>>()
            };
            let names = names_for("fa");
            let prefixed = names
                .iter()
                .take_while(|name| name.to_lowercase().starts_with("fa"))
                .count();
            assert!(prefixed >= 3, "{names:?}");
            assert!(names.contains(&"fastGap".to_owned()), "{names:?}");
            // The exact name beats the longer names that start with it.
            assert_eq!(names_for("fast")[0], "fast");
            assert_eq!(names_for("cutoff")[0], "lpf", "an exact synonym wins too");
            assert_eq!(reference.search("").len(), reference.len());
            assert!(reference.search("zzzzqqq").is_empty());
        }

        #[test]
        fn underscore_search_discovers_inline_painters_without_one_letter_typo_matches() {
            let reference = Reference::load(engine_knows());
            let results = reference.search("_");
            assert!(!results.is_empty());
            assert!(
                results
                    .iter()
                    .all(|&index| reference.entry(index).unwrap().name.starts_with('_'))
            );
            for method in rustel_transpiler::VISUAL_WIDGET_METHODS
                .iter()
                .filter(|name| name.starts_with('_'))
            {
                let index = reference.lookup(method).expect("documented inline painter");
                assert!(
                    results.contains(&index),
                    "{method} missing from underscore search"
                );
            }
            for (query, wanted) in [
                ("_piano", "_pianoroll"),
                ("_PIANOROLL", "_pianoroll"),
                ("_pianorol", "_pianoroll"),
                ("_tscope", "_scope"),
                ("tag:vis _piano", "_pianoroll"),
                ("pianoroll", "pianoroll"),
                ("scope", "scope"),
                ("cutoff", "lpf"),
            ] {
                let matches = reference.search(query);
                assert_eq!(reference.entry(matches[0]).unwrap().name, wanted, "{query}");
            }
            let unavailable = Reference::load(|name| name == "pianoroll");
            assert!(
                unavailable.search("_").is_empty(),
                "only installed inline names are offered"
            );
        }

        #[test]
        fn inline_search_rows_and_completion_keep_the_underscore() {
            let reference = Reference::load(engine_knows());
            let mut panel = ReferencePanel::browse_for(&reference, "_piano");
            assert_eq!(
                panel.confirm(&reference),
                PanelAction::Insert("_pianoroll".into())
            );

            let panel = ReferencePanel::browse_for(&reference, "_");
            let area = Rect::new(0, 0, 80, 26);
            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                reference: &reference,
                panel: &panel,
                theme: &Theme::built_in_default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                caching: 0,
                importing: 0,
                library_loading: false,
            }
            .render(area, &mut buffer);
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("_pianoroll"), "{text}");
            assert!(text.contains("_spectrum"), "{text}");
            assert!(
                !text.contains('▏'),
                "the terminal cursor is the only search caret: {text}"
            );
            let inner = inner_area(area);
            assert_eq!(panel.search_cursor(inner), Some((inner.x + 9, inner.y + 1)));
        }

        #[test]
        fn the_list_shows_camel_spellings_and_keeps_lowercase_as_aliases() {
            let reference = Reference::load(engine_knows());
            let index = reference.lookup("setcpm").expect("setcpm still resolves");
            let entry = reference.entry(index).unwrap();
            assert_eq!(entry.name, "setCpm", "the list suggests the camel spelling");
            assert!(
                entry.synonyms.iter().any(|synonym| synonym == "setcpm"),
                "the documented lowercase stays visible as an alias: {:?}",
                entry.synonyms
            );
            // And no listed name is the lowercase fold of a camel spelling the
            // engine also answers to.
            for index in 0..reference.len() {
                let name = &reference.entry(index).unwrap().name;
                if let Some(camel) = canonical_spellings().get(name.as_str()) {
                    assert_eq!(camel, name, "{name} should be listed as {camel}");
                }
            }
        }

        /// `scale("c:major pentatonic")` is two mini-notation steps, the second
        /// of them a scale with no root; the list offers the form that plays.
        #[test]
        fn scale_names_are_offered_with_colons_not_spaces() {
            let names = scale_vocabulary();
            assert!(names.iter().all(|name| !name.contains(' ')), "{names:?}");
            assert!(
                names.iter().any(|name| name == "major:pentatonic"),
                "{names:?}"
            );
            assert!(names.iter().any(|name| name == "bebop:major"), "{names:?}");
            assert!(
                rustel_core::tonaljs::get_scale("c:bebop:major").is_ok(),
                "and the engine accepts the offered form"
            );
        }

        /// The search a clumsy typist needs: the letters of the word, in
        /// order, with the nearest answer first - in every list, not just
        /// the functions.
        #[test]
        fn a_few_letters_find_the_word_in_every_list() {
            let reference = Reference::load_all();
            let names = |results: &[usize]| {
                results
                    .iter()
                    .filter_map(|&index| reference.entry(index))
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>()
            };
            // `chrd` is an extension name that starts with the query, so it
            // leads; `chord`, which only holds the letters in order, follows
            // it rather than the hundred names that hold them further apart.
            let results = reference.search("chr");
            let top = names(&results);
            assert!(
                top.iter().take(3).any(|name| *name == "chord"),
                "{:?}",
                top.iter().take(5).collect::<Vec<_>>()
            );
            assert!(
                top.first().is_some_and(|name| name.starts_with("chr")),
                "{:?}",
                top.iter().take(5).collect::<Vec<_>>()
            );
            let results = reference.search("smtms");
            assert!(
                names(&results)
                    .iter()
                    .take(3)
                    .any(|name| name.starts_with("sometimes")),
                "{:?}",
                names(&results).iter().take(5).collect::<Vec<_>>()
            );

            // Sounds: the same matcher, so `sprsw` finds `supersaw`.
            let sound = |name: &str| SoundEntry {
                name: name.into(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Synth,
                category: SoundCategory::Synth,
                location: None,
                import: None,
            };
            let mut panel = ReferencePanel::samples(
                &reference,
                vec![sound("supersaw"), sound("sawtooth"), sound("sine")],
            );
            panel.sound_query = "sprsw".into();
            panel.refresh_sounds();
            assert_eq!(panel.sound_results.first().copied(), Some(0));

            // Chords, by name or by symbol, however roughly typed.
            let mut chords = ReferencePanel::browse(&reference);
            chords.tab = Tab::Chords;
            chords.chord_query = "mnr7".into();
            assert_eq!(
                chords
                    .chord_qualities()
                    .first()
                    .map(|quality| quality.symbol),
                Some("-7")
            );

            // Scales, through the vocabulary the completion opens.
            let scales = ReferencePanel::vocabulary_for(
                &reference,
                "scales",
                scale_vocabulary(),
                "pentatnic",
            );
            assert!(
                scales
                    .results
                    .first()
                    .and_then(|&index| scales.vocabulary.as_ref()?.names.get(index))
                    .is_some_and(|name| name.contains("pentatonic")),
                "a slip in a scale name still finds it"
            );
        }

        #[test]
        fn half_a_name_still_finds_its_family() {
            let reference = Reference::load_all();
            // No name contains `someto` and every one is too many edits away;
            // the shared `somet` prefix must still surface the family.
            let results = reference.search("someto");
            assert!(
                !results.is_empty(),
                "a near-miss query must not come up empty"
            );
            let names: Vec<&str> = results
                .iter()
                .take(8)
                .filter_map(|&index| reference.entry(index))
                .map(|entry| entry.name.as_str())
                .collect();
            assert!(
                names.iter().any(|name| name.starts_with("sometimes")),
                "{names:?}"
            );
        }

        /// A search box takes a whole paste as one edit, holds a bounded
        /// amount of it, and empties in one gesture.
        #[test]
        fn a_search_box_pastes_once_stays_bounded_and_empties_in_one_press() {
            let reference = Reference::load_all();
            let entry = |name: &str| SoundEntry {
                name: name.into(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Global,
                category: SoundCategory::Mine,
                location: None,
                import: None,
            };
            let mut panel =
                ReferencePanel::samples(&reference, vec![entry("kick"), entry("snare_hard")]);

            let path = "/Users/EXAMPLE/Library/CloudStorage/ExampleDrive/Audio Projects/Samples/percussion";
            assert!(path.chars().count() > MAX_QUERY_CHARS);
            assert!(panel.paste_query(&reference, path));
            assert_eq!(panel.sound_query.chars().count(), MAX_QUERY_CHARS);
            assert!(path.starts_with(panel.sound_query.as_str()));
            assert!(panel.sound_results.is_empty(), "and it matched nothing");

            // A full box has nothing left to narrow, and the keystroke is
            // still the panel's rather than the score's.
            assert!(panel.type_char(&reference, 'x'));
            assert_eq!(panel.sound_query.chars().count(), MAX_QUERY_CHARS);

            assert!(panel.clear_query(&reference));
            assert_eq!(panel.sound_query, "");
            assert_eq!(panel.sound_results.len(), 2, "the whole list is back");
            assert!(!panel.clear_query(&reference), "nothing left to clear");

            // Every tab with a box behaves the same, and the shelf has none.
            for tab in [Tab::Chords, Tab::Scales, Tab::Reference] {
                panel.tab = tab;
                assert!(panel.paste_query(&reference, "min"));
                assert!(panel.clear_query(&reference));
            }
            #[cfg(feature = "hydra")]
            {
                panel.tab = Tab::Examples;
                assert!(!panel.paste_query(&reference, "min"));
                assert!(!panel.clear_query(&reference));
            }
        }

        /// A hidden category leaves the list. Its `tag:` and its names still
        /// reach the entries.
        #[test]
        fn hidden_categories_leave_search_but_not_lookup() {
            let mut reference = Reference::load_all();
            let names = |reference: &Reference, query: &str| -> Vec<String> {
                reference
                    .search(query)
                    .into_iter()
                    .map(|index| reference.entry(index).unwrap().name.clone())
                    .collect()
            };
            let everything = reference.search("").len();
            assert_eq!(reference.hidden_len(), 0);
            for name in ["squiz", "osc", "fadeInTime", "oschost", "serial"] {
                assert!(names(&reference, "").contains(&name.to_owned()), "{name}");
            }

            reference.set_hidden(Category::ALL);
            let listed = names(&reference, "");
            for name in ["squiz", "osc", "fadeInTime", "oschost", "serial"] {
                assert!(!listed.contains(&name.to_owned()), "{name} is hidden");
                assert!(reference.lookup(name).is_some(), "{name} still resolves");
            }
            assert!(listed.contains(&"lpf".to_owned()), "native controls stay");
            assert!(
                listed.contains(&"dry".to_owned()),
                "native superdirt-tagged stay"
            );
            assert_eq!(listed.len() + reference.hidden_len(), everything);
            assert!(!names(&reference, "squiz").contains(&"squiz".to_owned()));
            assert!(names(&reference, "tag:osc").contains(&"squiz".to_owned()));
            assert!(names(&reference, "tag:serial").contains(&"serial".to_owned()));
            assert!(
                !names(&reference, "tag:control").contains(&"fadeInTime".to_owned()),
                "another tag does not bring a hidden entry back"
            );

            for name in ["fmi12", "fmi88", "bind", "innerJoin", "appLeft", "withHap"] {
                assert!(!listed.contains(&name.to_owned()), "{name} is hidden");
            }
            for name in ["fmi", "fmi2", "fmh8", "set", "keep", "withValue", "stack"] {
                assert!(listed.contains(&name.to_owned()), "{name} stays");
            }
            assert!(names(&reference, "tag:fm_matrix").contains(&"fmi12".to_owned()));
            assert!(names(&reference, "tag:bind").contains(&"squeezeJoin".to_owned()));
            assert!(names(&reference, "tag:internals").contains(&"withHap".to_owned()));

            reference.set_hidden([Category::Serial]);
            assert!(names(&reference, "").contains(&"squiz".to_owned()));
            assert!(!names(&reference, "").contains(&"serial".to_owned()));
        }

        /// A control only SuperDirt plays carries `superdirt` and `osc`.
        #[test]
        fn superdirt_entries_are_osc_entries() {
            let reference = Reference::load_all();
            for index in 0..reference.len() {
                let entry = reference.entry(index).unwrap();
                if entry.tags.iter().any(|tag| tag == "superdirt") {
                    assert!(Category::Osc.files(entry), "{} lacks osc", entry.name);
                }
            }
        }
    }
    mod snippets {
        use super::super::*;

        /// Snippet events use the theme's mark style and fade toward the panel
        /// surface, which can differ from the editor background.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_snippet_mark_wears_the_themes_mark_on_the_panels_ground() {
            use super::super::*;
            use ratatui::style::{Color, Modifier};

            let reference = Reference::load_all();
            let mut theme = Theme::built_in_default();
            assert_ne!(
                theme.surface, theme.background,
                "the default theme has to tell a surface from a background for this to mean anything"
            );
            theme.event_mark = super::super::super::theme::MarkStyle::Outline;
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Examples;
            // Open the first shelf that holds a music snippet and land on it.
            let (section, shelf) = super::super::super::examples::SECTIONS
                .iter()
                .enumerate()
                .find_map(|(index, section)| {
                    (section.kind == super::super::super::examples::Kind::Music)
                        .then_some((index, 0))
                })
                .expect("a music section");
            panel.section_open.insert(section);
            panel.snippet_open.insert((section, shelf));
            panel.snippet_selected = panel
                .snippet_lines()
                .iter()
                .position(|line| matches!(line, SnippetLine::Snippet(..)))
                .expect("a snippet row");
            let code = panel
                .selected_snippet_code()
                .expect("the row has code")
                .into_owned();
            let first_line_len = code.lines().next().unwrap_or("").len();
            assert!(first_line_len >= 4, "a line to mark: {code:?}");
            let mark_color = Color::Rgb(250, 40, 170);
            let marks = [super::super::super::visuals::SourceMark {
                from: 0,
                to: first_line_len.min(8),
                color: mark_color,
                onset_id: 1,
                strength: 1.0,
            }];

            let area = Rect::new(0, 0, 60, 24);
            let render = |theme: &Theme, marks: &[super::super::super::visuals::SourceMark]| {
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    playing: Some((code.as_str(), marks)),
                    preview_note: None,
                    #[cfg(feature = "hydra")]
                    preview_progress: None,
                    #[cfg(feature = "hydra")]
                    preview_row: None,
                    picture: false,
                    refused: None,
                    reference: &reference,
                    panel: &panel,
                    theme,
                }
                .render(area, &mut buffer);
                buffer
            };
            let rows = panel.snippet_layout(inner_area(area)).code_rows();
            let first_row = |buffer: &Buffer| -> Vec<Style> {
                (rows.x..rows.right())
                    .map(|x| buffer.cell((x, rows.y)).expect("a cell").style())
                    .collect()
            };

            // An outline puts the event colour on the text and
            // under it, nothing filled - the syntax colour beneath is what the
            // outline was chosen to spare.
            let buffer = render(&theme, &marks);
            let styles = first_row(&buffer);
            let marked: Vec<&Style> = styles
                .iter()
                .take(marks[0].to)
                .filter(|style| style.fg == Some(mark_color))
                .collect();
            assert!(!marked.is_empty(), "the mark reached the row: {styles:?}");
            assert!(
                marked
                    .iter()
                    .all(|style| style.add_modifier.contains(Modifier::UNDERLINED)),
                "an outline underlines: {marked:?}"
            );
            assert!(
                styles
                    .iter()
                    .all(|style| style.bg != Some(theme.background)),
                "no cell of the panel ends on the editor's background: {styles:?}"
            );

            // A fill is the event colour behind text the panel can read - the
            // panel's surface, not the editor's background.
            theme.event_mark = super::super::super::theme::MarkStyle::Fill;
            let buffer = render(&theme, &marks);
            let styles = first_row(&buffer);
            let filled: Vec<&Style> = styles
                .iter()
                .filter(|style| style.bg == Some(mark_color))
                .collect();
            assert!(!filled.is_empty(), "the fill reached the row: {styles:?}");
            assert!(
                filled.iter().all(|style| style.fg == Some(theme.surface)),
                "a fill reads its text colour from the panel's ground: {filled:?}"
            );

            // A mark letting go eases back towards the panel's ground, never
            // towards the editor's.
            let fading = [super::super::super::visuals::SourceMark {
                strength: 0.5,
                ..marks[0]
            }];
            let buffer = render(&theme, &fading);
            let styles = first_row(&buffer);
            assert!(
                styles
                    .iter()
                    .all(|style| style.bg != Some(theme.background)),
                "a fading mark never lands on the editor's background: {styles:?}"
            );
        }

        /// The preview's playhead strip is eight cells just left of the note
        /// tag. The fill moves left to right through the bar and shows the
        /// position of the pattern in the music.
        #[cfg(feature = "hydra")]
        #[test]
        fn the_preview_row_draws_a_playhead_strip_through_the_bar() {
            use super::super::*;

            let reference = Reference::load_all();
            let theme = Theme::built_in_default();
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Examples;
            // Open the first shelf that holds a music snippet and land on it.
            let (section, shelf) = super::super::super::examples::SECTIONS
                .iter()
                .enumerate()
                .find_map(|(index, section)| {
                    (section.kind == super::super::super::examples::Kind::Music)
                        .then_some((index, 0))
                })
                .expect("a music section");
            panel.section_open.insert(section);
            panel.snippet_open.insert((section, shelf));
            let selected = panel
                .snippet_lines()
                .iter()
                .position(|line| matches!(line, SnippetLine::Snippet(..)))
                .expect("a snippet row");
            panel.snippet_selected = selected;
            let row = SnippetLine::Snippet(section, shelf, 0);

            let area = Rect::new(0, 0, 60, 24);
            // The sounding preview: the strip alone - no word beside it.
            let render = |progress: Option<f32>| {
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    playing: None,
                    preview_note: None,
                    preview_row: Some(Some(row)),
                    preview_progress: progress,
                    picture: false,
                    refused: None,
                    reference: &reference,
                    panel: &panel,
                    theme: &theme,
                }
                .render(area, &mut buffer);
                buffer
            };
            // The strip rides the row the note tag is on: the same y the list
            // itself scrolls that row to.
            let list_area = panel.snippet_layout(inner_area(area)).list;
            let first = panel.first_visible_row(list_area.height);
            let offset = panel
                .snippet_lines()
                .iter()
                .skip(first)
                .position(|line| *line == row)
                .expect("the selected row is on screen");
            let row_y = list_area.y + offset as u16;
            let strip_row = |buffer: &Buffer| -> String {
                (0..area.width)
                    .map(|x| {
                        buffer
                            .cell((x, row_y))
                            .map(|cell| cell.symbol().to_owned())
                            .unwrap_or_default()
                    })
                    .collect()
            };

            // Half through the bar: half the strip is filled.
            let buffer = render(Some(0.5));
            let text = strip_row(&buffer);
            let filled = text.matches('▓').count();
            let empty = text.matches('░').count();
            assert_eq!(filled, 4, "half of eight: {text:?}");
            assert_eq!(empty, 4, "the rest waits: {text:?}");

            // Just started: nothing filled yet.
            let text = strip_row(&render(Some(0.0)));
            assert_eq!(text.matches('▓').count(), 0, "{text:?}");

            // The whole bar: the strip is full.
            let text = strip_row(&render(Some(1.0)));
            assert_eq!(text.matches('▓').count(), 8, "{text:?}");

            // No bar position - the strip says nothing at all.
            let text = strip_row(&render(None));
            assert!(!text.contains('▓'), "{text:?}");
            assert!(!text.contains('░'), "{text:?}");
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn the_code_container_keeps_its_rows_inside_the_border_and_below_the_tree() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            for width in [3, 6, 36, 44, 60, 72] {
                for height in 0..=48 {
                    let inner = inner_area(Rect::new(1, 2, width, height));
                    for tab in [Tab::Examples, Tab::Generator] {
                        panel.tab = tab;
                        let layout = panel.snippet_layout(inner);
                        let rows = layout.code_rows();
                        assert!(layout.list.bottom() <= layout.code.y);
                        assert!(layout.code.bottom() <= layout.footer);
                        if !rows.is_empty() {
                            assert_eq!(rows.x, layout.code.x + 1);
                            assert_eq!(rows.y, layout.code.y + 1);
                            assert_eq!(rows.right() + 1, layout.code.right());
                            assert_eq!(rows.bottom() + 1, layout.code.bottom());
                        }
                        for area in [layout.list, layout.code, rows] {
                            if !area.is_empty() {
                                assert_eq!(
                                    area.intersection(inner),
                                    area,
                                    "{tab:?} {width}x{height}"
                                );
                            }
                        }
                    }
                }
            }
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn music_hydra_and_generated_code_render_in_highlighted_containers() {
            let reference = Reference::load_all();
            let theme = Theme::built_in_default();
            let call_colour = theme.accent;
            assert_ne!(call_colour, theme.syntax.text);
            let area = Rect::new(0, 0, 72, 40);
            for kind in [
                Some(super::super::super::examples::Kind::Music),
                Some(super::super::super::examples::Kind::Hydra),
                None,
            ] {
                let mut panel = ReferencePanel::browse(&reference);
                if let Some(kind) = kind {
                    panel.select_tab(Tab::Examples);
                    let section = super::super::super::examples::section_of(kind).unwrap();
                    panel.section_open.insert(section);
                    panel.snippet_open.insert((section, 0));
                    panel.snippet_selected = panel
                        .snippet_lines()
                        .iter()
                        .position(|line| *line == SnippetLine::Snippet(section, 0, 0))
                        .unwrap();
                } else {
                    panel.select_tab(Tab::Generator);
                    panel.generator = super::super::super::ideas::Generator::seeded(41);
                }
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    focused: true,
                    pulse: None,
                    sounding_note: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    playing: None,
                    preview_note: None,
                    preview_progress: None,
                    preview_row: None,
                    picture: true,
                    refused: None,
                    reference: &reference,
                    panel: &panel,
                    theme: &theme,
                }
                .render(area, &mut buffer);
                let layout = panel.snippet_layout(inner_area(area));
                assert_eq!(
                    buffer
                        .cell((layout.code.x, layout.code.y))
                        .unwrap()
                        .symbol(),
                    "╭"
                );
                assert_eq!(
                    buffer
                        .cell((layout.code.x, layout.code.bottom() - 1))
                        .unwrap()
                        .symbol(),
                    "╰"
                );
                let rows = layout.code_rows();
                assert!(
                    (rows.y..rows.bottom()).any(|y| (rows.x..rows.right()).any(|x| buffer
                        .cell((x, y))
                        .unwrap()
                        .fg
                        == call_colour)),
                    "{kind:?} highlights function calls"
                );
            }
        }

        /// A chain wider than the column is drawn over several rows, and a
        /// drag is held in the rows it is drawn in. Read as logical lines
        /// instead, the second row of a wrapped chain was taken for the
        /// snippet's second line: the highlight landed a row below the
        /// pointer and `c` copied a different line's text.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_drag_over_a_wrapped_chain_takes_the_row_it_is_on() {
            use crate::textblock::{TextPoint, TextSelection};
            let reference = Reference::load(engine_knows());
            let inner = inner_area(Rect::new(0, 0, 38, 40));
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Examples;
            panel.section_open.insert(1);
            panel.snippet_open.insert((1, 0));
            panel.snippet_selected = panel
                .snippet_lines()
                .iter()
                .position(|line| *line == SnippetLine::Snippet(1, 0, 0))
                .unwrap();
            let rows = panel.snippet_layout(inner).code_rows();
            let room = usize::from(rows.height);
            let width = usize::from(rows.width);
            let lines = panel.snippet_code_lines(room, width);
            let spans = panel.snippet_code_rows(room, width);
            let code = panel
                .selected_snippet_code()
                .expect("the example row has code")
                .into_owned();
            let logical = code.lines().count();
            assert!(
                lines.len() > logical,
                "a column this narrow breaks the chains: {logical} lines over {} rows",
                lines.len()
            );
            for (row, drawn) in lines.iter().enumerate().take(4).skip(1) {
                panel.selection = Some(PaneSelection {
                    target: SelectionTarget::Snippet {
                        row: panel.snippet_selected,

                        width: rows.width,
                    },
                    selection: TextSelection {
                        anchor: TextPoint {
                            line: row,
                            column: 0,
                        },
                        head: TextPoint {
                            line: row,
                            column: drawn.chars().count(),
                        },
                    },
                });
                // The row's own bytes out of the line it is part of. A
                // continuation row is drawn with an indent that is display
                // alone, so this is not the string the screen shows.
                let (index, span) = &spans[row];
                let source = code.lines().nth(*index).expect("the row's own line");
                assert_eq!(
                    panel.live_selection_text(&reference, inner).as_deref(),
                    Some(&source[span.from..span.to]),
                    "row {row} copies the bytes it shows, not the {drawn:?} drawn for them"
                );
            }
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn scrolling_wrapped_generator_code_keeps_drag_copy_on_the_visible_source() {
            use crate::textblock::{TextPoint, TextSelection};
            let reference = Reference::load(engine_knows());
            let inner = inner_area(Rect::new(0, 0, 38, 40));
            let mut panel = ReferencePanel::browse(&reference);
            panel.select_tab(Tab::Generator);
            panel.generator = super::super::super::ideas::Generator::seeded(41);
            let rows = panel.snippet_layout(inner).code_rows();
            let room = usize::from(rows.height);
            let width = usize::from(rows.width);
            let code = panel.selected_snippet_code().unwrap().into_owned();
            let all = panel.all_snippet_code_rows(width);
            let last = all.len() - room;
            for offset in [1, last / 2, last] {
                panel.snippet_code_scroll.set(offset);
                let lines = panel.snippet_code_lines(room, width);
                let screen = 1;
                let (logical, span) = &all[offset + screen];
                panel.selection = Some(PaneSelection {
                    target: SelectionTarget::Snippet {
                        row: panel.snippet_selected,
                        width: rows.width,
                    },
                    selection: TextSelection {
                        anchor: TextPoint {
                            line: screen,
                            column: 0,
                        },
                        head: TextPoint {
                            line: screen,
                            column: lines[screen].chars().count(),
                        },
                    },
                });
                let source = code.lines().nth(*logical).unwrap();
                assert_eq!(
                    panel.live_selection_text(&reference, inner).as_deref(),
                    Some(&source[span.from..span.to])
                );
                panel.scroll_snippet_code(inner, 1);
                assert!(
                    panel.selection.is_none(),
                    "scrolling invalidates screen-based selections"
                );
            }
            panel.scroll_snippet_code(inner, isize::MAX);
            assert_eq!(panel.snippet_code_scroll.get(), last);
            let (rail, top, length, _) = panel.snippet_scrollbar(inner).unwrap();
            assert_eq!(
                top + length,
                rail.height,
                "the thumb reaches the exact bottom"
            );
            panel.scroll_snippet_code(inner, isize::MIN);
            assert_eq!(panel.snippet_code_scroll.get(), 0);
        }

        /// A drag over every row of one wrapped line copies the source line
        /// whole. Rows joined as drawn would put a newline and the
        /// continuation indent inside a mini-notation string.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_drag_over_every_row_of_a_line_copies_the_line_whole() {
            use crate::textblock::{TextPoint, TextSelection};
            let reference = Reference::load(engine_knows());
            let inner = inner_area(Rect::new(0, 0, 38, 40));
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Examples;
            panel.section_open.insert(1);
            panel.snippet_open.insert((1, 0));
            panel.snippet_selected = panel
                .snippet_lines()
                .iter()
                .position(|line| *line == SnippetLine::Snippet(1, 0, 0))
                .unwrap();
            let rows = panel.snippet_layout(inner).code_rows();
            let room = usize::from(rows.height);
            let width = usize::from(rows.width);
            let lines = panel.snippet_code_lines(room, width);
            let spans = panel.snippet_code_rows(room, width);
            let code = panel
                .selected_snippet_code()
                .expect("the example row has code")
                .into_owned();
            // The first logical line this column broke over several rows.
            let start = (0..spans.len())
                .find(|&row| {
                    spans
                        .get(row + 1)
                        .is_some_and(|next| next.0 == spans[row].0)
                })
                .expect("a column this narrow breaks some chain");
            let index = spans[start].0;
            let mut last = start;
            while spans.get(last + 1).is_some_and(|next| next.0 == index) {
                last += 1;
            }
            assert!(last > start, "the line is drawn over several rows");

            panel.selection = Some(PaneSelection {
                target: SelectionTarget::Snippet {
                    row: panel.snippet_selected,

                    width: rows.width,
                },
                selection: TextSelection {
                    anchor: TextPoint {
                        line: start,
                        column: 0,
                    },
                    head: TextPoint {
                        line: last,
                        column: lines[last].chars().count(),
                    },
                },
            });
            let copied = panel
                .live_selection_text(&reference, inner)
                .expect("the drag holds something");
            assert_eq!(
                copied,
                code.lines().nth(index).expect("the line it was drawn from"),
                "the line, and not its rows joined"
            );
            assert!(
                !copied.contains('\n'),
                "nothing in one line was joined with a newline: {copied:?}"
            );
        }

        /// A resize re-wraps the rows a drag is held in, so the selection goes
        /// rather than following them. A selection that survived a re-wrap would
        /// be held in rows that mean other text now, and this feature's whole
        /// bug history is pointing at the wrong text without saying so; dropping
        /// it fails visibly, which a re-map that got it subtly wrong would not.
        /// The width tried is one that still draws the row, so what is tested is
        /// the drop and not the out-of-range guard.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_resize_drops_a_snippet_selection_rather_than_re_wrapping_it() {
            use crate::textblock::{TextPoint, TextSelection};
            let reference = Reference::load(engine_knows());
            let inner = inner_area(Rect::new(0, 0, 38, 40));
            let mut panel = ReferencePanel::browse(&reference);
            panel.tab = Tab::Examples;
            panel.snippet_open.insert((0, 0));
            panel.snippet_selected = 2;
            let rows = panel.snippet_layout(inner).code_rows();
            let lines = panel.snippet_code_lines(usize::from(rows.height), usize::from(rows.width));
            assert!(!lines.is_empty(), "the example row has code");
            panel.selection = Some(PaneSelection {
                target: SelectionTarget::Snippet {
                    row: panel.snippet_selected,

                    width: rows.width,
                },
                selection: TextSelection {
                    anchor: TextPoint { line: 0, column: 0 },
                    head: TextPoint {
                        line: 0,
                        column: lines[0].chars().count(),
                    },
                },
            });
            assert!(
                panel.live_selection_text(&reference, inner).is_some(),
                "the drag holds something at the width it was made"
            );

            // Narrower, but still drawing the row: the rows are re-wrapped and
            // mean other text, so the selection goes.
            let narrower = inner_area(Rect::new(0, 0, 30, 40));
            let rows = panel.snippet_layout(narrower).code_rows();
            assert!(
                !panel
                    .snippet_code_lines(usize::from(rows.height), usize::from(rows.width))
                    .is_empty(),
                "the shelf still draws rows at the new width, so only the resize drops it"
            );
            assert_eq!(
                panel.live_selection_text(&reference, narrower),
                None,
                "a resize drops the selection instead of re-wrapping it"
            );
        }

        /// The shelf has no search box, but its letters do not go to the score.
        /// A focused panel never writes to the editor. The only exception is
        /// the reference open on an entry.
        #[cfg(feature = "hydra")]
        #[test]
        fn the_snippet_shelf_does_not_type_through_to_the_score() {
            let reference = Reference::load(engine_knows());
            let shelf = ReferencePanel::snippets(&reference);
            assert!(!shelf.wants_text(), "the shelf has no search box");
            assert!(
                !shelf.types_through(),
                "and its letters are still its own, not the score's"
            );
            // Every other view in the column, for the same reason.
            for browse in [
                ReferencePanel::browse(&reference),
                ReferencePanel::snippets(&reference),
            ] {
                assert!(!browse.types_through());
            }
        }

        /// A snippet is in the same list as the functions, found by the same
        /// search, labelled, and pasted whole rather than inserted as a name:
        /// `gamepad` finds the function and, beside it, the lines that use it.
        #[test]
        fn a_snippet_is_searched_labelled_and_pasted_whole() {
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            for character in "gamepad".chars() {
                panel.type_char(&reference, character);
            }
            let names: Vec<&str> = panel
                .results
                .iter()
                .map(|&index| reference.entry(index).unwrap().name.as_str())
                .collect();
            assert_eq!(names[0], "gamepad", "the function first: {names:?}");
            let row = names
                .iter()
                .position(|name| *name == "gamepad setup")
                .unwrap_or_else(|| panic!("the snippet is in the list: {names:?}"));
            // `row` is a place in the results; the cursor counts rows, and a
            // grouped list has headings among them.
            panel.selected = panel.row_of_result(row);
            let entry = reference.entry(panel.results[row]).unwrap();
            assert!(entry.snippet.is_some());
            assert!(entry.tags.iter().any(|tag| tag == "snippet"));
            assert_eq!(entry.signature(), "gamepad setup", "a heading, not a call");
            match panel.confirm(&reference) {
                PanelAction::Paste(code) => {
                    assert!(code.starts_with("const gp = gamepad(0)"), "{code}");
                    assert!(code.contains('\n'), "whole, on lines of its own");
                }
                other => panic!("Enter pastes a snippet, not {other:?}"),
            }
            // → reads it first; Enter from the page pastes too.
            assert!(panel.expand());
            let lines = entry_body(entry, 60);
            assert!(
                lines.iter().any(|line| line.text.contains("Enter pastes")),
                "the page says what it is"
            );
            assert!(matches!(panel.confirm(&reference), PanelAction::Paste(_)));

            // The list draws the label on the row.
            let mut panel = ReferencePanel::browse(&reference);
            for character in "midi input".chars() {
                panel.type_char(&reference, character);
            }
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 70, 20);
            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                reference: &reference,
                panel: &panel,
                theme: &theme,
            }
            .render(area, &mut buffer);
            let drawn = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(drawn.contains("midi input"), "{drawn}");
            assert!(drawn.contains("snippet"), "labelled: {drawn}");

            // What a reader calls a thing finds it: the audio input by
            // `microphone`, serial by `arduino`, a gamepad by `joystick`.
            let first = |query: &str| {
                reference
                    .search(query)
                    .first()
                    .map(|&index| reference.entry(index).unwrap().name.clone())
            };
            assert_eq!(first("microphone").as_deref(), Some("audio input"));
            assert_eq!(first("mic").as_deref(), Some("audio input"));
            assert_eq!(first("arduino").as_deref(), Some("serial output"));
            assert_eq!(first("joystick").as_deref(), Some("gamepad setup"));
            assert_eq!(first("bpm").as_deref(), Some("set tempo"));
            let page = entry_body(
                reference
                    .entry(reference.lookup("audio input").unwrap())
                    .unwrap(),
                60,
            );
            assert!(
                !page.iter().any(|line| line.kind == BodyKind::Synonyms),
                "a keyword is for finding, not for reading as an alias"
            );

            // `tag:snippet` is every one of them, and nothing else.
            let tagged = reference.search("tag:snippet");
            assert_eq!(tagged.len(), super::super::super::snippets::SNIPPETS.len());
            assert!(
                tagged
                    .iter()
                    .all(|&index| reference.entry(index).unwrap().snippet.is_some())
            );
        }

        /// Snippet names are phrases of their own: none hides a function, and
        /// none is written twice.
        #[test]
        fn snippet_names_hide_nothing_and_repeat_nothing() {
            let reference = Reference::load_all();
            let mut seen = std::collections::HashSet::new();
            for snippet in super::super::super::snippets::SNIPPETS {
                assert!(seen.insert(snippet.name), "twice: {}", snippet.name);
                assert!(
                    snippet.name.contains(' '),
                    "a phrase, not a name: {}",
                    snippet.name
                );
                let entry = reference
                    .lookup(snippet.name)
                    .and_then(|index| reference.entry(index));
                assert!(
                    entry.is_some_and(|entry| entry.snippet.as_deref() == Some(snippet.code)),
                    "{} is in the reference as itself",
                    snippet.name
                );
                assert!(!snippet.code.trim().is_empty());
                assert!(!snippet.summary.is_empty() && !snippet.description.is_empty());
            }
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn a_click_in_the_snippet_shelf_selects_the_row_under_the_pointer() {
            let reference = Reference::load_all();
            let inner = inner_area(Rect::new(0, 0, 62, 30));
            let mut panel = ReferencePanel::snippets(&reference);
            let layout = panel.snippet_layout(inner);
            let geometry = panel.geometry(inner);
            assert_eq!(geometry.list, layout.list);

            assert_eq!(
                panel.click(&reference, geometry, layout.list.y + 2),
                PanelAction::Nothing,
                "a click selects; Enter inserts"
            );
            assert_eq!(panel.snippet_selected, 2);

            // Below the last row nothing moves.
            let rows = panel.snippet_lines().len();
            let before = panel.snippet_selected;
            panel.click(&reference, geometry, layout.list.y + rows as u16 + 1);
            assert_eq!(panel.snippet_selected, before);
        }
    }
}

#[cfg(all(test, feature = "hydra"))]
mod sketch_tab_tests {
    use super::*;

    fn shelf() -> ReferencePanel {
        let reference = Reference::load(|_| true);
        let mut panel = ReferencePanel::browse(&reference);
        panel.tab = Tab::Examples;
        panel
    }

    #[test]
    fn examples_tree_expands_and_collapses_without_composer_rows() {
        let mut panel = shelf();
        assert_eq!(panel.snippet_lines()[0], SnippetLine::Section(0));
        assert!(
            panel
                .snippet_lines()
                .iter()
                .all(|row| !matches!(row, SnippetLine::Snippet(..)))
        );
        panel.move_by(1);
        assert_eq!(
            panel.snippet_lines()[panel.snippet_selected],
            SnippetLine::Shelf(0, 0)
        );
        assert!(panel.expand());
        panel.move_by(1);
        assert_eq!(
            panel.snippet_lines()[panel.snippet_selected],
            SnippetLine::Snippet(0, 0, 0)
        );
        assert!(matches!(panel.preview(), PanelAction::PreviewScore(_)));
        assert!(panel.collapse());
        assert_eq!(
            panel.snippet_lines()[panel.snippet_selected],
            SnippetLine::Shelf(0, 0)
        );
        assert!(panel.collapse());
        assert!(!panel.snippet_open.contains(&(0, 0)));
    }

    #[test]
    fn every_json_example_is_reachable_in_the_tree_with_its_code_and_kind() {
        let mut panel = shelf();
        for (section, group) in super::super::examples::SECTIONS.iter().enumerate() {
            panel.section_open.insert(section);
            for shelf in 0..group.shelves.len() {
                panel.snippet_open.insert((section, shelf));
            }
        }
        let lines = panel.snippet_lines();
        let mut count = 0;
        for (row, line) in lines.iter().enumerate() {
            panel.snippet_selected = row;
            if let SnippetLine::Snippet(section, shelf, index) = *line {
                count += 1;
                let group = &super::super::examples::SECTIONS[section];
                assert_eq!(
                    panel.selected_snippet().unwrap().code,
                    group.shelves[shelf].snippets[index].code
                );
                assert_eq!(panel.selected_snippet_kind(), Some(group.kind));
                match (group.kind, panel.preview()) {
                    (super::super::examples::Kind::Music, PanelAction::PreviewScore(code)) => {
                        assert_eq!(code, panel.selected_snippet_code().unwrap())
                    }
                    (super::super::examples::Kind::Hydra, PanelAction::Nothing) => (),
                    other => panic!("wrong preview route: {other:?}"),
                }
            } else {
                assert!(panel.selected_snippet_code().is_none());
                assert_eq!(panel.preview(), PanelAction::Nothing);
            }
        }
        assert_eq!(count, super::super::examples::embedded().len());
    }

    #[test]
    fn hydra_examples_reserve_their_picture_above_the_tree_and_code() {
        use super::super::examples::{Kind, SECTIONS};
        let mut panel = shelf();
        for (section, group) in SECTIONS.iter().enumerate() {
            panel.section_open.insert(section);
            for shelf in 0..group.shelves.len() {
                panel.snippet_open.insert((section, shelf));
            }
        }
        let inner = Rect::new(2, 1, 40, 35);
        for (row, line) in panel.snippet_lines().iter().enumerate() {
            panel.snippet_selected = row;
            let hydra = matches!(line,
                SnippetLine::Section(section)
                | SnippetLine::Shelf(section, _)
                | SnippetLine::Snippet(section, ..)
                if SECTIONS[*section].kind == Kind::Hydra);
            assert_eq!(panel.shows_picture(), hydra, "{line:?}");
            let layout = panel.snippet_layout(inner);
            assert_eq!(layout, snippet_layout(inner, hydra), "{line:?}");
            if hydra {
                assert_eq!(layout.preview, preview_area(inner));
                assert!(layout.preview.bottom() <= layout.list.y);
            } else {
                assert!(layout.preview.is_empty());
            }
        }
        // Keep the cursor on the last Hydra example while changing tabs.
        assert!(panel.shows_picture());
        for tab in [
            Tab::Samples,
            Tab::Chords,
            Tab::Scales,
            Tab::Reference,
            Tab::Generator,
        ] {
            panel.tab = tab;
            assert!(!panel.shows_picture(), "{tab:?}");
        }
    }

    /// Enter copies the complete snippet chain into the score.
    #[test]
    fn enter_copies_the_whole_chain() {
        let reference = Reference::load(|_| true);
        let mut panel = shelf();
        panel.move_by(1);
        panel.expand();
        // Move from the shelf heading onto its first example.
        panel.move_by(1);
        let code = super::super::examples::SECTIONS[0].shelves[0].snippets[0].code;
        assert_eq!(
            panel.confirm(&reference),
            PanelAction::Copy(code.to_owned()),
            "Enter takes the chain rather than pasting it into the score"
        );
        assert_eq!(panel.copy(), PanelAction::Copy(code.to_owned()));
    }

    /// A closed shelf has nothing to show, so nothing is previewed and the
    /// score keeps the screen.
    #[test]
    fn a_shelf_heading_previews_nothing() {
        let mut panel = shelf();
        panel.move_by(1);
        assert!(panel.selected_snippet_code().is_none());
        assert_eq!(panel.copy(), PanelAction::Nothing);
    }
    /// Each row shows up to three aliases beside the name. A matching alias comes
    /// first; extra aliases are counted. The lowercase name is not an alias.
    #[test]
    fn a_rows_aliases_stand_beside_its_name_with_the_matched_one_first() {
        let reference = Reference::load_all();
        // A function with aliases, and one of them that is not a prefix of
        // the name, so a hit through it is plainly through the alias.
        let (index, entry, alias) = reference
            .search("")
            .into_iter()
            .filter_map(|index| reference.entry(index).map(|entry| (index, entry)))
            .find_map(|(index, entry)| {
                let lowercase = entry.name.to_lowercase();
                let alias = entry.synonyms.iter().find(|alias| {
                    alias.to_lowercase() != lowercase
                        && !lowercase.starts_with(&alias.to_lowercase())
                        && alias.len() >= 2
                })?;
                Some((index, entry, alias.clone()))
            })
            .expect("a function with an alias");
        let text = aliases_text(entry, "");
        assert!(text.contains(&alias), "{text} lists {alias}");
        assert!(
            aliases_text(entry, &alias).starts_with(&alias),
            "the hit comes first: {}",
            aliases_text(entry, &alias)
        );
        assert!(
            aliases_text(entry, &alias.to_uppercase()).starts_with(&alias),
            "whatever the case it was typed in"
        );
        let shown = text
            .split(' ')
            .filter(|word| !word.starts_with('+'))
            .count();
        assert!(shown <= ALIASES_SHOWN, "{text}");
        let mut lone = entry.clone();
        lone.synonyms = vec![lone.name.to_lowercase()];
        assert_eq!(aliases_text(&lone, ""), "", "a name is not its own alias");
        let _ = index;

        let mut panel = ReferencePanel::browse(&reference);
        for character in alias.chars() {
            panel.type_char(&reference, character);
        }
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 80, 14);
        let mut buffer = Buffer::empty(area);
        ReferenceView {
            keybinds: &crate::keybinds::Keybinds::default(),
            focused: true,
            pulse: None,
            sounding_note: None,
            loading: None,
            caching: 0,
            importing: 0,
            library_loading: false,
            #[cfg(feature = "hydra")]
            playing: None,
            #[cfg(feature = "hydra")]
            preview_note: None,
            #[cfg(feature = "hydra")]
            preview_progress: None,
            #[cfg(feature = "hydra")]
            preview_row: None,
            picture: false,
            refused: None,
            reference: &reference,
            panel: &panel,
            theme: &theme,
        }
        .render(area, &mut buffer);
        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                    .collect()
            })
            .collect();
        let label = format!(" {} ", entry.name);
        let (y, row) = rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.contains(&label))
            .expect("the entry's row");
        let after_name = &row[row.find(&label).unwrap() + label.len()..];
        assert!(
            after_name.trim_start().starts_with(&alias),
            "the matched alias stands beside the name: {row}"
        );
        let alias_x = row.find(after_name.trim_start()).unwrap() as u16;
        assert_eq!(
            buffer.cell((alias_x, y as u16)).unwrap().fg,
            theme.muted,
            "muted"
        );
    }
}

#[cfg(test)]
mod tag_discovery_tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    fn drawn(panel: &ReferencePanel, reference: &Reference) -> String {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 90, 24);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ReferenceView {
            keybinds: &crate::keybinds::Keybinds::default(),
            focused: true,
            pulse: None,
            sounding_note: None,
            loading: None,
            caching: 0,
            importing: 0,
            library_loading: false,
            #[cfg(feature = "hydra")]
            playing: None,
            #[cfg(feature = "hydra")]
            preview_note: None,
            #[cfg(feature = "hydra")]
            preview_progress: None,
            #[cfg(feature = "hydra")]
            preview_row: None,
            picture: false,
            refused: None,
            reference,
            panel,
            theme: &theme,
        }
        .render(area, &mut buffer);
        (0..area.height)
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// An empty search shows a `tag:` hint instead of a redundant result count.
    /// Typing restores the count, and `tag:` offers the available tags.
    #[test]
    fn an_empty_search_box_says_that_tags_can_be_typed_into_it() {
        let reference = Reference::load_all();
        let panel = ReferencePanel::browse(&reference);
        let text = drawn(&panel, &reference);
        assert!(text.contains("type tag:"), "{text}");
        assert!(
            !text.contains(&format!("{} of {}", reference.len(), reference.len())),
            "and does not spend the row on a count of everything: {text}"
        );

        // Typing gives the row back to the count, and typing `tag:` offers
        // the vocabulary rather than an empty result.
        let mut panel = ReferencePanel::browse(&reference);
        for character in "tag:".chars() {
            panel.type_char(&reference, character);
        }
        let text = drawn(&panel, &reference);
        assert!(text.contains("tags: "), "the words themselves: {text}");
        assert!(!text.contains("type tag:"), "{text}");

        // And a real one narrows and names the filter.
        let mut panel = ReferencePanel::browse(&reference);
        for character in "tag:samples".chars() {
            panel.type_char(&reference, character);
        }
        assert!(!panel.results.is_empty(), "tag:samples found nothing");
        assert!(panel.results.len() < reference.len(), "it narrowed nothing");
    }

    #[test]
    fn unicode_search_words_do_not_break_tag_parsing() {
        let reference = Reference::load_all();
        let mut panel = ReferencePanel::browse(&reference);
        for character in "aé界".chars() {
            assert!(panel.type_char(&reference, character));
        }
        assert_eq!(panel.query, "aé界");
        assert_eq!(
            TagFilter::parse("aé界 tag:audio"),
            TagFilter {
                name: Some("audio"),
                rest: "aé界",
            }
        );
    }

    #[test]
    fn unfocused_reference_footer_tracks_rebinding_and_unbinding() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let reference = Reference::load_all();
        let panel = ReferencePanel::browse(&reference);
        let theme = Theme::built_in_default();
        let draw = |keybinds: &Keybinds| {
            let area = Rect::new(0, 0, 60, 25);
            let mut buffer = Buffer::empty(area);
            ReferenceView {
                keybinds,
                reference: &reference,
                panel: &panel,
                theme: &theme,
                focused: false,
                pulse: None,
                sounding_note: None,
                picture: false,
                refused: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
            }
            .render(area, &mut buffer);
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let mut bindings = Keybinds::default();
        bindings.learn(BindAction::Reference, KeyCombo::parse("f3"));
        assert!(draw(&bindings).contains("F3 returns"));
        assert!(!draw(&bindings).contains("^F returns"));
        bindings.unbind(BindAction::Reference);
        assert!(draw(&bindings).contains("click returns"));
    }
}

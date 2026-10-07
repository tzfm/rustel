//! The visuals panel: a column of widgets that move with the music, for
//! the screen a set is recorded or streamed from - a scope, an analyser, a
//! vectorscope, the events as they fire, and the set's name as ANSI art
//! the way a demoscene keygen wrote it. The widgets stack down the column,
//! scroll when they outgrow it, and are kept in the studio's preferences,
//! the same in every set and every launch.
//!
//! This module keeps what the preferences know - the kinds, their style
//! vocabularies, the colourings, a widget's spec - and the panel's own
//! state; `art`, `scopes` and `events` draw the widgets, `column` draws
//! the panel and its add sheet. Every widget draws on the visualizers'
//! point canvas, so it is as fine as the terminal's tier allows.

use std::cell::RefCell;

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

mod art;
mod column;
mod events;
mod mixer;
mod scopes;

pub use art::{ArtView, art_rows};
pub use column::{VizAddView, VizDockView};
pub use events::EventsView;
pub use mixer::{
    METER_FLOOR_DB, MixerFacts, MixerFader, MixerStrip, MixerStripKind, MixerView,
    REDUCTION_FULL_DB,
};
pub use scopes::{Look, ScopeView, SpectrumView, VectorView};

/// Cells a docked column takes by default, its rule included, and the
/// least and the most a resize allows.
pub const VIZ_WIDTH: u16 = 36;
pub const COLUMN_MIN_WIDTH: u16 = 24;
pub const COLUMN_MAX_WIDTH: u16 = 80;
/// Rows a band takes by default, its rule and hint included - about half
/// a column's presence, a row being twice a cell's height - and the
/// least and the most a resize allows.
pub const BAND_DEFAULT_HEIGHT: u16 = 9;
pub const BAND_MIN_HEIGHT: u16 = 5;
pub const BAND_MAX_HEIGHT: u16 = 30;
/// Cells a band gives each widget at least; past that the rest wait.
pub const BAND_SLOT_MIN_WIDTH: u16 = 12;
/// How many visuals docks the studio has.
pub const DOCKS: usize = 2;
/// A pasted drawing stays small enough to edit and lay out interactively.
pub const ART_MAX_BYTES: usize = 64 * 1024;
pub const ART_MAX_LINES: usize = 256;

pub fn art_text_error(text: &str) -> Option<&'static str> {
    if text.len() > ART_MAX_BYTES {
        Some("artwork limit: 64 KiB; shorten the text before pasting")
    } else if text.lines().take(ART_MAX_LINES + 1).count() > ART_MAX_LINES {
        Some("artwork limit: 256 lines; shorten the drawing before pasting")
    } else {
        None
    }
}

/// Where a visuals dock sits: a column down either side, or a band
/// across the top or the bottom.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    #[default]
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|edge| edge.name() == text)
    }

    /// A column down the side, as against a band across.
    pub fn is_column(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }

    pub fn next(self, forwards: bool) -> Self {
        step(&Self::ALL, self, forwards)
    }

    /// The band this edge stands for, for a fixture that is only ever a
    /// band: the docked log or the memory breakdown. A side edge reads as
    /// the bottom. A log has long lines that wrap badly in a column, and
    /// its sheet is anchored at the bottom.
    pub fn band(self) -> Self {
        match self {
            Self::Top => Self::Top,
            Self::Left | Self::Right | Self::Bottom => Self::Bottom,
        }
    }

    /// The other band: `e` on a fixture that is only ever a band moves it
    /// between the top and the bottom, as the mixer's own `e` does.
    pub fn flipped_band(self) -> Self {
        match self.band() {
            Self::Top => Self::Bottom,
            _ => Self::Top,
        }
    }
}

/// A band's rows one step taller or shorter, within the bounds every band
/// shares - a row at a time, the same step a visuals band takes.
pub fn step_band(rows: u16, grow: bool) -> u16 {
    if grow {
        rows.saturating_add(1)
    } else {
        rows.saturating_sub(1)
    }
    .clamp(BAND_MIN_HEIGHT, BAND_MAX_HEIGHT)
}

/// What the layout is asked for a dock: its edge, and how much of the
/// screen it wants - cells across for a column, rows for a band.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dock {
    pub edge: Edge,
    pub extent: u16,
}

/// A dock as the preferences keep it: its edge, whether it is open, how
/// big it was made, and its widgets - the studio's own, the same in
/// every set.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DockPrefs {
    #[serde(default)]
    pub edge: Edge,
    #[serde(default)]
    pub open: bool,
    /// Cells across for a column, rows for a band, once a resize has
    /// set it; unset, the dock has its default size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub widgets: Vec<WidgetSpec>,
}

impl DockPrefs {
    /// The room the dock asks for: what a resize set, else the default
    /// for its edge.
    pub fn extent(&self) -> u16 {
        let (default, least, most) = self.extent_bounds();
        self.extent.unwrap_or(default).clamp(least, most)
    }

    fn extent_bounds(&self) -> (u16, u16, u16) {
        if self.edge.is_column() {
            (VIZ_WIDTH, COLUMN_MIN_WIDTH, COLUMN_MAX_WIDTH)
        } else {
            (BAND_DEFAULT_HEIGHT, BAND_MIN_HEIGHT, BAND_MAX_HEIGHT)
        }
    }

    /// Bigger or smaller by a step - four cells for a column, a row for
    /// a band - within the bounds; whether it changed.
    pub fn resize(&mut self, grow: bool) -> bool {
        let (_, least, most) = self.extent_bounds();
        let now = self.extent();
        let next = if !self.edge.is_column() {
            step_band(now, grow)
        } else if grow {
            now.saturating_add(4).clamp(least, most)
        } else {
            now.saturating_sub(4).clamp(least, most)
        };
        self.extent = Some(next);
        next != now
    }

    /// The docks a studio starts with: a column at the right, and a band
    /// across the top for the art, both shut.
    pub fn defaults() -> [Self; DOCKS] {
        [
            Self {
                edge: Edge::Right,
                ..Self::default()
            },
            Self {
                edge: Edge::Top,
                ..Self::default()
            },
        ]
    }
}

/// What a widget is, as the preferences keep it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetKind {
    /// The set's name, as ANSI art.
    Art,
    /// The mix leaving the machine, as a trace.
    Scope,
    /// The analyser: bars per band, peaks held.
    Spectrum,
    /// The stereo field, left against right, on a polar dial.
    #[serde(rename = "vectorscope")]
    Vector,
    /// The events as they are queued to sound, newest last.
    Events,
    /// The input, the orbits and the master as meters with faders, and
    /// the MIDI and pad lights.
    Mixer,
    /// Nothing at all: an empty slot, to place the others.
    Spacer,
}

impl WidgetKind {
    /// Whether the widget is there whatever the set is doing.
    ///
    /// The art is a title, not a picture of the mix: a set's name belongs
    /// on the screen it is streamed from whether or not anything is
    /// sounding, and so does the space a spacer holds. Everything else is
    /// a picture of sound, and there is none to draw when nothing plays.
    pub fn always_visible(self) -> bool {
        // The mixer is a control surface: its faders are there to be set
        // before anything plays, and its lights say what is plugged in.
        matches!(self, Self::Art | Self::Spacer | Self::Mixer)
    }

    pub const ALL: [Self; 7] = [
        Self::Art,
        Self::Scope,
        Self::Spectrum,
        Self::Vector,
        Self::Events,
        Self::Mixer,
        Self::Spacer,
    ];

    /// The kind after or before this one, round the ends.
    pub fn next(self, forwards: bool) -> Self {
        step(&Self::ALL, self, forwards)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Art => "ansi art",
            Self::Scope => "scope",
            Self::Spectrum => "spectrum",
            Self::Vector => "vectorscope",
            Self::Events => "events",
            Self::Mixer => "mixer",
            Self::Spacer => "spacer",
        }
    }

    /// What the add sheet says beside the name.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Art => "the set's name, in text-mode glyphs",
            Self::Scope => "the mix as a trace, six ways",
            Self::Spectrum => "the analyser, bars to waterfall",
            Self::Vector => "the stereo field on a polar dial",
            Self::Events => "every event as it fires",
            Self::Mixer => "a row a strip: input, orbits, master - meters and faders",
            Self::Spacer => "an empty slot, to place the others",
        }
    }

    /// Rows the widget's body takes, apart from the art, which is as tall
    /// as its picture.
    fn fixed_rows(self) -> u16 {
        match self {
            Self::Art => 0,
            Self::Scope => 6,
            Self::Spectrum => 8,
            Self::Vector => 14,
            Self::Events => 10,
            Self::Mixer => 8,
            Self::Spacer => 2,
        }
    }

    /// The names of the styles the kind draws in, in stepping order;
    /// none for the events.
    pub fn styles(self) -> &'static [&'static str] {
        match self {
            Self::Art => ArtStyle::NAMES,
            Self::Scope => ScopeStyle::NAMES,
            Self::Spectrum => SpectrumStyle::NAMES,
            Self::Vector => VectorStyle::NAMES,
            Self::Events | Self::Mixer | Self::Spacer => &[],
        }
    }
}

/// A style vocabulary: the variants in stepping order, each under the
/// name the set file keeps it by.
macro_rules! styles {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub const NAMES: &'static [&'static str] = &[$($text,)+];

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }

            /// The style under that name, if the vocabulary has it.
            pub fn parse(text: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|style| style.name() == text)
            }
        }
    };
}

styles! {
    /// How the art's glyphs are drawn: the four that earned their keep.
    /// A style a file names from before draws as solid.
    ArtStyle {
        /// Every pixel a block.
        #[default]
        Solid = "solid",
        /// A halo round every glyph, breathing.
        Glow = "glow",
        /// A bright band sweeping across the glyphs.
        Beam = "beam",
        /// The glyphs riding a wave.
        Wobble = "wobble",
    }
}

impl ArtStyle {
    /// Whether the style moves on its own, with the clock or the mix.
    pub fn animated(self) -> bool {
        matches!(self, Self::Glow | Self::Beam | Self::Wobble)
    }
}

styles! {
    /// How the scope draws the mix.
    ScopeStyle {
        /// The trace, a line.
        #[default]
        Line = "line",
        /// The trace filled to the centre line.
        Fill = "fill",
        /// The envelope, mirrored above and below the middle: a waveform
        /// view.
        Mirror = "mirror",
        /// The samples as dots alone.
        Dots = "dots",
        /// The line in a halo: a tube's glow.
        Glow = "glow",
        /// Bars of level from the floor, every hit a spike.
        Bars = "bars",
    }
}

styles! {
    /// How the analyser draws its bands.
    SpectrumStyle {
        /// Bars from the floor, peaks held.
        #[default]
        Bars = "bars",
        /// Bars from the middle out, reflected below in half light: the
        /// equalizer of a car stereo.
        Mirror = "mirror",
        /// A grid of lamps, lit up to the level.
        Matrix = "matrix",
        /// The bands as a curve.
        Line = "line",
        /// The curve filled to the floor.
        Fill = "fill",
        /// Time across with the newest frame at the right edge, frequency
        /// up the rows, level as colour: a spectrogram.
        Waterfall = "waterfall",
        /// The same picture in Braille dots - two columns of time and four
        /// rows of frequency a cell, the level as how many dots light - so
        /// a harmonic reads as a line rather than a stripe of blocks.
        Braille = "braille",
    }
}

styles! {
    /// How the vectorscope draws the field.
    VectorStyle {
        /// Every frame a point, side across and mid up, so a mono mix
        /// stands upright and a wide one lies flat.
        #[default]
        Polar = "polar",
        /// The level by direction, as petals: the loudest frame in every
        /// sector, easing back.
        Petals = "petals",
        /// The frames joined into a thread, the newest bright, the oldest
        /// fading.
        Trails = "trails",
        /// Left across, right up: the Lissajous figure of a lab scope.
        Lissajous = "lissajous",
        /// A hand sweeping round, drawing the level where it passes: a
        /// radar.
        Sweep = "sweep",
    }
}

/// How a widget is coloured. Named for the art, which had colours first,
/// and shared by the scopes: across the width, down the height, or with
/// the clock and the mix.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Colouring {
    /// The theme's foreground alone.
    Mono,
    /// Theme colours: across frequency for spectrum shapes, top to bottom elsewhere.
    #[default]
    Theme,
    /// Cyan, white, magenta, red, yellow, orange: an ANSI pack's palette.
    Acid,
    /// Every hue across the width.
    Rainbow,
    /// Yellow through red to embers, top to bottom.
    Fire,
    /// The rainbow, rolling across with time.
    Wave,
    /// The palette breathing with the mix: dim between hits, lit on them.
    Pulse,
    /// Hot pink and cyan by turns, the tubes flickering.
    Neon,
    /// A metal's bands, light and dark, top to bottom.
    Chrome,
    /// Gold, lit from above.
    Gold,
}

impl Colouring {
    pub const ALL: [Self; 10] = [
        Self::Mono,
        Self::Theme,
        Self::Acid,
        Self::Rainbow,
        Self::Fire,
        Self::Wave,
        Self::Pulse,
        Self::Neon,
        Self::Chrome,
        Self::Gold,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Mono => "mono",
            Self::Theme => "theme",
            Self::Acid => "acid",
            Self::Rainbow => "rainbow",
            Self::Fire => "fire",
            Self::Wave => "wave",
            Self::Pulse => "pulse",
            Self::Neon => "neon",
            Self::Chrome => "chrome",
            Self::Gold => "gold",
        }
    }

    /// Whether the picture changes on its own, so the screen is drawn
    /// again without anything else happening.
    pub fn animated(self) -> bool {
        matches!(self, Self::Wave | Self::Pulse | Self::Neon)
    }

    fn next(self, forwards: bool) -> Self {
        step(&Self::ALL, self, forwards)
    }
}

fn step<T: Copy + PartialEq>(all: &[T], current: T, forwards: bool) -> T {
    let index = all.iter().position(|item| *item == current).unwrap_or(0);
    let count = all.len() as isize;
    let next = (index as isize + if forwards { 1 } else { -1 }).rem_euclid(count) as usize;
    all[next]
}

/// Placement along the dock: horizontally in a band, vertically in a column.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtAlignment {
    Start,
    #[default]
    Center,
    End,
}

impl ArtAlignment {
    pub fn name(self, edge: Edge) -> &'static str {
        match (edge.is_column(), self) {
            (false, Self::Start) => "left",
            (false, Self::Center) => "center",
            (false, Self::End) => "right",
            (true, Self::Start) => "top",
            (true, Self::Center) => "middle",
            (true, Self::End) => "bottom",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Start => Self::Center,
            Self::Center => Self::End,
            Self::End => Self::Start,
        }
    }

    fn is_center(&self) -> bool {
        *self == Self::Center
    }

    pub(super) fn offset(self, available: usize, used: usize) -> usize {
        let spare = available.saturating_sub(used);
        match self {
            Self::Start => 0,
            Self::Center => spare / 2,
            Self::End => spare,
        }
    }
}

/// One widget as the preferences keep it: what it is, and how it is
/// drawn and coloured.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WidgetSpec {
    pub kind: WidgetKind,
    /// The style's name, from the kind's own vocabulary; a name the kind
    /// does not know draws as its first style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Colouring>,
    /// The art's own text, when it is not the set's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "ArtAlignment::is_center")]
    pub alignment: ArtAlignment,
}

impl WidgetSpec {
    pub fn new(kind: WidgetKind) -> Self {
        Self {
            kind,
            style: None,
            colour: None,
            text: None,
            alignment: ArtAlignment::default(),
        }
    }

    /// What the art writes: its own text, or the set's name.
    pub fn art_text<'a>(&'a self, set_name: &'a str) -> &'a str {
        self.text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(set_name)
    }

    /// Give the art a text of its own; empty means the set's name again.
    pub fn set_text(&mut self, text: &str) {
        let stored = if text.contains('\n') {
            text
        } else {
            text.trim()
        };
        self.text = (!stored.trim().is_empty()).then(|| stored.to_owned());
    }

    /// The next kind along, either way. The style belongs to the kind
    /// and goes with it; the colouring and the text stay.
    pub fn step_kind(&mut self, forwards: bool) {
        self.kind = self.kind.next(forwards);
        self.style = None;
    }

    /// The widget an empty panel gets: the set's name, in art.
    pub fn default_art() -> Self {
        Self::new(WidgetKind::Art)
    }

    pub fn art_style(&self) -> ArtStyle {
        self.style
            .as_deref()
            .and_then(ArtStyle::parse)
            .unwrap_or_default()
    }

    pub fn scope_style(&self) -> ScopeStyle {
        self.style
            .as_deref()
            .and_then(ScopeStyle::parse)
            .unwrap_or_default()
    }

    pub fn spectrum_style(&self) -> SpectrumStyle {
        self.style
            .as_deref()
            .and_then(SpectrumStyle::parse)
            .unwrap_or_default()
    }

    pub fn vector_style(&self) -> VectorStyle {
        self.style
            .as_deref()
            .and_then(VectorStyle::parse)
            .unwrap_or_default()
    }

    /// The style's name as it draws: the kind's first when unset or
    /// unknown, empty for a kind without styles.
    pub fn style_name(&self) -> &'static str {
        let names = self.kind.styles();
        let current = self.style.as_deref();
        names
            .iter()
            .copied()
            .find(|name| Some(*name) == current)
            .or_else(|| names.first().copied())
            .unwrap_or("")
    }

    pub fn colour(&self) -> Colouring {
        self.colour.unwrap_or_default()
    }

    /// The next style along, either way, in the kind's own vocabulary.
    pub fn step_style(&mut self, forwards: bool) -> bool {
        let names = self.kind.styles();
        if names.is_empty() {
            return false;
        }
        let current = self.style_name();
        let index = names.iter().position(|name| *name == current).unwrap_or(0);
        let count = names.len() as isize;
        let next = (index as isize + if forwards { 1 } else { -1 }).rem_euclid(count) as usize;
        self.style = Some(names[next].to_owned());
        true
    }

    pub fn step_colour(&mut self, forwards: bool) -> bool {
        if matches!(
            self.kind,
            WidgetKind::Events | WidgetKind::Mixer | WidgetKind::Spacer
        ) {
            return false;
        }
        self.colour = Some(self.colour().next(forwards));
        true
    }

    /// What the widget's header says: its kind, style and colour.
    pub fn title(&self) -> String {
        match self.kind {
            WidgetKind::Events | WidgetKind::Mixer | WidgetKind::Spacer => {
                self.kind.name().to_owned()
            }
            kind => format!(
                "{} · {} · {}",
                kind.name(),
                self.style_name(),
                self.colour().name()
            ),
        }
    }

    pub fn title_at(&self, edge: Edge) -> String {
        if self.kind == WidgetKind::Art {
            format!(
                "{} · {} · {} · {}",
                self.kind.name(),
                self.alignment.name(edge),
                self.style_name(),
                self.colour().name()
            )
        } else {
            self.title()
        }
    }

    /// Whether the widget moves on its own, without the music: with the
    /// clock or the mix's level, or the sweep's hand.
    pub fn animated(&self) -> bool {
        match self.kind {
            WidgetKind::Art => self.colour().animated() || self.art_style().animated(),
            WidgetKind::Vector => {
                self.colour().animated() || self.vector_style() == VectorStyle::Sweep
            }
            WidgetKind::Scope | WidgetKind::Spectrum => self.colour().animated(),
            WidgetKind::Events | WidgetKind::Mixer | WidgetKind::Spacer => false,
        }
    }
}

/// Whether there is sound for the widgets to show.
///
/// A widget that shows the mix is not drawn while nothing plays: neither
/// its trace nor its static parts. A picture over silence looks like
/// playback.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Motion {
    /// Silent: the widgets that show the mix are not drawn.
    #[default]
    Off,
    /// Sounding: the pictures follow the mix.
    Live,
}

impl Motion {
    /// Whether there is sound to draw.
    pub fn shows_sound(self) -> bool {
        matches!(self, Self::Live)
    }
}

/// A dock's own state while it is open: which widget is chosen, how far
/// a column is scrolled, and the question it is asking.
#[derive(Clone, Debug)]
pub struct VizPanel {
    pub selected: usize,
    /// Rows scrolled past the top of a column.
    pub scroll: u16,
    /// Choosing a kind to add, on a sheet: the index into `WidgetKind::ALL`.
    pub adding: Option<usize>,
    /// The art's text being typed, on a sheet.
    pub prompt: Option<super::file_picker::FilePicker>,
    pub error: Option<String>,
    /// Which edge the dock sits at.
    pub edge: Edge,
    /// What a widget's style keeps between frames - the petals' reach,
    /// the sweep's trace - by widget. The view fills it as it draws.
    pub memory: RefCell<Vec<Vec<f32>>>,
}

impl Default for VizPanel {
    fn default() -> Self {
        Self {
            selected: 0,
            scroll: 0,
            adding: None,
            prompt: None,
            error: None,
            edge: Edge::Right,
            memory: RefCell::new(Vec::new()),
        }
    }
}

/// A dock's parts: a title row (a column's only), the widgets' body, the
/// hint rows, and the rule on the editor's side.
#[derive(Clone, Copy, Debug)]
pub struct VizParts {
    pub title: Rect,
    pub body: Rect,
    pub hint: Rect,
    pub rule: Rect,
}

impl VizPanel {
    pub fn move_by(&mut self, delta: isize, count: usize) {
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = ((self.selected as isize + delta).rem_euclid(count as isize)) as usize;
        self.error = None;
    }

    pub fn clamp(&mut self, count: usize) {
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    /// The dock's parts, when the layout gave it room. A column keeps a
    /// title row and two hint rows; a band a hint row alone, its title
    /// in it, so the widgets get the height.
    pub fn parts(area: Rect, edge: Edge) -> Option<VizParts> {
        if area.is_empty() || area.height < 4 || area.width < 8 {
            return None;
        }
        if edge.is_column() {
            let inner_width = area.width.saturating_sub(2);
            let inner_x = if edge == Edge::Right {
                area.x + 2
            } else {
                area.x + 1
            };
            let title = Rect::new(inner_x, area.y, inner_width, 1);
            let hints = 2u16.min(area.height.saturating_sub(3));
            let hint = Rect::new(inner_x, area.bottom() - hints, inner_width, hints);
            let body = Rect::new(
                inner_x,
                area.y + 1,
                inner_width,
                area.height.saturating_sub(1 + hints),
            );
            let rule_x = if edge == Edge::Right {
                area.x
            } else {
                area.right().saturating_sub(1)
            };
            let rule = Rect::new(rule_x, area.y, 1, area.height);
            return Some(VizParts {
                title,
                body,
                hint,
                rule,
            });
        }
        // A band: the rule along the editor's side, the hint row at the
        // outer edge, the widgets between.
        let inner_x = area.x + 1;
        let inner_width = area.width.saturating_sub(2);
        let (rule_y, hint_y, body_y) = if edge == Edge::Top {
            (area.bottom() - 1, area.y, area.y + 1)
        } else {
            (area.y, area.bottom() - 1, area.y + 1)
        };
        Some(VizParts {
            title: Rect::new(inner_x, hint_y, 0, 0),
            body: Rect::new(inner_x, body_y, inner_width, area.height.saturating_sub(2)),
            hint: Rect::new(inner_x, hint_y, inner_width, 1),
            rule: Rect::new(area.x, rule_y, area.width, 1),
        })
    }

    /// A band's widgets side by side: each gets an equal share of the
    /// width, at least [`BAND_SLOT_MIN_WIDTH`] cells, and the ones that
    /// would not fit wait unseen. A slot's first row is its header.
    pub fn band_slots(count: usize, body: Rect) -> Vec<Rect> {
        if count == 0 || body.is_empty() {
            return Vec::new();
        }
        let shown = count.min(usize::from(body.width / BAND_SLOT_MIN_WIDTH).max(1));
        let share = body.width / shown as u16;
        (0..shown)
            .map(|index| {
                let x = body.x + share * index as u16;
                let width = if index + 1 == shown {
                    body.right().saturating_sub(x)
                } else {
                    share
                };
                Rect::new(x, body.y, width, body.height)
            })
            .collect()
    }

    /// A sheet asks: the dock's prompt or its add list is up.
    pub fn asking(&self) -> bool {
        self.adding.is_some() || self.prompt.is_some()
    }

    /// The add sheet's place: bottom-right like the other sheets, a row
    /// per kind up to a screenful - so a hundred kinds would scroll, not
    /// spill.
    pub fn add_sheet_geometry(available: Rect) -> Option<(Rect, Rect)> {
        let rows = (WidgetKind::ALL.len() as u16).clamp(1, 12);
        let height = rows + 4;
        let width = 52u16.min(available.width.saturating_sub(2));
        if width < 24 || available.height < height + 1 {
            return None;
        }
        let area = Rect::new(
            available.right().saturating_sub(width + 1),
            available.bottom().saturating_sub(height + 1),
            width,
            height,
        );
        let list = Rect::new(area.x + 2, area.y + 2, area.width.saturating_sub(4), rows);
        Some((area, list))
    }

    /// The kind under a point of the add sheet's list.
    pub fn add_row_at(&self, available: Rect, x: u16, y: u16) -> Option<usize> {
        let chosen = self.adding?;
        let (_, list) = Self::add_sheet_geometry(available)?;
        if !list.contains((x, y).into()) {
            return None;
        }
        let first = chosen.saturating_sub(usize::from(list.height).saturating_sub(1));
        let index = first + usize::from(y - list.y);
        (index < WidgetKind::ALL.len()).then_some(index)
    }

    /// Whether the point is over the add sheet.
    pub fn add_sheet_contains(&self, available: Rect, x: u16, y: u16) -> bool {
        self.adding.is_some()
            && Self::add_sheet_geometry(available)
                .is_some_and(|(area, _)| area.contains((x, y).into()))
    }

    /// Each widget's rows in the column before scrolling: a header row
    /// and its body, one under the other. The header row stays a row
    /// when the panel is not focused, blank, so the pictures keep their
    /// places whether or not they are being edited.
    pub fn stack(widgets: &[WidgetSpec], set_name: &str, width: u16) -> Vec<(u16, u16)> {
        let mut rows = Vec::with_capacity(widgets.len());
        let mut y = 0u16;
        for spec in widgets {
            let body = match spec.kind {
                WidgetKind::Art => art_rows(spec.art_text(set_name), spec.art_style(), width),
                kind => kind.fixed_rows(),
            };
            rows.push((y, body.saturating_add(1)));
            y = y.saturating_add(body.saturating_add(1));
        }
        rows
    }

    /// The rows the widgets take together.
    pub fn stack_height(widgets: &[WidgetSpec], set_name: &str, width: u16) -> u16 {
        Self::stack(widgets, set_name, width)
            .last()
            .map_or(0, |(y, height)| y.saturating_add(*height))
    }

    /// Give art and events the column's unused rows, leaving other widgets at their
    /// natural size. Drawing, scrolling and pointer selection share these slots.
    pub fn stack_in(widgets: &[WidgetSpec], set_name: &str, body: Rect) -> Vec<(u16, u16)> {
        let mut stack = Self::stack(widgets, set_name, body.width);
        let total = stack
            .last()
            .map_or(0, |(y, height)| y.saturating_add(*height));
        let mut spare = body.height.saturating_sub(total);
        let mut remaining = widgets
            .iter()
            .filter(|spec| matches!(spec.kind, WidgetKind::Art | WidgetKind::Events))
            .count();
        let mut top = 0u16;
        for (spec, (y, height)) in widgets.iter().zip(&mut stack) {
            if matches!(spec.kind, WidgetKind::Art | WidgetKind::Events) {
                let extra = (usize::from(spare) / remaining) as u16;
                *height = height.saturating_add(extra);
                spare -= extra;
                remaining -= 1;
            }
            *y = top;
            top = top.saturating_add(*height);
        }
        stack
    }

    /// Keep the chosen widget's header in view.
    pub fn ensure_visible(&mut self, widgets: &[WidgetSpec], set_name: &str, body: Rect) {
        let stack = Self::stack_in(widgets, set_name, body);
        let total = stack
            .last()
            .map_or(0, |(y, height)| y.saturating_add(*height));
        let visible = body.height.max(1);
        self.scroll = self.scroll.min(total.saturating_sub(visible));
        if let Some(&(top, height)) = stack.get(self.selected) {
            let bottom = top.saturating_add(height.min(visible));
            if top < self.scroll {
                self.scroll = top;
            } else if bottom > self.scroll.saturating_add(visible) {
                self.scroll = bottom.saturating_sub(visible);
            }
        }
    }

    pub fn scroll_by(&mut self, delta: i32, widgets: &[WidgetSpec], set_name: &str, body: Rect) {
        let total = Self::stack_height(widgets, set_name, body.width);
        let most = total.saturating_sub(body.height.max(1));
        self.scroll = (i32::from(self.scroll) + delta).clamp(0, i32::from(most)) as u16;
    }

    /// The widget under a point of the body: by its rows in a column,
    /// by its slot in a band.
    pub fn widget_at(
        &self,
        widgets: &[WidgetSpec],
        set_name: &str,
        body: Rect,
        x: u16,
        y: u16,
    ) -> Option<usize> {
        if !body.contains((x, y).into()) {
            return None;
        }
        if !self.edge.is_column() {
            return Self::band_slots(widgets.len(), body)
                .iter()
                .position(|slot| slot.contains((x, y).into()));
        }
        let row = (y - body.y).saturating_add(self.scroll);
        Self::stack_in(widgets, set_name, body)
            .iter()
            .position(|(top, height)| row >= *top && row < top.saturating_add(*height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The widgets stack down the column with a header each; the chosen
    /// one is kept in view, and a point maps back to its widget through
    /// the scroll.
    #[test]
    fn widgets_stack_and_scroll() {
        let widgets = vec![
            WidgetSpec::new(WidgetKind::Scope),
            WidgetSpec::new(WidgetKind::Events),
            WidgetSpec::new(WidgetKind::Spectrum),
        ];
        let stack = VizPanel::stack(&widgets, "set", 34);
        assert_eq!(stack, [(0, 7), (7, 11), (18, 9)]);
        assert_eq!(VizPanel::stack_height(&widgets, "set", 34), 27);
        let body = Rect::new(1, 1, 34, 12);
        let mut panel = VizPanel {
            selected: 2,
            ..VizPanel::default()
        };
        panel.ensure_visible(&widgets, "set", body);
        assert_eq!(panel.scroll, 15, "the last widget's rows come into view");
        assert_eq!(
            panel.widget_at(&widgets, "set", body, 5, body.y + 3),
            Some(2)
        );
        panel.selected = 0;
        panel.ensure_visible(&widgets, "set", body);
        assert_eq!(panel.scroll, 0);
        assert_eq!(
            panel.widget_at(&widgets, "set", body, 5, body.y + 8),
            Some(1)
        );
        panel.scroll_by(100, &widgets, "set", body);
        assert_eq!(panel.scroll, 15, "never past the end");
        panel.scroll_by(-100, &widgets, "set", body);
        assert_eq!(panel.scroll, 0);
        let parts = VizPanel::parts(Rect::new(64, 3, VIZ_WIDTH, 30), Edge::Right).unwrap();
        assert_eq!(parts.rule, Rect::new(64, 3, 1, 30));
        assert_eq!(parts.body, Rect::new(66, 4, 34, 27), "two hint rows");
        let parts = VizPanel::parts(Rect::new(0, 3, VIZ_WIDTH, 30), Edge::Left).unwrap();
        assert_eq!(parts.rule, Rect::new(35, 3, 1, 30));
        assert_eq!(parts.body.x, 1);
    }

    /// A band lays its widgets side by side in equal slots, keeps the
    /// rule along the editor's side, and stands as tall as its tallest
    /// widget wants, within reason.
    #[test]
    fn a_band_lays_widgets_side_by_side() {
        let widgets = vec![
            WidgetSpec::new(WidgetKind::Scope),
            WidgetSpec::new(WidgetKind::Spectrum),
            WidgetSpec::new(WidgetKind::Events),
        ];
        let top = VizPanel::parts(Rect::new(0, 3, 120, 12), Edge::Top).unwrap();
        assert_eq!(top.rule, Rect::new(0, 14, 120, 1), "the rule under it");
        assert_eq!(top.hint, Rect::new(1, 3, 118, 1), "the hint along the top");
        assert_eq!(top.body, Rect::new(1, 4, 118, 10));
        let bottom = VizPanel::parts(Rect::new(0, 30, 120, 12), Edge::Bottom).unwrap();
        assert_eq!(bottom.rule.y, 30);
        assert_eq!(bottom.hint.y, 41);
        let slots = VizPanel::band_slots(widgets.len(), top.body);
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0], Rect::new(1, 4, 39, 10));
        assert_eq!(
            slots[2].right(),
            top.body.right(),
            "the last takes the rest"
        );
        assert_eq!(
            VizPanel::band_slots(9, Rect::new(0, 0, 40, 5)).len(),
            3,
            "twelve cells each at least"
        );
        let mut panel = VizPanel {
            edge: Edge::Top,
            ..VizPanel::default()
        };
        assert_eq!(panel.widget_at(&widgets, "set", top.body, 50, 6), Some(1));
        panel.edge = Edge::Right;
        assert_eq!(panel.widget_at(&widgets, "set", top.body, 50, 6), Some(0));
        assert_eq!(Edge::Bottom.next(true), Edge::Left, "round the far end");
        assert_eq!(Edge::parse("top"), Some(Edge::Top));
    }

    /// A dock is its default size until a resize sets it: a column four
    /// cells at a time, a band a row, within bounds, and the setting
    /// keeps with the dock.
    #[test]
    fn a_dock_resizes_within_bounds_and_remembers() {
        let mut column = DockPrefs {
            edge: Edge::Right,
            ..DockPrefs::default()
        };
        assert_eq!(column.extent(), VIZ_WIDTH);
        assert!(column.resize(true));
        assert_eq!(column.extent(), VIZ_WIDTH + 4);
        column.extent = Some(200);
        assert_eq!(column.extent(), COLUMN_MAX_WIDTH, "clamped on the way out");
        assert!(!column.resize(true), "no larger");
        let mut band = DockPrefs {
            edge: Edge::Bottom,
            ..DockPrefs::default()
        };
        assert_eq!(band.extent(), BAND_DEFAULT_HEIGHT);
        assert!(band.resize(false));
        assert_eq!(band.extent(), BAND_DEFAULT_HEIGHT - 1);
        for _ in 0..10 {
            band.resize(false);
        }
        assert_eq!(band.extent(), BAND_MIN_HEIGHT);
        let text = serde_json::to_string(&band).unwrap();
        assert!(text.contains("\"extent\":5"), "{text}");
    }

    /// A widget's kind steps round with ↑/↓, the style going with the
    /// kind and the colouring staying; a spacer has no style or colour;
    /// the art writes its own text when it has one.
    #[test]
    fn a_widget_changes_kind_and_the_art_takes_a_text() {
        let mut spec = WidgetSpec::new(WidgetKind::Scope);
        spec.style = Some("glow".into());
        spec.colour = Some(Colouring::Fire);
        spec.step_kind(true);
        assert_eq!(spec.kind, WidgetKind::Spectrum);
        assert_eq!(spec.style, None, "the style belonged to the scope");
        assert_eq!(spec.colour, Some(Colouring::Fire));
        spec.step_kind(false);
        spec.step_kind(false);
        assert_eq!(spec.kind, WidgetKind::Art);
        assert_eq!(spec.art_text("the set"), "the set");
        spec.set_text("  HELLO  ");
        assert_eq!(spec.art_text("the set"), "HELLO");
        spec.set_text("   ");
        assert_eq!(
            spec.art_text("the set"),
            "the set",
            "empty means the set's name"
        );
        let mut spacer = WidgetSpec::new(WidgetKind::Spacer);
        assert!(!spacer.step_style(true) && !spacer.step_colour(true));
        assert_eq!(spacer.title(), "spacer");
        assert_eq!(
            WidgetKind::Spacer.next(true),
            WidgetKind::Art,
            "round the far end"
        );
    }

    #[test]
    fn art_alignment_and_multiline_text_survive_preferences_and_dock_changes() {
        let mut spec: WidgetSpec = serde_json::from_str(r#"{"kind":"art"}"#).unwrap();
        assert_eq!(
            spec.alignment,
            ArtAlignment::Center,
            "older preferences stay centered"
        );
        let drawing = "  /\\_/\\\n ( o.o )\n  > ^ <  \n";
        spec.set_text(drawing);
        assert_eq!(spec.art_text("set"), drawing);
        for (alignment, band, column) in [
            (ArtAlignment::Center, "center", "middle"),
            (ArtAlignment::End, "right", "bottom"),
            (ArtAlignment::Start, "left", "top"),
        ] {
            assert_eq!(spec.alignment, alignment);
            for edge in Edge::ALL {
                let expected = if edge.is_column() { column } else { band };
                assert!(spec.title_at(edge).contains(expected));
            }
            let encoded = serde_json::to_string(&spec).unwrap();
            assert_eq!(serde_json::from_str::<WidgetSpec>(&encoded).unwrap(), spec);
            spec.alignment = spec.alignment.next();
        }
        assert_eq!(spec.alignment, ArtAlignment::Center);
    }

    #[test]
    fn a_single_events_widget_uses_the_whole_vertical_dock() {
        let widgets = [WidgetSpec::new(WidgetKind::Events)];
        let body = Rect::new(4, 3, 28, 42);
        assert_eq!(VizPanel::stack_in(&widgets, "set", body), vec![(0, 42)]);
        let mut panel = VizPanel::default();
        for y in body.y..body.bottom() {
            assert_eq!(panel.widget_at(&widgets, "set", body, body.x, y), Some(0));
        }
        panel.scroll_by(100, &widgets, "set", body);
        assert_eq!(panel.scroll, 0);
    }

    #[test]
    fn spare_column_rows_belong_to_art_for_drawing_and_pointer_selection() {
        let mut art = WidgetSpec::default_art();
        art.set_text("one\ntwo");
        let widgets = [art.clone(), WidgetSpec::new(WidgetKind::Scope), art];
        let natural = VizPanel::stack(&widgets, "set", 30);
        let total = VizPanel::stack_height(&widgets, "set", 30);
        let body = Rect::new(4, 2, 30, total + 9);
        let expanded = VizPanel::stack_in(&widgets, "set", body);
        assert_eq!(expanded[0].1, natural[0].1 + 4);
        assert_eq!(expanded[1].1, natural[1].1, "scope keeps its size");
        assert_eq!(expanded[2].1, natural[2].1 + 5);
        let mut panel = VizPanel::default();
        for (index, &(top, height)) in expanded.iter().enumerate() {
            for row in top..top + height {
                assert_eq!(
                    panel.widget_at(&widgets, "set", body, body.x, body.y + row),
                    Some(index)
                );
            }
        }
        panel.scroll_by(100, &widgets, "set", body);
        assert_eq!(panel.scroll, 0, "unused rows do not create scrolling");
        assert_eq!(
            VizPanel::stack_in(&widgets, "set", Rect::new(0, 0, 30, total - 1)),
            natural,
            "a crowded column keeps its natural scroll layout"
        );
    }

    /// A spec steps its style and colour round, only for the art, and
    /// round-trips through the set file.
    #[test]
    fn specs_step_and_round_trip() {
        let mut art = WidgetSpec::default_art();
        assert_eq!(art.title(), "ansi art · solid · theme");
        assert!(art.step_style(true));
        assert_eq!(art.art_style(), ArtStyle::Glow);
        assert!(art.step_style(false));
        assert!(art.step_style(false));
        assert_eq!(art.art_style(), ArtStyle::Wobble, "round the far end");
        assert!(art.animated(), "a moving style is animation enough");
        art.style = Some("solid".into());
        assert!(art.step_colour(true));
        assert_eq!(art.colour(), Colouring::Acid);
        assert!(!art.animated());
        art.colour = Some(Colouring::Wave);
        assert!(art.animated());
        // Every kind steps its own vocabulary; the events have none.
        let mut scope = WidgetSpec::new(WidgetKind::Scope);
        assert_eq!(scope.title(), "scope · line · theme");
        assert!(scope.step_style(false));
        assert_eq!(scope.scope_style(), ScopeStyle::Bars, "round the far end");
        assert!(scope.step_colour(true));
        assert_eq!(scope.title(), "scope · bars · acid");
        let mut vector = WidgetSpec::new(WidgetKind::Vector);
        assert!(!vector.animated());
        while vector.vector_style() != VectorStyle::Sweep {
            assert!(vector.step_style(true));
        }
        assert!(vector.animated(), "the sweep's hand turns on its own");
        let mut events = WidgetSpec::new(WidgetKind::Events);
        assert!(!events.step_style(true));
        assert!(!events.step_colour(true));
        assert_eq!(events.title(), "events");
        // A style from another kind's vocabulary draws as the first.
        let odd: WidgetSpec =
            serde_json::from_str(r#"{"kind":"spectrum","style":"slant"}"#).unwrap();
        assert_eq!(odd.spectrum_style(), SpectrumStyle::Bars);
        assert_eq!(odd.title(), "spectrum · bars · theme");
        let text = serde_json::to_string(&[art.clone(), scope.clone()]).unwrap();
        assert!(text.contains("\"kind\":\"art\""), "{text}");
        assert!(text.contains("\"style\":\"solid\""), "{text}");
        assert!(text.contains("\"style\":\"bars\""), "{text}");
        let back: Vec<WidgetSpec> = serde_json::from_str(&text).unwrap();
        assert_eq!(back[0], art);
        assert_eq!(back[1], scope);
    }
}

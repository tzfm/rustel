//! The theme editor: a theme shaped live, on the studio it is shaping.
//!
//! Two tabs over one draft. The form walks the theme's fields - every colour
//! with a swatch, the opacities, the sketch - and the code tab is the same
//! theme as the file it would be, in a real editor with the studio's own
//! syntax colouring. Editing either side rebuilds the other, everything
//! applies to the terminal as it changes, and nothing is kept until it is
//! written: `s` writes the draft into the theme directory, and Esc goes back
//! to the theme list - asking first when there is unsaved work.
//!
//! Built-ins live inside the executable and are a fixed vocabulary: editing
//! one starts a new theme, and saving it needs a new name. Your themes
//! live in the theme directory beside the settings.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::editor::{Editor, GridRect, ScreenRow};
use super::syntax::Lexer;
use super::terminal::CaretShape;
use super::theme::{BracketMark, CharacterEffect, MarkStyle, TachyonMode, Theme, parse_color};

/// How long the code tab waits after a keystroke before parsing the draft.
pub const CODE_DEBOUNCE: Duration = Duration::from_millis(400);

/// Which side of the editor is showing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EditorTab {
    #[default]
    Form,
    Code,
}

/// One editable colour: where it lives in the theme, and whether it may be
/// absent.
struct ColorField {
    label: &'static str,
    get: fn(&Theme) -> Option<Color>,
    set: fn(&mut Theme, Option<Color>),
    optional: bool,
}

macro_rules! required {
    ($label:expr, $($path:tt)+) => {
        ColorField {
            label: $label,
            get: |theme| Some(theme.$($path)+),
            set: |theme, color| {
                if let Some(color) = color {
                    theme.$($path)+ = color;
                }
            },
            optional: false,
        }
    };
}

macro_rules! optional {
    ($label:expr, $($path:tt)+) => {
        ColorField {
            label: $label,
            get: |theme| theme.$($path)+,
            set: |theme, color| theme.$($path)+ = color,
            optional: true,
        }
    };
}

/// Every colour the form walks, in the order the file writes them.
static COLORS: &[ColorField] = &[
    required!("background", background),
    required!("surface", surface),
    required!("overlay", overlay),
    required!("foreground", foreground),
    required!("muted", muted),
    required!("rule", rule),
    required!("accent", accent),
    required!("ok", ok),
    required!("warn", warn),
    required!("error", error),
    ColorField {
        label: "caret",
        get: |theme| Some(theme.caret()),
        set: |theme, color| {
            if let Some(color) = color {
                theme.caret = Some(color);
            }
        },
        optional: false,
    },
    optional!("current line", current_line),
    required!("selection", selection),
    required!("selection text", selection_text),
    required!("mini", mini),
    optional!("mini fill", mini_fill),
    optional!("bracket", bracket),
    optional!("slider fill", slider_fill),
    required!("event", event),
    required!("event inactive", event_inactive),
    required!("playhead", playhead),
    required!("grid", grid),
    required!("minimap", minimap),
    required!("minimap viewport", minimap_viewport),
    required!("syntax text", syntax.text),
    required!("syntax comment", syntax.comment),
    required!("syntax string", syntax.string),
    required!("syntax number", syntax.number),
    required!("syntax punctuation", syntax.punctuation),
    optional!("syntax keyword", syntax.keyword),
    optional!("syntax function", syntax.function),
    required!("meter low", meter.low),
    required!("meter mid", meter.mid),
    required!("meter high", meter.high),
    required!("meter peak", meter.peak),
    required!("meter track", meter.track),
    required!("meter fader", meter.fader),
];

/// The rows above the colours.
const ROW_NAME: usize = 0;
/// The three opacities the theme suggests for the global settings - the same
/// three the settings sheet carries, and the reason each theme can be read
/// through the right amount of its own backdrop. Distinct from
/// `ROW_HYDRA_OPACITY`, which is how strong this theme's own sketch is drawn.
const ROW_OPACITY_BACKDROP: usize = 1;
const ROW_OPACITY_INTERFACE: usize = 2;
const ROW_OPACITY_EDITOR: usize = 3;
const ROW_TACHYON_MODE: usize = 4;
const ROW_CHARACTER_EFFECT: usize = 5;
const ROW_CHARACTER_STRENGTH: usize = 6;
const ROW_CHARACTER_SPEED: usize = 7;
const ROW_HYDRA_OPACITY: usize = 8;
/// Public: Enter here routes to the code tab, and the app does the routing.
pub const ROW_SKETCH: usize = 9;
const ROW_CARET_SHAPE: usize = 10;
const ROW_EVENT_MARK: usize = 11;
const ROW_BRACKET_MARK: usize = 12;
pub const HEAD_ROWS: usize = 13;

/// Which of the three a row edits.
fn opacity_field(row: usize) -> Option<&'static str> {
    match row {
        ROW_OPACITY_BACKDROP => Some("visuals"),
        ROW_OPACITY_INTERFACE => Some("ui"),
        ROW_OPACITY_EDITOR => Some("editor"),
        _ => None,
    }
}

pub struct ThemeEditor {
    /// The theme being shaped. Every committed change applies to the studio.
    pub draft: Theme,
    /// What the studio wore when the editor opened, for Esc.
    pub original: Box<Theme>,
    /// The theme as it opened, as its file - what "unsaved" is measured
    /// against. A save that keeps editing moves it.
    pub loaded: String,
    /// The name a save writes under.
    pub name: String,
    /// The name the editor was opened under - the one name a save may
    /// overwrite without being a conflict. Empty for new themes and
    /// built-ins.
    pub opened_as: String,
    /// Whether the draft started from a compiled-in theme - those are a
    /// fixed vocabulary, so saving demands a new name.
    pub from_built_in: bool,
    pub tab: EditorTab,
    pub selected: usize,
    /// A value being typed into on the form, when one is.
    pub entry: Option<String>,
    /// The code tab: the theme as its file, in a real editor.
    pub code: Editor,
    /// When the code last changed and has not been parsed yet.
    pub code_dirty_at: Option<Instant>,
    /// Why the code does not parse, when it does not.
    pub code_error: Option<String>,
    /// The save sheet, when it is up: a name, and which button is armed.
    pub saving: Option<SaveSheet>,
    /// The colour picker, when a colour row opened it.
    pub picker: Option<ColorPicker>,
}

/// The little sheet that names a theme and saves it - plain save keeps
/// editing, save-and-keep makes it the studio's theme and closes.
#[derive(Clone, Debug)]
pub struct SaveSheet {
    pub name: String,
    /// true = the "save & keep" button is armed.
    pub keep: bool,
    /// Why the last Enter did not save, when it did not.
    pub note: Option<String>,
    /// Raised by Esc over unsaved changes rather than by `s`: Enter saves
    /// and leaves, a second Esc leaves without.
    pub closing: bool,
}

/// A paint-tool colour picker: a hue-by-lightness grid walked with the
/// arrows, applying LIVE as the cursor moves, with the hex editable
/// beside it. Esc puts the previous colour back; Enter keeps.
#[derive(Clone, Debug)]
pub struct ColorPicker {
    /// Which colour field is being painted, as an index into [`COLORS`].
    pub field: usize,
    /// What the field held when the picker opened - Esc's target.
    pub previous: Option<Color>,
    pub column: i32,
    pub row: i32,
    /// The hex line, editable; kept in step with the grid cursor.
    pub hex: String,
}

/// The picker grid: hue across, lightness down, and a grey ramp on the
/// last row.
pub const PICKER_COLUMNS: i32 = 16;
pub const PICKER_ROWS: i32 = 7;

impl ColorPicker {
    pub fn open(field: usize, previous: Option<Color>) -> Self {
        let mut picker = Self {
            field,
            previous,
            column: 0,
            row: 3,
            hex: String::new(),
        };
        // Start the cursor near the colour that is already there, so a
        // nudge is a variation rather than a jump to red.
        if let Some((red, green, blue)) = previous.and_then(|color| match color {
            Color::Rgb(red, green, blue) => Some((red, green, blue)),
            _ => None,
        }) {
            let (mut best, mut close) = ((0, 3), u32::MAX);
            for column in 0..PICKER_COLUMNS {
                for row in 0..PICKER_ROWS {
                    let Color::Rgb(r, g, b) = Self::color_at(column, row) else {
                        continue;
                    };
                    let distance = (i32::from(r) - i32::from(red)).unsigned_abs().pow(2)
                        + (i32::from(g) - i32::from(green)).unsigned_abs().pow(2)
                        + (i32::from(b) - i32::from(blue)).unsigned_abs().pow(2);
                    if distance < close {
                        close = distance;
                        best = (column, row);
                    }
                }
            }
            picker.column = best.0;
            picker.row = best.1;
        }
        picker.hex = Self::hex_of(picker.current());
        picker
    }

    /// The grid's colour at a cell: hues across the upper rows, greys
    /// along the bottom one.
    pub fn color_at(column: i32, row: i32) -> Color {
        if row == PICKER_ROWS - 1 {
            let level = (column * 255 / (PICKER_COLUMNS - 1)).clamp(0, 255) as u8;
            return Color::Rgb(level, level, level);
        }
        let hue = column as f32 / PICKER_COLUMNS as f32 * 360.0;
        let light = 0.82 - row as f32 * 0.13;
        hsl_to_rgb(hue, 0.62, light)
    }

    pub fn current(&self) -> Color {
        Self::color_at(self.column, self.row)
    }

    pub fn hex_of(color: Color) -> String {
        match color {
            Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
            _ => String::new(),
        }
    }

    /// Move the cursor and refresh the hex line to match.
    pub fn step(&mut self, delta_column: i32, delta_row: i32) {
        self.column = (self.column + delta_column).rem_euclid(PICKER_COLUMNS);
        self.row = (self.row + delta_row).rem_euclid(PICKER_ROWS);
        self.hex = Self::hex_of(self.current());
    }

    /// The colour the picker means right now: a hex that parses wins over
    /// the grid cursor, so a pasted `#ff8800` is exactly that.
    pub fn chosen(&self) -> Option<Color> {
        super::theme::parse_color(self.hex.trim()).or(Some(self.current()))
    }
}

/// One HSL point as RGB, for the picker's grid.
fn hsl_to_rgb(hue: f32, saturation: f32, light: f32) -> Color {
    let c = (1.0 - (2.0 * light - 1.0).abs()) * saturation;
    let h = hue / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = light - c / 2.0;
    let channel = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(channel(r), channel(g), channel(b))
}

/// A name like `base`, not colliding with anything `exists` says is
/// taken: `base`, then `base-2`, `base-3`…
pub fn unique_name(base: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(base) {
        return base.to_owned();
    }
    for counter in 2u32.. {
        let candidate = format!("{base}-{counter}");
        if !exists(&candidate) {
            return candidate;
        }
    }
    unreachable!("the counter finds a free name before it wraps")
}

impl ThemeEditor {
    pub fn open(theme: &Theme, current: &Theme, name: String, from_built_in: bool) -> Self {
        let json = theme.to_json();
        Self {
            draft: theme.clone(),
            original: Box::new(current.clone()),
            loaded: json.clone(),
            opened_as: name.clone(),
            name,
            from_built_in,
            tab: EditorTab::Form,
            selected: 0,
            entry: None,
            code: Editor::new(&json)
                .unwrap_or_else(|_| Editor::new("{}").expect("an empty document always opens")),
            code_dirty_at: None,
            code_error: None,
            saving: None,
            picker: None,
        }
    }

    /// Whether `row` is a colour row - the ones whose Enter opens the
    /// picker rather than a text entry.
    pub fn is_color_row(row: usize) -> bool {
        row >= HEAD_ROWS
    }

    /// The colour field index a colour row means.
    pub fn color_field_of(row: usize) -> Option<usize> {
        row.checked_sub(HEAD_ROWS)
            .filter(|index| *index < COLORS.len())
    }

    /// What the selected colour row holds right now.
    pub fn color_of_row(&self, row: usize) -> Option<Color> {
        Self::color_field_of(row).and_then(|index| (COLORS[index].get)(&self.draft))
    }

    /// Apply the picker's colour into the draft, live.
    pub fn paint(&mut self) -> bool {
        let Some(picker) = &self.picker else {
            return false;
        };
        let Some(field) = COLORS.get(picker.field) else {
            return false;
        };
        let Some(color) = picker.chosen() else {
            return false;
        };
        (field.set)(&mut self.draft, Some(color));
        true
    }

    /// Put the colour from before the picker opened back.
    pub fn unpaint(&mut self) -> bool {
        let Some(picker) = &self.picker else {
            return false;
        };
        let Some(field) = COLORS.get(picker.field) else {
            return false;
        };
        (field.set)(&mut self.draft, picker.previous);
        true
    }

    pub fn rows(&self) -> usize {
        HEAD_ROWS + COLORS.len()
    }

    /// Rebuild the code tab from the draft, after a form edit.
    pub fn rebuild_code(&mut self) {
        let json = self.draft.to_json();
        if let Ok(editor) = Editor::new(&json) {
            self.code = editor;
        }
        self.code_dirty_at = None;
        self.code_error = None;
    }

    /// Parse the code tab back into the draft. `Ok(true)` when the draft
    /// changed.
    pub fn parse_code(&mut self) -> bool {
        self.code_dirty_at = None;
        match Theme::from_json(&self.code.source()) {
            Ok(mut theme) => {
                theme.name = if self.name.is_empty() {
                    theme.name
                } else {
                    self.name.clone()
                };
                self.code_error = None;
                let changed = self.draft.to_json() != theme.to_json();
                if changed {
                    self.draft = theme;
                }
                changed
            }
            Err(error) => {
                self.code_error = Some(error.to_string());
                false
            }
        }
    }

    /// What one form row shows: label and value.
    fn row_text(&self, row: usize) -> (String, String) {
        match row {
            ROW_NAME => (
                "name".to_owned(),
                if self.name.is_empty() {
                    "(unnamed - type one before saving)".to_owned()
                } else {
                    self.name.clone()
                },
            ),
            row if opacity_field(row).is_some() => (
                format!("{} opacity", opacity_field(row).unwrap_or_default()),
                match self.draft_opacity(row) {
                    Some(value) => format!("{value}%"),
                    None => format!("{}% - unset", super::theme::THEME_OPACITY_DEFAULT),
                },
            ),
            ROW_TACHYON_MODE => (
                "tachyon mode".to_owned(),
                match self.draft.tachyon_mode() {
                    Some(TachyonMode::Text) => "text".to_owned(),
                    Some(TachyonMode::Image) => "image".to_owned(),
                    None => "not used by this renderer".to_owned(),
                },
            ),
            ROW_CHARACTER_EFFECT => (
                "character fx".to_owned(),
                self.draft
                    .character_visual()
                    .map(|visual| format!("{:?}", visual.effect).to_lowercase())
                    .unwrap_or_else(|| "off".to_owned()),
            ),
            ROW_CHARACTER_STRENGTH => (
                "character strength".to_owned(),
                self.draft.character_visual().map_or_else(
                    || "not used".to_owned(),
                    |visual| format!("{}%", visual.strength),
                ),
            ),
            ROW_CHARACTER_SPEED => (
                "character speed".to_owned(),
                self.draft.character_visual().map_or_else(
                    || "not used".to_owned(),
                    |visual| format!("{}%", visual.speed),
                ),
            ),
            ROW_HYDRA_OPACITY => (
                "sketch opacity".to_owned(),
                match self.draft.hydra_opacity_percent() {
                    Some(value) => format!("{value}%"),
                    None if self.draft.cell_visual().is_some() => {
                        "not used by TachyonFX".to_owned()
                    }
                    None => "45% (default)".to_owned(),
                },
            ),
            ROW_SKETCH => (
                "hydra sketch".to_owned(),
                match self.draft.hydra_code() {
                    Some(code) => code.lines().next().unwrap_or_default().to_owned(),
                    None if self.draft.cell_visual().is_some() => {
                        "TachyonFX renderer - edit it on the code tab".to_owned()
                    }
                    None => "none - edit on the code tab, or Enter for a start".to_owned(),
                },
            ),
            ROW_CARET_SHAPE => (
                "caret shape".to_owned(),
                self.draft.caret_shape.map_or_else(
                    || "settings (inherit)".to_owned(),
                    |shape| shape.label().to_owned(),
                ),
            ),
            ROW_BRACKET_MARK => (
                "bracket mark".to_owned(),
                format!("{:?}", self.draft.bracket_mark).to_lowercase(),
            ),
            ROW_EVENT_MARK => (
                "event mark".to_owned(),
                format!("{:?}", self.draft.event_mark).to_lowercase(),
            ),
            _ => {
                let field = &COLORS[row - HEAD_ROWS];
                let value = match (field.get)(&self.draft) {
                    Some(color) => super::theme::write_color(color),
                    None => "none".to_owned(),
                };
                (field.label.to_owned(), value)
            }
        }
    }

    /// What Enter starts typing with on the selected row: the value already
    /// there, ready to be corrected rather than retyped.
    pub fn entry_seed(&self) -> (String, String) {
        let (label, value) = self.row_text(self.selected);
        let seed = match self.selected {
            ROW_NAME => self.name.clone(),
            row if opacity_field(row).is_some() => self
                .draft_opacity(row)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            ROW_TACHYON_MODE => match self.draft.tachyon_mode() {
                Some(TachyonMode::Text) => "text".to_owned(),
                Some(TachyonMode::Image) => "image".to_owned(),
                None => String::new(),
            },
            ROW_CHARACTER_EFFECT => self
                .draft
                .character_visual()
                .map(|visual| format!("{:?}", visual.effect).to_lowercase())
                .unwrap_or_else(|| "off".to_owned()),
            ROW_CHARACTER_STRENGTH => self
                .draft
                .character_visual()
                .map(|visual| visual.strength.to_string())
                .unwrap_or_default(),
            ROW_CHARACTER_SPEED => self
                .draft
                .character_visual()
                .map(|visual| visual.speed.to_string())
                .unwrap_or_default(),
            ROW_HYDRA_OPACITY => self
                .draft
                .hydra_opacity_percent()
                .map(|value| value.to_string())
                .unwrap_or_default(),
            ROW_CARET_SHAPE => self
                .draft
                .caret_shape
                .map_or_else(|| "inherit".to_owned(), |shape| shape.key().to_owned()),
            ROW_EVENT_MARK => match self.draft.event_mark {
                super::theme::MarkStyle::Tint => "tint".to_owned(),
                super::theme::MarkStyle::Underline => "underline".to_owned(),
                super::theme::MarkStyle::Outline => "outline".to_owned(),
                super::theme::MarkStyle::Text => "text".to_owned(),
                super::theme::MarkStyle::Fill => "fill".to_owned(),
                super::theme::MarkStyle::Invert => "invert".to_owned(),
            },
            _ if self.selected >= HEAD_ROWS => {
                match (COLORS[self.selected - HEAD_ROWS].get)(&self.draft) {
                    Some(color) => super::theme::write_color(color),
                    None => String::new(),
                }
            }
            _ => value,
        };
        (label, seed)
    }

    /// Commit what was typed into the selected row. `Ok(true)` = the draft
    /// changed.
    pub fn commit_entry(&mut self) -> Result<bool, String> {
        let Some(text) = self.entry.take() else {
            return Ok(false);
        };
        let text = text.trim().to_owned();
        match self.selected {
            ROW_NAME => {
                self.name = text;
                Ok(false)
            }
            _ if self.selected >= HEAD_ROWS => {
                let field = &COLORS[self.selected - HEAD_ROWS];
                if field.optional && (text.is_empty() || text.eq_ignore_ascii_case("none")) {
                    (field.set)(&mut self.draft, None);
                    return Ok(true);
                }
                match parse_color(&text) {
                    Some(color) => {
                        (field.set)(&mut self.draft, Some(color));
                        Ok(true)
                    }
                    None => Err(format!(
                        "{text:?} is not a colour - #rrggbb, a CSS name, ansiN or reset"
                    )),
                }
            }
            row if opacity_field(row).is_some() => {
                let value = if text.is_empty() || text.eq_ignore_ascii_case("unset") {
                    None
                } else {
                    match text.trim_end_matches('%').parse::<u8>() {
                        Ok(value) if value <= 100 => Some(value),
                        _ => return Err(format!("{text:?} is not an opacity - 0 to 100")),
                    }
                };
                self.set_draft_opacity(row, value);
                Ok(true)
            }
            ROW_HYDRA_OPACITY => {
                let value = if text.is_empty() || text.eq_ignore_ascii_case("unset") {
                    None
                } else {
                    let digits = text.trim_end_matches('%');
                    match digits.parse::<u8>() {
                        Ok(value) if value <= 100 => Some(value),
                        _ => return Err(format!("{text:?} is not an opacity - 0 to 100")),
                    }
                };
                if !self.draft.set_hydra_opacity(value) {
                    return Err(
                        "TachyonFX renderers have no sketch opacity - edit visual on the code tab"
                            .into(),
                    );
                }
                Ok(true)
            }
            ROW_TACHYON_MODE => {
                let mode = match text.to_ascii_lowercase().as_str() {
                    "text" => TachyonMode::Text,
                    "image" => TachyonMode::Image,
                    _ => return Err(format!("{text:?} is not a TachyonFX mode - text or image")),
                };
                if !self.draft.set_tachyon_mode(mode) {
                    return Err("this theme does not use the TachyonFX renderer".into());
                }
                Ok(true)
            }
            ROW_CHARACTER_EFFECT => {
                let effect = match text.to_ascii_lowercase().as_str() {
                    "off" | "none" | "unset" | "" => None,
                    "fade" => Some(CharacterEffect::Fade),
                    "aurora" => Some(CharacterEffect::Aurora),
                    "hologram" => Some(CharacterEffect::Hologram),
                    "rainbow" => Some(CharacterEffect::Rainbow),
                    _ => {
                        return Err(format!(
                            "{text:?} is not a character effect - off, fade, aurora, hologram or rainbow"
                        ));
                    }
                };
                Ok(self.draft.set_character_effect(effect))
            }
            ROW_CHARACTER_STRENGTH | ROW_CHARACTER_SPEED => {
                let value = text.trim_end_matches('%').parse::<u8>().ok();
                let Some(value) = value.filter(|value| (1..=100).contains(value)) else {
                    return Err(format!("{text:?} is not a value - 1 to 100"));
                };
                let changed = if self.selected == ROW_CHARACTER_STRENGTH {
                    self.draft.set_character_strength(value)
                } else {
                    self.draft.set_character_speed(value)
                };
                if !changed {
                    return Err("choose a character effect first".into());
                }
                Ok(true)
            }
            ROW_CARET_SHAPE => {
                self.draft.caret_shape = match text.to_ascii_lowercase().as_str() {
                    "" | "inherit" | "settings" | "unset" | "none" => None,
                    key => Some(CaretShape::parse(key).ok_or_else(|| {
                        format!(
                            "{text:?} is not a caret shape - steady-bar, blinking-bar, steady-block, blinking-block, steady-underline, blinking-underline or inherit"
                        )
                    })?),
                };
                Ok(true)
            }
            ROW_BRACKET_MARK => {
                self.draft.bracket_mark = match text.to_ascii_lowercase().as_str() {
                    "auto" => BracketMark::Auto,
                    "underline" => BracketMark::Underline,
                    "block" => BracketMark::Block,
                    _ => {
                        return Err(format!(
                            "{text:?} is not a bracket mark - auto, underline or block"
                        ));
                    }
                };
                Ok(true)
            }
            ROW_EVENT_MARK => {
                let style = match text.to_ascii_lowercase().as_str() {
                    "tint" => super::theme::MarkStyle::Tint,
                    "underline" => super::theme::MarkStyle::Underline,
                    "outline" => super::theme::MarkStyle::Outline,
                    "text" => super::theme::MarkStyle::Text,
                    "fill" => super::theme::MarkStyle::Fill,
                    "invert" => super::theme::MarkStyle::Invert,
                    _ => {
                        return Err(format!(
                            "{text:?} is not a mark style - tint, underline, outline, text, fill or invert"
                        ));
                    }
                };
                self.draft.event_mark = style;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Whether leaving now would lose something: a value half-typed, code
    /// typed and not yet parsed, or a draft that no longer matches the file
    /// it opened as.
    pub fn is_modified(&self) -> bool {
        self.entry.is_some() || self.code_dirty_at.is_some() || self.draft.to_json() != self.loaded
    }

    /// Bring every half-typed thing into the draft: an open entry commits,
    /// a dirty code tab parses. `Ok(true)` = the draft changed. An error is
    /// the entry's or the parser's, and the state that caused it is kept so
    /// the author can see and fix it.
    pub fn settle(&mut self) -> Result<bool, String> {
        let mut changed = false;
        if self.entry.is_some() {
            changed |= self.commit_entry()?;
        }
        if self.code_dirty_at.take().is_some() {
            changed |= self.parse_code();
            if let Some(error) = &self.code_error {
                return Err(format!("the code tab does not parse: {error}"));
            }
        }
        Ok(changed)
    }

    /// Read one of the draft's three suggested opacities.
    ///
    /// `ui_opacity` answers for the interface value when the newer key is
    /// silent, so opening a theme written before the three existed shows what
    /// that theme has always asked for rather than an empty row.
    fn draft_opacity(&self, row: usize) -> Option<u8> {
        match row {
            ROW_OPACITY_BACKDROP => self.draft.opacity.backdrop,
            ROW_OPACITY_INTERFACE => self.draft.opacity.interface.or(self.draft.ui_opacity),
            ROW_OPACITY_EDITOR => self.draft.opacity.editor,
            _ => None,
        }
    }

    fn set_draft_opacity(&mut self, row: usize, value: Option<u8>) {
        match row {
            ROW_OPACITY_BACKDROP => self.draft.opacity.backdrop = value,
            ROW_OPACITY_INTERFACE => {
                self.draft.opacity.interface = value;
                // The older single-value key would otherwise go on answering
                // for a row the reader has just cleared.
                self.draft.ui_opacity = None;
            }
            ROW_OPACITY_EDITOR => self.draft.opacity.editor = value,
            _ => {}
        }
    }

    /// Step the selected row sideways. `true` = the draft changed.
    pub fn step(&mut self, forwards: bool) -> bool {
        let delta: i16 = if forwards { 5 } else { -5 };
        match self.selected {
            row if opacity_field(row).is_some() => {
                let base = self
                    .draft_opacity(row)
                    .unwrap_or(super::theme::THEME_OPACITY_DEFAULT);
                self.set_draft_opacity(row, Some((base as i16 + delta).clamp(0, 100) as u8));
                true
            }
            ROW_HYDRA_OPACITY => {
                let value = self.draft.hydra_opacity_percent().unwrap_or(45) as i16 + delta;
                self.draft
                    .set_hydra_opacity(Some(value.clamp(0, 100) as u8))
            }
            ROW_TACHYON_MODE => match self.draft.tachyon_mode() {
                Some(TachyonMode::Text) => self.draft.set_tachyon_mode(TachyonMode::Image),
                Some(TachyonMode::Image) => self.draft.set_tachyon_mode(TachyonMode::Text),
                None => false,
            },
            ROW_CHARACTER_EFFECT => {
                let next = match (
                    self.draft.character_visual().map(|visual| visual.effect),
                    forwards,
                ) {
                    (None, true) => Some(CharacterEffect::Fade),
                    (Some(CharacterEffect::Fade), true) => Some(CharacterEffect::Aurora),
                    (Some(CharacterEffect::Aurora), true) => Some(CharacterEffect::Hologram),
                    (Some(CharacterEffect::Hologram), true) => Some(CharacterEffect::Rainbow),
                    (Some(CharacterEffect::Rainbow), true) => None,
                    (None, false) => Some(CharacterEffect::Rainbow),
                    (Some(CharacterEffect::Fade), false) => None,
                    (Some(CharacterEffect::Rainbow), false) => Some(CharacterEffect::Hologram),
                    (Some(CharacterEffect::Hologram), false) => Some(CharacterEffect::Aurora),
                    (Some(CharacterEffect::Aurora), false) => Some(CharacterEffect::Fade),
                };
                self.draft.set_character_effect(next)
            }
            ROW_CHARACTER_STRENGTH => {
                let Some(visual) = self.draft.character_visual() else {
                    return false;
                };
                self.draft
                    .set_character_strength((i16::from(visual.strength) + delta).clamp(1, 100) as u8)
            }
            ROW_CHARACTER_SPEED => {
                let Some(visual) = self.draft.character_visual() else {
                    return false;
                };
                self.draft
                    .set_character_speed((i16::from(visual.speed) + delta).clamp(1, 100) as u8)
            }
            ROW_CARET_SHAPE => {
                let choices = [
                    None,
                    Some(CaretShape::SteadyBar),
                    Some(CaretShape::BlinkingBar),
                    Some(CaretShape::SteadyBlock),
                    Some(CaretShape::BlinkingBlock),
                    Some(CaretShape::SteadyUnderline),
                    Some(CaretShape::BlinkingUnderline),
                ];
                let at = choices
                    .iter()
                    .position(|shape| *shape == self.draft.caret_shape)
                    .unwrap_or(0);
                let next = if forwards {
                    (at + 1) % choices.len()
                } else {
                    (at + choices.len() - 1) % choices.len()
                };
                self.draft.caret_shape = choices[next];
                true
            }
            ROW_BRACKET_MARK => {
                self.draft.bracket_mark = match (self.draft.bracket_mark, forwards) {
                    (BracketMark::Auto, true) | (BracketMark::Block, false) => {
                        BracketMark::Underline
                    }
                    (BracketMark::Underline, true) | (BracketMark::Auto, false) => {
                        BracketMark::Block
                    }
                    (BracketMark::Block, true) | (BracketMark::Underline, false) => {
                        BracketMark::Auto
                    }
                };
                true
            }
            ROW_EVENT_MARK => {
                self.draft.event_mark = match (self.draft.event_mark, forwards) {
                    (MarkStyle::Tint, true) => MarkStyle::Underline,
                    (MarkStyle::Underline, true) => MarkStyle::Outline,
                    (MarkStyle::Outline, true) => MarkStyle::Text,
                    (MarkStyle::Text, true) => MarkStyle::Fill,
                    (MarkStyle::Fill, true) => MarkStyle::Invert,
                    (MarkStyle::Invert, true) => MarkStyle::Tint,
                    (MarkStyle::Tint, false) => MarkStyle::Invert,
                    (MarkStyle::Underline, false) => MarkStyle::Tint,
                    (MarkStyle::Outline, false) => MarkStyle::Underline,
                    (MarkStyle::Text, false) => MarkStyle::Outline,
                    (MarkStyle::Fill, false) => MarkStyle::Text,
                    (MarkStyle::Invert, false) => MarkStyle::Fill,
                };
                true
            }
            _ => false,
        }
    }

    /// A starting sketch for a theme that has none, so Enter on the row is
    /// never a dead key.
    pub fn seed_sketch(&mut self) -> bool {
        if self.draft.hydra_code().is_some() || self.draft.cell_visual().is_some() {
            return false;
        }
        self.draft.set_hydra_code(
            "osc(6, 0.04, 0.7)\n  .colorama(0.01)\n  .contrast(1.2)\n  .out()".to_owned(),
        );
        if self.draft.hydra_opacity_percent().is_none() {
            self.draft.set_hydra_opacity(Some(35));
        }
        true
    }
}

/// The sheet, drawn over everything but the toast.
pub struct ThemeEditorView<'a> {
    pub keybinds: &'a super::keybinds::Keybinds,
    pub editor: &'a mut ThemeEditor,
    pub theme: &'a Theme,
    pub focused: bool,
}

impl ThemeEditorView<'_> {
    /// The sheet stands at the right, in the picker's corner but taller.
    /// It is a side panel and never covers the whole studio: the theme
    /// under edit applies to the whole screen and must stay visible.
    pub fn geometry(available: Rect) -> Option<Rect> {
        if available.width < 50 || available.height < 14 {
            return None;
        }
        let width = 46u16;
        let height = available.height.saturating_sub(3).min(30);
        Some(Rect::new(
            available.right().saturating_sub(width + 2),
            available
                .bottom()
                .saturating_sub(height + 1)
                .max(available.y + 1),
            width,
            height,
        ))
    }

    /// The header tab a click means, if it lands on one.
    pub fn tab_at(available: Rect, x: u16, y: u16) -> Option<EditorTab> {
        let area = Self::geometry(available)?;
        if y != area.y {
            return None;
        }
        if x >= area.x + 2 && x < area.x + 12 {
            return Some(EditorTab::Form);
        }
        if x >= area.x + 13 && x < area.x + 19 {
            return Some(EditorTab::Code);
        }
        None
    }

    /// The form row a click lands on, if any.
    pub fn form_row_at(editor: &ThemeEditor, available: Rect, x: u16, y: u16) -> Option<usize> {
        let area = Self::geometry(available)?;
        let inner = Self::inner_of(area);
        if x < inner.x || x >= inner.right() || y < inner.y || y >= inner.bottom() {
            return None;
        }
        let rows = editor.rows();
        let visible = usize::from(inner.height);
        let first = editor
            .selected
            .saturating_sub(visible.saturating_sub(1) / 2)
            .min(rows.saturating_sub(visible));
        let row = first + usize::from(y - inner.y);
        (row < rows).then_some(row)
    }

    /// Where the code tab's text sits, for routing a click to the caret.
    pub fn code_area(available: Rect) -> Option<Rect> {
        let area = Self::geometry(available)?;
        let inner = Self::inner_of(area);
        Some(Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        ))
    }

    fn inner_of(area: Rect) -> Rect {
        // One row under the tabs belongs to the subtitle line.
        Rect::new(
            area.x + 2,
            area.y + 2,
            area.width.saturating_sub(4),
            area.height.saturating_sub(4),
        )
    }

    pub fn render(self, available: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        let Some(area) = Self::geometry(available) else {
            // Too small to draw the sheet - but the editor is still open
            // and holding the keyboard, so it must say so rather than be
            // an invisible modal.
            if available.width > 0 && available.height > 0 {
                buffer.set_stringn(
                    available.x,
                    available.y,
                    "theme editor open - more room needed; Esc goes back to the themes",
                    usize::from(available.width),
                    Style::default().fg(theme.warn).bg(theme.overlay),
                );
            }
            return;
        };
        super::view::clear_overlay(
            buffer,
            area,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        super::devices::draw_border(buffer, area, theme);
        let tab_style = |active: bool| {
            if active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            }
        };
        buffer.set_stringn(
            area.x + 2,
            area.y,
            " settings ",
            10,
            tab_style(self.editor.tab == EditorTab::Form),
        );
        buffer.set_stringn(
            area.x + 13,
            area.y,
            " code ",
            6,
            tab_style(self.editor.tab == EditorTab::Code),
        );
        let subtitle = if self.editor.from_built_in {
            "varying a built-in - saving asks for a new name".to_owned()
        } else if self.editor.name.trim().is_empty() {
            "a new theme - s saves it under a name".to_owned()
        } else {
            self.editor.name.trim().to_owned()
        };
        buffer.set_stringn(
            area.x + 2,
            area.y + 1,
            subtitle,
            usize::from(area.width.saturating_sub(4)),
            Style::default().fg(theme.muted),
        );

        let inner = Self::inner_of(area);
        let focused = self.focused;
        match self.editor.tab {
            EditorTab::Form => render_form(self.editor, inner, buffer, theme, focused),
            EditorTab::Code => render_code(self.editor, inner, buffer, theme, focused),
        }
        if self.editor.picker.is_some() {
            render_picker(self.editor, area, buffer, theme);
        }
        if self.editor.saving.is_some() {
            render_save_sheet(self.editor, area, buffer, theme);
        }

        let copy = self
            .keybinds
            .binding(super::keybinds::BindAction::Copy)
            .map(|binding| format!(" · {} copies", binding.hint()))
            .unwrap_or_default();
        let code_hint = format!("type · drag selects{copy} · Tab form · Esc themes");
        let hint = if let Some(saving) = &self.editor.saving {
            if saving.closing {
                "type the name · Tab picks the button · Enter saves · Esc discards"
            } else {
                "type the name · Tab picks the button · Enter saves · Esc cancels"
            }
        } else if self.editor.picker.is_some() {
            "arrows paint live · type a hex · Enter keeps · Esc puts it back"
        } else {
            match self.editor.tab {
                EditorTab::Form => "↑↓ rows · Enter opens · ←/→ steps · s saves · Esc themes",
                EditorTab::Code => &code_hint,
            }
        };
        buffer.set_stringn(
            area.x + 2,
            area.bottom().saturating_sub(2),
            hint,
            usize::from(area.width.saturating_sub(4)),
            Style::default().fg(theme.muted),
        );
    }
}

/// Where the painter's sheet sits over the editor's area.
pub fn picker_sheet(area: Rect) -> Rect {
    let grid_width = (PICKER_COLUMNS * 2) as u16;
    let height = PICKER_ROWS as u16 + 4;
    Rect::new(
        area.x + (area.width.saturating_sub(grid_width + 4)) / 2,
        area.bottom().saturating_sub(height + 2),
        grid_width + 4,
        height,
    )
}

/// The swatch a click on the painter lands on, if any.
pub fn picker_cell_at(area: Rect, x: u16, y: u16) -> Option<(i32, i32)> {
    let sheet = picker_sheet(area);
    let grid_x = sheet.x + 2;
    let grid_y = sheet.y + 1;
    if y < grid_y || y >= grid_y + PICKER_ROWS as u16 {
        return None;
    }
    if x < grid_x || x >= grid_x + (PICKER_COLUMNS * 2) as u16 {
        return None;
    }
    Some((i32::from((x - grid_x) / 2), i32::from(y - grid_y)))
}

/// The colour picker, drawn over the sheet's lower half: the grid, the
/// hex line, and the field it paints.
fn render_picker(editor: &ThemeEditor, area: Rect, buffer: &mut Buffer, theme: &Theme) {
    let Some(picker) = &editor.picker else {
        return;
    };
    let sheet = picker_sheet(area);
    super::view::clear_overlay(
        buffer,
        sheet,
        Style::default().bg(theme.surface).fg(theme.foreground),
    );
    super::devices::draw_border(buffer, sheet, theme);
    let label = COLORS
        .get(picker.field)
        .map(|field| field.label)
        .unwrap_or("colour");
    buffer.set_stringn(
        sheet.x + 2,
        sheet.y,
        format!(" {label} "),
        usize::from(sheet.width.saturating_sub(4)),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    for row in 0..PICKER_ROWS {
        for column in 0..PICKER_COLUMNS {
            let x = sheet.x + 2 + (column * 2) as u16;
            let y = sheet.y + 1 + row as u16;
            let color = ColorPicker::color_at(column, row);
            let cursor = column == picker.column && row == picker.row;
            let text = if cursor { "◆ " } else { "  " };
            let mark = if cursor {
                // Black or white, whichever reads on the swatch.
                if super::theme::luminance(color) > 0.5 {
                    Color::Rgb(0, 0, 0)
                } else {
                    Color::Rgb(255, 255, 255)
                }
            } else {
                color
            };
            buffer.set_stringn(x, y, text, 2, Style::default().bg(color).fg(mark));
        }
    }
    buffer.set_stringn(
        sheet.x + 2,
        sheet.y + 1 + PICKER_ROWS as u16,
        format!("hex {}{}", picker.hex, super::terminal::symbol("▏")),
        usize::from(sheet.width.saturating_sub(4)),
        Style::default().fg(theme.foreground),
    );
}

/// The save sheet: the name line, where the file will land, and the two
/// buttons.
fn render_save_sheet(editor: &ThemeEditor, area: Rect, buffer: &mut Buffer, theme: &Theme) {
    let Some(saving) = &editor.saving else {
        return;
    };
    let width = area.width.saturating_sub(6).min(40);
    let sheet = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + area.height / 2 - 3,
        width,
        6,
    );
    super::view::clear_overlay(
        buffer,
        sheet,
        Style::default().bg(theme.surface).fg(theme.foreground),
    );
    super::devices::draw_border(buffer, sheet, theme);
    let title = if saving.closing {
        " unsaved changes "
    } else {
        " save theme "
    };
    buffer.set_stringn(
        sheet.x + 2,
        sheet.y,
        title,
        title.len(),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    buffer.set_stringn(
        sheet.x + 2,
        sheet.y + 1,
        format!("name: {}{}", saving.name, super::terminal::symbol("▏")),
        usize::from(sheet.width.saturating_sub(4)),
        Style::default().fg(theme.foreground),
    );
    let button = |armed: bool| {
        if armed {
            Style::default()
                .fg(theme.background)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        }
    };
    buffer.set_stringn(sheet.x + 2, sheet.y + 3, " save ", 6, button(!saving.keep));
    buffer.set_stringn(
        sheet.x + 10,
        sheet.y + 3,
        " save & keep ",
        13,
        button(saving.keep),
    );
    if let Some(note) = &saving.note {
        buffer.set_stringn(
            sheet.x + 2,
            sheet.y + 4,
            note,
            usize::from(sheet.width.saturating_sub(4)),
            Style::default().fg(theme.warn),
        );
    }
}

fn render_form(
    editor: &ThemeEditor,
    inner: Rect,
    buffer: &mut Buffer,
    theme: &Theme,
    focused: bool,
) {
    let rows = editor.rows();
    let visible = usize::from(inner.height);
    let first = editor
        .selected
        .saturating_sub(visible.saturating_sub(1) / 2)
        .min(rows.saturating_sub(visible));
    for (offset, row) in (first..rows).take(visible).enumerate() {
        let y = inner.y + offset as u16;
        let selected = row == editor.selected;
        let (label, value) = editor.row_text(row);
        let shown = match (&editor.entry, selected) {
            (Some(entry), true) if focused => {
                format!("{entry}{}", super::terminal::symbol("▏"))
            }
            (Some(entry), true) => entry.clone(),
            _ => value,
        };
        // The swatch: the colour itself, twice as wide as a glyph, so the
        // form is a palette and not a list of hex numbers.
        if row >= HEAD_ROWS {
            if let Some(color) = (COLORS[row - HEAD_ROWS].get)(&editor.draft) {
                buffer.set_stringn(inner.x, y, "  ", 2, Style::default().bg(color));
            } else {
                buffer.set_stringn(inner.x, y, "··", 2, Style::default().fg(theme.muted));
            }
        }
        let marker = if selected {
            format!("{} ", crate::terminal::symbol("▸"))
        } else {
            "  ".to_owned()
        };
        let style = if selected {
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        buffer.set_stringn(
            inner.x + 3,
            y,
            format!("{marker}{label:<18} {shown}"),
            usize::from(inner.width.saturating_sub(3)),
            style,
        );
    }
}

fn render_code(
    editor: &mut ThemeEditor,
    inner: Rect,
    buffer: &mut Buffer,
    theme: &Theme,
    focused: bool,
) {
    // The status line first: the code is live, and it says so, or says what
    // is wrong while nothing is applied.
    let status_y = inner.bottom().saturating_sub(1);
    let (status, style) = match &editor.code_error {
        Some(error) => (format!("✗ {error}"), Style::default().fg(theme.error)),
        None if editor.code_dirty_at.is_some() => {
            ("…".to_owned(), Style::default().fg(theme.muted))
        }
        None => (
            "✓ valid - applied live".to_owned(),
            Style::default().fg(theme.ok),
        ),
    };
    buffer.set_stringn(inner.x, status_y, status, usize::from(inner.width), style);

    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    if body.is_empty() {
        return;
    }
    let grid = GridRect::new(body.x, body.y, body.width, body.height);
    editor
        .code
        .set_view_size(usize::from(body.width), usize::from(body.height));
    let Ok(map) = editor.code.screen_map(grid) else {
        return;
    };
    // The selection's bytes, so that a mouse drag is visible. The band
    // sets only the background, like the score's, so the syntax colours
    // stay.
    let selections: Vec<std::ops::Range<usize>> = editor
        .code
        .selections()
        .ranges()
        .iter()
        .filter(|selection| !selection.is_empty())
        .map(|selection| {
            let ordered = selection.ordered();
            ordered.start.0..ordered.end.0
        })
        .collect();
    for row in map.rows() {
        let ScreenRow::Text(row) = row else { continue };
        let mut lexer = Lexer::default();
        for (index, cell) in row.cells.iter().enumerate() {
            let lookahead = row.cells.get(index + 1).map(|next| next.display.as_str());
            let mut style = Style::default().fg(lexer.next(&cell.display, lookahead).color(theme));
            let selected = selections
                .iter()
                .any(|range| cell.bytes.start.0 < range.end && range.start < cell.bytes.end.0);
            if selected {
                style = style.bg(theme.selection);
            }
            buffer.set_stringn(
                cell.screen_x.start,
                row.screen_y,
                &cell.display,
                usize::from(cell.screen_x.end.saturating_sub(cell.screen_x.start)),
                style,
            );
        }
    }
    // The one caret that matters here is the code's - and only while the
    // keys actually land here.
    if focused
        && let Some(position) = map.cell_for_offset(editor.code.primary_selection().head)
        && let Some(cell) = buffer.cell_mut((position.x, position.y))
    {
        cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> ThemeEditor {
        let theme = Theme::built_in_default();
        ThemeEditor::open(&theme, &theme, "mine".into(), true)
    }

    /// The two tabs are one theme: a form edit lands in the code, a code
    /// edit lands in the form.
    #[test]
    fn the_form_and_the_code_are_one_theme() {
        let mut editor = editor();
        // Form → code.
        editor.selected = HEAD_ROWS; // background
        editor.entry = Some("#102030".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        editor.rebuild_code();
        assert!(
            editor.code.source().contains("#102030"),
            "{}",
            editor.code.source()
        );

        // Code → form.
        let swapped = editor.code.source().replacen("#102030", "#a0b0c0", 1);
        editor.code = Editor::new(&swapped).unwrap();
        assert!(editor.parse_code(), "a changed colour is a changed draft");
        assert_eq!(
            super::super::theme::write_color(editor.draft.background),
            "#a0b0c0"
        );
        assert!(editor.code_error.is_none());
    }

    /// Broken code reports and applies nothing.
    #[test]
    fn broken_code_reports_and_applies_nothing() {
        let mut editor = editor();
        let before = editor.draft.to_json();
        editor.code = Editor::new("{ not json").unwrap();
        assert!(!editor.parse_code());
        assert!(editor.code_error.is_some());
        assert_eq!(editor.draft.to_json(), before, "the draft is untouched");
    }

    /// A colour that does not parse is refused with the accepted spellings.
    #[test]
    fn a_bad_colour_is_refused_with_the_spellings_named() {
        let mut editor = editor();
        editor.selected = HEAD_ROWS;
        editor.entry = Some("rgb(1,2,3)".into());
        let error = editor.commit_entry().expect_err("refused");
        assert!(error.contains("#rrggbb"), "{error}");

        // An optional colour can be told "none".
        let bracket = HEAD_ROWS
            + COLORS
                .iter()
                .position(|field| field.label == "bracket")
                .unwrap();
        editor.selected = bracket;
        editor.entry = Some("none".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(editor.draft.bracket, None);
    }

    /// A typed colour can carry multi-byte characters; the entry refuses it
    /// with its message rather than panicking mid-parse.
    #[test]
    fn a_multi_byte_colour_is_refused_without_panicking() {
        let mut editor = editor();
        editor.selected = HEAD_ROWS;
        editor.entry = Some("#éa".into());
        let error = editor.commit_entry().expect_err("refused");
        assert!(error.contains("#rrggbb"), "{error}");
    }

    /// Opacity rows step in fives and stay inside 0..=100.
    #[test]
    fn the_opacity_rows_step_and_clamp() {
        let mut editor = editor();
        editor.selected = ROW_OPACITY_INTERFACE;
        assert!(editor.step(true));
        // Stepping starts from the theme's own suggestion, not from a number
        // this row invented.
        assert_eq!(
            editor.draft.opacity.interface,
            Some(super::super::theme::THEME_OPACITY_DEFAULT + 5)
        );
        for _ in 0..40 {
            editor.step(true);
        }
        assert_eq!(editor.draft.opacity.interface, Some(100));
    }

    #[test]
    fn tachyon_mode_is_visible_editable_and_limited_to_native_themes() {
        let burn = super::super::theme::Theme::built_in("burn").expect("burn theme");
        let mut editor = ThemeEditor::open(&burn, &burn, "mine".into(), false);
        editor.selected = ROW_TACHYON_MODE;
        assert_eq!(editor.entry_seed().1, "image");
        assert!(editor.step(true));
        assert_eq!(editor.draft.tachyon_mode(), Some(TachyonMode::Text));
        editor.entry = Some("image".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(editor.draft.tachyon_mode(), Some(TachyonMode::Image));
        editor.entry = Some("canvas".into());
        assert!(editor.commit_entry().is_err());

        let hydra = super::super::theme::Theme::built_in("prism").expect("prism theme");
        let mut hydra_editor = ThemeEditor::open(&hydra, &hydra, "mine".into(), false);
        hydra_editor.selected = ROW_TACHYON_MODE;
        assert!(!hydra_editor.step(true));
        hydra_editor.entry = Some("image".into());
        assert!(hydra_editor.commit_entry().is_err());
    }

    #[test]
    fn character_effects_can_be_chosen_tuned_and_turned_off_in_the_form() {
        let theme = Theme::built_in_default();
        let mut editor = ThemeEditor::open(&theme, &theme, "mine".into(), false);
        editor.selected = ROW_CHARACTER_EFFECT;
        assert_eq!(editor.entry_seed().1, "off");
        assert!(editor.step(true));
        assert_eq!(
            editor.draft.character_visual().map(|visual| visual.effect),
            Some(CharacterEffect::Fade)
        );
        assert!(editor.step(true));
        assert_eq!(
            editor.draft.character_visual().map(|visual| visual.effect),
            Some(CharacterEffect::Aurora)
        );
        editor.entry = Some("hologram".into());
        assert_eq!(editor.commit_entry(), Ok(true));

        editor.selected = ROW_CHARACTER_STRENGTH;
        editor.entry = Some("73%".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        editor.selected = ROW_CHARACTER_SPEED;
        editor.entry = Some("91".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        let visual = editor.draft.character_visual().expect("character effect");
        assert_eq!((visual.strength, visual.speed), (73, 91));

        editor.selected = ROW_CHARACTER_EFFECT;
        editor.entry = Some("off".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(editor.draft.character_visual(), None);
        editor.selected = ROW_CHARACTER_SPEED;
        editor.entry = Some("50".into());
        assert!(
            editor.commit_entry().is_err(),
            "tuning an absent effect is refused"
        );
    }

    /// The head rows take typed values, not just arrow steps: an opacity
    /// typed as a number lands, the mark style by name, and text that is
    /// neither is refused with a list of the valid spellings.
    #[test]
    fn typed_values_land_on_the_head_rows() {
        let theme = super::super::theme::Theme::resolve(None).expect("theme");
        let mut editor = ThemeEditor::open(&theme, &theme, "mine".into(), false);

        editor.selected = ROW_OPACITY_INTERFACE;
        let (_, seed) = editor.entry_seed();
        assert!(seed.chars().all(|c| c.is_ascii_digit()) || seed.is_empty());
        editor.entry = Some("55".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(editor.draft.opacity.interface, Some(55));
        editor.entry = Some("140".into());
        assert!(editor.commit_entry().is_err(), "over 100 is refused");

        editor.selected = ROW_HYDRA_OPACITY;
        editor.entry = Some("unset".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(editor.draft.hydra_opacity, None);

        editor.selected = ROW_EVENT_MARK;
        let (_, seed) = editor.entry_seed();
        assert_eq!(seed, "tint");
        editor.entry = Some("Invert".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(
            editor.draft.event_mark,
            super::super::theme::MarkStyle::Invert
        );
        editor.entry = Some("sparkle".into());
        let refused = editor.commit_entry().expect_err("not a style");
        assert!(refused.contains("outline"));
    }

    #[test]
    fn bracket_mark_can_be_stepped_typed_and_saved_with_the_theme() {
        let mut editor = editor();
        editor.selected = ROW_BRACKET_MARK;
        assert_eq!(editor.entry_seed().1, "auto");
        assert!(editor.step(true));
        assert_eq!(editor.draft.bracket_mark, BracketMark::Underline);
        let saved = Theme::from_json(&editor.draft.to_json()).unwrap();
        assert_eq!(saved.bracket_mark, BracketMark::Underline);
        editor.entry = Some("block".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(editor.draft.bracket_mark, BracketMark::Block);
        editor.entry = Some("sparkle".into());
        assert!(editor.commit_entry().is_err());
    }

    #[test]
    fn caret_shape_inherits_settings_until_a_theme_explicitly_overrides_it() {
        let mut editor = editor();
        editor.selected = ROW_CARET_SHAPE;
        assert_eq!(editor.draft.caret_shape, None);
        assert_eq!(editor.row_text(ROW_CARET_SHAPE).1, "settings (inherit)");

        assert!(editor.step(true));
        assert_eq!(editor.draft.caret_shape, Some(CaretShape::SteadyBar));
        editor.entry = Some("blinking-underline".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(
            editor.draft.caret_shape,
            Some(CaretShape::BlinkingUnderline)
        );
        editor.entry = Some("inherit".into());
        assert_eq!(editor.commit_entry(), Ok(true));
        assert_eq!(editor.draft.caret_shape, None);
    }

    /// settle() brings a half-typed entry and a dirty code tab into the
    /// draft, so a save writes what is on screen.
    #[test]
    fn settle_commits_the_entry_and_parses_the_dirty_code() {
        let theme = super::super::theme::Theme::resolve(None).expect("theme");
        let mut editor = ThemeEditor::open(&theme, &theme, "mine".into(), false);
        editor.selected = ROW_OPACITY_INTERFACE;
        editor.entry = Some("33".into());
        assert_eq!(editor.settle(), Ok(true));
        assert_eq!(editor.draft.opacity.interface, Some(33));
        assert!(editor.entry.is_none());

        // A dirty code tab that parses lands; one that does not names the
        // fault and keeps its text for fixing.
        editor.code = super::super::editor::Editor::new("{ not json").expect("any text opens");
        editor.code_dirty_at = Some(std::time::Instant::now());
        assert!(editor.settle().is_err());
        assert!(editor.code_error.is_some());
    }
}

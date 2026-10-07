//! A picker for one file: the folder's candidates as a list, a path typed
//! by hand, or a walk through the folders - the one component behind adding
//! a score to the set and opening another set, so all three are learnt
//! once.
//!
//! The candidates are whatever the caller found worth offering; the typed
//! path is for everything else, `~` included; and Tab opens a browser that
//! walks folders from wherever the picker started - or from a typed path -
//! folders first, dotfiles left out. The picker only chooses; the caller
//! checks what was chosen and puts any refusal back on the picker as its
//! error line, so the reader corrects the path rather than starting over.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

/// Keep pasted lines in a single text field. Line endings and tabs are
/// separators, never key presses that confirm the prompt or move focus.
pub(super) fn single_line_paste(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut separator = false;
    for character in text.chars() {
        if matches!(character, '\r' | '\n' | '\t' | '\u{2028}' | '\u{2029}') {
            if !separator {
                result.push(' ');
            }
            separator = true;
        } else if !character.is_control() {
            result.push(character);
            separator = false;
        }
    }
    result
}

/// The field is a one-row preview, even when its stored artwork is not.
fn field_character(character: char) -> char {
    match character {
        '\n' => '↵',
        '\r' => '␍',
        '\t' => '→',
        character if character.is_control() => '�',
        character => character,
    }
}

/// Display columns and character indices must agree with Ratatui's grapheme
/// renderer. In particular, a joined emoji occupies one glyph, not the sum
/// of the widths of each Unicode scalar in the stored text.
struct FieldDisplay(String);

impl FieldDisplay {
    fn new(text: &str) -> Self {
        Self(text.chars().map(field_character).collect())
    }

    fn graphemes(&self) -> impl Iterator<Item = (usize, usize, &str)> {
        self.0
            .graphemes(true)
            .scan((0, 0), |(first, column), grapheme| {
                let result = (*first, *column, grapheme);
                *first += grapheme.chars().count();
                *column += grapheme.width();
                Some(result)
            })
    }

    fn column(&self, index: usize) -> usize {
        let mut end = 0;
        for (first, column, grapheme) in self.graphemes() {
            if index < first + grapheme.chars().count() {
                return column;
            }
            end = column + grapheme.width();
        }
        end
    }

    fn scroll(&self, caret: usize, typing: bool, room: usize) -> (usize, usize) {
        let wanted = if typing {
            self.column(caret).saturating_sub(room.saturating_sub(1))
        } else {
            0
        };
        let mut end = (0, 0);
        for (first, column, grapheme) in self.graphemes() {
            if column >= wanted {
                return (first, column);
            }
            end = (first + grapheme.chars().count(), column + grapheme.width());
        }
        end
    }

    fn at_column(&self, column: usize, first_visible: usize) -> usize {
        let mut end = 0;
        for (first, start, grapheme) in self.graphemes() {
            if first >= first_visible && column < start + grapheme.width() {
                return first;
            }
            end = first + grapheme.chars().count();
        }
        end
    }

    fn text_from_character(&self, first: usize) -> &str {
        let byte = self
            .0
            .char_indices()
            .nth(first)
            .map_or(self.0.len(), |(byte, _)| byte);
        &self.0[byte..]
    }
}

/// One file on offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    /// What the row says: a scene name, a set name.
    pub label: String,
    /// What is worth knowing beside it: "12 lines", "3 scenes".
    pub detail: String,
    pub path: PathBuf,
}

/// What the reader chose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerChoice {
    Candidate(PathBuf),
    Typed(PathBuf),
}

/// One line of the browser: a folder, a file, or the way up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub folder: bool,
    /// Beside the name: "up", "folder", or the file's size.
    pub detail: String,
}

/// The most entries a folder listing shows: enough for any folder a set
/// lives near, and a bound on what one keypress reads.
const MAX_ENTRIES: usize = 2000;

/// A walk through the folders, one folder at a time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Browser {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// The folder itself is on offer, as `.`: what a picker choosing a
    /// folder needs, since Enter on a folder goes into it.
    offers_self: bool,
}

impl Browser {
    /// The browser in `dir`, on its first entry.
    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            entries: read_entries(dir),
            selected: 0,
            offers_self: false,
        }
    }

    /// The browser in `dir`, offering `dir` itself as a choice.
    pub fn for_folders(dir: &Path) -> Self {
        let mut browser = Self::at(dir);
        browser.offers_self = true;
        browser.offer_self();
        browser
    }

    fn offer_self(&mut self) {
        if !self.offers_self {
            return;
        }
        let at = usize::from(self.entries.first().is_some_and(|entry| entry.name == ".."));
        self.entries.insert(
            at,
            Entry {
                name: ".".to_owned(),
                path: self.dir.clone(),
                folder: false,
                detail: "this folder".to_owned(),
            },
        );
        self.selected = at;
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    pub fn move_by(&mut self, delta: isize) {
        let count = self.entries.len();
        if count == 0 {
            return;
        }
        self.selected = ((self.selected as isize + delta).rem_euclid(count as isize)) as usize;
    }

    /// Enter: into a folder, or the file chosen.
    pub fn enter(&mut self) -> Option<PathBuf> {
        let entry = self.selected_entry()?.clone();
        if entry.folder {
            self.go(&entry.path);
            None
        } else {
            Some(entry.path)
        }
    }

    /// Up one folder, landing on the one just left.
    pub fn ascend(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let from = self.dir.clone();
        self.go(&parent);
        if let Some(index) = self.entries.iter().position(|entry| entry.path == from) {
            self.selected = index;
        }
    }

    fn go(&mut self, dir: &Path) {
        self.dir = dir.to_path_buf();
        self.entries = read_entries(dir);
        self.selected = 0;
        self.offer_self();
    }
}

/// A folder's entries: the way up first, then folders, then files, each
/// group by name; dotfiles left out, as a shell leaves them.
fn read_entries(dir: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(parent) = dir.parent() {
        entries.push(Entry {
            name: "..".to_owned(),
            path: parent.to_path_buf(),
            folder: true,
            detail: "up".to_owned(),
        });
    }
    let Ok(listing) = std::fs::read_dir(dir) else {
        return entries;
    };
    let mut folders = Vec::new();
    let mut files = Vec::new();
    for entry in listing.filter_map(Result::ok).take(MAX_ENTRIES) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            folders.push(Entry {
                name,
                path,
                folder: true,
                detail: "folder".to_owned(),
            });
        } else if metadata.is_file() {
            files.push(Entry {
                name,
                path,
                folder: false,
                detail: size_label(metadata.len()),
            });
        }
    }
    folders.sort_by_key(|entry| entry.name.to_lowercase());
    files.sort_by_key(|entry| entry.name.to_lowercase());
    entries.extend(folders);
    entries.extend(files);
    entries
}

fn size_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilePicker {
    pub title: String,
    /// What Enter does, said the way the caller wants it said.
    pub verb: String,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
    /// The path field has the keys rather than the list.
    pub typing: bool,
    pub path: String,
    /// What the field is called: a path, a name, a text.
    pub label: &'static str,
    /// The caret, in characters, and the other end of the selection: the
    /// same place when nothing is selected.
    pub caret: usize,
    pub anchor: usize,
    /// Where a browse starts: the set's folder.
    pub home: PathBuf,
    /// The browser, while the reader is walking folders.
    pub browser: Option<Browser>,
    /// Whether there is anything to browse for: a name prompt has not.
    pub browsable: bool,
    /// The choice is a folder: the browser offers the folder it is in.
    pub folders: bool,

    /// Why the last choice was refused, from the caller.
    pub error: Option<String>,
    /// The first row the list up now draws, kept from move to move so
    /// walking the middle of it leaves it still. A cell, because it is
    /// settled where the list's height is known - drawing and hit-testing -
    /// and both read the picker. See [`super::scroll`].
    pub scroll: std::cell::Cell<usize>,
    /// The selection was last put there by a click: until a key moves it
    /// the list keeps no margin, so the clicked row stays under the pointer.
    pub hold_scroll: bool,
    /// The last row click, for the second press of a double click.
    last_click: Option<(usize, Instant)>,
}

impl FilePicker {
    pub fn new(title: &str, verb: &str, candidates: Vec<Candidate>, home: &Path) -> Self {
        Self {
            title: title.to_owned(),
            verb: verb.to_owned(),
            // Nothing to list means the path is the only way in.
            typing: candidates.is_empty(),
            candidates,
            selected: 0,
            path: String::new(),
            label: "path",
            caret: 0,
            anchor: 0,
            home: home.to_path_buf(),
            browser: None,
            browsable: true,
            folders: false,
            error: None,
            scroll: std::cell::Cell::new(0),
            hold_scroll: false,
            last_click: None,
        }
    }

    /// A picker for a folder rather than a file: Enter still goes into a
    /// folder, and `.` at the top of the listing chooses it.
    pub fn choosing_folders(mut self) -> Self {
        self.folders = true;
        self
    }

    /// A prompt for a name rather than a file: nothing to browse.
    pub fn naming(title: &str, verb: &str) -> Self {
        let mut picker = Self::new(title, verb, Vec::new(), Path::new("."));
        picker.browsable = false;
        picker.label = "name";
        picker
    }

    /// A prompt for a line of text.
    pub fn text(title: &str, verb: &str) -> Self {
        let mut picker = Self::naming(title, verb);
        picker.label = "text";
        picker
    }

    pub fn browsing(&self) -> bool {
        self.browser.is_some()
    }

    /// Tab: open the browser - at the typed path when there is one that
    /// names a folder or a file in one, else at home - or close it again.
    pub fn toggle_browsing(&mut self) {
        self.cancel_click();
        if !self.browsable {
            return;
        }
        self.error = None;
        if self.browser.is_some() {
            self.browser = None;
            self.typing = self.candidates.is_empty();
            return;
        }
        let open = |dir: &Path| {
            if self.folders {
                Browser::for_folders(dir)
            } else {
                Browser::at(dir)
            }
        };
        let typed = self.path.trim();
        let mut browser = if !typed.is_empty() {
            let typed = expand_home(typed);
            if typed.is_dir() {
                open(&typed)
            } else if let Some(parent) = typed.parent().filter(|parent| parent.is_dir()) {
                let mut browser = open(parent);
                if let Some(index) = browser.entries.iter().position(|entry| entry.path == typed) {
                    browser.selected = index;
                }
                browser
            } else {
                open(&self.home)
            }
        } else {
            open(&self.home)
        };
        if browser.dir.as_os_str().is_empty() {
            browser = open(Path::new("."));
        }

        self.typing = false;
        self.browser = Some(browser);
    }

    pub fn move_by(&mut self, delta: isize) {
        let count = self.candidates.len();
        if count == 0 {
            return;
        }
        self.selected = ((self.selected as isize + delta).rem_euclid(count as isize)) as usize;
        self.error = None;
        self.hold_scroll = false;
    }

    pub fn start_typing(&mut self) {
        self.cancel_click();
        self.typing = true;
        self.error = None;
    }

    /// Offer `text` in the field, selected: typing replaces it, Enter
    /// keeps it - a rename that starts from the old name.
    pub fn offer(&mut self, text: &str) {
        self.set_path(text);
        self.select_all();
        self.typing = true;
        self.error = None;
    }

    /// Put `text` in the field, the caret after it.
    pub fn set_path(&mut self, text: &str) {
        self.path = text.to_owned();
        self.caret = self.chars();
        self.anchor = self.caret;
    }

    fn chars(&self) -> usize {
        self.path.chars().count()
    }

    fn byte_at(&self, index: usize) -> usize {
        self.path
            .char_indices()
            .nth(index)
            .map_or(self.path.len(), |(at, _)| at)
    }

    /// The selected span, in characters, ordered; none when the caret
    /// stands alone.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        (self.caret != self.anchor)
            .then(|| self.caret.min(self.anchor)..self.caret.max(self.anchor))
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.chars();
    }

    /// The caret to `at`; with `select`, the selection stretches to it.
    pub fn caret_to(&mut self, at: usize, select: bool) {
        self.caret = at.min(self.chars());
        if !select {
            self.anchor = self.caret;
        }
        self.error = None;
    }

    pub fn move_caret(&mut self, delta: isize, select: bool) {
        // Leaving a selection without Shift lands at its near end.
        let from = match (select, self.selection()) {
            (false, Some(span)) if delta < 0 => span.start as isize + 1,
            (false, Some(span)) => span.end as isize - 1,
            _ => self.caret as isize,
        };
        let at = (from + delta).clamp(0, self.chars() as isize) as usize;
        self.caret_to(at, select);
    }

    pub fn home(&mut self, select: bool) {
        self.caret_to(0, select);
    }

    pub fn end(&mut self, select: bool) {
        self.caret_to(self.chars(), select);
    }

    /// Take the selection out; whether there was one.
    fn remove_selection(&mut self) -> bool {
        let Some(span) = self.selection() else {
            return false;
        };
        let (from, to) = (self.byte_at(span.start), self.byte_at(span.end));
        self.path.replace_range(from..to, "");
        self.caret = span.start;
        self.anchor = span.start;
        true
    }

    /// Back to the list, where there is one.
    pub fn stop_typing(&mut self) -> bool {
        if self.candidates.is_empty() {
            return false;
        }
        self.typing = false;
        self.error = None;
        true
    }

    /// A character at the caret, in place of the selection if there is one.
    pub fn push(&mut self, character: char) {
        self.remove_selection();
        let at = self.byte_at(self.caret);
        self.path.insert(at, character);
        self.caret += 1;
        self.anchor = self.caret;
        self.error = None;
    }

    /// Insert a whole paste, replacing the offered or selected text once.
    /// Artwork uses this path so spaces, backslashes and line endings stay
    /// literal; none of its characters can submit the picker.
    pub fn paste_multiline(&mut self, text: &str) {
        self.cancel_click();
        self.remove_selection();
        let at = self.byte_at(self.caret);
        self.path.insert_str(at, text);
        self.caret += text.chars().count();
        self.anchor = self.caret;
        self.typing = true;
        self.browser = None;
        self.error = None;
    }

    /// Paths and names remain single-line even when their paste is not.
    pub fn paste_text(&mut self, text: &str) {
        self.paste_multiline(&single_line_paste(text));
    }

    /// Backspace: the selection, else the character before the caret.
    pub fn pop(&mut self) {
        if !self.remove_selection() && self.caret > 0 {
            let (from, to) = (self.byte_at(self.caret - 1), self.byte_at(self.caret));
            self.path.replace_range(from..to, "");
            self.caret -= 1;
            self.anchor = self.caret;
        }
        self.error = None;
    }

    /// Delete: the selection, else the character after the caret.
    pub fn delete_forward(&mut self) {
        if !self.remove_selection() && self.caret < self.chars() {
            let (from, to) = (self.byte_at(self.caret), self.byte_at(self.caret + 1));
            self.path.replace_range(from..to, "");
        }
        self.error = None;
    }

    /// The character index under a pointer on the field's row, if the
    /// pointer is on the field: past the text counts as its end.
    pub fn field_at(&self, available: Rect, x: u16, y: u16) -> Option<usize> {
        let (area, list) = self.geometry(available)?;
        if y != area.y + 1 || x < list.x || x >= list.right() {
            return None;
        }
        let start = list.x + self.label.len() as u16 + 2;
        let display = FieldDisplay::new(&self.path);
        let (first, scrolled) = display.scroll(
            self.caret,
            self.typing,
            usize::from(list.right().saturating_sub(start)),
        );
        let column = usize::from(x.saturating_sub(start));
        Some(display.at_column(column + scrolled, first))
    }

    #[cfg(test)]
    fn field_column(&self, index: usize) -> usize {
        FieldDisplay::new(&self.path).column(index)
    }

    /// First visible character and its display column, keeping the caret
    /// in the row after a long paste. Mouse, selection and paint share it.
    #[cfg(test)]
    fn field_scroll(&self, room: usize) -> (usize, usize) {
        FieldDisplay::new(&self.path).scroll(self.caret, self.typing, room)
    }

    /// What Enter means now: the typed path while typing, the file under
    /// the cursor while browsing (a folder is not a choice, Enter goes into
    /// it), else the row.
    pub fn choice(&self) -> Option<PickerChoice> {
        if self.typing {
            let typed = self.path.trim();
            (!typed.is_empty()).then(|| PickerChoice::Typed(expand_home(typed)))
        } else if let Some(browser) = &self.browser {
            browser
                .selected_entry()
                .filter(|entry| !entry.folder)
                .map(|entry| PickerChoice::Typed(entry.path.clone()))
        } else {
            self.candidates
                .get(self.selected)
                .map(|candidate| PickerChoice::Candidate(candidate.path.clone()))
        }
    }

    /// A click on a row of whichever list is up.
    pub fn select_row(&mut self, row: usize) {
        self.cancel_click();
        self.error = None;
        self.hold_scroll = true;
        match self.browser.as_mut() {
            Some(browser) => browser.selected = row.min(browser.entries.len().saturating_sub(1)),
            None => {
                self.selected = row.min(self.candidates.len().saturating_sub(1));
                self.typing = false;
            }
        }
    }

    /// Select a row; two clicks on it within half a second activate it,
    /// just as in the set panel. A completed pair consumes both clicks.
    pub fn click_row(&mut self, row: usize, now: Instant) -> bool {
        if row >= self.row_count() {
            self.cancel_click();
            return false;
        }
        let double = self
            .last_click
            .is_some_and(|(last, at)| last == row && now.duration_since(at).as_millis() <= 500);
        self.select_row(row);
        self.last_click = if double { None } else { Some((row, now)) };
        double
    }

    /// A key or a click away from the list breaks a pending double click.
    pub fn cancel_click(&mut self) {
        self.last_click = None;
    }

    /// How many rows the list up now has.
    fn row_count(&self) -> usize {
        match &self.browser {
            Some(browser) => browser.entries.len(),
            None => self.candidates.len(),
        }
    }

    fn selected_index(&self) -> usize {
        match &self.browser {
            Some(browser) => browser.selected,
            None => self.selected,
        }
    }

    /// What a key can change about where the list stands: which list is
    /// up, the folder it shows, the selection and how many rows there are.
    pub fn scroll_signature(&self) -> (Option<PathBuf>, usize, usize) {
        (
            self.browser.as_ref().map(|browser| browser.dir.clone()),
            self.selected_index(),
            self.row_count(),
        )
    }

    /// The first row the list up now draws, `shown` rows at a time: the
    /// selection among them, with a margin of rows around it unless a click
    /// put it there.
    fn first_row(&self, shown: usize) -> usize {
        let margin = if self.hold_scroll {
            0
        } else {
            super::scroll::margin(shown)
        };
        let first = super::scroll::follow(
            self.scroll.get(),
            self.selected_index(),
            shown,
            self.row_count(),
            margin,
        );
        self.scroll.set(first);
        first
    }

    /// The sheet's place: bottom-right, like the pickers before it, with a
    /// row per entry up to a screenful.
    pub fn geometry(&self, available: Rect) -> Option<(Rect, Rect)> {
        let rows = (self.row_count() as u16).clamp(1, 12);
        // Title, path field, the list, an error line, a hint line, borders.
        let height = rows + 6;
        let width = 52u16;
        if available.width < width + 2 || available.height < height + 1 {
            return None;
        }
        let area = Rect::new(
            available.right().saturating_sub(width + 1),
            available.bottom().saturating_sub(height + 1),
            width,
            height,
        );
        let list = Rect::new(area.x + 2, area.y + 3, area.width.saturating_sub(4), rows);
        Some((area, list))
    }

    /// The row under a pointer, if any - of the browser while it is up.
    pub fn row_at(&self, available: Rect, x: u16, y: u16) -> Option<usize> {
        let (_, list) = self.geometry(available)?;
        if x < list.x || x >= list.right() || y < list.y || y >= list.bottom() {
            return None;
        }
        let first = self.first_row(usize::from(list.height));
        let index = first + usize::from(y - list.y);
        (index < self.row_count()).then_some(index)
    }

    /// Whether the pointer is over the sheet at all.
    pub fn contains(&self, available: Rect, x: u16, y: u16) -> bool {
        self.geometry(available)
            .is_some_and(|(area, _)| area.contains((x, y).into()))
    }
}

/// `~`, `~/…` and `~\…` mean the home folder. Accepting both separators
/// keeps a path copied from Settings useful on every platform.
fn expand_home(typed: &str) -> PathBuf {
    if let Some(rest) = typed.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with(['/', '\\']))
        && let Some(home) = rustel_runtime::config_dir::home()
    {
        return Path::new(&home).join(rest.trim_start_matches(['/', '\\']));
    }
    PathBuf::from(typed)
}

pub struct FilePickerView<'a> {
    pub picker: &'a FilePicker,
    pub theme: &'a Theme,
}

impl ratatui::widgets::Widget for FilePickerView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some((panel, list)) = self.picker.geometry(area) else {
            return;
        };
        let theme = self.theme;
        super::view::clear_overlay(
            buffer,
            panel,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        super::devices::draw_border(buffer, panel, theme);
        let bold = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        buffer.set_stringn(
            panel.x + 2,
            panel.y,
            format!(" {} ", self.picker.title),
            usize::from(panel.width.saturating_sub(4)),
            bold,
        );
        // The field: lit while it has the keys, its text drawn as a
        // selection while it is only offered.
        let label = self.picker.label;
        let start = list.x + label.len() as u16 + 2;
        let display = FieldDisplay::new(&self.picker.path);
        let (first, scrolled) = display.scroll(
            self.picker.caret,
            self.picker.typing,
            usize::from(list.right().saturating_sub(start)),
        );
        let field = if self.picker.path.is_empty() && !self.picker.typing {
            format!("{label}: / to type one")
        } else {
            format!("{label}: {}", display.text_from_character(first))
        };
        buffer.set_stringn(
            list.x,
            panel.y + 1,
            &field,
            usize::from(list.width),
            if self.picker.typing {
                Style::default().fg(theme.foreground)
            } else {
                Style::default().fg(theme.muted)
            },
        );
        // The selection in the selection colours, and the caret as a
        // reversed cell - over the character it stands before, or the
        // space after the text.
        if self.picker.typing {
            let cell = |index: usize| {
                let column = display.column(index).saturating_sub(scrolled);
                start.saturating_add(column.min(usize::from(u16::MAX)) as u16)
            };
            if let Some(span) = self.picker.selection() {
                let from = cell(span.start);
                let to = cell(span.end).min(list.right());
                if from < to {
                    buffer.set_style(
                        Rect::new(from, panel.y + 1, to - from, 1),
                        Style::default()
                            .fg(theme.selection_text)
                            .bg(theme.selection),
                    );
                }
            }
            let caret = cell(self.picker.caret);
            if caret < list.right() {
                buffer.set_style(
                    Rect::new(caret, panel.y + 1, 1, 1),
                    Style::default().add_modifier(Modifier::REVERSED),
                );
            }
        }
        let width = usize::from(list.width);
        let heading = match &self.picker.browser {
            Some(browser) => {
                let shown = browser.dir.display().to_string();
                // The end of a long path is the part that says where.
                let excess = shown
                    .chars()
                    .count()
                    .saturating_sub(width.saturating_sub(2));
                if excess > 0 {
                    format!("…{}", shown.chars().skip(excess + 1).collect::<String>())
                } else {
                    shown
                }
            }
            None if !self.picker.browsable && self.picker.path.contains(['\n', '\r']) => {
                format!(
                    "{} lines · ↵ newline · → tab",
                    self.picker.path.lines().count().max(1)
                )
            }
            // A name prompt has no folder to speak of.
            None if !self.picker.browsable => String::new(),
            None if self.picker.candidates.is_empty() => {
                "nothing in the folder to offer".to_owned()
            }
            None => format!("in the folder ({})", self.picker.candidates.len()),
        };
        buffer.set_stringn(
            list.x,
            panel.y + 2,
            &heading,
            width,
            Style::default().fg(theme.muted),
        );
        // The rows of whichever list is up: (label, detail, a folder).
        let rows: Vec<(String, String, bool)> = match &self.picker.browser {
            Some(browser) => browser
                .entries
                .iter()
                .map(|entry| {
                    let label = if entry.folder && entry.name != ".." {
                        format!("{}/", entry.name)
                    } else {
                        entry.name.clone()
                    };
                    (label, entry.detail.clone(), entry.folder)
                })
                .collect(),
            None => self
                .picker
                .candidates
                .iter()
                .map(|candidate| (candidate.label.clone(), candidate.detail.clone(), false))
                .collect(),
        };
        let selected_index = self.picker.selected_index();
        let first = self.picker.first_row(usize::from(list.height));
        for (row, (label, detail, folder)) in rows
            .iter()
            .enumerate()
            .skip(first)
            .take(usize::from(list.height))
        {
            let selected = row == selected_index && !self.picker.typing;
            let style = if selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else if *folder {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.foreground)
            };
            let marker = if selected {
                format!("{} ", crate::terminal::symbol("▸"))
            } else {
                "  ".to_owned()
            };
            let room = width.saturating_sub(marker.len() + detail.chars().count() + 1);
            let label: String = label.chars().take(room).collect();
            let text = format!("{marker}{label:<room$} {detail}");
            buffer.set_stringn(list.x, list.y + (row - first) as u16, &text, width, style);
        }
        let error_y = list.bottom();
        if let Some(error) = &self.picker.error {
            buffer.set_stringn(
                list.x,
                error_y,
                error,
                usize::from(list.width),
                Style::default().fg(theme.error),
            );
        }
        let hint = if self.picker.browsing() {
            format!(
                "Enter opens a folder / {} · Backspace up · Tab back · Esc back",
                self.picker.verb
            )
        } else if self.picker.typing && self.picker.browsable {
            format!("Enter {} · Tab browses there · Esc back", self.picker.verb)
        } else if self.picker.typing {
            format!("Enter {} · Esc back", self.picker.verb)
        } else {
            format!(
                "Enter {} · Tab browses · type a path · Esc back",
                self.picker.verb
            )
        };
        buffer.set_stringn(
            list.x,
            error_y + 1,
            &hint,
            usize::from(list.width),
            Style::default().fg(theme.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::Widget;

    #[test]
    fn multiline_field_preview_never_emits_control_characters() {
        let mut picker = FilePicker::text("art", "writes it");
        let text = "  /\\_/\\\r\n\t(猫)\n  > ^ <  \n";
        picker.paste_multiline(text);
        picker.home(false);
        let frame = Rect::new(0, 0, 120, 40);
        let theme = Theme::resolve(None).unwrap();
        let mut buffer = Buffer::empty(frame);
        FilePickerView {
            picker: &picker,
            theme: &theme,
        }
        .render(frame, &mut buffer);
        assert_eq!(picker.path, text, "the preview cannot rewrite the artwork");
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| !cell.symbol().chars().any(char::is_control))
        );
        let symbols: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(symbols.contains('↵'));
        assert!(symbols.contains('→'));
        assert!(symbols.contains("3 lines"));
    }

    #[test]
    fn wide_text_and_scrolling_share_the_caret_and_mouse_columns() {
        let mut picker = FilePicker::text("art", "writes it");
        picker.paste_multiline("猫x\ny");
        let frame = Rect::new(0, 0, 120, 40);
        let (panel, list) = picker.geometry(frame).unwrap();
        let start = list.x + picker.label.len() as u16 + 2;
        assert_eq!(picker.field_at(frame, start, panel.y + 1), Some(0));
        assert_eq!(picker.field_at(frame, start + 1, panel.y + 1), Some(0));
        assert_eq!(picker.field_at(frame, start + 2, panel.y + 1), Some(1));
        assert_eq!(picker.field_at(frame, start + 3, panel.y + 1), Some(2));

        picker.set_path(&format!("{}猫", "x".repeat(150)));
        let room = usize::from(list.right() - start);
        let (first, scrolled) = picker.field_scroll(room);
        assert!(first > 0);
        assert_eq!(picker.field_at(frame, start, panel.y + 1), Some(first));
        assert!(picker.field_column(picker.caret) - scrolled < room);
        let theme = Theme::resolve(None).unwrap();
        let mut buffer = Buffer::empty(frame);
        FilePickerView {
            picker: &picker,
            theme: &theme,
        }
        .render(frame, &mut buffer);
        let caret = start + (picker.field_column(picker.caret) - scrolled) as u16;
        assert!(
            buffer[(caret, panel.y + 1)]
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn joined_emoji_share_rendered_caret_selection_and_mouse_boundaries() {
        let mut picker = FilePicker::text("art", "writes it");
        let text = "👩‍💻x";
        picker.paste_multiline(text);
        picker.caret_to(3, false);
        let frame = Rect::new(0, 0, 120, 40);
        let (panel, list) = picker.geometry(frame).unwrap();
        let start = list.x + picker.label.len() as u16 + 2;
        assert_eq!(
            picker.field_column(3),
            2,
            "one joined glyph occupies two cells"
        );
        assert_eq!(picker.field_column(4), 3);
        assert_eq!(picker.field_at(frame, start, panel.y + 1), Some(0));
        assert_eq!(picker.field_at(frame, start + 1, panel.y + 1), Some(0));
        assert_eq!(
            picker.field_at(frame, start + 2, panel.y + 1),
            Some(3),
            "clicking x never lands inside the emoji"
        );
        let theme = Theme::resolve(None).unwrap();
        let mut buffer = Buffer::empty(frame);
        FilePickerView {
            picker: &picker,
            theme: &theme,
        }
        .render(frame, &mut buffer);
        assert_eq!(buffer[(start, panel.y + 1)].symbol(), "👩‍💻");
        assert_eq!(buffer[(start + 2, panel.y + 1)].symbol(), "x");
        assert!(
            buffer[(start + 2, panel.y + 1)]
                .modifier
                .contains(Modifier::REVERSED)
        );
        picker.caret_to(0, false);
        picker.caret_to(3, true);
        FilePickerView {
            picker: &picker,
            theme: &theme,
        }
        .render(frame, &mut buffer);
        assert_eq!(buffer[(start, panel.y + 1)].bg, theme.selection);
        assert_eq!(buffer[(start + 1, panel.y + 1)].bg, theme.selection);
        assert_ne!(buffer[(start + 2, panel.y + 1)].bg, theme.selection);
        assert_eq!(picker.path, text);
        let long = format!("{}x", "👩‍💻".repeat(60));
        picker.set_path(&long);
        let room = usize::from(list.right() - start);
        let (first, scrolled) = picker.field_scroll(room);
        assert!(first > 0);
        assert_eq!(first % 3, 0, "scrolling cannot start inside a joined glyph");
        assert_eq!(picker.field_at(frame, start, panel.y + 1), Some(first));
        assert!(picker.field_column(picker.caret) - scrolled < room);
        FilePickerView {
            picker: &picker,
            theme: &theme,
        }
        .render(frame, &mut buffer);
        assert_eq!(buffer[(start, panel.y + 1)].symbol(), "👩‍💻");
        assert_eq!(
            picker.path, long,
            "display mapping preserves every stored byte"
        );
    }

    #[test]
    fn picker_masks_earlier_pixel_images_in_its_sheet() {
        use crate::graphics::{PixelImage, capture_images, push_image};
        let picker = FilePicker::text("art", "writes it");
        let frame = Rect::new(0, 0, 120, 40);
        let panel = picker.geometry(frame).unwrap().0;
        let theme = Theme::resolve(None).unwrap();
        let (_, images) = capture_images(|| {
            push_image(PixelImage::inline(frame, 120, 40, vec![255; 120 * 40 * 4]));
            let mut buffer = Buffer::empty(frame);
            FilePickerView {
                picker: &picker,
                theme: &theme,
            }
            .render(frame, &mut buffer);
        });
        assert_eq!(images.len(), 1);
        for position in frame.positions() {
            let alpha =
                images[0].rgba[(usize::from(position.y) * 120 + usize::from(position.x)) * 4 + 3];
            assert_eq!(alpha, if panel.contains(position) { 0 } else { 255 });
        }
    }

    fn candidates() -> Vec<Candidate> {
        ["drop", "intro"]
            .iter()
            .map(|name| Candidate {
                label: (*name).to_owned(),
                detail: "3 lines".to_owned(),
                path: PathBuf::from(format!("{name}.strudel")),
            })
            .collect()
    }

    /// The list is the choice until a path is typed; a typed path is the
    /// choice while it is being typed, `~` expanded like a shell would.
    #[test]
    fn the_choice_is_the_row_or_the_typed_path() {
        let mut picker = FilePicker::new("add a scene", "adds", candidates(), Path::new("."));
        assert!(!picker.typing);
        picker.move_by(1);
        assert_eq!(
            picker.choice(),
            Some(PickerChoice::Candidate(PathBuf::from("intro.strudel")))
        );
        picker.start_typing();
        assert_eq!(picker.choice(), None, "nothing typed yet");
        for character in format!("~{}song.strudel", std::path::MAIN_SEPARATOR).chars() {
            picker.push(character);
        }
        let home = rustel_runtime::config_dir::home()
            .map(PathBuf::from)
            .unwrap();
        assert_eq!(
            picker.choice(),
            Some(PickerChoice::Typed(home.join("song.strudel")))
        );
        assert!(picker.stop_typing());
        assert_eq!(
            picker.choice(),
            Some(PickerChoice::Candidate(PathBuf::from("intro.strudel")))
        );
        // With nothing to list the path is the only way in.
        let mut empty = FilePicker::new("open a set", "opens", Vec::new(), Path::new("."));
        assert!(empty.typing);
        assert!(!empty.stop_typing(), "no list to go back to");
    }

    /// Tab opens the browser at home, or at a typed folder, or in the
    /// A name offered in the field is a selection: the first character
    /// typed replaces it, Backspace clears it, and Enter keeps it whole.
    #[test]
    fn an_offered_name_is_replaced_by_typing() {
        let mut picker = FilePicker::naming("rename the set", "renames its folder");
        picker.offer("old name");
        assert!(picker.typing);
        assert_eq!(picker.selection(), Some(0..8));
        assert_eq!(
            picker.choice(),
            Some(PickerChoice::Typed(PathBuf::from("old name")))
        );
        picker.push('n');
        assert_eq!(picker.path, "n");
        assert_eq!(picker.selection(), None);
        picker.push('u');
        assert_eq!(picker.path, "nu");
        picker.offer("again");
        picker.pop();
        assert_eq!(picker.path, "", "Backspace clears the offer");
        picker.pop();
        assert_eq!(picker.path, "");
    }

    /// The field has a caret: arrows walk it, Shift with them selects,
    /// Home and End go to the ends, typing lands where it stands and
    /// replaces a selection, Backspace and Delete take either side of it,
    /// and a click puts it under the pointer.
    #[test]
    fn the_field_has_a_caret_and_a_selection() {
        let mut picker = FilePicker::text("the art's text", "writes it");
        picker.set_path("héllo");
        assert_eq!(picker.caret, 5);
        picker.move_caret(-2, false);
        assert_eq!(picker.caret, 3);
        picker.push('X');
        assert_eq!(picker.path, "hélXlo");
        picker.move_caret(-1, true);
        picker.move_caret(-1, true);
        assert_eq!(picker.selection(), Some(2..4), "back over two characters");
        picker.push('Y');
        assert_eq!(picker.path, "héYlo", "typing replaces the selection");
        picker.home(false);
        picker.delete_forward();
        assert_eq!(picker.path, "éYlo");
        picker.end(true);
        assert_eq!(picker.selection(), Some(0..4));
        picker.move_caret(1, false);
        assert_eq!(picker.selection(), None);
        assert_eq!(
            picker.caret, 4,
            "leaving a selection forward lands at its end"
        );
        picker.home(false);
        picker.select_all();
        picker.pop();
        assert_eq!(picker.path, "");
        picker.set_path("abc");
        let frame = Rect::new(0, 0, 120, 40);
        let (area, list) = picker.geometry(frame).unwrap();
        let start = list.x + u16::try_from(picker.label.len()).unwrap() + 2;
        assert_eq!(picker.field_at(frame, start + 1, area.y + 1), Some(1));
        assert_eq!(
            picker.field_at(frame, start + 40, area.y + 1),
            Some(3),
            "past the end is the end"
        );
        assert_eq!(
            picker.field_at(frame, start, area.y + 2),
            None,
            "another row"
        );
        picker.caret_to(1, false);
        picker.caret_to(3, true);
        assert_eq!(picker.selection(), Some(1..3));
    }

    /// folder of a typed file with that file under the cursor. Folders come
    /// first, dotfiles stay out, Enter goes into a folder and chooses a
    /// file, Backspace goes up and lands on the folder just left.
    #[test]
    fn the_browser_walks_folders_and_chooses_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("songs")).unwrap();
        std::fs::create_dir(root.path().join("Archive")).unwrap();
        std::fs::write(root.path().join("zed.strudel"), "// z").unwrap();
        std::fs::write(root.path().join("alpha.txt"), "aa").unwrap();
        std::fs::write(root.path().join(".hidden"), "").unwrap();
        std::fs::write(root.path().join("songs").join("tune.strudel"), "// t").unwrap();

        let mut picker = FilePicker::new("add a scene", "adds", candidates(), root.path());
        picker.toggle_browsing();
        let browser = picker.browser.as_ref().expect("browsing");
        assert_eq!(browser.dir, root.path());
        assert_eq!(
            browser
                .entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.folder))
                .collect::<Vec<_>>(),
            [
                ("..", true),
                ("Archive", true),
                ("songs", true),
                ("alpha.txt", false),
                ("zed.strudel", false),
            ]
        );
        assert_eq!(picker.choice(), None, "a folder is not a choice");

        // Into songs, choose the tune.
        picker.browser.as_mut().unwrap().selected = 2;
        assert_eq!(
            picker.browser.as_mut().unwrap().enter(),
            None,
            "Enter went in"
        );
        assert_eq!(
            picker.browser.as_ref().unwrap().dir,
            root.path().join("songs")
        );
        picker.browser.as_mut().unwrap().selected = 1;
        assert_eq!(
            picker.choice(),
            Some(PickerChoice::Typed(
                root.path().join("songs").join("tune.strudel")
            ))
        );
        // Up again lands on the folder just left.
        picker.browser.as_mut().unwrap().ascend();
        let browser = picker.browser.as_ref().unwrap();
        assert_eq!(browser.dir, root.path());
        assert_eq!(browser.selected_entry().unwrap().name, "songs");

        // Tab closes it; a typed file's folder opens with the file chosen.
        picker.toggle_browsing();
        assert!(picker.browser.is_none());
        picker.start_typing();
        for character in root.path().join("zed.strudel").to_string_lossy().chars() {
            picker.push(character);
        }
        picker.toggle_browsing();
        let browser = picker.browser.as_ref().expect("browsing at the typed path");
        assert_eq!(browser.dir, root.path());
        assert_eq!(browser.selected_entry().unwrap().name, "zed.strudel");
        assert!(!picker.typing);

        // A name prompt has nothing to browse.
        let mut naming = FilePicker::naming("new set", "starts it");
        naming.toggle_browsing();
        assert!(naming.browser.is_none());
    }

    /// Clicks land on the rows the sheet draws.
    #[test]
    fn rows_are_where_the_sheet_draws_them() {
        let picker = FilePicker::new("add a scene", "adds", candidates(), Path::new("."));
        let area = Rect::new(0, 0, 100, 30);
        let (_, list) = picker.geometry(area).expect("fits");
        assert_eq!(picker.row_at(area, list.x, list.y), Some(0));
        assert_eq!(picker.row_at(area, list.x + 3, list.y + 1), Some(1));
        assert_eq!(
            picker.row_at(area, list.x, list.y + 2),
            None,
            "no third row"
        );
        assert_eq!(picker.row_at(area, 0, 0), None);
        assert!(
            picker.geometry(Rect::new(0, 0, 40, 10)).is_none(),
            "too narrow"
        );
    }

    #[test]
    fn two_quick_clicks_on_the_same_row_activate_once() {
        use std::time::{Duration, Instant};

        let mut picker = FilePicker::new("open a set", "opens", candidates(), Path::new("."));
        let now = Instant::now();
        assert!(!picker.click_row(0, now));
        assert!(!picker.click_row(1, now + Duration::from_millis(10)));
        assert_eq!(picker.selected, 1);
        assert!(picker.hold_scroll);
        assert!(
            picker.click_row(1, now + Duration::from_millis(510)),
            "the second click on the same row within 500 ms activates"
        );
        assert!(
            !picker.click_row(1, now + Duration::from_millis(600)),
            "a completed pair is consumed"
        );
        assert!(
            !picker.click_row(1, now + Duration::from_millis(1101)),
            "a click after more than 500 ms only selects"
        );
        assert!(picker.click_row(1, now + Duration::from_millis(1200)));
    }

    #[test]
    fn clicks_outside_the_list_do_not_select_or_activate() {
        use std::time::{Duration, Instant};

        let mut picker = FilePicker::new("open a set", "opens", candidates(), Path::new("."));
        let now = Instant::now();
        assert!(!picker.click_row(0, now));
        assert!(!picker.click_row(2, now + Duration::from_millis(10)));
        assert_eq!(picker.selected, 0);
        assert!(
            !picker.click_row(0, now + Duration::from_millis(20)),
            "an invalid row cancels the pending pair"
        );
        assert!(!picker.click_row(usize::MAX, now + Duration::from_millis(30)));
        assert!(!picker.click_row(usize::MAX, now + Duration::from_millis(40)));
        assert_eq!(picker.selected, 0);

        let mut empty = FilePicker::new("open a set", "opens", Vec::new(), Path::new("."));
        assert!(!empty.click_row(0, now));
        assert!(!empty.click_row(0, now + Duration::from_millis(10)));
        assert!(empty.typing, "an empty picker keeps its path field active");
    }

    #[test]
    fn changing_picker_interaction_cancels_a_pending_double_click() {
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().unwrap();
        let mut picker = FilePicker::new("open a set", "opens", candidates(), root.path());
        let now = Instant::now();
        assert!(!picker.click_row(1, now));
        picker.select_row(1);
        assert!(!picker.click_row(1, now + Duration::from_millis(10)));
        picker.start_typing();
        assert!(!picker.click_row(1, now + Duration::from_millis(20)));
        assert!(!picker.typing, "clicking a candidate leaves the path field");
        picker.cancel_click();
        assert!(!picker.click_row(1, now + Duration::from_millis(30)));

        picker.toggle_browsing();
        assert!(!picker.click_row(0, now + Duration::from_millis(40)));
        picker.toggle_browsing();
        assert!(
            !picker.click_row(0, now + Duration::from_millis(50)),
            "the same row in another list is a fresh click"
        );
        picker.toggle_browsing();
        assert!(!picker.click_row(0, now + Duration::from_millis(60)));
        assert!(picker.click_row(0, now + Duration::from_millis(70)));
        assert!(!picker.click_row(0, now + Duration::from_millis(80)));
        picker.paste_text("another set");
        assert!(
            !picker.click_row(0, now + Duration::from_millis(90)),
            "pasting a path cancels a pending click in the browser"
        );
    }

    /// Walking the candidates keeps rows in sight past the selection; a
    /// click selects without scrolling, and the next key keeps the margin.
    #[test]
    fn walking_the_candidates_keeps_rows_in_sight_past_the_selection() {
        let candidates = (0..30)
            .map(|index| Candidate {
                label: format!("scene {index}"),
                detail: String::new(),
                path: PathBuf::from(format!("scene-{index}.strudel")),
            })
            .collect();
        let mut picker = FilePicker::new("Open", "open", candidates, Path::new("."));
        let available = Rect::new(0, 0, 100, 40);
        let (_, list) = picker.geometry(available).expect("room for the sheet");
        let shown = usize::from(list.height);
        let margin = crate::scroll::margin(shown);
        assert_eq!(margin, 2);
        for _ in 0..20 {
            picker.move_by(1);
            let first = picker.first_row(shown);
            assert!(first + shown >= (picker.selected + margin + 1).min(30));
        }
        // A click on the last row drawn: selected where the pointer is, and
        // the row under the pointer is still the one clicked.
        let first = picker.first_row(shown);
        let bottom = list.bottom() - 1;
        let row = picker.row_at(available, list.x, bottom).expect("a row");
        picker.select_row(row);
        assert_eq!(picker.selected, first + shown - 1);
        assert_eq!(picker.row_at(available, list.x, bottom), Some(row), "held");
        picker.move_by(1);
        assert_eq!(
            picker.first_row(shown) + shown,
            picker.selected + margin + 1,
            "the key brings the margin back"
        );
    }
}

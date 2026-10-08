//! The set panel lists the set's folder: its scores and, under a fold, the
//! tapes recorded from it. A score is open on the strip, or it is only a
//! file until Enter opens it. ^B docks the panel beside the editor, on the
//! side the settings choose, until ^B again. On a terminal too narrow for
//! a dock, the panel is a sheet with the same list.
//!
//! The panel reads the folder rather than a list of its own: a score is
//! in the set because its file is in the folder, and closing a tab (^W)
//! leaves the file where it is, to be opened again from here.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::scenes::{SceneId, SceneSet, SetFile};
use super::theme::Theme;
use rustel_runtime::product;

/// Default, minimum and maximum column widths, including the rule.
pub const SIDEBAR_WIDTH: u16 = 40;
pub const SIDEBAR_MIN_WIDTH: u16 = 24;
pub const SIDEBAR_MAX_WIDTH: u16 = 80;
/// Two clicks on the same row this close together open it.
const DOUBLE_CLICK_MS: u128 = 500;

/// One tape of the set, as the panel lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TapeRow {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// The tape being written right now.
    pub recording: bool,
    /// When it was recorded, ISO 8601 from the tape's header, or read off
    /// its name when the header cannot be.
    pub recorded: String,
}

/// One line of the list: a score of the folder, the sessions fold, or a
/// tape under it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetLine {
    File(usize),
    Sessions,
    Tape(usize),
}

/// A score as the panel shows it: the folder's file, and where it is on
/// the strip when it is open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRow {
    pub name: String,
    pub path: PathBuf,
    pub open: Option<SceneId>,
    /// Its number on the strip, from one, when it is open.
    pub strip: Option<usize>,
    pub current: bool,
    pub dirty: bool,
}

#[derive(Clone, Debug)]
pub struct SetPanel {
    /// The set's name: its folder's.
    pub set_name: String,
    pub files: Vec<FileRow>,
    /// The set's tapes, newest first.
    pub tapes: Vec<TapeRow>,
    pub lines: Vec<SetLine>,
    pub selected: usize,
    /// The tapes sit under a fold, shut until opened.
    pub sessions_open: bool,
    /// A delete waiting for its second press, by line - one press asks,
    /// the next confirms, anything else calls it off.
    pub deleting: Option<usize>,
    /// What the last verb refused, shown under the list.
    pub error: Option<String>,
    /// The last click, for the second of a double click.
    last_click: Option<(usize, Instant)>,
    /// Lines scrolled past the top, so the selection stays in view with a
    /// margin of lines around it; see [`super::scroll`].
    pub scroll: usize,
    /// The selection was last put there by a click. Until a key or the wheel
    /// moves it the list scrolls only to keep it on screen, so the line
    /// clicked stays under the pointer for the second click of a double.
    pub hold_scroll: bool,
    /// Docked at the right of the editor rather than the left: the rule
    /// is on the panel's left edge then.
    pub on_right: bool,
    /// Preferred width, retained even when the terminal cannot fit it.
    pub width: u16,
}

impl SetPanel {
    /// The panel over `set`, opened on its current scene, its tapes read
    /// from `sessions`; `recording` is the tape being written, when one is.
    pub fn open(set: &SceneSet, sessions: &Path, recording: Option<&Path>) -> Self {
        let mut panel = Self {
            set_name: set.name(),
            files: Vec::new(),
            tapes: Vec::new(),
            lines: Vec::new(),
            selected: 0,
            sessions_open: false,
            deleting: None,
            error: None,
            last_click: None,
            hold_scroll: false,
            scroll: 0,
            on_right: false,
            width: SIDEBAR_WIDTH,
        };
        panel.refresh(set, sessions, recording, None);
        panel
    }

    /// Resize in four-column steps, like a visuals column.
    pub fn resize(&mut self, grow: bool) -> bool {
        let next = if grow {
            self.width.saturating_add(4)
        } else {
            self.width.saturating_sub(4)
        }
        .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        let changed = self.width != next;
        self.width = next;
        changed
    }

    /// Read the folders again, keeping the selection on `keep` - a file
    /// or a tape - or on the current scene.
    pub fn refresh(
        &mut self,
        set: &SceneSet,
        sessions: &Path,
        recording: Option<&Path>,
        keep: Option<&Path>,
    ) {
        self.set_name = set.name();
        let current = set.current().id;
        let files: Vec<FileRow> = set
            .folder_files()
            .into_iter()
            .map(|file: SetFile| {
                let scene = file.open.and_then(|id| set.get(id));
                FileRow {
                    strip: file
                        .open
                        .and_then(|id| set.scores().position(|scene| scene.id == id))
                        .map(|index| index + 1),
                    current: file.open == Some(current),
                    dirty: scene.is_some_and(|scene| scene.dirty),
                    name: file.name,
                    path: file.path,
                    open: file.open,
                }
            })
            .collect();
        let tapes = scan_tapes(sessions, recording);
        let wanted = keep.and_then(|path| {
            files
                .iter()
                .position(|file| file.path == path)
                .map(SetLine::File)
                .or_else(|| {
                    tapes
                        .iter()
                        .position(|tape| tape.path == path)
                        .map(SetLine::Tape)
                })
        });
        if matches!(wanted, Some(SetLine::Tape(_))) {
            self.sessions_open = true;
        }
        let wanted = wanted.or_else(|| {
            files
                .iter()
                .position(|file| file.current)
                .map(SetLine::File)
        });
        self.files = files;
        self.tapes = tapes;
        self.rebuild();
        self.selected = wanted
            .and_then(|line| self.lines.iter().position(|candidate| *candidate == line))
            .unwrap_or(0)
            .min(self.lines.len().saturating_sub(1));
        self.deleting = None;
    }

    /// Where the cursor goes after a row is deleted: the row that took
    /// its place - the next tape, or the last one when the end went -
    /// rather than the top of the list, which is nowhere near what was
    /// being worked on.
    pub fn select_after_delete(&mut self, gone: SetLine) {
        let same_or_last = |index: usize, count: usize| (count > 0).then(|| index.min(count - 1));
        let wanted = match gone {
            SetLine::Tape(index) => same_or_last(index, self.tapes.len())
                .map(SetLine::Tape)
                .unwrap_or(SetLine::Sessions),
            SetLine::File(index) => same_or_last(index, self.files.len())
                .map(SetLine::File)
                .unwrap_or(SetLine::Sessions),
            SetLine::Sessions => SetLine::Sessions,
        };
        self.selected = self
            .lines
            .iter()
            .position(|line| *line == wanted)
            .unwrap_or(0);
    }

    /// What changes without the folder changing: which scene is on
    /// screen, which are edited. Cheap enough for every frame.
    pub fn sync_marks(&mut self, set: &SceneSet) {
        let current = set.current().id;
        for file in &mut self.files {
            let scene = file.open.and_then(|id| set.get(id));
            file.current = file.open == Some(current);
            file.dirty = scene.is_some_and(|scene| scene.dirty);
        }
    }

    /// The lines: every score, then the fold and, open, the tapes under it.
    fn rebuild(&mut self) {
        let mut lines: Vec<SetLine> = (0..self.files.len()).map(SetLine::File).collect();
        lines.push(SetLine::Sessions);
        if self.sessions_open {
            lines.extend((0..self.tapes.len()).map(SetLine::Tape));
        }
        self.lines = lines;
        self.selected = self.selected.min(self.lines.len().saturating_sub(1));
    }

    pub fn move_by(&mut self, delta: isize) {
        let count = self.lines.len();
        if count == 0 {
            return;
        }
        self.selected = ((self.selected as isize + delta).rem_euclid(count as isize)) as usize;
        self.deleting = None;
        self.error = None;
        self.hold_scroll = false;
    }

    /// Move without wrapping: what the wheel and the page keys do.
    pub fn step(&mut self, delta: isize) {
        let count = self.lines.len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        self.deleting = None;
        self.error = None;
        self.hold_scroll = false;
    }

    pub fn selected_line(&self) -> Option<SetLine> {
        self.lines.get(self.selected).copied()
    }

    /// The score on the chosen line, if it is one.
    pub fn selected_file(&self) -> Option<&FileRow> {
        match self.selected_line()? {
            SetLine::File(index) => self.files.get(index),
            _ => None,
        }
    }

    /// The tape on the chosen line, if it is one.
    pub fn selected_tape(&self) -> Option<&TapeRow> {
        match self.selected_line()? {
            SetLine::Tape(index) => self.tapes.get(index),
            _ => None,
        }
    }

    /// Open or shut the fold the tapes sit under.
    pub fn toggle_sessions(&mut self) {
        self.sessions_open = !self.sessions_open;
        self.rebuild();
        if !self.sessions_open
            && let Some(fold) = self
                .lines
                .iter()
                .position(|line| *line == SetLine::Sessions)
        {
            self.selected = self.selected.min(fold);
        }
        self.deleting = None;
        self.error = None;
    }

    /// A click on a line chooses it; a second click on the same line
    /// within half a second is the double click that opens it. Returns
    /// whether this was the second.
    pub fn click(&mut self, line: usize, now: Instant) -> bool {
        let double = self.last_click.is_some_and(|(last, at)| {
            last == line && now.duration_since(at).as_millis() <= DOUBLE_CLICK_MS
        });
        self.selected = line.min(self.lines.len().saturating_sub(1));
        self.deleting = None;
        self.error = None;
        self.last_click = if double { None } else { Some((line, now)) };
        self.hold_scroll = true;
        double
    }

    /// Keep the selection inside a list this tall, with a margin of lines
    /// around it when a key or the wheel put it there.
    pub fn ensure_visible(&mut self, height: u16) {
        let height = usize::from(height.max(1));
        // A click keeps no margin and does not pull the list in from its end
        // either: folding the tapes by click must not move the lines under
        // the pointer.
        let (margin, total) = if self.hold_scroll {
            (0, usize::MAX)
        } else {
            (super::scroll::margin(height), self.lines.len())
        };
        self.scroll = super::scroll::follow(self.scroll, self.selected, height, total, margin);
    }

    /// The panel's docked place: the sidebar, when the layout gave it one.
    /// The rows are the title, the list, the error line and two hint rows;
    /// the column on the editor's side is the rule between the two.
    pub fn docked_parts(sidebar: Rect, on_right: bool) -> Option<DockedParts> {
        if sidebar.is_empty() || sidebar.height < 5 || sidebar.width < 8 {
            return None;
        }
        let inner_width = sidebar.width.saturating_sub(2);
        let inner_x = if on_right {
            sidebar.x + 2
        } else {
            sidebar.x + 1
        };
        let title = Rect::new(inner_x, sidebar.y, inner_width, 1);
        let hints = 2u16.min(sidebar.height.saturating_sub(3));
        let list_height = sidebar
            .height
            .saturating_sub(1)
            .saturating_sub(1)
            .saturating_sub(hints);
        let list = Rect::new(inner_x, sidebar.y + 1, inner_width, list_height);
        let error = Rect::new(inner_x, list.bottom(), inner_width, 1);
        let hint = Rect::new(inner_x, error.bottom(), inner_width, hints);
        let rule_x = if on_right {
            sidebar.x
        } else {
            sidebar.right().saturating_sub(1)
        };
        let rule = Rect::new(rule_x, sidebar.y, 1, sidebar.height);
        Some(DockedParts {
            title,
            list,
            error,
            hint,
            rule,
        })
    }

    /// The sheet form's place: bottom-right like the other sheets, a row
    /// per line up to a screenful.
    pub fn sheet_geometry(&self, available: Rect) -> Option<(Rect, Rect)> {
        let rows = (self.lines.len() as u16).clamp(1, 16);
        // Title, the list, an error line, two hint lines, borders.
        let height = rows + 6;
        let width = self
            .width
            .saturating_add(4)
            .min(available.width.saturating_sub(2));
        if width < SIDEBAR_MIN_WIDTH || available.height < height + 1 {
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

    /// Where the list is drawn: in the sidebar when the layout has one,
    /// else in the sheet.
    pub fn list_area(&self, sidebar: Rect, available: Rect) -> Option<Rect> {
        match Self::docked_parts(sidebar, self.on_right) {
            Some(parts) => Some(parts.list),
            None => self.sheet_geometry(available).map(|(_, list)| list),
        }
    }

    /// The line under a point of the list, if any.
    pub fn row_at(&self, sidebar: Rect, available: Rect, x: u16, y: u16) -> Option<usize> {
        let list = self.list_area(sidebar, available)?;
        if x < list.x || x >= list.right() || y < list.y || y >= list.bottom() {
            return None;
        }
        let index = self.scroll + usize::from(y - list.y);
        (index < self.lines.len()).then_some(index)
    }

    /// Whether the point is over the panel at all.
    pub fn contains(&self, sidebar: Rect, available: Rect, x: u16, y: u16) -> bool {
        match Self::docked_parts(sidebar, self.on_right) {
            Some(_) => sidebar.contains((x, y).into()),
            None => self
                .sheet_geometry(available)
                .is_some_and(|(area, _)| area.contains((x, y).into())),
        }
    }
}

/// The docked panel's rows.
#[derive(Clone, Copy, Debug)]
pub struct DockedParts {
    pub title: Rect,
    pub list: Rect,
    pub error: Rect,
    pub hint: Rect,
    pub rule: Rect,
}

/// The tapes in a set's sessions folder, newest first by when each says
/// it was recorded.
pub fn scan_tapes(directory: &Path, recording: Option<&Path>) -> Vec<TapeRow> {
    let mut tapes: Vec<TapeRow> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .ends_with(product::SESSION_FILE_SUFFIX)
                })
        })
        .map(|path| {
            let name = super::scenes::replay_name(&path);
            let recorded = recorded_at(&path).unwrap_or_else(|| recorded_from_name(&name));
            TapeRow {
                recorded,
                bytes: std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0),
                recording: recording.is_some_and(|current| current == path),
                name,
                path,
            }
        })
        .collect();
    tapes.sort_by(|a, b| b.recorded.cmp(&a.recorded).then(b.name.cmp(&a.name)));
    tapes
}

/// When a tape says it was recorded: the `recorded` field of its first
/// line, which every tape rustel writes begins with.
fn recorded_at(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let header: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    header
        .get("recorded")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The time a tape's name carries, `2026-09-05T12-09-48`, as ISO 8601, so
/// a tape with no readable header still sorts with the rest.
fn recorded_from_name(name: &str) -> String {
    match name.split_once('T') {
        Some((day, time)) if day.len() == 10 => format!("{day}T{}", time.replace('-', ":")),
        _ => name.to_owned(),
    }
}

/// Shorten only generated timestamp names, leaving user names untouched.
fn tape_label(name: &str) -> String {
    let Some(stamp) = name.get(..19) else {
        return name.to_owned();
    };
    let bytes = stamp.as_bytes();
    if !bytes.iter().enumerate().all(|(i, byte)| match i {
        4 | 7 | 13 | 16 => *byte == b'-',
        10 => *byte == b'T',
        _ => byte.is_ascii_digit(),
    }) {
        return name.to_owned();
    }
    let suffix = &name[19..];
    if !suffix.is_empty()
        && !suffix
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return name.to_owned();
    }
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = stamp[5..7].parse::<usize>().unwrap_or(0);
    let Some(month) = month.checked_sub(1).and_then(|n| months.get(n)) else {
        return name.to_owned();
    };
    format!(
        "{month} {} {}{suffix}",
        &stamp[8..10],
        stamp[11..].replace('-', ":")
    )
}

/// Clip by terminal columns so a wide custom name cannot displace its size.
fn fit_tape_label(name: &str, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let clipped = name.width() > width;
    let room = width.saturating_sub(usize::from(clipped));
    let mut result = String::new();
    let mut used = 0;
    for glyph in name.graphemes(true) {
        let columns = glyph.width();
        if used + columns > room {
            break;
        }
        result.push_str(glyph);
        used += columns;
    }
    if clipped && width > 0 {
        result.push('…');
        used += 1;
    }
    result.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    result
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

pub struct SetPanelView<'a> {
    pub panel: &'a SetPanel,
    pub theme: &'a Theme,
    /// The sidebar the layout gave the panel; empty for the sheet form.
    pub sidebar: Rect,
    /// The panel docks at the right of the editor.
    pub on_right: bool,
    pub focused: bool,
    pub keybinds: &'a super::keybinds::Keybinds,
}

impl ratatui::widgets::Widget for SetPanelView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        let bold = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        let (title, list, error, hint) = match SetPanel::docked_parts(self.sidebar, self.on_right) {
            Some(parts) => {
                // Docked, the panel stands on the theme's own ground, so a
                // theme's picture runs under it at the interface opacity
                // as it does under the header; the sheet form is a sheet.
                super::view::clear_surface(
                    buffer,
                    self.sidebar,
                    Style::default().bg(theme.background).fg(theme.foreground),
                );
                let rule = Style::default().fg(theme.rule);
                for y in parts.rule.y..parts.rule.bottom() {
                    buffer.set_stringn(parts.rule.x, y, "│", 1, rule);
                }
                (parts.title, parts.list, parts.error, parts.hint)
            }
            None => {
                let Some((sheet, list)) = self.panel.sheet_geometry(area) else {
                    return;
                };
                super::view::clear_surface(
                    buffer,
                    sheet,
                    Style::default().bg(theme.overlay).fg(theme.foreground),
                );
                super::devices::draw_border(buffer, sheet, theme);
                let title = Rect::new(sheet.x + 2, sheet.y, sheet.width.saturating_sub(4), 1);
                let error = Rect::new(list.x, list.bottom(), list.width, 1);
                let hint = Rect::new(list.x, list.bottom() + 1, list.width, 2);
                (title, list, error, hint)
            }
        };
        let name = format!(" {} ", self.panel.set_name);
        buffer.set_stringn(
            title.x,
            title.y,
            &name,
            usize::from(title.width),
            if self.focused {
                bold
            } else {
                bold.remove_modifier(Modifier::BOLD)
            },
        );
        let width = usize::from(list.width);
        for (offset, line) in self
            .panel
            .lines
            .iter()
            .skip(self.panel.scroll)
            .take(usize::from(list.height))
            .enumerate()
        {
            let index = self.panel.scroll + offset;
            let selected = index == self.panel.selected;
            let y = list.y + offset as u16;
            // Every row's first cell is the selection marker, shown while
            // the panel has the keyboard.
            let marker = if selected && self.focused {
                crate::terminal::symbol("▸")
            } else {
                " "
            };
            let (text, base) = match line {
                SetLine::File(file) => {
                    let Some(file) = self.panel.files.get(*file) else {
                        continue;
                    };
                    let number = match file.strip {
                        Some(number) => format!("{number:>2}"),
                        None => " ·".to_owned(),
                    };
                    let mut notes = String::new();
                    if file.dirty {
                        notes.push_str(" ●");
                    }
                    let room = width.saturating_sub(5 + notes.chars().count());
                    let name: String = file.name.chars().take(room).collect();
                    let style = if file.current {
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD)
                    } else if file.open.is_some() {
                        Style::default().fg(theme.foreground)
                    } else {
                        Style::default().fg(theme.muted)
                    };
                    (format!("{marker}{number} {name}{notes}"), style)
                }
                SetLine::Sessions => {
                    let fold = if self.panel.sessions_open {
                        crate::terminal::symbol("▾")
                    } else {
                        crate::terminal::symbol("▸")
                    };
                    (
                        format!("{marker}{fold} sessions ({})", self.panel.tapes.len()),
                        Style::default().fg(theme.foreground),
                    )
                }
                SetLine::Tape(tape) => {
                    let Some(tape) = self.panel.tapes.get(*tape) else {
                        continue;
                    };
                    let note = if tape.recording {
                        "● rec".to_owned()
                    } else {
                        size_label(tape.bytes)
                    };
                    let room = width.saturating_sub(5 + note.chars().count());
                    let name = fit_tape_label(&tape_label(&tape.name), room);
                    let style = if tape.recording {
                        Style::default().fg(theme.error)
                    } else {
                        Style::default().fg(theme.muted)
                    };
                    (format!("{marker}   {name} {note}"), style)
                }
            };
            let style = if selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                base
            };
            let padded = format!("{text:<width$}");
            buffer.set_stringn(list.x, y, &padded, width, style);
        }
        if let Some(message) = &self.panel.error {
            buffer.set_stringn(
                error.x,
                error.y,
                message,
                usize::from(error.width),
                Style::default().fg(theme.error),
            );
        } else if let Some(line) = self
            .panel
            .deleting
            .and_then(|index| self.panel.lines.get(index))
        {
            let what = match line {
                SetLine::File(file) => self.panel.files.get(*file).map(|file| file.name.clone()),
                SetLine::Tape(tape) => self.panel.tapes.get(*tape).map(|tape| tape.name.clone()),
                SetLine::Sessions => None,
            };
            if let Some(what) = what {
                buffer.set_stringn(
                    error.x,
                    error.y,
                    format!("delete {what}? Enter deletes · Esc keeps"),
                    usize::from(error.width),
                    Style::default().fg(theme.error),
                );
            }
        }
        if self.panel.error.is_none()
            && self.panel.deleting.is_none()
            && let Some(tape) = self.panel.selected_tape()
        {
            let recorded = tape.recorded.replace('T', " ");
            let recorded = recorded
                .strip_suffix('Z')
                .map_or_else(|| recorded.clone(), |stamp| format!("{stamp} UTC"));
            buffer.set_stringn(
                error.x,
                error.y,
                recorded,
                usize::from(error.width),
                Style::default().fg(theme.muted),
            );
        }
        use super::keybinds::BindAction;
        let rename = self.keybinds.hint(BindAction::RenameFile);
        let reveal = self.keybinds.hint(BindAction::ShowFile);
        let rename_hint = format!("{rename} rename · Enter open");
        let reveal_hint = format!("{reveal} reveal · Del delete · +/- size");
        let live_tape_hint = format!("{reveal} reveal · n new first");
        let folder_hint = format!("{reveal} reveal · +/- size");
        let hints = if let Some(tape) = self.panel.selected_tape() {
            [
                rename_hint.as_str(),
                if tape.recording {
                    live_tape_hint.as_str()
                } else {
                    reveal_hint.as_str()
                },
            ]
        } else if self.panel.selected_file().is_some() {
            ["Enter open · n new", reveal_hint.as_str()]
        } else {
            ["←/→ fold · n new", folder_hint.as_str()]
        };
        for (row, text) in hints.iter().enumerate().take(usize::from(hint.height)) {
            buffer.set_stringn(
                hint.x,
                hint.y + row as u16,
                text,
                usize::from(hint.width),
                Style::default().fg(theme.muted),
            );
        }
    }
}

#[cfg(test)]
mod selection_tests {
    //! The selected row's marker and the new-session hint, as drawn.

    use super::tests::{rendered_rows, set_in};
    use super::*;

    const AREA: Rect = Rect::new(0, 0, SIDEBAR_WIDTH, 12);

    /// A panel over a set of three scores and one tape.
    fn panel_with_a_tape(directory: &Path) -> SetPanel {
        let set = set_in(directory);
        let sessions = set.sessions_directory();
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("session-2026-09-24T03-15-30.rustel-session"),
            "{\"recorded\":\"2026-09-24T03:15:30Z\"}\n",
        )
        .unwrap();
        SetPanel::open(&set, &sessions, None)
    }

    /// The list's first cell on the row that shows `label`.
    fn marker(panel: &SetPanel, focused: bool, label: &str) -> char {
        let first = usize::from(SetPanel::docked_parts(AREA, true).unwrap().list.x);
        let rows = rendered_rows(panel, AREA, focused);
        let row = rows.iter().find(|row| row.contains(label)).expect(label);
        row.chars().nth(first).unwrap()
    }

    #[test]
    fn the_selected_row_is_marked_while_the_panel_has_the_keyboard() {
        let directory = tempfile::tempdir().unwrap();
        let mut panel = panel_with_a_tape(directory.path());
        for (selected, label) in [(0, "drop"), (panel.files.len(), "sessions")] {
            panel.selected = selected;
            assert_eq!(marker(&panel, true, label), '▸', "{label}");
            assert_eq!(marker(&panel, false, label), ' ', "{label}");
        }
        assert_eq!(marker(&panel, true, "drop"), ' ', "only the selected row");
        panel.toggle_sessions();
        panel.move_by(1);
        assert_eq!(marker(&panel, true, "Sep 24 03:15:30"), '▸');
        assert_eq!(marker(&panel, false, "Sep 24 03:15:30"), ' ');
    }

    #[test]
    fn the_hints_name_the_unshifted_new_session_key() {
        let directory = tempfile::tempdir().unwrap();
        let mut panel = panel_with_a_tape(directory.path());
        panel.selected = 0;
        let score = rendered_rows(&panel, AREA, true).join("\n");
        assert!(score.contains("Enter open · n new"), "{score}");
        panel.selected = panel.files.len();
        let fold = rendered_rows(&panel, AREA, true).join("\n");
        assert!(fold.contains("←/→ fold · n new"), "{fold}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn set_in(directory: &Path) -> SceneSet {
        std::fs::write(directory.join("intro.strudel"), "// intro").unwrap();
        std::fs::write(directory.join("drop.strudel"), "// drop").unwrap();
        std::fs::write(directory.join("spare.strudel"), "// spare").unwrap();
        SceneSet::open(directory, "starter").unwrap()
    }

    /// The panel docked at the right of `area`, drawn, one string per row.
    pub(super) fn rendered_rows(panel: &SetPanel, area: Rect, focused: bool) -> Vec<String> {
        use ratatui::widgets::Widget;
        let theme = Theme::built_in_default();
        let mut buffer = Buffer::empty(area);
        SetPanelView {
            panel,
            theme: &theme,
            sidebar: area,
            on_right: true,
            focused,
            keybinds: &crate::keybinds::Keybinds::default(),
        }
        .render(area, &mut buffer);
        (0..area.height)
            .map(|y| (0..area.width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// The panel lists the folder's scores by name with their strip
    /// numbers, then the sessions fold, shut, with the tapes under it once
    /// opened; the selection opens on the current scene and follows a
    /// refresh.
    #[test]
    fn the_panel_lists_the_folder_and_folds_the_tapes() {
        let directory = tempfile::tempdir().unwrap();
        let mut set = set_in(directory.path());
        set.select(1);
        set.close_current().unwrap();
        let sessions = set.sessions_directory();
        std::fs::create_dir_all(&sessions).unwrap();
        for name in ["session-2026-09-05T10-00-00", "session-2026-09-06T09-00-00"] {
            std::fs::write(
                sessions.join(format!("{name}{}", product::SESSION_FILE_SUFFIX)),
                "{\"recorded\":\"x\"}\n",
            )
            .unwrap();
        }
        let live = sessions.join("session-2026-09-06T09-00-00.rustel-session");
        let panel = SetPanel::open(&set, &sessions, Some(&live));
        assert_eq!(panel.set_name, set.name());
        assert_eq!(
            panel
                .files
                .iter()
                .map(|file| (file.name.as_str(), file.strip, file.current))
                .collect::<Vec<_>>(),
            [
                ("drop", Some(1), false),
                ("intro", None, false),
                ("spare", Some(2), true)
            ]
        );
        assert_eq!(panel.lines.len(), 4, "three files and the fold");
        assert_eq!(panel.selected, 2, "opened on the current scene");
        assert!(!panel.sessions_open);
        let mut panel = panel;
        panel.selected = 3;
        panel.toggle_sessions();
        assert_eq!(panel.lines.len(), 6);
        assert_eq!(
            panel
                .tapes
                .iter()
                .map(|tape| (tape.name.as_str(), tape.recording))
                .collect::<Vec<_>>(),
            [
                ("2026-09-06T09-00-00", true),
                ("2026-09-05T10-00-00", false)
            ],
            "newest first, the live one marked"
        );
        panel.move_by(1);
        assert!(panel.selected_tape().is_some_and(|tape| tape.recording));
        // A refresh keeps the selection on the tape it was on.
        let keep = panel.selected_tape().map(|tape| tape.path.clone()).unwrap();
        panel.refresh(&set, &sessions, None, Some(&keep));
        assert!(panel.selected_tape().is_some_and(|tape| tape.path == keep));
        assert!(!panel.selected_tape().unwrap().recording);
        panel.toggle_sessions();
        assert_eq!(
            panel.selected, 3,
            "shut, the selection comes up to the fold"
        );
    }

    #[test]
    fn generated_session_names_fit_beside_sizes_without_renaming_files() {
        use unicode_width::UnicodeWidthStr;
        let directory = tempfile::tempdir().unwrap();
        let set = set_in(directory.path());
        let sessions = set.sessions_directory();
        std::fs::create_dir_all(&sessions).unwrap();
        let name = "2026-09-24T03-15-30";
        let path = sessions.join(format!("session-{name}.rustel-session"));
        std::fs::write(&path, "{\"recorded\":\"2026-09-24T03:15:30Z\"}\n").unwrap();
        let mut panel = SetPanel::open(&set, &sessions, None);
        panel.selected = 3;
        panel.toggle_sessions();
        panel.move_by(1);
        let rows = rendered_rows(&panel, Rect::new(0, 0, 30, 15), true);
        let row = rows
            .iter()
            .find(|r| r.contains("Sep 24 03:15:30"))
            .expect("compact date");
        assert!(
            row.contains(&size_label(std::fs::metadata(&path).unwrap().len())),
            "{row}"
        );
        assert!(rows.iter().any(|r| r.contains("2026-09-24 03:15:30 UTC")));
        assert_eq!(panel.selected_tape().unwrap().name, name);
        assert!(path.exists());
        assert_eq!(tape_label("2026-09-24T03-15-30-2"), "Sep 24 03:15:30-2");
        assert_eq!(tape_label("my favourite take"), "my favourite take");
        let clipped = fit_tape_label("演奏🎹 session", 6);
        assert_eq!(clipped.width(), 6);
        assert!(clipped.trim_end().ends_with('…'));
    }

    #[test]
    fn live_tape_hint_does_not_advertise_delete_until_the_tape_is_finished() {
        use ratatui::widgets::Widget;

        let directory = tempfile::tempdir().unwrap();
        let set = set_in(directory.path());
        let sessions = set.sessions_directory();
        std::fs::create_dir_all(&sessions).unwrap();
        let live = sessions.join("session-2026-09-30T16-35-30.rustel-session");
        std::fs::write(&live, "{\"recorded\":\"2026-09-30T16:35:30Z\"}\n").unwrap();
        let mut panel = SetPanel::open(&set, &sessions, Some(&live));
        panel.refresh(&set, &sessions, Some(&live), Some(&live));
        assert!(panel.selected_tape().unwrap().recording);

        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, SIDEBAR_WIDTH, 15);
        let render_text = |panel: &SetPanel| {
            let mut buffer = Buffer::empty(area);
            SetPanelView {
                panel,
                theme: &theme,
                sidebar: area,
                on_right: false,
                focused: true,
                keybinds: &crate::keybinds::Keybinds::default(),
            }
            .render(area, &mut buffer);
            (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = render_text(&panel);
        assert!(text.contains("n new first"), "{text}");
        assert!(!text.contains("Del delete"), "{text}");

        panel.refresh(&set, &sessions, None, Some(&live));
        assert!(!panel.selected_tape().unwrap().recording);
        let text = render_text(&panel);
        assert!(text.contains("Del delete"), "{text}");
    }

    /// A second click on the same line within half a second is a double
    /// click; a click elsewhere, or a slow one, is not.
    #[test]
    fn two_quick_clicks_on_a_line_are_a_double_click() {
        let directory = tempfile::tempdir().unwrap();
        let set = set_in(directory.path());
        let mut panel = SetPanel::open(&set, &set.sessions_directory(), None);
        let now = Instant::now();
        assert!(!panel.click(1, now));
        assert_eq!(panel.selected, 1);
        assert!(!panel.click(2, now + std::time::Duration::from_millis(10)));
        assert!(
            panel.click(2, now + std::time::Duration::from_millis(200)),
            "the same line, quickly"
        );
        assert!(
            !panel.click(2, now + std::time::Duration::from_millis(300)),
            "a double click is spent"
        );
        assert!(!panel.click(2, now + std::time::Duration::from_millis(2000)));
    }

    /// Walking by keys keeps lines in sight past the selection, a click
    /// selects without scrolling so its double click lands on the same line,
    /// and the next key brings the margin back.
    #[test]
    fn keys_keep_a_margin_of_lines_and_a_click_holds_the_list() {
        let directory = tempfile::tempdir().unwrap();
        let set = set_in(directory.path());
        let mut panel = SetPanel::open(&set, &set.sessions_directory(), None);
        let line = panel.lines[0];
        panel.lines = vec![line; 30];
        let height: u16 = 10;
        for _ in 0..9 {
            panel.move_by(1);
            panel.ensure_visible(height);
        }
        assert_eq!(panel.selected, 9);
        assert_eq!(panel.scroll, 2, "two lines stay in sight below");

        let now = Instant::now();
        panel.click(panel.scroll + 9, now);
        panel.ensure_visible(height);
        assert_eq!(panel.scroll, 2, "the clicked line stays under the pointer");
        assert!(
            panel.click(11, now + std::time::Duration::from_millis(100)),
            "the second click lands on the same line"
        );

        panel.move_by(1);
        panel.ensure_visible(height);
        assert_eq!(panel.scroll, 5, "the key keeps two lines below 12");
        panel.step(100);
        panel.ensure_visible(height);
        assert_eq!(panel.scroll, 20, "the last line may reach the bottom");
    }

    /// A click that folds lines away does not pull the list in from its
    /// end: the line clicked stays on the row it was clicked on.
    #[test]
    fn a_click_that_folds_lines_leaves_the_list_where_it_is() {
        let directory = tempfile::tempdir().unwrap();
        let set = set_in(directory.path());
        let mut panel = SetPanel::open(&set, &set.sessions_directory(), None);
        let line = panel.lines[0];
        panel.lines = vec![line; 26];
        panel.scroll = 5;
        panel.click(5, Instant::now());
        // The fold shuts: twenty lines below the clicked one go.
        panel.lines.truncate(6);
        panel.ensure_visible(10);
        assert_eq!(panel.scroll, 5, "the clicked line is still on the top row");
    }

    /// Docked, the panel is the sidebar: a title row, the list, an error
    /// row, two hint rows and the rule; the sheet form comes up
    /// bottom-right, and a point maps to the line under it either way.
    #[test]
    fn docked_and_sheet_forms_map_points_to_lines() {
        let directory = tempfile::tempdir().unwrap();
        let set = set_in(directory.path());
        let mut panel = SetPanel::open(&set, &set.sessions_directory(), None);
        let sidebar = Rect::new(0, 2, SIDEBAR_WIDTH, 20);
        let parts = SetPanel::docked_parts(sidebar, false).unwrap();
        assert_eq!(parts.title, Rect::new(1, 2, 38, 1));
        assert_eq!(parts.list, Rect::new(1, 3, 38, 16));
        assert_eq!(parts.error.y, 19);
        assert_eq!(parts.hint, Rect::new(1, 20, 38, 2));
        assert_eq!(parts.rule, Rect::new(39, 2, 1, 20));
        // At the right, the rule is the panel's first column.
        let right = Rect::new(70, 2, SIDEBAR_WIDTH, 20);
        let parts = SetPanel::docked_parts(right, true).unwrap();
        assert_eq!(parts.rule, Rect::new(70, 2, 1, 20));
        assert_eq!(parts.list, Rect::new(72, 3, 38, 16));
        let frame = Rect::new(0, 0, 100, 30);
        assert_eq!(panel.row_at(sidebar, frame, 5, 4), Some(1));
        assert_eq!(panel.row_at(sidebar, frame, 5, 10), None, "past the lines");
        assert!(panel.contains(sidebar, frame, 39, 10));
        assert!(!panel.contains(sidebar, frame, 40, 10));
        // Scrolled, the rows map through the offset.
        panel.scroll = 1;
        assert_eq!(panel.row_at(sidebar, frame, 5, 3), Some(1));
        panel.scroll = 0;
        // The sheet: bottom-right, a row per line.
        let (sheet, list) = panel.sheet_geometry(frame).unwrap();
        assert_eq!(sheet.width, 44);
        assert_eq!(sheet.right(), 99);
        assert_eq!(list.height, 4);
        assert_eq!(
            panel.row_at(Rect::default(), frame, list.x + 1, list.y + 3),
            Some(3)
        );
        assert!(panel.contains(Rect::default(), frame, sheet.x, sheet.y));
        assert!(SetPanel::docked_parts(Rect::new(0, 0, 6, 20), false).is_none());
        // The selection stays in view.
        panel.selected = 3;
        panel.ensure_visible(2);
        assert_eq!(panel.scroll, 2);
        panel.selected = 0;
        panel.ensure_visible(2);
        assert_eq!(panel.scroll, 0);
    }
}

//! Shared viewports and hit-testing for reference tabs.

use super::*;

/// Where the list sits on a tab that opens straight onto one.
fn default_list(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(3),
        area.width,
        area.height.saturating_sub(4),
    )
}

/// Waveform height above the volume row: three rows when space allows,
/// otherwise one so the sample list remains usable.
pub fn samples_wave_rows(area: Rect) -> u16 {
    const TALL: u16 = 3;
    // Reserve list space before choosing the taller waveform.
    const LIST_FLOOR: u16 = 6;
    if default_list(area).height > LIST_FLOOR + TALL {
        TALL
    } else {
        1
    }
}

/// `(scope_y, meter_y)`: the single row above the volume fader, and the
/// fader's own. The chords and scales tabs draw their one-row scope here;
/// the samples tab gives its wave more room, see [`samples_wave_area`].
pub fn samples_pulse_rows(area: Rect) -> (u16, u16) {
    let meter = area.bottom().saturating_sub(2);
    (meter.saturating_sub(1), meter)
}

pub(crate) fn sample_pulse_rows(area: Rect) -> (u16, u16) {
    // Source, then the two shortcut rows, live below the sample's fader.
    let meter = area.bottom().saturating_sub(4);
    (meter.saturating_sub(1), meter)
}

/// Where the samples tab draws the sounding sample's shape: the rows
/// immediately above the volume fader.
pub fn samples_wave_area(area: Rect) -> Rect {
    let (_, meter) = sample_pulse_rows(area);
    let rows = samples_wave_rows(area).min(meter.saturating_sub(area.y));
    Rect::new(area.x, meter.saturating_sub(rows), area.width, rows)
}

/// A list on a tab with a live audition at its foot.
///
/// Samples need the scope and meter rows. Chords and scales also keep one
/// row immediately above them for the keyboard or note spelling. Reserving
/// these rows in the shared geometry keeps drawing, scrolling and pointer
/// hit-testing on the same viewport.
fn audition_list(area: Rect, tab: Tab) -> Rect {
    let list = default_list(area);
    let reserved = match tab {
        Tab::Samples => 3 + samples_wave_rows(area),
        Tab::Chords | Tab::Scales => 2 + keyboard_rows(area),
        Tab::Reference => 0,
        #[cfg(feature = "hydra")]
        Tab::Examples | Tab::Generator => 0,
    };
    Rect::new(
        list.x,
        list.y,
        list.width,
        list.height.saturating_sub(reserved),
    )
}

#[cfg(feature = "hydra")]
impl SnippetLayout {
    /// The code inside its border, shared by drawing and hit-testing.
    pub fn code_rows(&self) -> Rect {
        Rect::new(
            self.code.x.saturating_add(self.code.width.min(1)),
            self.code.y.saturating_add(self.code.height.min(1)),
            self.code.width.saturating_sub(2),
            self.code.height.saturating_sub(2),
        )
    }
}

#[cfg(feature = "hydra")]
pub fn snippet_layout(inner: Rect, picture: bool) -> SnippetLayout {
    let preview = if picture {
        preview_area(inner)
    } else {
        Rect::new(inner.x, inner.y, 0, 0)
    };
    // The last row is the hint line, the way every other tab reserves one.
    let footer = inner.bottom().saturating_sub(1).max(inner.y);
    let scope = if picture {
        preview.bottom().min(footer)
    } else {
        inner.y.saturating_add(1).min(footer)
    };
    let body_top = scope.saturating_add(1).min(footer);
    let body_height = footer.saturating_sub(body_top);
    let code_height = (body_height / 3)
        .clamp(3, 12)
        .min(body_height.saturating_sub(2));
    // Give the tree more room: 70% of the former code height, rounded to rows.
    let code_height = ((code_height * 7 + 5) / 10 + 2).min(body_height.saturating_sub(2));
    let list_height = body_height.saturating_sub(code_height);
    let list = Rect::new(inner.x, body_top, inner.width, list_height);
    let code = Rect::new(inner.x, list.bottom(), inner.width, code_height);
    SnippetLayout {
        preview,
        scope,
        list,
        code,
        footer,
    }
}

#[cfg(feature = "hydra")]
pub fn generator_rail(row: Rect) -> Rect {
    Rect::new(
        row.x.saturating_add(15),
        row.y,
        row.width.saturating_sub(21),
        1,
    )
}

/// Where the snippets tab puts its picture. Terminal cells are roughly twice
/// as tall as they are wide, and the thumbnail leaves room for the tree.
#[cfg(feature = "hydra")]
pub fn preview_area(inner: Rect) -> Rect {
    let width = inner.width.min(34);
    let height = (width / 2).min(inner.height.saturating_sub(8)).min(11);
    Rect::new(inner.x, inner.y.saturating_add(1), width, height)
}

/// The part of the column inside its rule and margin, shared by drawing
/// and hit-testing.
pub fn inner_area(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(2),
        area.y,
        area.width.saturating_sub(3),
        area.height,
    )
}

/// Tabs in drawing and cycling order, with labels from widest to shortest.
pub(super) fn tabs() -> &'static [(Tab, [&'static str; 3])] {
    #[cfg(feature = "hydra")]
    {
        &[
            (Tab::Reference, ["reference", "ref", "ref"]),
            (Tab::Samples, ["samples", "samp", "smp"]),
            (Tab::Chords, ["chords", "chord", "chd"]),
            (Tab::Scales, ["scales", "scale", "scl"]),
            (Tab::Generator, ["generator", "gen", "gen"]),
            (Tab::Examples, ["examples", "examp", "ex"]),
        ]
    }
    #[cfg(not(feature = "hydra"))]
    {
        &[
            (Tab::Reference, ["reference", "ref", "ref"]),
            (Tab::Samples, ["samples", "samp", "smp"]),
            (Tab::Chords, ["chords", "chord", "chd"]),
            (Tab::Scales, ["scales", "scale", "scl"]),
        ]
    }
}

/// Label positions and the number of hidden tabs, shared by drawing and
/// hit-testing. Use the widest labels that fit; if even the shortest do
/// not fit, shift the visible window toward `current` and reserve `+N`.
pub(crate) fn tab_layout(inner: Rect, current: Tab) -> (Vec<(Tab, &'static str, u16)>, usize) {
    const GAP: u16 = 2;
    // `+N` is two columns for any number of tabs this panel will ever
    // have, and one more keeps it off the last label.
    const MARKER: u16 = 3;
    let all = tabs();
    let width_of = |label: &str| UnicodeWidthStr::width(label) as u16;
    let fit = |tier: usize, from: usize, right: u16| {
        let mut placed: Vec<(Tab, &'static str, u16)> = Vec::with_capacity(all.len());
        let mut x = inner.x;
        for (tab, labels) in &all[from.min(all.len())..] {
            let label = labels[tier];
            if x.saturating_add(width_of(label)) > right {
                break;
            }
            placed.push((*tab, label, x));
            x = x.saturating_add(width_of(label) + GAP);
        }
        placed
    };
    for tier in 0..3 {
        let placed = fit(tier, 0, inner.right());
        if placed.len() == all.len() {
            return (placed, 0);
        }
    }
    // Narrower than the tightest tier: seat what fits, from as far left as
    // the current tab allows.
    let at = all.iter().position(|(tab, _)| *tab == current).unwrap_or(0);
    let right = inner.right().saturating_sub(MARKER);
    let mut from = 0;
    let mut placed = fit(2, from, right);
    // Not on the row yet: give up the leftmost tab and look again. It
    // ends at the current one, which is seated alone if it comes to that.
    while from < at && at >= from + placed.len() {
        from += 1;
        placed = fit(2, from, right);
    }
    let hidden = all.len() - placed.len();
    (placed, hidden)
}

/// The tab under a pointer, read off the same layout the header drew -
/// which is why it takes the tab the panel is showing too: the row of
/// labels depends on it once they stop all fitting.
pub fn tab_at(inner: Rect, current: Tab, x: u16, y: u16) -> Option<Tab> {
    if y != inner.y {
        return None;
    }
    tab_layout(inner, current)
        .0
        .into_iter()
        .find(|(_, label, at)| {
            x >= *at && x < at.saturating_add(UnicodeWidthStr::width(*label) as u16)
        })
        .map(|(tab, _, _)| tab)
}

/// The rows an entry's body occupies inside the column: under the header
/// line, above the footer. Drawing and hit-testing both read this.
pub fn entry_body_area(inner: Rect) -> Rect {
    Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(2),
    )
}

impl ReferencePanel {
    /// Rows of the panel the pointer can act on.
    pub fn geometry(&self, area: Rect) -> PanelGeometry {
        // Snippets share their list's space with a code panel.
        // Hit-testing follows the same layout as drawing.
        #[cfg(feature = "hydra")]
        let list = if self.tab.is_snippets() {
            self.snippet_layout(area).list
        } else if self.bank_list() {
            audition_list(area, Tab::Reference)
        } else {
            audition_list(area, self.tab)
        };
        #[cfg(not(feature = "hydra"))]
        let list = audition_list(
            area,
            if self.bank_list() {
                Tab::Reference
            } else {
                self.tab
            },
        );
        PanelGeometry {
            list,
            first_row: self.first_visible_row(list.height),
        }
    }

    /// The open tab's selection, and where its list is scrolled to.
    pub(super) fn selection_and_scroll(&self) -> (usize, &std::cell::Cell<usize>) {
        if self.bank_list() {
            return (self.selected, &self.scroll);
        }
        match self.tab {
            Tab::Reference => (self.selected, &self.scroll),
            Tab::Samples => (self.sound_selected, &self.sound_scroll),
            Tab::Chords => (self.chord_selected, &self.chord_scroll),
            Tab::Scales => (self.scale_selected, &self.scale_scroll),
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => (self.snippet_selected, &self.snippet_scroll),
        }
    }

    /// How many rows the open tab's list has.
    fn row_count(&self) -> usize {
        if self.bank_list() {
            return self.rows.len();
        }
        match self.tab {
            Tab::Reference => self.rows.len(),
            Tab::Samples => self.sound_rows().len(),
            Tab::Chords => self.chord_rows().len(),
            Tab::Scales => self.scale_rows().len(),
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => self.snippet_lines().len(),
        }
    }

    /// What a key can change about where the list stands: the tab, the
    /// selection, and how many rows there are once a row opened or folded.
    /// A key that changes none of them - a preview, a copy - leaves a
    /// clicked row where the click put it.
    pub fn scroll_signature(&self) -> (Tab, usize, usize) {
        (self.tab, self.selection_and_scroll().0, self.row_count())
    }

    pub(super) fn first_visible_row(&self, height: u16) -> usize {
        let height = usize::from(height.max(1));
        let (selected, scroll) = self.selection_and_scroll();
        // The window moves only as far as it must, keeping a margin of rows
        // around a selection the keys or the wheel moved, so the rows coming
        // next are on screen before they are selected - an opened bank's
        // first samples included. A click holds the margin back and never
        // pulls the list in from its end either: expanding or folding a
        // bank by click must not move the list underneath the pointer.
        let first = if self.hold_scroll {
            super::super::scroll::follow(scroll.get(), selected, height, usize::MAX, 0)
        } else if self.tab == Tab::Reference {
            // The reference's tag headings are labels, not choices: the
            // margin counts the entries past them.
            super::super::scroll::follow_choices(
                scroll.get(),
                selected,
                height,
                self.rows.len(),
                super::super::scroll::margin(height),
                |row| matches!(self.rows.get(row), Some(BrowseRow::Entry(_))),
            )
        } else {
            super::super::scroll::follow(
                scroll.get(),
                selected,
                height,
                self.row_count(),
                super::super::scroll::margin(height),
            )
        };
        scroll.set(first);
        first
    }
}

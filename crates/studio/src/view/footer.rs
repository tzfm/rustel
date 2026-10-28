use super::*;

/// The master dock at the bottom right: meter, fader and readouts.
pub(super) const DOCK_WIDTH: u16 = 40;
/// A narrower dock for terminals without room for the full one.
pub(super) const DOCK_WIDTH_NARROW: u16 = 28;
/// The master scope beside the dock: what it takes when the row can spare
/// it, and the least it is worth drawing in. Between the two it takes what
/// is free; under the floor the row keeps the cells for the status and the
/// orbit chips instead.
pub(super) const SCOPE_WIDTH: u16 = 18;
const SCOPE_MIN_WIDTH: u16 = 8;
/// The narrowest footer that gets the full master dock rather than the
/// narrow one.
const WIDE_FOOTER_WIDTH: u16 = 110;
/// Cells the status line is promised before the scope and the orbit chips
/// take theirs, and the least it is ever promised on a narrow row. It
/// keeps whatever they leave on top of this.
const STATUS_RESERVED_WIDTH: u16 = 28;
const STATUS_LEAST_WIDTH: u16 = 20;

/// Regions of the footer that respond to a click.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FooterHits {
    /// The output-device chip.
    pub device_chip: Rect,
    /// The MIDI-port chip.
    pub midi_chip: Rect,
    /// The master dock (meter, fader, readouts).
    pub dock: Rect,
    /// The draggable level bar inside the dock.
    pub meter: Rect,
    /// The master scope, when there is room for one.
    pub scope: Rect,
    /// The status line, whose path is clickable when it names a file.
    pub status: Rect,
    /// The orbit chips on the status row, lowest orbit first.
    pub orbits: [(u8, Rect); MAX_ORBIT_CHIPS],
    pub orbit_count: usize,
}

/// The remote-control icon sits at the left of the status line, with a
/// blank cell before the status message. Paint and hit testing use the
/// icon's two cells; short status lines keep their room for a useful message.
pub fn remote_indicator_rect(status: Rect, enabled: bool) -> Rect {
    const WIDTH: u16 = 2; // "<>"
    if !enabled || status.height == 0 || status.width < 20 {
        return Rect::default();
    }
    Rect::new(status.x, status.y, WIDTH, 1)
}

/// The most orbit chips the footer shows.
pub const MAX_ORBIT_CHIPS: usize = 8;
/// One chip: `3 ▮▮▮▯ 5/6`.
const ORBIT_CHIP_WIDTH: u16 = 12;

/// Keep status, devices and master controls below notice and retained-note rows.
/// Tiny degraded layouts preserve the controls and give errors priority.
/// `show_footer` off leaves the status line alone under the notices.
pub(crate) fn footer_content_area(
    area: Rect,
    notice_rows: u16,
    has_notes: bool,
    show_footer: bool,
) -> Rect {
    let content_rows = 1 + u16::from(show_footer);
    let extra =
        (notice_rows.min(2) + u16::from(has_notes)).min(area.height.saturating_sub(content_rows));
    Rect::new(area.x, area.y + extra, area.width, area.height - extra)
}

/// The status line alone: an error and the caret's line:column, with no
/// meter, orbits or device chips. What the footer setting leaves when it
/// is off.
pub fn status_footer_hits(area: Rect) -> FooterHits {
    if area.is_empty() {
        return FooterHits::default();
    }
    FooterHits {
        status: Rect::new(
            area.x.saturating_add(1),
            area.y,
            area.width.saturating_sub(2),
            1.min(area.height),
        ),
        ..FooterHits::default()
    }
}

/// Where the footer draws its chips and its dock, computed the same way it
/// renders them. The dock sits at the bottom right, like the master section
/// of a desk, with the scope to its left.
pub fn footer_hits(
    area: Rect,
    chrome_device: Option<&str>,
    chrome_input: Option<&str>,
    midi_ports: MidiPortCounts,
    pads: usize,
    orbits: &[OrbitLevel],
) -> FooterHits {
    if area.is_empty() {
        return FooterHits::default();
    }
    let dock_width = if area.width >= WIDE_FOOTER_WIDTH {
        DOCK_WIDTH
    } else {
        DOCK_WIDTH_NARROW
    }
    .min(area.width.saturating_sub(24));
    let dock = Rect::new(
        area.right().saturating_sub(dock_width + 1),
        area.y,
        dock_width,
        area.height.min(2),
    );
    // The scope takes what is free between the status line's own third and
    // the dock, up to its full width: a terminal too narrow for all of it
    // still gets a smaller one, and only a row with nothing to spare goes
    // without. (A hundred and ten columns used to be the price of any
    // scope at all, which is a common terminal width to be just under.)
    // The status keeps a third of the row, but no more than a sentence's
    // worth: past that the cells are better spent on the scope and the
    // orbit chips, and the status still takes everything they leave.
    let least_status = (area.width / 3).clamp(STATUS_LEAST_WIDTH, STATUS_RESERVED_WIDTH);
    // The cell between the status and the scope is the gap they need to
    // read as two things rather than one run of text.
    let free = dock.x.saturating_sub(area.x + 2 + least_status);
    let scope_width = if area.height >= 2 && settings::master_scope() && free >= SCOPE_MIN_WIDTH {
        SCOPE_WIDTH.min(free)
    } else {
        0
    };
    let scope = Rect::new(
        dock.x.saturating_sub(scope_width + 1),
        area.y,
        scope_width,
        area.height.min(2),
    );
    let chip_y = area.y + u16::from(area.height >= 2);
    let device = device_chip_text(chrome_device, chrome_input);
    let midi = devices_chip_text(midi_ports, pads);
    let device_width = UnicodeWidthStr::width(device.as_str()) as u16;
    let midi_width = UnicodeWidthStr::width(midi.as_str()) as u16;
    let x = area.x + 1;
    let mut status_right = if scope.is_empty() { dock.x } else { scope.x }.saturating_sub(1);
    // Orbit chips sit at the right of the status row, the highest orbits
    // kept when there is no room for all; the status text takes what is
    // left, never less than a third of the row.
    let mut orbit_hits = [(0u8, Rect::default()); MAX_ORBIT_CHIPS];
    let mut orbit_count = 0;
    let min_status = least_status;
    let room = status_right.saturating_sub(x + min_status) / (ORBIT_CHIP_WIDTH + 1);
    let shown = orbits.len().min(MAX_ORBIT_CHIPS).min(usize::from(room));
    if shown > 0 {
        let first = orbits.len() - shown;
        let mut chip_x = status_right.saturating_sub(shown as u16 * (ORBIT_CHIP_WIDTH + 1));
        for level in &orbits[first..] {
            orbit_hits[orbit_count] = (level.orbit, Rect::new(chip_x, area.y, ORBIT_CHIP_WIDTH, 1));
            orbit_count += 1;
            chip_x += ORBIT_CHIP_WIDTH + 1;
        }
        status_right = status_right.saturating_sub(shown as u16 * (ORBIT_CHIP_WIDTH + 1) + 1);
    }
    let device_chip = Rect::new(x, chip_y, device_width.min(area.width), 1);
    let midi_chip = Rect::new(
        (x + device_width + 2).min(area.right()),
        chip_y,
        midi_width.min(area.width),
        1,
    );
    // A chip keeps its cells whenever the row has room for it, and loses the
    // rect along with the paint when it does not - one source of truth, so a
    // click can never land on a chip that was not drawn.
    let fits = |chip: Rect| chip.right() <= status_right;
    FooterHits {
        orbits: orbit_hits,
        orbit_count,
        status: Rect::new(x, area.y, status_right.saturating_sub(x), 1),
        device_chip: if area.height >= 2 && fits(device_chip) {
            device_chip
        } else {
            Rect::default()
        },
        midi_chip: if area.height >= 2 && fits(midi_chip) {
            midi_chip
        } else {
            Rect::default()
        },
        dock,
        meter: MasterDock::geometry(dock).unwrap_or_default(),
        scope,
    }
}

fn device_chip_text(device: Option<&str>, input: Option<&str>) -> String {
    match (device, input) {
        (Some(device), Some(input)) => format!("♪ {device} · ♩ {input}"),
        (Some(device), None) => format!("♪ {device}"),
        (None, Some(input)) => format!("♪ no output · ♩ {input}"),
        (None, None) => "♪ no output".to_owned(),
    }
}

/// How many cells of the device label the current input peak lights. Full
/// scale reaches the end of the label; the 250 ms activity light keeps one
/// cell on between snapshots so a quiet microphone does not strobe.
fn input_chip_lit_width(peak_db: f32, width: u16) -> u16 {
    let full_scale = scale_position(0.0);
    let fraction = (scale_position(peak_db) / full_scale).clamp(0.0, 1.0);
    ((f32::from(width) * fraction).ceil() as u16)
        .max(1)
        .min(width)
}

/// `⌁ 1 out · 0 in`, or `⌁ no midi`: the footer chip and the mixer desk
/// word the ports alike.
pub(crate) fn midi_chip_text(ports: MidiPortCounts) -> String {
    let symbol = crate::terminal::symbol("⌁");
    if ports.is_empty() {
        format!("{symbol} no midi")
    } else {
        format!("{symbol} {} out · {} in", ports.outputs, ports.inputs)
    }
}

/// The MIDI chip with the pads beside it: `⌁ 1 out · 0 in · ▣ 1 pad`.
fn devices_chip_text(ports: MidiPortCounts, pads: usize) -> String {
    match pads {
        0 => midi_chip_text(ports),
        1 => format!(
            "{} · {} 1 pad",
            midi_chip_text(ports),
            crate::terminal::symbol("▣")
        ),
        many => format!(
            "{} · {} {many} pads",
            midi_chip_text(ports),
            crate::terminal::symbol("▣")
        ),
    }
}

pub(super) struct Footer<'a, 'b> {
    pub(super) chrome: &'a StudioChrome<'b>,
}

pub(crate) fn render_piano_notes(buffer: &mut Buffer, area: Rect, notes: &str, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let area = Rect { height: 1, ..area };
    let style = Style::default().bg(theme.surface).fg(theme.foreground);
    clear_surface(buffer, area, style);
    buffer.set_stringn(
        area.x,
        area.y,
        ellipsize(notes, area.width),
        usize::from(area.width),
        style,
    );
}

/// A compact mode indicator usable in the ordinary footer or a spare Zen row.
/// The caller owns its room; the label fades while note names stay steady.
pub(crate) fn render_piano_indicator(
    buffer: &mut Buffer,
    area: Rect,
    status: &str,
    theme: &Theme,
    pulse: f32,
) {
    if area.is_empty() {
        return;
    }
    let area = Rect { height: 1, ..area };
    clear_surface(
        buffer,
        area,
        Style::default().bg(theme.surface).fg(theme.foreground),
    );
    let status = crate::terminal::safe_text(status);
    buffer.set_stringn(
        area.x,
        area.y,
        status,
        usize::from(area.width),
        Style::default().fg(theme.foreground),
    );
    // Terminal-owned palette colours cannot be interpolated reliably. Keep
    // those steady instead of turning the smooth fade into a hard blink.
    let background = if crate::theme::true_rgb(theme.surface).is_some()
        && crate::theme::true_rgb(theme.accent).is_some()
    {
        mix(theme.surface, theme.accent, pulse)
    } else {
        theme.accent
    };
    let style = Style::default()
        .fg(theme.surface)
        .bg(background)
        .add_modifier(Modifier::BOLD);
    buffer.set_stringn(area.x, area.y, "PIANO", usize::from(area.width), style);
}

impl Widget for Footer<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.chrome.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        let warning = self.chrome.audio_warning.map(|message| {
            if UnicodeWidthStr::width(message) > usize::from(area.width.saturating_sub(2)) {
                "Try raising latency: Settings → General"
            } else {
                message
            }
        });
        let content = footer_content_area(
            area,
            u16::from(self.chrome.error.is_some()) + u16::from(warning.is_some()),
            self.chrome.piano_notes.is_some(),
            self.chrome.show_footer,
        );
        // Allocate spare rows to errors, retained piano notes, then audio advice.
        let mut notice_y = area.y;
        if notice_y < content.y
            && let Some(message) = self.chrome.error
        {
            let message = crate::terminal::safe_text(message);
            buffer.set_stringn(
                area.x.saturating_add(1),
                notice_y,
                ellipsize(&message, area.width.saturating_sub(2)),
                usize::from(area.width.saturating_sub(2)),
                Style::default().fg(theme.error),
            );
            notice_y += 1;
        }
        if notice_y < content.y
            && let Some(notes) = self.chrome.piano_notes
        {
            render_piano_notes(
                buffer,
                Rect::new(
                    area.x.saturating_add(1),
                    notice_y,
                    area.width.saturating_sub(2),
                    1,
                ),
                notes,
                theme,
            );
            notice_y += 1;
        }
        // A retained query error must not hide sustained-audio advice. Give
        // both full-width rows; in a cramped terminal errors and piano notes
        // take priority over the optional hint.
        if notice_y < content.y
            && let Some(message) = warning
        {
            let message = crate::terminal::safe_text(message);
            buffer.set_stringn(
                area.x.saturating_add(1),
                notice_y,
                ellipsize(&message, area.width.saturating_sub(2)),
                usize::from(area.width.saturating_sub(2)),
                Style::default().fg(theme.warn),
            );
        }
        let hits = if self.chrome.show_footer {
            footer_hits(
                content,
                self.chrome.device,
                self.chrome.input,
                self.chrome.midi_ports,
                self.chrome.pads,
                self.chrome.orbits,
            )
        } else {
            status_footer_hits(content)
        };
        // Everything textual stops short of the orbit chips, the scope and
        // the dock.
        let remote_indicator = remote_indicator_rect(hits.status, self.chrome.remote_control);
        let right_edge = hits.status.right();
        let mut message_left = if remote_indicator.is_empty() {
            content.x.saturating_add(1)
        } else {
            remote_indicator.right().saturating_add(1)
        };
        for (index, (orbit, rect)) in hits.orbits.iter().enumerate() {
            if index >= hits.orbit_count {
                break;
            }
            let Some(level) = self
                .chrome
                .orbits
                .iter()
                .find(|level| level.orbit == *orbit)
            else {
                continue;
            };
            render_orbit_chip(buffer, *rect, level, self.chrome.output_pairs, theme);
        }

        // Piano mode must remain visible while typing plays notes. Errors keep
        // their dedicated row; a terminal too short for that still shows them.
        let inline_notice = content.y == area.y;
        let piano = self
            .chrome
            .piano
            .filter(|_| !inline_notice || self.chrome.error.is_none());
        let (message, color) = match (
            inline_notice,
            self.chrome.error,
            piano,
            warning,
            self.chrome.lint,
        ) {
            (true, Some(error), _, _, _) => (error.to_owned(), theme.error),
            (_, _, Some(_), _, _) => (self.chrome.status.to_owned(), theme.foreground),
            (true, _, _, Some(warning), _) => (warning.to_owned(), theme.warn),
            (_, _, _, _, Some(lint)) => (format!("✗ {lint}"), theme.warn),
            _ => (self.chrome.status.to_owned(), theme.foreground),
        };
        let label = self
            .chrome
            .find_label
            .map(str::to_owned)
            .or_else(|| self.chrome.vim_label.map(|label| format!("VIM {label}")));
        if self.chrome.piano.is_none()
            && let Some(label) = label
        {
            let label = crate::terminal::safe_text(&label);
            let label = ellipsize(&label, right_edge.saturating_sub(message_left));
            buffer.set_stringn(
                message_left,
                content.y,
                &label,
                usize::from(right_edge.saturating_sub(message_left)),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            );
            message_left += UnicodeWidthStr::width(label.as_str()) as u16;
            if right_edge.saturating_sub(message_left) >= 3 {
                buffer.set_string(
                    message_left,
                    content.y,
                    " · ",
                    Style::default().fg(theme.muted),
                );
                message_left += 3;
            } else {
                message_left = right_edge;
            }
        }
        // The caret's line and column, a vim-style ruler (`2,4`), shown the
        // way vim's own ruler always is. The gutter can be turned off and
        // the column is shown nowhere else, so it stays put right of the
        // message with one cell of gap before it; the message gives way
        // instead, cut to fit with a trailing ellipsis, and only when the
        // status area cannot even hold the ruler, its gap and an ellipsis
        // does the ruler step aside and the message take the whole row.
        // Whatever the sentence, and wherever in the studio it was
        // written: the status line is read more often than anything else
        // on the screen, and threading the glyph table through every
        // status string in the place would miss one.
        let message = crate::terminal::safe_text(&message).into_owned();
        let mut message = message;
        let mut message_right = right_edge;
        if self.chrome.piano.is_none()
            && let Some((line, column)) = self.chrome.caret
        {
            let ruler = format!("{line},{column}");
            let ruler_width = UnicodeWidthStr::width(ruler.as_str()) as u16;
            if right_edge.saturating_sub(message_left) >= ruler_width + 2 {
                let ruler_x = right_edge.saturating_sub(ruler_width);
                buffer.set_stringn(
                    ruler_x,
                    content.y,
                    ruler,
                    usize::from(ruler_width),
                    Style::default().fg(theme.muted),
                );
                message_right = ruler_x.saturating_sub(1);
                let room = message_right.saturating_sub(message_left);
                message = ellipsize(&message, room);
            }
        }
        if let Some(pulse) = piano {
            render_piano_indicator(
                buffer,
                Rect::new(
                    message_left,
                    content.y,
                    message_right.saturating_sub(message_left),
                    1,
                ),
                &message,
                theme,
                pulse,
            );
        } else {
            buffer.set_stringn(
                message_left,
                content.y,
                message,
                usize::from(message_right.saturating_sub(message_left)),
                Style::default().fg(color),
            );
        }
        if !remote_indicator.is_empty() {
            let style = if self.chrome.remote_receiving {
                Style::default().fg(theme.ok).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            buffer.set_stringn(
                remote_indicator.x,
                remote_indicator.y,
                "<>",
                usize::from(remote_indicator.width),
                style,
            );
        }

        // Stopped, the widget draws its own centre line: the meter's
        // neighbour is an instrument at rest rather than a hole.
        if !hits.scope.is_empty() {
            // The footer's scope is the docks' own widget in its line
            // style and the theme's colours: the mix as a trace, the same
            // picture at a glance that a dock shows in full.
            crate::viz_panel::ScopeView {
                audio: self.chrome.audio,
                motion: self.chrome.motion,
                style: crate::viz_panel::ScopeStyle::Line,
                look: crate::viz_panel::Look {
                    theme,
                    colour: crate::viz_panel::Colouring::Theme,
                    seconds: 0.0,
                    level: 0.0,
                },
            }
            .render(hits.scope, buffer);
        }
        MasterDock {
            keybinds: self.chrome.keybinds,
            state: self.chrome.master,
            theme,
            now: self.chrome.now,
            playing: self.chrome.playing,
            limiter: self.chrome.master_limiter,
        }
        .render(hits.dock, buffer);

        if content.height < 2 {
            return;
        }
        // A chip with no rect was not drawn, and cannot be clicked.
        // The chips are the lights: a device that is doing something right
        // now - signal on the input, a message on a MIDI port, a thumb on a
        // pad - is drawn lit, so what is connected can be seen working
        // before a score asks it anything.
        if !hits.device_chip.is_empty() {
            buffer.set_stringn(
                hits.device_chip.x,
                hits.device_chip.y,
                device_chip_text(self.chrome.device, self.chrome.input),
                usize::from(hits.device_chip.width),
                Style::default().fg(theme.accent),
            );
            // `set_stringn` leaves the continuation cell of a wide glyph
            // unstyled; give the whole chip its idle colour first.
            buffer.set_style(hits.device_chip, Style::default().fg(theme.accent));
            if self.chrome.input_active && self.chrome.input.is_some() {
                let lit_width =
                    input_chip_lit_width(self.chrome.input_peak_db, hits.device_chip.width);
                let lit_color = legible_against_floor(theme.ok, theme.surface, 3.0);
                for x in hits.device_chip.x..hits.device_chip.x + lit_width {
                    if let Some(cell) = buffer.cell_mut((x, hits.device_chip.y)) {
                        cell.set_fg(lit_color)
                            .set_style(Style::default().add_modifier(Modifier::BOLD));
                    }
                }
            }
        }
        if !hits.midi_chip.is_empty() {
            let style = if self.chrome.midi_active || self.chrome.pad_active {
                Style::default().fg(theme.ok).add_modifier(Modifier::BOLD)
            } else if !self.chrome.midi_ports.is_empty() || self.chrome.pads > 0 {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.muted)
            };
            buffer.set_stringn(
                hits.midi_chip.x,
                hits.midi_chip.y,
                devices_chip_text(self.chrome.midi_ports, self.chrome.pads),
                usize::from(hits.midi_chip.width),
                style,
            );
        }
    }
}
/// Draw the toast: centred near the top, over everything.
///
/// Deliberately not the status line. A confirmation competes with whatever the
/// status was already saying, and the thing being confirmed usually made the
/// panel disappear - so it goes where the eye already is.
///
/// `top` is the first row the chrome above it does not own, so the toast
/// does not land on the menu bar or the header.
pub(super) fn render_toast(buffer: &mut Buffer, area: Rect, top: u16, text: &str, theme: &Theme) {
    let width = (UnicodeWidthStr::width(text) as u16).saturating_add(4);
    if area.width < width || area.height < 3 {
        return;
    }
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = top.min(area.bottom().saturating_sub(1));
    let box_area = Rect::new(x, y, width, 1);
    clear_surface(
        buffer,
        box_area,
        Style::default().bg(theme.accent).fg(theme.fill_text()),
    );
    buffer.set_stringn(
        x + 2,
        y,
        text,
        usize::from(width.saturating_sub(3)),
        Style::default()
            .bg(theme.accent)
            .fg(theme.fill_text())
            .add_modifier(Modifier::BOLD),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::tests::*;

    /// The pads ride the MIDI chip, so the footer says what is plugged in
    /// before a score asks for any of it.
    #[test]
    fn the_devices_chip_counts_the_pads_beside_the_midi_ports() {
        assert_eq!(devices_chip_text(midi_ports(0, 0), 0), "⌁ no midi");
        assert_eq!(
            devices_chip_text(midi_ports(1, 0), 1),
            "⌁ 1 out · 0 in · ▣ 1 pad"
        );
        assert_eq!(devices_chip_text(midi_ports(0, 1), 0), "⌁ 0 out · 1 in");
        assert_eq!(
            devices_chip_text(midi_ports(2, 1), 3),
            "⌁ 2 out · 1 in · ▣ 3 pads"
        );
        let with = footer_hits(
            Rect::new(0, 0, 160, 2),
            Some("out"),
            None,
            midi_ports(1, 0),
            2,
            &[],
        );
        let without = footer_hits(
            Rect::new(0, 0, 160, 2),
            Some("out"),
            None,
            midi_ports(1, 0),
            0,
            &[],
        );
        assert!(
            with.midi_chip.width > without.midi_chip.width,
            "the chip grows to fit the pads"
        );
    }

    #[test]
    fn a_status_only_footer_has_no_meter_orbits_or_device_chips() {
        let area = Rect::new(0, 20, 150, 1);
        let hits = status_footer_hits(area);
        assert!(hits.dock.is_empty());
        assert!(hits.scope.is_empty());
        assert!(hits.device_chip.is_empty());
        assert!(hits.midi_chip.is_empty());
        assert_eq!(hits.orbit_count, 0);
        assert_eq!(hits.status.y, area.y);
        assert_eq!(
            hits.status.right(),
            area.right() - 1,
            "the status keeps the footer's one-cell right margin"
        );
    }
    /// The scope keeps its place on a narrow row: it takes what is free
    /// between the status's share and the dock, shrinking rather than
    /// vanishing, and the orbit chips take what it leaves. A hundred and
    /// ten columns used to be the price of any scope at all.
    #[test]
    fn the_footer_keeps_a_scope_on_a_narrow_row() {
        let orbits: Vec<crate::engine::OrbitLevel> = (0..3)
            .map(|orbit| crate::engine::OrbitLevel {
                orbit,
                peak: 0.5,
                pair: 0,
            })
            .collect();
        let hits = |width: u16| {
            footer_hits(
                Rect::new(0, 0, width, 2),
                Some("out"),
                None,
                midi_ports(1, 0),
                0,
                &orbits,
            )
        };
        let narrow = hits(100);
        assert_eq!(
            narrow.dock.width, DOCK_WIDTH_NARROW,
            "a narrow row still narrows the dock"
        );
        assert_eq!(narrow.scope.width, SCOPE_WIDTH, "and keeps its scope");
        assert_eq!(narrow.scope.right(), narrow.dock.x.saturating_sub(1));
        assert!(narrow.orbit_count > 0, "with room left for a chip");
        // Wider, the dock grows and the chips multiply; the scope is the
        // same width throughout.
        let wide = hits(150);
        assert_eq!(wide.dock.width, DOCK_WIDTH);
        assert_eq!(wide.scope.width, SCOPE_WIDTH);
        assert!(wide.orbit_count > narrow.orbit_count);
        // Narrower still, the scope shrinks before it goes.
        let small = hits(64);
        assert!(
            (SCOPE_MIN_WIDTH..SCOPE_WIDTH).contains(&small.scope.width),
            "shrunk, not gone: {}",
            small.scope.width
        );
        assert_eq!(small.orbit_count, 0, "the chips give way first");
        // And a row with nothing to spare has none.
        assert!(hits(48).scope.is_empty());
        // The status keeps its share whatever else is drawn.
        for width in [48u16, 64, 100, 150] {
            let hits = hits(width);
            assert!(
                hits.status.width >= STATUS_LEAST_WIDTH.min(width),
                "the status kept {} of {width}",
                hits.status.width
            );
        }
    }

    #[test]
    fn the_footer_shows_a_lint_finding_when_there_is_no_error() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 150, 2);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.lint = Some("line 2: [mini] parse error");
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("✗ line 2: [mini] parse error"), "{text}");
        assert!(!text.contains("ready"), "the status yields to the finding");
        assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.warn);
    }

    /// At every width from 30 to 200 columns a chip's hit rect is empty
    /// exactly when none of it was painted, and a painted chip stops short
    /// of the status text's end.
    #[test]
    fn a_chip_that_lost_the_squeeze_keeps_no_clickable_rect() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.device = Some("Scarlett 2i2");
        chrome.midi_ports = midi_ports(2, 1);
        let mut squeezed = 0;
        for width in 30..=200u16 {
            let area = Rect::new(0, 20, width, 2);
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, 22));
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let hits = footer_hits(area, Some("Scarlett 2i2"), None, midi_ports(2, 1), 0, &[]);
            let rows = (area.y..area.bottom())
                .map(|y| {
                    (area.x..area.right())
                        .filter_map(|x| buffer.cell((x, y)))
                        .map(|cell| cell.symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            let painted = |text: &str| rows.iter().any(|row| row.contains(text));
            for (rect, text) in [(hits.device_chip, "Scarlett"), (hits.midi_chip, "2 out")] {
                if rect.is_empty() {
                    assert!(!painted(text), "width {width}: {text} painted with no rect");
                    squeezed += 1;
                } else {
                    assert!(
                        painted(text),
                        "width {width}: {text} has a rect but no paint"
                    );
                    assert!(
                        rect.right() <= hits.status.right(),
                        "width {width}: {text} chip runs past the status text into the dock"
                    );
                }
            }
        }
        assert!(squeezed > 0, "no width squeezed a chip out");
    }

    #[test]
    fn piano_footer_pulses_only_its_label_and_preserves_errors_and_controls() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "PIANO  C4 + E4 + G4";
        chrome.lint = Some("an unrelated lint finding");
        chrome.caret = Some((42, 17));
        chrome.error = Some("evaluation failed");
        chrome.device = Some("Speakers");
        chrome.midi_ports = midi_ports(1, 1);
        let orbits = [OrbitLevel {
            orbit: 1,
            peak: 0.4,
            pair: 0,
        }];
        chrome.orbits = &orbits;
        let area = Rect::new(0, 20, 150, 3);
        let frame = Rect::new(0, 0, 150, 23);
        let content = footer_content_area(area, 1, false, true);
        let hits = footer_hits(content, chrome.device, None, chrome.midi_ports, 0, &orbits);
        let mut normal = Buffer::empty(frame);
        Footer { chrome: &chrome }.render(area, &mut normal);

        let mut phases = Vec::new();
        for pulse in [0.0, 0.5, 1.0] {
            chrome.piano = Some(pulse);
            let mut buffer = Buffer::empty(frame);
            buffer.set_string(1, area.y - 1, "score stays visible", Style::default());
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let row = |y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            };
            let status = row(content.y);
            assert!(status.contains("PIANO  C4 + E4 + G4"), "{status}");
            assert!(!status.contains("lint finding"));
            assert!(
                !status.contains("42,17"),
                "piano notes get the ruler's room"
            );
            assert!(row(area.y).contains("evaluation failed"));
            assert!(row(area.y - 1).contains("score stays visible"));
            let label = buffer.cell((content.x + 1, content.y)).unwrap();
            assert!(label.modifier.contains(Modifier::BOLD));
            assert_eq!(label.fg, theme.surface);
            assert_eq!(label.bg, mix(theme.surface, theme.accent, pulse));
            for rect in [
                hits.dock,
                hits.scope,
                hits.device_chip,
                hits.midi_chip,
                hits.orbits[0].1,
            ] {
                for y in rect.y..rect.bottom() {
                    for x in rect.x..rect.right() {
                        assert_eq!(
                            buffer.cell((x, y)),
                            normal.cell((x, y)),
                            "control at {x},{y}"
                        );
                    }
                }
            }
            phases.push(buffer);
        }
        // The chord stays readable and stationary throughout the software pulse.
        for x in content.x + 6..hits.status.right() {
            assert_eq!(
                phases[0].cell((x, content.y)),
                phases[1].cell((x, content.y))
            );
            assert_eq!(
                phases[0].cell((x, content.y)),
                phases[2].cell((x, content.y))
            );
        }
    }

    #[test]
    fn piano_footer_keeps_its_label_when_narrow_and_errors_when_too_short() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "PIANO  C4 + E4 + G4";
        chrome.lint = Some("lint must not hide piano mode");
        chrome.caret = Some((999, 999));
        for width in 0..80 {
            for height in 0..=3 {
                let area = Rect::new(3, 1, width, height);
                let mut buffer = Buffer::empty(Rect::new(0, 0, area.right(), area.bottom()));
                chrome.piano = Some(f32::from(width % 2));
                Footer { chrome: &chrome }.render(area, &mut buffer);
                let hits = footer_hits(area, None, None, midi_ports(0, 0), 0, &[]);
                for (offset, character) in "PIANO"
                    .chars()
                    .take(usize::from(hits.status.width))
                    .enumerate()
                {
                    assert_eq!(
                        buffer
                            .cell((hits.status.x + offset as u16, hits.status.y))
                            .unwrap()
                            .symbol(),
                        character.to_string(),
                        "{width}x{height} keeps the mode label"
                    );
                }
            }
        }
        chrome.error = Some("evaluation failed");
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("evaluation failed"), "{text}");
        assert!(
            !text.contains("PIANO"),
            "the only row must retain its error"
        );
    }

    #[test]
    fn retained_piano_footer_fits_short_terminals_and_prioritizes_errors() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.piano_notes = Some("Notes  C4 + E4 + G4 · [60,64,67]");
        for error in [None, Some("evaluation failed")] {
            chrome.error = error;
            for height in 1..=12 {
                let area = Rect::new(0, 0, 80, height);
                let regions = regions_with_footer(
                    area,
                    ChromeLayout::shown(false),
                    2,
                    false,
                    false,
                    None,
                    [None, None],
                    None,
                    None,
                    None,
                    false,
                    u16::from(error.is_some()),
                    true,
                );
                assert!(regions.footer.bottom() <= area.bottom());
                let mut buffer = Buffer::empty(area);
                Footer { chrome: &chrome }.render(regions.footer, &mut buffer);
                let text = text_of(&buffer);
                if error.is_some() {
                    assert!(
                        text.contains("evaluation failed"),
                        "height {height}: {text}"
                    );
                }
                if regions.footer.height >= 3 + u16::from(error.is_some()) {
                    assert!(
                        text.contains("Notes  C4 + E4 + G4"),
                        "height {height}: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn audio_warning_has_a_full_width_row_and_keeps_footer_controls() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "ready";
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → General");
        chrome.device = Some("Speakers");
        for width in [80, 100] {
            let area = Rect::new(0, 0, width, 3);
            let content = footer_content_area(area, 1, false, true);
            let hits = footer_hits(content, chrome.device, None, midi_ports(0, 0), 0, &[]);
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let row = |y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            };
            assert!(row(0).contains(chrome.audio_warning.unwrap()));
            assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.warn);
            assert!(row(1).contains("ready"));
            assert!(row(2).contains("Speakers"));
            assert_eq!(hits.status.y, 1);
            assert_eq!(hits.device_chip.y, 2);
            assert!(hits.dock.y > area.y);
        }

        let area = Rect::new(0, 0, 50, 3);
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(text_of(&buffer).contains("Try raising latency: Settings → General"));
    }

    #[test]
    fn audio_warning_remains_actionable_below_a_query_error() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.error = Some("the query exceeded its 249 ms deadline and was refused");
        chrome.status = "the producer fell behind the audio clock; resumed at the present";
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → General");
        chrome.device = Some("Speakers");
        for width in [50, 100] {
            let regions = regions_with_footer(
                Rect::new(0, 0, width, 30),
                ChromeLayout::shown(false),
                1,
                false,
                false,
                None,
                [None, None],
                None,
                None,
                None,
                false,
                2,
                false,
            );
            assert_eq!(regions.footer.height, 4);
            let area = Rect::new(0, 0, width, regions.footer.height);
            let content = footer_content_area(area, 2, false, true);
            let hits = footer_hits(content, chrome.device, None, midi_ports(0, 0), 0, &[]);
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let row = |y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            };
            assert!(row(0).contains("the query exceeded its 249 ms deadline"));
            assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.error);
            assert!(row(1).contains("raising"), "warning row: {}", row(1));
            assert!(
                row(1).contains("Settings → General"),
                "warning row: {}",
                row(1)
            );
            assert_eq!(buffer.cell((1, 1)).unwrap().fg, theme.warn);
            assert!(row(2).contains("the producer"));
            assert!(row(3).contains("Speakers"));
            assert_eq!(hits.status.y, 2);
            assert_eq!(hits.device_chip.y, 3);
            assert_eq!(hits.dock.y, 2);
        }
    }

    #[test]
    fn audio_warning_preserves_errors_piano_and_retained_notes() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "PIANO  C4 + E4 + G4";
        chrome.piano = Some(0.5);
        chrome.vim_label = Some("NORMAL");
        chrome.piano_notes = Some("Notes  C4 + E4 + G4");
        chrome.device = Some("Speakers");
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → General");
        let row = |buffer: &Buffer, y| {
            (0..100)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect::<String>()
        };
        let area = Rect::new(0, 0, 100, 4);
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains(chrome.audio_warning.unwrap()));
        assert!(text.contains("Notes  C4 + E4 + G4"));
        assert!(text.contains("PIANO  C4 + E4 + G4"));
        assert!(!text.contains("VIM"));
        assert!(row(&buffer, 0).contains("Notes  C4 + E4 + G4"));
        assert!(row(&buffer, 1).contains(chrome.audio_warning.unwrap()));
        assert!(row(&buffer, 2).contains("PIANO"));
        assert!(row(&buffer, 3).contains("Speakers"));

        let short_area = Rect::new(0, 0, 100, 3);
        let mut buffer = Buffer::empty(short_area);
        Footer { chrome: &chrome }.render(short_area, &mut buffer);
        assert!(row(&buffer, 0).contains("Notes  C4 + E4 + G4"));
        assert!(row(&buffer, 1).contains("PIANO"));
        assert!(row(&buffer, 2).contains("Speakers"));
        assert!(!text_of(&buffer).contains("Audio struggling"));

        chrome.error = Some("evaluation failed");
        let full_area = Rect::new(0, 0, 100, 5);
        let mut buffer = Buffer::empty(full_area);
        Footer { chrome: &chrome }.render(full_area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("evaluation failed"));
        assert!(text.contains(chrome.audio_warning.unwrap()));
        assert!(text.contains("Notes  C4 + E4 + G4"));
        assert!(text.contains("PIANO  C4 + E4 + G4"));
        assert!(row(&buffer, 0).contains("evaluation failed"));
        assert!(row(&buffer, 1).contains("Notes  C4 + E4 + G4"));
        assert!(row(&buffer, 2).contains(chrome.audio_warning.unwrap()));
        assert!(row(&buffer, 3).contains("PIANO"));
        assert!(row(&buffer, 4).contains("Speakers"));

        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("evaluation failed"));
        assert!(!text.contains("Audio struggling"));
        assert!(text.contains("Notes  C4 + E4 + G4"));
        assert!(text.contains("PIANO  C4 + E4 + G4"));

        for height in [1, 2] {
            let area = Rect::new(0, 0, 100, height);
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            assert!(text_of(&buffer).contains("evaluation failed"));
            chrome.error = None;
            let mut buffer = Buffer::empty(area);
            Footer { chrome: &chrome }.render(area, &mut buffer);
            assert!(text_of(&buffer).contains("PIANO"));
            chrome.error = Some("evaluation failed");
        }
    }

    /// With no row to spare above the controls, the hint takes the status line
    /// in the warning colour when no error or piano claims it, short where the
    /// full sentence would not fit.
    #[test]
    fn audio_warning_takes_the_status_line_when_no_row_is_free() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "ready";
        chrome.audio_warning =
            Some("Audio struggling · try raising audio out latency in Settings → General");
        for (width, hint) in [(100, "Audio struggling"), (50, "Try raising latency")] {
            for height in [1, 2] {
                let area = Rect::new(0, 0, width, height);
                let mut buffer = Buffer::empty(area);
                Footer { chrome: &chrome }.render(area, &mut buffer);
                let row = (0..width)
                    .map(|x| buffer.cell((x, 0)).unwrap().symbol())
                    .collect::<String>();
                assert!(row.contains(hint), "{width}x{height}: {row}");
                assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.warn);
                assert!(!text_of(&buffer).contains("ready"), "{width}x{height}");
            }
        }
    }

    #[test]
    fn vim_commands_stay_visible_beside_inline_notices() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.vim_label = Some(":32");
        chrome.caret = Some((2, 4));
        chrome.audio_warning = Some("Audio struggling");
        for show_footer in [true, false] {
            chrome.show_footer = show_footer;
            for error in [None, Some("evaluation failed")] {
                chrome.error = error;
                let area = Rect::new(0, 0, 80, 1);
                let mut buffer = Buffer::empty(area);
                Footer { chrome: &chrome }.render(area, &mut buffer);
                let text = text_of(&buffer);
                assert!(text.contains("VIM :32 · "), "{text}");
                assert!(text.contains(error.unwrap_or("Audio struggling")), "{text}");
                assert!(text.contains("2,4"), "{text}");
                assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.accent);
                assert_eq!(
                    buffer.cell((11, 0)).unwrap().fg,
                    if error.is_some() {
                        theme.error
                    } else {
                        theme.warn
                    }
                );
            }
        }
    }

    #[test]
    fn an_error_gets_a_footer_row_of_its_own() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.status = "ready";
        chrome.error = Some("evaluation · javascript: ReferenceError: ssss is not defined");
        chrome.device = Some("Speakers");

        let area = Rect::new(0, 20, 150, 3);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 23));
        Footer { chrome: &chrome }.render(area, &mut buffer);
        let row = |y| {
            (area.x..area.right())
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let error = row(area.y);
        let status = row(area.y + 1);
        let devices = row(area.y + 2);
        assert!(error.contains("ReferenceError"), "error row: {error:?}");
        assert!(
            !error.contains("ready"),
            "status leaked onto error row: {error:?}"
        );
        assert!(
            !error.contains("Speakers"),
            "devices leaked onto error row: {error:?}"
        );
        assert!(
            status.contains("ready"),
            "ordinary status stays below: {status:?}"
        );
        assert!(
            devices.contains("Speakers"),
            "devices keep their row: {devices:?}"
        );

        let layout_area = Rect::new(0, 0, 150, 30);
        let plain = regions_with(
            layout_area,
            false,
            1,
            false,
            false,
            None,
            [None, None],
            None,
            None,
        );
        let errored = regions_with_footer(
            layout_area,
            ChromeLayout::shown(false),
            1,
            false,
            false,
            None,
            [None, None],
            None,
            None,
            None,
            false,
            1,
            false,
        );
        assert_eq!(errored.footer.height, plain.footer.height + 1);
        assert_eq!(errored.footer.bottom(), plain.footer.bottom());
    }

    #[test]
    fn remote_indicator_uses_its_visible_footer_hit_area() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.show_footer = false;
        chrome.remote_control = true;
        chrome.caret = Some((2, 4));
        let area = Rect::new(0, 20, 80, 1);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 21));
        Footer { chrome: &chrome }.render(area, &mut buffer);

        let hit = remote_indicator_rect(status_footer_hits(area).status, true);
        assert_eq!(hit, Rect::new(1, 20, 2, 1));
        let text = |rect: Rect| {
            (rect.x..rect.right())
                .map(|x| buffer.cell((x, rect.y)).expect("cell").symbol())
                .collect::<String>()
        };
        assert_eq!(text(hit), "<>");
        assert_eq!(text(Rect::new(hit.x, hit.y, 3, 1)), "<> ");
        assert!(text(area).starts_with(" <> ready"));
        assert!(!text(area).contains("remote"));
        assert!(text(area).contains("ready"));
        assert!(text(area).contains("2,4"));
        assert!(remote_indicator_rect(area, false).is_empty());
        assert!(remote_indicator_rect(Rect::new(0, 20, 19, 1), true).is_empty());

        let idle = buffer.cell((hit.x, hit.y)).expect("idle indicator").style();
        assert_eq!(idle.fg, Some(theme.muted));
        chrome.remote_receiving = true;
        Footer { chrome: &chrome }.render(area, &mut buffer);
        for (offset, symbol) in ["<", ">"].into_iter().enumerate() {
            let active = buffer
                .cell((hit.x + offset as u16, hit.y))
                .expect("active indicator");
            assert_eq!(active.symbol(), symbol);
            assert_eq!(active.style().fg, Some(theme.ok));
            assert!(active.style().add_modifier.contains(Modifier::BOLD));
        }
        chrome.remote_receiving = false;
        Footer { chrome: &chrome }.render(area, &mut buffer);
        for (offset, symbol) in ["<", ">"].into_iter().enumerate() {
            let restored = buffer
                .cell((hit.x + offset as u16, hit.y))
                .expect("idle indicator");
            assert_eq!(restored.symbol(), symbol);
            assert_eq!(restored.style(), idle);
        }

        chrome.remote_control = false;
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(!text_of(&buffer).contains("<>"));

        // The icon stays at the left while orbit chips remain on the right.
        chrome.show_footer = true;
        chrome.remote_control = true;
        let orbits = [OrbitLevel {
            orbit: 1,
            peak: 0.4,
            pair: 0,
        }];
        chrome.orbits = &orbits;
        let area = Rect::new(0, 20, 150, 2);
        let hits = footer_hits(
            area,
            chrome.device,
            chrome.input,
            chrome.midi_ports,
            0,
            &orbits,
        );
        assert_eq!(hits.orbit_count, 1);
        let hit = remote_indicator_rect(hits.status, true);
        let orbit = hits.orbits[0].1;
        let mut buffer = Buffer::empty(area);
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert_eq!(hit.x, hits.status.x);
        assert_eq!(buffer.cell((hit.right(), hit.y)).unwrap().symbol(), " ");
        assert!(orbit.x >= hits.status.right());
        assert_eq!(buffer.cell((orbit.x, orbit.y)).unwrap().symbol(), "1");
    }

    #[test]
    fn footer_chips_are_hit_tested_where_they_are_drawn() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 20, 150, 2);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.device = Some("Scarlett 2i2");
        chrome.midi_ports = midi_ports(2, 1);
        Footer { chrome: &chrome }.render(area, &mut buffer);

        let hits = footer_hits(area, Some("Scarlett 2i2"), None, midi_ports(2, 1), 0, &[]);
        let read = |rect: Rect| {
            (rect.x..rect.right())
                .filter_map(|x| buffer.cell((x, rect.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(read(hits.device_chip).contains("Scarlett 2i2"));
        assert!(read(hits.midi_chip).contains("2 out · 1 in"));
        assert!(!hits.device_chip.intersects(hits.midi_chip));
        // The dock's fader and readouts are drawn where the hits say. The
        // cap is ground rather than a glyph, so it is one cell of the
        // meter row whose background nothing else on that row shares.
        let bg = |x: u16, y: u16| buffer.cell((x, y)).unwrap().style().bg;
        let row = hits.meter.y;
        assert_eq!(
            (hits.meter.x..hits.meter.right())
                .filter(|x| {
                    let here = bg(*x, row);
                    (hits.meter.x..hits.meter.right())
                        .filter(|other| bg(*other, row) == here)
                        .count()
                        == 1
                })
                .count(),
            1,
            "the fader's cap sits on the meter the hits name"
        );
        let text = text_of(&buffer);
        assert!(text.contains("MASTER"), "{text}");
        assert!(text.contains("0.0dB"), "{text}");
    }

    #[test]
    fn footer_input_level_fills_device_characters_from_left_to_right() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 20, 150, 2);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.device = Some("Speakers");
        chrome.input = Some("Mic界");
        chrome.midi_ports = midi_ports(2, 0);
        chrome.input_active = true;
        let hits = footer_hits(area, chrome.device, chrome.input, midi_ports(2, 0), 0, &[]);
        let lit_color = legible_against_floor(theme.ok, theme.surface, 3.0);
        assert_ne!(lit_color, theme.accent);
        assert_eq!(
            usize::from(hits.device_chip.width),
            UnicodeWidthStr::width(device_chip_text(chrome.device, chrome.input).as_str()),
        );

        let mut previous = 0;
        for peak_db in [-60.0, -30.0, -10.0, 0.0] {
            chrome.input_peak_db = peak_db;
            let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
            Footer { chrome: &chrome }.render(area, &mut buffer);
            let colors = (hits.device_chip.x..hits.device_chip.right())
                .map(|x| buffer.cell((x, hits.device_chip.y)).unwrap().fg)
                .collect::<Vec<_>>();
            let lit = colors
                .iter()
                .take_while(|color| **color == lit_color)
                .count();
            assert!(
                lit > previous,
                "{peak_db} dB should fill farther than the prior level"
            );
            assert!(colors[..lit].iter().all(|color| *color == lit_color));
            assert!(
                colors[lit..].iter().all(|color| *color == theme.accent),
                "{peak_db} dB, lit={lit}, colors={colors:?}, accent={:?}",
                theme.accent
            );
            assert_eq!(
                buffer
                    .cell((hits.midi_chip.x, hits.midi_chip.y))
                    .unwrap()
                    .fg,
                theme.accent,
                "MIDI keeps its own activity color",
            );
            let text = (hits.device_chip.x..hits.device_chip.right())
                .map(|x| buffer.cell((x, hits.device_chip.y)).unwrap().symbol())
                .collect::<String>();
            assert!(text.contains("Speakers · ♩ Mic界"), "{text}");
            previous = lit;
        }
        assert_eq!(previous, usize::from(hits.device_chip.width));

        chrome.input_active = false;
        chrome.input_peak_db = 0.0;
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(
            (hits.device_chip.x..hits.device_chip.right()).all(|x| buffer
                .cell((x, hits.device_chip.y))
                .unwrap()
                .fg
                == theme.accent),
            "an idle microphone leaves the label unfilled",
        );

        chrome.input = None;
        chrome.input_active = true;
        let disconnected = footer_hits(area, chrome.device, None, midi_ports(2, 0), 0, &[]);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 150, 22));
        Footer { chrome: &chrome }.render(area, &mut buffer);
        assert!(
            (disconnected.device_chip.x..disconnected.device_chip.right()).all(|x| buffer
                .cell((x, disconnected.device_chip.y))
                .unwrap()
                .fg
                == theme.accent),
            "a removed microphone cannot leave a stale fill",
        );
    }

    #[test]
    fn orbit_chips_take_the_right_of_the_status_row_and_yield_to_a_narrow_footer() {
        let orbits = [
            OrbitLevel {
                orbit: 0,
                peak: 0.9,
                pair: 0,
            },
            OrbitLevel {
                orbit: 1,
                peak: 0.1,
                pair: 1,
            },
            OrbitLevel {
                orbit: 5,
                peak: 0.0,
                pair: 0,
            },
        ];
        let area = Rect::new(0, 20, 160, 2);
        let hits = footer_hits(area, Some("Scarlett"), None, midi_ports(0, 0), 0, &orbits);
        assert_eq!(hits.orbit_count, 3);
        let plain = footer_hits(area, Some("Scarlett"), None, midi_ports(0, 0), 0, &[]);
        assert!(
            hits.status.width < plain.status.width,
            "the status yields room"
        );
        assert!(hits.orbits[2].1.right() <= plain.status.right());
        assert_eq!(hits.orbits[0].0, 0);
        assert_eq!(hits.orbits[2].0, 5);
        // Too narrow for any chip: the status keeps its third.
        let narrow = footer_hits(
            Rect::new(0, 20, 60, 2),
            None,
            None,
            midi_ports(0, 0),
            0,
            &orbits,
        );
        assert_eq!(narrow.orbit_count, 0);
        // Rendered: number, meter, pair.
        let theme = theme();
        let mut buffer = Buffer::empty(area);
        render_orbit_chip(&mut buffer, hits.orbits[0].1, &orbits[0], 2, &theme);
        render_orbit_chip(&mut buffer, hits.orbits[1].1, &orbits[1], 2, &theme);
        let text = text_of(&buffer);
        assert!(text.contains("0 ▮▮▮▮ 1/2"), "{text}");
        assert!(text.contains("1 ▮▮▯▯ 3/4"), "{text}");
    }
    /// The footer shows the caret as a vim-style ruler (`line,col`) right of
    /// the status message. A long message is cut with an ellipsis, and a
    /// status area too narrow for the ruler gives the row to the message.
    #[test]
    fn the_footer_reads_the_caret_as_a_vim_style_ruler() {
        UiSettings::default().apply();
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.caret = Some((2, 4));
        let row = |chrome: &StudioChrome, area: Rect| {
            let mut buffer = Buffer::empty(Rect::new(0, 0, area.right(), area.bottom()));
            Footer { chrome }.render(area, &mut buffer);
            (area.x..area.right())
                .filter_map(|x| buffer.cell((x, area.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };

        // A short message is unchanged, with the ruler to its right.
        let area = Rect::new(0, 20, 150, 2);
        chrome.status = "ready";
        let wide = row(&chrome, area);
        let ruler_at = wide.find("2,4").expect("the ruler is on the status row");
        let ready_at = wide.find("ready").expect("the message keeps the row");
        assert!(
            ruler_at > ready_at,
            "the ruler sits after the message: {wide}"
        );

        // A message too long for the room left of the ruler is shortened
        // to fit, ending with an ellipsis; the ruler still reads.
        let long = "compiling the whole score after the sample cache moved, \
                     which will take a little while";
        chrome.status = long;
        let crowded = row(&chrome, area);
        assert!(crowded.contains("2,4"), "the ruler still reads: {crowded}");
        assert!(
            !crowded.contains(long),
            "the long message is cut down to fit: {crowded}"
        );
        assert!(
            crowded.contains('…'),
            "the cut message ends with an ellipsis: {crowded}"
        );

        // A status area too narrow for the ruler, its gap and even an
        // ellipsis gives the row back to the message instead.
        chrome.status = "ready";
        let narrow = row(&chrome, Rect::new(0, 20, 7, 2));
        assert!(!narrow.contains("2,4"), "no room for the ruler: {narrow}");
    }
}

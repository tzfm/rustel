use ratatui::buffer::Cell;

use super::*;

/// Whether a glyph is one of the drawing rasters' - a block, a sextant, a
/// Braille cell - as opposed to text.
fn is_raster_glyph(symbol: &str) -> bool {
    symbol.chars().next().is_some_and(|glyph| {
        ('\u{2580}'..='\u{259f}').contains(&glyph)
            || ('\u{1fb00}'..='\u{1fbff}').contains(&glyph)
            || ('\u{2800}'..='\u{28ff}').contains(&glyph)
    })
}

/// Lay a painter's picture under the score: the global stage. `stage` is
/// the painter's own render of `area`; every cell it drew - a raster glyph,
/// or a background of its own - tints the frame cell's background by
/// `strength`, so the text sits on the picture and the picture sits over
/// Hydra. Chrome keeps its colour (by rect, like the backdrop), and so do
/// `untouched` cells: a sounding event's highlight is the top layer. The
/// painter's words follow [`stage_words`].
pub(crate) fn paint_stage(
    buffer: &mut Buffer,
    stage: &Buffer,
    area: Rect,
    background: Color,
    chrome: &[Rect],
    untouched: &std::collections::HashSet<(u16, u16)>,
    strength: f32,
) {
    if strength <= 0.0 {
        return;
    }
    for y in area.y..area.bottom() {
        // Only the score's own ground takes the stage: a chip or a
        // highlight painted its own colour and keeps it.
        let ground: Vec<bool> = (area.x..area.right())
            .map(|x| {
                let at = (x, y);
                !untouched.contains(&at)
                    && !chrome.iter().any(|panel| panel.contains(at.into()))
                    && buffer
                        .cell(at)
                        .is_some_and(|cell| cell.bg == background || cell.bg == Color::Reset)
            })
            .collect();
        let words = stage_words(stage, area, y, &blank_ground(buffer, area, y, &ground));
        for x in area.x..area.right() {
            let at = (x, y);
            let index = usize::from(x - area.x);
            let Some(painted) = stage.cell(at) else {
                continue;
            };
            let glyph = painted.symbol();
            let drawn = is_raster_glyph(glyph);
            let written = !drawn && glyph != " " && words[index];
            let colour = if painted.bg != background && painted.bg != Color::Reset {
                painted.bg
            } else if drawn || written {
                painted.fg
            } else {
                continue;
            };
            if !ground[index] {
                continue;
            }
            let Some(cell) = buffer.cell_mut(at) else {
                continue;
            };
            cell.set_bg(crate::theme::mix(background, colour, strength));
            // Brighter than the wash, or a word would be a smudge the
            // shape of a word.
            if written {
                cell.set_char(glyph.chars().next().unwrap_or(' '));
                cell.set_fg(crate::theme::mix(
                    background,
                    colour,
                    (strength * 2.5).clamp(0.45, 0.9),
                ));
            }
        }
    }
}

/// The cells of `ground` on row `y` that hold nothing of the score: a blank
/// symbol that no wide character to its left covers.
fn blank_ground(buffer: &Buffer, area: Rect, y: u16, ground: &[bool]) -> Vec<bool> {
    let mut covered: usize = 0;
    (area.x..area.right())
        .zip(ground)
        .map(|(x, &open)| {
            let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
            let blank = open && covered == 0 && symbol == " ";
            covered = usize::max(
                covered.saturating_sub(1),
                UnicodeWidthStr::width(symbol).saturating_sub(1),
            );
            blank
        })
        .collect()
}

/// The columns of `area` on row `y` that take the painter's words. A word
/// is a run of the painter's text, single spaces inside it included; it is
/// written only where `blank` holds for every cell under it and for the
/// cell on either side (the pane's edge counts as blank), and is left out
/// whole otherwise, so it never mixes with the score's own text.
fn stage_words(stage: &Buffer, area: Rect, y: u16, blank: &[bool]) -> Vec<bool> {
    let free = |x: u16| blank[usize::from(x - area.x)];
    let symbol = |x: u16| stage.cell((x, y)).map_or(" ", |cell| cell.symbol());
    let text = |x: u16| {
        let glyph = symbol(x);
        glyph != " " && !is_raster_glyph(glyph)
    };
    let right = area.right();
    let mut words = vec![false; usize::from(area.width)];
    let mut x = area.x;
    while x < right {
        if !text(x) {
            x += 1;
            continue;
        }
        let start = x;
        let mut end = x;
        loop {
            if end < right && text(end) {
                let width = UnicodeWidthStr::width(symbol(end)).max(1) as u16;
                end = end.saturating_add(width).min(right);
            } else if end + 1 < right && symbol(end) == " " && text(end + 1) {
                end += 1;
            } else {
                break;
            }
        }
        let fits = (start == area.x || free(start - 1))
            && (start..end).all(&free)
            && (end >= right || free(end));
        if fits {
            words[usize::from(start - area.x)..usize::from(end - area.x)].fill(true);
        }
        x = end;
    }
    words
}

pub(crate) struct Backdrop<'a> {
    pub(crate) theme: &'a Theme,
}

/// One RGBA visual frame, as a terminal that has no graphics protocol can
/// show it: the colour of each cell.
///
/// Each cell is one pixel, so the picture has the resolution of the
/// grid, for example 80 by 24. This is the only version that works in
/// every terminal, including the ones on Windows.
pub struct VisualBackdrop {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
    /// How strongly it shows through the score's own background, 0..=1. The
    /// code has to stay readable, so the caller caps this by what `editor
    /// opacity` leaves (`backdrop_strength`).
    pub strength: f32,
    /// How strongly it shows through everything the interface painted for
    /// itself - the header, the footer, the minimap, a panel, a highlight.
    /// Zero leaves the interface solid, which is the picture behind it rather
    /// than through it. The code area has `strength`, the chrome has this:
    /// two surfaces, each capped by its own opacity setting.
    pub interface: f32,
}

/// Compatibility name for callers that consumed Hydra frames before the
/// compositor also became TachyonFX's image delivery path.
#[cfg(feature = "hydra")]
pub type HydraBackdrop = VisualBackdrop;

/// The editor's selected cells, and the wash the theme affords them.
///
/// The selection is the one editor decoration whose ground the picture may
/// run through at its own strength - decided by the theme's contrast budget
/// rather than the flat interface number - and colour cannot identify it at
/// composite time (four surfaces paint `theme.selection`, and two shipped
/// themes reuse the editor's own background), so the cells are named
/// outright.
pub struct SelectionWash<'a> {
    pub cells: &'a std::collections::HashSet<(u16, u16)>,
    /// 0..=1: how much of the picture the selection takes.
    pub strength: f32,
    /// Cells the picture never touches: a sounding event's highlight is
    /// the top layer, drawn over everything, and reads as a mark only
    /// while nothing washes through it.
    pub untouched: &'a std::collections::HashSet<(u16, u16)>,
}

impl VisualBackdrop {
    /// Paint through the original single-exclusion API.
    pub fn paint(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        under: Color,
        keep: Option<Rect>,
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) {
        match keep {
            Some(keep) => self.paint_excluding(
                buffer,
                area,
                under,
                std::slice::from_ref(&keep),
                chrome,
                selection,
            ),
            None => self.paint_excluding(buffer, area, under, &[], chrome, selection),
        }
    }

    /// Colour the finished frame with the picture, leaving `keep` untouched.
    ///
    /// Runs after everything else has drawn, so every cell already holds the
    /// colour it wanted. A cell showing nothing but the theme's own background
    /// takes the picture at [`Self::strength`]; a cell that painted something
    /// for itself - the header, the minimap, a panel, a highlight - takes it
    /// over that colour at [`Self::interface`], keeping its own identity while
    /// the picture washes through it.
    ///
    /// `keep` spares a rect from the picture entirely; `chrome` declares one
    /// interface by rect, for panels whose colours a theme makes
    /// indistinguishable from the score's.
    pub fn paint_excluding(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        under: Color,
        keep: &[Rect],
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) {
        for row in 0..area.height {
            for column in 0..area.width {
                let at = (area.x + column, area.y + row);
                if keep.iter().any(|panel| panel.contains(at.into())) {
                    continue;
                }
                let Some(cell) = buffer.cell_mut(at) else {
                    continue;
                };
                if selection.is_some_and(|wash| wash.untouched.contains(&at)) {
                    continue;
                }
                let (ground, strength) =
                    if let Some(wash) = selection.filter(|wash| wash.cells.contains(&at)) {
                        (cell.bg, wash.strength)
                    } else if chrome.iter().any(|panel| panel.contains(at.into())) {
                        // A rect declared interface takes the interface wash even
                        // where its colours are indistinguishable from the
                        // score's - mono paints its panels with the editor's own
                        // background, and reset-coloured themes paint nothing.
                        // The header, footer, strip and sheets are all here,
                        // which is what keeps a reset-coloured theme's chrome
                        // from pulsing at the picture's full strength.
                        let ground = if cell.bg == Color::Reset {
                            under
                        } else {
                            cell.bg
                        };
                        (ground, self.interface)
                    } else {
                        self.ground_of(cell.bg, under)
                    };
                if strength <= 0.0 {
                    continue;
                }
                cell.set_bg(self.cell(column, row, area, ground, strength));
            }
        }
    }

    /// What a cell is composited over, and how much of the picture it takes.
    ///
    /// The score's own background is the picture's to fill. Anything else on
    /// screen was painted deliberately, so the picture goes over that colour
    /// rather than replacing it, and how far is the reader's setting.
    fn ground_of(&self, background: Color, under: Color) -> (Color, f32) {
        if background == under || background == Color::Reset {
            (under, self.strength)
        } else {
            (background, self.interface)
        }
    }

    /// The picture as an image for a terminal that draws real pixels, with
    /// the same rule the cell path follows.
    ///
    /// Two things are decided here rather than left to the terminal.
    ///
    /// The colour is composited against `under` and sent opaque. A graphics
    /// protocol does carry alpha, but a translucent image is resolved in the
    /// terminal's own colour space against whatever the terminal decides is
    /// beneath it, and no two agree; doing it here is how both delivery paths
    /// arrive at the same colour on every terminal.
    ///
    /// `frame` is the finished frame the image will be laid over, and lets the
    /// image follow the cell path exactly: every pixel is composited over the
    /// colour of the cell it lands in, at that cell's own strength. An image
    /// drawn under the text is still drawn OVER the cell backgrounds, so
    /// without this the picture covers the interface and leaves only the
    /// glyphs, which is a wall of colour rather than a backdrop.
    ///
    /// A cell taking none of the picture is cut out of the image rather than
    /// painted in its own colour. That keeps the guarantee exact: the terminal
    /// draws nothing there, so its idea of that colour never has to match
    /// ours.
    pub fn composited(
        &self,
        under: Color,
        frame: Option<(&Buffer, Rect)>,
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) -> Vec<u8> {
        self.composited_excluding(under, frame, &[], chrome, selection)
    }

    /// The image delivery path with more than one protected overlay.
    pub fn composited_excluding(
        &self,
        under: Color,
        frame: Option<(&Buffer, Rect)>,
        keep: &[Rect],
        chrome: &[Rect],
        selection: Option<&SelectionWash>,
    ) -> Vec<u8> {
        let default = crate::graphics::rgb(under);
        let mut out = Vec::with_capacity(self.rgba.len());
        let paint = |out: &mut Vec<u8>, pixel: &[u8], ground: (u8, u8, u8), strength: f32| {
            if strength <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                return;
            }
            let (red, green, blue) = over(pixel, ground, strength);
            out.extend_from_slice(&[red, green, blue, 255]);
        };
        // No frame to follow, or nothing to follow it on: one strength
        // over the theme's own colour, everywhere.
        let Some((buffer, area)) =
            frame.filter(|(_, area)| !area.is_empty() && self.width != 0 && self.height != 0)
        else {
            for pixel in self.rgba.as_chunks::<4>().0 {
                paint(&mut out, pixel, default, self.strength);
            }
            return out;
        };
        // Every cell under the picture, resolved once.
        //
        // The rule a cell falls under - kept clear, washed by a selection,
        // chrome, or plain score background - is a property of the cell, and
        // deciding it costs two hash lookups and two rect scans. A frame can
        // have a million and a half pixels for a few thousand cells, so the
        // pixel loop below only reads a cell's answer out of this table.
        let width = usize::from(area.width);
        let mut table: Vec<((u8, u8, u8), f32)> =
            Vec::with_capacity(width * usize::from(area.height));
        for row in 0..area.height {
            for column in 0..area.width {
                let at = (area.x + column, area.y + row);
                let Some(background) = buffer.cell(at).map(|cell| cell.bg) else {
                    table.push((default, self.strength));
                    continue;
                };
                let (ground, strength) = if keep.iter().any(|rect| rect.contains(at.into()))
                    || selection.is_some_and(|wash| wash.untouched.contains(&at))
                {
                    (background, 0.0)
                } else if let Some(wash) = selection.filter(|wash| wash.cells.contains(&at)) {
                    (background, wash.strength)
                } else if chrome.iter().any(|panel| panel.contains(at.into())) {
                    let ground = if background == Color::Reset {
                        under
                    } else {
                        background
                    };
                    (ground, self.interface)
                } else {
                    self.ground_of(background, under)
                };
                table.push((crate::graphics::rgb(ground), strength));
            }
        }
        // Which cell a pixel row and a pixel column land in, once each
        // rather than two divisions per pixel.
        let stride = self.width as usize * 4;
        let row_count = self.rgba.len().div_ceil(stride.max(1));
        let row_of: Vec<usize> = (0..row_count)
            .map(|y| {
                ((y as u64 * u64::from(area.height)) / u64::from(self.height)) as usize * width
            })
            .map(|base| base.min(table.len().saturating_sub(width)))
            .collect();
        let column_of: Vec<usize> = (0..self.width as usize)
            .map(|x| {
                (((x as u64 * u64::from(area.width)) / u64::from(self.width)) as usize)
                    .min(width.saturating_sub(1))
            })
            .collect();
        for (y, line) in self.rgba.chunks(stride).enumerate() {
            let base = row_of[y];
            for (x, pixel) in line.as_chunks::<4>().0.iter().enumerate() {
                let (ground, strength) = table[base + column_of[x]];
                paint(&mut out, pixel, ground, strength);
            }
        }
        out
    }

    /// The colour under one cell, dimmed toward the theme's own background so
    /// the text on top of it keeps its contrast.
    fn cell(&self, column: u16, row: u16, area: Rect, under: Color, strength: f32) -> Color {
        if self.width == 0 || self.height == 0 || area.width == 0 || area.height == 0 {
            return under;
        }
        // Nearest-neighbour, and deliberately so: this is already the coarsest
        // possible raster and averaging would only turn it grey.
        let x = u32::from(column) * u32::from(self.width) / u32::from(area.width);
        let y = u32::from(row) * u32::from(self.height) / u32::from(area.height);
        let at = ((y.min(u32::from(self.height) - 1) * u32::from(self.width))
            + x.min(u32::from(self.width) - 1)) as usize
            * 4;
        let Some(pixel) = self.rgba.get(at..at + 4) else {
            return under;
        };
        let (red, green, blue) = over(pixel, crate::graphics::rgb(under), strength);
        Color::Rgb(red, green, blue)
    }
}

/// One pixel of the picture over `under`, at `strength`.
///
/// Hydra's transforms write alpha and write it premultiplied - `luma` returns
/// `vec4(rgb * a, a)`, `mask` and `layer` the same - which is the shape a
/// WebGL canvas hands to the page it is drawn on. So the composite is the one
/// a browser would do, `picture + under * (1 - a)`, and a sketch that thins
/// itself out shows the terminal through the gaps the way it does upstream.
///
/// `strength` scales how much of the picture is let through. A sketch that
/// writes no alpha at all - most of them - reduces to a plain mix.
fn over(pixel: &[u8], under: (u8, u8, u8), strength: f32) -> (u8, u8, u8) {
    let coverage = (f32::from(pixel[3]) / 255.0) * strength;
    let mix = |picture: u8, under: u8| -> u8 {
        (f32::from(picture) * strength + f32::from(under) * (1.0 - coverage)).clamp(0.0, 255.0)
            as u8
    };
    (
        mix(pixel[0], under.0),
        mix(pixel[1], under.1),
        mix(pixel[2], under.2),
    )
}

impl Widget for Backdrop<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        buffer.set_style(
            area,
            Style::default()
                .bg(self.theme.background)
                .fg(self.theme.foreground),
        );
    }
}

/// A geometry-neutral acknowledgement that the score was evaluated. Only
/// visible, non-empty source lines flash: the terminal visualizers and the
/// unused editor background keep moving without a full-pane white blast.
/// The pulse says how the flash moves, so no clock is needed here.
pub(super) struct EvaluationFlash<'a> {
    pub(super) map: &'a ScreenMap,
    pub(super) theme: &'a Theme,
    pub(super) pulse: super::EvaluationPulse,
}

/// A subtle, full-width cue for the caret's logical line, including wrapped
/// segments. Preserve syntax, selections and bracket marks so the caret wins.
pub(super) struct CaretLineFlash<'a> {
    pub(super) map: &'a ScreenMap,
    pub(super) line: usize,
    pub(super) theme: &'a Theme,
}

/// A short Braille-dot ring that converges on the caret. Each glyph resolves
/// the curve within a terminal cell without hiding its background. Measure
/// that background and move the theme accent only as far as visibility needs.
struct CaretLocatorRing<'a> {
    center: CellPoint,
    progress: f32,
    theme: &'a Theme,
}

impl Widget for CaretLocatorRing<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        const BRAILLE_DOTS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        if !area.contains((self.center.x, self.center.y).into()) {
            return;
        }
        let radius = self.progress.clamp(0.0, 1.0) * 3.2;
        let reach_x = (radius * 2.0 + 1.0).ceil() as i32;
        let reach_y = (radius + 1.0).ceil() as i32;
        let center_x = i32::from(self.center.x);
        let center_y = i32::from(self.center.y);
        for dy in -reach_y..=reach_y {
            for dx in -reach_x..=reach_x {
                let mut dots = 0;
                for (dot_x, column) in BRAILLE_DOTS.iter().enumerate() {
                    for (dot_y, bit) in column.iter().enumerate() {
                        let x = (dx as f32 + (dot_x as f32 + 0.5) / 2.0 - 0.5) / 2.0;
                        let y = dy as f32 + (dot_y as f32 + 0.5) / 4.0 - 0.5;
                        let distance = (x.powi(2) + y.powi(2)).sqrt();
                        if (distance - radius).abs() <= 0.19 {
                            dots |= *bit;
                        }
                    }
                }
                if dots == 0 {
                    continue;
                }
                let (x, y) = (center_x + dx, center_y + dy);
                if x < i32::from(area.x)
                    || x >= i32::from(area.right())
                    || y < i32::from(area.y)
                    || y >= i32::from(area.bottom())
                {
                    continue;
                }
                if let Some(cell) = buffer.cell_mut((x as u16, y as u16)) {
                    let ground = if cell.bg == Color::Reset {
                        self.theme.background
                    } else {
                        cell.bg
                    };
                    let ring = legible_against_floor(self.theme.accent, ground, 5.5);
                    let glyph = char::from_u32(0x2800 + dots).expect("Braille dots are valid");
                    cell.set_symbol(&glyph.to_string())
                        .set_style(Style::default().fg(ring).remove_modifier(Modifier::all()));
                }
            }
        }
    }
}

/// Painted by the app after its picture, dropdown and help passes so the
/// locator stays in front of every layer that can cover the editor.
pub(crate) fn render_caret_locator(
    buffer: &mut Buffer,
    area: Rect,
    center: CellPoint,
    progress: f32,
    theme: &Theme,
) {
    CaretLocatorRing {
        center,
        progress,
        theme,
    }
    .render(area, buffer);
}

impl Widget for CaretLineFlash<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let fill = self.theme.caret_line_fill();
        for row in self.map.rows() {
            let ScreenRow::Text(row) = row else {
                continue;
            };
            if row.line == self.line && area.contains((area.x, row.screen_y).into()) {
                for x in area.x..area.right() {
                    if let Some(cell) = buffer.cell_mut((x, row.screen_y))
                        && (cell.bg == self.theme.background || cell.bg == Color::Reset)
                    {
                        if let Some(fill) = fill {
                            cell.set_bg(fill);
                        } else {
                            // Terminal-owned backgrounds have no measurable RGB;
                            // a little weight still locates the line without inversion.
                            cell.set_style(Style::default().add_modifier(Modifier::BOLD));
                        }
                    }
                }
            }
        }
    }
}

impl Widget for EvaluationFlash<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for row in self.map.rows() {
            let ScreenRow::Text(row) = row else {
                continue;
            };
            let Some(last_cell) = row.cells.last() else {
                continue;
            };
            if row.screen_y < area.y || row.screen_y >= area.bottom() {
                continue;
            }
            let end = last_cell.screen_x.end.min(area.right());
            if end <= area.x {
                continue;
            }
            let width = end - area.x;
            for x in area.x..end {
                let Some(cell) = buffer.cell_mut((x, row.screen_y)) else {
                    continue;
                };
                match self.pulse.look(row.line, x - area.x, width) {
                    FlashLook::Off => {}
                    FlashLook::Reverse => {
                        cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                    FlashLook::Wash(level) => {
                        wash(cell, self.theme, level);
                    }
                    FlashLook::Glyph(glyph) => {
                        cell.set_symbol(glyph);
                    }
                }
            }
        }
    }
}

/// Blend a cell toward the theme's opposite grounds by `level`: the text
/// drifts toward the background colour and the ground toward the text
/// colour, which at whole strength is the swap the reverse modifier makes
/// and in between is every step of a fade. Terminal-owned colours with no
/// measurable RGB start from the theme's own grounds.
fn wash(cell: &mut Cell, theme: &Theme, level: f32) {
    let (Some(front), Some(back)) = (rgb(theme.foreground), rgb(theme.background)) else {
        return;
    };
    let mix = |from: u8, toward: u8| {
        (f32::from(from) + (f32::from(toward) - f32::from(from)) * level).round() as u8
    };
    let blend = |from: Color, toward: (u8, u8, u8)| match from {
        Color::Rgb(red, green, blue) => Color::Rgb(
            mix(red, toward.0),
            mix(green, toward.1),
            mix(blue, toward.2),
        ),
        _ => Color::Rgb(toward.0, toward.1, toward.2),
    };
    cell.fg = blend(cell.fg, back);
    cell.bg = blend(cell.bg, front);
}

fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stage tints the score's ground with what a painter drew - a
    /// raster glyph's colour or a background of its own - and leaves chrome,
    /// highlights and cells with their own colour alone.
    #[test]
    fn the_stage_tints_only_the_ground_under_what_the_painter_drew() {
        let area = Rect::new(0, 0, 5, 1);
        let ground = Color::Rgb(10, 10, 20);
        let mut frame = Buffer::empty(area);
        frame.set_style(area, Style::default().bg(ground));
        frame.set_string(1, 0, "x", Style::default().fg(Color::White));
        // A chip with its own background at x = 4.
        frame.set_string(4, 0, " ", Style::default().bg(Color::Rgb(90, 90, 90)));

        let mut stage = Buffer::empty(area);
        stage.set_string(0, 0, "▀", Style::default().fg(Color::Rgb(200, 0, 0)));
        stage.set_string(1, 0, "▄", Style::default().fg(Color::Rgb(0, 200, 0)));
        stage.set_string(3, 0, "█", Style::default().fg(Color::Rgb(0, 0, 200)));
        stage.set_string(4, 0, "█", Style::default().fg(Color::Rgb(0, 0, 200)));

        let untouched = std::collections::HashSet::from([(3_u16, 0_u16)]);
        paint_stage(&mut frame, &stage, area, ground, &[], &untouched, 0.5);

        let bg = |x: u16| frame.cell((x, 0)).expect("cell").bg;
        assert_eq!(bg(0), crate::theme::mix(ground, Color::Rgb(200, 0, 0), 0.5));
        // Text keeps its glyph and takes the tint under it.
        assert_eq!(frame.cell((1, 0)).expect("cell").symbol(), "x");
        assert_eq!(bg(1), crate::theme::mix(ground, Color::Rgb(0, 200, 0), 0.5));
        assert_eq!(bg(2), ground, "ground the painter left bare stays bare");
        assert_eq!(bg(3), ground, "a highlight is the top layer");
        assert_eq!(bg(4), Color::Rgb(90, 90, 90), "a chip keeps its own colour");

        // Chrome is spared by rect, whatever its colour.
        let mut frame = Buffer::empty(area);
        frame.set_style(area, Style::default().bg(ground));
        paint_stage(
            &mut frame,
            &stage,
            area,
            ground,
            &[Rect::new(0, 0, 1, 1)],
            &untouched,
            0.5,
        );
        assert_eq!(frame.cell((0, 0)).expect("cell").bg, ground);
    }

    /// A painter's word on the stage is written whole where the score is blank
    /// under it and on either side of it, and not at all elsewhere: none of its
    /// letters or its wash lands among the score's text.
    #[test]
    fn a_stage_word_is_written_whole_in_blank_ground_or_not_at_all() {
        let area = Rect::new(0, 0, 32, 4);
        let ground = Color::Rgb(10, 10, 20);
        let mut frame = Buffer::empty(area);
        frame.set_style(area, Style::default().bg(ground));
        let code = Style::default().fg(Color::White);
        frame.set_string(0, 0, "// await samples('github:x')", code);
        frame.set_string(0, 2, "ab   cd", code);
        // The drum covers cells 3 and 4; cell 4 holds a blank symbol.
        frame.set_string(0, 3, "// 🥁", code);
        let score = frame.clone();

        let blue = Color::Rgb(0, 0, 200);
        let word = Style::default().fg(blue);
        let mut stage = Buffer::empty(area);
        stage.set_string(0, 0, "waiting for audio", word);
        stage.set_string(2, 1, "▸ bd", word);
        // Exactly the width of the gap, so it would touch the code on both sides.
        stage.set_string(2, 2, "xyz", word);
        // One cell, with a blank on either side.
        stage.set_string(20, 2, "q", word);
        // Against the drum's right half, then clear of it.
        stage.set_string(5, 3, "q", word);
        stage.set_string(8, 3, "r", word);

        paint_stage(
            &mut frame,
            &stage,
            area,
            ground,
            &[],
            &std::collections::HashSet::new(),
            0.5,
        );
        let row = |buffer: &Buffer, y: u16| -> String {
            (area.x..area.right())
                .map(|x| buffer.cell((x, y)).expect("cell").symbol())
                .collect()
        };
        let bg = |x: u16, y: u16| frame.cell((x, y)).expect("cell").bg;
        assert_eq!(
            row(&frame, 0),
            row(&score, 0),
            "no letter lands between the code's words"
        );
        assert!(
            (area.x..area.right()).all(|x| bg(x, 0) == ground),
            "a word left out leaves no wash"
        );
        assert_eq!(
            row(&frame, 1).trim_end(),
            "  ▸ bd",
            "a blank row takes it whole"
        );
        assert_eq!(bg(4, 1), crate::theme::mix(ground, blue, 0.5));
        assert_eq!(
            row(&frame, 2).trim_end(),
            "ab   cd             q",
            "a word flush against the code is left out; a spaced one stays"
        );
        assert_eq!(bg(3, 2), ground);
        assert_eq!(
            row(&frame, 3).trim_end(),
            "// 🥁    r",
            "a wide character's second cell is the score's, not a margin"
        );
    }

    #[test]
    fn evaluation_flash_changes_style_without_changing_any_cell() {
        let mut editor = Editor::new("$: note(60)\n\n.scope()\n").unwrap();
        let area = Rect::new(0, 0, 24, 4);
        let grid = source_grid(&editor, area, true);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);
        buffer.set_string(4, 0, "$: note(60)", Style::default());
        buffer.set_string(4, 2, ".scope()", Style::default());
        let symbols = buffer
            .content
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect::<Vec<_>>();

        let theme = Theme::built_in_default();
        EvaluationFlash {
            map: &map,
            theme: &theme,
            pulse: super::EvaluationPulse {
                animation: crate::settings::EvalAnimation::Normal,
                progress: 0.0,
                intensity: 1.0,
            },
        }
        .render(area, &mut buffer);

        assert_eq!(
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol().to_owned())
                .collect::<Vec<_>>(),
            symbols
        );
        assert!(
            buffer
                .cell((0, 0))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED))
        );
        assert!(
            buffer
                .cell((15, 0))
                .is_some_and(|cell| !cell.modifier.contains(Modifier::REVERSED)),
            "cells after the visible source stay untouched"
        );
        assert!(
            buffer
                .cell((14, 0))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED)),
            "the final visible source cell flashes"
        );
        assert!(
            buffer
                .cell((0, 1))
                .is_some_and(|cell| !cell.modifier.contains(Modifier::REVERSED)),
            "empty source rows stay untouched"
        );
        assert!(
            buffer
                .cell((0, 2))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED)),
            "every visible non-empty source row flashes"
        );
    }

    #[test]
    fn caret_locator_flashes_the_full_logical_line_including_wrapped_rows() {
        use crate::editor::{ByteOffset, Selection};

        let source = "first\nabcdefghijklmnopqrstuvwxyz\nlast";
        let mut editor = Editor::new(source).unwrap();
        editor.set_wrap(true);
        editor
            .set_selection(Selection::caret(ByteOffset(source.find('a').unwrap())))
            .unwrap();
        let area = Rect::new(0, 0, 12, 6);
        let grid = source_grid(&editor, area, false);
        editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
        let map = editor.screen_map(grid).unwrap();
        let mut buffer = Buffer::empty(area);

        let theme = Theme::built_in_default();
        let fill = theme.caret_line_fill().unwrap();
        buffer.set_style(area, Style::default().fg(theme.syntax.punctuation));
        let caret_row = map
            .rows()
            .iter()
            .find_map(|row| match row {
                ScreenRow::Text(row) if row.line == 1 => Some(row.screen_y),
                _ => None,
            })
            .unwrap();
        let marked = (area.x + 1, caret_row);
        buffer[marked]
            .set_bg(Color::Blue)
            .set_style(Style::default().add_modifier(Modifier::UNDERLINED));

        CaretLineFlash {
            map: &map,
            line: 1,
            theme: &theme,
        }
        .render(area, &mut buffer);

        let mut wrapped_rows = 0;
        for row in map.rows() {
            let ScreenRow::Text(row) = row else {
                continue;
            };
            if row.line == 1 {
                wrapped_rows += 1;
            }
            for x in area.x..area.right() {
                let cell = &buffer[(x, row.screen_y)];
                assert_eq!(cell.fg, theme.syntax.punctuation);
                assert!(!cell.modifier.contains(Modifier::REVERSED));
                if (x, row.screen_y) == marked {
                    assert_eq!(cell.bg, Color::Blue, "preserve bracket/selection fill");
                    assert!(cell.modifier.contains(Modifier::UNDERLINED));
                } else {
                    assert_eq!(cell.bg, if row.line == 1 { fill } else { Color::Reset });
                }
            }
        }
        assert!(wrapped_rows > 1, "the fixture must exercise wrapping");
    }

    #[test]
    fn caret_locator_ring_contracts_and_stays_visible_on_light_and_dark_cells() {
        let area = Rect::new(0, 0, 25, 13);
        let center = CellPoint::new(12, 6);
        for ground in [Color::Rgb(12, 16, 25), Color::Rgb(245, 244, 239)] {
            let mut theme = Theme::built_in_default();
            theme.background = ground;
            theme.accent = Color::Rgb(120, 120, 120);
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(ground));
            buffer[(18, 6)]
                .set_symbol("X")
                .set_bg(Color::Rgb(100, 60, 90));

            CaretLocatorRing {
                center,
                progress: 1.0,
                theme: &theme,
            }
            .render(area, &mut buffer);
            assert_eq!(buffer[(12, 6)].symbol(), " ", "ring starts clear of caret");
            assert_eq!(buffer[(18, 6)].bg, Color::Rgb(100, 60, 90));
            assert!(
                crate::theme::contrast_ratio(buffer[(18, 6)].fg, Color::Rgb(100, 60, 90)) >= 5.5,
                "ring clears a differently colored selection cell without filling it"
            );
            assert!(
                ('\u{2801}'..='\u{28ff}')
                    .contains(&buffer[(18, 6)].symbol().chars().next().unwrap_or(' ')),
                "Braille ring covers a later layer"
            );

            CaretLocatorRing {
                center,
                progress: 0.0,
                theme: &theme,
            }
            .render(area, &mut buffer);
            assert!(
                crate::theme::contrast_ratio(buffer[(12, 6)].fg, ground) >= 5.5,
                "ring reaches the caret"
            );
            assert_eq!(buffer[(12, 6)].bg, ground, "ring leaves the fill intact");
            assert!(
                ('\u{2801}'..='\u{28ff}')
                    .contains(&buffer[(12, 6)].symbol().chars().next().unwrap_or(' ')),
                "Braille ring reaches the caret"
            );
        }
    }
    #[cfg(test)]
    mod visual_backdrop_tests {
        use super::*;

        /// A picture at a given strength, with the interface left solid - the
        /// case most of these tests are about.
        fn picture(pixels: &[[u8; 3]], width: u16, height: u16, strength: f32) -> VisualBackdrop {
            let mut rgba = Vec::with_capacity(pixels.len() * 4);
            for pixel in pixels {
                rgba.extend_from_slice(pixel);
                rgba.push(255);
            }
            VisualBackdrop {
                width,
                height,
                rgba,
                strength,
                interface: 0.0,
            }
        }

        /// Every cell takes its colour from the picture, scaled to the grid.
        #[test]
        fn the_picture_becomes_the_colour_of_every_cell() {
            let theme = Theme::built_in_default();
            // Two pixels: red on the left, green on the right.
            let hydra = picture(&[[255, 0, 0], [0, 255, 0]], 2, 1, 1.0);
            let area = Rect::new(0, 0, 4, 2);
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);

            for row in 0..2 {
                assert_eq!(
                    buffer.cell((0, row)).unwrap().bg,
                    Color::Rgb(255, 0, 0),
                    "the left half is the left pixel"
                );
                assert_eq!(
                    buffer.cell((3, row)).unwrap().bg,
                    Color::Rgb(0, 255, 0),
                    "the right half is the right pixel"
                );
            }
        }

        /// At half strength the picture is mixed into the theme's own background,
        /// which is what keeps the code on top of it readable.
        #[test]
        fn strength_mixes_the_picture_into_the_theme() {
            let theme = Theme::built_in_default();
            let (base_r, _, _) = crate::graphics::rgb(theme.background);
            let hydra = picture(&[[255, 255, 255]], 1, 1, 0.5);
            let area = Rect::new(0, 0, 1, 1);
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
            let Color::Rgb(red, _, _) = buffer.cell((0, 0)).unwrap().bg else {
                panic!("a mixed background is an explicit colour");
            };
            let expected = (f32::from(base_r) * 0.5 + 255.0 * 0.5) as u8;
            assert_eq!(red, expected);
        }

        /// A selected cell takes the picture over its own colour, at the wash
        /// the caller says - on both delivery paths identically.
        #[test]
        fn a_named_selection_cell_takes_the_picture_at_its_own_wash() {
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 2, 1);
            let hydra = VisualBackdrop {
                width: 2,
                height: 1,
                rgba: vec![255; 8],
                strength: 1.0,
                interface: 0.0,
            };
            let selection_colour = Color::Rgb(40, 0, 80);
            let mut cells = std::collections::HashSet::new();
            cells.insert((1u16, 0u16));
            let untouched = std::collections::HashSet::new();
            let wash = SelectionWash {
                cells: &cells,
                strength: 0.5,
                untouched: &untouched,
            };

            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            buffer.cell_mut((1, 0)).unwrap().set_bg(selection_colour);
            let image = hydra.composited_excluding(
                theme.background,
                Some((&buffer, area)),
                &[],
                &[],
                Some(&wash),
            );
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], Some(&wash));

            // The named cell: half the picture over its own colour, 40 + 215/2.
            let expected = Color::Rgb(147, 127, 167);
            assert_eq!(buffer.cell((1, 0)).unwrap().bg, expected);
            assert_eq!(&image[4..8], &[147, 127, 167, 255]);
            // An unnamed interface cell would have been cut out entirely at
            // interface 0 - the wash names its cells, colour never does.
            assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Rgb(255, 255, 255));
        }

        /// The theme decides how much a selection can take, and it is the
        /// theme's contrast that decides it.
        #[test]
        fn the_selection_wash_budget_follows_the_themes_contrast() {
            use crate::theme::selection_wash;
            let generous = Theme::built_in_default();
            let budget = selection_wash(&generous);
            assert!((0.05..=1.0).contains(&budget), "a real budget: {budget}");

            // A theme whose selection barely clears its text affords less than
            // one with headroom.
            let mut tight = generous.clone();
            tight.selection = Color::Rgb(90, 90, 90);
            tight.selection_text = Color::Rgb(190, 190, 190);
            assert!(
                selection_wash(&tight) < budget,
                "{} < {budget}",
                selection_wash(&tight)
            );

            // Colours the theme hands to the terminal cannot be measured, and
            // take the conservative fixed budget rather than a made-up one.
            let mut reset = generous.clone();
            reset.selection = Color::Reset;
            assert_eq!(selection_wash(&reset), 0.2);
        }

        /// Hydra's `luma`, `mask` and `layer` transforms produce premultiplied alpha.
        /// Both delivery paths preserve transparency so the theme shows through.
        #[test]
        fn the_pictures_own_alpha_lets_the_theme_through() {
            let theme = Theme::built_in_default();
            let under = crate::graphics::rgb(theme.background);
            // Two pixels, both premultiplied: one solid white, one fully absent.
            // An absent pixel carries no colour of its own - that is what
            // premultiplied means - so the cell must come out as the theme's.
            let hydra = VisualBackdrop {
                width: 2,
                height: 1,
                rgba: vec![255, 255, 255, 255, 0, 0, 0, 0],
                strength: 1.0,
                interface: 0.0,
            };
            let area = Rect::new(0, 0, 2, 1);
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
            assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Rgb(255, 255, 255));
            assert_eq!(
                buffer.cell((1, 0)).unwrap().bg,
                Color::Rgb(under.0, under.1, under.2),
                "a transparent pixel is the theme, not black"
            );
        }

        /// Cell backgrounds and terminal images use the same compositing rules.
        /// Both delivery paths must produce the same colour.
        #[test]
        fn both_delivery_paths_composite_to_the_same_colour() {
            let theme = Theme::built_in_default();
            for alpha in [0_u8, 64, 128, 255] {
                for opacity in [0.05_f32, 0.55, 1.0] {
                    // Premultiplied, so a half-covered pixel carries half its
                    // colour, exactly as hydra's own shaders emit it.
                    let level = (200.0 * f32::from(alpha) / 255.0) as u8;
                    let hydra = VisualBackdrop {
                        width: 1,
                        height: 1,
                        rgba: vec![level, level, level, alpha],
                        strength: opacity,
                        interface: 0.0,
                    };
                    let area = Rect::new(0, 0, 1, 1);
                    let mut buffer = Buffer::empty(area);
                    Backdrop { theme: &theme }.render(area, &mut buffer);
                    hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
                    let cells = buffer.cell((0, 0)).unwrap().bg;

                    let image = hydra.composited_excluding(theme.background, None, &[], &[], None);
                    assert_eq!(image.len(), 4, "one pixel in, one pixel out");
                    assert_eq!(image[3], 255, "the image goes out opaque");
                    assert_eq!(
                        cells,
                        Color::Rgb(image[0], image[1], image[2]),
                        "alpha {alpha} at opacity {opacity}"
                    );
                }
            }
            // At zero the picture is not drawn at all rather than drawn as the
            // background, which is what turning the setting down to 0% has to
            // mean. The two paths still agree: one leaves the cell alone, the
            // other leaves a hole for it to show through.
            let hydra = VisualBackdrop {
                width: 1,
                height: 1,
                rgba: vec![255, 255, 255, 255],
                strength: 0.0,
                interface: 0.0,
            };
            let area = Rect::new(0, 0, 1, 1);
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);
            assert_eq!(
                buffer.cell((0, 0)).unwrap().bg,
                theme.background,
                "the cell is left as the theme painted it"
            );
            assert_eq!(
                hydra.composited_excluding(theme.background, None, &[], &[], None),
                vec![0, 0, 0, 0]
            );
        }

        /// A reset-coloured chrome inside a declared rect takes the interface
        /// wash, not the picture's full strength, so reset-coloured themes
        /// do not pulse with the sketch across the whole frame.
        #[test]
        fn a_reset_chrome_rect_takes_the_interface_wash_not_the_full_picture() {
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 2, 1);
            let hydra = VisualBackdrop {
                width: 2,
                height: 1,
                rgba: vec![255; 8],
                strength: 1.0,
                interface: 0.0,
            };
            let mut buffer = Buffer::empty(area);
            // Both cells use `reset`; only the left one is declared chrome.
            for x in 0..2 {
                buffer.cell_mut((x, 0)).unwrap().set_bg(Color::Reset);
            }
            let header = Rect::new(0, 0, 1, 1);
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[header], None);
            assert_eq!(
                buffer.cell((0, 0)).unwrap().bg,
                Color::Reset,
                "chrome at a solid interface keeps its colour"
            );
            assert_ne!(
                buffer.cell((1, 0)).unwrap().bg,
                Color::Reset,
                "the ground outside the rect still takes the picture"
            );
        }

        /// With the interface solid, it is cut out of the image rather than
        /// painted over.
        ///
        /// A terminal draws an image under the glyphs but still over the cell
        /// backgrounds, so an image covering the grid hides every panel, border
        /// and highlight and leaves the glyphs floating on it. Cutting those cells
        /// out is what gives the picture back its place behind the interface
        /// rather than in front of it.
        #[test]
        fn a_solid_interface_is_cut_out_of_the_image() {
            let theme = Theme::built_in_default();
            // Two cells wide, one deep, two picture pixels per cell.
            let area = Rect::new(0, 0, 2, 1);
            let hydra = VisualBackdrop {
                width: 4,
                height: 1,
                rgba: vec![255; 16],
                strength: 1.0,
                interface: 0.0,
            };
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            // The right-hand cell claims a colour, the way a highlight does.
            buffer.cell_mut((1, 0)).unwrap().set_bg(Color::Rgb(9, 9, 9));

            let image =
                hydra.composited_excluding(theme.background, Some((&buffer, area)), &[], &[], None);
            assert_eq!(image.len(), 16, "four pixels out");
            assert_eq!(
                &image[0..8],
                &[255, 255, 255, 255, 255, 255, 255, 255],
                "the cell that kept the theme takes the picture"
            );
            assert_eq!(
                &image[8..16],
                &[0, 0, 0, 0, 0, 0, 0, 0],
                "the cell that chose a colour is a hole, so its colour survives"
            );

            // With no frame to consult, nothing is cut out.
            let whole = hydra.composited_excluding(theme.background, None, &[], &[], None);
            assert!(whole.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
        }

        #[test]
        fn a_kept_preview_is_untouched_in_cell_and_pixel_delivery() {
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 2, 1);
            let keep = Rect::new(1, 0, 1, 1);
            let picture = VisualBackdrop {
                width: 2,
                height: 1,
                rgba: vec![255; 8],
                strength: 1.0,
                interface: 1.0,
            };
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            let before = buffer.cell((1, 0)).unwrap().clone();
            let image = picture.composited_excluding(
                theme.background,
                Some((&buffer, area)),
                &[keep],
                &[],
                None,
            );

            picture.paint_excluding(&mut buffer, area, theme.background, &[keep], &[], None);

            assert_eq!(buffer.cell((1, 0)), Some(&before));
            assert_eq!(&image[4..8], &[0, 0, 0, 0]);
            assert_eq!(&image[0..4], &[255, 255, 255, 255]);
        }

        /// A translucent interface blends the picture over each cell's own colour.
        /// Cell and image delivery must produce the same result.
        #[test]
        fn a_translucent_interface_takes_the_picture_over_its_own_colour() {
            let theme = Theme::built_in_default();
            let chrome = Color::Rgb(40, 0, 80);
            let area = Rect::new(0, 0, 2, 1);
            // Two cells, two picture pixels each, the picture solid white.
            let hydra = VisualBackdrop {
                width: 4,
                height: 1,
                rgba: vec![255; 16],
                strength: 1.0,
                interface: 0.5,
            };
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            buffer.cell_mut((1, 0)).unwrap().set_bg(chrome);

            let image =
                hydra.composited_excluding(theme.background, Some((&buffer, area)), &[], &[], None);
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);

            // The score's own background takes all of the picture.
            assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Rgb(255, 255, 255));
            assert_eq!(&image[0..4], &[255, 255, 255, 255]);

            // The interface cell takes half of it, over its own colour rather
            // than over the theme's - 40 + (255 - 40) / 2 in the red channel.
            let expected = Color::Rgb(147, 127, 167);
            assert_eq!(buffer.cell((1, 0)).unwrap().bg, expected);
            assert_eq!(
                &image[8..12],
                &[147, 127, 167, 255],
                "and the image says the same thing"
            );
        }

        /// Explicit cell backgrounds remain unchanged. Active-line and event
        /// highlights must not change colour as the picture animates.
        #[test]
        fn a_cell_that_chose_its_colour_keeps_it() {
            let theme = Theme::built_in_default();
            let hydra = picture(&[[255, 0, 0]], 1, 1, 1.0);
            let area = Rect::new(0, 0, 2, 1);
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            // Something drew a highlight over the second cell.
            buffer.cell_mut((1, 0)).unwrap().set_bg(Color::Rgb(0, 0, 0));
            hydra.paint_excluding(&mut buffer, area, theme.background, &[], &[], None);

            assert_eq!(
                buffer.cell((0, 0)).unwrap().bg,
                Color::Rgb(255, 0, 0),
                "plain background is the picture"
            );
            // The highlight keeps its own colour exactly. The picture moves
            // every frame, so a highlight that took part of it would never
            // settle.
            assert_eq!(
                buffer.cell((1, 0)).unwrap().bg,
                Color::Rgb(0, 0, 0),
                "a chosen background survives the picture untouched"
            );
        }

        /// Without a picture, the backdrop leaves the theme unchanged.
        #[test]
        fn without_a_picture_the_backdrop_is_the_theme() {
            let theme = Theme::built_in_default();
            let area = Rect::new(0, 0, 3, 1);
            let mut buffer = Buffer::empty(area);
            Backdrop { theme: &theme }.render(area, &mut buffer);
            assert_eq!(buffer.cell((1, 0)).unwrap().bg, theme.background);
        }
    }
}

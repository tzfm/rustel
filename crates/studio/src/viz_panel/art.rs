//! The set's name as ANSI art: a five-by-seven font, the largest glyphs
//! that fit the column with the words whole, the styles that shape and
//! move them, and the palette every widget colours itself from.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::super::theme::{Theme, hsv, mix};
use super::super::visuals::Canvas;
use super::{ART_MAX_LINES, ArtAlignment, ArtStyle, Colouring, Edge, art_text_error};

/// The glyphs, five points wide and seven tall, drawn the way a text-mode
/// screen drew its own: `#` lit, `.` dark. The space is three wide.
fn glyph(character: char) -> &'static [&'static str] {
    match character {
        'A' => &[
            "..#..", ".#.#.", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'B' => &[
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
        'C' => &[
            ".####", "#....", "#....", "#....", "#....", "#....", ".####",
        ],
        'D' => &[
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ],
        'E' => &[
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
        'F' => &[
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
        'G' => &[
            ".####", "#....", "#....", "#.###", "#...#", "#...#", ".####",
        ],
        'H' => &[
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'I' => &[
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
        ],
        'J' => &[
            "..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##..",
        ],
        'K' => &[
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
        'L' => &[
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
        'M' => &[
            "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#",
        ],
        'N' => &[
            "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#",
        ],
        'O' => &[
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'P' => &[
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
        'Q' => &[
            ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#",
        ],
        'R' => &[
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
        'S' => &[
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
        'T' => &[
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
        'U' => &[
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'V' => &[
            "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
        ],
        'W' => &[
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#",
        ],
        'X' => &[
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ],
        'Y' => &[
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
        'Z' => &[
            "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####",
        ],
        '0' => &[
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
        '1' => &[
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
        '2' => &[
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
        '3' => &[
            "#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###.",
        ],
        '4' => &[
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
        '5' => &[
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
        '6' => &[
            "..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###.",
        ],
        '7' => &[
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
        '8' => &[
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
        '9' => &[
            ".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##..",
        ],
        ' ' => &["...", "...", "...", "...", "...", "...", "..."],
        '-' => &[
            ".....", ".....", ".....", "#####", ".....", ".....", ".....",
        ],
        '_' => &[
            ".....", ".....", ".....", ".....", ".....", ".....", "#####",
        ],
        '.' => &["..", "..", "..", "..", "..", "##", "##"],
        '!' => &[
            "..#..", "..#..", "..#..", "..#..", "..#..", ".....", "..#..",
        ],
        '\'' => &["..#", "..#", ".#.", "...", "...", "...", "..."],
        '&' => &[
            ".##..", "#..#.", "#..#.", ".##..", "#.#.#", "#..#.", ".##.#",
        ],
        ':' => &["..", "##", "##", "..", "##", "##", ".."],
        '+' => &[
            ".....", "..#..", "..#..", "#####", "..#..", "..#..", ".....",
        ],
        _ => &[
            ".###.", "#...#", "....#", "...#.", "..#..", ".....", "..#..",
        ],
    }
}

/// Points a glyph is wide, at one point per pixel.
fn glyph_width(character: char) -> usize {
    glyph(character)[0].len()
}

const GLYPH_ROWS: usize = 7;
/// Points between glyphs, and between lines of glyphs.
const GLYPH_GAP: usize = 1;
const LINE_GAP: usize = 2;

/// Points a line of text is wide, at one point per pixel.
fn text_width(text: &str) -> usize {
    let glyphs: usize = text.chars().map(glyph_width).sum();
    let gaps = text.chars().count().saturating_sub(1) * GLYPH_GAP;
    glyphs + gaps
}

/// The name as the font can draw it: capitals, one space at a time.
fn artable(name: &str) -> String {
    let mut text = String::new();
    for character in name.chars() {
        let character = character.to_ascii_uppercase();
        if character == ' ' && text.ends_with(' ') {
            continue;
        }
        text.push(character);
    }
    let trimmed = text.trim().to_owned();
    if trimmed.is_empty() {
        "RUSTEL".to_owned()
    } else {
        trimmed
    }
}

/// The most a pixel grows to, in points, on the rasters the art was drawn
/// for: four points is two cells across on sextants and Braille.
const MAX_SCALE: f32 = 4.0;

/// Points to a cell across on the glyph rasters the ceiling was chosen on.
const GLYPH_ACROSS: usize = 2;

/// The most a pixel grows to on a raster with `across` points to a cell.
///
/// The ceiling is really a size on the screen, not a count of points: on
/// the glyph rasters a point is half a cell, so four of them is two cells
/// and a name fills its dock. On the pixel raster a point is one screen
/// pixel, and four of them is a fifth of one cell - the whole picture
/// collapses to a line of specks. So the ceiling grows with the raster
/// and the art keeps the size it has always had.
fn max_scale(across: usize) -> f32 {
    (MAX_SCALE * across as f32 / GLYPH_ACROSS as f32).max(MAX_SCALE)
}

/// `points` pixels at `scale` points a pixel, never less than one.
fn scaled(points: usize, scale: f32) -> usize {
    (points as f32 * scale).round().max(1.0) as usize
}

/// The lines the name wraps to at `columns` points across, `scale` points
/// a pixel: whole words while they fit, and a word alone too wide either
/// cut where it must be or, with `cut_words` off, kept whole on its own
/// line - and whether any was too wide.
fn wrap(text: &str, columns: usize, scale: usize, cut_words: bool) -> (Vec<String>, bool) {
    let fits = |line: &str| text_width(line) * scale <= columns;
    let mut lines: Vec<String> = Vec::new();
    let mut too_wide = false;
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        if let Some(last) = lines.last_mut() {
            let joined = format!("{last} {word}");
            if fits(&joined) {
                *last = joined;
                continue;
            }
        }
        if fits(word) || !cut_words {
            too_wide |= !fits(word);
            lines.push(word.to_owned());
            continue;
        }
        // The word starts a line, cut where it must.
        let mut piece = String::new();
        for character in word.chars() {
            let mut longer = piece.clone();
            longer.push(character);
            if !piece.is_empty() && !fits(&longer) {
                lines.push(piece);
                piece = character.to_string();
                too_wide = true;
            } else {
                piece = longer;
            }
        }
        lines.push(piece);
    }
    (lines, too_wide)
}

/// The scale the name fills the column at, in points a pixel, and its
/// lines: the largest whole scale the words fit at on a few lines, then
/// grown to the width from there - so a name a little too wide for the
/// next scale still fills what it has. A word too wide for the column
/// even at one point a pixel is shrunk to fit while it stays legible,
/// and cut only past that.
#[cfg(test)]
fn fit(text: &str, columns: usize, style: ArtStyle) -> (f32, Vec<String>) {
    fit_in(text, columns, usize::MAX, style)
}

/// [`fit`] with the ceiling the raster asks for.
fn fit_at(text: &str, columns: usize, style: ArtStyle, ceiling: f32) -> (f32, Vec<String>) {
    fit_in_at(text, columns, usize::MAX, style, ceiling)
}

/// [`fit`] within `rows` points of height as well: the scale comes down
/// until the lines and what the style reaches past them fit, and no
/// lower than half - unless the width already brought it under half, and
/// then the height never raises it past what the width allows.
#[cfg(test)]
fn fit_in(text: &str, columns: usize, rows: usize, style: ArtStyle) -> (f32, Vec<String>) {
    fit_in_at(text, columns, rows, style, MAX_SCALE)
}

/// [`fit_in`] with the ceiling the raster asks for.
fn fit_in_at(
    text: &str,
    columns: usize,
    rows: usize,
    style: ArtStyle,
    ceiling: f32,
) -> (f32, Vec<String>) {
    let (scale, lines) = fit_width(text, columns, style, ceiling);
    if rows == usize::MAX || art_height(scale, lines.len(), style) <= rows {
        return (scale, lines);
    }
    let extra = art_height(scale, lines.len(), style)
        - lines.len() * scaled(GLYPH_ROWS, scale)
        - lines.len().saturating_sub(1) * scaled(LINE_GAP, scale);
    let glyph_points = (lines.len() * GLYPH_ROWS + lines.len().saturating_sub(1) * LINE_GAP) as f32;
    // Not `clamp(0.5, scale)`: a narrow column fits the width under half,
    // and a clamp whose floor sits above its ceiling panics.
    let shrunk = (rows.saturating_sub(extra) as f32 / glyph_points)
        .max(0.5)
        .min(scale);
    (shrunk, lines)
}

fn fit_width(text: &str, columns: usize, style: ArtStyle, ceiling: f32) -> (f32, Vec<String>) {
    let columns = columns.saturating_sub(style_margin(style));
    let widest = |lines: &[String]| {
        lines
            .iter()
            .map(|line| text_width(line))
            .max()
            .unwrap_or(1)
            .max(1)
    };
    for scale in (1..=ceiling.max(1.0) as usize).rev() {
        let (lines, too_wide) = wrap(text, columns, scale, false);
        if !too_wide && lines.len() <= 3 {
            let grown = (columns as f32 / widest(&lines) as f32).min(ceiling);
            return (
                scale_that_fits(&lines, columns, grown.max(scale as f32)),
                lines,
            );
        }
    }
    let (lines, _) = wrap(text, columns, 1, false);
    let shrunk = columns as f32 / widest(&lines) as f32;
    if shrunk >= 0.5 && lines.len() <= 3 {
        return (scale_that_fits(&lines, columns, shrunk), lines);
    }
    // Too wide even so: cut over up to three lines at a width that
    // shrinks to half at worst, so a long name still reads.
    let (lines, _) = wrap(text, columns * 2, 1, true);
    if lines.len() <= 3 {
        let shrunk = (columns as f32 / widest(&lines) as f32).min(1.0);
        return (scale_that_fits(&lines, columns, shrunk), lines);
    }
    (1.0, wrap(text, columns, 1, true).0)
}

/// Below this a line is drawn at one point a glyph and cannot get any
/// narrower, so there is nothing left to give up.
const MIN_FIT_SCALE: f32 = 0.25;

/// How wide a line really draws at `scale`, in points: the same sum
/// `line_pixels` makes, rounding each glyph and each gap on its own and
/// keeping every one of them at least a point.
fn line_points(line: &str, scale: f32) -> usize {
    let gap = if scale >= 1.0 {
        scaled(GLYPH_GAP, scale)
    } else {
        GLYPH_GAP
    };
    let glyphs = line.chars().count();
    (line
        .chars()
        .map(|character| scaled(glyph_width(character), scale))
        .sum::<usize>()
        + gap * glyphs.saturating_sub(1))
    .max(1)
}

/// The largest scale at or below `scale` whose drawing fits `columns`.
///
/// `text_width` times a scale is what the budget above is struck against,
/// but rounding each glyph separately - and holding the gaps a full point
/// wide once the scale drops under one - draws wider than that ideal. Left
/// alone, the picture overruns its column, `x0` collapses to zero, and the
/// right-hand points of every row are dropped. The drawn width never grows
/// as the scale falls, so stepping down settles.
fn scale_that_fits(lines: &[String], columns: usize, scale: f32) -> f32 {
    let mut scale = scale;
    while scale > MIN_FIT_SCALE
        && lines
            .iter()
            .map(|line| line_points(line, scale))
            .max()
            .unwrap_or(1)
            > columns
    {
        scale -= 0.01;
    }
    scale
}

/// Points a style reaches past the glyphs: the glow's halo.
fn style_margin(style: ArtStyle) -> usize {
    match style {
        ArtStyle::Glow => 2,
        _ => 0,
    }
}

/// Rows the art takes at `width` cells, on the terminal's own raster.
pub fn art_rows(set_name: &str, style: ArtStyle, width: u16) -> u16 {
    if art_text_error(set_name).is_some() {
        return 1;
    }
    if set_name.contains('\n') {
        return set_name.lines().count().clamp(1, ART_MAX_LINES) as u16;
    }
    if width < 4 {
        return 1;
    }
    let (across, down) = Canvas::bars(Rect::new(0, 0, width, 1))
        .raster()
        .points_per_cell();
    let columns = usize::from(width) * across;
    let text = artable(set_name);
    let (scale, lines) = fit_at(&text, columns, style, max_scale(across));
    let points = art_height(scale, lines.len(), style);
    points.div_ceil(down.max(1)).clamp(1, ART_MAX_LINES) as u16
}

/// The whole points a style moves or steps by at `scale`.
fn unit(scale: f32) -> usize {
    scale.ceil().max(1.0) as usize
}

/// Points tall the picture is: the lines, the gaps between them, and what
/// the style reaches below or above them.
fn art_height(scale: f32, lines: usize, style: ArtStyle) -> usize {
    let extra = match style {
        ArtStyle::Glow => 2,
        ArtStyle::Wobble => 2 * unit(scale),
        _ => 0,
    };
    lines * scaled(GLYPH_ROWS, scale) + lines.saturating_sub(1) * scaled(LINE_GAP, scale) + extra
}

/// A glyph at one point a pixel: its width, and its pixels row by row.
fn glyph_bitmap(character: char) -> (usize, Vec<bool>) {
    let rows = glyph(character);
    let width = glyph_width(character);
    let bits = rows
        .iter()
        .flat_map(|row| row.chars().map(|cell| cell == '#'))
        .collect();
    (width, bits)
}

/// A line of the font drawn `scale` points a pixel: its width, and its
/// pixels row by row. Grown, the gaps between glyphs grow with them;
/// shrunk, each glyph is resampled on its own and the gaps kept a point
/// wide, so the letters stay apart.
fn line_pixels(line: &str, scale: f32, height: usize) -> (usize, Vec<bool>) {
    let glyphs: Vec<(usize, Vec<bool>)> = line.chars().map(glyph_bitmap).collect();
    let gap = if scale >= 1.0 {
        scaled(GLYPH_GAP, scale)
    } else {
        GLYPH_GAP
    };
    let widths: Vec<usize> = glyphs
        .iter()
        .map(|(width, _)| scaled(*width, scale))
        .collect();
    let width = (widths.iter().sum::<usize>() + gap * glyphs.len().saturating_sub(1)).max(1);
    let mut bits = vec![false; width * height];
    let mut x = 0;
    for ((source_width, source), target_width) in glyphs.iter().zip(&widths) {
        let drawn = resample(source, *source_width, *target_width, height);
        for (index, on) in drawn.iter().enumerate() {
            if *on {
                bits[(index / target_width) * width + x + index % target_width] = true;
            }
        }
        x += target_width + gap;
    }
    (width, bits)
}

/// The source pixels a target pixel stands for when `source` pixels are
/// drawn as `target`: one when growing, its whole footprint when
/// shrinking, so a stroke thickens rather than drops out.
fn span(at: usize, target: usize, source: usize) -> std::ops::Range<usize> {
    let from = at * source / target;
    let to = if target >= source {
        from + 1
    } else {
        ((at + 1) * source).div_ceil(target).max(from + 1)
    };
    from..to.min(source)
}

/// A line's pixels resampled to `width` by `height` points.
fn resample(bits: &[bool], source_width: usize, width: usize, height: usize) -> Vec<bool> {
    let mut out = vec![false; width * height];
    for ty in 0..height {
        let rows = span(ty, height, GLYPH_ROWS);
        for tx in 0..width {
            let columns = span(tx, width, source_width);
            let lit = rows
                .clone()
                .any(|sy| columns.clone().any(|sx| bits[sy * source_width + sx]));
            out[ty * width + tx] = lit;
        }
    }
    out
}

/// What colours the art: where a point is, how big the picture is, how
/// long the panel has been open, and how loud the mix is.
pub(super) struct Palette<'a> {
    pub(super) colour: Colouring,
    pub(super) theme: &'a Theme,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) seconds: f32,
    pub(super) level: f32,
    pub(super) scale: usize,
}

impl Palette<'_> {
    pub(super) fn at(&self, x: usize, y: usize) -> Color {
        let theme = self.theme;
        let across = (x as f32 / self.width.max(1) as f32).clamp(0.0, 1.0);
        let down = (y as f32 / self.height.max(1) as f32).clamp(0.0, 1.0);
        match self.colour {
            Colouring::Mono => theme.foreground,
            Colouring::Theme => mix(theme.accent, theme.selection, down),
            Colouring::Acid => {
                let glyph = x / (6 * self.scale.max(1));
                let band = y / (3 * self.scale.max(1));
                ACID[(glyph + band) % ACID.len()]
            }
            Colouring::Rainbow => hsv(across * 360.0, 0.85, 1.0),
            Colouring::Fire => {
                let ember = Color::Rgb(90, 10, 0);
                let red = Color::Rgb(230, 40, 20);
                let yellow = Color::Rgb(255, 220, 60);
                if down < 0.5 {
                    mix(yellow, red, down * 2.0)
                } else {
                    mix(red, ember, (down - 0.5) * 2.0)
                }
            }
            Colouring::Wave => hsv(((across + self.seconds * 0.25) % 1.0) * 360.0, 0.85, 1.0),
            Colouring::Pulse => {
                // Dim between hits, lit on them, and every hit turns the
                // palette a step.
                let glyph = x / (6 * self.scale.max(1));
                let band = y / (3 * self.scale.max(1));
                let turn = (self.seconds * 0.5 + self.level * 3.0) as usize;
                let base = ACID[(glyph + band + turn) % ACID.len()];
                let lit = 0.25 + 0.75 * self.level.clamp(0.0, 1.0);
                mix(theme.background, base, lit)
            }
            Colouring::Neon => {
                let glyph = x / (6 * self.scale.max(1));
                let tube = if glyph.is_multiple_of(2) {
                    Color::Rgb(255, 40, 200)
                } else {
                    Color::Rgb(40, 230, 255)
                };
                // A tube's flicker: mostly on, now and then a dip.
                let flicker = hash_unit(x as u64 / 3, (self.seconds * 12.0) as u64);
                let lit = if flicker > 0.93 {
                    0.55
                } else {
                    0.9 + 0.1 * (self.seconds * 6.0).sin()
                };
                mix(theme.background, tube, lit)
            }
            Colouring::Chrome => {
                let band = (down * 6.0).fract();
                let light = Color::Rgb(235, 240, 250);
                let dark = Color::Rgb(70, 90, 120);
                let steel = if band < 0.5 {
                    mix(dark, light, band * 2.0)
                } else {
                    mix(light, dark, (band - 0.5) * 2.0)
                };
                mix(steel, Color::Rgb(120, 200, 255), 0.15)
            }
            Colouring::Gold => {
                let top = Color::Rgb(255, 235, 140);
                let bottom = Color::Rgb(170, 110, 20);
                mix(top, bottom, down.powf(0.8))
            }
        }
    }
}

/// A steady pseudo-random 0..1 for a point and a moment.
fn hash_unit(a: u64, b: u64) -> f32 {
    let mut h = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 31;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 29;
    (h % 10_000) as f32 / 10_000.0
}

/// An ANSI pack's palette: cyan, white, magenta, red, yellow, orange.
const ACID: [Color; 6] = [
    Color::Rgb(0, 255, 255),
    Color::Rgb(255, 255, 255),
    Color::Rgb(255, 0, 255),
    Color::Rgb(255, 60, 60),
    Color::Rgb(255, 255, 0),
    Color::Rgb(255, 140, 0),
];

/// The set's name as ANSI art, filling `area`.
pub struct ArtView<'a> {
    pub set_name: &'a str,
    pub style: ArtStyle,
    pub colour: Colouring,
    pub alignment: ArtAlignment,
    pub edge: Edge,
    pub theme: &'a Theme,
    pub seconds: f32,
    /// The mix's level, 0..1.
    pub level: f32,
}

impl ArtView<'_> {
    /// Multiline text is already a drawing: keep its line breaks and
    /// indentation instead of passing it through the banner font.
    fn render_literal(&self, area: Rect, buffer: &mut Buffer) {
        let lines: Vec<String> = self
            .set_name
            .lines()
            .map(|line| {
                line.replace('\t', "    ")
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .collect()
            })
            .collect();
        let width = lines.iter().map(|line| line.width()).max().unwrap_or(0);
        let horizontal = if self.edge.is_column() {
            ArtAlignment::Center
        } else {
            self.alignment
        };
        let vertical = if self.edge.is_column() {
            self.alignment
        } else {
            ArtAlignment::Start
        };
        let left = horizontal.offset(usize::from(area.width), width);
        let top = vertical.offset(usize::from(area.height), lines.len());
        let palette = Palette {
            colour: self.colour,
            theme: self.theme,
            width: width.max(1),
            height: lines.len().max(1),
            seconds: self.seconds,
            level: self.level,
            scale: 1,
        };
        for (row, line) in lines
            .iter()
            .take(usize::from(area.height).saturating_sub(top))
            .enumerate()
        {
            let mut column = 0;
            for grapheme in line.graphemes(true) {
                let cells = grapheme.width();
                if left + column + cells > usize::from(area.width) {
                    break;
                }
                if cells > 0 {
                    buffer.set_stringn(
                        area.x + (left + column) as u16,
                        area.y + (top + row) as u16,
                        grapheme,
                        cells,
                        Style::default()
                            .fg(palette.at(column, row))
                            .bg(self.theme.background),
                    );
                }
                column += cells;
            }
        }
    }
}

impl Widget for ArtView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        // Preferences from earlier versions may contain arbitrarily long
        // artwork. Refuse it visibly without allocating a full-height canvas.
        if art_text_error(self.set_name).is_some() {
            buffer.set_stringn(
                area.x,
                area.y,
                "artwork too large · Enter to edit",
                usize::from(area.width),
                Style::default()
                    .fg(self.theme.muted)
                    .bg(self.theme.background),
            );
            return;
        }
        if self.set_name.contains('\n') {
            self.render_literal(area, buffer);
            return;
        }
        if area.width < 4 || area.height == 0 {
            return;
        }
        let mut canvas = Canvas::bars(area);
        let (columns, rows) = (canvas.width(), canvas.height());
        let across = canvas.raster().points_per_cell().0;
        let text = artable(self.set_name);
        let (scale, lines) = fit_in_at(&text, columns, rows, self.style, max_scale(across));
        let unit = unit(scale);
        let height = art_height(scale, lines.len(), self.style);
        let vertical_offset = if self.edge.is_column() {
            self.alignment.offset(rows, height)
        } else {
            0
        };
        // Every lit pixel, at its point: each line drawn at one point a
        // pixel, then grown or shrunk to the width it was given. The
        // moving styles keep the room they move in above the glyphs.
        let mut pixels: Vec<(usize, usize)> = Vec::new();
        let mut lit = vec![false; columns * rows.max(1)];
        let top = match self.style {
            ArtStyle::Wobble => unit,
            ArtStyle::Glow => 1,
            _ => 0,
        };
        let line_height = scaled(GLYPH_ROWS, scale);
        let mut y0 = vertical_offset + top;
        for line in &lines {
            let (line_width, drawn) = line_pixels(line, scale, line_height);
            let horizontal = if self.edge.is_column() {
                ArtAlignment::Center
            } else {
                self.alignment
            };
            let margin = style_margin(self.style) / 2;
            let x0 = margin + horizontal.offset(columns.saturating_sub(margin * 2), line_width);
            for (index, on) in drawn.iter().enumerate() {
                if !on {
                    continue;
                }
                let point_x = x0 + index % line_width;
                let mut y = y0 + index / line_width;
                if self.style == ArtStyle::Wobble {
                    let wave = ((point_x as f32) * 0.35 + self.seconds * 3.0).sin();
                    let lift = ((wave + 1.0) * unit as f32 * 0.5).round() as usize;
                    y = y.saturating_sub(lift);
                }
                if point_x < columns && y < rows {
                    pixels.push((point_x, y));
                    lit[y * columns + point_x] = true;
                }
            }
            y0 += line_height + scaled(LINE_GAP, scale);
        }
        let palette = Palette {
            colour: self.colour,
            theme: self.theme,
            width: columns,
            height: height.max(1),
            seconds: self.seconds,
            level: self.level,
            scale: unit,
        };
        let is_lit = |x: usize, y: usize| x < columns && y < rows && lit[y * columns + x];
        let background = self.theme.background;
        // The glow's halo, one point out, breathing with the clock and lit
        // by the mix; it goes down first, so the glyphs paint over it.
        if self.style == ArtStyle::Glow {
            let breath = 0.3 + 0.2 * (self.seconds * 2.5).sin() + 0.3 * self.level.clamp(0.0, 1.0);
            for &(x, y) in &pixels {
                let halo = mix(
                    background,
                    palette.at(x, y.saturating_sub(vertical_offset)),
                    breath.clamp(0.1, 0.8),
                );
                for (dx, dy) in [(1usize, 0usize), (0, 1)] {
                    let (hx, hy) = (x + dx, y + dy);
                    if !is_lit(hx, hy) {
                        canvas.set(hx, hy, halo);
                    }
                }
                if x > 0 && !is_lit(x - 1, y) {
                    canvas.set(x - 1, y, halo);
                }
                if y > 0 && !is_lit(x, y - 1) {
                    canvas.set(x, y - 1, halo);
                }
            }
        }
        let beam_x = ((self.seconds * 0.6).fract() * (columns as f32 + 8.0)) as usize;
        for &(x, y) in &pixels {
            let mut colour = palette.at(x, y.saturating_sub(vertical_offset));
            // The beam lights what it crosses, four points wide, and the
            // rest sits back a little so the sweep reads.
            if self.style == ArtStyle::Beam {
                let distance = (x as isize - beam_x as isize).unsigned_abs();
                colour = if distance < 2 {
                    mix(colour, Color::Rgb(255, 255, 255), 0.85)
                } else if distance < 5 {
                    mix(colour, Color::Rgb(255, 255, 255), 0.35)
                } else {
                    mix(background, colour, 0.75)
                };
            }
            canvas.set(x, y, colour);
        }
        canvas.paint(buffer, self.theme.background);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    /// The art is drawn in raster points, and how big a point is depends
    /// on the raster: half a cell on sextants, one screen pixel on the
    /// pixel tier. A ceiling fixed at four points is two cells on one and
    /// a fifth of a cell on the other, which collapsed the whole name to a
    /// line of specks. The ceiling follows the raster, so the picture
    /// keeps the size it has on every tier.
    #[test]
    fn the_art_keeps_its_size_on_the_pixel_raster() {
        assert_eq!(
            max_scale(2),
            MAX_SCALE,
            "sextants are what it was drawn for"
        );
        assert_eq!(max_scale(1), MAX_SCALE, "and half blocks never shrink it");
        assert_eq!(
            max_scale(9),
            18.0,
            "a nine-pixel cell wants nine times more"
        );

        let _renderer = crate::graphics::HeldRenderer::pixels((9, 18));
        crate::graphics::set_tier(crate::graphics::Tier::Fine);
        let glyphs = art_rows("LIVE SET", ArtStyle::Solid, 30);
        crate::graphics::set_tier(crate::graphics::Tier::Pixels);
        let pixels = art_rows("LIVE SET", ArtStyle::Solid, 30);
        // Not to the row: a glyph and a gap are rounded to whole points,
        // and a point is a different size on the two rasters. Within a
        // quarter is the same picture; a fixed ceiling gave one row.
        assert!(
            pixels * 4 > glyphs * 3 && glyphs * 4 > pixels * 3,
            "the name took {glyphs} rows of glyphs and {pixels} of pixels"
        );
    }

    /// The painter rounds each glyph and each gap, so a line can draw wider
    /// than its budgeted scale. Every line must still fit its column.
    #[test]
    fn the_picture_fits_the_column_it_was_budgeted_for() {
        for columns in [34usize, 68, 74, 100, 118] {
            for name in [
                "RUSTEL",
                "LIVE",
                "OPENING NIGHT",
                "ABCDEFGHIJKLMNOP",
                "WWWWWWWWWWWW",
            ] {
                let (scale, lines) = fit_width(name, columns, ArtStyle::Solid, MAX_SCALE);
                for line in &lines {
                    let points = line_points(line, scale);
                    assert!(
                        points <= columns,
                        "{name:?} at {columns} columns drew {points} points at scale {scale}"
                    );
                }
            }
        }
    }

    /// Every glyph is seven rows tall and every row the same width, so a
    /// line of them rasterises without a gap or a tear.
    #[test]
    fn the_font_is_a_grid() {
        for character in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -_.!'&:+?".chars() {
            let rows = glyph(character);
            assert_eq!(rows.len(), GLYPH_ROWS, "{character:?}");
            let width = rows[0].len();
            assert!(rows.iter().all(|row| row.len() == width), "{character:?}");
            assert!(
                rows.iter()
                    .all(|row| row.chars().all(|cell| cell == '#' || cell == '.')),
                "{character:?}"
            );
        }
        assert_eq!(text_width("AB"), 11);
        assert_eq!(artable("opening  night "), "OPENING NIGHT");
        assert_eq!(artable(""), "RUSTEL");
    }

    /// The name takes the largest glyphs that fit the column on a few
    /// lines, wrapping at spaces, and a word that alone outgrows the column
    /// is cut.
    #[test]
    fn the_art_fits_its_column() {
        let near = |scale: f32, expected: f32| (scale - expected).abs() < 0.01;
        // "LIVE" is 23 points wide at one point a pixel; the picture grows
        // to the column from the largest whole scale its words fit at.
        let (scale, lines) = fit("LIVE SET", 34, ArtStyle::Solid);
        assert_eq!(lines, ["LIVE", "SET"], "whole words, a line each");
        assert!(near(scale, 34.0 / 23.0), "{scale}");
        let (scale, lines) = fit("LIVE", 68, ArtStyle::Solid);
        assert_eq!(lines, ["LIVE"]);
        // A little under the ideal 68/23: rounding each glyph and each gap
        // on its own draws 69 points at that scale, one past the column.
        assert!(line_points(&lines[0], scale) <= 68, "{scale}");
        assert!(scale < 68.0 / 23.0, "{scale}");
        assert!(near(fit("LIVE", 69, ArtStyle::Solid).0, 3.0));
        assert!(
            near(fit("AB", 200, ArtStyle::Solid).0, MAX_SCALE),
            "no larger than four"
        );
        let (scale, lines) = fit("OPENING NIGHT", 68, ArtStyle::Solid);
        assert_eq!(lines, ["OPENING", "NIGHT"]);
        assert!(near(scale, 68.0 / 41.0), "{scale}");
        // A word wider than the column shrinks to fit rather than break:
        // "TRANCE" is 35 points, one too many for 34.
        let (scale, lines) = fit("TRANCE", 34, ArtStyle::Solid);
        assert_eq!(lines, ["TRANCE"]);
        assert!(line_points(&lines[0], scale) <= 34, "{scale}");
        let (scale, lines) = fit("OPENING NIGHT", 34, ArtStyle::Solid);
        assert_eq!(lines, ["OPENING", "NIGHT"]);
        assert!(near(scale, 34.0 / 41.0), "{scale}");
        // A name too long to read whole is cut over a few lines and shrunk
        // until what is drawn is really inside the column, however small
        // that makes it: a shaved letter reads worse than a small one.
        let (scale, lines) = fit("ABCDEFGHIJKLMNOP", 34, ArtStyle::Solid);
        assert!(lines.len() <= 3 && lines.iter().all(|line| text_width(line) <= 68));
        assert!(
            lines.iter().all(|line| line_points(line, scale) <= 34),
            "{scale} {lines:?}"
        );
        let (scale, lines) = fit("KJDKJKJKDJDKJDKJDKJDKJDJD", 34, ArtStyle::Solid);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines.iter().all(|line| line_points(line, scale) <= 34),
            "{scale} {lines:?}"
        );
        let (scale, lines) = fit(
            "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ",
            34,
            ArtStyle::Solid,
        );
        assert!(
            near(scale, 1.0) && lines.len() > 3,
            "far too long: cut at one point a pixel"
        );
        assert_eq!(art_height(1.0, 2, ArtStyle::Solid), 16);
        assert_eq!(art_height(1.0, 1, ArtStyle::Glow), 9);
        assert_eq!(
            art_height(0.5, 1, ArtStyle::Solid),
            4,
            "seven points at half, rounded up"
        );
        // Given a height too, the scale comes down to it: "LIVE" would
        // fill 68 points across at nearly three, but eight rows of points
        // hold one.
        let (scale, lines) = fit_in("LIVE", 68, 8, ArtStyle::Solid);
        assert_eq!(lines, ["LIVE"]);
        assert!(near(scale, 8.0 / 7.0), "{scale}");
        assert!(art_height(scale, 1, ArtStyle::Solid) <= 8);
        assert!(
            near(fit_in("LIVE", 68, 1, ArtStyle::Solid).0, 0.5),
            "no lower than half"
        );
        // The glow keeps a margin for its halo.
        let (scale, _) = fit("LIVE", 68, ArtStyle::Glow);
        assert!(scale < 68.0 / 23.0, "{scale}");
        // Resampling: grown, a pixel becomes a block; shrunk, a stroke
        // thickens rather than dropping out, and the gaps between glyphs
        // stay a point wide.
        let (width, bits) = glyph_bitmap('I');
        assert_eq!(width, 5);
        let grown = resample(&bits, width, 10, 14);
        assert_eq!(
            grown.iter().filter(|on| **on).count(),
            bits.iter().filter(|on| **on).count() * 4
        );
        let shrunk = resample(&bits, width, 3, 4);
        assert!(
            shrunk[0..3].iter().all(|on| *on),
            "the top bar survives: {shrunk:?}"
        );
        let (width, pixels) = line_pixels("II", 0.8, 6);
        assert_eq!(width, 4 + 1 + 4);
        assert!(
            (0..6).all(|row| !pixels[row * width + 4]),
            "the gap column stays dark"
        );
        let (width, _) = line_pixels("II", 2.0, 14);
        assert_eq!(width, 10 + 2 + 10);
    }

    /// A column narrow enough to fit the name under half, and short enough
    /// that the height shrinks it too, drew nothing but a panic: the height
    /// clamped between half and a width scale already below it. Shrinking
    /// the terminal window was enough to get there.
    #[test]
    fn a_narrow_short_column_draws_without_panicking() {
        for &style in ArtStyle::ALL {
            for columns in 0..80 {
                let (widest, _) = fit("2026-09-08", columns, style);
                for rows in 0..16 {
                    let (scale, _) = fit_in("2026-09-08", columns, rows, style);
                    assert!(
                        scale <= widest,
                        "{style:?} {columns}x{rows}: {scale} past {widest}"
                    );
                }
            }
        }
        let theme = Theme::resolve(None).expect("theme");
        for &style in ArtStyle::ALL {
            for width in 0..40 {
                for height in 0..8 {
                    let area = Rect::new(0, 0, width, height);
                    let mut buffer = Buffer::empty(area);
                    ArtView {
                        set_name: "2026-09-08",
                        style,
                        colour: Colouring::Mono,
                        alignment: ArtAlignment::Center,
                        edge: Edge::Right,
                        theme: &theme,
                        seconds: 1.5,
                        level: 0.5,
                    }
                    .render(area, &mut buffer);
                }
            }
        }
    }

    /// Drawn, the art lights the points of its glyphs and nothing else, in
    /// the palette asked for; the styles change the shape without leaving
    /// the column.
    #[test]
    fn the_art_paints_glyph_points_in_its_palette() {
        let theme = Theme::resolve(None).expect("theme");
        let area = Rect::new(0, 0, 34, 6);
        for &style in ArtStyle::ALL {
            for colour in Colouring::ALL {
                let mut buffer = Buffer::empty(area);
                ArtView {
                    set_name: "abc",
                    style,
                    colour,
                    alignment: ArtAlignment::Center,
                    edge: Edge::Right,
                    theme: &theme,
                    seconds: 1.5,
                    level: 0.5,
                }
                .render(area, &mut buffer);
                let lit = area
                    .positions()
                    .filter(|position| {
                        buffer
                            .cell(*position)
                            .is_some_and(|cell| cell.symbol() != " ")
                    })
                    .count();
                assert!(lit > 10, "{style:?} {colour:?} drew {lit} cells");
            }
        }
        // Mono is the foreground and nothing else.
        let mut buffer = Buffer::empty(area);
        ArtView {
            set_name: "I",
            style: ArtStyle::Solid,
            colour: Colouring::Mono,
            alignment: ArtAlignment::Center,
            edge: Edge::Right,
            theme: &theme,
            seconds: 0.0,
            level: 0.0,
        }
        .render(area, &mut buffer);
        let colours: std::collections::HashSet<String> = area
            .positions()
            .filter_map(|position| buffer.cell(position))
            .filter(|cell| cell.symbol() != " ")
            .map(|cell| format!("{:?}", cell.fg))
            .collect();
        assert_eq!(colours.len(), 1, "{colours:?}");
        // The rows follow the picture's points on the terminal's raster.
        let (across, down) = Canvas::bars(Rect::new(0, 0, 34, 1))
            .raster()
            .points_per_cell();
        let (scale, lines) = fit("ABC", 34 * across, ArtStyle::Solid);
        let expected = art_height(scale, lines.len(), ArtStyle::Solid).div_ceil(down) as u16;
        assert_eq!(art_rows("ABC", ArtStyle::Solid, 34), expected);
        // A short name grows to the width; a long one wraps, no shorter.
        assert!(
            art_rows("OPENING NIGHT", ArtStyle::Solid, 34) >= art_rows("ABC", ArtStyle::Solid, 34)
        );
    }

    #[test]
    fn literal_art_keeps_indentation_and_aligns_on_the_actual_dock_axis() {
        let theme = Theme::resolve(None).expect("theme");
        let art = "  /\\_/\\\n ( o.o )\n  > ^ <";
        let lines: Vec<&str> = art.lines().collect();
        let width = lines.iter().map(|line| line.width()).max().unwrap();
        assert_eq!(art_rows(art, ArtStyle::Solid, 40), 3);
        // Include unusually wide columns and tall bands: the edge, not
        // the slot's aspect ratio, determines which axis J changes.
        for edge in Edge::ALL {
            for area in [Rect::new(3, 2, 40, 8), Rect::new(3, 2, 10, 20)] {
                for alignment in [ArtAlignment::Start, ArtAlignment::Center, ArtAlignment::End] {
                    let screen = Rect::new(0, 0, 48, 26);
                    let mut buffer = Buffer::empty(screen);
                    for position in screen
                        .positions()
                        .filter(|position| !area.contains(*position))
                    {
                        buffer.cell_mut(position).unwrap().set_symbol("#");
                    }
                    ArtView {
                        set_name: art,
                        style: ArtStyle::Solid,
                        colour: Colouring::Mono,
                        alignment,
                        edge,
                        theme: &theme,
                        seconds: 0.0,
                        level: 0.0,
                    }
                    .render(area, &mut buffer);
                    let x = area.x
                        + if edge.is_column() {
                            (usize::from(area.width) - width) / 2
                        } else {
                            alignment.offset(usize::from(area.width), width)
                        } as u16;
                    let y = area.y
                        + if edge.is_column() {
                            alignment.offset(usize::from(area.height), lines.len())
                        } else {
                            0
                        } as u16;
                    for (row, line) in lines.iter().enumerate() {
                        for (column, ch) in line.chars().enumerate() {
                            assert_eq!(
                                buffer[(x + column as u16, y + row as u16)].symbol(),
                                ch.to_string(),
                                "{edge:?} {alignment:?} {area:?}"
                            );
                        }
                    }
                    for position in screen
                        .positions()
                        .filter(|position| !area.contains(*position))
                    {
                        assert_eq!(
                            buffer.cell(position).unwrap().symbol(),
                            "#",
                            "art escaped its widget"
                        );
                    }
                }
            }
        }
        // A clipped drawing keeps its characters and cannot overwrite its neighbours.
        let area = Rect::new(1, 1, 3, 1);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 3));
        ArtView {
            set_name: "abcdef\nghijkl",
            style: ArtStyle::Solid,
            colour: Colouring::Mono,
            alignment: ArtAlignment::End,
            edge: Edge::Bottom,
            theme: &theme,
            seconds: 0.0,
            level: 0.0,
        }
        .render(area, &mut buffer);
        assert_eq!(
            (1..4).map(|x| buffer[(x, 1)].symbol()).collect::<String>(),
            "abc"
        );
        assert_eq!(buffer[(4, 1)].symbol(), " ");
        assert_eq!(buffer[(1, 2)].symbol(), " ");
    }

    #[test]
    fn banner_alignment_moves_along_the_dock_without_changing_the_font() {
        let theme = Theme::resolve(None).expect("theme");
        let area = Rect::new(2, 3, 40, 30);
        for edge in Edge::ALL {
            let positions: Vec<_> = [ArtAlignment::Start, ArtAlignment::Center, ArtAlignment::End]
                .into_iter()
                .map(|alignment| {
                    let mut buffer = Buffer::empty(area);
                    ArtView {
                        set_name: "I",
                        style: ArtStyle::Solid,
                        colour: Colouring::Mono,
                        alignment,
                        edge,
                        theme: &theme,
                        seconds: 0.0,
                        level: 0.0,
                    }
                    .render(area, &mut buffer);
                    let lit: Vec<_> = area
                        .positions()
                        .filter(|position| buffer.cell(*position).unwrap().symbol() != " ")
                        .collect();
                    let x = lit
                        .iter()
                        .map(|point| point.x)
                        .min()
                        .expect("banner pixels");
                    let y = lit.iter().map(|point| point.y).min().unwrap();
                    let columns = lit.iter().map(|point| point.x).max().unwrap() - x + 1;
                    (x, y, columns)
                })
                .collect();
            if edge.is_column() {
                assert!(positions[0].1 < positions[1].1 && positions[1].1 < positions[2].1);
                assert!(
                    positions
                        .iter()
                        .all(|position| position.0 == positions[0].0)
                );
            } else {
                assert!(positions[0].0 < positions[1].0 && positions[1].0 < positions[2].0);
                assert!(
                    positions
                        .iter()
                        .all(|position| position.1 == positions[0].1)
                );
            }
            assert!(
                positions
                    .iter()
                    .all(|position| position.2 == positions[0].2),
                "alignment preserves font width"
            );
        }
    }
}

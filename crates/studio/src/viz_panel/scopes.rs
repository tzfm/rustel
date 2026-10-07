//! The scopes: the mix as a trace, the analyser's bands and the stereo
//! field on a polar dial, each in its own styles, all coloured from the
//! shared palette.

use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

use super::super::theme::{Theme, mix};
use std::collections::VecDeque;

use super::super::visuals::{
    AnalyserBands, Canvas, SPECTROGRAM_BANDS, SPECTROGRAM_FLOOR_DB, SpectrogramGrid,
    SpectrogramRamp, rising_zero_crossing, spectrum_color, spectrum_level,
};
use super::art::Palette;
use super::{Colouring, ScopeStyle, SpectrumStyle, VectorStyle};

/// The master analyser: a bar per band across the width, peaks held.
/// What the scopes share: the palette, the clock and the beat.
#[derive(Clone, Copy)]
pub struct Look<'a> {
    pub theme: &'a Theme,
    pub colour: Colouring,
    pub seconds: f32,
    /// The mix's level, 0..1.
    pub level: f32,
}

impl<'a> Look<'a> {
    fn palette(self, width: usize, height: usize) -> Palette<'a> {
        Palette {
            colour: self.colour,
            theme: self.theme,
            width,
            height,
            seconds: self.seconds,
            level: self.level,
            scale: 2,
        }
    }
}

/// The mix as a trace, in one of the scope's styles.
pub struct ScopeView<'a> {
    pub audio: Option<&'a rustel_runtime::ui_analysis::UiAudioAnalysisFrame>,
    pub motion: super::Motion,
    pub style: ScopeStyle,
    pub look: Look<'a>,
}

impl Widget for ScopeView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.look.theme;
        // Nothing sounding, nothing to be a picture of: the scope is not
        // drawn at all rather than shown as a flat line, which reads as a
        // set that is playing silence.
        let samples = match self.audio {
            Some(audio) if self.motion.shows_sound() => audio.scope.as_slice(),
            _ => &[],
        };
        if samples.is_empty() {
            return;
        }
        // A few periods from a rising zero crossing, scaled so the trace
        // fills its rows whatever the level.
        let start = rising_zero_crossing(samples);
        let span = (samples.len() / 4)
            .clamp(64, 768)
            .min(samples.len().saturating_sub(start));
        let window = &samples[start..start + span];
        let peak = window
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        let scale = if peak > 0.002 {
            (0.92 / peak).min(12.0)
        } else {
            1.0
        };
        // The glow draws on the block raster too: a Braille cell holds one
        // colour, so a dim halo round a bright dot would only thicken it.
        let filled = matches!(
            self.style,
            ScopeStyle::Fill | ScopeStyle::Mirror | ScopeStyle::Bars | ScopeStyle::Glow
        );
        let mut canvas = if filled {
            Canvas::bars(area)
        } else {
            Canvas::lines(area)
        };
        let (width, height) = (canvas.width(), canvas.height());
        if width < 2 || height < 2 || window.len() < 2 {
            return;
        }
        let palette = self.look.palette(width, height);
        let middle = (height - 1) as f32 / 2.0;
        let centre = middle.round() as usize;
        let at = |x: usize| -> f32 {
            let index = x * (window.len() - 1) / (width - 1);
            (window[index] * scale).clamp(-1.0, 1.0)
        };
        let row = |value: f32| -> usize { (middle - value * middle).round().max(0.0) as usize };
        let loud = |x: usize| -> f32 {
            let from = x * window.len() / width;
            let to = ((x + 1) * window.len() / width).clamp(from + 1, window.len());
            (window[from..to]
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
                * scale)
                .clamp(0.0, 1.0)
        };
        match self.style {
            ScopeStyle::Line | ScopeStyle::Glow => {
                let halo = mix(theme.background, palette.at(width / 2, centre), 0.35);
                let mut previous: Option<(isize, isize)> = None;
                for x in 0..width {
                    let y = row(at(x));
                    if self.style == ScopeStyle::Glow {
                        // The halo first, so the line paints over it.
                        for (dx, dy) in [(0isize, -1isize), (0, 1), (-1, 0), (1, 0)] {
                            let (hx, hy) = (x as isize + dx, y as isize + dy);
                            if hx >= 0 && hy >= 0 && canvas.get(hx as usize, hy as usize).is_none()
                            {
                                canvas.set(hx as usize, hy as usize, halo);
                            }
                        }
                    }
                    let colour = palette.at(x, y);
                    let here = (x as isize, y as isize);
                    match previous {
                        Some(from) => canvas.line(from, here, colour),
                        None => canvas.set(x, y, colour),
                    }
                    previous = Some(here);
                }
            }
            ScopeStyle::Dots => {
                for x in 0..width {
                    let y = row(at(x));
                    canvas.set(x, y, palette.at(x, y));
                }
            }
            ScopeStyle::Fill => {
                for x in 0..width {
                    let y = row(at(x));
                    for py in y.min(centre)..=y.max(centre) {
                        canvas.set(x, py, palette.at(x, py));
                    }
                }
            }
            ScopeStyle::Mirror => {
                // The envelope: the loudest frame in each column's slice of
                // the window, up and down from the middle.
                for x in 0..width {
                    let reach = (loud(x) * middle).round() as usize;
                    for py in centre.saturating_sub(reach)..=(centre + reach).min(height - 1) {
                        canvas.set(x, py, palette.at(x, py));
                    }
                }
            }
            ScopeStyle::Bars => {
                for x in 0..width {
                    let lit = (loud(x) * height as f32).round() as usize;
                    for py in height.saturating_sub(lit)..height {
                        canvas.set(x, py, palette.at(x, py));
                    }
                }
            }
        }
        canvas.paint(buffer, theme.background);
    }
}

/// The analyser, in one of the spectrum's styles. It takes its bands
/// rather than the studio's visual state, so the footer, a dock or a
/// score can each hand it theirs.
pub struct SpectrumView<'a> {
    /// The bands and their held peaks; none while there is nothing to
    /// analyse.
    pub bands: Option<&'a AnalyserBands>,
    /// The frames behind this one, oldest first: the spectrogram styles'
    /// time axis.
    pub history: Option<&'a VecDeque<[f32; SPECTROGRAM_BANDS]>>,
    pub motion: super::Motion,
    pub style: SpectrumStyle,
    pub look: Look<'a>,
}

impl SpectrumView<'_> {
    /// The two spectrogram styles: time across with the newest frame at
    /// the right edge, frequency up the rows, level as colour. They are a
    /// different picture from the other five - a ground with light on it
    /// rather than shapes over a ground - and they want a raster of their
    /// own, so they are drawn here rather than among the bars.
    fn render_spectrogram(&self, area: Rect, buffer: &mut Buffer) {
        let theme = self.look.theme;
        let Some(columns) = self.history.filter(|columns| !columns.is_empty()) else {
            return;
        };
        // A ramp a frequency row, and the palette read on the frequency
        // axis for both of its own: with time across, sampling the palette
        // by position would key the colour to the moment rather than to
        // the pitch, and every hue would roll past as the picture scrolled.
        let ramps = |rows: usize| {
            let palette = self.look.palette(rows.max(1), rows.max(1));
            (0..rows)
                .map(|y| SpectrogramRamp::new(theme, theme.background, palette.at(y, y)))
                .collect::<Vec<_>>()
        };
        if self.style == SpectrumStyle::Braille {
            // Two columns of time and four rows of frequency a cell: the
            // finest picture a glyph can carry, at the price of one colour
            // for all eight dots.
            let (width, height) = (usize::from(area.width) * 2, usize::from(area.height) * 4);
            let ramps = ramps(height);
            SpectrogramGrid::new(columns, width, height, 1, SPECTROGRAM_FLOOR_DB, 0.0)
                .paint_braille(buffer, area, theme.background, |_, y, level| {
                    ramps[y.min(height - 1)].at(level)
                });
            return;
        }
        let mut canvas = SpectrogramGrid::canvas(area);
        let (width, height) = (canvas.width(), canvas.height());
        if width == 0 || height == 0 {
            return;
        }
        let ramps = ramps(height);
        SpectrogramGrid::new(columns, width, height, 1, SPECTROGRAM_FLOOR_DB, 0.0)
            .paint(&mut canvas, |_, y, level| {
                ramps[y.min(height - 1)].at(level)
            });
        canvas.paint(buffer, theme.background);
    }

    /// The five styles that draw the bands as shapes over the ground.
    fn render_bands(&self, bands: &AnalyserBands, area: Rect, buffer: &mut Buffer) {
        let theme = self.look.theme;
        let mut canvas = if matches!(
            self.style,
            SpectrumStyle::Bars | SpectrumStyle::Line | SpectrumStyle::Braille
        ) {
            Canvas::lines(area)
        } else {
            Canvas::bars(area)
        };
        let (across, _) = canvas.raster().points_per_cell();
        let (width, height) = (canvas.width(), canvas.height());
        if width == 0 || height == 0 {
            return;
        }
        let palette = self.look.palette(width, height);
        let level = |db: f32| ((db - SPECTROGRAM_FLOOR_DB) / -SPECTROGRAM_FLOOR_DB).clamp(0.0, 1.0);
        // Default bars fill every cell; mirror and matrix keep their gaps.
        let cells = usize::from(area.width);
        let colour_at = |x, y| {
            if self.look.colour == Colouring::Theme {
                spectrum_color(theme, x / across, cells)
            } else {
                palette.at(x, y)
            }
        };
        let bars = if self.style == SpectrumStyle::Bars {
            cells
        } else {
            (cells / 2).clamp(1, SPECTROGRAM_BANDS).min(cells.max(1))
        };
        let start = |bar: usize| bar * cells / bars;
        let gap = usize::from(self.style != SpectrumStyle::Bars && cells / bars >= 2);
        let loudest = |values: &[f32], bar: usize| spectrum_level(values, bar, bars);
        let span = |bar: usize| -> (usize, usize) {
            let end = if bar + 1 == bars {
                cells
            } else {
                start(bar + 1) - gap
            };
            (
                start(bar) * across,
                (end.max(start(bar) + 1) * across).min(width),
            )
        };
        let white = Color::Rgb(255, 255, 255);
        match self.style {
            SpectrumStyle::Bars => {
                for bar in 0..bars {
                    let lit = (level(loudest(&bands.levels, bar)) * height as f32).round() as usize;
                    let peak = (level(loudest(&bands.peaks, bar)) * height as f32).round() as usize;
                    let (x0, x1) = span(bar);
                    for x in x0..x1 {
                        for y in height.saturating_sub(lit)..height {
                            let colour = colour_at(x, y);
                            let shade = if y == height - lit {
                                mix(colour, white, 0.35)
                            } else {
                                colour
                            };
                            canvas.set(x, y, shade);
                        }
                        if peak > lit + 1 && peak <= height {
                            let y = height - peak;
                            canvas.set(x, y, mix(theme.background, colour_at(x, y), 0.6));
                        }
                    }
                }
            }
            SpectrumStyle::Mirror => {
                // Up from the middle in full colour, down from it in the
                // reflection's half light.
                let half = height / 2;
                for bar in 0..bars {
                    let lit = (level(loudest(&bands.levels, bar)) * half as f32).round() as usize;
                    let (x0, x1) = span(bar);
                    for x in x0..x1 {
                        for step in 0..lit {
                            let up = half.saturating_sub(step + 1);
                            let down = half + step;
                            let colour = colour_at(x, up);
                            canvas.set(x, up, colour);
                            if down < height {
                                canvas.set(x, down, mix(theme.background, colour, 0.45));
                            }
                        }
                    }
                }
            }
            SpectrumStyle::Matrix => {
                // A lamp every other point row, lit up to the level; the
                // dark ones show faintly, so the grid reads as a grid.
                let lamps = (height / 2).max(1);
                for bar in 0..bars {
                    let lit = (level(loudest(&bands.levels, bar)) * lamps as f32).round() as usize;
                    let (x0, x1) = span(bar);
                    for x in x0..x1 {
                        for lamp in 0..lamps {
                            let y = height - 1 - lamp * 2;
                            let colour = colour_at(x, y);
                            let shade = if lamp < lit {
                                colour
                            } else {
                                mix(theme.background, colour, 0.12)
                            };
                            canvas.set(x, y, shade);
                        }
                    }
                }
            }
            SpectrumStyle::Line | SpectrumStyle::Fill => {
                // The bands as a curve across the width.
                let curve: Vec<usize> = (0..width)
                    .map(|x| {
                        let lit = (level(spectrum_level(&bands.levels, x, width))
                            * (height - 1) as f32)
                            .round() as usize;
                        height - 1 - lit
                    })
                    .collect();
                let mut previous: Option<(isize, isize)> = None;
                for (x, &y) in curve.iter().enumerate() {
                    if self.style == SpectrumStyle::Fill {
                        for py in y..height {
                            canvas.set(x, py, colour_at(x, py));
                        }
                    } else {
                        let colour = colour_at(x, y);
                        let here = (x as isize, y as isize);
                        match previous {
                            Some(from) => canvas.line(from, here, colour),
                            None => canvas.set(x, y, colour),
                        }
                        previous = Some(here);
                    }
                }
            }
            // Drawn by `render_spectrogram`; `render` sends them there.
            SpectrumStyle::Waterfall | SpectrumStyle::Braille => {}
        }
        canvas.paint(buffer, theme.background);
    }
}

impl Widget for SpectrumView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        // As with the scope: silent, it is not there.
        let Some(bands) = self.bands.filter(|_| self.motion.shows_sound()) else {
            return;
        };
        match self.style {
            SpectrumStyle::Waterfall | SpectrumStyle::Braille => {
                self.render_spectrogram(area, buffer);
            }
            _ => self.render_bands(bands, area, buffer),
        }
    }
}

/// The stereo field on a polar dial, in one of the vectorscope's styles.
///
/// The dial is laid out in cell units - a cell a unit wide and two tall -
/// so the ring is round on every raster, and every style is drawn side
/// across and mid up: a mono mix stands as a line, a wide one lies flat,
/// and one side alone leans its way.
pub struct VectorView<'a> {
    /// The mix's newest frames as left and right, oldest first.
    pub sides: &'a [(f32, f32)],
    pub motion: super::Motion,
    pub style: VectorStyle,
    pub look: Look<'a>,
    /// What the style keeps between frames: the petals' reach, the
    /// sweep's trace.
    pub memory: &'a mut Vec<f32>,
}

impl Widget for VectorView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_2, TAU};
        if area.width < 4 || area.height < 2 {
            return;
        }
        if !self.motion.shows_sound() {
            // The dial belongs to the sound on it: with none, no dial.
            return;
        }
        let theme = self.look.theme;
        let frames: &[(f32, f32)] = self.sides;
        let filled = matches!(self.style, VectorStyle::Petals | VectorStyle::Sweep);
        let mut canvas = if filled {
            Canvas::bars(area)
        } else {
            Canvas::lines(area)
        };
        let (across, down) = canvas.raster().points_per_cell();
        let (width, height) = (canvas.width(), canvas.height());
        if width < 4 || height < 4 {
            return;
        }
        let palette = self.look.palette(width, height);
        let units_wide = f32::from(area.width);
        let units_tall = f32::from(area.height) * 2.0;
        let radius = (units_wide / 2.0).min(units_tall / 2.0) - 0.5;
        let centre = (units_wide / 2.0, units_tall / 2.0);
        // A place on the dial, in units from its centre, as a point.
        let point = |u: f32, v: f32| -> (isize, isize) {
            (
                ((centre.0 + u) * across as f32).round() as isize,
                ((centre.1 - v) * down as f32 / 2.0).round() as isize,
            )
        };
        let colour_at = |palette: &Palette, (x, y): (isize, isize)| -> Color {
            palette.at(x.max(0) as usize, y.max(0) as usize)
        };
        let origin = point(0.0, 0.0);
        // The ring and the centre, faint.
        let faint = mix(theme.background, theme.rule, 0.8);
        let mut previous = None;
        for step in 0..=96 {
            let angle = step as f32 / 96.0 * TAU;
            let here = point(radius * angle.cos(), radius * angle.sin());
            if let Some(from) = previous {
                canvas.line(from, here, faint);
            }
            previous = Some(here);
        }
        canvas.set(origin.0.max(0) as usize, origin.1.max(0) as usize, faint);
        // The field scaled to the dial, up to a limit, so a quiet mix
        // still draws and a mono full-scale one just reaches the ring.
        let peak = frames.iter().fold(0.0f32, |peak, &(left, right)| {
            peak.max(left.abs()).max(right.abs())
        });
        let gain = if peak > 0.002 {
            (0.65 / peak).min(6.0)
        } else {
            1.0
        };
        let polar = |&(left, right): &(f32, f32)| -> (f32, f32) {
            let mid = (left + right) * FRAC_1_SQRT_2 * gain;
            let side = (right - left) * FRAC_1_SQRT_2 * gain;
            (side, mid)
        };
        match self.style {
            VectorStyle::Polar => {
                for frame in frames {
                    let (u, v) = polar(frame);
                    let here = point(u * radius, v * radius);
                    if here.0 >= 0 && here.1 >= 0 {
                        canvas.set(here.0 as usize, here.1 as usize, colour_at(&palette, here));
                    }
                }
            }
            VectorStyle::Lissajous => {
                let mut previous = None;
                for &(left, right) in frames {
                    let here = point(left * gain * radius, right * gain * radius);
                    let colour = colour_at(&palette, here);
                    match previous {
                        Some(from) => canvas.line(from, here, colour),
                        None if here.0 >= 0 && here.1 >= 0 => {
                            canvas.set(here.0 as usize, here.1 as usize, colour);
                        }
                        None => {}
                    }
                    previous = Some(here);
                }
            }
            VectorStyle::Trails => {
                let count = frames.len().max(1) as f32;
                let mut previous = None;
                for (index, frame) in frames.iter().enumerate() {
                    let (u, v) = polar(frame);
                    let here = point(u * radius, v * radius);
                    let age = index as f32 / count;
                    let colour = mix(
                        theme.background,
                        colour_at(&palette, here),
                        0.15 + 0.85 * age * age,
                    );
                    if let Some(from) = previous {
                        canvas.line(from, here, colour);
                    }
                    previous = Some(here);
                }
            }
            VectorStyle::Petals => {
                const SECTORS: usize = 48;
                self.memory.resize(SECTORS, 0.0);
                // Every sector takes this frame's loudest, or eases back.
                let mut reach = [0.0f32; SECTORS];
                for frame in frames {
                    let (u, v) = polar(frame);
                    let magnitude = (u * u + v * v).sqrt().min(1.0);
                    let sector = ((v.atan2(u).rem_euclid(TAU) / TAU) * SECTORS as f32) as usize;
                    let sector = sector.min(SECTORS - 1);
                    reach[sector] = reach[sector].max(magnitude);
                }
                if self.motion.shows_sound() {
                    for (held, now) in self.memory.iter_mut().zip(reach) {
                        *held = now.max(*held * 0.82);
                    }
                }
                // Filled wedges: lines from the centre to the rim across
                // every sector.
                for (sector, held) in self.memory.iter().enumerate() {
                    let reach = held * radius;
                    if reach < 0.5 {
                        continue;
                    }
                    let from = sector as f32 / SECTORS as f32 * TAU;
                    let to = (sector + 1) as f32 / SECTORS as f32 * TAU;
                    for step in 0..=4 {
                        let angle = from + (to - from) * step as f32 / 4.0;
                        let tip = point(reach * angle.cos(), reach * angle.sin());
                        canvas.line(origin, tip, colour_at(&palette, tip));
                    }
                }
            }
            VectorStyle::Sweep => {
                const SPOKES: usize = 72;
                self.memory.resize(SPOKES, 0.0);
                // The hand turns once every four seconds, clockwise from
                // the top; the spoke it passes takes the level, and the
                // trace fades behind it.
                let turn = (self.look.seconds * 0.25).fract();
                let hand = ((turn * SPOKES as f32) as usize).min(SPOKES - 1);
                let loud = frames
                    .iter()
                    .fold(0.0f32, |peak, frame| {
                        let (u, v) = polar(frame);
                        peak.max((u * u + v * v).sqrt())
                    })
                    .min(1.0);
                if !frames.is_empty() && self.motion.shows_sound() {
                    self.memory[hand] = loud;
                }
                for (spoke, held) in self.memory.iter().enumerate() {
                    let behind = (hand + SPOKES - spoke) % SPOKES;
                    let fade = 1.0 - behind as f32 / SPOKES as f32;
                    let angle = FRAC_PI_2 - spoke as f32 / SPOKES as f32 * TAU;
                    let reach = held * radius;
                    if reach >= 0.5 {
                        let tip = point(reach * angle.cos(), reach * angle.sin());
                        let colour = mix(
                            theme.background,
                            colour_at(&palette, tip),
                            0.15 + 0.85 * fade * fade,
                        );
                        canvas.line(origin, tip, colour);
                    }
                    if spoke == hand {
                        let tip = point(radius * angle.cos(), radius * angle.sin());
                        let colour = mix(colour_at(&palette, tip), Color::Rgb(255, 255, 255), 0.5);
                        canvas.line(origin, tip, colour);
                    }
                }
            }
        }
        canvas.paint(buffer, theme.background);
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::visuals::VisualState;
    use super::*;
    use ratatui::buffer::Buffer;
    use rustel_runtime::ui_analysis::UI_SPECTRUM_BINS;

    /// The cells a vectorscope lit in the palette's own colour, apart from
    /// the faint ring: with a mono palette, the foreground.
    fn vector_cells(
        frames: &[(f32, f32)],
        style: VectorStyle,
        memory: &mut Vec<f32>,
    ) -> Vec<(u16, u16)> {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 34, 14);
        let mut buffer = Buffer::empty(area);
        VectorView {
            sides: frames,
            motion: super::super::Motion::Live,
            style,
            look: Look {
                theme: &theme,
                colour: Colouring::Mono,
                seconds: 1.0,
                level: 0.5,
            },
            memory,
        }
        .render(area, &mut buffer);
        let mut lit = Vec::new();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = buffer.cell((x, y)).unwrap();
                if cell.fg == theme.foreground && cell.symbol() != " " {
                    lit.push((x, y));
                }
            }
        }
        lit
    }

    /// Silent, a widget that shows the mix is not drawn at all: not its
    /// trace, not its empty furniture. A picture over silence reads as a
    /// set that is playing.
    #[test]
    fn a_silent_widget_draws_nothing_at_all() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 34, 14);
        let frames: Vec<(f32, f32)> = (0..512)
            .map(|index| {
                let sample = (index as f32 * 0.2).sin() * 0.5;
                (sample, -sample)
            })
            .collect();
        let draw = |motion: super::super::Motion, memory: &mut Vec<f32>| {
            let mut buffer = Buffer::empty(area);
            VectorView {
                sides: &frames,
                motion,
                style: VectorStyle::Sweep,
                look: Look {
                    theme: &theme,
                    colour: Colouring::Mono,
                    seconds: 1.0,
                    level: 0.5,
                },
                memory,
            }
            .render(area, &mut buffer);
            buffer
        };
        let mut memory = Vec::new();
        let sounding = draw(super::super::Motion::Live, &mut memory);
        let silent = draw(super::super::Motion::Off, &mut memory);
        let empty = Buffer::empty(area);
        assert_ne!(sounding, empty, "a sounding widget drew nothing");
        assert_eq!(silent, empty, "a silent widget drew something");
    }

    /// The art is a title rather than a picture of the mix, and the mixer
    /// a control surface, so they are there whatever the set is doing;
    /// everything else goes with the sound.
    #[test]
    fn only_the_art_the_spacer_and_the_mixer_are_always_there() {
        use super::super::WidgetKind;
        for kind in WidgetKind::ALL {
            let always = matches!(
                kind,
                WidgetKind::Art | WidgetKind::Spacer | WidgetKind::Mixer
            );
            assert_eq!(kind.always_visible(), always, "{kind:?}");
        }
    }

    /// The dial is polar: a mix panned to the sides lies across it, a
    /// mono one stands up it; the styles that ease keep their memory.
    #[test]
    fn the_vectorscope_lays_side_across_and_mid_up() {
        let wide: Vec<(f32, f32)> = (0..512)
            .map(|index| {
                let sample = (index as f32 * 0.2).sin() * 0.5;
                (sample, -sample)
            })
            .collect();
        let mono: Vec<(f32, f32)> = wide.iter().map(|&(left, _)| (left, left)).collect();
        let span = |cells: &[(u16, u16)]| {
            let xs = cells.iter().map(|&(x, _)| x);
            let ys = cells.iter().map(|&(_, y)| y);
            (
                xs.clone().max().unwrap_or(0) - xs.min().unwrap_or(0),
                ys.clone().max().unwrap_or(0) - ys.min().unwrap_or(0),
            )
        };
        let mut memory = Vec::new();
        let across = vector_cells(&wide, VectorStyle::Polar, &mut memory);
        assert!(!across.is_empty(), "a wide mix draws");
        let (wide_x, wide_y) = span(&across);
        assert!(
            wide_x >= 10 && wide_y <= 1,
            "wide lies flat: {wide_x}×{wide_y}"
        );
        let up = vector_cells(&mono, VectorStyle::Polar, &mut memory);
        let (mono_x, mono_y) = span(&up);
        assert!(
            mono_x <= 1 && mono_y >= 4,
            "mono stands up: {mono_x}×{mono_y}"
        );
        assert!(memory.is_empty(), "the points keep nothing between frames");
        assert!(
            vector_cells(&mono, VectorStyle::Trails, &mut memory).is_empty(),
            "a trail has no history to fade on its first frame"
        );
        let petals = vector_cells(&wide, VectorStyle::Petals, &mut memory);
        assert_eq!(memory.len(), 48, "a reach per sector");
        assert!(
            memory.iter().any(|reach| *reach > 0.3),
            "the sides reach out"
        );
        assert!(!petals.is_empty());
        let mut memory = Vec::new();
        vector_cells(&mono, VectorStyle::Sweep, &mut memory);
        assert_eq!(memory.len(), 72, "a level per spoke");
        assert!(
            memory.iter().any(|held| *held > 0.0),
            "the hand wrote the level"
        );
        assert!(
            vector_cells(&[], VectorStyle::Sweep, &mut memory).is_empty(),
            "a stopped sweep draws nothing"
        );
    }

    /// Every scope and spectrum style draws something from a signal, in
    /// the palette's colour, and nothing but the placeholder from silence.
    #[test]
    fn every_scope_and_spectrum_style_draws() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 34, 8);
        let look = Look {
            theme: &theme,
            colour: Colouring::Mono,
            seconds: 0.0,
            level: 0.5,
        };
        let mut frame = rustel_runtime::ui_analysis::UiAudioAnalysisFrame {
            scope: [0.0; rustel_runtime::ui_analysis::UI_SCOPE_SAMPLES],
            spectrum: [-20.0; rustel_runtime::ui_analysis::UI_SPECTRUM_BINS],
        };
        for (index, sample) in frame.scope.iter_mut().enumerate() {
            *sample = (index as f32 * 0.1).sin() * 0.4;
        }
        let lit = |buffer: &Buffer| {
            (0..area.height)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let cell = buffer.cell((x, y)).unwrap();
                    cell.fg == theme.foreground && cell.symbol() != " "
                })
                .count()
        };
        for style in ScopeStyle::ALL {
            let mut buffer = Buffer::empty(area);
            ScopeView {
                audio: Some(&frame),
                motion: super::super::Motion::Live,
                style: *style,
                look,
            }
            .render(area, &mut buffer);
            assert!(lit(&buffer) > 8, "{} drew {}", style.name(), lit(&buffer));
            // Silent, every style draws nothing at all: a picture of the
            // mix with no mix is not a picture of anything.
            let mut quiet = Buffer::empty(area);
            ScopeView {
                audio: Some(&frame),
                motion: super::super::Motion::Off,
                style: *style,
                look,
            }
            .render(area, &mut quiet);
            assert_eq!(
                quiet,
                Buffer::empty(area),
                "{} drew over silence",
                style.name()
            );
        }
        let mut state = VisualState::default();
        let mut column = [-80.0f32; UI_SPECTRUM_BINS];
        for (band, level) in column.iter_mut().enumerate() {
            *level = -60.0 + band as f32 * 63.0 / (UI_SPECTRUM_BINS - 1) as f32;
        }
        for _ in 0..6 {
            state.install_analyser_for_tests(column);
        }
        // The spectrograms and the matrix shade by level, so any drawn
        // cell counts for the spectrum.
        let drawn = |buffer: &Buffer| {
            (0..area.height)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| buffer.cell((x, y)).unwrap().symbol() != " ")
                .count()
        };
        // Time runs across: the newest frame at the right edge and the
        // older ones to its left, as far back as the ring reaches and no
        // further. A Braille cell holds two columns of time, so the same
        // six frames take half as many cells as the block waterfall's.
        {
            let columns = |style: SpectrumStyle| {
                let mut buffer = Buffer::empty(area);
                SpectrumView {
                    bands: state.analyser(None),
                    history: state.spectrogram(None),
                    motion: super::super::Motion::Live,
                    style,
                    look,
                }
                .render(area, &mut buffer);
                (0..area.width)
                    .filter(|&x| {
                        (0..area.height).any(|y| buffer.cell((x, y)).unwrap().symbol() != " ")
                    })
                    .collect::<Vec<u16>>()
            };
            let braille = columns(SpectrumStyle::Braille);
            let block = columns(SpectrumStyle::Waterfall);
            assert_eq!(
                block.last().copied(),
                Some(area.width - 1),
                "the newest frame is at the right edge: {block:?}"
            );
            assert_eq!(braille.last().copied(), Some(area.width - 1), "{braille:?}");
            assert_eq!(block.len(), 6, "six frames, six columns: {block:?}");
            assert_eq!(
                braille.len(),
                3,
                "two columns of time a Braille cell: {braille:?}"
            );
        }
        for style in SpectrumStyle::ALL {
            let mut buffer = Buffer::empty(area);
            SpectrumView {
                bands: state.analyser(None),
                history: state.spectrogram(None),
                motion: super::super::Motion::Live,
                style: *style,
                look,
            }
            .render(area, &mut buffer);
            assert!(
                drawn(&buffer) > 8,
                "{} drew {}",
                style.name(),
                drawn(&buffer)
            );
            let mut quiet = Buffer::empty(area);
            SpectrumView {
                bands: state.analyser(None),
                history: state.spectrogram(None),
                motion: super::super::Motion::Off,
                style: *style,
                look,
            }
            .render(area, &mut quiet);
            assert_eq!(
                quiet,
                Buffer::empty(area),
                "{} drew while off",
                style.name()
            );
        }
    }

    #[test]
    fn spectrum_bars_fill_each_cell_and_keep_the_palette() {
        use super::super::super::graphics::{Tier, set_tier, tier};

        struct Restore(Tier);
        impl Drop for Restore {
            fn drop(&mut self) {
                set_tier(self.0);
            }
        }

        let _restore = Restore(tier());
        let theme = Theme::built_in_default();
        let mut bands = AnalyserBands::default();
        bands.levels.fill(-3.0);
        bands.peaks.fill(-3.0);
        for raster in [Tier::Cells, Tier::Fine] {
            set_tier(raster);
            for cells in [1, 24, 96] {
                let area = Rect::new(0, 0, cells, 8);
                for colour in [Colouring::Mono, Colouring::Theme, Colouring::Rainbow] {
                    let look = Look {
                        theme: &theme,
                        colour,
                        seconds: 0.0,
                        level: 0.5,
                    };
                    let mut buffer = Buffer::empty(area);
                    SpectrumView {
                        bands: Some(&bands),
                        history: None,
                        motion: super::super::Motion::Live,
                        style: SpectrumStyle::Bars,
                        look,
                    }
                    .render(area, &mut buffer);
                    let palette = look.palette(usize::from(cells) * 2, 32);
                    for x in 0..cells {
                        let cell = &buffer[(x, area.height - 1)];
                        assert_eq!(cell.symbol(), "⣿", "gap at column {x}");
                        let expected = if colour == Colouring::Theme {
                            spectrum_color(&theme, usize::from(x), usize::from(cells))
                        } else {
                            palette.at(usize::from(x) * 2 + 1, 31)
                        };
                        assert_eq!(cell.fg, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn spectrum_bars_keep_narrow_high_frequency_peaks() {
        let theme = Theme::built_in_default();
        let look = Look {
            theme: &theme,
            colour: Colouring::Mono,
            seconds: 0.0,
            level: 0.5,
        };
        for cells in [24, 96, 200] {
            let area = Rect::new(0, 0, cells, 8);
            for bin in [400, UI_SPECTRUM_BINS - 1] {
                let mut bands = AnalyserBands::default();
                bands.levels[bin] = -3.0;
                bands.peaks[bin] = 0.0;
                let mut buffer = Buffer::empty(area);
                SpectrumView {
                    bands: Some(&bands),
                    history: None,
                    motion: super::super::Motion::Live,
                    style: SpectrumStyle::Bars,
                    look,
                }
                .render(area, &mut buffer);
                let lit: Vec<_> = (0..cells)
                    .filter(|&x| buffer[(x, area.height - 1)].symbol() != " ")
                    .collect();
                assert_eq!(lit.len(), 1, "bin {bin} at width {cells}: {lit:?}");
                assert!(lit[0] > cells * 3 / 4);
                if bin == UI_SPECTRUM_BINS - 1 {
                    assert_eq!(lit[0], cells - 1);
                }
            }
        }
    }

    /// A spectrogram needs two shades a cell, which only half blocks
    /// carry. The dock must use the spectrogram's own canvas, not
    /// `Canvas::bars`, which is sextants on the fine tier and reduces a
    /// cell to one colour.
    #[test]
    fn the_dock_spectrogram_keeps_its_shades_on_fine() {
        use super::super::super::graphics::{Tier, set_tier, tier};

        struct Restore(Tier);
        impl Drop for Restore {
            fn drop(&mut self) {
                set_tier(self.0);
            }
        }

        let theme = super::super::super::theme::Theme::built_in_default();
        let look = Look {
            theme: &theme,
            colour: Colouring::Theme,
            seconds: 0.0,
            level: 0.5,
        };
        let mut state = VisualState::default();
        let mut column = [-90.0f32; UI_SPECTRUM_BINS];
        // A ramp across log-spaced rows, so neighbouring rows want different shades.
        for (bin, level) in column.iter_mut().enumerate() {
            *level = -80.0 + (bin as f32 + 1.0).ln() * 63.0 / (UI_SPECTRUM_BINS as f32).ln();
        }
        for _ in 0..8 {
            state.install_analyser_for_tests(column);
        }
        let area = Rect::new(0, 0, 24, 6);
        let _restore = Restore(tier());
        set_tier(Tier::Fine);
        let mut buffer = Buffer::empty(area);
        SpectrumView {
            bands: state.analyser(None),
            history: state.spectrogram(None),
            motion: super::super::Motion::Live,
            style: SpectrumStyle::Waterfall,
            look,
        }
        .render(area, &mut buffer);

        let mut shades = std::collections::HashSet::new();
        let mut halves = 0;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buffer[(x, y)];
                match cell.symbol() {
                    " " => {}
                    "▀" => {
                        halves += 1;
                        shades.insert((cell.fg, cell.bg));
                    }
                    "█" => {
                        shades.insert((cell.fg, cell.bg));
                    }
                    other => panic!("the waterfall drew {other:?}, not half blocks"),
                }
            }
        }
        assert!(halves > 0, "a sextant cell cannot hold two colours");
        assert!(
            shades.len() > 4,
            "the picture kept only {} shades",
            shades.len()
        );
    }
}

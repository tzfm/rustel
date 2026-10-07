//! The matrix rain: streams of glyphs falling through the unused cells.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct MatrixRain {
    pub(super) elapsed_ms: u64,
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) muted: Color,
    pub(super) density: u8,
    pub(super) speed: u8,
    pub(super) area: Option<Rect>,
}

impl MatrixRain {
    pub(super) fn new(active: &ActiveVisual) -> Self {
        Self {
            elapsed_ms: 0,
            background: active.background,
            foreground: active.foreground,
            accent: active.accent,
            muted: active.muted,
            density: active.config.density,
            speed: active.config.speed,
            area: None,
        }
    }

    pub(super) fn render(&self, buffer: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        let glyph_tick = self.elapsed_ms / 85;
        for x in area.x..area.right() {
            let column = hash(u64::from(x), 0x85eb_ca6b, 0xc2b2_ae35);
            if column % 100 >= u64::from(self.density) {
                continue;
            }

            let height = i32::from(area.height);
            let max_trail = height.clamp(5, 22);
            let trail = 5 + (column % u64::try_from((max_trail - 4).max(1)).unwrap_or(1)) as i32;
            let gap = 4 + ((column >> 11) % 13) as i32;
            let cycle = height + trail + gap;
            let variation = 0.65 + ((column >> 19) % 71) as f32 / 100.0;
            let rows_per_second = (4.0 + f32::from(self.speed) * 0.18) * variation;
            let travelled = (self.elapsed_ms as f32 * rows_per_second / 1000.0) as i32;
            let phase = ((column >> 27) % u64::try_from(cycle).unwrap_or(1)) as i32;
            let head = (travelled + phase).rem_euclid(cycle) - trail;

            for distance in 0..trail {
                let local_y = head - distance;
                if !(0..height).contains(&local_y) {
                    continue;
                }
                // Broken trails read as separate falling fragments instead of
                // solid green bars. The leader itself is never dropped.
                let speck = hash(u64::from(x), local_y as u64, glyph_tick);
                if distance > 1 && speck % 100 > 88 {
                    continue;
                }
                let y = area.y + local_y as u16;
                let Some(cell) = buffer.cell_mut((x, y)) else {
                    continue;
                };
                // Never repaint source text, a selection, a visualizer or a
                // panel. This also avoids the continuation half of a wide
                // glyph, whose symbol is empty rather than one blank cell.
                if cell.symbol() != " " || cell.bg != self.background {
                    continue;
                }

                let glyph = MATRIX_GLYPHS[(speck as usize) % MATRIX_GLYPHS.len()];
                let color = if distance == 0 {
                    self.foreground
                } else if distance <= 2 {
                    self.accent
                } else {
                    blend_rgb(
                        self.accent,
                        self.muted,
                        distance as f32 / trail.max(1) as f32,
                    )
                };
                cell.set_symbol(glyph).set_fg(color);
                if distance == 0 {
                    cell.modifier.insert(Modifier::BOLD);
                } else if distance * 3 > trail * 2 {
                    cell.modifier.insert(Modifier::DIM);
                }
            }
        }
    }
}

impl Shader for MatrixRain {
    fn name(&self) -> &'static str {
        "rustel_matrix"
    }

    fn process(&mut self, duration: Duration, buffer: &mut Buffer, area: Rect) -> Option<Duration> {
        self.elapsed_ms = self
            .elapsed_ms
            .wrapping_add(u64::from(duration.as_millis()));
        self.render(buffer, self.area.unwrap_or(area).intersection(area));
        None
    }

    fn done(&self) -> bool {
        false
    }

    fn clone_box(&self) -> Box<dyn Shader> {
        Box::new(self.clone())
    }

    fn area(&self) -> Option<Rect> {
        self.area
    }

    fn set_area(&mut self, area: Rect) {
        self.area = Some(area);
    }

    fn filter(&mut self, _filter: CellFilter) {}
}

pub(super) const MATRIX_GLYPHS: &[&str] = &[
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "ｱ", "ｲ", "ｳ", "ｴ", "ｵ", "ｶ", "ｷ", "ｸ", "ｹ",
    "ｺ", "ｻ", "ｼ", "ｽ", "ｾ", "ｿ", "ﾀ", "ﾁ", "ﾂ", "ﾃ", "ﾄ", "ﾅ", "ﾆ", "ﾇ", "ﾈ", "ﾉ", "ﾊ", "ﾋ", "ﾌ",
    "ﾍ", "ﾎ", "ﾏ", "ﾐ", "ﾑ", "ﾒ", "ﾓ", "ﾔ", "ﾕ", "ﾖ", "ﾗ", "ﾘ", "ﾙ", "ﾚ", "ﾛ", "ﾜ", "ﾝ", ":", "+",
    "*", "=", "<", ">",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_visuals::test_support::*;
    use ratatui::style::Style;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn matrix_uses_only_unused_theme_background_cells() {
        let area = Rect::new(0, 0, 48, 18);
        let background = Color::Rgb(0, 6, 0);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, Style::default().bg(background));
        buffer
            .cell_mut((5, 5))
            .expect("text cell")
            .set_symbol("code")
            .set_fg(Color::White);
        buffer
            .cell_mut((6, 5))
            .expect("panel cell")
            .set_bg(Color::Rgb(4, 24, 10));

        let active = matrix_test_visual(background);
        let mut rain = MatrixRain::new(&active);
        rain.process(Duration::from_millis(700), &mut buffer, area);

        assert_eq!(buffer.cell((5, 5)).expect("text cell").symbol(), "code");
        assert_eq!(buffer.cell((6, 5)).expect("panel cell").symbol(), " ");
        assert!(
            buffer.content.iter().any(|cell| cell.symbol() != " "),
            "the remaining background receives rain"
        );
    }

    #[test]
    fn matrix_leaders_move_down_the_terminal() {
        let area = Rect::new(0, 0, 120, 40);
        let background = Color::Rgb(0, 6, 0);
        let foreground = Color::Rgb(184, 255, 200);
        let active = matrix_test_visual(background);
        let render = |elapsed| {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            let mut rain = MatrixRain::new(&active);
            rain.process(Duration::from_millis(elapsed), &mut buffer, area);
            (area.x..area.right())
                .map(|x| {
                    (area.y..area.bottom()).find(|&y| {
                        buffer
                            .cell((x, y))
                            .is_some_and(|cell| cell.fg == foreground)
                    })
                })
                .collect::<Vec<_>>()
        };

        let earlier = render(250);
        let later = render(650);
        assert!(
            earlier
                .iter()
                .zip(later)
                .any(|(before, after)| matches!((before, after), (Some(a), Some(b)) if b > *a)),
            "at least one uninterrupted stream leader falls to a lower row"
        );
    }

    #[test]
    fn every_matrix_glyph_occupies_exactly_one_cell() {
        for glyph in MATRIX_GLYPHS {
            assert_eq!(glyph.width(), 1, "{glyph:?} would damage the next cell");
        }
    }
}

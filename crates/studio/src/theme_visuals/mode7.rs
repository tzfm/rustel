//! Mode 7: a checkered plane in perspective, driven toward the viewer the
//! way a SNES drew a track out of a flat picture.

use super::*;

impl ShowroomFx {
    pub(super) fn paint_mode7(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame {
            time,
            density,
            aspect,
            ..
        } = *frame;
        let Spot {
            x,
            y,
            nx,
            ny,
            blank,
            text,
            ..
        } = *spot;
        // A flat picture in perspective, driven at the viewer
        // the way a SNES drew a track: a checkered plain under
        // a road that curves, thinning into haze at the horizon.
        let horizon = 0.42 + 0.008 * (time * 3.1).sin();
        let speed = 6.0 + f32::from(self.speed) * 0.12;
        if ny > horizon {
            let depth = ny - horizon;
            let z = 0.28 / depth;
            let curve = (time * 0.45).sin() * 0.9;
            let u = (nx - 0.5) * aspect * z * 2.4 + curve * z * 0.35;
            let v = z - time * speed;
            let fade = (depth * 5.0).clamp(0.0, 1.0);
            let checker = ((u * 0.9).floor() as i64 + (v * 0.9).floor() as i64).rem_euclid(2) == 0;
            let base = if u.abs() < 1.35 {
                if u.abs() < 0.08 && (v * 1.5).floor().rem_euclid(2.0) == 0.0 {
                    self.foreground
                } else if u.abs() > 1.15 {
                    if (v * 1.2).floor().rem_euclid(2.0) == 0.0 {
                        self.accent
                    } else {
                        self.foreground
                    }
                } else {
                    blend_rgb(self.muted, self.background, 0.62)
                }
            } else if checker {
                blend_rgb(self.accent, self.background, 0.45)
            } else {
                blend_rgb(self.secondary, self.background, 0.5)
            };
            let colour = blend_rgb(self.background, base, 0.3 + 0.7 * fade);
            if blank {
                let glyph = if fade > 0.75 {
                    "█"
                } else if fade > 0.45 {
                    "▓"
                } else if fade > 0.2 {
                    "▒"
                } else {
                    "░"
                };
                cell.set_symbol(glyph).set_fg(colour);
            } else if text {
                cell.set_fg(blend_rgb(cell.fg, base, 0.3));
            }
        } else if blank {
            // The sky: haze thickening toward the horizon,
            // a few stars above it.
            let haze = (1.0 - (horizon - ny) / horizon).powi(3).clamp(0.0, 1.0);
            if haze > 0.55 {
                cell.set_symbol("▒").set_fg(blend_rgb(
                    self.background,
                    self.accent,
                    (haze - 0.55) * 1.2,
                ));
            } else if haze > 0.3 {
                cell.set_symbol("░")
                    .set_fg(blend_rgb(self.background, self.accent, 0.25));
            } else {
                let seed = hash(u64::from(x), u64::from(y), 0x0de7);
                if hash_unit(seed) < 0.01 + density * 0.012 {
                    cell.set_symbol("·")
                        .set_fg(blend_rgb(self.muted, self.foreground, 0.5));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_visuals::test_support::*;
    use ratatui::style::Style;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn mode7_is_a_checkered_plane_racing_toward_the_viewer() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(13, 11, 30);
        let active = showroom_test_visual(CellEffect::Mode7, background);
        let signal = Arc::new(ReactiveAudioSignal::default());
        let mut effect = ShowroomFx::new(&active, Arc::clone(&signal));
        let render = |effect: &ShowroomFx| {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            effect.render(&mut buffer, area);
            buffer
        };
        let painted = |buffer: &Buffer, rows: std::ops::Range<u16>| {
            rows.flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    ["█", "▓", "▒", "░"].contains(&buffer.cell((x, y)).expect("cell").symbol())
                })
                .count()
        };
        effect.elapsed_ms = 500;
        let early = render(&effect);
        effect.elapsed_ms = 620;
        let later = render(&effect);
        assert!(
            painted(&early, 14..30) >= 100 * 16 * 9 / 10,
            "the plain fills the ground"
        );
        assert!(
            painted(&early, 0..8) <= 100 * 8 / 6,
            "the sky stays mostly open"
        );
        let ground = |buffer: &Buffer| {
            (14..30u16)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let cell = buffer.cell((x, y)).expect("cell");
                    (cell.symbol().to_owned(), cell.fg)
                })
                .collect::<Vec<_>>()
        };
        assert_ne!(ground(&early), ground(&later), "it moves");
        let foreground = active.foreground;
        assert!(
            (14..30u16)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .any(|(x, y)| early.cell((x, y)).expect("cell").fg == foreground),
            "a road with its markings"
        );
        assert!(early.content.iter().all(|cell| cell.symbol().width() <= 1));
    }
}

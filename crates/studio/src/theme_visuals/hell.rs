//! Hell: a lava sea with veins and bubbles, flames along it, embers up and
//! ash down, a sky that beats.

use super::*;

impl ShowroomFx {
    /// Where the lava stands at `x`: a swell that rolls slowly one way and
    /// a shorter one the other.
    pub(super) fn lava_surface(&self, x: f32, height: f32, time: f32) -> f32 {
        height * 0.72 - 1.2 * (x * 0.35 + time * 1.1).sin() - 0.8 * (x * 0.13 - time * 0.6).sin()
    }

    /// Hell: the sprites over the lava - flame tongues along its surface,
    /// embers rising off it, ash coming down through everything.
    pub(super) fn hell_frame(&self, area: Rect, time: f32, rms: f32) -> SceneFrame {
        use std::f32::consts::TAU;
        let width = f32::from(area.width);
        let height = f32::from(area.height);
        let mut canvas = SceneCanvas {
            area,
            light: (width * 0.5, height),
            background: self.background,
            foreground: self.foreground,
            sprites: std::collections::HashMap::new(),
        };
        let ember = blend_rgb(self.accent, self.background, 0.35);
        let sprite = |glyph: &'static str, colour: Color, bold: bool| SpaceSprite {
            glyph,
            colour,
            bold,
        };
        // Flame tongues, each flaring on a clock of its own.
        let sources = 6 + u64::from(self.density) / 12;
        for k in 0..sources {
            let seed = hash(k, 0x4e11, 0x0f1a);
            let sx = width * (0.03 + 0.94 * hash_unit(seed));
            let surface = self.lava_surface(sx, height, time);
            let flare = 0.5
                + 0.5
                    * (time * (0.7 + hash_unit(seed >> 8) * 1.1) + hash_unit(seed >> 16) * TAU)
                        .sin();
            let flame_height = (2.0 + flare * height * 0.22) * (0.6 + rms * 0.8);
            for row in 1..=(flame_height.ceil() as i32) {
                let y = surface - row as f32;
                let v = (row as f32 / flame_height).clamp(0.0, 1.0);
                let edge = (2.5 * (1.0 - v * 0.85)).max(0.5);
                let sway = (time * 4.0 + v * 5.0 + k as f32).sin() * v * 1.5;
                for dx in -4..=4 {
                    let x = (sx + sway).round() + dx as f32;
                    let u = (x - sx - sway) / edge;
                    if u.abs() > 1.0 {
                        continue;
                    }
                    let flick =
                        0.7 + 0.6 * hash_unit(hash(x as u64, y as u64, (time * 7.0) as u64));
                    let heat =
                        ((1.0 - u.abs()).powf(0.7) * (1.0 - v).powf(0.6) * flick).clamp(0.0, 1.2);
                    let tongue = if heat > 0.85 {
                        Some(("█", blend_rgb(self.secondary, self.foreground, 0.5), true))
                    } else if heat > 0.6 {
                        Some(("▓", self.secondary, true))
                    } else if heat > 0.4 {
                        Some(("▒", self.accent, false))
                    } else if heat > 0.24 {
                        Some(("░", ember, false))
                    } else if heat > 0.12 {
                        Some(("'", ember, false))
                    } else {
                        None
                    };
                    if let Some((glyph, colour, bold)) = tongue {
                        canvas.put(x, y, sprite(glyph, colour, bold), false);
                    }
                }
            }
        }
        // Embers off the lava, rising and cooling.
        for k in 0..(14 + u64::from(self.density) / 5) {
            let seed = hash(k, 0xe3be, 0x0f1e);
            let life = fract(time * (0.22 + hash_unit(seed >> 8) * 0.2) + hash_unit(seed));
            let sx =
                width * hash_unit(seed >> 16) + (time * 1.5 + k as f32).sin() * (1.0 + life * 4.0);
            let surface = self.lava_surface(sx, height, time);
            let sy = surface - 1.0 - life * height * 0.7;
            if sy >= 0.0 {
                let (glyph, colour, bold) = if life < 0.3 {
                    ("*", self.secondary, true)
                } else if life < 0.65 {
                    ("·", self.accent, false)
                } else {
                    ("·", ember, false)
                };
                canvas.put(sx, sy, sprite(glyph, colour, bold), false);
            }
        }
        // Ash, falling through it all.
        for k in 0..(10 + u64::from(self.density) / 6) {
            let seed = hash(k, 0xa5a5, 0x0ce);
            let life = fract(time * (0.09 + hash_unit(seed >> 8) * 0.06) + hash_unit(seed));
            let sx = width * hash_unit(seed >> 16) + (time * 0.8 + k as f32 * 1.3).sin() * 2.0;
            let sy = life * height;
            if sy < self.lava_surface(sx, height, time) {
                canvas.put(
                    sx,
                    sy,
                    sprite(
                        if hash_unit(seed >> 24) < 0.5 {
                            "∙"
                        } else {
                            "·"
                        },
                        blend_rgb(self.muted, self.background, 0.3 + life * 0.3),
                        false,
                    ),
                    false,
                );
            }
        }
        SceneFrame {
            sprites: canvas.sprites,
            glow: None,
        }
    }
}

impl ShowroomFx {
    pub(super) fn paint_hell(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame {
            height,
            time,
            reactive,
            ..
        } = *frame;
        let Spot {
            x,
            y,
            px,
            py,
            blank,
            text,
            ..
        } = *spot;
        let hell = &frame.hell;
        if let Some(frame) = &hell {
            let surface = self.lava_surface(px, height, time);
            if let Some(sprite) = frame.sprites.get(&(x, y)) {
                if blank {
                    cell.set_symbol(sprite.glyph);
                }
                cell.set_fg(sprite.colour);
                if sprite.bold {
                    cell.modifier.insert(Modifier::BOLD);
                }
            } else if py >= surface {
                // The lava: dark crust over bright veins that
                // drift, bubbling where it breaks the surface.
                let depth = py - surface;
                let vein = ((px * 0.22 + depth * 0.7 + time * 0.9).sin()
                    * (px * 0.09 - time * 0.5 + depth).cos())
                .abs();
                let flow = hash_unit(hash(u64::from(x), u64::from(y), (time * 2.0) as u64)) * 0.15;
                let heat = (vein + flow - depth * 0.04 + reactive.bass * 0.15).clamp(0.0, 1.0);
                if blank {
                    let bubble = depth < 1.0
                        && hash_unit(hash(u64::from(x), (time * 3.0) as u64, 0xb0b)) < 0.05;
                    let (glyph, colour) = if bubble {
                        ("∘", self.secondary)
                    } else if heat > 0.8 {
                        ("█", blend_rgb(self.secondary, self.foreground, 0.5))
                    } else if heat > 0.55 {
                        ("▓", self.secondary)
                    } else if heat > 0.35 {
                        ("▒", self.accent)
                    } else {
                        ("░", blend_rgb(self.accent, self.background, 0.5))
                    };
                    cell.set_symbol(glyph).set_fg(colour);
                } else if text {
                    // Code in the lava glows with it.
                    cell.set_fg(blend_rgb(cell.fg, self.accent, 0.6));
                    cell.modifier.insert(Modifier::BOLD);
                }
            } else {
                // The sky: a heartbeat of red, thickest just
                // over the lava, beating harder with the music.
                let above = (surface - py) / height;
                let pulse = 0.5 + 0.5 * (time * 1.6).sin();
                let wash = ((1.0 - above * 2.2).clamp(0.0, 1.0)
                    * (0.25 + 0.35 * pulse + reactive.rms * 0.4))
                    .min(1.0);
                if blank {
                    let seed = hash(u64::from(x), u64::from(y), 0x5e11 + (time * 1.5) as u64);
                    if hash_unit(seed) < wash * 0.35 {
                        cell.set_symbol("░").set_fg(blend_rgb(
                            self.background,
                            self.accent,
                            0.5 + wash * 0.3,
                        ));
                    }
                } else if text && wash > 0.05 {
                    cell.set_fg(blend_rgb(cell.fg, self.accent, wash * 0.5));
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
    fn hell_is_a_lava_sea_with_flames_over_it_and_ash_through_it() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(10, 2, 4);
        let active = showroom_test_visual(CellEffect::Hell, background);
        let signal = Arc::new(ReactiveAudioSignal::default());
        let mut effect = ShowroomFx::new(&active, Arc::clone(&signal));
        let render = |effect: &ShowroomFx| {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            effect.render(&mut buffer, area);
            buffer
        };
        let cells = |buffer: &Buffer, glyphs: &[&str]| {
            (0..area.height)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| glyphs.contains(&buffer.cell((x, y)).expect("cell").symbol()))
                .collect::<Vec<_>>()
        };
        effect.elapsed_ms = 1_000;
        let early = render(&effect);
        effect.elapsed_ms = 2_400;
        let later = render(&effect);
        let lava = cells(&early, &["█", "▓", "▒", "░", "∘"])
            .into_iter()
            .filter(|&(_, y)| y >= 23)
            .count();
        assert!(lava >= 100 * 7 * 6 / 10, "a sea of it: {lava}");
        let flames = cells(&early, &["█", "▓", "▒", "'"])
            .into_iter()
            .filter(|&(_, y)| (10..20).contains(&y))
            .count();
        assert!(flames >= 10, "flames over it: {flames}");
        assert!(
            cells(&early, &["*", "·", "∙"]).len() >= 12,
            "embers and ash"
        );
        assert_ne!(
            cells(&early, &["*", "·", "∙"]),
            cells(&later, &["*", "·", "∙"]),
            "moving"
        );
        assert!(early.content.iter().all(|cell| cell.symbol().width() <= 1));
    }
}

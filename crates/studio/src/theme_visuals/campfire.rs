//! The campfire: logs, a fire, sparks and smoke, a tent lit on its near
//! side, the night over everything.

use super::*;

impl ShowroomFx {
    /// The camp: logs on the ground, a fire over them, sparks and smoke
    /// above it, a tent beside it with the fire on its canvas, and the night
    /// over all of it.
    pub(super) fn campfire_frame(&self, area: Rect, time: f32, rms: f32) -> SceneFrame {
        let width = f32::from(area.width);
        let height = f32::from(area.height);
        let fire_x = width * 0.56;
        // The first row of ground, and the row the logs lie on.
        let ground = height - 2.0;
        let base = ground - 1.0;
        let flame_height = (height * 0.32).clamp(5.0, 12.0);
        let mut canvas = SceneCanvas {
            area,
            light: (fire_x, base - 1.0),
            background: self.background,
            foreground: self.foreground,
            sprites: std::collections::HashMap::new(),
        };
        let ember = blend_rgb(self.accent, self.background, 0.35);
        let bark = blend_rgb(self.accent, self.background, 0.62);
        let sprite = |glyph: &'static str, colour: Color, bold: bool| SpaceSprite {
            glyph,
            colour,
            bold,
        };

        // The ground: grass on the first row, earth below, both warmed by
        // the fire and fading with distance from it.
        let breath = 0.55 + rms * 0.6;
        let green = blend_rgb(self.muted, self.background, 0.35);
        let earth = blend_rgb(self.muted, self.background, 0.62);
        let mut x = 0.0;
        while x < width {
            let distance = ((x - fire_x) * 0.5).abs() / (width * 0.25).max(1.0);
            let glow =
                (1.0 - distance).clamp(0.0, 1.0) * (breath + 0.08 * (time * 9.0 + x * 0.3).sin());
            let tuft = hash_unit(hash(x as u64, 0xca17, 0xf1e1));
            let grass = if tuft < 0.18 {
                "\""
            } else if tuft < 0.33 {
                "'"
            } else if tuft < 0.45 {
                ","
            } else {
                "▁"
            };
            canvas.put(
                x,
                ground,
                sprite(grass, blend_rgb(green, self.accent, glow * 0.7), false),
                true,
            );
            canvas.put(
                x,
                ground + 1.0,
                sprite("▒", blend_rgb(earth, ember, glow * 0.5), false),
                true,
            );
            x += 1.0;
        }

        // The logs, crossed, on the row above the ground.
        for (index, dx) in (-3..=3).enumerate() {
            let glyph = if index % 2 == 0 { "▄" } else { "▀" };
            canvas.put(fire_x + dx as f32, base, sprite(glyph, bark, false), true);
        }

        // The fire: hottest and widest at the logs, narrowing as it rises,
        // flickering cell by cell and swaying as a whole.
        let mut y = (base - flame_height).ceil();
        while y < base {
            let v = ((base - y) / flame_height).clamp(0.0, 1.0);
            let edge = (4.5 * (1.0 - v * 0.8)).max(0.6);
            let sway = (time * 3.0 + v * 4.0).sin() * v * 1.2;
            let mut x = (fire_x - 6.0).floor();
            while x <= (fire_x + 6.0).ceil() {
                let u = (x - fire_x - sway) / edge;
                if u.abs() <= 1.0 {
                    let flick =
                        0.7 + 0.6 * hash_unit(hash(x as u64, y as u64, (time * 7.0) as u64));
                    let wave = 0.12 * (y * 1.1 - time * 11.0 + x * 0.6).sin();
                    let core = (1.0 - u.abs()).powf(0.7) * (1.0 - v).powf(0.6);
                    let heat = (core * flick + wave + rms * 0.2).clamp(0.0, 1.2);
                    let tongue = if heat > 0.85 {
                        Some(("█", blend_rgb(self.secondary, self.foreground, 0.55), true))
                    } else if heat > 0.62 {
                        Some(("▓", self.secondary, true))
                    } else if heat > 0.42 {
                        Some(("▒", self.accent, false))
                    } else if heat > 0.26 {
                        Some(("░", ember, false))
                    } else if heat > 0.14 {
                        Some(("'", ember, false))
                    } else {
                        None
                    };
                    if let Some((glyph, colour, bold)) = tongue {
                        canvas.put(x, y, sprite(glyph, colour, bold), true);
                    }
                }
                x += 1.0;
            }
            y += 1.0;
        }

        // Sparks, rising and drifting, cooling as they go; smoke above them.
        for k in 0..16u64 {
            let seed = hash(k, 0x5a4b, 0x0f1e);
            let life = fract(time * (0.25 + hash_unit(seed >> 8) * 0.2) + hash_unit(seed));
            let rise = flame_height + height * 0.35;
            let sy = base - 1.0 - life * rise;
            let sx = fire_x
                + (hash_unit(seed >> 16) - 0.5) * 4.0
                + (time * 2.0 + k as f32).sin() * (1.0 + life * 3.0);
            if sy >= 0.0 {
                let (glyph, colour, bold) = if life < 0.35 {
                    ("*", self.secondary, true)
                } else if life < 0.7 {
                    ("·", self.accent, false)
                } else {
                    ("·", ember, false)
                };
                canvas.put(sx, sy, sprite(glyph, colour, bold), false);
            }
        }
        for k in 0..7u64 {
            let seed = hash(k, 0x5300, 0x0ce);
            let life = fract(time * 0.12 + hash_unit(seed));
            let sy = base - flame_height - 1.0 - life * height * 0.45;
            let sx = fire_x
                + (time * 0.6 + k as f32 * 1.7).sin() * 2.0
                + life * 6.0 * (hash_unit(seed >> 8) - 0.3);
            if sy >= 0.0 {
                let smoke = blend_rgb(self.muted, self.background, 0.45 + life * 0.4);
                canvas.put(
                    sx,
                    sy,
                    sprite(if life < 0.5 { "░" } else { "∙" }, smoke, false),
                    false,
                );
            }
        }

        // The tent, its fire-facing side lit, a dark doorway at the bottom.
        let tent_x = width * 0.22;
        let tent_height = (height * 0.22).clamp(4.0, 8.0);
        let apex = base - tent_height + 1.0;
        let cloth = blend_rgb(self.muted, self.background, 0.45);
        let lit = blend_rgb(cloth, self.accent, 0.35);
        let doorway = blend_rgb(self.background, self.accent, 0.12);
        let mut r = 0.0;
        while r < tent_height {
            let y = apex + r;
            let half = r * 2.0;
            if r == 0.0 {
                canvas.put(tent_x, y, sprite("▲", lit, false), true);
            } else {
                let mut x = (tent_x - half).floor();
                while x <= (tent_x + half).ceil() {
                    let offset = x - tent_x;
                    if offset.abs() <= half {
                        let (glyph, colour) = if offset <= -half + 0.5 {
                            ("╱", self.foreground)
                        } else if offset >= half - 0.5 {
                            ("╲", self.foreground)
                        } else if r >= tent_height - 2.0 && offset.abs() < half * 0.28 {
                            ("█", doorway)
                        } else if offset > 0.0 {
                            ("▓", lit)
                        } else {
                            ("▒", cloth)
                        };
                        canvas.put(x, y, sprite(glyph, colour, false), true);
                    }
                    x += 1.0;
                }
            }
            r += 1.0;
        }

        // The moon, lit from above.
        let moon = SpaceBody {
            x: width * 0.86,
            y: 2.5,
            rx: 3.0,
            ry: 1.5,
            colour: blend_rgb(self.foreground, self.background, 0.3),
            banded: false,
            near: true,
        };
        canvas.light = (moon.x, -40.0);
        canvas.disc(&moon, false);

        SceneFrame {
            sprites: canvas.sprites,
            glow: Some((fire_x, base, flame_height * 1.6)),
        }
    }
}

impl ShowroomFx {
    pub(super) fn paint_campfire(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame {
            height,
            time,
            density,
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
        let campfire = &frame.campfire;
        if let Some(frame) = &campfire {
            if let Some(sprite) = frame.sprites.get(&(x, y)) {
                if blank {
                    cell.set_symbol(sprite.glyph);
                }
                cell.set_fg(sprite.colour);
                if sprite.bold {
                    cell.modifier.insert(Modifier::BOLD);
                }
            } else if text {
                // The code near the fire is warmed by it.
                if let Some((gx, gy, reach)) = frame.glow {
                    let distance = (((px - gx) * 0.5).powi(2) + (py - gy).powi(2)).sqrt();
                    let warmth = (1.0 - distance / reach).clamp(0.0, 1.0);
                    if warmth > 0.0 {
                        cell.set_fg(blend_rgb(cell.fg, self.accent, warmth * 0.45));
                    }
                }
            } else if blank && py < height - 4.0 {
                // A still sky, a few stars twinkling in it.
                let seed = hash(u64::from(x), u64::from(y), 0xca11);
                if hash_unit(seed) < 0.012 + density * 0.01 {
                    let twinkle = 0.55
                        + 0.45
                            * (time * (0.8 + hash_unit(seed >> 8) * 1.5)
                                + hash_unit(seed >> 16) * std::f32::consts::TAU)
                                .sin();
                    cell.set_symbol(if seed.is_multiple_of(9) { "✦" } else { "·" })
                        .set_fg(blend_rgb(self.muted, self.foreground, twinkle * 0.8));
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
    fn campfire_burns_between_logs_beside_a_tent_under_the_stars() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(10, 14, 23);
        let active = showroom_test_visual(CellEffect::Campfire, background);
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
        effect.elapsed_ms = 2_500;
        let later = render(&effect);
        // The logs on the row above the ground, the fire over them.
        let logs = (53..=59)
            .filter(|&x| matches!(early.cell((x, 27)).expect("log").symbol(), "▄" | "▀"))
            .count();
        assert_eq!(logs, 7, "the logs");
        let flames = cells(&early, &["█", "▓", "▒", "░"])
            .into_iter()
            .filter(|&(x, y)| (48..=64).contains(&x) && (15..27).contains(&y))
            .count();
        assert!(flames >= 20, "a fire: {flames} cells");
        assert_ne!(
            cells(&early, &["*", "·"]),
            cells(&later, &["*", "·"]),
            "sparks rise"
        );
        // The tent, and the moon.
        assert!(
            cells(&early, &["╱"]).len() >= 4 && cells(&early, &["╲"]).len() >= 4,
            "a tent"
        );
        assert!(cells(&early, &["▲"]).len() == 1, "with its ridge");
        assert!(
            (80..=92).any(|x| early.cell((x, 2)).expect("sky").symbol() == "█"),
            "the moon"
        );
        assert!(
            early.content.iter().all(|cell| cell.symbol().width() <= 1),
            "nothing wider than a cell"
        );
    }
}

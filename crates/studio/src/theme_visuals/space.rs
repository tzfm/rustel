//! Space: a sun, planets on their orbits, moons and trails, stars drifting
//! past.

use super::*;

pub(super) const SPACE_MAX_PLANETS: usize = 5;

impl ShowroomFx {
    /// Where everything in the sky is this frame.
    ///
    /// A terminal cell is about twice as tall as it is wide, so every orbit
    /// and every body is an ellipse twice as wide as high: a circle seen
    /// from a little above. A planet on the far half of its orbit passes
    /// behind the sun, smaller and dim; on the near half it passes in
    /// front, larger and lit. Periods follow Kepler - the far planets crawl,
    /// the near ones hurry - which is most of what makes it feel like space
    /// rather than a clock.
    pub(super) fn space_frame(&self, area: Rect, time: f32, rms: f32) -> SceneFrame {
        use std::f32::consts::TAU;
        let width = f32::from(area.width);
        let height = f32::from(area.height);
        let (sun_x, sun_y) = (width * 0.6, height * 0.5);
        let mut canvas = SceneCanvas {
            area,
            light: (sun_x, sun_y),
            background: self.background,
            foreground: self.foreground,
            sprites: std::collections::HashMap::new(),
        };
        // Bodies grow with the terminal, within reason.
        let scale = (height / 24.0).clamp(0.8, 2.2);
        let sun_ry = 1.6 * scale;

        // The planets: as many as the density buys, orbits spread from just
        // outside the sun to most of the way to the edge, sizes of their own.
        let count = (2 + usize::from(self.density) / 30).clamp(3, SPACE_MAX_PLANETS);
        let reach = (height * 0.47).max(sun_ry + 4.0);
        let sizes = [0.7, 1.3, 2.2, 0.9, 1.7];
        let mut bodies = Vec::with_capacity(count);
        for index in 0..count {
            let seed = hash(index as u64, 0x5ace, 0x0b17);
            let ry =
                sun_ry + 2.5 + (reach - sun_ry - 2.5) * index as f32 / (count.max(2) - 1) as f32;
            let rx = ry * 2.1;
            // Kepler: the period grows with r^1.5.
            let omega = 2.6 / ry.powf(1.5);
            let theta = hash_unit(seed) * TAU + time * omega;
            // The lower half of the ellipse is the near side; depth also
            // scales the body a little, as perspective would.
            let depth = theta.sin();
            let size = sizes[index % sizes.len()] * scale * (1.0 + 0.25 * depth);
            let colour = match index % 4 {
                0 => self.secondary,
                1 => blend_rgb(self.secondary, self.foreground, 0.5),
                2 => blend_rgb(self.accent, self.secondary, 0.5),
                _ => self.foreground,
            };
            let body = SpaceBody {
                x: sun_x + rx * theta.cos(),
                y: sun_y + ry * theta.sin(),
                rx: size * 2.0,
                ry: size,
                colour,
                banded: index == 2,
                near: depth > 0.0,
            };
            bodies.push((depth, body, rx, ry, theta, seed));
        }
        // Far to near, so what is nearer paints over what is farther, with
        // the sun between the two halves.
        bodies.sort_by(|a, b| a.0.total_cmp(&b.0));
        let orbit = SpaceSprite {
            glyph: "·",
            colour: blend_rgb(self.muted, self.background, 0.62),
            bold: false,
        };
        for (_, _, rx, ry, _, _) in &bodies {
            let dots = ((rx * 1.2) as usize).max(12);
            for dot in 0..dots {
                let a = dot as f32 / dots as f32 * TAU;
                canvas.put(sun_x + rx * a.cos(), sun_y + ry * a.sin(), orbit, false);
            }
        }
        let mut sun_drawn = false;
        for (depth, body, _, _, theta, seed) in &bodies {
            if *depth > 0.0 && !sun_drawn {
                self.space_sun(&mut canvas, sun_ry, time, rms);
                sun_drawn = true;
            }
            if body.banded {
                canvas.ring(body, false, blend_rgb(body.colour, self.muted, 0.5));
            }
            canvas.disc(body, true);
            if body.banded {
                canvas.ring(body, true, blend_rgb(body.colour, self.foreground, 0.35));
            }
            // A moon on the bigger bodies, quick and small.
            if body.ry >= 1.2 {
                let m = theta * 6.0 + hash_unit(seed >> 8) * TAU;
                let moon = SpaceBody {
                    x: body.x + (body.rx + 2.5) * m.cos(),
                    y: body.y + (body.ry + 1.2) * m.sin(),
                    rx: 0.9,
                    ry: 0.45,
                    colour: blend_rgb(body.colour, self.foreground, 0.4),
                    banded: false,
                    near: body.near,
                };
                canvas.disc(&moon, m.sin() >= 0.0);
            }
        }
        if !sun_drawn {
            self.space_sun(&mut canvas, sun_ry, time, rms);
        }
        SceneFrame {
            sprites: canvas.sprites,
            glow: None,
        }
    }

    /// The sun: a disc brightest at the core, and a corona that breathes
    /// with the music.
    pub(super) fn space_sun(&self, canvas: &mut SceneCanvas, ry: f32, time: f32, rms: f32) {
        let (sun_x, sun_y) = canvas.light;
        let rx = ry * 2.0;
        let glow = 0.55 + rms * 1.2 + 0.1 * (time * 2.3).sin();
        // The corona first, so the disc paints over its inner edge.
        let reach = 1.0 + glow * 0.9;
        let (x0, x1) = ((sun_x - rx * reach).floor(), (sun_x + rx * reach).ceil());
        let (y0, y1) = ((sun_y - ry * reach).floor(), (sun_y + ry * reach).ceil());
        let mut y = y0;
        while y <= y1 {
            let mut x = x0;
            while x <= x1 {
                let (nx, ny) = ((x - sun_x) / rx, (y - sun_y) / ry);
                let r = (nx * nx + ny * ny).sqrt();
                if r > 1.0 && r <= reach {
                    let flicker = hash_unit(hash(x as u64, y as u64, (time * 6.0) as u64));
                    if flicker < 0.55 {
                        let fade = ((r - 1.0) / (reach - 1.0)).clamp(0.0, 1.0);
                        canvas.put(
                            x,
                            y,
                            SpaceSprite {
                                glyph: if flicker < 0.12 { "✦" } else { "·" },
                                colour: blend_rgb(self.accent, self.background, 0.2 + fade * 0.6),
                                bold: false,
                            },
                            true,
                        );
                    }
                }
                x += 1.0;
            }
            y += 1.0;
        }
        let mut y = (sun_y - ry).floor();
        while y <= (sun_y + ry).ceil() {
            let mut x = (sun_x - rx).floor();
            while x <= (sun_x + rx).ceil() {
                let (nx, ny) = ((x - sun_x) / rx, (y - sun_y) / ry);
                let r = (nx * nx + ny * ny).sqrt();
                if r <= 1.0 {
                    let (glyph, colour) = if r < 0.55 {
                        ("█", blend_rgb(self.accent, self.foreground, 0.45))
                    } else if r < 0.85 {
                        ("█", self.accent)
                    } else {
                        ("▓", self.accent)
                    };
                    canvas.put(
                        x,
                        y,
                        SpaceSprite {
                            glyph,
                            colour,
                            bold: true,
                        },
                        true,
                    );
                }
                x += 1.0;
            }
            y += 1.0;
        }
    }
}

impl ShowroomFx {
    pub(super) fn paint_space(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, density, .. } = *frame;
        let Spot { x, y, blank, .. } = *spot;
        let space = &frame.space;
        if let Some(frame) = &space {
            if let Some(sprite) = frame.sprites.get(&(x, y)) {
                // A planet passing behind the code lights it.
                if blank {
                    cell.set_symbol(sprite.glyph);
                }
                cell.set_fg(sprite.colour);
                if sprite.bold {
                    cell.modifier.insert(Modifier::BOLD);
                }
            } else if blank {
                // Two layers of stars: the far ones drift
                // slowly, the near ones a little faster, and
                // each twinkles on a clock of its own.
                let layers = [
                    ((time * 0.35) as u64, 0.018 + density * 0.02, 0.55f32),
                    ((time * 0.9) as u64, 0.008 + density * 0.012, 1.0),
                ];
                for (layer, (shift, chance, bright)) in layers.into_iter().enumerate() {
                    let seed = hash(
                        u64::from(x).wrapping_add(shift),
                        u64::from(y),
                        0x57a2 + layer as u64,
                    );
                    if hash_unit(seed) < chance {
                        let twinkle = 0.5
                            + 0.5
                                * (time * (1.2 + hash_unit(seed >> 8) * 2.5)
                                    + hash_unit(seed >> 16) * std::f32::consts::TAU)
                                    .sin();
                        let glyph = match seed % 11 {
                            0 => "✦",
                            1 | 2 => "•",
                            _ => "·",
                        };
                        cell.set_symbol(glyph).set_fg(blend_rgb(
                            self.muted,
                            self.foreground,
                            twinkle * bright,
                        ));
                        break;
                    }
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
    fn space_keeps_a_sun_and_planets_that_orbit_it_over_a_field_of_stars() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(4, 6, 13);
        let active = showroom_test_visual(CellEffect::Space, background);
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
        effect.elapsed_ms = 4_000;
        let later = render(&effect);
        // The sun is a disc of solid cells at the centre, and holds still.
        for buffer in [&early, &later] {
            for x in 57..=63 {
                assert_eq!(
                    buffer.cell((x, 15)).expect("sun").symbol(),
                    "█",
                    "sun at {x}"
                );
            }
        }
        // The planets are bodies - dozens of shaded cells - and they move.
        let body_glyphs = ["█", "▓", "▒", "░"];
        let bodies_early = cells(&early, &body_glyphs);
        let bodies_later = cells(&later, &body_glyphs);
        assert!(
            bodies_early.len() >= 60,
            "bodies, not dots: {}",
            bodies_early.len()
        );
        assert_ne!(bodies_early, bodies_later, "the planets move");
        assert!(
            cells(&early, &["▒", "░"]).len() >= 8,
            "a dark side on the far side of each planet"
        );
        assert!(cells(&early, &["─"]).len() >= 8, "a ring");
        assert!(
            cells(&early, &["·", "✦", "•"]).len() > 40,
            "orbits, corona and stars"
        );
        assert!(
            early.content.iter().all(|cell| cell.symbol().width() <= 1),
            "nothing wider than a cell"
        );
    }
}

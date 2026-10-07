//! The forest: trees, flowers and animals in a clearing under the sun, the
//! animals dancing to the music - rabbits bounding on the beat, a fox
//! nodding to it, birds in the air, butterflies fluttering to the treble,
//! the flowers and the canopies swaying - and resting while it is quiet.

use super::*;

/// How soft a sound still counts as music to dance to.
const FOREST_HEARS: f32 = 0.015;

/// One band of the music as the animals feel it: where it is this moment,
/// and where it has been lately. A hit is the moment leaping over the
/// average - what a foot taps to - where the level alone, high through a
/// whole bar, would only sway.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Pulse {
    fast: f32,
    slow: f32,
}

impl Pulse {
    fn advance(&mut self, value: f32, dt: f32) {
        self.fast += (value - self.fast) * (dt * 25.0).min(1.0);
        self.slow += (value - self.slow) * (dt * 2.5).min(1.0);
    }

    /// How far this moment stands over the average, nought to one.
    fn hit(&self) -> f32 {
        ((self.fast - self.slow) * 4.0).clamp(0.0, 1.0)
    }

    fn level(&self) -> f32 {
        self.fast.clamp(0.0, 1.0)
    }
}

/// What the clearing has felt of the music, carried from frame to frame.
#[derive(Clone, Debug, Default)]
pub(super) struct ForestMemo {
    level: Pulse,
    bass: Pulse,
    mid: Pulse,
    treble: Pulse,
    /// The rabbits' hops, in rows: set off by a hit, falling back with time.
    hops: [f32; 2],
    /// The fox's nod, falling back too.
    nod: f32,
}

impl ForestMemo {
    /// A little later, with what the clearing hears.
    pub(super) fn advance(&mut self, dt: f32, audio: ReactiveAudio) {
        self.level.advance(audio.rms, dt);
        self.bass.advance(audio.bass, dt);
        self.mid.advance(audio.mid, dt);
        self.treble.advance(audio.treble, dt);
        // A hit in the bass sends the first rabbit up, one in the middle
        // the second; a rabbit in the air waits for the next one.
        for (rabbit, band) in [self.bass, self.mid].into_iter().enumerate() {
            self.hops[rabbit] = (self.hops[rabbit] - dt * 6.0).max(0.0);
            if band.hit() > 0.35 && self.hops[rabbit] < 0.3 {
                self.hops[rabbit] = 2.0 + band.level() * 1.5;
            }
        }
        self.nod = (self.nod - dt * 4.0).max(self.bass.hit());
    }

    /// Whether there is music to dance to: on now, or a moment ago - a
    /// rest in a score is not the end of it.
    fn dancing(&self) -> bool {
        self.level.fast > FOREST_HEARS || self.level.slow > FOREST_HEARS
    }
}

impl ShowroomFx {
    pub(super) fn forest_frame(&self, area: Rect, time: f32) -> SceneFrame {
        let width = f32::from(area.width);
        let height = f32::from(area.height);
        let ground = height - 2.0;
        let floor = ground - 1.0;
        let mut canvas = SceneCanvas {
            area,
            light: (width * 0.85, 1.0),
            background: self.background,
            foreground: self.foreground,
            sprites: std::collections::HashMap::new(),
        };
        let sprite = |glyph: &'static str, colour: Color, bold: bool| SpaceSprite {
            glyph,
            colour,
            bold,
        };
        let felt = &self.forest;
        let dancing = felt.dancing();
        let loudness = felt.level.level();
        let sway = if dancing { felt.mid.level() } else { 0.0 };
        let flutter = if dancing { felt.treble.level() } else { 0.0 };

        // The sun, swelling with the music.
        canvas.disc(
            &SpaceBody {
                x: width * 0.86,
                y: 2.2,
                rx: 2.6 + loudness * 1.2,
                ry: 1.4 + loudness * 0.6,
                colour: self.accent,
                banded: false,
                near: true,
            },
            true,
        );

        // The ground: grass on the first row, earth below.
        let grass = blend_rgb(self.secondary, self.background, 0.4);
        let earth = blend_rgb(self.muted, self.background, 0.6);
        let mut x = 0.0;
        while x < width {
            let tuft = hash_unit(hash(x as u64, 0xf0e5, 0x7));
            let glyph = if tuft < 0.25 {
                "\""
            } else if tuft < 0.45 {
                "'"
            } else if tuft < 0.6 {
                ","
            } else {
                "▁"
            };
            canvas.put(x, ground, sprite(glyph, grass, false), true);
            canvas.put(x, ground + 1.0, sprite("▒", earth, false), true);
            x += 1.0;
        }

        // The trees, spaced across the clearing, each its own height, the
        // canopies swaying with the middle of the music.
        let trees = ((width / 16.0) as usize).clamp(3, 9);
        let bark = blend_rgb(self.accent, self.background, 0.55);
        // Where each canopy is - its middle, its top, its half-width - for
        // a bird to perch on; and where each trunk stands, for the animals
        // to keep off.
        let mut canopies: Vec<(f32, f32, f32)> = Vec::new();
        let mut trunks: Vec<f32> = Vec::new();
        for tree in 0..trees {
            let seed = hash(tree as u64, 0x7ee, 0xf0e5);
            let x = ((tree as f32 + 0.5) * width / trees as f32 + (hash_unit(seed) - 0.5) * 6.0)
                .floor()
                .clamp(1.0, (width - 3.0).max(1.0));
            // `clamp` panics when min is greater than max. On a frame shorter
            // than the canopy floor, hold the cap at the floor and let the
            // canvas clip.
            let trunk_cap = (height - 6.0).max(6.0);
            let h = (height * (0.32 + 0.22 * hash_unit(hash(seed, 1, 2))))
                .clamp(6.0f32.min(trunk_cap), trunk_cap);
            let top = ground - h;
            let rx = (h * 0.5).clamp(3.0, 7.0);
            let ry = (h * 0.27).clamp(2.0, 4.0);
            let mut y = top + ry * 0.5;
            while y < ground {
                canvas.put(x, y, sprite("█", bark, false), true);
                canvas.put(x + 1.0, y, sprite("▓", bark, false), true);
                y += 1.0;
            }
            let lean = (sway * 2.0 * (time * 3.0 + tree as f32).sin()).round();
            // The leaves lit brighter the louder it is.
            let leaves = blend_rgb(
                if tree % 2 == 0 {
                    self.secondary
                } else {
                    blend_rgb(self.secondary, self.foreground, 0.22)
                },
                self.foreground,
                loudness * 0.3,
            );
            canvas.disc(
                &SpaceBody {
                    x: x + 0.5 + lean,
                    y: top,
                    rx,
                    ry,
                    colour: leaves,
                    banded: false,
                    near: true,
                },
                true,
            );
            canopies.push((x + 0.5 + lean, top - ry, rx));
            trunks.push(x);
        }
        // Clear ground for an animal `wide` cells wide, at or after `x`.
        let clear = |x: f32, wide: f32| -> f32 {
            let mut at = x;
            while trunks
                .iter()
                .any(|&trunk| trunk + 1.0 >= at - 1.0 && trunk <= at + wide)
                && at + wide < width - 1.0
            {
                at += 1.0;
            }
            at
        };

        // The flowers, between the trees, swaying too.
        let flowers = ((width / 5.0) as usize).clamp(4, 24);
        let petals = [
            self.accent,
            blend_rgb(self.accent, self.foreground, 0.5),
            self.foreground,
            blend_rgb(self.secondary, self.foreground, 0.5),
        ];
        for flower in 0..flowers {
            let seed = hash(flower as u64, 0xf1, 0x0e5);
            let x = (hash_unit(seed) * (width - 2.0) + 1.0).floor();
            let bend = (sway * 1.6 * (time * 3.0 + flower as f32).sin()).round();
            let glyph = if flower % 3 == 0 { "❀" } else { "✿" };
            canvas.put(x, floor, sprite("│", grass, false), false);
            canvas.put(
                x + bend,
                floor - 1.0,
                sprite(glyph, petals[flower % 4], true),
                true,
            );
        }

        // The animals. Rabbits bound on the beat; a fox nods to it; the
        // birds fly while there is music and perch on the trees when there
        // is none; butterflies flutter to the treble.
        let coat = blend_rgb(self.foreground, self.background, 0.12);
        let shade = blend_rgb(self.foreground, self.background, 0.5);
        let glyph_of = |glyph: char| -> Option<&'static str> {
            Some(match glyph {
                '▌' => "▌",
                '▐' => "▐",
                '▄' => "▄",
                '█' => "█",
                '▀' => "▀",
                '▲' => "▲",
                _ => return None,
            })
        };
        for (rabbit, along) in [0.18f32, 0.47].into_iter().enumerate() {
            let x = clear((width * along).floor(), 4.0);
            let hop = felt.hops[rabbit].round();
            let top = floor - 2.0 - hop;
            let rows: [(&str, Color); 3] = [(" ▌▐ ", coat), ("▄██▄", coat), (" ▀▀ ", shade)];
            for (row, (text, colour)) in rows.into_iter().enumerate() {
                for (column, glyph) in text.chars().enumerate() {
                    if let Some(glyph) = glyph_of(glyph) {
                        canvas.put(
                            x + column as f32,
                            top + row as f32,
                            sprite(glyph, colour, false),
                            true,
                        );
                    }
                }
            }
        }
        let fox_x = clear((width * 0.7).floor(), 4.0);
        let nod = if felt.nod > 0.4 { 1.0 } else { 0.0 };
        let fox_dark = blend_rgb(self.accent, self.background, 0.4);
        let rows: [(&str, Color, f32); 3] = [
            ("▲  ▲", self.accent, -nod),
            ("████", self.accent, -nod),
            ("▐██▌", fox_dark, 0.0),
        ];
        for (row, (text, colour, lift)) in rows.into_iter().enumerate() {
            for (column, glyph) in text.chars().enumerate() {
                if let Some(glyph) = glyph_of(glyph) {
                    canvas.put(
                        fox_x + column as f32,
                        floor - 2.0 + row as f32 + lift,
                        sprite(glyph, colour, false),
                        true,
                    );
                }
            }
        }
        for bird in 0..2u32 {
            let (x, y, glyph) = if dancing {
                let along = fract(time * 0.12 + bird as f32 * 0.5);
                let x = along * (width + 10.0) - 5.0;
                let y = 3.0 + (time * 2.0 + bird as f32 * 2.0).sin() * 2.0 + bird as f32 * 2.0;
                let flap = ((time * (6.0 + flutter * 10.0)) as u32 + bird).is_multiple_of(2);
                (x, y, if flap { "v" } else { "^" })
            } else {
                let (cx, top, _) = canopies[(bird as usize * 2) % canopies.len()];
                (cx, top - 1.0, "v")
            };
            if x >= 0.0 {
                canvas.put(x, y.max(0.0), sprite(glyph, self.foreground, true), true);
            }
        }
        for fly in 0..3u32 {
            let base = width * (0.3 + 0.2 * fly as f32);
            let x = base + (time * 0.7 + fly as f32).sin() * width * 0.12;
            let y = floor
                - 5.0
                - (time * 1.9 + fly as f32 * 2.0).sin() * 2.5
                - flutter * (time * 11.0 + fly as f32).sin() * 1.5;
            let wing = ((time * (5.0 + flutter * 12.0)) as u32 + fly).is_multiple_of(2);
            let colour = if fly == 1 {
                self.foreground
            } else {
                self.accent
            };
            canvas.put(
                x.max(0.0),
                y.max(0.0),
                sprite(if wing { "•" } else { "·" }, colour, wing),
                false,
            );
        }
        // A hit in the treble sets sparks off in the canopies.
        let sparks = (felt.treble.hit() * 12.0) as u32;
        for spark in 0..sparks {
            let seed = hash(u64::from(spark), (time * 8.0) as u64, 0x5a1c);
            let (cx, top, rx) = canopies[spark as usize % canopies.len()];
            let x = cx + (hash_unit(seed) - 0.5) * rx * 2.0;
            let y = top + hash_unit(hash(seed, 1, 0)) * 3.0;
            canvas.put(
                x.max(0.0),
                y.max(0.0),
                sprite("✦", self.foreground, true),
                false,
            );
        }
        SceneFrame {
            sprites: canvas.sprites,
            glow: None,
        }
    }

    pub(super) fn paint_forest(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Spot { x, y, blank, .. } = *spot;
        if let Some(scene) = &frame.forest
            && let Some(sprite) = scene.sprites.get(&(x, y))
        {
            if blank {
                cell.set_symbol(sprite.glyph);
            }
            cell.set_fg(sprite.colour);
            if sprite.bold {
                cell.modifier.insert(Modifier::BOLD);
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

    /// Trees, flowers and animals; the animals keep still while it is
    /// quiet and move to the music when there is some.
    #[test]
    fn the_forest_has_trees_flowers_and_animals_that_dance_to_the_music() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(6, 16, 10);
        let active = showroom_test_visual(CellEffect::Forest, background);
        let signal = Arc::new(ReactiveAudioSignal::default());
        let mut effect = ShowroomFx::new(&active, signal);
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
        let quiet = render(&effect);
        assert!(cells(&quiet, &["✿", "❀"]).len() >= 4, "flowers");
        let trunks = (0..area.width)
            .filter(|&x| (25..=27).all(|y| quiet.cell((x, y)).expect("cell").symbol() == "█"))
            .count();
        assert!(trunks >= 3, "trees: {trunks}");
        assert_eq!(cells(&quiet, &["v"]).len(), 2, "the birds are perched");
        assert!(
            quiet.content.iter().all(|cell| cell.symbol().width() <= 1),
            "nothing wider than a cell"
        );
        effect.elapsed_ms = 1_300;
        let still = render(&effect);
        assert_eq!(
            cells(&quiet, &["v"]),
            cells(&still, &["v"]),
            "still, while it is quiet"
        );

        // Music: the birds take off, the rabbits bound on the hits.
        let audio = ReactiveAudio {
            rms: 0.4,
            bass: 0.6,
            mid: 0.5,
            treble: 0.5,
        };
        for _ in 0..2 {
            effect.forest.advance(0.05, audio);
        }
        let dancing = render(&effect);
        for _ in 0..12 {
            effect.forest.advance(0.05, audio);
        }
        effect.elapsed_ms = 1_600;
        let dancing_later = render(&effect);
        assert_ne!(
            cells(&dancing, &["v", "^"]),
            cells(&dancing_later, &["v", "^"]),
            "the birds fly to the music"
        );
        let ears = |buffer: &Buffer| {
            cells(buffer, &["▌"])
                .into_iter()
                .filter(|&(x, _)| x < 60)
                .collect::<Vec<_>>()
        };
        assert_ne!(ears(&dancing), ears(&dancing_later), "the rabbits bound");
    }
}

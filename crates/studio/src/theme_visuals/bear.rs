//! The bear: asleep against a tree, snoring, until music wakes him with a
//! start. Then he gets up, walks out into the clearing and dances for as
//! long as it plays - one move after another. When it stops, whatever he
//! was doing, he stops too, puzzled, and trudges back to the tree to sit
//! down and sleep.

use super::*;

/// How soft a sound still counts as music to a bear.
const BEAR_HEARS: f32 = 0.015;
/// How long the music has to run to count as music, and how long a silence
/// has to last to count as the music ending - a rest in a score is not.
const BEAR_WAKES_AFTER: f32 = 0.1;
const BEAR_MUSIC_ENDS_AFTER: f32 = 0.7;
/// How long the start lasts, the puzzlement, and the settling down.
const BEAR_STARTLE: f32 = 1.2;
const BEAR_PUZZLED: f32 = 1.6;
const BEAR_SETTLE: f32 = 1.2;
/// Where he dances: this many cells out from his place at the tree.
const BEAR_DANCE_SPOT: f32 = -16.0;
/// Cells per second: out to the dance, and the trudge back to bed.
const BEAR_WALK: f32 = 6.0;
const BEAR_TRUDGE: f32 = 3.0;
/// One dance move's length; four moves make the routine.
const BEAR_MOVE: f32 = 2.4;

/// What the bear is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum Mood {
    #[default]
    Asleep,
    /// Woken with a start: up on his feet, eyes wide.
    Startled { since: f32 },
    /// Walking out to where he dances.
    WalkingOut,
    /// Dancing, one move after another.
    Dancing { since: f32 },
    /// The music stopped: huh?
    Puzzled { since: f32 },
    /// Trudging back to the tree.
    Returning,
    /// Sitting back down, eyes closing.
    Settling { since: f32 },
}

/// The bear, carried from frame to frame.
#[derive(Clone, Debug, Default)]
pub(super) struct BearMemo {
    pub(super) mood: Mood,
    /// Where he stands, in cells from his place at the tree: negative is
    /// out into the clearing.
    pub(super) offset: f32,
    /// How far he has walked, for the legs.
    stride: f32,
    /// How long the music has run for, and how long the quiet has.
    loud_for: f32,
    quiet_for: f32,
}

impl BearMemo {
    /// A little later: `rms` is what he hears, `time` the scene's clock.
    pub(super) fn advance(&mut self, dt: f32, rms: f32, time: f32) {
        if rms > BEAR_HEARS {
            self.loud_for += dt;
            self.quiet_for = 0.0;
        } else {
            self.quiet_for += dt;
            self.loud_for = 0.0;
        }
        let music = self.loud_for >= BEAR_WAKES_AFTER;
        let ended = self.quiet_for >= BEAR_MUSIC_ENDS_AFTER;
        self.mood = match self.mood {
            Mood::Asleep if music => Mood::Startled { since: time },
            Mood::Asleep => Mood::Asleep,
            // Music that stops before the start is over is puzzling too.
            Mood::Startled { .. } if ended => Mood::Puzzled { since: time },
            Mood::Startled { since } if time - since >= BEAR_STARTLE => Mood::WalkingOut,
            Mood::Startled { since } => Mood::Startled { since },
            Mood::WalkingOut if ended => Mood::Puzzled { since: time },
            Mood::WalkingOut => {
                if self.step_toward(BEAR_DANCE_SPOT, BEAR_WALK, dt) {
                    Mood::Dancing { since: time }
                } else {
                    Mood::WalkingOut
                }
            }
            Mood::Dancing { .. } if ended => Mood::Puzzled { since: time },
            Mood::Dancing { since } => Mood::Dancing { since },
            // Music again while he wonders: back to it, from wherever he is.
            Mood::Puzzled { .. } if music => {
                if self.offset <= BEAR_DANCE_SPOT + 0.5 {
                    Mood::Dancing { since: time }
                } else {
                    Mood::WalkingOut
                }
            }
            Mood::Puzzled { since } if time - since >= BEAR_PUZZLED => Mood::Returning,
            Mood::Puzzled { since } => Mood::Puzzled { since },
            Mood::Returning if music => Mood::WalkingOut,
            Mood::Returning => {
                if self.step_toward(0.0, BEAR_TRUDGE, dt) {
                    Mood::Settling { since: time }
                } else {
                    Mood::Returning
                }
            }
            Mood::Settling { .. } if music => Mood::WalkingOut,
            Mood::Settling { since } if time - since >= BEAR_SETTLE => Mood::Asleep,
            Mood::Settling { since } => Mood::Settling { since },
        };
    }

    /// Walk toward `target` at `speed`; true once there.
    fn step_toward(&mut self, target: f32, speed: f32, dt: f32) -> bool {
        let gap = target - self.offset;
        let step = speed * dt;
        if gap.abs() <= step {
            self.offset = target;
            return true;
        }
        self.offset += step * gap.signum();
        self.stride += step;
        false
    }
}

/// How the arms are held.
#[derive(Clone, Copy, PartialEq)]
enum Arms {
    Down,
    Up,
    Out,
}

/// What the eyes and mouth are doing.
#[derive(Clone, Copy, PartialEq)]
enum Face {
    Asleep,
    Awake,
    Wide,
    Dozing,
}

impl ShowroomFx {
    /// The clearing at night: a tree, the bear, and what the music has him
    /// doing.
    pub(super) fn bear_frame(&self, area: Rect, time: f32, reactive: ReactiveAudio) -> SceneFrame {
        let width = f32::from(area.width);
        let height = f32::from(area.height);
        let ground = height - 2.0;
        let floor = ground - 1.0;
        let tree_x = (width * 0.62).floor().max(12.0);
        let mut canvas = SceneCanvas {
            area,
            light: (width * 0.15, 0.0),
            background: self.background,
            foreground: self.foreground,
            sprites: std::collections::HashMap::new(),
        };
        let sprite = |glyph: &'static str, colour: Color, bold: bool| SpaceSprite {
            glyph,
            colour,
            bold,
        };

        // The night: stars, and a moon low on the left.
        let star = blend_rgb(self.foreground, self.background, 0.55);
        let mut y = 0.0;
        while y < ground - 10.0 {
            let mut x = 0.0;
            while x < width {
                if hash_unit(hash(x as u64, y as u64, 0xbea2)) < 0.012 {
                    let twinkle = (time * 1.7 + x * 0.3 + y).sin() > 0.3;
                    canvas.put(
                        x,
                        y,
                        sprite(if twinkle { "✦" } else { "·" }, star, false),
                        false,
                    );
                }
                x += 1.0;
            }
            y += 1.0;
        }
        canvas.disc(
            &SpaceBody {
                x: width * 0.14,
                y: 2.5,
                rx: 2.4,
                ry: 1.3,
                colour: blend_rgb(self.foreground, self.background, 0.15),
                banded: false,
                near: true,
            },
            true,
        );

        // The ground: grass on the first row, earth below.
        let grass = blend_rgb(self.secondary, self.background, 0.45);
        let earth = blend_rgb(self.muted, self.background, 0.6);
        let mut x = 0.0;
        while x < width {
            let tuft = hash_unit(hash(x as u64, 0xbea2, 0x9ead));
            let glyph = if tuft < 0.2 {
                "\""
            } else if tuft < 0.4 {
                "'"
            } else if tuft < 0.55 {
                ","
            } else {
                "▁"
            };
            canvas.put(x, ground, sprite(glyph, grass, false), true);
            canvas.put(x, ground + 1.0, sprite("▒", earth, false), true);
            x += 1.0;
        }

        // The tree: a trunk three cells wide, a canopy over it.
        let bark = blend_rgb(self.accent, self.background, 0.55);
        let trunk_top = (ground - 14.0).max(4.0);
        let mut y = trunk_top;
        while y < ground {
            for dx in 0..3u64 {
                let grain = hash_unit(hash(dx, y as u64, 0x7ee)) < 0.25;
                canvas.put(
                    tree_x + dx as f32,
                    y,
                    sprite(if grain { "▓" } else { "█" }, bark, false),
                    true,
                );
            }
            y += 1.0;
        }
        canvas.disc(
            &SpaceBody {
                x: tree_x + 1.0,
                y: trunk_top - 1.5,
                rx: (width * 0.11).clamp(6.0, 13.0),
                ry: (height * 0.14).clamp(3.0, 5.0),
                colour: self.secondary,
                banded: false,
                near: true,
            },
            true,
        );

        // The bear: where he is and what he is doing.
        let home = tree_x - 8.0;
        let bx = (home + self.bear.offset).max(5.0).round();
        let fur = self.accent;
        let dark = blend_rgb(fur, self.background, 0.4);
        let belly = blend_rgb(fur, self.foreground, 0.35);
        let body = |x: f32, y: f32, rx: f32, ry: f32, colour: Color| SpaceBody {
            x,
            y,
            rx,
            ry,
            colour,
            banded: false,
            near: true,
        };
        let beat = (time * 4.0).sin() > 0.0;
        let mood = self.bear.mood;
        let hears = reactive.rms > BEAR_HEARS;

        match mood {
            Mood::Asleep | Mood::Settling { .. } => {
                // Sat against the trunk: a body, a belly, two feet, a head
                // leaning back on the bark.
                canvas.disc(&body(bx, floor - 2.4, 6.0, 3.2, fur), true);
                canvas.disc(&body(bx - 0.5, floor - 1.9, 2.8, 1.6, belly), true);
                for foot in [-4.0, 1.5] {
                    for dx in 0..3 {
                        canvas.put(bx + foot + dx as f32, floor, sprite("▄", dark, false), true);
                    }
                }
                let head_y = floor - 6.6;
                let hx = bx + 2.0;
                let face = match mood {
                    Mood::Settling { since } if time - since < BEAR_SETTLE * 0.5 => Face::Dozing,
                    _ => Face::Asleep,
                };
                self.bear_head(&mut canvas, hx, head_y, 0.0, face, fur, dark, belly);
                if mood == Mood::Asleep {
                    // The zees, rising from the nose and drifting off, over
                    // and over.
                    let phase = fract(time / 2.6);
                    for (index, glyph) in ["z", "Z", "z"].into_iter().enumerate() {
                        let start = index as f32 / 3.0;
                        if phase > start {
                            let age = phase - start;
                            let dim = blend_rgb(self.foreground, self.background, 0.35 + age * 0.4);
                            canvas.put(
                                hx + 4.0 + index as f32 * 2.0 + (age * 4.0).floor().min(1.0),
                                head_y - 1.5 - index as f32 * 1.5 - (age * 3.0).floor().min(1.0),
                                sprite(glyph, dim, index == 1),
                                false,
                            );
                        }
                    }
                }
            }
            Mood::Startled { .. } => {
                let head_y = self.bear_standing(
                    &mut canvas,
                    bx,
                    floor,
                    0.0,
                    Arms::Up,
                    0.0,
                    fur,
                    dark,
                    belly,
                );
                self.bear_head(&mut canvas, bx, head_y, 0.0, Face::Wide, fur, dark, belly);
                canvas.put(bx, head_y - 4.0, sprite("!", self.foreground, true), true);
            }
            Mood::WalkingOut | Mood::Returning => {
                let face = if mood == Mood::Returning {
                    Face::Dozing
                } else {
                    Face::Awake
                };
                let stride = self.bear.stride;
                let head_y = self.bear_standing(
                    &mut canvas,
                    bx,
                    floor,
                    0.0,
                    Arms::Down,
                    stride,
                    fur,
                    dark,
                    belly,
                );
                let look = if mood == Mood::Returning { 1.0 } else { -1.0 };
                self.bear_head(&mut canvas, bx, head_y, look, face, fur, dark, belly);
            }
            Mood::Dancing { since } => {
                // The routine: four moves, round and round, on the beat.
                let into = time - since;
                let step = ((into / BEAR_MOVE) as u32) % 4;
                let within = fract(into / BEAR_MOVE);
                let (arms, dx, dy, look, face, stride) = match step {
                    // Arms up, bouncing.
                    0 => (
                        Arms::Up,
                        0.0,
                        if beat { -1.0 } else { 0.0 },
                        0.0,
                        Face::Awake,
                        0.0,
                    ),
                    // The twist: arms out, hips and head one way then the other.
                    1 => {
                        let side = if beat { 1.0 } else { -1.0 };
                        (Arms::Out, side, 0.0, side, Face::Awake, 0.0)
                    }
                    // The moonwalk: gliding out and back, feet going.
                    2 => (
                        Arms::Down,
                        ((within * std::f32::consts::TAU).sin() * 3.0).round(),
                        0.0,
                        -1.0,
                        Face::Awake,
                        into * 6.0,
                    ),
                    // Jumps: every beat, two rows off the ground, eyes wide.
                    _ => {
                        if beat {
                            (Arms::Up, 0.0, -2.0, 0.0, Face::Wide, 0.0)
                        } else {
                            (Arms::Out, 0.0, 0.0, 0.0, Face::Awake, 0.0)
                        }
                    }
                };
                let head_y = self.bear_standing(
                    &mut canvas,
                    bx + dx,
                    floor,
                    dy,
                    arms,
                    stride,
                    fur,
                    dark,
                    belly,
                );
                self.bear_head(&mut canvas, bx + dx, head_y, look, face, fur, dark, belly);
                // The music around him.
                for note in 0..3u32 {
                    let phase = fract(time * 0.5 + note as f32 / 3.0);
                    let x =
                        bx + (note as f32 - 1.0) * 5.0 + (phase * 6.0 + note as f32).sin() * 2.0;
                    let y = head_y - 2.0 - phase * 6.0;
                    let glyph = if note.is_multiple_of(2) { "♪" } else { "♫" };
                    canvas.put(
                        x,
                        y.max(0.0),
                        sprite(
                            glyph,
                            blend_rgb(self.foreground, self.background, phase * 0.7),
                            true,
                        ),
                        false,
                    );
                }
            }
            Mood::Puzzled { .. } => {
                // Huh? Standing still, looking one way and the other.
                let look = ((time * 1.5).sin() * 1.5).round();
                let head_y = self.bear_standing(
                    &mut canvas,
                    bx,
                    floor,
                    0.0,
                    Arms::Down,
                    0.0,
                    fur,
                    dark,
                    belly,
                );
                self.bear_head(&mut canvas, bx, head_y, look, Face::Wide, fur, dark, belly);
                canvas.put(bx, head_y - 4.0, sprite("?", self.foreground, true), true);
            }
        }

        // What he hears, drifting in from the right for as long as it
        // plays and he is not the one making a show of it.
        if hears && !matches!(mood, Mood::Dancing { .. }) {
            for note in 0..4u32 {
                let along = fract(time * 0.3 + note as f32 * 0.27);
                let x = width - 3.0 - along * width * 0.42;
                let y = trunk_top - 4.0 + (along * 9.0 + note as f32).sin() * 1.6;
                let glyph = if note.is_multiple_of(2) { "♪" } else { "♫" };
                canvas.put(
                    x,
                    y.max(0.0),
                    sprite(
                        glyph,
                        blend_rgb(self.foreground, self.background, 0.15 + along * 0.6),
                        true,
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

    /// The bear on his feet at `bx`, feet on `floor`, lifted `dy` rows:
    /// body, belly, arms as held, legs mid-stride. Returns the row of the
    /// head's middle for [`Self::bear_head`].
    #[allow(clippy::too_many_arguments)]
    fn bear_standing(
        &self,
        canvas: &mut SceneCanvas,
        bx: f32,
        floor: f32,
        dy: f32,
        arms: Arms,
        stride: f32,
        fur: Color,
        dark: Color,
        belly: Color,
    ) -> f32 {
        let sprite = |glyph: &'static str, colour: Color, bold: bool| SpaceSprite {
            glyph,
            colour,
            bold,
        };
        let body = |x: f32, y: f32, rx: f32, ry: f32, colour: Color| SpaceBody {
            x,
            y,
            rx,
            ry,
            colour,
            banded: false,
            near: true,
        };
        let head_y = floor - 8.6 + dy;
        let chest = head_y + 4.4;
        // Legs first, so the body sits over them; a stride shifts the
        // near leg forward and the far one back.
        let step = ((stride * 1.2) as u32).is_multiple_of(2);
        let (left, right) = if stride > 0.0 && step {
            (bx - 3.0, bx + 1.0)
        } else {
            (bx - 2.0, bx + 2.0)
        };
        for leg in [left, right] {
            canvas.put(leg, floor - 1.0 + dy, sprite("█", dark, false), true);
            canvas.put(leg, floor + dy, sprite("█", dark, false), true);
        }
        canvas.disc(&body(bx, chest, 3.8, 2.7, fur), true);
        canvas.disc(&body(bx, chest + 0.3, 2.0, 1.5, belly), true);
        match arms {
            Arms::Down => {
                for row in 0..3 {
                    canvas.put(
                        bx - 4.0,
                        chest - 1.0 + row as f32,
                        sprite("▐", dark, false),
                        true,
                    );
                    canvas.put(
                        bx + 4.0,
                        chest - 1.0 + row as f32,
                        sprite("▌", dark, false),
                        true,
                    );
                }
            }
            Arms::Up => {
                for row in 0..4 {
                    canvas.put(
                        bx - 4.0,
                        chest - 1.0 - row as f32,
                        sprite("█", dark, false),
                        true,
                    );
                    canvas.put(
                        bx + 4.0,
                        chest - 1.0 - row as f32,
                        sprite("█", dark, false),
                        true,
                    );
                }
                canvas.put(bx - 4.0, chest - 5.0, sprite("▄", dark, false), true);
                canvas.put(bx + 4.0, chest - 5.0, sprite("▄", dark, false), true);
            }
            Arms::Out => {
                for reach in 1..4 {
                    canvas.put(
                        bx - 3.0 - reach as f32,
                        chest - 1.0,
                        sprite("▀", dark, false),
                        true,
                    );
                    canvas.put(
                        bx + 3.0 + reach as f32,
                        chest - 1.0,
                        sprite("▀", dark, false),
                        true,
                    );
                }
            }
        }
        head_y
    }

    /// The head at `(hx, head_y)`: ears, face, muzzle, nose; the eyes
    /// looking `look` cells aside.
    #[allow(clippy::too_many_arguments)]
    fn bear_head(
        &self,
        canvas: &mut SceneCanvas,
        hx: f32,
        head_y: f32,
        look: f32,
        face: Face,
        fur: Color,
        dark: Color,
        belly: Color,
    ) {
        let sprite = |glyph: &'static str, colour: Color, bold: bool| SpaceSprite {
            glyph,
            colour,
            bold,
        };
        let body = |x: f32, y: f32, rx: f32, ry: f32, colour: Color| SpaceBody {
            x,
            y,
            rx,
            ry,
            colour,
            banded: false,
            near: true,
        };
        for ear in [-2.6, 2.6] {
            canvas.disc(&body(hx + ear, head_y - 1.8, 1.1, 0.7, dark), true);
        }
        canvas.disc(&body(hx, head_y, 3.6, 2.1, fur), true);
        canvas.disc(&body(hx + 0.6, head_y + 0.9, 1.7, 0.8, belly), true);
        canvas.put(hx + 0.6, head_y + 0.4, sprite("●", dark, true), true);
        let (eye, open) = match face {
            Face::Asleep => ("─", false),
            Face::Dozing => ("─", false),
            Face::Awake => ("●", true),
            Face::Wide => ("○", true),
        };
        let eye_colour = if open { self.foreground } else { dark };
        for ex in [-1.6, 1.6] {
            canvas.put(
                hx + ex + look,
                head_y - 0.7,
                sprite(eye, eye_colour, open),
                true,
            );
        }
        if face == Face::Wide {
            canvas.put(hx + 0.6, head_y + 1.5, sprite("○", dark, true), true);
        } else if face == Face::Dozing {
            canvas.put(hx + 0.6, head_y + 1.5, sprite("~", dark, true), true);
        }
    }

    pub(super) fn paint_bear(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Spot { x, y, blank, .. } = *spot;
        if let Some(scene) = &frame.bear
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

    /// Asleep he snores. Music wakes him with a start; he walks out and
    /// dances for as long as it plays. When it stops he is puzzled, then
    /// trudges back to the tree, sits, and is snoring again.
    #[test]
    fn the_bear_wakes_dances_and_goes_back_to_sleep_when_the_music_stops() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(7, 10, 22);
        let active = showroom_test_visual(CellEffect::Bear, background);
        let signal = Arc::new(ReactiveAudioSignal::default());
        let mut effect = ShowroomFx::new(&active, Arc::clone(&signal));
        let scale = 0.45 + 58.0 / 80.0;
        let cells = |buffer: &Buffer, glyphs: &[&str]| {
            (0..area.height)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| glyphs.contains(&buffer.cell((x, y)).expect("cell").symbol()))
                .collect::<Vec<_>>()
        };
        // The scene's clock, driven a frame at a time.
        let mut clock = 2.0f32;
        let mut play = |effect: &mut ShowroomFx, rms: f32, seconds: f32| {
            let frames = (seconds / 0.05).round() as usize;
            for _ in 0..frames {
                clock += 0.05;
                effect.bear.advance(0.05, rms, clock);
            }
            effect.elapsed_ms = (clock * 1000.0 / scale) as u64;
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            effect.render(&mut buffer, area);
            buffer
        };

        let asleep = play(&mut effect, 0.0, 0.5);
        assert!(!cells(&asleep, &["z", "Z"]).is_empty(), "snoring");
        assert_eq!(cells(&asleep, &["●"]).len(), 1, "eyes shut: only the nose");
        assert!(
            asleep.content.iter().all(|cell| cell.symbol().width() <= 1),
            "nothing wider than a cell"
        );

        // Music: a start, on his feet, eyes wide.
        let startled = play(&mut effect, 0.3, 0.3);
        assert!(matches!(effect.bear.mood, Mood::Startled { .. }));
        assert!(!cells(&startled, &["!"]).is_empty(), "surprised");
        assert!(cells(&startled, &["○"]).len() >= 2, "eyes wide");
        assert!(cells(&startled, &["z", "Z"]).is_empty(), "no snoring awake");

        // Then out into the clearing, and dancing.
        play(&mut effect, 0.3, 1.2);
        assert_eq!(effect.bear.mood, Mood::WalkingOut);
        assert!(effect.bear.offset < 0.0, "on his way out");
        let dancing = play(&mut effect, 0.3, 3.5);
        assert!(matches!(effect.bear.mood, Mood::Dancing { .. }));
        assert_eq!(effect.bear.offset, BEAR_DANCE_SPOT);
        assert!(
            !cells(&dancing, &["♪", "♫"]).is_empty(),
            "the music he dances to"
        );
        let later = play(&mut effect, 0.3, 2.5);
        assert_ne!(
            cells(&dancing, &["█"]),
            cells(&later, &["█"]),
            "the moves change"
        );

        // The music stops: huh? Then the slow way back, and sleep.
        let puzzled = play(&mut effect, 0.0, 1.0);
        assert!(matches!(effect.bear.mood, Mood::Puzzled { .. }));
        assert!(!cells(&puzzled, &["?"]).is_empty(), "huh?");
        play(&mut effect, 0.0, 1.6);
        assert_eq!(effect.bear.mood, Mood::Returning);
        let returning = play(&mut effect, 0.0, 1.0);
        assert!(effect.bear.offset > BEAR_DANCE_SPOT, "on his way back");
        assert!(cells(&returning, &["?", "!"]).is_empty());
        let again = play(&mut effect, 0.0, 8.0);
        assert_eq!(effect.bear.mood, Mood::Asleep);
        assert_eq!(effect.bear.offset, 0.0, "back at the tree");
        assert!(!cells(&again, &["z", "Z"]).is_empty(), "snoring again");

        // Music while he is on his way back turns him around.
        play(&mut effect, 0.3, 0.3);
        play(&mut effect, 0.3, 1.3);
        play(&mut effect, 0.0, 2.8);
        assert_eq!(effect.bear.mood, Mood::Returning);
        play(&mut effect, 0.3, 0.3);
        assert_eq!(effect.bear.mood, Mood::WalkingOut, "back to the dance");
    }
}

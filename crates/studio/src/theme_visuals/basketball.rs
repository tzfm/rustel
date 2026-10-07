//! The basketball court: a big player, a hoop built to their size, a ball
//! with some physics, a throw that is fetched and thrown again.

use super::*;

/// A throw: the ball leaves the hands at `COURT_RELEASE`; after it comes to
/// rest the player walks to it, picks it up, walks home and shoots again.
pub(super) const COURT_WINDUP: f32 = 0.5;
pub(super) const COURT_RELEASE: f32 = 1.0;
/// Rows per second squared. Cells are tall, so this is gentle.
pub(super) const COURT_GRAVITY: f32 = 16.0;
/// Cells per second, walking.
pub(super) const COURT_WALK: f32 = 14.0;
pub(super) const COURT_PICKUP: f32 = 0.35;
pub(super) const COURT_BREATH: f32 = 0.3;
/// How long the player jumps for joy after a swish, before going for the
/// ball.
pub(super) const COURT_CHEER: f32 = 1.4;
/// Cells from the player to the board: a free throw and a bit, for a
/// player eight rows tall in cells twice as tall as they are wide. The
/// pane's width does not change it; a pane too narrow for it brings the
/// hoop nearer.
pub(super) const COURT_RANGE: f32 = 48.0;
/// A ball that will not settle is settled: the throw ends here regardless.
pub(super) const COURT_LONGEST_FLIGHT: f32 = 9.0;

/// Where the court's fixtures are, in cells.
#[derive(Clone, Copy, Debug)]
pub(super) struct Court {
    /// Where the player stands to shoot.
    pub(super) home_x: f32,
    /// The row of the top of the player's head.
    pub(super) head_y: f32,
    /// The line the floor is drawn on; the ball rests one row above it.
    pub(super) line_y: f32,
    pub(super) floor: f32,
    pub(super) board_x: f32,
    pub(super) rim_left: f32,
    pub(super) rim_x: f32,
    pub(super) rim_y: f32,
}

impl Court {
    pub(super) fn over(width: f32, height: f32) -> Self {
        let line_y = height - 2.0;
        let head_y = line_y - 8.0;
        // The player and the hoop, `COURT_RANGE` apart, centred in the pane.
        let span = COURT_RANGE.min(width - 10.0).max(12.0);
        let home_x = ((width - span) / 2.0).floor().max(3.0);
        let board_x = (home_x + span).min(width - 3.0);
        Self {
            home_x,
            head_y,
            line_y,
            floor: line_y - 1.0,
            board_x,
            rim_left: board_x - 10.0,
            rim_x: board_x - 5.0,
            // A ten-foot rim over a player of six and a half: half a player
            // above the head.
            rim_y: (head_y - 4.0).max(2.0),
        }
    }
}

/// What one throw turned out to be, decided once so any moment of it
/// draws the same: whether it went in, when and where the ball came to
/// rest, and how long the whole thing takes with the walk to fetch it.
#[derive(Clone, Copy, Debug)]
pub(super) struct ThrowPlan {
    pub(super) rest_t: f32,
    pub(super) rest_x: f32,
    /// When the player sets off for the ball: once it rests, and once the
    /// jumping for joy is done if it went in.
    pub(super) fetch_t: f32,
    pub(super) length: f32,
}

impl ThrowPlan {
    pub(super) fn walk(&self, court: &Court) -> f32 {
        (self.rest_x - court.home_x).abs() / COURT_WALK
    }
}

/// The ball at a moment of a throw.
pub(super) struct Ball {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) vx: f32,
    pub(super) vy: f32,
    pub(super) in_net: bool,
    /// When it hit the rim or the board, if it did.
    pub(super) clank_at: Option<f32>,
    /// When it went through, if it did.
    pub(super) swish_at: Option<f32>,
    /// At rest on the floor.
    pub(super) rested: bool,
}

/// What the player is doing.
pub(super) enum Pose {
    Idle,
    WindUp,
    Jump,
    /// Jumping for joy after a swish: arms up, off the ground on alternate
    /// frames.
    Cheer {
        up: bool,
    },
    Walk {
        step: bool,
    },
    PickUp,
}

/// The throws already worked out, so a frame finds its throw by a lookup
/// rather than by replaying every throw since the theme was chosen.
#[derive(Clone, Debug, Default)]
pub(super) struct CourtMemo {
    pub(super) area: Option<Rect>,
    /// `(start time, plan)`, in order.
    pub(super) plans: Vec<(f32, ThrowPlan)>,
}

impl ShowroomFx {
    /// Whether throw `throw` goes in. Decided once per throw, so a frame
    /// can be drawn from any moment of it.
    pub(super) fn throw_made(throw: u64) -> bool {
        hash(throw, 0xba11, 0x05c0) % 100 < 58
    }

    /// Where the ball is `t` seconds into throw `throw`, and what has
    /// happened to it: cheap physics - gravity, a rim, a backboard, a floor
    /// with some bounce in it, the court's own edges - run from the release
    /// each time it is asked, which is what keeps every frame the same for
    /// the same moment. Before the release, in the hands.
    pub(super) fn ball_at(court: &Court, width: f32, throw: u64, t: f32) -> Ball {
        let made = Self::throw_made(throw);
        let off_board = hash(throw, 0x0b0a, 0x0a11).is_multiple_of(2);
        let (x0, y0) = (court.home_x, court.head_y - 2.0);
        let held = |x: f32, y: f32| Ball {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            in_net: false,
            clank_at: None,
            swish_at: None,
            rested: false,
        };
        if t < COURT_WINDUP {
            return held(court.home_x + 2.0, court.head_y + 4.0);
        }
        if t < COURT_RELEASE {
            return held(x0, y0);
        }
        // Aim: a make flies to just over the rim's centre; a miss to the
        // front lip, or high off the board.
        let (tx, ty, flight) = if made {
            (court.rim_x, court.rim_y - 0.5, 1.6)
        } else if off_board {
            (court.board_x - 0.5, court.rim_y - 3.0, 1.45)
        } else {
            (court.rim_left - 0.5, court.rim_y - 0.2, 1.55)
        };
        let g = COURT_GRAVITY;
        let mut ball = Ball {
            x: x0,
            y: y0,
            vx: (tx - x0) / flight,
            vy: (ty - y0 - 0.5 * g * flight * flight) / flight,
            in_net: false,
            clank_at: None,
            swish_at: None,
            rested: false,
        };
        let dt = 1.0 / 120.0;
        let mut now = COURT_RELEASE;
        while now < t && !ball.rested {
            now += dt;
            ball.vy += g * dt;
            ball.x += ball.vx * dt;
            ball.y += ball.vy * dt;
            if ball.in_net {
                // Through the net: held to the middle and slowed until it
                // is out the bottom.
                ball.x = court.rim_x;
                ball.vx = 0.0;
                if ball.y < court.rim_y + 4.5 {
                    ball.vy = ball.vy.min(3.0);
                } else {
                    ball.in_net = false;
                }
            } else {
                if made
                    && ball.vy > 0.0
                    && ball.y >= court.rim_y
                    && ball.y < court.rim_y + 1.0
                    && (ball.x - court.rim_x).abs() <= 3.5
                {
                    ball.in_net = true;
                    ball.swish_at.get_or_insert(now);
                }
                // The front lip, from above: back the way it came.
                if !made
                    && ball.vy > 0.0
                    && (ball.x - court.rim_left).abs() < 0.9
                    && (ball.y - court.rim_y).abs() < 0.7
                {
                    ball.vy = -ball.vy * 0.5;
                    ball.vx = -ball.vx.abs() * 0.35;
                    ball.clank_at.get_or_insert(now);
                }
                // The board: back off it, most of the speed kept.
                if ball.x >= court.board_x - 1.0
                    && ball.y >= court.rim_y - 5.0
                    && ball.y <= court.rim_y + 1.0
                {
                    ball.x = court.board_x - 1.0;
                    ball.vx = -ball.vx.abs() * 0.55;
                    ball.clank_at.get_or_insert(now);
                }
                // The pole under the board, lower down.
                if ball.x >= court.board_x - 1.0 && ball.y > court.rim_y + 1.0 {
                    ball.x = court.board_x - 1.0;
                    ball.vx = -ball.vx.abs() * 0.5;
                }
            }
            if ball.y >= court.floor {
                ball.y = court.floor;
                ball.vy = -ball.vy * 0.42;
                ball.vx *= 0.7;
                if ball.vy.abs() < 1.2 {
                    ball.vy = 0.0;
                    ball.vx *= 0.9;
                    if ball.vx.abs() < 0.3 {
                        ball.vx = 0.0;
                        ball.rested = true;
                    }
                }
            }
            // The court's own edges: the ball stays on it.
            if ball.x < 1.0 {
                ball.x = 1.0;
                ball.vx = ball.vx.abs() * 0.5;
            }
            if ball.x > width - 3.0 {
                ball.x = width - 3.0;
                ball.vx = -ball.vx.abs() * 0.5;
            }
        }
        ball
    }

    /// Everything about throw `throw`, worked out by running it.
    pub(super) fn plan_throw(court: &Court, width: f32, throw: u64) -> ThrowPlan {
        let mut rest_t = COURT_LONGEST_FLIGHT;
        let mut probe = COURT_RELEASE;
        while probe < COURT_LONGEST_FLIGHT {
            probe += 0.1;
            if Self::ball_at(court, width, throw, probe).rested {
                rest_t = probe;
                break;
            }
        }
        let end = Self::ball_at(court, width, throw, rest_t);
        let fetch_t = end
            .swish_at
            .map_or(rest_t, |at| rest_t.max(at + COURT_CHEER));
        let plan = ThrowPlan {
            rest_t,
            rest_x: end.x,
            fetch_t,
            length: 0.0,
        };
        let walk = plan.walk(court);
        ThrowPlan {
            length: fetch_t + walk + COURT_PICKUP + walk + COURT_BREATH,
            ..plan
        }
    }

    /// The throw under way at `time`: its index, its plan, and how far into
    /// it we are. Reads the memo as far as it goes and works out the rest.
    pub(super) fn throw_at(&self, court: &Court, width: f32, time: f32) -> (u64, ThrowPlan, f32) {
        let mut start = 0.0;
        let mut throw = 0u64;
        if self.court.area.is_some_and(|area| {
            (f32::from(area.width) - width).abs() < 0.5
                && (f32::from(area.height) - (court.line_y + 2.0)).abs() < 0.5
        }) {
            for &(at, plan) in &self.court.plans {
                if at + plan.length > time {
                    return (throw, plan, time - at);
                }
                start = at + plan.length;
                throw += 1;
            }
        }
        loop {
            let plan = Self::plan_throw(court, width, throw);
            if start + plan.length > time {
                return (throw, plan, time - start);
            }
            start += plan.length;
            throw += 1;
        }
    }

    /// Work the throws out ahead of the frame that needs them, so a frame
    /// never replays more than one.
    pub(super) fn extend_court(&mut self, area: Rect, time: f32) {
        if self.court.area != Some(area) {
            self.court = CourtMemo {
                area: Some(area),
                plans: Vec::new(),
            };
        }
        let court = Court::over(f32::from(area.width), f32::from(area.height));
        let width = f32::from(area.width);
        let mut end = self
            .court
            .plans
            .last()
            .map_or(0.0, |(at, plan)| at + plan.length);
        while end <= time + 1.0 && self.court.plans.len() < 4096 {
            let throw = self.court.plans.len() as u64;
            let plan = Self::plan_throw(&court, width, throw);
            self.court.plans.push((end, plan));
            end += plan.length;
        }
    }

    /// The court: the player, the ball, the hoop and what it did to the
    /// ball, the score so far.
    pub(super) fn basketball_frame(&self, area: Rect, time: f32) -> SceneFrame {
        let width = f32::from(area.width);
        let height = f32::from(area.height);
        let court = Court::over(width, height);
        let mut canvas = SceneCanvas {
            area,
            light: (width * 0.5, 0.0),
            background: self.background,
            foreground: self.foreground,
            sprites: std::collections::HashMap::new(),
        };
        let sprite = |glyph: &'static str, colour: Color, bold: bool| SpaceSprite {
            glyph,
            colour,
            bold,
        };
        let (throw, plan, t) = self.throw_at(&court, width, time);
        let ball = Self::ball_at(&court, width, throw, t.min(plan.rest_t));

        // The floor: the line, and the boards below it.
        let wood = blend_rgb(self.accent, self.background, 0.22);
        let mut x = 0.0;
        while x < width {
            canvas.put(x, court.line_y, sprite("═", self.muted, false), true);
            canvas.put(x, court.line_y + 1.0, sprite("▒", wood, false), true);
            x += 1.0;
        }
        // The hoop: board, rim, net and the pole holding it up.
        let board = blend_rgb(self.foreground, self.background, 0.2);
        for dy in -4..=1 {
            canvas.put(
                court.board_x,
                court.rim_y + dy as f32,
                sprite("█", board, false),
                true,
            );
        }
        let mut y = court.rim_y + 2.0;
        while y < court.line_y {
            canvas.put(court.board_x, y, sprite("│", self.muted, false), true);
            y += 1.0;
        }
        let mut x = court.rim_left;
        while x < court.board_x {
            canvas.put(x, court.rim_y, sprite("─", self.accent, true), true);
            x += 1.0;
        }
        // The net narrows down four rows, and swings for a moment after a
        // swish.
        let swinging = ball
            .swish_at
            .is_some_and(|at| t - at < 0.8 && (((t - at) * 12.0) as u32).is_multiple_of(2));
        let net = blend_rgb(self.foreground, self.background, 0.35);
        for (row, inset) in [(1.0, 0.0), (2.0, 1.0), (3.0, 2.0), (4.0, 3.0)] {
            let mut x = court.rim_left + inset;
            let mut left = swinging;
            while x < court.board_x - inset {
                canvas.put(
                    x,
                    court.rim_y + row,
                    sprite(if left { "╲" } else { "╱" }, net, false),
                    true,
                );
                left = !left;
                x += 1.0;
            }
        }

        // The player: where they are and what they are doing.
        let walk = plan.walk(&court);
        let fetch_start = plan.fetch_t;
        let pick_start = fetch_start + walk;
        let back_start = pick_start + COURT_PICKUP;
        let home_again = back_start + walk;
        let toward = if plan.rest_x >= court.home_x {
            1.0
        } else {
            -1.0
        };
        let (px, pose, carrying) = if t < COURT_WINDUP {
            (court.home_x, Pose::Idle, true)
        } else if t < COURT_RELEASE {
            (court.home_x, Pose::WindUp, true)
        } else if t < COURT_RELEASE + 0.4 {
            (court.home_x, Pose::Jump, false)
        } else if let Some(at) = ball.swish_at.filter(|&at| t >= at && t < at + COURT_CHEER) {
            let up = (((t - at) * 5.0) as u32).is_multiple_of(2);
            (court.home_x, Pose::Cheer { up }, false)
        } else if t < fetch_start {
            (court.home_x, Pose::Idle, false)
        } else if t < pick_start {
            let gone = (t - fetch_start) * COURT_WALK * toward;
            let step = ((t * 6.0) as u32).is_multiple_of(2);
            (court.home_x + gone, Pose::Walk { step }, false)
        } else if t < back_start {
            (plan.rest_x - 2.0 * toward, Pose::PickUp, false)
        } else if t < home_again {
            let left = (home_again - t) * COURT_WALK * toward;
            let step = ((t * 6.0) as u32).is_multiple_of(2);
            (court.home_x + left, Pose::Walk { step }, true)
        } else {
            (court.home_x, Pose::Idle, true)
        };
        let px = px.round();
        let head = court.head_y
            - if matches!(pose, Pose::Jump | Pose::Cheer { up: true }) {
                1.0
            } else {
                0.0
            };
        let skin = self.foreground;
        let shirt = self.secondary;
        let shorts = blend_rgb(self.secondary, self.background, 0.45);
        let legs = blend_rgb(self.foreground, self.background, 0.3);
        // Five columns around `px`, rows down from the head, as rows of
        // glyphs; a space is nothing. The part picks the colour.
        let (rows, top): (&[(&str, u8)], f32) = match pose {
            Pose::Idle => (
                &[
                    (" ▄▄▄ ", 0),
                    (" ███ ", 0),
                    ("  █  ", 0),
                    ("▄███▄", 1),
                    ("▐███▌", 1),
                    (" ███ ", 2),
                    (" █ █ ", 3),
                    (" █ █ ", 3),
                ],
                0.0,
            ),
            Pose::WindUp | Pose::Jump => (
                &[
                    ("▌   ▐", 0),
                    ("▌▄▄▄▐", 0),
                    ("▐███▌", 0),
                    (" ▀█▀ ", 0),
                    (" ███ ", 1),
                    (" ███ ", 1),
                    (" ███ ", 2),
                    (" █ █ ", 3),
                    (" █ █ ", 3),
                ],
                -1.0,
            ),
            Pose::Cheer { up: true } => (
                &[
                    ("▌   ▐", 0),
                    ("▌▄▄▄▐", 0),
                    ("▐███▌", 0),
                    (" ▀█▀ ", 0),
                    (" ███ ", 1),
                    (" ███ ", 1),
                    (" ███ ", 2),
                    (" ▀█▀ ", 3),
                    ("  █  ", 3),
                ],
                -1.0,
            ),
            Pose::Cheer { up: false } => (
                &[
                    ("▌   ▐", 0),
                    ("▌▄▄▄▐", 0),
                    ("▐███▌", 0),
                    (" ▀█▀ ", 0),
                    (" ███ ", 1),
                    (" ███ ", 1),
                    (" ███ ", 2),
                    ("▐█ █▌", 3),
                    ("▐   ▌", 3),
                ],
                -1.0,
            ),
            Pose::Walk { step: true } => (
                &[
                    (" ▄▄▄ ", 0),
                    (" ███ ", 0),
                    ("  █  ", 0),
                    ("▄███▄", 1),
                    ("▐███▌", 1),
                    (" ███ ", 2),
                    ("▐█ █▌", 3),
                    ("▐   ▌", 3),
                ],
                0.0,
            ),
            Pose::Walk { step: false } => (
                &[
                    (" ▄▄▄ ", 0),
                    (" ███ ", 0),
                    ("  █  ", 0),
                    ("▄███▄", 1),
                    ("▐███▌", 1),
                    (" ███ ", 2),
                    (" ▀█▀ ", 3),
                    ("  █  ", 3),
                ],
                0.0,
            ),
            Pose::PickUp => (
                &[
                    ("     ", 0),
                    (" ▄▄▄ ", 0),
                    (" ███ ", 0),
                    ("▄███▄", 1),
                    ("▐███▌", 1),
                    (" ███ ", 2),
                    (" █ █ ", 3),
                    (" █ █▄", 3),
                ],
                0.0,
            ),
        };
        for (index, (row, part)) in rows.iter().enumerate() {
            let y = head + top + index as f32;
            let colour = match part {
                0 => skin,
                1 => shirt,
                2 => shorts,
                _ => legs,
            };
            for (column, glyph) in row.chars().enumerate() {
                let glyph: &'static str = match glyph {
                    '▄' => "▄",
                    '█' => "█",
                    '▀' => "▀",
                    '▐' => "▐",
                    '▌' => "▌",
                    _ => continue,
                };
                canvas.put(
                    px - 2.0 + column as f32,
                    y,
                    sprite(glyph, colour, *part == 1),
                    true,
                );
            }
        }

        // The ball, the size of the head - two rows by three cells, `(bx,
        // by)` its bottom middle: in the hands, in the air with a trail, at
        // rest, or carried back.
        let (bx, by) = if carrying {
            if matches!(pose, Pose::WindUp | Pose::Jump) {
                (px, head - 2.0)
            } else {
                (px + 3.0, head + 4.0)
            }
        } else if matches!(pose, Pose::PickUp) {
            (plan.rest_x, court.floor)
        } else {
            (ball.x, ball.y)
        };
        if t >= COURT_RELEASE && t < plan.rest_t {
            for back in 1..=3u32 {
                let earlier = t - back as f32 * 0.07;
                if earlier > COURT_RELEASE {
                    let then = Self::ball_at(&court, width, throw, earlier);
                    canvas.put(
                        then.x + 0.5,
                        then.y,
                        sprite(
                            "·",
                            blend_rgb(self.accent, self.background, 0.3 + back as f32 * 0.18),
                            false,
                        ),
                        false,
                    );
                }
            }
        }
        for (dx, glyph) in [(-1.0, "▄"), (0.0, "█"), (1.0, "▄")] {
            canvas.put(bx + dx, by - 1.0, sprite(glyph, self.accent, true), true);
        }
        for (dx, glyph) in [(-1.0, "▀"), (0.0, "█"), (1.0, "▀")] {
            canvas.put(bx + dx, by, sprite(glyph, self.accent, true), true);
        }

        // What the hoop said, for a moment, and the score so far.
        let word = if let Some(at) = ball.swish_at.filter(|at| t - at < 1.2) {
            Some(("SWISH", self.secondary, t - at))
        } else {
            ball.clank_at
                .filter(|at| t - at < 1.0)
                .map(|at| ("CLANK", self.muted, t - at))
        };
        if let Some((word, colour, age)) = word {
            let rise = (age * 2.0).floor();
            let letters: [&'static str; 5] = if word == "SWISH" {
                ["S", "W", "I", "S", "H"]
            } else {
                ["C", "L", "A", "N", "K"]
            };
            for (index, glyph) in letters.into_iter().enumerate() {
                canvas.put(
                    court.rim_left + 2.0 + index as f32,
                    court.rim_y - 6.0 - rise,
                    sprite(glyph, colour, true),
                    true,
                );
            }
        }
        let made = (0..throw).filter(|&k| Self::throw_made(k)).count();
        let score = format!("{made}/{throw}");
        for (index, character) in score.chars().enumerate() {
            let glyph: &'static str = match character {
                '0' => "0",
                '1' => "1",
                '2' => "2",
                '3' => "3",
                '4' => "4",
                '5' => "5",
                '6' => "6",
                '7' => "7",
                '8' => "8",
                '9' => "9",
                _ => "/",
            };
            canvas.put(
                width - 1.0 - score.len() as f32 + index as f32,
                1.0,
                sprite(glyph, self.muted, false),
                true,
            );
        }
        SceneFrame {
            sprites: canvas.sprites,
            glow: None,
        }
    }
}

impl ShowroomFx {
    pub(super) fn paint_basketball(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Spot { x, y, blank, .. } = *spot;
        let basketball = &frame.basketball;
        if let Some(frame) = &basketball
            && let Some(sprite) = frame.sprites.get(&(x, y))
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

    #[test]
    fn basketball_shoots_fetches_the_ball_and_shoots_again() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(15, 17, 21);
        let active = showroom_test_visual(CellEffect::Basketball, background);
        let signal = Arc::new(ReactiveAudioSignal::default());
        let mut effect = ShowroomFx::new(&active, Arc::clone(&signal));
        let render = |effect: &ShowroomFx| {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            effect.render(&mut buffer, area);
            buffer
        };
        let find = |buffer: &Buffer, glyph: &str| {
            (0..area.height)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| buffer.cell((x, y)).expect("cell").symbol() == glyph)
                .collect::<Vec<_>>()
        };
        let court = Court::over(100.0, 30.0);
        // Both kinds of throw happen; a make goes through, a miss does not,
        // and every ball comes to rest on the court with a walk to fetch it.
        let made = (0..40).filter(|&k| ShowroomFx::throw_made(k)).count();
        assert!((10..32).contains(&made), "makes and misses: {made}/40");
        let make = (0..40)
            .find(|&k| ShowroomFx::throw_made(k))
            .expect("a make");
        let miss = (0..40)
            .find(|&k| !ShowroomFx::throw_made(k))
            .expect("a miss");
        let through = |throw: u64| {
            (0..500).any(|step| {
                ShowroomFx::ball_at(&court, 100.0, throw, COURT_RELEASE + step as f32 * 0.01)
                    .swish_at
                    .is_some()
            })
        };
        assert!(through(make), "the make goes through");
        assert!(!through(miss), "the miss does not");
        for throw in 0..12 {
            let plan = ShowroomFx::plan_throw(&court, 100.0, throw);
            assert!(
                plan.rest_t < COURT_LONGEST_FLIGHT,
                "throw {throw} settles: {plan:?}"
            );
            assert!(
                (1.0..=97.0).contains(&plan.rest_x),
                "on the court: {plan:?}"
            );
            assert!(
                plan.length > plan.fetch_t + 0.6,
                "with a walk to fetch it: {plan:?}"
            );
            let swish = ShowroomFx::ball_at(&court, 100.0, throw, plan.rest_t).swish_at;
            match swish {
                Some(at) => assert!(
                    plan.fetch_t >= at + COURT_CHEER && plan.fetch_t >= plan.rest_t,
                    "a make is cheered before the fetch: {plan:?}"
                ),
                None => assert_eq!(plan.fetch_t, plan.rest_t, "a miss is fetched at once"),
            }
        }

        // The hoop is built to the player's size, and stands the same
        // distance from them whatever the pane's width; a narrow pane
        // brings it nearer rather than off the edge.
        assert_eq!(
            court.rim_y,
            court.head_y - 4.0,
            "half a player above the head"
        );
        let wide = Court::over(220.0, 40.0);
        assert_eq!(wide.board_x - wide.home_x, COURT_RANGE);
        assert_eq!(court.board_x - court.home_x, COURT_RANGE);
        let narrow = Court::over(36.0, 18.0);
        assert!(narrow.board_x - narrow.home_x < COURT_RANGE);
        assert!(narrow.board_x <= 33.0 && narrow.home_x >= 3.0, "{narrow:?}");

        // The player is a figure, the hoop a hoop, the ball the size of
        // the head: two rows, three cells, in the accent colour.
        effect.elapsed_ms = 300;
        let idle = render(&effect);
        assert_eq!(find(&idle, "─").len(), 10, "the rim");
        assert!(find(&idle, "█").len() >= 20, "a board and a big player");
        let ball = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let cell = idle.cell((x, y)).expect("cell");
                cell.symbol() == "█" && cell.fg == active.accent
            })
            .collect::<Vec<_>>();
        assert_eq!(ball.len(), 2, "the ball's middle column: {ball:?}");
        assert_eq!(ball[0].0, ball[1].0);
        assert_eq!(ball[0].1 + 1, ball[1].1);
        assert_eq!(
            idle.cell((ball[0].0 - 1, ball[0].1))
                .expect("cell")
                .symbol(),
            "▄"
        );
        assert_eq!(
            idle.cell((ball[1].0 + 1, ball[1].1))
                .expect("cell")
                .symbol(),
            "▀"
        );
        assert!(
            (ball[0].0 as f32 - court.home_x).abs() <= 4.0,
            "in the hands: {ball:?}"
        );

        // A swish is jumped for: at the cheer's start the arms are up over
        // the head, and the head is one row higher on alternate frames.
        let plan = ShowroomFx::plan_throw(&court, 100.0, make);
        let swish = ShowroomFx::ball_at(&court, 100.0, make, plan.rest_t)
            .swish_at
            .expect("the make went through");
        let start: f32 = (0..make)
            .map(|k| ShowroomFx::plan_throw(&court, 100.0, k).length)
            .sum();
        let heads = [0.02f32, 0.24].map(|after| {
            effect.elapsed_ms = ((start + swish + after) * 1000.0 / (0.45 + 58.0 / 80.0)) as u64;
            let cheering = render(&effect);
            (0..area.height)
                .find(|&y| {
                    (0..area.width).any(|x| {
                        let cell = cheering.cell((x, y)).expect("cell");
                        cell.symbol() == "▌"
                            && cell.fg == active.foreground
                            && (x as f32 - court.home_x).abs() <= 3.0
                    })
                })
                .expect("arms up over the head")
        });
        assert_ne!(heads[0], heads[1], "the cheer hops: {heads:?}");

        // Mid-fetch the player is between home and the ball, walking.
        let plan = ShowroomFx::plan_throw(&court, 100.0, 0);
        let mid = plan.fetch_t + plan.walk(&court) * 0.5;
        effect.elapsed_ms = (mid * 1000.0 / (0.45 + 58.0 / 80.0)) as u64;
        let fetching = render(&effect);
        let head_left = (0..area.width)
            .find(|&x| {
                (0..area.height).any(|y| {
                    let cell = fetching.cell((x, y)).expect("cell");
                    cell.symbol() == "▄" && cell.fg == active.foreground
                })
            })
            .map(|x| x as f32 + 1.0)
            .expect("a head");
        assert!(
            (head_left - court.home_x).abs() > 0.5 && (head_left - plan.rest_x).abs() > 0.5,
            "walking between home and the ball: head at {head_left}, home {}, ball {}",
            court.home_x,
            plan.rest_x
        );
        assert!(
            fetching
                .content
                .iter()
                .all(|cell| cell.symbol().width() <= 1)
        );
    }
}

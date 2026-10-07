//! The leak: water pouring into a container, its surface, its drips and
//! splashes, kept honest by a little physics.

use super::*;

/// Fixed simulation step for the leak. The frame loop hands out at most
/// 100 ms (`ThemeVisualEngine::process`), so a frame is at most ten steps of
/// an O(width) update.
pub(super) const LEAK_STEP: f32 = 1.0 / 60.0;
pub(super) const LEAK_MAX_STEPS: u32 = 10;
/// Wave stiffness. c = sqrt(320) = 17.9 cells/s, so the Courant number at the
/// fixed step is 0.30 - a third of the leapfrog stability limit.
pub(super) const LEAK_TENSION: f32 = 320.0;
/// Flow friction per second, applied implicitly so it can only lose energy.
pub(super) const LEAK_DRAG: f32 = 0.9;
/// Drip acceleration (cells/s²) and terminal speed (cells/s).
pub(super) const LEAK_GRAVITY: f32 = 26.0;
pub(super) const LEAK_FALL_MAX: f32 = 42.0;
/// Depth one drop adds where it lands, and the sideways kick of its crown.
pub(super) const LEAK_DROP: f32 = 0.9;
pub(super) const LEAK_SPLASH: f32 = 2.4;
/// Seconds for one fill → settle → breach → seal, before the speed scale.
pub(super) const LEAK_CYCLE: f32 = 27.0;
/// The breach behaves like a hole 1.6% of the screen wide.
pub(super) const LEAK_BREACH: f32 = 0.016;
pub(super) const LEAK_LADDER: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

#[derive(Clone, Copy, Debug)]
pub(super) struct LeakDrip {
    pub(super) column: u16,
    /// Cells below the ceiling.
    pub(super) y: f32,
    pub(super) speed: f32,
}

/// The container: a one-dimensional height field and the drops above it.
///
/// Volume is conserved exactly. Every cell of water is in `depth`, inside a
/// drip, or waiting in `reservoir` above the ceiling - so what the breach lets
/// out is the same water that comes back down as rain. Three floats per
/// column plus a bounded drip list: about 2.4 KB at 200 columns.
#[derive(Clone, Debug, Default)]
pub(super) struct LeakWater {
    pub(super) width: u16,
    pub(super) height: u16,
    /// Water per column, in cells, 0..=height.
    pub(super) depth: Vec<f32>,
    /// Flow into column `i` across its LEFT edge, cells/s. `flux[0]` and
    /// `flux[width]` are the walls and stay zero.
    pub(super) flux: Vec<f32>,
    /// Lowest drip head per column, `INFINITY` if none.
    pub(super) heads: Vec<f32>,
    pub(super) drips: Vec<LeakDrip>,
    /// Water above the ceiling, in cells of depth.
    pub(super) reservoir: f32,
    /// Seconds, already scaled by the theme's speed.
    pub(super) clock: f32,
    /// Simulation time not yet spent in a fixed step.
    pub(super) lag: f32,
    /// Fractional drips owed to the spawner.
    pub(super) credit: f32,
}

impl LeakWater {
    /// Resting depth of a full container, in cells. `density` buys volume
    /// here rather than the dither the other effects spend it on.
    pub(super) fn level(height: f32, density: u8) -> f32 {
        height * (0.16 + f32::from(density) / 100.0 * 0.26)
    }

    pub(super) fn total(&self, density: u8) -> f32 {
        Self::level(f32::from(self.height), density) * f32::from(self.width)
    }

    /// Everything the container owns, wherever it is. The invariant the
    /// physics is held to.
    #[cfg(test)]
    pub(super) fn volume(&self) -> f32 {
        self.depth.iter().sum::<f32>() + self.reservoir + self.drips.len() as f32 * LEAK_DROP
    }

    pub(super) fn resize(&mut self, area: Rect, density: u8) {
        let (w, h) = (usize::from(area.width), f32::from(area.height));
        self.width = area.width;
        self.height = area.height;
        self.depth.clear();
        self.depth.resize(w, Self::level(h, density) * 0.55);
        self.flux.clear();
        self.flux.resize(w + 1, 0.0);
        self.heads.clear();
        self.heads.resize(w, f32::INFINITY);
        self.drips.clear();
        // Selecting the theme should show water, not an empty box: start it
        // just over half full and hold the rest overhead.
        self.reservoir = (self.total(density) - self.depth.iter().sum::<f32>()).max(0.0);
    }

    /// The breach for the cycle the clock is in: which column, how open.
    /// Derived from the clock rather than stored, so it needs no state.
    pub(super) fn breach(&self) -> (usize, f32) {
        if self.width == 0 {
            return (0, 0.0);
        }
        let stage = self.clock / LEAK_CYCLE;
        let generation = stage.max(0.0).floor() as u64;
        let phase = fract(stage);
        let column = (hash(generation, 0x1ea4, 0x0d1e) % u64::from(self.width)) as usize;
        // Opens over 0.58..0.64 of the cycle, seals over 0.93..0.99.
        let ramp = |a: f32, b: f32| ((phase - a) / (b - a)).clamp(0.0, 1.0);
        let open = ramp(0.58, 0.64) * (1.0 - ramp(0.93, 0.99));
        (column, open * open * (3.0 - 2.0 * open))
    }

    /// Called from `process`, with `dt` already scaled by the theme's speed.
    pub(super) fn advance(&mut self, area: Rect, dt: f32, density: u8) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.width != area.width || self.height != area.height {
            self.resize(area, density);
        }
        self.lag += dt;
        let mut budget = LEAK_MAX_STEPS;
        while self.lag >= LEAK_STEP && budget > 0 {
            self.lag -= LEAK_STEP;
            budget -= 1;
            self.step(density);
        }
        // Drop whatever backlog a stall built up rather than replaying it.
        self.lag = self.lag.min(LEAK_STEP);
        self.heads.fill(f32::INFINITY);
        for drip in &self.drips {
            let slot = usize::from(drip.column);
            if drip.y < self.heads[slot] {
                self.heads[slot] = drip.y;
            }
        }
    }

    pub(super) fn step(&mut self, density: u8) {
        let dt = LEAK_STEP;
        let width = self.depth.len();
        let ceiling = f32::from(self.height);
        self.clock += dt;

        // 1. Waves. A staggered height field: every interior edge accelerates
        //    toward the lower of the two columns it separates, loses a little
        //    to friction, then carries that much water across.
        for i in 1..width {
            let slope = self.depth[i - 1] - self.depth[i];
            self.flux[i] = (self.flux[i] + LEAK_TENSION * slope * dt) / (1.0 + LEAK_DRAG * dt);
        }
        for i in 0..width {
            self.depth[i] += (self.flux[i] - self.flux[i + 1]) * dt;
        }

        // 2. The breach. Torricelli: the jet is as fast as the head above it,
        //    so the drain slows as the level falls. Scaled by width so a wide
        //    terminal empties in the same seconds as a narrow one.
        let (hole, open) = self.breach();
        if open > 0.0 && hole < width {
            let head = self.depth[hole].max(0.0);
            let rate = open * LEAK_BREACH * width as f32 * (2.0 * LEAK_GRAVITY * head).sqrt();
            let escaped = (rate * dt).min(self.depth[hole] * 0.5);
            self.depth[hole] -= escaped;
            self.reservoir += escaped;
        }

        // 3. Floor and ceiling. Whatever the solver overshot is water the
        //    budget still owns, so it goes back overhead - never invented,
        //    never destroyed.
        let mut spill = 0.0;
        for depth in &mut self.depth {
            if *depth < 0.0 {
                spill += *depth;
                *depth = 0.0;
            } else if *depth > ceiling {
                spill += *depth - ceiling;
                *depth = ceiling;
            }
        }
        self.reservoir += spill;

        // 4. Rain. The fuller the reservoir the harder it pours, so the
        //    ceiling opens up right after a breach and drizzles when full.
        let share = (self.reservoir / self.total(density).max(1.0)).clamp(0.0, 1.0);
        let rate = width as f32 * (0.05 + f32::from(density) / 100.0 * 0.25) * (0.15 + share);
        self.credit += rate * dt;
        let cap = (width / 6).clamp(6, 48);
        while self.credit >= 1.0
            && self.drips.len() < cap
            && self.reservoir >= LEAK_DROP
            && width > 0
        {
            self.credit -= 1.0;
            self.reservoir -= LEAK_DROP;
            let seed = hash(
                self.drips.len() as u64,
                (self.clock * 1000.0) as u64,
                0xd819,
            );
            self.drips.push(LeakDrip {
                column: (seed % width as u64) as u16,
                y: 0.0,
                speed: 6.0 + hash_unit(seed >> 16) * 8.0,
            });
        }

        // 5. Fall and land. Taken out so the closure can touch the field.
        let mut drips = std::mem::take(&mut self.drips);
        drips.retain_mut(|drip| {
            drip.speed = (drip.speed + LEAK_GRAVITY * dt).min(LEAK_FALL_MAX);
            drip.y += drip.speed * dt;
            let column = usize::from(drip.column);
            if drip.y < ceiling - self.depth[column] {
                return true;
            }
            self.depth[column] += LEAK_DROP;
            // The crown: push water out to both sides. Interior edges only -
            // the walls stay zero or the container leaks volume out of the
            // domain.
            if column > 0 {
                self.flux[column] -= LEAK_SPLASH;
            }
            if column + 1 < width {
                self.flux[column + 1] += LEAK_SPLASH;
            }
            false
        });
        self.drips = drips;
    }
}

impl ShowroomFx {
    pub(super) fn paint_leak(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { area, height, .. } = *frame;
        let Spot {
            x,
            py,
            ny,
            blank,
            text,
            ..
        } = *spot;
        let leak = &frame.leak;
        let column = usize::from(x - area.x);
        // `render` is also called without `process` in tests;
        // an unsized container simply has no water yet.
        if let (Some(&depth), Some(&head)) =
            (self.leak.depth.get(column), self.leak.heads.get(column))
        {
            let (hole, open) = leak.unwrap_or((0, 0.0));
            // Cells below the ceiling.
            let surface = height - depth;
            let fill = (py + 1.0 - surface).clamp(0.0, 1.0);
            let floor = py >= height - 1.0;
            let breached = open > 0.25 && column == hole;

            if floor && blank && !breached {
                // The container the water is sitting in.
                cell.set_symbol("─")
                    .set_fg(blend_rgb(self.muted, self.background, 0.4));
            } else if breached && blank && fill > 0.0 {
                // Water leaving through the hole, streaking down.
                cell.set_symbol(if fract(py * 0.34 - self.leak.clock * 1.7) < 0.5 {
                    "│"
                } else {
                    "╷"
                })
                .set_fg(self.foreground);
                cell.modifier.insert(Modifier::BOLD);
            } else if fill >= 1.0 {
                // The body. Darker with depth so the code stays
                // readable through it.
                let below = py - surface;
                if blank {
                    cell.set_symbol("█").set_fg(blend_rgb(
                        self.accent,
                        self.background,
                        (0.42 + below * 0.05).min(0.86),
                    ));
                } else if text {
                    cell.set_fg(blend_rgb(cell.fg, self.accent, 0.55));
                    if below > 6.0 {
                        cell.modifier.insert(Modifier::DIM);
                    }
                }
            } else if fill > 0.0 {
                // The surface itself, on the eighth-block ladder.
                if blank {
                    let rung = ((fill * 8.0).ceil() as usize).clamp(1, 8) - 1;
                    cell.set_symbol(LEAK_LADDER[rung]).set_fg(blend_rgb(
                        self.accent,
                        self.secondary,
                        fill,
                    ));
                } else if text {
                    cell.set_fg(self.foreground);
                    cell.modifier.insert(Modifier::BOLD);
                }
            } else if blank && head.is_finite() && py <= head && head - py < 3.0 {
                // A drop still in the air: its head, then two
                // cells of trail.
                let behind = head - py;
                cell.set_symbol(if behind < 1.0 {
                    "•"
                } else if behind < 2.0 {
                    "│"
                } else {
                    "╷"
                })
                .set_fg(blend_rgb(self.secondary, self.accent, ny));
                if behind < 1.0 {
                    cell.modifier.insert(Modifier::BOLD);
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
    fn leak_conserves_its_water_and_keeps_it_inside_the_container() {
        let area = Rect::new(0, 0, 120, 40);
        let density = 60;
        let mut water = LeakWater::default();
        water.advance(area, 0.0, density);
        let total = water.total(density);
        assert!((water.volume() - total).abs() < 1e-2, "sized to its level");

        let mut breached = false;
        let mut rained = false;
        let mut held_before_breach = None;
        let mut held_peak: f32 = 0.0;
        // Three full cycles, a frame at a time.
        let frames = (LEAK_CYCLE * 3.0 / 0.016) as usize;
        for _ in 0..frames {
            water.advance(area, 0.016, density);
            assert!(
                (water.volume() - total).abs() < 1.0,
                "water was created or destroyed"
            );
            assert!(water.depth.iter().all(|depth| (0.0..=40.0).contains(depth)));
            assert_eq!(water.flux[0], 0.0, "the left wall holds");
            assert_eq!(water.flux[120], 0.0, "the right wall holds");
            assert!(water.drips.len() <= 48);
            rained |= !water.drips.is_empty();
            let (_, open) = water.breach();
            if open > 0.5 {
                breached = true;
                held_before_breach.get_or_insert(water.reservoir);
                held_peak = held_peak.max(water.reservoir);
            }
        }
        assert!(breached, "the cycle opens a breach");
        assert!(rained, "the ceiling drips");
        assert!(
            held_peak > held_before_breach.unwrap_or(0.0) + total * 0.1,
            "a breach lets a tenth of the water out"
        );

        // A resize is a fresh container at the new size, not a stretched one.
        let narrow = Rect::new(0, 0, 40, 12);
        water.advance(narrow, 0.016, density);
        assert_eq!(water.depth.len(), 40);
        assert_eq!(water.flux.len(), 41);
        assert!((water.volume() - water.total(density)).abs() < 1.0);
    }

    #[test]
    fn leak_paints_a_floor_a_body_and_a_surface_on_the_ladder() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(7, 7, 19);
        let active = showroom_test_visual(CellEffect::Leak, background);
        let signal = Arc::new(ReactiveAudioSignal::default());
        let mut effect = ShowroomFx::new(&active, Arc::clone(&signal));
        // Each frame starts from the freshly drawn interface, as in the app.
        let mut buffer = Buffer::empty(area);
        for _ in 0..30 {
            buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            effect.process(Duration::from_millis(100), &mut buffer, area);
        }
        let symbols = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>();
        assert!(
            symbols.iter().all(|symbol| symbol.width() <= 1),
            "nothing wider than a cell"
        );
        assert!(symbols.contains(&"█"), "the body of the water");
        assert!(
            symbols
                .iter()
                .any(|symbol| LEAK_LADDER[..7].contains(symbol)),
            "a surface part way up a cell"
        );
        assert!(
            (area.x..area.right()).any(|x| buffer
                .cell((x, area.bottom() - 1))
                .is_some_and(|cell| cell.symbol() == "─")),
            "the floor of the container"
        );
    }
}

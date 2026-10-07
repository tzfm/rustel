//! Particles: bubbles rising, fumes off a burn, smoke - frames of them for
//! a moment, and the cells they become.

use super::*;

/// Live-theme interpretations of the TerminalTextEffects showroom. These run
/// against the freshly rendered Studio buffer rather than a captured string,
/// so typing, playback state and meters keep updating while the effect moves.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct BubbleFrame {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) radius: f32,
    pub(super) pop: f32,
    pub(super) color: u8,
    pub(super) visible: bool,
}

pub(super) const BUBBLE_COUNT: usize = 14;

pub(super) fn bubble_frames(time: f32, density: u8) -> [BubbleFrame; BUBBLE_COUNT] {
    let visible = 3 + usize::from(density) * (BUBBLE_COUNT - 3) / 100;
    std::array::from_fn(|index| {
        let seed = hash(index as u64, 0x6275_6262_6c65, 0x0070_6f70);
        let lifetime = 5.5 + hash_unit(seed.rotate_left(7)) * 11.5;
        let phase = fract(time / lifetime + hash_unit(seed.rotate_left(19)));
        let pop_at = 0.64 + hash_unit(seed.rotate_left(31)) * 0.29;
        let rise = (phase / pop_at).min(1.0);
        let drift = (phase * (3.0 + hash_unit(seed) * 4.0) + hash_unit(seed.rotate_left(11))).sin()
            * (0.018 + hash_unit(seed.rotate_left(23)) * 0.045);
        let base_radius = 0.018 + hash_unit(seed.rotate_left(37)) * 0.072;
        BubbleFrame {
            x: 0.06 + hash_unit(seed.rotate_left(43)) * 0.88 + drift,
            y: 1.08 - rise * 1.14,
            radius: base_radius * (0.42 + rise * 0.88),
            pop: ((phase - pop_at) / (1.0 - pop_at)).clamp(0.0, 1.0),
            color: (seed % 3) as u8,
            visible: index < visible,
        }
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FumeFrame {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) radius: f32,
    pub(super) age: f32,
    pub(super) visible: bool,
}

pub(super) const FUME_COUNT: usize = 12;

pub(super) fn fume_frames(time: f32, density: u8) -> [FumeFrame; FUME_COUNT] {
    let visible = 3 + usize::from(density) * (FUME_COUNT - 3) / 100;
    std::array::from_fn(|index| {
        let seed = hash(index as u64, 0x6675_6d65, 0x7269_7365);
        let lifetime = 4.0 + hash_unit(seed.rotate_left(9)) * 8.5;
        let age = fract(time / lifetime + hash_unit(seed.rotate_left(21)));
        let curl = (age * (4.0 + hash_unit(seed) * 5.0) + hash_unit(seed.rotate_left(33))).sin()
            * (0.025 + age * 0.075);
        FumeFrame {
            x: 0.08 + hash_unit(seed.rotate_left(45)) * 0.84 + curl,
            y: 1.03 - age * 1.17,
            radius: 0.015 + age * (0.045 + hash_unit(seed.rotate_left(13)) * 0.055),
            age,
            visible: index < visible,
        }
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SmokeFrame {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) radius: f32,
    pub(super) age: f32,
    pub(super) opacity: f32,
    pub(super) visible: bool,
}

pub(super) const SMOKE_COUNT: usize = 60;

pub(super) fn smoke_frames(time: f32, density: u8) -> [SmokeFrame; SMOKE_COUNT] {
    let visible = 30 + usize::from(density) * (SMOKE_COUNT - 30) / 100;
    std::array::from_fn(|index| {
        let seed = hash(index as u64, 0x736d_6f6b_655f_7075, 0x0000_0000_6666_6673);
        let lane = index % 3;
        let order = index / 3;
        let lifetime = 9.0 + lane as f32 * 1.7;
        let age =
            fract(time / lifetime + order as f32 / (SMOKE_COUNT / 3) as f32 + lane as f32 * 0.13);
        let source = 0.22 + lane as f32 * 0.28;
        let curl = (age * (4.2 + lane as f32 * 0.8) + hash_unit(seed.rotate_left(33))).sin()
            * (0.01 + age * 0.11)
            + (age * 13.0 - time * 0.18 + lane as f32).cos() * age * 0.018;
        let opacity = (age * std::f32::consts::PI).sin().max(0.0).powf(0.72);
        SmokeFrame {
            x: source + (hash_unit(seed.rotate_left(43)) - 0.5) * 0.055 + curl,
            y: 1.08 - age * 1.24,
            radius: 0.026 + age * (0.088 + hash_unit(seed.rotate_left(13)) * 0.045),
            age,
            opacity,
            visible: index < visible,
        }
    })
}

impl ShowroomFx {
    pub(super) fn paint_bubbles(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame {
            time,
            aspect,
            stroke,
            ..
        } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            ..
        } = *spot;
        let bubbles = &frame.bubbles;
        let point = Point2 { x: nx, y: ny };
        for bubble in bubbles.as_ref().expect("bubble scene") {
            if !bubble.visible {
                continue;
            }
            let distance = point_distance(
                point,
                Point2 {
                    x: bubble.x,
                    y: bubble.y,
                },
                aspect,
            );
            let dx = (nx - bubble.x) * aspect;
            let dy = ny - bubble.y;
            let base_color = match bubble.color {
                0 => self.accent,
                1 => self.secondary,
                _ => self.foreground,
            };
            let hit = if bubble.pop > 0.0 {
                let burst = bubble.radius + bubble.pop * 0.13;
                let spoke = (dx.atan2(dy) * 9.0).sin().abs();
                (distance - burst).abs() < stroke * 1.6 && spoke > 0.38 + bubble.pop * 0.34
            } else {
                (distance - bubble.radius).abs() < stroke
                    || (bubble.radius < stroke * 1.8 && distance < stroke)
            };
            if !hit {
                continue;
            }
            if blank {
                let glyph = if bubble.pop > 0.0 {
                    if noise & 1 == 0 { "*" } else { "·" }
                } else if bubble.radius > 0.04 {
                    if dx.abs() < dy.abs() * 0.45 {
                        "─"
                    } else if dy.abs() < dx.abs() * 0.45 {
                        "│"
                    } else if dx.signum() == dy.signum() {
                        "╱"
                    } else {
                        "╲"
                    }
                } else if bubble.radius > 0.025 {
                    "○"
                } else if bubble.radius > 0.015 {
                    "o"
                } else {
                    "·"
                };
                cell.set_symbol(glyph);
            }
            cell.set_fg(if bubble.pop > 0.0 {
                blend_rgb(base_color, self.muted, bubble.pop * 0.72)
            } else {
                cycle_color(
                    base_color,
                    self.foreground,
                    self.secondary,
                    fract(time * 0.045 + f32::from(bubble.color) / 3.0),
                )
            });
            if bubble.pop < 0.12 {
                cell.modifier.insert(Modifier::BOLD);
            } else if bubble.pop > 0.72 {
                cell.modifier.insert(Modifier::DIM);
            }
            break;
        }
    }

    pub(super) fn paint_burn(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, aspect, .. } = *frame;
        let Spot {
            px,
            nx,
            ny,
            noise,
            blank,
            ..
        } = *spot;
        let fumes = &frame.fumes;
        let from_bottom = 1.0 - ny;
        let flame_height = 0.065
            + ((px * 0.43 + time * 5.2).sin() + 1.0) * 0.035
            + ((px * 0.91 - time * 3.7).sin() + 1.0) * 0.018;
        if from_bottom <= flame_height {
            if blank {
                cell.set_symbol(["▒", "╱", "╲", "│"][(noise as usize) % 4]);
            }
            cell.set_fg(if from_bottom < flame_height * 0.34 {
                self.foreground
            } else if noise & 1 == 0 {
                self.secondary
            } else {
                self.accent
            });
            cell.modifier.insert(Modifier::BOLD);
            return;
        }

        for fume in fumes.as_ref().expect("fume scene") {
            if !fume.visible {
                continue;
            }
            let dx = (nx - fume.x) * aspect / fume.radius.max(0.001);
            let dy = (ny - fume.y) / (fume.radius * 0.72).max(0.001);
            let distance = dx * dx + dy * dy;
            if distance >= 1.0
                || noise % 100 >= u64::from((self.density / 2).saturating_add(28).min(92))
            {
                continue;
            }
            if blank {
                cell.set_symbol(if distance < 0.28 {
                    "▒"
                } else if distance < 0.68 {
                    "░"
                } else {
                    "·"
                });
            }
            let smoke = blend_rgb(self.foreground, self.muted, 0.68 + fume.age * 0.32);
            cell.set_fg(blend_rgb(smoke, self.background, fume.age * 0.64));
            if fume.age > 0.68 {
                cell.modifier.insert(Modifier::DIM);
            }
            break;
        }
    }

    pub(super) fn paint_smoke(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, aspect, .. } = *frame;
        let Spot {
            x,
            y,
            nx,
            ny,
            blank,
            ..
        } = *spot;
        let smoke = &frame.smoke;
        let point = Point2 { x: nx, y: ny };
        let mut cloud = 0.0;
        let mut age_weight = 0.0;
        let mut turbulence_weight = 0.0;
        let billow =
            ((point.y * 17.0 + time * 0.7).sin() + (point.x * 13.0 - time * 0.43).cos()) * 0.022;
        for (index, puff) in smoke.as_ref().expect("smoke scene").iter().enumerate() {
            if !puff.visible {
                continue;
            }
            let turbulence = hash_unit(hash(u64::from(x), u64::from(y), index as u64 * 0x9e37));
            let dx = (point.x - puff.x) * aspect / puff.radius.max(0.001)
                + billow
                + (turbulence - 0.5) * 0.04;
            let dy = (point.y - puff.y) / (puff.radius * 0.74).max(0.001);
            let distance = (dx * dx + dy * dy).sqrt();
            let body = (1.0 - distance + (turbulence - 0.5) * 0.28)
                .max(0.0)
                .powf(1.45)
                * puff.opacity;
            cloud += body * 0.72;
            age_weight += body * puff.age;
            turbulence_weight += body * turbulence;
        }
        let amount = 1.0 - (-cloud).exp();
        if blank && amount > 0.045 {
            let age = age_weight / cloud.max(0.001);
            let turbulence = turbulence_weight / cloud.max(0.001);
            cell.set_symbol(if amount > 0.72 {
                "▓"
            } else if amount > 0.46 {
                "▒"
            } else if amount > 0.22 {
                "░"
            } else {
                "·"
            });
            let smoke_color = blend_rgb(self.muted, self.foreground, 0.08 + turbulence * 0.26);
            cell.set_fg(blend_rgb(
                self.background,
                smoke_color,
                (0.28 + amount * 0.72) * (1.0 - age * 0.38),
            ));
            if amount < 0.18 || age > 0.78 {
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn smoke_remains_visible_and_moves_below_full_density() {
        let theme = Theme::built_in("smoke").expect("smoke preset");
        let active = ActiveVisual {
            config: theme.cell_visual().expect("native smoke"),
            background: theme.background,
            foreground: theme.foreground,
            accent: theme.accent,
            secondary: theme.syntax.number,
            muted: theme.muted,
        };
        let area = Rect::new(0, 0, 80, 24);
        let mut baseline = Buffer::empty(area);
        baseline.set_style(area, Style::default().bg(theme.background));
        let mut effect = ShowroomFx::new(&active, Arc::new(ReactiveAudioSignal::default()));

        for density in [0, active.config.density, 99, 100] {
            effect.density = density;
            let mut previous = None;
            for elapsed in [0, 1_500, 5_000, 12_000] {
                effect.elapsed_ms = elapsed;
                let mut frame = baseline.clone();
                effect.render(&mut frame, area);
                assert!(
                    frame.content.iter().any(|cell| cell.symbol() != " "),
                    "density {density}, time {elapsed}: visible puffs must reach the frame"
                );
                if let Some(previous) = previous {
                    assert_ne!(frame, previous, "density {density}: smoke must move");
                }
                previous = Some(frame);
            }
        }
    }

    #[test]
    fn bubbles_have_distinct_sizes_colors_and_burst_times() {
        let mut minimum_radius = f32::MAX;
        let mut maximum_radius = f32::MIN;
        let mut colors = [false; 3];
        let mut rising = false;
        let mut bursting = false;

        for time in [0.0, 2.0, 5.0, 9.0, 14.0, 20.0] {
            for bubble in bubble_frames(time, 100) {
                minimum_radius = minimum_radius.min(bubble.radius);
                maximum_radius = maximum_radius.max(bubble.radius);
                colors[usize::from(bubble.color)] = true;
                rising |= bubble.pop == 0.0 && bubble.y > 0.1 && bubble.y < 1.0;
                bursting |= bubble.pop > 0.25;
            }
        }

        assert!(maximum_radius - minimum_radius > 0.06);
        assert!(colors.into_iter().all(|seen| seen));
        assert!(rising, "at least one intact bubble crosses the screen");
        assert!(bursting, "bubble lifetimes include a visible burst phase");
    }

    #[test]
    fn burn_fumes_span_the_flame_bed_to_the_top_and_expand() {
        let frames = [
            fume_frames(0.0, 100),
            fume_frames(5.0, 100),
            fume_frames(11.0, 100),
        ];
        let mut near_bottom = false;
        let mut near_top = false;
        let mut small = false;
        let mut large = false;

        for fume in frames.into_iter().flatten() {
            near_bottom |= fume.y > 0.85;
            near_top |= fume.y < 0.15;
            small |= fume.radius < 0.035;
            large |= fume.radius > 0.07;
        }

        assert!(near_bottom && near_top, "fumes traverse the full screen");
        assert!(small && large, "fumes widen as their lifetimes progress");
    }
}

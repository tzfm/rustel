//! Things in motion: paths, balls, fireworks, swarms, rings, waves and a
//! grid - effects that move shapes across the pane.

use super::*;

impl ShowroomFx {
    pub(super) fn paint_binary_path(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { area, time, .. } = *frame;
        let Spot {
            y,
            px,
            noise,
            blank,
            ..
        } = *spot;
        let lane =
            ((px * 3.0 + time * 16.0) as i32).rem_euclid(i32::from(area.height.max(1))) as u16;
        let row = y - area.y;
        let trail = lane.saturating_sub(row);
        if blank && row <= lane && trail < 7 && noise % 100 < u64::from(self.density) {
            cell.set_symbol(if noise & 1 == 0 { "0" } else { "1" })
                .set_fg(if trail == 0 {
                    self.foreground
                } else {
                    blend_rgb(self.accent, self.muted, f32::from(trail) / 7.0)
                });
        }
    }

    pub(super) fn paint_blackhole(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            text,
            ..
        } = *spot;
        let dx = (nx - 0.5) * 2.0;
        let dy = (ny - 0.5) * 1.1;
        let radius = (dx * dx + dy * dy).sqrt();
        let ring = fract(time * 0.13) * 0.85;
        if blank && (radius - ring).abs() < 0.035 {
            cell.set_symbol(if noise & 1 == 0 { "○" } else { "·" })
                .set_fg(self.accent);
        } else if text && radius < 0.16 + 0.04 * (time * 3.0).sin().abs() {
            cell.set_fg(self.muted).modifier.insert(Modifier::DIM);
        }
    }

    pub(super) fn paint_bouncy_balls(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { nx, ny, blank, .. } = *spot;
        let mut hit = None;
        for ball in 0..7u64 {
            let bx = fract(time * (0.055 + ball as f32 * 0.004) + ball as f32 * 0.17);
            let bounce = (time * (1.3 + ball as f32 * 0.09) + ball as f32)
                .sin()
                .abs();
            let by = 0.88 - bounce * (0.48 + (ball % 3) as f32 * 0.09);
            if (nx - bx).abs() < 0.012 && (ny - by).abs() < 0.028 {
                hit = Some(ball);
                break;
            }
        }
        if blank && let Some(ball) = hit {
            cell.set_symbol("●").set_fg(if ball & 1 == 0 {
                self.accent
            } else {
                self.secondary
            });
        }
    }

    pub(super) fn paint_fireworks(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            ..
        } = *spot;
        let burst = time * 0.34;
        let generation = burst.floor() as u64;
        let progress = fract(burst);
        let cx = 0.18 + (hash(generation, 7, 11) % 640) as f32 / 1000.0;
        let cy = 0.18 + (hash(generation, 13, 17) % 380) as f32 / 1000.0;
        let dx = (nx - cx) * 1.8;
        let dy = ny - cy;
        let radius = (dx * dx + dy * dy).sqrt();
        let spoke = ((dx.atan2(dy) * 11.0).sin()).abs();
        if blank
            && (radius - progress * 0.65).abs() < 0.035
            && spoke > 0.72
            && noise % 100 < u64::from(self.density)
        {
            cell.set_symbol(if noise & 1 == 0 { "*" } else { "·" })
                .set_fg(if noise & 2 == 0 {
                    self.secondary
                } else {
                    self.accent
                });
        }
    }

    pub(super) fn paint_orbitting_volley(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            ..
        } = *spot;
        let angle = time * 1.9 + (noise % 5) as f32 * 1.256;
        let orbit = 0.16 + (noise % 3) as f32 * 0.09;
        let ox = 0.5 + angle.cos() * orbit;
        let oy = 0.5 + angle.sin() * orbit * 0.55;
        if blank && (nx - ox).abs() < 0.011 && (ny - oy).abs() < 0.025 {
            cell.set_symbol("●").set_fg(if noise & 1 == 0 {
                self.accent
            } else {
                self.secondary
            });
        }
    }

    pub(super) fn paint_rings(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            ..
        } = *spot;
        let dx = (nx - 0.5) * 1.8;
        let dy = ny - 0.5;
        let radius = (dx * dx + dy * dy).sqrt();
        let bands = fract(radius * 4.5 - time * 0.34);
        if bands < 0.085 {
            if blank {
                cell.set_symbol(if noise & 1 == 0 { "○" } else { "·" });
            }
            cell.set_fg(cycle_color(
                self.accent,
                self.secondary,
                self.foreground,
                fract(radius + time * 0.08),
            ));
        }
    }

    pub(super) fn paint_swarm(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            ..
        } = *spot;
        let cx = 0.5 + (time * 0.47).sin() * 0.32;
        let cy = 0.5 + (time * 0.71).cos() * 0.24;
        let distance = ((nx - cx) * 1.8).powi(2) + (ny - cy).powi(2);
        if blank && distance < 0.055 && noise % 100 < u64::from(self.density / 2) {
            cell.set_symbol(["·", "∙", "•"][(noise as usize) % 3])
                .set_fg(if noise & 1 == 0 {
                    self.accent
                } else {
                    self.secondary
                });
        }
    }

    pub(super) fn paint_synth_grid(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { nx, ny, blank, .. } = *spot;
        let horizon = 0.40;
        let below = ny - horizon;
        if blank && below >= 0.0 {
            let horizontal =
                wrapped_distance(fract((below * 10.0).sqrt() - time * 0.24), 0.0) < 0.055;
            let perspective = ((nx - 0.5) / below.max(0.04) * 0.16).abs();
            let vertical = wrapped_distance(fract(perspective), 0.0) < 0.045;
            if horizontal || vertical {
                cell.set_symbol(if horizontal {
                    "─"
                } else if nx < 0.5 {
                    "╱"
                } else {
                    "╲"
                })
                .set_fg(if horizontal && vertical {
                    self.foreground
                } else if horizontal {
                    self.secondary
                } else {
                    self.accent
                });
            }
        }
    }

    pub(super) fn paint_waves(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, reactive, .. } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            text,
            ..
        } = *spot;
        let response = if self.kind == CellEffect::WavesReactive {
            reactive
        } else {
            ReactiveAudio::default()
        };
        let wave = 0.5
            + (nx * (12.0 + response.bass * 4.0) + time * 1.7).sin() * (0.14 + response.rms * 0.16)
            + (nx * (23.0 + response.treble * 12.0) - time * 1.1).sin()
                * (0.055 + response.mid * 0.075);
        let distance = (ny - wave).abs();
        if blank && distance < 0.022 + response.bass * 0.012 {
            cell.set_symbol(if noise & 1 == 0 { "~" } else { "·" })
                .set_fg(cycle_color(
                    self.accent,
                    self.secondary,
                    self.foreground,
                    fract(nx * 0.35 + response.treble * 0.5 + time * 0.03),
                ));
        } else if text && distance < 0.065 + response.rms * 0.045 {
            // Colour only, with no BOLD. The compositor (`fade_cell`)
            // applies a changed modifier only on the cells where a fixed
            // hash of the position is below the editor's opacity. At a
            // low opacity a few characters of a word would go bold and
            // their neighbours would not. The colour blend applies to
            // every cell, so the glow stays smooth.
            cell.set_fg(blend_rgb(self.secondary, self.foreground, response.rms));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_visuals::test_support::*;
    use ratatui::style::Style;

    #[test]
    fn waves_reactive_uses_native_audio_while_waves_stays_deterministic() {
        let area = Rect::new(0, 0, 100, 30);
        let background = Color::Rgb(7, 7, 19);
        let active = showroom_test_visual(CellEffect::WavesReactive, background);
        let signal = Arc::new(ReactiveAudioSignal::default());
        let mut effect = ShowroomFx::new(&active, Arc::clone(&signal));
        effect.elapsed_ms = 1_300;
        let render = |effect: &ShowroomFx| {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            effect.render(&mut buffer, area);
            buffer
        };
        let silent = render(&effect);
        signal.store(ReactiveAudio {
            rms: 0.9,
            bass: 1.0,
            mid: 0.75,
            treble: 0.85,
        });
        let loud = render(&effect);
        assert_ne!(
            silent, loud,
            "audio changes wave height, harmonics, and colour"
        );
    }
}

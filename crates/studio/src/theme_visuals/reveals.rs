//! Reveals and sweeps: beams, spotlights, wipes, slides, slices and the
//! rest - effects that light, uncover or cross the text.

use super::*;

impl ShowroomFx {
    pub(super) fn paint_beams(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, density, .. } = *frame;
        let Spot { nx, ny, blank, .. } = *spot;
        let left = wrapped_distance(nx + ny * 0.55, fract(time * 0.22));
        let right = wrapped_distance((1.0 - nx) + ny * 0.55, fract(time * 0.19));
        let distance = left.min(right);
        if distance < 0.035 * density.max(0.25) {
            if blank {
                cell.set_symbol(if left < right { "╲" } else { "╱" });
            }
            cell.set_fg(if distance < 0.012 {
                self.foreground
            } else {
                self.accent
            });
            cell.modifier.insert(Modifier::BOLD);
        }
    }

    pub(super) fn paint_expand(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            blank,
            text,
            ..
        } = *spot;
        let dx = (nx - 0.5) * 1.7;
        let dy = ny - 0.5;
        let radius = (dx * dx + dy * dy).sqrt();
        let edge = fract(time * 0.15) * 0.85;
        if blank && (radius - edge).abs() < 0.025 {
            cell.set_symbol(if dx.abs() > dy.abs() { "─" } else { "│" })
                .set_fg(self.accent);
        } else if text && radius < edge {
            cell.set_fg(blend_rgb(self.muted, self.foreground, edge));
        }
    }

    pub(super) fn paint_highlight(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { nx, text, .. } = *spot;
        let beam = fract(time * 0.16) * 1.25 - 0.125;
        if text && (nx - beam).abs() < 0.08 {
            cell.set_fg(self.foreground).modifier.insert(Modifier::BOLD);
            if (nx - beam).abs() < 0.025 {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }

    pub(super) fn paint_laser_etch(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            ny, blank, text, ..
        } = *spot;
        let scan = fract(time * 0.13);
        if blank && (ny - scan).abs() < 0.014 {
            cell.set_symbol("─").set_fg(self.foreground);
            cell.modifier.insert(Modifier::BOLD);
        } else if text && (ny - scan).abs() < 0.045 {
            cell.set_fg(self.secondary).modifier.insert(Modifier::BOLD);
        } else if text && ny > scan {
            cell.set_fg(blend_rgb(cell.fg, self.muted, 0.45));
        }
    }

    pub(super) fn paint_middle_out(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx, blank, text, ..
        } = *spot;
        let edge = fract(time * 0.16) * 0.55;
        let distance = (nx - 0.5).abs();
        if blank && (distance - edge).abs() < 0.012 {
            cell.set_symbol("│").set_fg(self.accent);
        } else if text && distance < edge {
            cell.set_fg(self.foreground);
        } else if text {
            cell.set_fg(blend_rgb(cell.fg, self.muted, 0.38));
        }
    }

    pub(super) fn paint_overflow(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { x, ny, blank, .. } = *spot;
        let lane = hash(u64::from(x), 41, 73);
        let head = fract(time * (0.09 + (lane % 7) as f32 * 0.008) + (lane % 1000) as f32 / 1000.0);
        let distance = wrapped_distance(ny, head);
        if blank && distance < 0.035 && lane % 100 < u64::from(self.density) {
            cell.set_symbol(if distance < 0.012 { "▼" } else { "│" })
                .set_fg(if distance < 0.012 {
                    self.foreground
                } else {
                    self.accent
                });
        }
    }

    pub(super) fn paint_print(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { area, time, .. } = *frame;
        let Spot {
            x, y, blank, text, ..
        } = *spot;
        let total = u64::from(area.width) * u64::from(area.height);
        let cursor = ((time * 22.0) as u64) % total.max(1);
        let index = u64::from(y - area.y) * u64::from(area.width) + u64::from(x - area.x);
        if text && index > cursor {
            cell.set_fg(self.muted).modifier.insert(Modifier::DIM);
        } else if text && cursor.saturating_sub(index) < 8 {
            cell.set_fg(self.foreground).modifier.insert(Modifier::BOLD);
        } else if blank && index == cursor {
            cell.set_symbol("▌").set_fg(self.accent);
        }
    }

    pub(super) fn paint_slice(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { area, time, .. } = *frame;
        let Spot {
            y,
            py,
            nx,
            blank,
            text,
            ..
        } = *spot;
        let row_phase = fract(time * 0.14 + py * 0.071);
        let edge = if (y - area.y) & 1 == 0 {
            row_phase
        } else {
            1.0 - row_phase
        };
        if text && (nx - edge).abs() < 0.08 {
            cell.set_fg(self.secondary).modifier.insert(Modifier::BOLD);
        } else if blank && (nx - edge).abs() < 0.012 {
            cell.set_symbol(if (y - area.y) & 1 == 0 { "▶" } else { "◀" })
                .set_fg(self.accent);
        }
    }

    pub(super) fn paint_slide(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx, blank, text, ..
        } = *spot;
        let edge = fract(time * 0.12) * 1.2 - 0.1;
        if blank && (nx - edge).abs() < 0.014 {
            cell.set_symbol("»").set_fg(self.accent);
        } else if text && nx < edge && edge - nx < 0.16 {
            cell.set_fg(self.foreground).modifier.insert(Modifier::BOLD);
        }
    }

    pub(super) fn paint_spotlights(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            noise,
            blank,
            text,
            ..
        } = *spot;
        let first_x = 0.5 + (time * 0.43).sin() * 0.43;
        let second_x = 0.5 + (time * 0.31 + 2.1).sin() * 0.40;
        let lit = ((nx - first_x) / 0.16).powi(2) + ((ny - 0.55) / 0.75).powi(2) < 1.0
            || ((nx - second_x) / 0.13).powi(2) + ((ny - 0.45) / 0.68).powi(2) < 1.0;
        if text {
            cell.set_fg(if lit { self.foreground } else { self.muted });
            if lit {
                cell.modifier.insert(Modifier::BOLD);
            } else {
                cell.modifier.insert(Modifier::DIM);
            }
        } else if blank && lit && noise.is_multiple_of(173) {
            cell.set_symbol("·").set_fg(self.accent);
        }
    }

    pub(super) fn paint_sweep(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { nx, ny, blank, .. } = *spot;
        let mut nearest: Option<(f32, usize)> = None;
        for pass in 0..5usize {
            let seed = hash(pass as u64, 0x7377_6565_705f_7061, 0x7373);
            let interval = 1.4 + hash_unit(seed.rotate_left(11)) * 4.8;
            let phase = fract(time / interval + hash_unit(seed.rotate_left(29)));
            let edge = phase * 1.3 - 0.15;
            let position = match pass {
                0 => nx,
                1 => 1.0 - nx,
                2 => ny,
                3 => nx * 0.72 + ny * 0.28,
                _ => (1.0 - nx) * 0.64 + ny * 0.36,
            };
            let distance = (position - edge).abs();
            if nearest.is_none_or(|(closest, _)| distance < closest) {
                nearest = Some((distance, pass));
            }
        }
        if let Some((distance, pass)) = nearest
            && distance < 0.055
        {
            if blank && distance < 0.011 {
                cell.set_symbol(match pass {
                    0 | 1 => "│",
                    2 => "─",
                    3 => "╲",
                    _ => "╱",
                });
            }
            cell.set_fg(if distance < 0.014 {
                self.foreground
            } else if pass & 1 == 0 {
                self.accent
            } else {
                self.secondary
            });
            if distance < 0.018 {
                cell.modifier.insert(Modifier::BOLD);
            }
        }
    }

    pub(super) fn paint_wipe(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            nx,
            ny,
            blank,
            text,
            ..
        } = *spot;
        let edge = fract(time * 0.11);
        let position = nx * 0.72 + ny * 0.28;
        if blank && (position - edge).abs() < 0.014 {
            cell.set_symbol("╱").set_fg(self.foreground);
        } else if text && position > edge {
            cell.set_fg(self.muted).modifier.insert(Modifier::DIM);
        } else if text && edge - position < 0.055 {
            cell.set_fg(self.accent).modifier.insert(Modifier::BOLD);
        }
    }
}

//! Weather: rain, snow and thunder over the code.

use super::*;

impl ShowroomFx {
    pub(super) fn paint_rain(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { x, ny, blank, .. } = *spot;
        let lane = hash(u64::from(x), 101, 151);
        let head = fract(time * (0.14 + (lane % 9) as f32 * 0.008) + (lane % 997) as f32 / 997.0);
        if blank && wrapped_distance(ny, head) < 0.028 && lane % 100 < u64::from(self.density) {
            cell.set_symbol(if lane & 1 == 0 { "│" } else { "╷" })
                .set_fg(blend_rgb(self.accent, self.foreground, 1.0 - ny));
        }
    }

    pub(super) fn paint_snow(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { height, time, .. } = *frame;
        let Spot {
            x, nx, ny, blank, ..
        } = *spot;
        let mut flake = None;
        for layer in 0..5u64 {
            let lane = hash(u64::from(x), layer, 0x736e_6f77_6661_6c6c);
            if lane % 100 >= 42 + u64::from(self.density) / 2 {
                continue;
            }
            let depth = (layer as f32 + 1.0) / 5.0;
            let fall = fract(
                time * (0.075 + depth * 0.105)
                    + hash_unit(lane.rotate_left(17))
                    + nx * (0.04 + depth * 0.09),
            );
            let cell_y = (ny - fall).abs();
            if cell_y < (0.42 + depth * 0.32) / height {
                flake = Some((depth, lane));
                break;
            }
        }
        if blank && let Some((depth, lane)) = flake {
            cell.set_symbol(match (depth * 4.0).round() as u8 {
                0 | 1 => "·",
                2 => "∙",
                3 => "*",
                _ => "•",
            });
            let shade = cycle_color(
                self.muted,
                self.accent,
                self.foreground,
                hash_unit(lane.rotate_left(31)) * 0.35 + depth * 0.65,
            );
            cell.set_fg(shade);
            if depth < 0.35 {
                cell.modifier.insert(Modifier::DIM);
            } else if depth > 0.78 {
                cell.modifier.insert(Modifier::BOLD);
            }
        }
    }

    pub(super) fn paint_thunderstorm(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame {
            area, time, step, ..
        } = *frame;
        let Spot {
            x,
            y,
            py,
            nx,
            ny,
            blank,
            text,
            ..
        } = *spot;
        let flash = fract(time * 0.17);
        let lane = hash(u64::from(x), 191, 223);
        let drop = fract(time * (0.18 + (lane % 9) as f32 * 0.01) + (lane % 991) as f32 / 991.0);
        let bolt_x = 0.48 + ((py * 1.7 + step as f32).sin() * 0.08);
        if flash < 0.045 && text {
            cell.set_fg(self.foreground).modifier.insert(Modifier::BOLD);
        } else if blank && flash < 0.13 && (nx - bolt_x).abs() < 0.012 && ny < 0.78 {
            cell.set_symbol(if (y - area.y) & 1 == 0 { "╲" } else { "╱" })
                .set_fg(self.foreground);
            cell.modifier.insert(Modifier::BOLD);
        } else if blank
            && wrapped_distance(ny, drop) < 0.022
            && lane % 100 < u64::from(self.density)
        {
            cell.set_symbol("╷").set_fg(self.accent);
        }
    }
}

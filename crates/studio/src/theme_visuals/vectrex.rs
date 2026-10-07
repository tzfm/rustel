//! The Vectrex flight: a vector tunnel of depth rings and stars.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct VectrexFlight {
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) secondary: Color,
    pub(super) muted: Color,
    pub(super) density: u8,
    pub(super) speed: u8,
}

impl VectrexFlight {
    pub(super) fn new(active: &ActiveVisual) -> Self {
        Self {
            background: active.background,
            foreground: active.foreground,
            accent: active.accent,
            secondary: active.secondary,
            muted: active.muted,
            density: active.config.density,
            speed: active.config.speed,
        }
    }
}

impl NativeScene for VectrexFlight {
    fn name(&self) -> &'static str {
        "rustel_vectrex_flight"
    }

    fn render(&self, elapsed_ms: u64, _audio: ReactiveAudio, buffer: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        let width = f32::from(area.width.max(1));
        let height = f32::from(area.height.max(1));
        let aspect = (width * 0.5 / height).clamp(0.6, 3.0);
        let stroke = (0.55 / height).max(0.006);
        let cell_x = 0.52 / width;
        let cell_y = 0.52 / height;
        let time = elapsed_ms as f32 / 1000.0;
        let travel = time * (0.07 + f32::from(self.speed) * 0.0018);
        let vanish = Point2 { x: 0.5, y: 0.43 };
        let rails = [
            Point2 { x: 0.01, y: 0.02 },
            Point2 { x: 0.99, y: 0.02 },
            Point2 { x: 0.01, y: 0.98 },
            Point2 { x: 0.99, y: 0.98 },
        ];

        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let Some(cell) = buffer.cell_mut((x, y)) else {
                    continue;
                };
                if cell.bg != self.background
                    || (cell.symbol() != " " && cell.symbol().width() != 1)
                {
                    continue;
                }
                let blank = cell.symbol() == " ";
                let point = Point2 {
                    x: (f32::from(x - area.x) + 0.5) / width,
                    y: (f32::from(y - area.y) + 0.5) / height,
                };
                let mut glyph = None;
                let mut color = self.muted;

                // Vectrex scenes were sparse by necessity. Widely spaced
                // frames give the flight its depth without becoming a wall
                // of parallel lines over the score.
                const RINGS: usize = 5;
                for ring in 0..RINGS {
                    let depth = fract(ring as f32 / RINGS as f32 + travel).powf(1.48);
                    let half_width = 0.025 + depth * 0.47;
                    let half_height = 0.018 + depth * 0.47;
                    let dx = (point.x - vanish.x).abs();
                    let dy = (point.y - vanish.y).abs();
                    let on_vertical =
                        (dx - half_width).abs() < cell_x && dy <= half_height + cell_y;
                    let on_horizontal =
                        (dy - half_height).abs() < cell_y && dx <= half_width + cell_x;
                    if on_vertical || on_horizontal {
                        glyph = Some(match (on_horizontal, on_vertical) {
                            (true, true) => "◇",
                            (true, false) => "─",
                            (false, true) => "│",
                            _ => "·",
                        });
                        color = cycle_color(
                            self.accent,
                            self.secondary,
                            self.foreground,
                            depth * 0.7 + travel * 0.08,
                        );
                        break;
                    }
                }

                if glyph.is_none() {
                    for (index, edge) in rails.iter().enumerate() {
                        if segment_distance(point, vanish, *edge, aspect) < stroke * 0.72 {
                            glyph =
                                Some(if (edge.x - vanish.x).abs() > (edge.y - vanish.y).abs() {
                                    if edge.x < vanish.x { "╲" } else { "╱" }
                                } else {
                                    "│"
                                });
                            color = if index & 1 == 0 {
                                self.accent
                            } else {
                                self.secondary
                            };
                            break;
                        }
                    }
                }

                if glyph.is_none() {
                    let stars = 8 + usize::from(self.density) / 5;
                    for index in 0..stars {
                        let seed = hash(index as u64, 0x7665_6374, 0x7265_785f_7374_6172);
                        let depth = fract(
                            travel * (0.7 + hash_unit(seed) * 0.8)
                                + hash_unit(seed.rotate_left(13)),
                        );
                        let angle = hash_unit(seed.rotate_left(29)) * std::f32::consts::TAU;
                        let sx = vanish.x + angle.cos() * depth.powi(2) * 0.62 / aspect;
                        let sy = vanish.y + angle.sin() * depth.powi(2) * 0.56;
                        if ((point.x - sx) * aspect).abs() < stroke * 0.55
                            && (point.y - sy).abs() < stroke * 0.55
                        {
                            glyph = Some(if depth > 0.75 { "+" } else { "·" });
                            color = blend_rgb(self.muted, self.foreground, depth);
                            break;
                        }
                    }
                }

                if let Some(glyph) = glyph {
                    if blank {
                        cell.set_symbol(glyph);
                    }
                    cell.set_fg(color);
                    cell.modifier.insert(Modifier::BOLD);
                }
            }
        }
    }
}

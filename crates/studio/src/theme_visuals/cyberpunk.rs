//! The cyberpunk landscape: a sun over a grid, lines racing toward it.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct CyberpunkLandscape {
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) secondary: Color,
    pub(super) muted: Color,
    pub(super) density: u8,
    pub(super) speed: u8,
}

impl CyberpunkLandscape {
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

impl NativeScene for CyberpunkLandscape {
    fn name(&self) -> &'static str {
        "rustel_cyberpunk_landscape"
    }

    fn render(&self, elapsed_ms: u64, _audio: ReactiveAudio, buffer: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        let width = f32::from(area.width.max(1));
        let height = f32::from(area.height.max(1));
        let aspect = (width * 0.5 / height).clamp(0.6, 3.0);
        // A thin stroke. In image mode a lit cell sets its background, so
        // every line is a solid bar behind the code. A thin stroke and few
        // lanes keep the landscape and let the score read through it.
        let stroke = (0.42 / height).max(0.006);
        let time = elapsed_ms as f32 / 1000.0;
        let motion = time * (0.055 + f32::from(self.speed) * 0.0015);
        let horizon = 0.43;

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
                let nx = (f32::from(x - area.x) + 0.5) / width;
                let ny = (f32::from(y - area.y) + 0.5) / height;
                // A few two-row bands shear sideways on a shared tick. The
                // geometry is still one coherent landscape; only those scan
                // bands momentarily take the cyan/magenta VHS separation.
                let glitch_tick = (time * (2.0 + f32::from(self.speed) * 0.04)) as u64;
                let glitch_seed = hash(
                    u64::from((y - area.y) / 2),
                    glitch_tick,
                    0x6379_6265_7276_6873,
                );
                let glitched = glitch_seed % 1_000 < 35 + u64::from(self.density) / 2;
                let sample_x = if glitched {
                    (nx + (hash_unit(glitch_seed.rotate_left(23)) - 0.5) * 0.09).rem_euclid(1.0)
                } else {
                    nx
                };

                if ny < horizon {
                    let ridge = horizon
                        - 0.035
                        - ((sample_x * 7.0 + 0.4).sin().abs() * 0.105
                            + (sample_x * 19.0 - 0.7).sin().abs() * 0.045);
                    let ridge_back = horizon
                        - 0.012
                        - ((sample_x * 5.0 - 0.9).sin().abs() * 0.065
                            + (sample_x * 27.0).sin().abs() * 0.018);
                    if (ny - ridge).abs() < stroke || (ny - ridge_back).abs() < stroke * 0.72 {
                        if blank {
                            let slope = ((sample_x + 1.0 / width) * 7.0 + 0.4).sin().abs()
                                - ((sample_x - 1.0 / width) * 7.0 + 0.4).sin().abs();
                            cell.set_symbol(if slope > 0.02 {
                                "╱"
                            } else if slope < -0.02 {
                                "╲"
                            } else {
                                "─"
                            });
                        }
                        cell.set_fg(if glitched {
                            cycle_color(
                                self.accent,
                                self.secondary,
                                self.foreground,
                                hash_unit(glitch_seed),
                            )
                        } else if (ny - ridge).abs() < stroke {
                            self.secondary
                        } else {
                            self.accent
                        });
                        cell.modifier.insert(Modifier::BOLD);
                        continue;
                    }

                    let sun_x = (sample_x - 0.5) / 0.085;
                    let sun_y = (ny - 0.235) / 0.115;
                    if sun_x * sun_x + sun_y * sun_y <= 1.0 {
                        let stripe = ((ny - 0.12) * height * 0.72).floor() as i32;
                        if stripe.rem_euclid(3) != 1 {
                            if blank {
                                cell.set_symbol("━");
                            }
                            cell.set_fg(blend_rgb(
                                self.secondary,
                                self.accent,
                                ((ny - 0.12) / 0.23).clamp(0.0, 1.0),
                            ));
                            cell.modifier.insert(Modifier::BOLD);
                        }
                        continue;
                    }

                    let stars = 18 + usize::from(self.density) / 2;
                    let mut star = None;
                    for index in 0..stars {
                        let seed = hash(index as u64, 0x7374_6172, 0x0000_0066_6965_6c64);
                        let depth = fract(
                            motion * (0.42 + hash_unit(seed) * 0.55)
                                + hash_unit(seed.rotate_left(17)),
                        );
                        let angle = std::f32::consts::PI
                            + hash_unit(seed.rotate_left(31)) * std::f32::consts::PI;
                        let radius = depth.powf(1.65);
                        let sx = 0.5 + angle.cos() * radius * 0.64 / aspect;
                        let sy = horizon - angle.sin().abs() * radius * 0.44;
                        if ((sample_x - sx) * aspect).abs() < stroke * 0.65
                            && (ny - sy).abs() < stroke * 0.65
                        {
                            star = Some(depth);
                            break;
                        }
                    }
                    if let Some(depth) = star {
                        if blank {
                            cell.set_symbol(if depth > 0.72 { "*" } else { "·" });
                        }
                        cell.set_fg(blend_rgb(self.muted, self.foreground, depth));
                    }
                    continue;
                }

                let depth = ((ny - horizon) / (1.0 - horizon)).clamp(0.0, 1.0);
                let mut horizontal = false;
                for lane in 0..10 {
                    let distance = fract(lane as f32 / 10.0 - motion);
                    let projected =
                        ((1.0 / (0.12 + distance)) - (1.0 / 1.12)) / ((1.0 / 0.12) - (1.0 / 1.12));
                    let line_y = horizon + (1.0 - horizon) * projected;
                    if (ny - line_y).abs() < stroke * (0.6 + depth * 0.42) {
                        horizontal = true;
                        break;
                    }
                }
                let mut vertical = false;
                for lane in -8..=8 {
                    let edge = 0.5 + lane as f32 / 8.0 * 0.72;
                    let line_x = 0.5 + (edge - 0.5) * depth;
                    if ((sample_x - line_x) * aspect).abs() < stroke * 0.72 {
                        vertical = true;
                        break;
                    }
                }
                if horizontal || vertical {
                    if blank {
                        cell.set_symbol(match (horizontal, vertical) {
                            (true, true) => "┼",
                            (true, false) => "─",
                            (false, true) if sample_x < 0.5 => "╲",
                            (false, true) => "╱",
                            _ => "·",
                        });
                    }
                    cell.set_fg(if glitched {
                        cycle_color(
                            self.accent,
                            self.secondary,
                            self.foreground,
                            hash_unit(glitch_seed),
                        )
                    } else if horizontal && vertical {
                        self.foreground
                    } else if horizontal {
                        self.accent
                    } else {
                        self.secondary
                    });
                    if depth > 0.64 {
                        cell.modifier.insert(Modifier::BOLD);
                    }
                }
            }
        }
    }
}

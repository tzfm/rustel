//! The rustel brand: the pixel logo, drifting, pulsing and tearing with
//! the music.

use super::*;

pub(super) const RUSTEL_LOGO: &[u8] = include_bytes!("../themes/rustel-logo.txt");
pub(super) const RUSTEL_LOGO_WIDTH: usize = 74;
pub(super) const RUSTEL_LOGO_HEIGHT: usize = 18;
pub(super) const RUSTEL_LOGO_STRIDE: usize = RUSTEL_LOGO_WIDTH + 1;

pub(super) fn rustel_logo_pixel(x: usize, y: usize) -> Option<u8> {
    (x < RUSTEL_LOGO_WIDTH && y < RUSTEL_LOGO_HEIGHT)
        .then(|| RUSTEL_LOGO[y * RUSTEL_LOGO_STRIDE + x])
        .filter(|pixel| *pixel != b'.')
}

#[derive(Clone, Debug)]
pub(super) struct RustelBrandScene {
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) muted: Color,
    pub(super) density: u8,
    pub(super) speed: u8,
}

impl RustelBrandScene {
    const MAGENTA: Color = Color::Rgb(0xf2, 0x02, 0xf7);

    pub(super) fn new(active: &ActiveVisual) -> Self {
        Self {
            background: active.background,
            foreground: active.foreground,
            accent: active.accent,
            muted: active.muted,
            density: active.config.density,
            speed: active.config.speed,
        }
    }
}

impl NativeScene for RustelBrandScene {
    fn name(&self) -> &'static str {
        "rustel_brand_logo"
    }

    fn render(&self, elapsed_ms: u64, audio: ReactiveAudio, buffer: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        let width = f32::from(area.width.max(1));
        let height = f32::from(area.height.max(1));
        let time = elapsed_ms as f32 / 1000.0;
        let tempo = 0.16 + f32::from(self.speed) * 0.0022;
        let pulse = 0.72 + audio.bass * 0.16 + audio.rms * 0.1;
        // `clamp` panics when min is greater than max. On a frame narrower
        // or shorter than the logo's floor, hold the cap at the floor and
        // let the rasteriser clip.
        let width_cap = (width * 0.92).max(28.0);
        let logo_width = (width * pulse).clamp(28.0f32.min(width_cap), width_cap);
        let height_cap = (height * 0.68).max(7.0);
        let logo_height = (logo_width * RUSTEL_LOGO_HEIGHT as f32 / RUSTEL_LOGO_WIDTH as f32)
            .clamp(7.0f32.min(height_cap), height_cap);
        let center_x = width * (0.5 + (time * tempo).sin() * (0.08 + audio.mid * 0.035));
        let center_y =
            height * (0.5 + (time * tempo * 1.37 + 1.1).cos() * (0.1 + audio.treble * 0.025));
        let left = center_x - logo_width * 0.5;
        let top = center_y - logo_height * 0.5;
        let tick = (time * (8.0 + audio.treble * 22.0)) as u64;

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
                let px = f32::from(x - area.x) + 0.5;
                let py = f32::from(y - area.y) + 0.5;
                let local_y = (py - top) / logo_height;
                let row_seed = hash(u64::from(y - area.y), tick, 0x7275_7374_656c_6c6f);
                let tear = if row_seed % 1_000 < (audio.treble * 130.0) as u64 {
                    (hash_unit(row_seed.rotate_left(19)) - 0.5) * 0.11
                } else {
                    0.0
                };
                let warp = (local_y * 11.0 + time * 1.3).sin() * audio.mid * 0.012;
                let local_x = (px - left) / logo_width + tear + warp;
                let logo_pixel = if (0.0..1.0).contains(&local_x) && (0.0..1.0).contains(&local_y) {
                    rustel_logo_pixel(
                        (local_x * RUSTEL_LOGO_WIDTH as f32) as usize,
                        (local_y * RUSTEL_LOGO_HEIGHT as f32) as usize,
                    )
                } else {
                    None
                };

                if let Some(pixel) = logo_pixel {
                    let base = match pixel {
                        b'M' => Self::MAGENTA,
                        b'C' => self.accent,
                        _ => self.foreground,
                    };
                    let energy = match pixel {
                        b'M' => audio.mid,
                        b'C' => audio.bass,
                        _ => audio.rms,
                    };
                    if cell.symbol() == " " {
                        cell.set_symbol("█");
                    }
                    cell.set_fg(blend_rgb(base, self.foreground, energy * 0.32));
                    cell.modifier.insert(Modifier::BOLD);
                    continue;
                }

                let nx = px / width;
                let ny = py / height;
                let distance = (((nx - center_x / width) * 1.7).powi(2)
                    + (ny - center_y / height).powi(2))
                .sqrt();
                let spark = hash(u64::from(x), u64::from(y), tick ^ 0x0062_7261_6e64_u64);
                let spark_limit = 3
                    + u64::from(self.density) / 12
                    + (audio.treble * 42.0 + audio.rms * 18.0) as u64;
                if cell.symbol() == " " && distance < 0.58 && spark % 1_000 < spark_limit {
                    cell.set_symbol(if spark & 3 == 0 { "+" } else { "·" });
                    cell.set_fg(if spark & 1 == 0 {
                        blend_rgb(self.muted, self.accent, 0.55 + audio.treble * 0.45)
                    } else {
                        blend_rgb(self.muted, Self::MAGENTA, 0.55 + audio.mid * 0.45)
                    });
                }
            }
        }
    }
}

//! Glitches: decrypts, corrections, colour shifts, crumbles, tape noise -
//! effects that disturb the text itself.

use super::*;

impl ShowroomFx {
    pub(super) fn paint_color_shift(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { nx, ny, text, .. } = *spot;
        if text {
            let phase = fract(nx * 0.8 + ny * 0.3 + time * 0.12);
            cell.set_fg(cycle_color(
                self.accent,
                self.secondary,
                self.foreground,
                phase,
            ));
        }
    }

    pub(super) fn paint_crumble(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, density, .. } = *frame;
        let Spot {
            ny,
            noise,
            blank,
            text,
            ..
        } = *spot;
        let threshold = fract(time * 0.11);
        let rank = (noise % 1000) as f32 / 1000.0;
        if text && wrapped_distance(rank, threshold) < 0.07 * density {
            cell.set_symbol(["·", "▪", "▫", "░"][(noise as usize) % 4])
                .set_fg(blend_rgb(self.accent, self.muted, ny));
        } else if blank
            && noise % 100 < u64::from(self.density / 3)
            && wrapped_distance(ny, fract(threshold + rank * 0.2)) < 0.02
        {
            cell.set_symbol("·").set_fg(self.muted);
        }
    }

    pub(super) fn paint_decrypt(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot { noise, text, .. } = *spot;
        if text {
            let reveal = fract(time * 0.10);
            let rank = (noise % 1000) as f32 / 1000.0;
            if rank > reveal && rank - reveal < 0.45 {
                cell.set_symbol(DIGITAL_GLYPHS[(noise as usize) % DIGITAL_GLYPHS.len()])
                    .set_fg(if rank - reveal < 0.08 {
                        self.foreground
                    } else {
                        self.accent
                    });
            }
        }
    }

    pub(super) fn paint_error_correct(&self, _frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Spot { noise, text, .. } = *spot;
        if text && noise % 100 < u64::from((self.density / 8).max(2)) {
            if noise & 1 == 0 {
                cell.set_symbol(DIGITAL_GLYPHS[(noise as usize) % DIGITAL_GLYPHS.len()]);
            }
            cell.set_fg(self.secondary)
                .modifier
                .insert(Modifier::UNDERLINED);
        } else if text && noise.is_multiple_of(19) {
            cell.set_fg(self.accent);
        }
    }

    pub(super) fn paint_random_sequence(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, .. } = *frame;
        let Spot {
            x, y, nx, ny, text, ..
        } = *spot;
        let phase = fract(time * 0.12);
        let rank = (hash(u64::from(x), u64::from(y), 0) % 1000) as f32 / 1000.0;
        if text {
            let rainbow = cycle_color(
                Color::Rgb(255, 72, 142),
                Color::Rgb(80, 240, 255),
                Color::Rgb(255, 226, 92),
                fract(nx * 0.8 + ny * 0.24 - time * 0.28),
            );
            cell.set_fg(rainbow);
            if wrapped_distance(rank, phase) < 0.055 {
                cell.modifier.insert(Modifier::BOLD);
            }
        }
    }

    pub(super) fn paint_scattered(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame { time, density, .. } = *frame;
        let Spot {
            noise, blank, text, ..
        } = *spot;
        let phase = fract(time * 0.14);
        let rank = (noise % 1000) as f32 / 1000.0;
        if text && wrapped_distance(rank, phase) < 0.09 * density {
            cell.set_symbol(["·", "∙", "▪"][(noise as usize) % 3])
                .set_fg(self.accent);
        } else if blank
            && noise % 100 < u64::from(self.density / 5)
            && wrapped_distance(rank, phase) < 0.045
        {
            cell.set_symbol("·").set_fg(self.muted);
        }
    }

    pub(super) fn paint_unstable(&self, _frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Spot { noise, text, .. } = *spot;
        if text && noise % 100 < u64::from((self.density / 7).max(3)) {
            if cell.symbol().is_ascii() && noise & 1 == 0 {
                let character = cell.symbol().chars().next().unwrap_or(' ');
                cell.set_char(if character.is_ascii_uppercase() {
                    character.to_ascii_lowercase()
                } else {
                    character.to_ascii_uppercase()
                });
            } else {
                cell.set_symbol(DIGITAL_GLYPHS[(noise as usize) % DIGITAL_GLYPHS.len()]);
            }
            cell.set_fg(self.secondary).modifier.insert(Modifier::BOLD);
        }
    }

    pub(super) fn paint_vhs_tape(&self, frame: &Frame, spot: &Spot, cell: &mut Cell) {
        let Frame {
            area, time, step, ..
        } = *frame;
        let Spot {
            y, ny, noise, text, ..
        } = *spot;
        let scan = fract(time * 0.18);
        let mut damaged = None;
        for tear in 0..3u64 {
            let tear_noise = hash(step / 2, 307 + tear, 331 + tear * 17);
            let row = tear_noise % u64::from(area.height.max(1));
            let distance = u64::from(y - area.y).abs_diff(row);
            if distance <= u64::from(tear == 0) {
                damaged = Some(tear_noise);
                break;
            }
        }
        if text && wrapped_distance(ny, scan) < 0.034 {
            cell.set_fg(self.foreground)
                .modifier
                .insert(Modifier::REVERSED);
        } else if let Some(tear_noise) = damaged
            && noise % 100 < ((u64::from(self.density) * 3 / 4) + 18).min(96)
        {
            // Half the torn rows are drawn light. Keyed on
            // the row alone, so a row keeps its weight for as
            // long as it is torn instead of flickering with
            // `step`, and the weight varies down the screen.
            let thin = hash(u64::from(y), 0x7668_735f_7765_6967, 0x6874) & 1 == 0;
            if text && !noise.is_multiple_of(5) {
                cell.set_symbol(DIGITAL_GLYPHS[(noise as usize) % DIGITAL_GLYPHS.len()]);
            } else if thin {
                cell.set_symbol("─");
            } else {
                cell.set_symbol(if tear_noise & 1 == 0 { "━" } else { "═" });
            }
            let ink = if noise.wrapping_add(tear_noise) & 1 == 0 {
                self.accent
            } else {
                self.secondary
            };
            if thin {
                cell.set_fg(blend_rgb(ink, self.muted, 0.42));
            } else {
                cell.set_fg(ink);
                cell.modifier.insert(Modifier::BOLD);
            }
        } else if (y - area.y).is_multiple_of(2) && text {
            cell.set_fg(blend_rgb(cell.fg, self.muted, 0.34));
        }
    }
}

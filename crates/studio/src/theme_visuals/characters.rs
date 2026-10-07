//! The character effects: what a theme does to the glyphs themselves -
//! a fade, an aurora, a hologram - over whichever surfaces it names.

use super::super::theme::hsv;
use super::*;

#[derive(Clone, Copy)]
pub(super) struct CharacterPoint {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) nx: f32,
    pub(super) ny: f32,
    pub(super) seed: u64,
    pub(super) ground: Color,
}

pub(super) trait CharacterShader: std::fmt::Debug + Send {
    fn background(&self) -> Color;
    fn paint(&self, elapsed: f32, point: CharacterPoint, cell: &mut Cell);
}

pub(super) fn character_effect(active: &ActiveCharacters) -> Box<dyn CharacterShader> {
    match active.config.effect {
        CharacterEffect::Fade => Box::new(FadeCharacters(active.clone())),
        CharacterEffect::Aurora => Box::new(AuroraCharacters(active.clone())),
        CharacterEffect::Hologram => Box::new(HologramCharacters(active.clone())),
        CharacterEffect::Rainbow => Box::new(RainbowCharacters(active.clone())),
    }
}

/// Every hue across the text, rolling with time: the glyphs keep their
/// weight and take the colour of where they stand.
#[derive(Debug)]
pub(super) struct RainbowCharacters(ActiveCharacters);

impl CharacterShader for RainbowCharacters {
    fn background(&self) -> Color {
        self.0.background
    }

    fn paint(&self, elapsed: f32, point: CharacterPoint, cell: &mut Cell) {
        let strength = f32::from(self.0.config.strength) / 100.0;
        let speed = f32::from(self.0.config.speed) / 100.0;
        let hue = fract(point.nx * 0.9 + point.ny * 0.15 - elapsed * (0.02 + speed * 0.14));
        cell.fg = blend_rgb(cell.fg, hsv(hue * 360.0, 1.0, 1.0), strength * 0.92);
    }
}

#[derive(Debug)]
pub(super) struct FadeCharacters(ActiveCharacters);

impl CharacterShader for FadeCharacters {
    fn background(&self) -> Color {
        self.0.background
    }

    fn paint(&self, elapsed: f32, point: CharacterPoint, cell: &mut Cell) {
        let strength = f32::from(self.0.config.strength) / 100.0;
        let speed = f32::from(self.0.config.speed) / 100.0;
        let phase = fract(
            elapsed * (0.16 + speed * 0.58)
                + hash_unit(point.seed) * 0.82
                + point.nx * 0.13
                + point.ny * 0.19,
        );
        let pulse = ((phase * std::f32::consts::TAU).sin() * 0.5 + 0.5).powf(1.35);
        let visibility = 1.0 - strength * (1.0 - pulse) * 0.94;
        cell.fg = blend_rgb(point.ground, cell.fg, visibility);
        if visibility < 0.32 {
            cell.modifier.insert(Modifier::DIM);
        }
    }
}

#[derive(Debug)]
pub(super) struct AuroraCharacters(ActiveCharacters);

impl CharacterShader for AuroraCharacters {
    fn background(&self) -> Color {
        self.0.background
    }

    fn paint(&self, elapsed: f32, point: CharacterPoint, cell: &mut Cell) {
        let strength = f32::from(self.0.config.strength) / 100.0;
        let speed = f32::from(self.0.config.speed) / 100.0;
        let forward = ((point.nx * 1.35 + point.ny * 0.42 - elapsed * (0.07 + speed * 0.2))
            * std::f32::consts::TAU)
            .sin()
            * 0.5
            + 0.5;
        let returning = ((point.ny * 1.7 - point.nx * 0.31 + elapsed * (0.05 + speed * 0.16))
            * std::f32::consts::TAU)
            .cos()
            * 0.5
            + 0.5;
        let crest = (forward * returning).sqrt();
        let target = cycle_color(
            self.0.accent,
            self.0.secondary,
            self.0.foreground,
            point.nx * 0.72 + point.ny * 0.28 + elapsed * (0.025 + speed * 0.09),
        );
        cell.fg = blend_rgb(cell.fg, target, strength * (0.16 + crest * 0.78));
        if crest > 0.86 && strength > 0.45 {
            cell.modifier.insert(Modifier::BOLD);
        }
    }
}

#[derive(Debug)]
pub(super) struct HologramCharacters(ActiveCharacters);

impl CharacterShader for HologramCharacters {
    fn background(&self) -> Color {
        self.0.background
    }

    fn paint(&self, elapsed: f32, point: CharacterPoint, cell: &mut Cell) {
        let strength = f32::from(self.0.config.strength) / 100.0;
        let speed = f32::from(self.0.config.speed) / 100.0;
        let tick = (elapsed * (5.0 + speed * 12.0)) as u64;
        let scan = fract(elapsed * (0.08 + speed * 0.16));
        let beam = (1.0 - wrapped_distance(point.ny, scan) / (0.025 + strength * 0.055)).max(0.0);
        let row_noise = hash(u64::from(point.y), tick, 0x0068_6f6c_6f72_6f77);
        let interference = row_noise % 1_000 < u64::from(self.0.config.strength) * 2;
        let flicker = 0.65 + hash_unit(hash(point.seed, tick, row_noise)) * 0.35;
        let target = if interference {
            if (u64::from(point.x) + tick) & 1 == 0 {
                self.0.accent
            } else {
                self.0.secondary
            }
        } else {
            blend_rgb(self.0.muted, self.0.foreground, 0.58 + beam * 0.42)
        };
        let interference_gain = if interference { 0.72 } else { 0.0 };
        let amount = strength * (0.08 + beam * 0.84 + interference_gain).min(1.0) * flicker;
        cell.fg = blend_rgb(cell.fg, target, amount);
        if beam > 0.58 || (interference && (point.x + point.y) & 1 == 0) {
            cell.modifier.insert(Modifier::BOLD);
        }
    }
}

pub(super) fn paint_character_effect(
    shader: &dyn CharacterShader,
    elapsed: f32,
    buffer: &mut Buffer,
    area: Rect,
    surfaces: CharacterVisualSurfaces<'_>,
) {
    let visual_strength = surfaces.strength.clamp(0.0, 1.0);
    if visual_strength <= 0.0 {
        return;
    }
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return;
    }
    let width = f32::from(area.width.max(1));
    let height = f32::from(area.height.max(1));

    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if inside_any(surfaces.interface, x, y)
                || inside_any(surfaces.protected, x, y)
                || surfaces.selected.contains(&(x, y))
                || surfaces.highlighted.contains(&(x, y))
            {
                continue;
            }
            let Some(cell) = buffer.cell_mut((x, y)) else {
                continue;
            };
            if cell.symbol() == " "
                || cell.symbol().width() != 1
                || cell
                    .modifier
                    .intersects(Modifier::REVERSED | Modifier::HIDDEN)
            {
                continue;
            }

            let nx = f32::from(x - area.x) / width;
            let ny = f32::from(y - area.y) / height;
            let seed = hash(u64::from(x), u64::from(y), 0x6368_6172_6678);
            let ground = if cell.bg == Color::Reset {
                shader.background()
            } else {
                cell.bg
            };
            let original_foreground = cell.fg;
            let original_modifier = cell.modifier;
            shader.paint(
                elapsed,
                CharacterPoint {
                    x,
                    y,
                    nx,
                    ny,
                    seed,
                    ground,
                },
                cell,
            );
            if visual_strength < 1.0 {
                cell.fg = blend_rgb(original_foreground, cell.fg, visual_strength);
                let dither = hash_unit(hash(seed, (elapsed * 30.0) as u64, 0x006f_7061_6369_7479));
                if dither >= visual_strength {
                    cell.modifier = original_modifier;
                }
            }
        }
    }
}

pub(super) fn fade_cell(cell: &mut Cell, effect: &Cell, strength: f32, dither: f32) {
    let original_symbol_is_blank = cell.symbol() == " ";
    let effect_symbol = effect.symbol();
    let symbol_changed = cell.symbol() != effect_symbol;
    let foreground_base = if symbol_changed && original_symbol_is_blank {
        cell.bg
    } else {
        cell.fg
    };
    cell.fg = blend_rgb(foreground_base, effect.fg, strength);
    cell.bg = blend_rgb(cell.bg, effect.bg, strength);
    if symbol_changed && original_symbol_is_blank {
        // A line or particle owns no source glyph, so opacity belongs in its
        // colour. Dropping half its cells turns a vector, grid or logo into a
        // perforated shape instead of a dimmer one.
        cell.set_symbol(effect_symbol);
    } else if dither < strength {
        if symbol_changed {
            cell.set_symbol(effect_symbol);
        }
        cell.modifier = effect.modifier;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn character_scenes_move_each_glyph_but_spare_editor_state_and_chrome() {
        let area = Rect::new(0, 0, 24, 4);
        let interface = Rect::new(0, 0, 4, 4);
        let overlay = Rect::new(20, 0, 4, 4);
        let selected = (10, 2);
        let mut baseline = Buffer::empty(area);
        baseline.set_style(
            area,
            Style::default()
                .bg(Color::Rgb(8, 10, 18))
                .fg(Color::Rgb(220, 225, 235)),
        );
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                baseline.cell_mut((x, y)).unwrap().set_symbol("x");
            }
        }
        let protected_cells = std::collections::HashSet::from([selected]);
        let no_highlights = std::collections::HashSet::new();

        for effect in [
            CharacterEffect::Fade,
            CharacterEffect::Aurora,
            CharacterEffect::Hologram,
            CharacterEffect::Rainbow,
        ] {
            let mut theme = Theme::built_in_default();
            assert!(theme.set_character_effect(Some(effect)));
            assert!(theme.set_character_strength(100));
            let started = Instant::now();
            let mut engine = ThemeVisualEngine::default();
            engine.sync(&theme, started);
            assert_eq!(engine.mode(), None, "characters do not become a backdrop");
            let mut hidden = baseline.clone();
            engine.paint_characters(
                started + WallDuration::from_millis(740),
                &mut hidden,
                area,
                CharacterVisualSurfaces {
                    strength: 0.0,
                    interface: &[interface],
                    protected: &[overlay],
                    selected: &protected_cells,
                    highlighted: &no_highlights,
                },
            );
            assert_eq!(hidden, baseline, "visuals opacity hides {effect:?}");
            let mut first = baseline.clone();
            engine.paint_characters(
                started + WallDuration::from_millis(740),
                &mut first,
                area,
                CharacterVisualSurfaces {
                    strength: 1.0,
                    interface: &[interface],
                    protected: &[overlay],
                    selected: &protected_cells,
                    highlighted: &no_highlights,
                },
            );
            assert_eq!(first.cell((1, 1)), baseline.cell((1, 1)), "{effect:?}");
            assert_eq!(first.cell((21, 1)), baseline.cell((21, 1)), "{effect:?}");
            assert_eq!(first.cell(selected), baseline.cell(selected), "{effect:?}");
            assert!(
                (4..20)
                    .any(|x| first.cell((x, 1)).unwrap().fg != baseline.cell((x, 1)).unwrap().fg),
                "{effect:?} changes score glyph colours"
            );
            assert!(first.content.iter().all(|cell| cell.symbol() == "x"));
            assert!(
                first
                    .content
                    .iter()
                    .all(|cell| cell.bg == Color::Rgb(8, 10, 18))
            );

            let mut later = baseline.clone();
            engine.paint_characters(
                started + WallDuration::from_millis(1_430),
                &mut later,
                area,
                CharacterVisualSurfaces {
                    strength: 1.0,
                    interface: &[interface],
                    protected: &[overlay],
                    selected: &protected_cells,
                    highlighted: &no_highlights,
                },
            );
            assert_ne!(first, later, "{effect:?} has a moving phase");
        }
    }
}

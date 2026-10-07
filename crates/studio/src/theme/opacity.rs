//! Visual balance for Randomize and an authoring aid for fixed JSON presets.
//!
//! Surface sliders are gamma-shaped and cap, rather than multiply, picture
//! strength. Work in the compositor's actual blend space, then find the least
//! opaque integer setting that fits the palette's contrast and motion budgets.

use super::{
    CharacterEffect, Color, TachyonMode, Theme, ThemeOpacity, contrast_ratio, mix, true_rgb,
};

const SURFACE_GAMMA: f32 = 2.2;

/// Shared with the frame compositor: 100% surface opacity is completely solid.
pub(crate) fn backdrop_strength(visuals: u8, surface: u8) -> f32 {
    (f32::from(visuals.min(100)) / 100.0)
        .min((f32::from(100 - surface.min(100)) / 100.0).powf(SURFACE_GAMMA))
}

/// A box enclosing every RGB colour an animated foreground can reach.
/// Components, rather than just the darkest/brightest palette entries, also
/// bound intermediate hues when a shader interpolates between those entries.
#[derive(Clone, Copy)]
struct ColorBounds {
    low: Color,
    high: Color,
}

impl ColorBounds {
    const ANY: Self = Self {
        low: Color::Rgb(0, 0, 0),
        high: Color::Rgb(255, 255, 255),
    };

    fn enclosing(colors: &[Color]) -> Self {
        let mut low = (255_u8, 255_u8, 255_u8);
        let mut high = (0_u8, 0_u8, 0_u8);
        for &color in colors {
            let Some((red, green, blue)) = true_rgb(color) else {
                return Self::ANY;
            };
            low = (low.0.min(red), low.1.min(green), low.2.min(blue));
            high = (high.0.max(red), high.1.max(green), high.2.max(blue));
        }
        Self {
            low: Color::Rgb(low.0, low.1, low.2),
            high: Color::Rgb(high.0, high.1, high.2),
        }
    }

    fn native(foreground: Color, amount: f32) -> Self {
        Self {
            low: mix(foreground, Self::ANY.low, amount),
            high: mix(foreground, Self::ANY.high, amount),
        }
    }

    fn characters(self, wash: CharacterWash) -> Self {
        // Character phases can contribute anything from zero to the maximum
        // strength. Enclose the untouched native colour as well, so brighter
        // targets never buy a background budget that fails between crests.
        let low = Self::enclosing(&[self.low, mix(self.low, wash.targets.low, wash.strength)]).low;
        let high =
            Self::enclosing(&[self.high, mix(self.high, wash.targets.high, wash.strength)]).high;
        // The real shader rounds its colour, then the visuals slider blends
        // it back toward the native foreground. Retain that order and both
        // rounding steps instead of multiplying the two blend amounts.
        Self {
            low: mix(self.low, low, wash.opacity),
            high: mix(self.high, high, wash.opacity),
        }
    }
}

#[derive(Clone, Copy)]
struct CharacterWash {
    targets: ColorBounds,
    strength: f32,
    opacity: f32,
}

impl CharacterWash {
    fn from_theme(theme: &Theme) -> Option<Self> {
        let visual = theme.character_visual()?;
        let targets = match visual.effect {
            CharacterEffect::Aurora => {
                ColorBounds::enclosing(&[theme.accent, theme.syntax.number, theme.foreground])
            }
            CharacterEffect::Hologram => ColorBounds::enclosing(&[
                theme.accent,
                theme.syntax.number,
                theme.foreground,
                mix(theme.muted, theme.foreground, 0.58),
            ]),
            // Fade targets the moving ground; Rainbow traverses the hue
            // wheel. Neither is restricted to this theme's accent palette.
            CharacterEffect::Fade | CharacterEffect::Rainbow => ColorBounds::ANY,
        };
        Some(Self {
            targets,
            strength: f32::from(visual.strength) / 100.0,
            opacity: 1.0,
        })
    }

    fn at_opacity(self, opacity: u8) -> Self {
        Self {
            opacity: f32::from(opacity) / 100.0,
            ..self
        }
    }
}

#[derive(Clone, Copy)]
struct Pair {
    foreground: Color,
    background: Color,
    floor: f32,
}

impl Pair {
    fn new(foreground: Color, background: Color, floor: f32) -> Self {
        // Keep at least the preferred floor or 85% of the palette's original
        // contrast above 1:1, whichever is lower. This grants near-floor and
        // deliberately quiet colours a small budget without repainting them.
        let baseline = contrast_ratio(foreground, background);
        Self {
            foreground,
            background,
            floor: floor.min(1.0 + (baseline - 1.0) * 0.85),
        }
    }

    fn holds(self, background_wash: f32, foreground_wash: f32) -> bool {
        self.holds_characters(background_wash, foreground_wash, None)
    }

    fn holds_characters(
        self,
        background_wash: f32,
        native_wash: f32,
        characters: Option<CharacterWash>,
    ) -> bool {
        let black = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(255, 255, 255);
        if true_rgb(self.foreground).is_none() || true_rgb(self.background).is_none() {
            // A terminal-owned palette cannot be measured by the application.
            let foreground_wash = 1.0
                - (1.0 - native_wash)
                    * (1.0 - characters.map_or(0.0, |wash| wash.strength * wash.opacity));
            return background_wash <= 0.08 && foreground_wash <= 0.08;
        }
        // Each blended RGB channel lies between its black and white endpoint.
        // Luminance is monotonic in all three channels. Check the nearest ends
        // of the foreground/background intervals AND that they never cross;
        // checking contrast at both ends alone can miss matching mid-tones.
        let light_text =
            contrast_ratio(self.foreground, black) >= contrast_ratio(self.background, black);
        let mut foreground = ColorBounds::native(self.foreground, native_wash);
        if let Some(characters) = characters {
            foreground = foreground.characters(characters);
        }
        let foreground = if light_text {
            foreground.low
        } else {
            foreground.high
        };
        let background = mix(
            self.background,
            if light_text { white } else { black },
            background_wash,
        );
        let still_light = contrast_ratio(foreground, black) >= contrast_ratio(background, black);
        still_light == light_text && contrast_ratio(foreground, background) >= self.floor
    }
}

fn editor_pairs(theme: &Theme) -> Vec<Pair> {
    let syntax = &theme.syntax;
    let mut pairs = vec![
        Pair::new(theme.foreground, theme.background, 4.5),
        Pair::new(syntax.text, theme.background, 4.5),
        Pair::new(syntax.comment, theme.background, 2.0),
        Pair::new(syntax.string, theme.background, 3.0),
        Pair::new(syntax.number, theme.background, 3.0),
        Pair::new(syntax.punctuation, theme.background, 3.0),
        Pair::new(theme.mini, theme.background, 3.0),
    ];
    for foreground in [syntax.keyword, syntax.function].into_iter().flatten() {
        pairs.push(Pair::new(foreground, theme.background, 3.0));
    }
    if let Some(current_line) = theme.current_line {
        let on_line = pairs
            .iter()
            .map(|pair| Pair::new(pair.foreground, current_line, pair.floor))
            .collect::<Vec<_>>();
        pairs.extend(on_line);
    }
    pairs
}

fn interface_pairs(theme: &Theme) -> Vec<Pair> {
    let mut pairs = Vec::new();
    for ground in [
        Some(theme.background),
        Some(theme.surface),
        Some(theme.overlay),
        theme.current_line,
        theme.slider_fill,
    ]
    .into_iter()
    .flatten()
    {
        for (foreground, floor) in [
            (theme.foreground, 4.5),
            (theme.muted, 2.0),
            (theme.accent, 3.0),
            (theme.ok, 3.0),
            (theme.warn, 3.0),
            (theme.error, 3.0),
        ] {
            pairs.push(Pair::new(foreground, ground, floor));
        }
    }
    pairs.push(Pair::new(theme.selection_text, theme.selection, 4.5));
    pairs
}

fn holds(pairs: &[Pair], background: f32, foreground: f32) -> bool {
    pairs.iter().all(|pair| pair.holds(background, foreground))
}

fn holds_editor(
    pairs: &[Pair],
    editor_ground: Color,
    background: f32,
    interface: f32,
    native: f32,
    characters: Option<CharacterWash>,
) -> bool {
    pairs.iter().all(|pair| {
        // A distinct current-line fill takes the interface wash in the image
        // compositor. If it matches the plain editor ground it takes the
        // editor wash, exactly as VisualBackdrop::ground_of decides.
        let background = if pair.background == editor_ground {
            background
        } else {
            background.min(interface)
        };
        pair.holds_characters(background, native, characters)
    })
}

fn surface_opacity(visuals: u8, maximum: f32, allowed: impl Fn(f32) -> bool) -> u8 {
    (0..=100)
        .find(|&opacity| {
            let wash = backdrop_strength(visuals, opacity);
            wash <= maximum && allowed(wash)
        })
        .unwrap_or(100)
}

impl Theme {
    /// Randomize calls this after rolling its palette. Fixed presets, custom
    /// JSON and manual sliders retain their authored choices unchanged.
    pub(super) fn balance_opacities(&mut self) {
        let native = self.cell_visual();
        let hydra = self.hydra_code().is_some();
        let characters = CharacterWash::from_theme(self);
        if native.is_none() && !hydra && characters.is_none() {
            return;
        }

        let motion = native.map_or(0.55, |visual| {
            f32::from(visual.density) / 100.0 * 0.6 + f32::from(visual.speed) / 100.0 * 0.4
        });
        let text_effect = native.is_some_and(|visual| visual.mode == TachyonMode::Text);
        // Keep intentional glitch/reveal effects playful. Their source glyph
        // substitutions remain part of the theme; only menu labels are fixed.
        let picture_limit = 0.30 - 0.10 * motion;
        let interface_limit = 0.075 - 0.035 * motion;
        let editor = editor_pairs(self);
        let interface = interface_pairs(self);
        let mut visuals = (80.0 - 25.0 * motion).round() as u8;

        if let Some(characters) = characters {
            // Glyph animation uses visuals opacity directly, not editor
            // opacity. Reserve half the background budget before allocating
            // its foreground wash, so neither layer consumes the whole look.
            // Character-only themes may still sit over a score's Hydra frame.
            let reserve = backdrop_strength(
                visuals,
                surface_opacity(visuals, picture_limit, |wash| {
                    holds_editor(
                        &editor,
                        self.background,
                        if text_effect { 0.0 } else { wash },
                        interface_limit,
                        if text_effect { wash } else { 0.0 },
                        None,
                    )
                }),
            ) * 0.5;
            visuals = (0..=visuals)
                .rev()
                .find(|&value| {
                    holds_editor(
                        &editor,
                        self.background,
                        if text_effect { 0.0 } else { reserve },
                        interface_limit,
                        if text_effect { reserve } else { 0.0 },
                        Some(characters.at_opacity(value)),
                    )
                })
                .unwrap_or(0);
        }
        let characters = characters.map(|wash| wash.at_opacity(visuals));
        let editor_opacity = surface_opacity(visuals, picture_limit, |wash| {
            holds_editor(
                &editor,
                self.background,
                if text_effect { 0.0 } else { wash },
                interface_limit,
                if text_effect { wash } else { 0.0 },
                characters,
            )
        });
        let interface_limit = interface_limit.min(backdrop_strength(visuals, editor_opacity));
        let interface_opacity = surface_opacity(visuals, interface_limit, |wash| {
            holds(&interface, wash, 0.0)
        });
        self.opacity = ThemeOpacity {
            backdrop: Some(visuals),
            interface: Some(interface_opacity),
            editor: Some(editor_opacity),
        };
        // Use one spelling in newly materialised defaults; old external JSON
        // still accepts ui_opacity through Theme::opacities().
        self.ui_opacity = None;
        if hydra {
            // The sketch cap is another minimum, not another multiplication.
            // Match its strongest intended wash; no hidden second dimmer.
            self.set_hydra_opacity(Some((picture_limit * 100.0).ceil() as u8));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn fixed_presets_load_their_authored_json_without_recalibration() {
        let mut names = BTreeSet::new();
        for &(name, document) in super::super::BUILT_INS
            .iter()
            .chain(super::super::SHOWROOM_THEMES)
        {
            assert!(names.insert(name), "duplicate fixed theme: {name}");
            let authored = Theme::from_json(document).unwrap().named(name);
            let loaded = Theme::built_in(name).unwrap();
            assert_eq!(loaded.to_json(), authored.to_json(), "{name}");
        }
        let fixed_names = Theme::built_in_names()
            .filter(|name| *name != super::super::RANDOMIZE_THEME)
            .collect::<BTreeSet<_>>();
        assert_eq!(names, fixed_names, "every fixed preset comes from JSON");
    }

    #[test]
    fn every_animated_preset_and_random_palette_has_a_visible_bounded_balance() {
        let names: BTreeSet<_> = Theme::built_in_names()
            .filter(|name| *name != super::super::RANDOMIZE_THEME)
            .collect();
        let mut animated = 0;
        let mut balances = BTreeSet::new();
        let themes = names
            .iter()
            .map(|name| Theme::built_in(name).unwrap())
            .chain((0..200).map(|seed| super::super::randomize_theme(seed * 7_919)));
        for theme in themes {
            if theme.cell_visual().is_none()
                && theme.hydra_code().is_none()
                && theme.character_visual().is_none()
            {
                continue;
            }
            animated += 1;
            let opacity = theme.opacities();
            assert!(theme.opacity.backdrop.is_some(), "{}", theme.name);
            assert!(theme.opacity.interface.is_some(), "{}", theme.name);
            assert!(theme.opacity.editor.is_some(), "{}", theme.name);
            let editor = backdrop_strength(opacity.backdrop, opacity.editor);
            let interface = backdrop_strength(opacity.backdrop, opacity.interface);
            assert!(opacity.backdrop > 0, "{} lost its animation", theme.name);
            assert!(editor > 0.015, "{}: editor wash {editor}", theme.name);
            assert!(
                interface > 0.002,
                "{}: interface wash {interface}",
                theme.name
            );
            assert!(
                interface <= editor,
                "{}: menus need a quieter wash",
                theme.name
            );
            assert!(
                holds(&interface_pairs(&theme), interface, 0.0),
                "{}",
                theme.name
            );
            let text = theme
                .cell_visual()
                .is_some_and(|v| v.mode == TachyonMode::Text);
            assert!(
                holds_editor(
                    &editor_pairs(&theme),
                    theme.background,
                    if text { 0.0 } else { editor },
                    interface,
                    if text { editor } else { 0.0 },
                    CharacterWash::from_theme(&theme).map(|wash| wash.at_opacity(opacity.backdrop)),
                ),
                "{}",
                theme.name
            );
            balances.insert((opacity.backdrop, opacity.interface, opacity.editor));
            let restored = Theme::from_json(&theme.to_json()).unwrap();
            assert_eq!(restored.opacities(), opacity, "saved defaults must survive");
        }
        assert_eq!(
            animated, 257,
            "cover all 57 presets and 200 random palettes"
        );
        assert!(
            balances.len() > 30,
            "defaults must respond to the palette/effect"
        );
    }

    #[test]
    fn character_bounds_keep_the_quiet_phase_and_native_then_character_order() {
        let pair = Pair::new(Color::Rgb(140, 140, 140), Color::Rgb(20, 20, 20), 4.5);
        let bright = CharacterWash {
            targets: ColorBounds::enclosing(&[Color::Rgb(255, 255, 255)]),
            strength: 1.0,
            opacity: 1.0,
        };
        assert!(
            contrast_ratio(
                bright.targets.high,
                mix(pair.background, bright.targets.high, 0.2)
            ) > pair.floor
        );
        assert!(
            !pair.holds_characters(0.2, 0.0, Some(bright)),
            "a bright crest cannot buy a background that obscures the quiet phase"
        );

        let theme = Theme::built_in("lsd").unwrap();
        for effect in [CharacterEffect::Aurora, CharacterEffect::Hologram] {
            let mut theme = theme.clone();
            theme.set_character_effect(Some(effect));
            let character = CharacterWash::from_theme(&theme).unwrap();
            let mut targets = vec![theme.accent, theme.syntax.number, theme.foreground];
            if effect == CharacterEffect::Hologram {
                targets.push(mix(theme.muted, theme.foreground, 0.58));
            }
            let endpoints = targets.clone();
            for &first in &endpoints {
                for &second in &endpoints {
                    targets.push(mix(first, second, 0.37));
                }
            }
            for original in [theme.syntax.comment, theme.syntax.string, theme.foreground] {
                for native_amount in [0.0, 0.18] {
                    for opacity in [0, 37, 100] {
                        let character = character.at_opacity(opacity);
                        let bound =
                            ColorBounds::native(original, native_amount).characters(character);
                        let (lr, lg, lb) = true_rgb(bound.low).unwrap();
                        let (hr, hg, hb) = true_rgb(bound.high).unwrap();
                        for native_target in [
                            Color::Rgb(0, 0, 0),
                            Color::Rgb(255, 255, 255),
                            Color::Rgb(255, 0, 128),
                        ] {
                            let native = mix(original, native_target, native_amount);
                            for &target in &targets {
                                for phase in [0.0, 0.5, 1.0] {
                                    let shaded = mix(native, target, character.strength * phase);
                                    let (r, g, b) =
                                        true_rgb(mix(native, shaded, character.opacity)).unwrap();
                                    assert!(
                                        (lr..=hr).contains(&r)
                                            && (lg..=hg).contains(&g)
                                            && (lb..=hb).contains(&b),
                                        "{effect:?}: ordered phase escaped its RGB bounds"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_distinct_current_line_fill_uses_the_interface_wash() {
        let line = Pair::new(Color::Rgb(255, 255, 255), Color::Rgb(100, 100, 100), 4.5);
        assert!(holds_editor(
            &[line],
            Color::Rgb(0, 0, 0),
            0.25,
            0.02,
            0.0,
            None
        ));
        assert!(
            !holds_editor(&[line], line.background, 0.25, 0.02, 0.0, None),
            "a fill matching the editor ground takes its full picture wash"
        );
    }

    #[test]
    fn palette_bound_character_effects_can_use_more_of_their_colours() {
        for name in ["lsd", "synthwave"] {
            let mut palette_bound = Theme::built_in(name).unwrap();
            palette_bound.balance_opacities();
            let mut unrestricted = palette_bound.clone();
            unrestricted.set_character_effect(Some(CharacterEffect::Rainbow));
            unrestricted.balance_opacities();
            assert!(
                palette_bound.opacities().backdrop > unrestricted.opacities().backdrop,
                "{name}: its own palette permits a stronger effect than arbitrary RGB targets"
            );
        }
    }

    #[test]
    fn arbitrary_picture_colours_cannot_cross_the_contrast_budget() {
        let theme = Theme::built_in("unicorn").unwrap();
        let opacity = theme.opacities();
        for (pairs, wash) in [
            (
                editor_pairs(&theme),
                backdrop_strength(opacity.backdrop, opacity.editor),
            ),
            (
                interface_pairs(&theme),
                backdrop_strength(opacity.backdrop, opacity.interface),
            ),
        ] {
            for pair in pairs {
                for r in [0, 64, 128, 192, 255] {
                    for g in [0, 64, 128, 192, 255] {
                        for b in [0, 64, 128, 192, 255] {
                            let ground = mix(pair.background, Color::Rgb(r, g, b), wash);
                            assert!(contrast_ratio(pair.foreground, ground) >= pair.floor);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn custom_opacities_and_static_themes_are_not_automatically_rewritten() {
        let mut theme = Theme::built_in("rustel-drift").unwrap();
        theme.opacity = ThemeOpacity {
            backdrop: Some(98),
            interface: Some(12),
            editor: Some(34),
        };
        theme.set_hydra_opacity(Some(87));
        let loaded = Theme::from_json(&theme.to_json()).unwrap();
        assert_eq!(loaded.opacity, theme.opacity);
        assert_eq!(loaded.hydra_opacity_percent(), Some(87));
        let plain = Theme::built_in_default();
        assert_eq!(plain.opacity, ThemeOpacity::default());
    }
}

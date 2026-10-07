//! Colour and decoration policy for the terminal studio.
//!
//! Every colour the studio draws is resolved through one [`Theme`] value, so a
//! community theme is a data file rather than a patch. Built-in themes are
//! compiled in; user themes are JSON documents loaded from a config directory
//! or an explicit path. Unknown keys are refused rather than ignored, because
//! a silently dropped `evnt_active` typo is worse than an error naming it.

use ratatui::layout::Rect;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use super::terminal::CaretShape;

mod opacity;
pub(super) use opacity::backdrop_strength;
#[cfg(test)]
mod opacity_render_tests {
    //! Exercise opacity defaults through the real cell compositors. The optional
    //! export is a visual QA aid: native scenes are rendered; Hydra pictures here
    //! are deliberately synthetic contrast stress frames, never sketch previews.

    use std::collections::{BTreeSet, HashSet};
    use std::time::{Duration, Instant};

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};

    use super::{CellEffect, Theme, backdrop_strength, contrast_ratio, write_color};
    #[cfg(feature = "hydra")]
    use crate::theme_visuals::CameraPicture;
    use crate::theme_visuals::{CharacterVisualSurfaces, ThemeVisualEngine, ThemeVisualSurfaces};
    use crate::view::VisualBackdrop;

    const AREA: Rect = Rect::new(0, 0, 80, 24);
    const INTERFACE: [Rect; 3] = [
        Rect::new(0, 0, 80, 1),
        Rect::new(50, 1, 30, 22),
        Rect::new(0, 23, 80, 1),
    ];
    const PROTECTED: [Rect; 1] = [Rect::new(2, 10, 39, 2)];

    fn fixture(theme: &Theme) -> Buffer {
        let mut buffer = Buffer::empty(AREA);
        buffer.set_style(
            AREA,
            Style::default().fg(theme.foreground).bg(theme.background),
        );
        for area in INTERFACE {
            buffer.set_style(area, Style::default().bg(theme.surface));
        }
        buffer.set_string(1, 0, "Rustel  score  samples  settings", Style::default());
        for (y, text, color) in [
            (2, "// a little room for the melody", theme.syntax.comment),
            (3, "$: note(\"d3 a3 f4 e4\")", theme.syntax.string),
            (4, "  .s(\"sine\").fm(2.4)", theme.syntax.number),
            (5, "  .lpf(1800).gain(0.3)", theme.syntax.text),
            (6, "  .delay(0.25).room(0.3)", theme.syntax.punctuation),
            (
                8,
                "// the rest of the screen is yours",
                theme.syntax.comment,
            ),
        ] {
            buffer.set_string(2, y, text, Style::default().fg(color));
        }
        buffer.set_style(
            Rect::new(0, 7, 50, 1),
            Style::default().bg(theme.current_line.unwrap_or(theme.background)),
        );
        buffer.set_string(2, 7, "  .slow(2)", Style::default().fg(theme.syntax.text));
        buffer.set_string(
            23,
            7,
            "// current line",
            Style::default().fg(theme.syntax.comment),
        );
        buffer.set_string(
            52,
            2,
            "Settings / Appearance",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        );
        buffer.set_string(52, 4, "Theme", Style::default().fg(theme.muted));
        buffer.set_style(
            Rect::new(51, 5, 28, 1),
            Style::default()
                .fg(theme.selection_text)
                .bg(theme.selection),
        );
        buffer.set_stringn(52, 5, &theme.name, 26, Style::default());
        for (y, text, color) in [
            (7, "Visuals       [------]", theme.accent),
            (9, "Interface     [------]", theme.foreground),
            (11, "Editor        [------]", theme.foreground),
            (14, "Ready to play", theme.ok),
            (16, "Example warning", theme.warn),
            (18, "Example error", theme.error),
            (21, "Esc back", theme.muted),
        ] {
            buffer.set_string(52, y, text, Style::default().fg(color));
        }
        buffer.set_string(
            1,
            23,
            "Ctrl+Enter play  Ctrl+. stop  Ctrl+D reference",
            Style::default().fg(theme.muted),
        );
        buffer.set_style(
            PROTECTED[0],
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        buffer.set_string(3, 10, "Protected preview / caret", Style::default());
        buffer
    }

    fn check_readability(name: &str, before: &Buffer, after: &Buffer) {
        for y in 0..AREA.height {
            for x in 0..AREA.width {
                let original = before.cell((x, y)).unwrap();
                let painted = after.cell((x, y)).unwrap();
                if PROTECTED.iter().any(|rect| rect.contains((x, y).into())) {
                    assert_eq!(painted, original, "{name}: protected cell {x},{y}");
                }
                if original.symbol().trim().is_empty() {
                    continue;
                }
                if INTERFACE.iter().any(|rect| rect.contains((x, y).into())) {
                    assert_eq!(
                        painted.symbol(),
                        original.symbol(),
                        "{name}: menu glyph {x},{y}"
                    );
                    assert_eq!(painted.fg, original.fg, "{name}: menu foreground {x},{y}");
                    assert_eq!(
                        painted.modifier, original.modifier,
                        "{name}: menu modifier {x},{y}"
                    );
                }
                // Reveal/glitch presets intentionally replace editor glyphs. The
                // original code, where retained, must still be readable. Preserve
                // a palette's own low-contrast choices instead of claiming every
                // authored comment already satisfies a universal contrast floor.
                if painted.symbol() == original.symbol() {
                    let baseline = contrast_ratio(original.fg, original.bg);
                    let floor = 2.0_f32.min(1.0 + (baseline - 1.0) * 0.85);
                    let actual = contrast_ratio(painted.fg, painted.bg);
                    assert!(
                        actual + 0.025 >= floor,
                        "{name}: retained glyph {x},{y} at {actual:.3}:1, needs {floor:.3}:1"
                    );
                }
            }
        }
    }

    fn exported_frame(
        theme: &Theme,
        frame: &Buffer,
        label: &str,
        phase_ms: u64,
    ) -> serde_json::Value {
        let opacity = theme.opacities();
        serde_json::json!({
            "theme": theme.name,
            "label": label,
            "phase_ms": phase_ms,
            "width": AREA.width,
            "height": AREA.height,
            "opacity": {
                "backdrop": opacity.backdrop,
                "interface": opacity.interface,
                "editor": opacity.editor,
                "sketch": theme.hydra_opacity_percent(),
            },
            "cells": frame.content.iter().map(|cell| serde_json::json!({
                "text": cell.symbol(),
                "fg": write_color(cell.fg),
                "bg": write_color(cell.bg),
                "bold": cell.modifier.contains(Modifier::BOLD),
            })).collect::<Vec<_>>(),
        })
    }

    fn export(name: &str, frames: Vec<serde_json::Value>) {
        let Some(directory) = std::env::var_os("RUSTEL_THEME_RENDER_EXPORT") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).expect("create requested render export directory");
        let path = directory.join(name);
        std::fs::write(path, serde_json::to_vec(&frames).unwrap())
            .expect("write requested render export");
    }

    #[test]
    fn animated_presets_render_visibly_while_menu_labels_stay_sharp() {
        let started = Instant::now();
        let empty = HashSet::new();
        #[cfg(feature = "hydra")]
        let camera_frame = CameraPicture {
            width: 64,
            height: 48,
            luma: (0..48)
                .flat_map(|y| (0..64).map(move |x| if (x / 8 + y / 8) % 2 == 0 { 255 } else { 40 }))
                .collect(),
        };
        let mut exported = Vec::new();
        let mut exercised = 0;
        for name in Theme::built_in_names().collect::<BTreeSet<_>>() {
            if name == super::RANDOMIZE_THEME {
                continue;
            }
            let theme = Theme::built_in(name).unwrap();
            if theme.cell_visual().is_none() && theme.character_visual().is_none() {
                continue;
            }
            exercised += 1;
            let opacity = theme.opacities();
            let baseline = fixture(&theme);
            let editor_strength = backdrop_strength(opacity.backdrop, opacity.editor);
            let interface_strength = backdrop_strength(opacity.backdrop, opacity.interface);
            let mut engine = ThemeVisualEngine::default();
            engine.sync(&theme, started);
            let camera = theme
                .cell_visual()
                .is_some_and(|visual| visual.effect == CellEffect::Camera);
            let mut changed = false;
            for step in 1..=80 {
                let now = started + Duration::from_millis(step * 100);
                #[cfg(feature = "hydra")]
                if camera {
                    // Inject pixels directly: the test neither acquires a camera
                    // nor asks for permission, and each frame is kept fresh.
                    engine.update_camera(camera_frame.clone(), now);
                }
                let mut frame = baseline.clone();
                engine.paint_text(
                    now,
                    &mut frame,
                    AREA,
                    ThemeVisualSurfaces {
                        editor_strength,
                        interface_strength,
                        interface: &INTERFACE,
                        protected: &PROTECTED,
                    },
                );
                if let Some(image) = engine.image(now, &frame, AREA) {
                    VisualBackdrop {
                        width: image.width,
                        height: image.height,
                        rgba: image.rgba,
                        strength: editor_strength,
                        interface: interface_strength,
                    }
                    .paint_excluding(
                        &mut frame,
                        AREA,
                        theme.background,
                        &PROTECTED,
                        &INTERFACE,
                        None,
                    );
                }
                engine.paint_characters(
                    now,
                    &mut frame,
                    AREA,
                    CharacterVisualSurfaces {
                        strength: f32::from(opacity.backdrop) / 100.0,
                        interface: &INTERFACE,
                        protected: &PROTECTED,
                        selected: &empty,
                        highlighted: &empty,
                    },
                );
                check_readability(name, &baseline, &frame);
                changed |= frame != baseline;
                if matches!(step, 10 | 80)
                    && std::env::var_os("RUSTEL_THEME_RENDER_EXPORT").is_some()
                {
                    let label = if camera && cfg!(feature = "hydra") {
                        "synthetic camera frame through native renderer; no camera acquired"
                    } else if camera {
                        "camera renderer without input; no camera acquired"
                    } else if theme.hydra_code().is_some() {
                        "rendered native character animation only; Hydra not rendered"
                    } else {
                        "rendered native animation"
                    };
                    exported.push(exported_frame(&theme, &frame, label, step * 100));
                }
            }
            if !camera || cfg!(feature = "hydra") {
                assert!(
                    changed,
                    "{name}: default opacity erased every rendered effect"
                );
            }
        }
        assert!(
            exercised >= 40,
            "native animated built-in coverage shrank: {exercised}"
        );
        export("native.json", exported);
    }

    #[test]
    fn hydra_defaults_keep_text_readable_over_synthetic_bright_and_dark_frames() {
        let started = Instant::now();
        let empty = HashSet::new();
        let mut exported = Vec::new();
        let mut exercised = 0;
        for name in Theme::built_in_names().collect::<BTreeSet<_>>() {
            if name == super::RANDOMIZE_THEME {
                continue;
            }
            let theme = Theme::built_in(name).unwrap();
            if theme.hydra_code().is_none() {
                continue;
            }
            exercised += 1;
            let opacity = theme.opacities();
            let visual = opacity
                .backdrop
                .min(theme.hydra_opacity_percent().unwrap_or(45));
            let baseline = fixture(&theme);
            let mut engine = ThemeVisualEngine::default();
            engine.sync(&theme, started);
            for (label, rgb) in [
                ("synthetic Hydra stress: black", [0, 0, 0]),
                ("synthetic Hydra stress: white", [255, 255, 255]),
                ("synthetic Hydra stress: red", [255, 0, 0]),
                ("synthetic Hydra stress: green", [0, 255, 0]),
                ("synthetic Hydra stress: blue", [0, 0, 255]),
            ] {
                let picture = VisualBackdrop {
                    width: 1,
                    height: 1,
                    rgba: vec![rgb[0], rgb[1], rgb[2], 255],
                    strength: backdrop_strength(visual, opacity.editor),
                    interface: backdrop_strength(visual, opacity.interface),
                };
                for phase_ms in [100, 1_300, 8_000] {
                    let mut frame = baseline.clone();
                    picture.paint_excluding(
                        &mut frame,
                        AREA,
                        theme.background,
                        &PROTECTED,
                        &INTERFACE,
                        None,
                    );
                    // The real pipeline animates glyphs over the already blended
                    // backdrop. Each pass alone being readable is insufficient.
                    engine.paint_characters(
                        started + Duration::from_millis(phase_ms),
                        &mut frame,
                        AREA,
                        CharacterVisualSurfaces {
                            strength: f32::from(opacity.backdrop) / 100.0,
                            interface: &INTERFACE,
                            protected: &PROTECTED,
                            selected: &empty,
                            highlighted: &empty,
                        },
                    );
                    check_readability(name, &baseline, &frame);
                    if phase_ms == 8_000 && std::env::var_os("RUSTEL_THEME_RENDER_EXPORT").is_some()
                    {
                        let label = if theme.character_visual().is_some() {
                            format!("{label} + rendered native character animation")
                        } else {
                            label.to_owned()
                        };
                        exported.push(exported_frame(&theme, &frame, &label, phase_ms));
                    }
                }
            }
        }
        assert!(
            exercised >= 8,
            "Hydra built-in coverage shrank: {exercised}"
        );
        export("hydra-synthetic-stress.json", exported);
    }
}

/// Name of the theme used when nothing else is selected.
pub const DEFAULT_THEME: &str = "rustel-dark";
const MAX_THEME_DOCUMENT_BYTES: usize = 64 * 1024;

const BUILT_INS: &[(&str, &str)] = &[
    ("rustel-dark", include_str!("themes/rustel-dark.json")),
    // rustel-dark with a slow drift breathing behind the code: the theme
    // that shows a theme can carry a sketch of its own.
    ("rustel-drift", include_str!("themes/rustel-drift.json")),
    // Camera-backed themes. They request the camera only while their own
    // picture is visible; the persisted webcam permission still owns
    // whether a device may open. The first three go through Hydra and its
    // `s0` texture; the fourth is drawn by the studio itself, in Braille,
    // and so needs no GPU and no sketch.
    ("webcam-clean", include_str!("themes/webcam-clean.json")),
    ("webcam-distort", include_str!("themes/webcam-distort.json")),
    ("webcam-hacker", include_str!("themes/webcam-hacker.json")),
    ("webcam-braille", include_str!("themes/webcam-braille.json")),
    // An independently authored feedback current from the snippet shelf.
    ("prism", include_str!("themes/prism.json")),
    ("rustel-light", include_str!("themes/rustel-light.json")),
    ("strudel", include_str!("themes/strudel.json")),
    ("powershell", include_str!("themes/powershell.json")),
    ("rustel-live", include_str!("themes/rustel-live.json")),
    ("synthwave", include_str!("themes/synthwave.json")),
    ("vectrex", include_str!("themes/vectrex.json")),
    ("mode7", include_str!("themes/mode7.json")),
    // Black ground, phosphor green, and a slow rain behind the code.
    ("matrix", include_str!("themes/matrix.json")),
    ("space", include_str!("themes/space.json")),
    ("campfire", include_str!("themes/campfire.json")),
    ("basketball", include_str!("themes/basketball.json")),
    ("forest", include_str!("themes/forest.json")),
    ("bear", include_str!("themes/bear.json")),
    ("mono", include_str!("themes/mono.json")),
    ("solarized", include_str!("themes/solarized.json")),
    ("ember", include_str!("themes/ember.json")),
    ("hell", include_str!("themes/hell.json")),
    ("lavender", include_str!("themes/lavender.json")),
    // Ports of the palettes people already read code in.
    ("one-dark", include_str!("themes/one-dark.json")),
    ("dracula", include_str!("themes/dracula.json")),
    ("tokyo-night", include_str!("themes/tokyo-night.json")),
    (
        "catppuccin-mocha",
        include_str!("themes/catppuccin-mocha.json"),
    ),
    (
        "catppuccin-latte",
        include_str!("themes/catppuccin-latte.json"),
    ),
    ("nord", include_str!("themes/nord.json")),
    ("ayu-mirage", include_str!("themes/ayu-mirage.json")),
    ("github-light", include_str!("themes/github-light.json")),
    (
        "solarized-light",
        include_str!("themes/solarized-light.json"),
    ),
    ("emacs", include_str!("themes/emacs.json")),
    // And some with a mood of their own.
    ("unicorn", include_str!("themes/unicorn.json")),
    // The dark palette with every hue rolling through the code.
    ("rainbow", include_str!("themes/rainbow.json")),
    // Everything at once, on purpose: a Hydra sketch under an aurora over
    // the glyphs, a lit current line, and a palette
    // where nothing agrees with anything. The one theme that is not trying
    // to stay out of the way.
    ("lsd", include_str!("themes/lsd.json")),
];

// Fixed effect presets live in JSON too, with independently editable defaults.
const SHOWROOM_THEMES: &[(&str, &str)] = &[
    ("beams", include_str!("themes/beams.json")),
    ("binarypath", include_str!("themes/binarypath.json")),
    ("blackhole", include_str!("themes/blackhole.json")),
    ("bouncyballs", include_str!("themes/bouncyballs.json")),
    ("bubbles", include_str!("themes/bubbles.json")),
    ("burn", include_str!("themes/burn.json")),
    ("colorshift", include_str!("themes/colorshift.json")),
    ("crumble", include_str!("themes/crumble.json")),
    ("decrypt", include_str!("themes/decrypt.json")),
    ("dvd-bounce", include_str!("themes/dvd-bounce.json")),
    ("errorcorrect", include_str!("themes/errorcorrect.json")),
    ("expand", include_str!("themes/expand.json")),
    ("fireworks", include_str!("themes/fireworks.json")),
    ("highlight", include_str!("themes/highlight.json")),
    ("leak", include_str!("themes/leak.json")),
    (
        "orbittingvolley",
        include_str!("themes/orbittingvolley.json"),
    ),
    ("overflow", include_str!("themes/overflow.json")),
    ("print", include_str!("themes/print.json")),
    ("rain", include_str!("themes/rain.json")),
    ("scramble", include_str!("themes/scramble.json")),
    ("rings", include_str!("themes/rings.json")),
    ("scattered", include_str!("themes/scattered.json")),
    ("slice", include_str!("themes/slice.json")),
    ("smoke", include_str!("themes/smoke.json")),
    ("spotlights", include_str!("themes/spotlights.json")),
    ("snow", include_str!("themes/snow.json")),
    ("swarm", include_str!("themes/swarm.json")),
    ("sweep", include_str!("themes/sweep.json")),
    ("synthgrid", include_str!("themes/synthgrid.json")),
    ("thunderstorm", include_str!("themes/thunderstorm.json")),
    ("unstable", include_str!("themes/unstable.json")),
    ("vhstape", include_str!("themes/vhstape.json")),
    ("waves", include_str!("themes/waves.json")),
    ("waves-reactive", include_str!("themes/waves-reactive.json")),
    ("wipe", include_str!("themes/wipe.json")),
];

/// How an active (currently sounding) source range is marked.
///
/// The default marks sounding text with a translucent tint of the event
/// colour, adjusting faint syntax colours when needed on light themes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkStyle {
    /// Tint behind the source; improve text contrast when needed on light themes.
    #[default]
    Tint,
    /// Underline the source in the event colour, preserving syntax colours.
    Underline,
    /// Recolour the text and underline it in the event colour. No fill.
    Outline,
    /// Recolour the text only.
    Text,
    /// Fill the cell background with the event colour.
    Fill,
    /// Swap foreground and background.
    Invert,
}

/// A native cell effect available to a theme.
///
/// This is a closed vocabulary on purpose. A downloaded theme may select a
/// renderer and tune it, but it cannot execute native code inside the studio.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CellEffect {
    Beams,
    BinaryPath,
    Blackhole,
    BouncyBalls,
    Bubbles,
    Burn,
    ColorShift,
    Crumble,
    Cyberpunk,
    Decrypt,
    Dvd,
    ErrorCorrect,
    Expand,
    Fireworks,
    Highlight,
    LaserEtch,
    Matrix,
    MiddleOut,
    OrbittingVolley,
    Overflow,
    #[serde(alias = "pour")]
    Leak,
    /// A sun, planets on their orbits, and stars drifting past.
    Space,
    /// A fire between logs, a tent beside it, the night over both.
    Campfire,
    /// A lava sea, flames over it, embers up and ash down, a sky that beats.
    Hell,
    /// A player shooting at a hoop, making some and missing some.
    Basketball,
    /// A flat picture in perspective, driven toward the viewer: a SNES track.
    Mode7,
    /// A clearing: trees, flowers, animals dancing to the music.
    Forest,
    /// A bear asleep against a tree, until the music wakes him.
    Bear,
    /// The camera drawn as Braille dots. The only effect that asks the
    /// studio for something outside itself, and the only one that draws
    /// nothing recognisable until the webcam switch in Settings allows it.
    /// Not aliased to `braille`: the dock's spectrum already has a style
    /// by that name meaning something else, and a Braille effect that is
    /// not the camera would want it.
    #[serde(alias = "webcam")]
    Camera,
    Print,
    Rain,
    #[serde(rename = "randomize", alias = "randomsequence")]
    RandomSequence,
    /// A vector tunnel flying past stars and depth rings, drawn the way
    /// a Vectrex drew.
    #[serde(alias = "retro3d")]
    Vectrex,
    RustelBrand,
    Rings,
    Scattered,
    Slice,
    Slide,
    Smoke,
    #[serde(alias = "spray")]
    Snow,
    Spotlights,
    Swarm,
    Sweep,
    SynthGrid,
    Thunderstorm,
    Unstable,
    VhsTape,
    Waves,
    WavesReactive,
    Wipe,
}

/// Native effects offered as fixed JSON presets and Randomize ingredients.
/// Effects that work better as ingredients than full-time stage looks remain
/// available only to user JSON.
pub(super) const SHOWROOM_EFFECTS: &[(&str, CellEffect)] = &[
    ("beams", CellEffect::Beams),
    ("basketball", CellEffect::Basketball),
    ("bear", CellEffect::Bear),
    ("binarypath", CellEffect::BinaryPath),
    ("blackhole", CellEffect::Blackhole),
    ("bouncyballs", CellEffect::BouncyBalls),
    ("bubbles", CellEffect::Bubbles),
    ("burn", CellEffect::Burn),
    ("campfire", CellEffect::Campfire),
    ("colorshift", CellEffect::ColorShift),
    ("crumble", CellEffect::Crumble),
    ("decrypt", CellEffect::Decrypt),
    ("dvd-bounce", CellEffect::Dvd),
    ("errorcorrect", CellEffect::ErrorCorrect),
    ("expand", CellEffect::Expand),
    ("fireworks", CellEffect::Fireworks),
    ("forest", CellEffect::Forest),
    ("hell", CellEffect::Hell),
    ("highlight", CellEffect::Highlight),
    ("leak", CellEffect::Leak),
    ("mode7", CellEffect::Mode7),
    ("orbittingvolley", CellEffect::OrbittingVolley),
    ("overflow", CellEffect::Overflow),
    ("print", CellEffect::Print),
    ("rain", CellEffect::Rain),
    ("scramble", CellEffect::RandomSequence),
    ("rings", CellEffect::Rings),
    ("scattered", CellEffect::Scattered),
    ("slice", CellEffect::Slice),
    ("smoke", CellEffect::Smoke),
    ("spotlights", CellEffect::Spotlights),
    ("snow", CellEffect::Snow),
    ("space", CellEffect::Space),
    ("swarm", CellEffect::Swarm),
    ("sweep", CellEffect::Sweep),
    ("synthgrid", CellEffect::SynthGrid),
    ("thunderstorm", CellEffect::Thunderstorm),
    ("unstable", CellEffect::Unstable),
    ("vhstape", CellEffect::VhsTape),
    ("waves", CellEffect::Waves),
    ("waves-reactive", CellEffect::WavesReactive),
    ("wipe", CellEffect::Wipe),
];

/// How a TachyonFX theme reaches the terminal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TachyonMode {
    /// Transform the finished Ratatui cells, including their characters.
    #[default]
    Text,
    /// Convert the transformed cells into a stretched RGBA backdrop.
    Image,
}

/// A bounded animation applied to score glyphs after the backdrop is composed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CharacterEffect {
    /// Each glyph breathes on its own phase.
    Fade,
    /// Travelling colour fields meet across the source text.
    Aurora,
    /// Every hue across the text, rolling with time.
    Rainbow,
    /// Scan light and chromatic interference pass over the glyphs.
    Hologram,
}

/// Per-theme character animation, independent of the background renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterVisual {
    pub effect: CharacterEffect,
    #[serde(default = "default_character_strength")]
    pub strength: u8,
    #[serde(default = "default_character_speed")]
    pub speed: u8,
}

const fn default_character_strength() -> u8 {
    60
}

const fn default_character_speed() -> u8 {
    50
}

/// The visual renderer owned by a theme.
///
/// Hydra supplies an RGBA frame. TachyonFX may either transform the terminal
/// cells directly or feed an RGBA frame into the same backdrop compositor.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "renderer", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ThemeVisual {
    Hydra {
        code: String,
        #[serde(default)]
        camera: bool,
        #[serde(default)]
        opacity: Option<u8>,
    },
    #[serde(rename = "tachyonfx", alias = "cells")]
    TachyonFx {
        effect: CellEffect,
        #[serde(default)]
        mode: TachyonMode,
        #[serde(default = "default_cell_density")]
        density: u8,
        #[serde(default = "default_cell_speed")]
        speed: u8,
    },
}

const fn default_cell_density() -> u8 {
    70
}

const fn default_cell_speed() -> u8 {
    55
}

/// A validated TachyonFX renderer selection, cheap to copy into the frame loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellVisual {
    pub effect: CellEffect,
    pub mode: TachyonMode,
    pub density: u8,
    pub speed: u8,
}

impl MarkStyle {
    /// Apply this marking to `style` for an event drawn in `color`.
    ///
    /// `surface` is the pane background, used as the readable foreground when
    /// the event colour becomes a fill.
    pub fn apply(self, style: Style, color: Color, surface: Color) -> Style {
        // The mark colour comes from the score, not the theme: `fading_marks`
        // takes the colour the event carries and falls back to `theme.event`.
        // A theme's own palette cannot make this pair readable. A score that
        // colours an event to match the string it is written in paints green
        // on green, and the text the player is watching becomes unreadable.
        //
        // Tint keeps syntax colours on dark backgrounds and limits the wash
        // to preserve their contrast. Light themes need a visible wash even
        // when a syntax colour starts out faint, so their marked text can
        // move toward black to remain readable.
        let ground = style.bg.unwrap_or(surface);
        match self {
            Self::Tint => {
                let mut marked = style.add_modifier(Modifier::BOLD);
                if true_rgb(ground).is_some()
                    && true_rgb(color).is_some()
                    && contrast_ratio(Color::Black, ground) > contrast_ratio(Color::White, ground)
                {
                    // On a light canvas, preserving an already faint syntax
                    // colour would erase the mark. Keep a quiet, consistent
                    // wash and adjust only text that needs more contrast.
                    let tint = subtle_fill(ground, color, 1.3).unwrap_or(ground);
                    marked = marked.bg(tint);
                    if let Some(fg) = style.fg.filter(|fg| true_rgb(*fg).is_some()) {
                        marked = marked.fg(legible_against_floor(fg, tint, 4.5));
                    }
                } else if let (Some(fg), Some(_), Some(_)) = (
                    style.fg.filter(|fg| true_rgb(*fg).is_some()),
                    true_rgb(ground),
                    true_rgb(color),
                ) {
                    let floor = contrast_ratio(fg, ground).min(4.5);
                    for step in (0..=20).rev() {
                        let tint = mix(ground, color, step as f32 / 100.0);
                        if contrast_ratio(fg, tint) + 0.01 >= floor {
                            marked = marked.bg(tint);
                            break;
                        }
                    }
                } else if true_rgb(ground).is_some() && true_rgb(color).is_some() {
                    marked = marked.bg(mix(ground, color, 0.2));
                }
                marked
            }
            Self::Underline => style
                .underline_color(legible_against(color, ground))
                .add_modifier(Modifier::UNDERLINED),
            Self::Outline => {
                let color = legible_against(color, ground);
                style
                    .fg(color)
                    .underline_color(color)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            }
            Self::Text => style
                .fg(legible_against(color, ground))
                .add_modifier(Modifier::BOLD),
            // Here the mark IS the ground, so it is the text that moves.
            Self::Fill => style
                .bg(color)
                .fg(legible_against(surface, color))
                .add_modifier(Modifier::BOLD),
            Self::Invert => style
                .fg(legible_against(color, ground))
                .add_modifier(Modifier::REVERSED),
        }
    }
}

/// The style between a cell's own and its marked one, `strength` of the
/// way to marked: the colours mixed, the mark's modifiers and underline
/// kept while it is more than half there.
///
/// `ground` is what the cell sits on when it names no background of its
/// own - the editor's `background`, a panel's `surface`. It has to be
/// passed rather than assumed: a mark fading towards the wrong ground ends
/// on a colour the pane does not have, and stays there.
pub fn fade_mark(
    base: Style,
    marked: Style,
    strength: f32,
    foreground: Color,
    ground: Color,
) -> Style {
    let strength = strength.clamp(0.0, 1.0);
    let mut faded = base.fg(mix(
        base.fg.unwrap_or(foreground),
        marked.fg.unwrap_or(foreground),
        strength,
    ));
    if let Some(bg) = marked.bg {
        faded = faded.bg(mix(base.bg.unwrap_or(ground), bg, strength));
    }
    if strength >= 0.5 {
        faded = faded.add_modifier(marked.add_modifier);
        if let Some(underline) = marked.underline_color {
            faded = faded.underline_color(underline);
        }
    }
    faded
}

/// Syntax colours for the source pane's single-pass lexer.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxPalette {
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub text: Color,
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub comment: Color,
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub string: Color,
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub number: Color,
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub punctuation: Color,
    /// Words the language owns. Absent falls back to `punctuation`, which is
    /// how every theme written before this key read.
    #[serde(
        default,
        deserialize_with = "de_color_option",
        serialize_with = "se_optional_color"
    )]
    pub keyword: Option<Color>,
    /// A word being called. Absent falls back to `text`.
    #[serde(
        default,
        deserialize_with = "de_color_option",
        serialize_with = "se_optional_color"
    )]
    pub function: Option<Color>,
}

/// Colours for the level meter's dB scale, low to high.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeterPalette {
    /// Below roughly -18 dBFS.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub low: Color,
    /// Between roughly -18 and -6 dBFS.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub mid: Color,
    /// Between roughly -6 and -1 dBFS.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub high: Color,
    /// At or above roughly -1 dBFS, and the clip indicator.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub peak: Color,
    /// The unlit part of the scale.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub track: Color,
    /// The fader handle and its dB readout.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub fader: Color,
}

/// Bracket matching stays distinct from the caret, including underline carets.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BracketMark {
    #[default]
    Auto,
    Underline,
    Block,
}

/// Every colour and decoration the studio draws.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// Display name. Defaults to the file stem or built-in key when absent.
    #[serde(default)]
    pub name: String,
    /// Editor pane background.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub background: Color,
    /// Header, footer, stage and panel background.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub surface: Color,
    /// Panels drawn on top of the surface (device pickers, popovers).
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub overlay: Color,
    /// Ordinary text.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub foreground: Color,
    /// De-emphasised text: line numbers, hints, inactive labels.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub muted: Color,
    /// Hairlines, pane separators, inline-widget rails.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub rule: Color,
    /// The studio's signature colour.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub accent: Color,
    /// Playing/OK state.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub ok: Color,
    /// Evaluating/dirty state.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub warn: Color,
    /// Errors.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub error: Color,
    /// Text caret. Older user themes fall back to `foreground`; every
    /// built-in names this explicitly so it remains visible beside bracket
    /// and diagnostic underlines.
    #[serde(
        default,
        deserialize_with = "de_color_option",
        serialize_with = "se_optional_color"
    )]
    pub caret: Option<Color>,
    /// Optional DECSCUSR caret shape. Absent inherits the global Editor
    /// setting; bundled themes deliberately leave it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caret_shape: Option<CaretShape>,
    /// A band across the caret's line. `None`, or absent from a theme file,
    /// means no band, which is the default: the caret already shows its
    /// line, and a full-width band covers the visuals behind it.
    #[serde(
        default,
        deserialize_with = "de_optional_color",
        serialize_with = "se_optional_color"
    )]
    pub current_line: Option<Color>,
    /// Selection background.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub selection: Color,
    /// Selection foreground.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub selection_text: Color,
    /// Foreground for mini-notation spans.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub mini: Color,
    /// Optional fill behind mini-notation spans. Absent keeps the syntax
    /// colours visible, which is the default.
    #[serde(
        default,
        deserialize_with = "de_color_option",
        serialize_with = "se_optional_color"
    )]
    pub mini_fill: Option<Color>,
    /// Optional bracket colour override. Absent derives a mark from the palette.
    #[serde(
        default,
        deserialize_with = "de_color_option",
        serialize_with = "se_optional_color"
    )]
    pub bracket: Option<Color>,
    /// Auto pairs the mark with the caret shape; explicit styles remain supported.
    #[serde(default)]
    pub bracket_mark: BracketMark,
    /// The replay tab's own colour: its chip, its title, the blocks along
    /// its top. Absent uses `warn`, which no other tab wears.
    #[serde(
        default,
        deserialize_with = "de_color_option",
        serialize_with = "se_optional_color"
    )]
    pub replay: Option<Color>,
    /// Fill behind a live slider control. Without a fill, background
    /// visuals show through the rail's gaps.
    #[serde(
        default,
        deserialize_with = "de_color_option",
        serialize_with = "se_optional_color"
    )]
    pub slider_fill: Option<Color>,
    /// How a sounding event marks its source range.
    #[serde(default)]
    pub event_mark: MarkStyle,
    /// Seconds a mark lingers after its event, easing off; unset is the
    /// studio's 0.3 s. The `highlight fade` setting overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_fade: Option<f32>,
    /// Fallback colour for events whose pattern names none.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub event: Color,
    /// Inactive (not currently sounding) events in the visualizers.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub event_inactive: Color,
    /// The visualizers' playhead.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub playhead: Color,
    /// The visualizers' beat/segment grid.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub grid: Color,
    /// Minimap background.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub minimap: Color,
    /// Minimap viewport indicator.
    #[serde(deserialize_with = "de_color", serialize_with = "se_color")]
    pub minimap_viewport: Color,
    /// An explicitly selected visual backend. This is the current theme
    /// format; the flat Hydra fields below remain readable for compatibility
    /// with themes written before renderers were named.
    #[serde(default)]
    pub visual: Option<ThemeVisual>,
    /// An optional glyph animation layered over any visual renderer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub characters: Option<CharacterVisual>,
    /// The theme's own Hydra sketch: one chain in the snippet-shelf grammar,
    /// no `initHydra` - furniture drawn behind the code whenever the score's
    /// picture is not on screen, transport or no transport.
    #[serde(default)]
    pub hydra: Option<String>,
    /// Whether this theme's `s0` is the default webcam. This is an acquisition
    /// declaration, never permission: Settings remains the sole consent gate.
    #[serde(default)]
    pub hydra_camera: bool,
    /// How strongly the theme's sketch shows through, a percentage. Forged:
    /// it is part of the theme's look, not a setting.
    #[serde(default)]
    pub hydra_opacity: Option<u8>,
    /// The `ui opacity` this theme asks for. Superseded by `opacity.interface`
    /// and kept so older theme files still read; a file carrying both is
    /// answered by `opacity`.
    #[serde(default)]
    pub ui_opacity: Option<u8>,
    /// The three opacities this theme suggests for the global settings.
    ///
    /// A suggestion, not a setting. Applying the theme writes these into the
    /// live settings and forgets that the reader ever had different ones -
    /// but only until the reader changes one, after which theirs is the value
    /// that survives a restart. See `StudioPrefs`, where the distinction is
    /// carried by whether the field is present at all.
    #[serde(default, skip_serializing_if = "ThemeOpacity::is_empty")]
    pub opacity: ThemeOpacity,
    pub syntax: SyntaxPalette,
    pub meter: MeterPalette,
}

/// What a theme that names no opacity asks for.
///
/// Compatibility fallback for static themes and user documents. Animated
/// presets store all three defaults in their JSON; Randomize calculates them
/// from its freshly rolled palette and renderer.
pub const THEME_OPACITY_DEFAULT: u8 = 50;

/// The opacities a theme suggests, each absent until someone says otherwise.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ThemeOpacity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<u8>,
}

/// The same three with every question answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Opacities {
    pub backdrop: u8,
    pub interface: u8,
    pub editor: u8,
}

impl ThemeOpacity {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Theme {
    /// The terminal cursor colour, always resolved even for an older theme.
    pub fn caret(&self) -> Color {
        self.caret.unwrap_or(self.foreground)
    }

    /// The saved global shape unless this theme deliberately asks for one.
    pub fn caret_shape(&self, fallback: CaretShape) -> CaretShape {
        self.caret_shape.unwrap_or(fallback)
    }

    /// The colour a replay tab wears.
    pub fn replay_colour(&self) -> Color {
        self.replay.unwrap_or(self.warn)
    }

    /// What this theme asks the global opacity settings to be.
    ///
    /// `ui_opacity` is the older spelling of the interface value and answers
    /// for it when `opacity.interface` is silent, so a theme file written
    /// before this existed still asks for what it always asked for.
    pub fn opacities(&self) -> Opacities {
        Opacities {
            backdrop: self
                .opacity
                .backdrop
                .unwrap_or(THEME_OPACITY_DEFAULT)
                .min(100),
            interface: self
                .opacity
                .interface
                .or(self.ui_opacity)
                .unwrap_or(THEME_OPACITY_DEFAULT)
                .min(100),
            editor: self
                .opacity
                .editor
                .unwrap_or(THEME_OPACITY_DEFAULT)
                .min(100),
        }
    }

    /// The colour a matched bracket pair is shown in.
    pub fn bracket(&self) -> Color {
        bracket_colour(self.bracket.unwrap_or(self.accent))
    }

    /// The caret locator should guide the eye without competing with the caret
    /// or changing syntax colours. Keep its contrast below bracket fills.
    pub fn caret_line_fill(&self) -> Option<Color> {
        subtle_fill(
            self.background,
            mix(self.accent, self.foreground, 0.35),
            1.12,
        )
    }

    /// Underline carets need a filled partner; bars and blocks need an underline.
    /// Both phases of a blinking caret keep the same bracket mark.
    pub fn bracket_style(&self, style: Style, caret: CaretShape, matched: bool) -> Style {
        let block = match self.bracket_mark {
            BracketMark::Auto => matches!(
                self.caret_shape(caret),
                CaretShape::SteadyUnderline | CaretShape::BlinkingUnderline
            ),
            BracketMark::Block => true,
            BracketMark::Underline => false,
        };
        let color = if matched { self.bracket() } else { self.error };
        let ground = style.bg.unwrap_or(self.background);
        if block
            && let Some(fill) = self
                .bracket
                .filter(|_| matched)
                .or_else(|| subtle_fill(ground, mix(color, self.foreground, 0.35), 1.45))
        {
            let fill = if matched { bracket_colour(fill) } else { fill };
            let text = if matched {
                bracket_colour(style.fg.unwrap_or(self.foreground))
            } else {
                color
            };
            return style
                .fg(legible_against_floor(text, fill, 4.5))
                .bg(fill)
                .remove_modifier(Modifier::UNDERLINED)
                .add_modifier(Modifier::BOLD);
        }
        // Terminal-owned colours cannot be measured. An underline remains
        // visible without guessing at their RGB values or inverting the text.
        let style = if matched {
            style.fg(bracket_colour(style.fg.unwrap_or(self.foreground)))
        } else {
            style.fg(color)
        };
        style
            .underline_color(color)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }

    /// Hydra source selected by either the renderer form or the legacy flat
    /// fields.
    pub fn hydra_code(&self) -> Option<&str> {
        match self.visual.as_ref() {
            Some(ThemeVisual::Hydra { code, .. }) => Some(code),
            _ => self.hydra.as_deref(),
        }
    }

    pub fn hydra_camera_enabled(&self) -> bool {
        match self.visual.as_ref() {
            Some(ThemeVisual::Hydra { camera, .. }) => *camera,
            _ => self.hydra_camera,
        }
    }

    /// Whether the theme's own painter draws the camera, rather than a
    /// Hydra sketch sampling it. A declaration and never permission: the
    /// webcam switch in Settings remains the only consent, exactly as for
    /// `hydra_camera`.
    pub fn native_camera_enabled(&self) -> bool {
        self.cell_visual()
            .is_some_and(|cell| cell.effect == CellEffect::Camera)
    }

    /// Whether this theme wants a camera at all, whoever ends up drawing
    /// it. Also a declaration and never permission.
    pub fn camera_enabled(&self) -> bool {
        self.hydra_camera_enabled() || self.native_camera_enabled()
    }

    pub fn hydra_opacity_percent(&self) -> Option<u8> {
        match self.visual.as_ref() {
            Some(ThemeVisual::Hydra { opacity, .. }) => *opacity,
            _ => self.hydra_opacity,
        }
    }

    pub fn cell_visual(&self) -> Option<CellVisual> {
        match self.visual.as_ref() {
            Some(ThemeVisual::TachyonFx {
                effect,
                mode,
                density,
                speed,
            }) => Some(CellVisual {
                effect: *effect,
                mode: *mode,
                density: *density,
                speed: *speed,
            }),
            _ => None,
        }
    }

    /// Replace the current renderer with a Hydra sketch. Used when a theme
    /// author takes a sketch from the shelf while editing a cell theme.
    pub fn set_hydra_code(&mut self, code: String) {
        match self.visual.as_mut() {
            Some(ThemeVisual::Hydra { code: current, .. }) => *current = code,
            Some(ThemeVisual::TachyonFx { .. }) => {
                self.visual = Some(ThemeVisual::Hydra {
                    code,
                    camera: false,
                    opacity: Some(35),
                });
                self.hydra = None;
                self.hydra_camera = false;
                self.hydra_opacity = None;
            }
            None => self.hydra = Some(code),
        }
    }

    pub fn set_hydra_opacity(&mut self, opacity: Option<u8>) -> bool {
        match self.visual.as_mut() {
            Some(ThemeVisual::Hydra {
                opacity: current, ..
            }) => {
                *current = opacity;
                true
            }
            Some(ThemeVisual::TachyonFx { .. }) => false,
            None => {
                self.hydra_opacity = opacity;
                true
            }
        }
    }

    pub fn animates_native_visual(&self) -> bool {
        self.cell_visual().is_some() || self.character_visual().is_some()
    }

    pub fn tachyon_mode(&self) -> Option<TachyonMode> {
        self.cell_visual().map(|visual| visual.mode)
    }

    pub fn set_tachyon_mode(&mut self, mode: TachyonMode) -> bool {
        let Some(ThemeVisual::TachyonFx { mode: current, .. }) = self.visual.as_mut() else {
            return false;
        };
        *current = mode;
        true
    }

    /// Seconds a sounding mark lingers after its event.
    pub fn event_fade(&self) -> f32 {
        self.event_fade
            .filter(|seconds| seconds.is_finite())
            .map_or(0.3, |seconds| seconds.clamp(0.0, 5.0))
    }

    pub fn character_visual(&self) -> Option<CharacterVisual> {
        self.characters
    }

    pub fn set_character_effect(&mut self, effect: Option<CharacterEffect>) -> bool {
        match (self.characters.as_mut(), effect) {
            (Some(current), Some(effect)) => current.effect = effect,
            (None, Some(effect)) => {
                self.characters = Some(CharacterVisual {
                    effect,
                    strength: default_character_strength(),
                    speed: default_character_speed(),
                });
            }
            (Some(_), None) => self.characters = None,
            (None, None) => return false,
        }
        true
    }

    pub fn set_character_strength(&mut self, strength: u8) -> bool {
        let Some(characters) = self.characters.as_mut() else {
            return false;
        };
        characters.strength = strength;
        true
    }

    pub fn set_character_speed(&mut self, speed: u8) -> bool {
        let Some(characters) = self.characters.as_mut() else {
            return false;
        };
        characters.speed = speed;
        true
    }

    /// The compiled-in default.
    pub fn built_in_default() -> Self {
        Self::built_in(DEFAULT_THEME).expect("the default theme is a valid built-in")
    }

    /// Load a compiled-in theme by name.
    pub fn built_in(name: &str) -> Option<Self> {
        // Preserve persisted selections from before the shorter picker name.
        let name = match name {
            "prism-current" => "prism",
            // The cell effect wore both of these before `randomize` became
            // the name of the roll.
            "randomsequence" => "scramble",
            // The pour became a container that fills, breaches and refills.
            "pour" => "leak",
            // `cyberpunk` was the neon-grid scene; the name it wore did not
            // describe it, and the theme that DID hold the name stepped aside.
            "cyberpunk" => "synthwave",
            // The second synthwave became the only one.
            "synthwave2" => "synthwave",
            "chaos" => "lsd",
            "spray" => "snow",
            // The vector tunnel was named for its look; it is named for
            // the machine it is drawn like.
            "retro-3d" => "vectrex",
            name => name,
        };
        if let Some(theme) = BUILT_INS
            .iter()
            .chain(SHOWROOM_THEMES)
            .find(|(key, _)| *key == name)
            .map(|(key, document)| match Self::from_json(document) {
                Ok(theme) => theme.named(key),
                Err(error) => panic!("built-in theme {key} is malformed: {error}"),
            })
        {
            return Some(theme);
        }
        if name == RANDOMIZE_THEME {
            // Every resolution is a new roll, so remembering `randomize` in
            // studio.json means a different studio every launch - which is
            // the point of choosing it.
            return Some(randomize_theme(randomize_seed()));
        }
        None
    }

    /// Names of the compiled-in themes, in presentation order.
    pub fn built_in_names() -> impl Iterator<Item = &'static str> {
        BUILT_INS
            .iter()
            .map(|(name, _)| *name)
            .chain(std::iter::once(RANDOMIZE_THEME))
            .chain(SHOWROOM_THEMES.iter().map(|(name, _)| *name))
    }

    pub fn from_json(document: &str) -> Result<Self, ThemeError> {
        if document.len() > MAX_THEME_DOCUMENT_BYTES {
            return Err(ThemeError::Parse(format!(
                "theme document is {} bytes; maximum is {MAX_THEME_DOCUMENT_BYTES}",
                document.len()
            )));
        }
        let theme: Self =
            serde_json::from_str(document).map_err(|error| ThemeError::Parse(error.to_string()))?;
        if theme.visual.is_some()
            && (theme.hydra.is_some() || theme.hydra_camera || theme.hydra_opacity.is_some())
        {
            return Err(ThemeError::Parse(
                "visual cannot be combined with legacy hydra fields".into(),
            ));
        }
        if theme
            .hydra_opacity_percent()
            .is_some_and(|opacity| opacity > 100)
        {
            return Err(ThemeError::Parse(
                "visual opacity must be between 0 and 100".into(),
            ));
        }
        if let Some(cell) = theme.cell_visual()
            && (cell.density == 0 || cell.density > 100 || cell.speed == 0 || cell.speed > 100)
        {
            return Err(ThemeError::Parse(
                "tachyonfx visual density and speed must be between 1 and 100".into(),
            ));
        }
        if let Some(characters) = theme.character_visual()
            && (characters.strength == 0
                || characters.strength > 100
                || characters.speed == 0
                || characters.speed > 100)
        {
            return Err(ThemeError::Parse(
                "character effect strength and speed must be between 1 and 100".into(),
            ));
        }
        #[cfg(feature = "hydra")]
        if theme.hydra_camera_enabled() {
            let code = theme
                .hydra_code()
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .ok_or_else(|| {
                    ThemeError::Parse(
                        "hydra_camera requires a Hydra sketch that visibly samples s0".into(),
                    )
                })?;
            let node = rustel_hydra::glsl::parse_chain(code).map_err(|error| {
                ThemeError::Parse(format!("camera-backed Hydra sketch: {error}"))
            })?;
            if !hydra_node_uses_s0(&node) {
                return Err(ThemeError::Parse(
                    "hydra_camera requires a Hydra sketch that visibly samples s0".into(),
                ));
            }
        }
        Ok(theme)
    }

    fn named(mut self, name: &str) -> Self {
        if self.name.is_empty() {
            self.name = name.to_owned();
        }
        self
    }

    /// Resolve a theme the person running the studio chose themselves -
    /// `--theme`, or `$RUSTEL_THEME`. A path here is loaded as written,
    /// because someone who can pass a flag can already read their own files
    /// and `docs/studio.md` documents `--theme ./midnight.json`: a theme file
    /// beside the score, not in the theme directory.
    pub fn resolve(selector: Option<&str>) -> Result<Self, ThemeError> {
        Self::resolve_from(selector, Provenance::Chosen)
    }

    /// Resolve a theme the studio remembered for itself, out of
    /// `studio.json`. Confined: that file is written by the picker, is
    /// synced and hand-edited, and has no business naming `/etc/passwd`.
    pub fn resolve_remembered(selector: &str) -> Result<Self, ThemeError> {
        Self::resolve_from(Some(selector), Provenance::Remembered)
    }

    fn resolve_from(selector: Option<&str>, provenance: Provenance) -> Result<Self, ThemeError> {
        let environment = selector
            .is_none()
            .then(|| std::env::var("RUSTEL_THEME").ok())
            .flatten()
            .filter(|value| !value.is_empty());
        let selector = selector.or(environment.as_deref()).unwrap_or(DEFAULT_THEME);
        if selector.is_empty() {
            return Err(ThemeError::EmptySelector);
        }
        if looks_like_path(selector) {
            return match provenance {
                Provenance::Chosen => Self::load_file(Path::new(selector)),
                Provenance::Remembered => Self::load_confined_theme_file(selector),
            };
        }
        for directory in theme_read_directories() {
            let candidate = directory.join(format!("{selector}.json"));
            if is_real_theme_file(&candidate) {
                return Self::load_file(&candidate);
            }
        }
        Self::built_in(selector).ok_or_else(|| ThemeError::Unknown {
            name: selector.to_owned(),
            available: Self::available_names(),
        })
    }

    fn load_file(path: &Path) -> Result<Self, ThemeError> {
        let file = std::fs::File::open(path).map_err(|error| ThemeError::Read {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        let mut document = String::new();
        file.take(MAX_THEME_DOCUMENT_BYTES as u64 + 1)
            .read_to_string(&mut document)
            .map_err(|error| ThemeError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        if document.len() > MAX_THEME_DOCUMENT_BYTES {
            return Err(ThemeError::Read {
                path: path.to_path_buf(),
                message: format!("theme file exceeds {MAX_THEME_DOCUMENT_BYTES} bytes"),
            });
        }
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::from_json(&document)
            .map_err(|error| ThemeError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
            .map(|theme| theme.named(&stem))
    }

    /// Path-shaped selectors only load files beneath the theme directory.
    /// A hand-edited `studio.json` or `$RUSTEL_THEME` that names `/etc/passwd`
    /// (or any escape with `..`) must not become an arbitrary filesystem read.
    fn load_confined_theme_file(selector: &str) -> Result<Self, ThemeError> {
        let directories = theme_read_directories();
        let Some(primary) = directories.first().cloned() else {
            return Err(ThemeError::Read {
                path: PathBuf::from(selector),
                message: "no theme directory is configured".into(),
            });
        };
        let raw = Path::new(selector);
        let reported_candidate = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            primary.join(raw)
        };
        for directory in directories {
            let candidate = if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                directory.join(raw)
            };
            if !is_real_theme_file(&candidate) {
                continue;
            }
            let Ok(directory) = directory.canonicalize() else {
                continue;
            };
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if canonical.starts_with(&directory) {
                return Self::load_file(&canonical);
            }
        }
        Err(ThemeError::Read {
            path: reported_candidate,
            message: format!("theme file is not reachable beneath {}", primary.display()),
        })
    }

    /// Built-in and user theme names, deduplicated and sorted.
    pub fn available_names() -> Vec<String> {
        let mut names = Self::built_in_names()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for directory in theme_read_directories() {
            if let Ok(entries) = std::fs::read_dir(directory) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_real_theme_file(&path)
                        && let Some(stem) = path.file_stem()
                    {
                        names.push(stem.to_string_lossy().into_owned());
                    }
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }
}

/// The default theme with every colour it draws from replaced.
///
/// Maps Randomize's nine rolled palette colours onto the whole of [`Theme`].
fn theme_from_palette(name: &str, palette: ThemePalette) -> Theme {
    let document = BUILT_INS
        .iter()
        .find(|(key, _)| *key == DEFAULT_THEME)
        .map(|(_, document)| *document)
        .expect("the default theme document is compiled in");
    let mut theme = Theme::from_json(document)
        .unwrap_or_else(|error| panic!("built-in theme {DEFAULT_THEME} is malformed: {error}"));
    theme.name = name.to_owned();
    theme.background = palette.background;
    theme.surface = palette.surface;
    theme.overlay = palette.overlay;
    theme.foreground = palette.foreground;
    theme.muted = palette.muted;
    theme.rule = palette.rule;
    theme.accent = palette.accent;
    theme.ok = palette.accent;
    theme.caret = Some(palette.foreground);
    theme.selection = palette.selection;
    theme.selection_text = palette.foreground;
    theme.mini = palette.accent;
    theme.bracket = None;
    theme.event = palette.accent;
    theme.event_inactive = palette.muted;
    theme.playhead = palette.foreground;
    theme.grid = palette.rule;
    theme.minimap = palette.surface;
    theme.minimap_viewport = palette.selection;
    theme.syntax.text = palette.foreground;
    theme.syntax.comment = palette.muted;
    theme.syntax.string = palette.accent;
    theme.syntax.number = palette.secondary;
    theme.syntax.punctuation = palette.secondary;
    theme.syntax.keyword = Some(palette.accent);
    theme.syntax.function = Some(palette.secondary);
    theme.meter.low = palette.accent;
    theme.meter.mid = palette.secondary;
    theme.meter.track = palette.overlay;
    theme.meter.fader = palette.foreground;
    theme
}

/// The name of the theme that is a different theme every time.
pub const RANDOMIZE_THEME: &str = "randomize";

struct ThemeRng(u32);
impl ThemeRng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(self.0) / f64::from(u32::MAX)
    }
    fn int(&mut self, low: i32, high: i32) -> i32 {
        low + ((self.next() * f64::from(high - low + 1)) as i32).min(high - low)
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let at = ((self.next() * items.len() as f64) as usize).min(items.len() - 1);
        &items[at]
    }
}

/// A theme rolled from a seed: palette, renderer and glyph animation.
///
/// The same seed gives the same theme, so a roll worth keeping can
/// be had again, and `e` in the picker opens this exact one in the editor.
///
/// The palette is built in HSL rather than picked out of a list, because the
/// interesting axis is *relation*: a random hue is fine, but a random
/// foreground against a random background is unreadable about half the time.
/// So one hue is rolled and the rest of the palette is placed around it at
/// fixed distances - the ground always dark and desaturated, the text always
/// near-white, the accents always across the wheel from each other.
pub fn randomize_theme(seed: u32) -> Theme {
    let mut rng = ThemeRng(seed | 1);
    let hue = rng.next() * 360.0;
    // Away from the seed hue, but never so far that the two read as one.
    let partner = (hue + 90.0 + rng.next() * 180.0) % 360.0;
    let saturation = 0.55 + rng.next() * 0.4;
    // Light themes are a third of the rolls: rarer, because a bright ground
    // under a picture is a harder thing to read code on.
    let light = rng.next() < 0.34;
    let (ground, surface, overlay, rule, text, selection_light) = if light {
        (0.96, 0.92, 0.87, 0.78, 0.14, 0.82)
    } else {
        (0.06, 0.10, 0.15, 0.26, 0.95, 0.30)
    };
    let background = hsl(hue, saturation * if light { 0.10 } else { 0.35 }, ground);
    // Every colour that has to be read is placed by measurement against the
    // ground it will be read on, never by lightness alone.
    let palette = ThemePalette {
        background,
        surface: hsl(hue, saturation * if light { 0.14 } else { 0.38 }, surface),
        overlay: hsl(hue, saturation * if light { 0.18 } else { 0.40 }, overlay),
        foreground: readable(hue, saturation * 0.30, text, background, TEXT_CONTRAST),
        muted: readable(
            hue,
            saturation * 0.30,
            if light { 0.48 } else { 0.55 },
            background,
            ACCENT_CONTRAST,
        ),
        rule: hsl(hue, saturation * if light { 0.20 } else { 0.35 }, rule),
        accent: readable(
            hue,
            saturation,
            if light { 0.42 } else { 0.66 },
            background,
            ACCENT_CONTRAST,
        ),
        secondary: readable(
            partner,
            saturation,
            if light { 0.40 } else { 0.68 },
            background,
            ACCENT_CONTRAST,
        ),
        selection: hsl(
            partner,
            saturation * if light { 0.35 } else { 0.45 },
            selection_light,
        ),
    };
    let mut theme = theme_from_palette(RANDOMIZE_THEME, palette);
    theme.warn = readable(
        partner,
        saturation,
        if light { 0.38 } else { 0.62 },
        background,
        ACCENT_CONTRAST,
    );
    theme.error = readable(
        (hue + 350.0) % 360.0,
        0.75,
        if light { 0.42 } else { 0.64 },
        background,
        ACCENT_CONTRAST,
    );
    theme.event_mark = MarkStyle::Tint;

    // Most rolls use a shipped Hydra example. The rest use cell effects,
    // which are also available in builds without Hydra.
    let hydra = cfg!(feature = "hydra") && rng.next() < 0.6;
    theme.visual = Some(if hydra {
        let sketches = super::examples::embedded()
            .iter()
            .filter(|entry| entry.kind == super::examples::Kind::Hydra)
            .collect::<Vec<_>>();
        ThemeVisual::Hydra {
            code: rng.pick(&sketches).code.clone(),
            camera: false,
            // Furniture, not the show: enough to see, never enough to fight
            // the code. The reader's `visuals opacity` still caps it.
            opacity: Some(22 + rng.int(0, 26) as u8),
        }
    } else {
        let effect = *rng.pick(
            &SHOWROOM_EFFECTS
                .iter()
                .map(|(_, effect)| *effect)
                .collect::<Vec<_>>(),
        );
        ThemeVisual::TachyonFx {
            effect,
            mode: TachyonMode::Text,
            density: 45 + rng.int(0, 45) as u8,
            speed: 35 + rng.int(0, 45) as u8,
        }
    });
    // A glyph animation on roughly a third of them: over a Hydra sketch it
    // is the second layer, and on its own it is the whole look.
    if rng.next() < 0.35 {
        theme.characters = Some(CharacterVisual {
            effect: *rng.pick(&[
                CharacterEffect::Fade,
                CharacterEffect::Aurora,
                CharacterEffect::Hologram,
                CharacterEffect::Rainbow,
            ]),
            strength: 35 + rng.int(0, 40) as u8,
            speed: 30 + rng.int(0, 50) as u8,
        });
    }
    theme.balance_opacities();
    theme
}

/// What code has to clear against the ground it is read on, and what an
/// accent, a warning or a line number has to. The first is WCAG's AAA for
/// body text; the second its floor for something that only has to be *seen*.
const TEXT_CONTRAST: f32 = 7.5;
const ACCENT_CONTRAST: f32 = 3.5;

/// Walk a colour's lightness until it stands off `ground`.
///
/// A fixed HSL lightness is not a fixed contrast: yellow at `0.42` is far
/// brighter than blue at `0.42`, so a palette placed by lightness alone comes
/// out readable for some hues and not for others. Rolling a hue and then
/// *measuring* is the difference between most rolls being legible and all of
/// them being legible.
fn readable(hue: f64, saturation: f64, lightness: f64, ground: Color, target: f32) -> Color {
    // Toward whichever end of the scale is away from the ground.
    let toward = if contrast_ratio(ground, Color::Rgb(0, 0, 0))
        > contrast_ratio(ground, Color::Rgb(255, 255, 255))
    {
        -0.015
    } else {
        0.015
    };
    let mut lightness = lightness;
    let mut colour = hsl(hue, saturation, lightness);
    // Bounded: a hue that cannot reach the target ends at the extreme it
    // walked to, which is the most contrast that hue has to give.
    for _ in 0..64 {
        if contrast_ratio(colour, ground) >= target {
            break;
        }
        lightness = (lightness + toward).clamp(0.0, 1.0);
        colour = hsl(hue, saturation, lightness);
    }
    colour
}

/// A seed nobody chose, for the first roll of a session.
pub fn randomize_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9e37_79b9, |since| {
            since.subsec_nanos() ^ since.as_secs() as u32
        })
        | 1
}

/// HSL to the RGB a terminal wants. Hue in degrees, the rest in `0..=1`.
fn hsl(hue: f64, saturation: f64, lightness: f64) -> Color {
    let hue = hue.rem_euclid(360.0);
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let second = chroma * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let (red, green, blue) = match hue as u32 / 60 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let base = lightness - chroma / 2.0;
    let byte = |value: f64| ((value + base) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(byte(red), byte(green), byte(blue))
}

#[derive(Clone, Copy)]
struct ThemePalette {
    background: Color,
    surface: Color,
    overlay: Color,
    foreground: Color,
    muted: Color,
    rule: Color,
    accent: Color,
    secondary: Color,
    selection: Color,
}

fn is_real_theme_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "json")
        && std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

/// A camera declaration is allowed to acquire only when the rendered chain
/// structurally consumes s0. A flag beside an unrelated oscillator must never
/// turn into invisible capture.
#[cfg(feature = "hydra")]
pub(crate) fn hydra_node_uses_s0(node: &rustel_hydra::HydraNode) -> bool {
    rustel_hydra::glsl::explicitly_samples(node, "s0")
}

impl Default for Theme {
    fn default() -> Self {
        Self::built_in_default()
    }
}

/// The theme picker: every theme by name, the one under the cursor shown
/// live, the one the picker opened on kept if it is closed without choosing.
#[derive(Clone, Debug)]
pub struct ThemePicker {
    pub names: Vec<String>,
    /// A lowercase substring typed directly into the picker.
    pub query: String,
    pub selected: usize,
    /// What was on screen when the picker opened.
    pub original: Box<Theme>,
    /// A delete waiting for its second press, by name - one `d` asks, the
    /// next confirms, anything else calls it off.
    pub deleting: Option<String>,
    /// Which roll of [`RANDOMIZE_THEME`] the arrows are showing. Held by the
    /// picker rather than the app because it is a property of *browsing*:
    /// the reader is walking a list, and on that one row the list is
    /// infinite in both directions.
    pub randomize_seed: u32,
    /// Whether the randomize row has been rolled yet. Only the forward arrow is
    /// offered until it has: before the first roll there is nothing behind to
    /// go back to, and an affordance for a move that does nothing teaches the
    /// wrong thing.
    pub rolled: bool,
    /// The first row the list draws, kept from move to move so walking the
    /// middle of the list leaves it still. A cell, because it is settled
    /// where the list's height is known - drawing and hit-testing - and both
    /// read the picker. See [`super::scroll`].
    pub scroll: std::cell::Cell<usize>,
    /// The selection was last put there by a click: until a key or the
    /// wheel moves it the list keeps no margin, so the clicked row stays
    /// under the pointer.
    pub hold_scroll: bool,
}

impl ThemePicker {
    pub fn open(current: &Theme) -> Self {
        let names = Theme::available_names();
        let selected = names
            .iter()
            .position(|name| *name == current.name)
            .unwrap_or(0);
        Self {
            names,
            query: String::new(),
            selected,
            original: Box::new(current.clone()),
            deleting: None,
            randomize_seed: randomize_seed(),
            rolled: false,
            scroll: std::cell::Cell::new(0),
            hold_scroll: false,
        }
    }

    /// Whether the row under the cursor is the one that rolls a new theme.
    pub fn on_randomize(&self) -> bool {
        self.selected_name() == Some(RANDOMIZE_THEME)
    }

    /// Roll the next (or previous) [`RANDOMIZE_THEME`], and answer with its seed.
    ///
    /// Stepping the seed rather than drawing a fresh one is what makes ← and
    /// → a pair: a roll walked past can be walked back to.
    pub fn roll_randomize(&mut self, forwards: bool) -> u32 {
        self.rolled = true;
        // An odd stride over the whole `u32` ring: every step lands
        // somewhere unrelated, and going back lands exactly where it was.
        // Odd, so the walk visits every seed before it repeats. Nothing is
        // forced onto the result: `randomize_theme` normalises the seed itself,
        // and a step that adjusted its own answer would not be reversible.
        const STRIDE: u32 = 0x9e37_79b9;
        self.randomize_seed = if forwards {
            self.randomize_seed.wrapping_add(STRIDE)
        } else {
            self.randomize_seed.wrapping_sub(STRIDE)
        };
        self.randomize_seed
    }

    pub fn move_by(&mut self, delta: isize) {
        let count = self.match_count();
        if count == 0 {
            return;
        }
        let len = count as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(len)) as usize;
        self.hold_scroll = false;
    }

    /// The first row the list draws, `shown` rows at a time: the selection
    /// among them, with a margin of rows around it unless a click put it
    /// there.
    pub fn first_row(&self, shown: usize) -> usize {
        let margin = if self.hold_scroll {
            0
        } else {
            super::scroll::margin(shown)
        };
        let first = super::scroll::follow(
            self.scroll.get(),
            self.selected,
            shown,
            self.match_count(),
            margin,
        );
        self.scroll.set(first);
        first
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.matches().nth(self.selected).map(String::as_str)
    }

    pub fn matches(&self) -> impl Iterator<Item = &String> {
        self.names
            .iter()
            .filter(|name| self.query.is_empty() || name.to_ascii_lowercase().contains(&self.query))
    }

    pub fn match_count(&self) -> usize {
        self.matches().count()
    }

    pub fn push_query(&mut self, character: char) {
        self.query.extend(character.to_lowercase());
        self.selected = 0;
        self.deleting = None;
        self.hold_scroll = false;
    }

    pub fn pop_query(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.deleting = None;
        self.hold_scroll = false;
    }

    /// The panel's place: bottom-right, like the device picker.
    pub fn geometry(&self, available: Rect) -> Option<(Rect, Rect)> {
        let rows = (self.match_count() as u16).clamp(1, 12);
        let height = rows + 6;
        let width = 34u16;
        if available.width < width + 2 || available.height < height + 1 {
            return None;
        }
        let area = Rect::new(
            available.right().saturating_sub(width + 1),
            available.bottom().saturating_sub(height + 1),
            width,
            height,
        );
        let list = Rect::new(area.x + 2, area.y + 2, area.width.saturating_sub(4), rows);
        Some((area, list))
    }

    /// The row under a pointer, if any.
    pub fn row_at(&self, available: Rect, x: u16, y: u16) -> Option<usize> {
        let (_, list) = self.geometry(available)?;
        if x < list.x || x >= list.right() || y < list.y || y >= list.bottom() {
            return None;
        }
        let first = self.first_row(usize::from(list.height));
        let index = first + usize::from(y - list.y);
        (index < self.match_count()).then_some(index)
    }
}

/// What the picker says under a theme that shows the webcam, in place of
/// the browsing hint: the one thing a reader cannot guess from the list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CameraNote {
    /// The Hydra webcam setting is off: the theme would show nothing.
    Off,
    /// Allowed and asked for, no frame on screen yet.
    Starting,
    Live,
    Failed,
}

impl CameraNote {
    /// Two short lines, so they fit the picker however narrow it is.
    pub fn lines(self) -> [&'static str; 2] {
        match self {
            Self::Off => [
                "uses your webcam, which is off",
                "turn on Hydra webcam in Settings",
            ],
            Self::Starting => ["webcam starting", "the picture lands in a moment"],
            Self::Live => ["webcam live", "Enter keeps this theme · Esc back"],
            Self::Failed => ["the webcam could not open", "see Settings · Esc back"],
        }
    }
}

pub struct ThemePickerView<'a> {
    pub picker: &'a ThemePicker,
    pub theme: &'a Theme,
    /// Under a camera theme: what the reader needs to know to see it.
    pub camera: Option<CameraNote>,
}

impl ratatui::widgets::Widget for ThemePickerView<'_> {
    fn render(self, area: Rect, buffer: &mut ratatui::buffer::Buffer) {
        let Some((panel, list)) = self.picker.geometry(area) else {
            return;
        };
        let theme = self.theme;
        super::view::clear_overlay(
            buffer,
            panel,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        super::devices::draw_border(buffer, panel, theme);
        buffer.set_stringn(
            panel.x + 2,
            panel.y,
            " theme ",
            usize::from(panel.width.saturating_sub(4)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        let match_count = self.picker.match_count();
        let position = if match_count == 0 {
            " 0/0 ".to_owned()
        } else {
            format!(
                " {}/{} ",
                self.picker.selected.min(match_count - 1) + 1,
                match_count
            )
        };
        buffer.set_stringn(
            panel.right().saturating_sub(position.len() as u16 + 1),
            panel.y,
            &position,
            position.len(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        let prompt = if self.picker.query.is_empty() {
            "search: type a theme".to_owned()
        } else {
            format!("search: {}_", self.picker.query)
        };
        buffer.set_stringn(
            list.x,
            panel.y + 1,
            &prompt,
            usize::from(list.width),
            Style::default().fg(theme.muted),
        );
        let first = self.picker.first_row(usize::from(list.height));
        for (row, name) in self
            .picker
            .matches()
            .enumerate()
            .skip(first)
            .take(usize::from(list.height))
        {
            let selected = row == self.picker.selected;
            let style = if selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            let marker = if selected {
                format!("{} ", crate::terminal::symbol("▸"))
            } else {
                "  ".to_owned()
            };
            let built_in = Theme::built_in_names().any(|built| built == name);
            let text = if name == RANDOMIZE_THEME {
                // The one row whose ←/→ do something says so. Solid triangles
                // rather than outlines, because at one cell an outline reads
                // as noise; and they breathe outward on a slow sine so the
                // affordance is noticed without being a blink. The back arrow
                // waits for the first roll, since before that there is nothing
                // behind to go back to.
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as f64;
                let gap = " ".repeat(1 + usize::from((millis / 420.0).sin() > 0.0));
                let back = if self.picker.rolled {
                    format!("\u{25c0}{gap}")
                } else {
                    String::new()
                };
                format!("{marker}{back}{name}{gap}\u{25b6}")
            } else {
                format!("{marker}{name}{}", if built_in { "" } else { "  (yours)" })
            };
            if name == RANDOMIZE_THEME {
                let phase = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    / 90;
                for (column, character) in text.chars().take(usize::from(list.width)).enumerate() {
                    const RAINBOW: [Color; 6] = [
                        Color::Rgb(255, 78, 144),
                        Color::Rgb(255, 158, 72),
                        Color::Rgb(255, 226, 92),
                        Color::Rgb(67, 230, 159),
                        Color::Rgb(71, 194, 255),
                        Color::Rgb(190, 104, 255),
                    ];
                    let color = RAINBOW[(column + phase as usize) % RAINBOW.len()];
                    buffer.set_string(
                        list.x + column as u16,
                        list.y + (row - first) as u16,
                        character.to_string(),
                        style.fg(color),
                    );
                }
            } else {
                buffer.set_stringn(
                    list.x,
                    list.y + (row - first) as u16,
                    &text,
                    usize::from(list.width),
                    style,
                );
            }
        }
        let (lines, colour) = match self.camera {
            Some(note @ (CameraNote::Off | CameraNote::Failed)) => (note.lines(), theme.warn),
            Some(note @ CameraNote::Starting) => (note.lines(), theme.accent),
            Some(note @ CameraNote::Live) => (note.lines(), theme.ok),
            None => (
                ["↑/↓ browse · Enter keep", "Esc back · ^n/e/d"],
                theme.muted,
            ),
        };
        for (offset, line) in lines.into_iter().enumerate() {
            buffer.set_stringn(
                list.x,
                panel.bottom().saturating_sub(3 - offset as u16),
                line,
                usize::from(list.width),
                Style::default().fg(colour),
            );
        }
    }
}

fn looks_like_path(selector: &str) -> bool {
    selector.contains(std::path::MAIN_SEPARATOR)
        || selector.contains('/')
        || selector.ends_with(".json")
}

/// Where new and changed user themes live: `$RUSTEL_THEME_DIR`, else the
/// `themes` folder in Rustel's config directory.
/// Where a theme selector came from, which decides whether a path in it is
/// taken at face value.
#[derive(Clone, Copy)]
enum Provenance {
    /// Typed by the person running the studio: a flag or the environment.
    Chosen,
    /// Read back out of `studio.json`.
    Remembered,
}

pub fn theme_directory() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("RUSTEL_THEME_DIR") {
        return Some(PathBuf::from(directory));
    }
    super::config::directory().map(|directory| directory.join("themes"))
}

fn theme_read_directories() -> Vec<PathBuf> {
    if let Some(directory) = std::env::var_os("RUSTEL_THEME_DIR") {
        vec![PathBuf::from(directory)]
    } else {
        super::config::read_directories("themes")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThemeError {
    EmptySelector,
    Parse(String),
    Read {
        path: PathBuf,
        message: String,
    },
    Unknown {
        name: String,
        available: Vec<String>,
    },
}

impl fmt::Display for ThemeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySelector => formatter.write_str("theme name is empty"),
            Self::Parse(message) => write!(formatter, "theme is not valid: {message}"),
            Self::Read { path, message } => {
                write!(
                    formatter,
                    "could not read theme {}: {message}",
                    path.display()
                )
            }
            Self::Unknown { name, available } => write!(
                formatter,
                "unknown theme {name:?}; available themes: {}",
                available.join(", ")
            ),
        }
    }
}

impl std::error::Error for ThemeError {}

/// A `#rgb`, `#rrggbb`, CSS colour name, or ANSI palette name.
///
/// Deserialization is defined on Ratatui's `Color` through this shim so theme
/// files stay readable instead of carrying `{"Rgb":[18,20,25]}` tuples.
pub fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        // ASCII hex digits only. `hex.len()` counts bytes, so a multi-byte
        // character could land in a branch below and split under its fixed
        // slices, and `from_str_radix` would take a leading `+` in a pair. A
        // score controls this string and it is parsed per frame.
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        return match hex.len() {
            3 => {
                let expand = |index: usize| {
                    let digit = u8::from_str_radix(&hex[index..index + 1], 16).ok()?;
                    Some(digit * 17)
                };
                Some(Color::Rgb(expand(0)?, expand(1)?, expand(2)?))
            }
            6 => Some(Color::Rgb(
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            )),
            _ => None,
        };
    }
    let lowered = value.to_ascii_lowercase();
    if let Some(index) = lowered.strip_prefix("ansi") {
        return index.trim().parse::<u8>().ok().map(Color::Indexed);
    }
    named_color(&lowered)
}

/// CSS-ish colour names, sharing one table with the event colours that scores
/// name through `.color(...)`.
fn named_color(lowered: &str) -> Option<Color> {
    let color = match lowered {
        "reset" | "default" => Color::Reset,
        "black" => Color::Rgb(0, 0, 0),
        "silver" => Color::Rgb(192, 192, 192),
        "gray" | "grey" => Color::Rgb(128, 128, 128),
        "white" => Color::Rgb(255, 255, 255),
        "maroon" => Color::Rgb(128, 0, 0),
        "red" => Color::Rgb(255, 107, 107),
        "purple" => Color::Rgb(128, 0, 128),
        "magenta" | "fuchsia" => Color::Rgb(217, 134, 255),
        "green" => Color::Rgb(112, 217, 139),
        "lime" => Color::Rgb(0, 255, 0),
        "olive" => Color::Rgb(128, 128, 0),
        "yellow" => Color::Rgb(255, 202, 40),
        "gold" => Color::Rgb(255, 215, 0),
        "navy" => Color::Rgb(0, 0, 128),
        "blue" => Color::Rgb(79, 140, 255),
        "teal" => Color::Rgb(0, 128, 128),
        "aqua" | "cyan" => Color::Rgb(85, 214, 232),
        "orange" => Color::Rgb(255, 159, 67),
        "orangered" => Color::Rgb(255, 69, 0),
        "pink" | "hotpink" => Color::Rgb(255, 112, 166),
        "salmon" => Color::Rgb(250, 128, 114),
        "steelblue" => Color::Rgb(70, 130, 180),
        "turquoise" => Color::Rgb(64, 224, 208),
        "violet" => Color::Rgb(238, 130, 238),
        "darkgray" | "darkgrey" => Color::Rgb(64, 64, 64),
        "lightgray" | "lightgrey" => Color::Rgb(211, 211, 211),
        _ => return None,
    };
    Some(color)
}

/// Every colour name [`named_color`] knows, in the order a picker shows
/// them: the greys, then round the wheel. A score's `.color(…)` and a
/// theme's colours read the same table, so this is the list for both.
pub const COLOR_NAMES: &[&str] = &[
    "black",
    "darkgray",
    "gray",
    "silver",
    "lightgray",
    "white",
    "maroon",
    "red",
    "orangered",
    "orange",
    "gold",
    "yellow",
    "olive",
    "lime",
    "green",
    "teal",
    "turquoise",
    "aqua",
    "cyan",
    "steelblue",
    "navy",
    "blue",
    "purple",
    "violet",
    "magenta",
    "fuchsia",
    "pink",
    "hotpink",
    "salmon",
];

/// The colours a blend can be computed from. `Reset` and the indexed
/// palette have no known RGB because the terminal owns them. A mid-grey
/// stand-in would make every tint and every animated blend of a
/// reset-coloured theme collapse onto a grey that belongs to no palette.
pub fn true_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        Color::Black => Some((0, 0, 0)),
        Color::White => Some((255, 255, 255)),
        Color::Red => Some((255, 0, 0)),
        Color::Green => Some((0, 255, 0)),
        Color::Blue => Some((0, 0, 255)),
        Color::Yellow => Some((255, 255, 0)),
        Color::Magenta => Some((255, 0, 255)),
        Color::Cyan => Some((0, 255, 255)),
        _ => None,
    }
}

/// How much of a fader cap's own colour lands on the ground it covers.
///
/// Half: the cap has to be obvious enough to aim a pointer at, and what
/// it covers is the meter you are reading while you move it. A solid
/// block hides the meter exactly where you are looking.
///
/// One number for every fader in the studio - the desk's strips and the
/// footer's master - because two caps blended differently read as two
/// different controls.
pub const FADER_HANDLE_OPACITY: f32 = 0.5;

/// Mix `left` towards `right` by `right_amount`, 0 through 1.
pub fn mix(left: Color, right: Color, right_amount: f32) -> Color {
    let amount = right_amount.clamp(0.0, 1.0);
    // A blend needs both sides to be real RGB. When the theme defers to
    // the terminal's palette there is nothing to compute with, so the mix
    // SNAPS to whichever side carries more weight: a decorative tint
    // (small amount) quietly disappears, an animated fade becomes a blink
    // between two palette colours, and no colour is ever invented.
    let (Some((lr, lg, lb)), Some((rr, rg, rb))) = (true_rgb(left), true_rgb(right)) else {
        return if amount >= 0.5 { right } else { left };
    };
    let blend = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    Color::Rgb(blend(lr, rr), blend(lg, rg), blend(lb, rb))
}

impl Theme {
    /// The theme as its own file format, ready to save or share.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Whether a user theme file of this name already sits in the theme
/// directory. Built-ins are not files and answer false.
pub fn user_theme_exists(name: &str) -> bool {
    let Ok(name) = valid_theme_name(name) else {
        return false;
    };
    theme_directory()
        .map(|directory| directory.join(format!("{name}.json")).is_file())
        .unwrap_or(false)
}

/// Write a theme into the theme directory under `name`, creating the
/// directory on the way. The name is a file stem, never a path, and never
/// a built-in's name: a changed built-in is saved as a new theme.
pub fn save_named(theme: &Theme, name: &str) -> Result<std::path::PathBuf, ThemeError> {
    let name = valid_theme_name(name)?;
    if Theme::built_in_names().any(|built_in| built_in == name) {
        return Err(ThemeError::Read {
            path: std::path::PathBuf::from(name),
            message: format!("{name} is built in - save your version under a new name"),
        });
    }
    let Some(directory) = theme_directory() else {
        return Err(ThemeError::Read {
            path: std::path::PathBuf::from("themes"),
            message: "no configuration directory to save into".to_owned(),
        });
    };
    std::fs::create_dir_all(&directory).map_err(|error| ThemeError::Read {
        path: directory.clone(),
        message: error.to_string(),
    })?;
    let path = directory.join(format!("{name}.json"));
    let mut named = theme.clone();
    named.name = name.to_owned();
    std::fs::write(&path, named.to_json()).map_err(|error| ThemeError::Read {
        path: path.clone(),
        message: error.to_string(),
    })?;
    Ok(path)
}

/// Delete a user theme by name. Only files in the theme directory go;
/// built-ins are compiled in and have no file to delete.
pub fn delete_named(name: &str) -> Result<std::path::PathBuf, ThemeError> {
    let name = valid_theme_name(name)?;
    let Some(directory) = theme_directory() else {
        return Err(ThemeError::Read {
            path: std::path::PathBuf::from("themes"),
            message: "no configuration directory".to_owned(),
        });
    };
    let path = directory.join(format!("{name}.json"));
    if !path.is_file() {
        return Err(ThemeError::Unknown {
            name: name.to_owned(),
            available: Theme::available_names(),
        });
    }
    std::fs::remove_file(&path).map_err(|error| ThemeError::Read {
        path: path.clone(),
        message: error.to_string(),
    })?;
    Ok(path)
}

/// A theme name fit to be a file stem: something, and no path in it.
fn valid_theme_name(name: &str) -> Result<&str, ThemeError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ThemeError::EmptySelector);
    }
    // ':' would be a Windows drive prefix ("C:name" replaces the whole
    // base in Path::join) or an NTFS stream; neither is a file stem.
    if name.contains(['/', '\\', '.', ':']) || name.contains("..") {
        return Err(ThemeError::Read {
            path: std::path::PathBuf::from(name),
            message: "a theme name is a file stem: letters, digits, - and _".to_owned(),
        });
    }
    Ok(name)
}

/// Best-effort RGB for blending. Non-RGB terminal colours fall back to a
/// mid-grey so a mix never produces a wildly wrong hue.
/// A colour from hue in degrees, saturation and value in 0..1.
pub fn hsv(hue: f32, saturation: f32, value: f32) -> Color {
    let hue = hue.rem_euclid(360.0) / 60.0;
    let sector = hue.floor() as i32;
    let fraction = hue - hue.floor();
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * fraction);
    let t = value * (1.0 - saturation * (1.0 - fraction));
    let (r, g, b) = match sector.rem_euclid(6) {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };
    Color::Rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

pub fn rgb_parts(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(red, green, blue) => (red, green, blue),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Red => (255, 0, 0),
        Color::Green => (0, 255, 0),
        Color::Blue => (0, 0, 255),
        Color::Yellow => (255, 255, 0),
        Color::Magenta => (255, 0, 255),
        Color::Cyan => (0, 255, 255),
        _ => (128, 128, 128),
    }
}

/// `color`, moved until it can be read against `ground`.
///
/// Its hue is kept and only its lightness travels, away from the ground -
/// so a green mark on a green string stays green and becomes a green
/// somebody can actually see. Toward black that holds its saturation too;
/// toward white it necessarily washes out, which is what moving a colour
/// to meet a dark ground means. A colour that already clears the floor is
/// returned untouched, which is almost always.
///
/// The floor is 3:1 rather than the 4.5:1 body-text figure: a mark is a
/// short span the eye is already looking at, drawn bold, and it is more
/// useful for it to stay close to the colour the score asked for than to
/// be dragged to the nearest extreme.
fn legible_against(color: Color, ground: Color) -> Color {
    legible_against_floor(color, ground, 3.0)
}

pub(crate) fn legible_against_floor(color: Color, ground: Color, floor: f32) -> Color {
    let (Some(from), true) = (true_rgb(color), true_rgb(ground).is_some()) else {
        // A palette colour whose real value only the terminal knows: there
        // is nothing to measure and nothing to move.
        return color;
    };
    if contrast_ratio(color, ground) >= floor {
        return color;
    }
    // Toward white on a dark ground, toward black on a light one - the
    // direction that has room to travel.
    let target = if contrast_ratio(Color::White, ground) >= contrast_ratio(Color::Black, ground) {
        255.0_f32
    } else {
        0.0_f32
    };
    let mut best = color;
    // Sixteen steps is finer than the eye reads on a terminal cell and
    // bounded, which matters: this runs for every marked cell of every
    // frame.
    for step in 1..=16 {
        let amount = step as f32 / 16.0;
        let mix = |channel: u8| {
            (f32::from(channel) + (target - f32::from(channel)) * amount).round() as u8
        };
        best = Color::Rgb(mix(from.0), mix(from.1), mix(from.2));
        if contrast_ratio(best, ground) >= floor {
            break;
        }
    }
    best
}

/// Matching is navigation, so red and rose tones must not look like an error.
/// Use a neutral with the same luminance, retaining the calculated contrast
/// even when a red background or custom bracket colour supplied the tint.
fn bracket_colour(color: Color) -> Color {
    let Some((red, green, blue)) = true_rgb(color) else {
        return match color {
            Color::LightRed => Color::Gray,
            Color::Indexed(_) => Color::Cyan,
            _ => color,
        };
    };
    if red <= green || red <= blue {
        return color;
    }
    let target = contrast_ratio(color, Color::Black);
    let mut low = 0u16;
    let mut high = 255u16;
    while low < high {
        let mid = (low + high) / 2;
        let gray = mid as u8;
        if contrast_ratio(Color::Rgb(gray, gray, gray), Color::Black) < target {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let gray = low as u8;
    Color::Rgb(gray, gray, gray)
}

/// A quiet, theme-coloured fill with consistent visual weight on both light
/// and dark backgrounds. Solve for contrast instead of using a fixed opacity,
/// which can be invisible on one palette and glaring on another.
fn subtle_fill(ground: Color, tint: Color, contrast: f32) -> Option<Color> {
    true_rgb(ground)?;
    true_rgb(tint)?;
    let tint = legible_against(tint, ground);
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..12 {
        let amount = (low + high) / 2.0;
        if contrast_ratio(mix(ground, tint, amount), ground) < contrast {
            low = amount;
        } else {
            high = amount;
        }
    }
    Some(mix(ground, tint, high))
}

/// WCAG-style contrast between two colours, 1..=21. Unlike [`luminance`],
/// which is a cheap linear weighting good for "is this fill light?", this is
/// gamma-corrected: the form a legibility floor is defined against.
pub fn contrast_ratio(left: Color, right: Color) -> f32 {
    fn channel(value: u8) -> f32 {
        let value = f32::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    fn relative(color: Color) -> f32 {
        let (red, green, blue) = rgb_parts(color);
        0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
    }
    let (lighter, darker) = {
        let (left, right) = (relative(left), relative(right));
        (left.max(right), left.min(right))
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// How much of a picture the theme's text selection can afford to let
/// through before the selected text stops being legible.
///
/// The picture can be any colour, so the budget is what survives both
/// extremes: the largest wash where a fully black and a fully white picture
/// each leave the blended selection at 3:1 or better against
/// `selection_text`. It is computed from the theme alone, so it does not
/// change from frame to frame. One theme can afford three times what
/// another can, so no single fixed number fits every theme. A theme
/// whose colours are the terminal's own (`reset`) cannot be measured and
/// takes a conservative fixed budget instead.
pub fn selection_wash(theme: &Theme) -> f32 {
    const FLOOR: f32 = 3.0;
    const UNMEASURABLE: f32 = 0.2;
    let (Color::Rgb(..), Color::Rgb(..)) = (theme.selection, theme.selection_text) else {
        return UNMEASURABLE;
    };
    let mut budget = 0.0;
    for step in 0..=20 {
        let wash = step as f32 / 20.0;
        let holds = [Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255)]
            .into_iter()
            .all(|extreme| {
                contrast_ratio(mix(theme.selection, extreme, wash), theme.selection_text) >= FLOOR
            });
        if holds {
            budget = wash;
        } else {
            break;
        }
    }
    budget
}

/// Brightness of a colour from 0.0 to 1.0, from the Rec. 709 weights on its
/// red, green and blue channels.
pub fn luminance(color: Color) -> f32 {
    let (red, green, blue) = rgb_parts(color);
    (0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue)) / 255.0
}

/// Themes are hand-written, so every colour is a readable string rather than
/// Ratatui's own `Color` enum shape.
/// The same, for a colour a theme may simply leave out.
fn de_optional_color<'de, D>(deserializer: D) -> Result<Option<Color>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Unexpected};
    let Some(raw) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_color(&raw)
        .map(Some)
        .ok_or_else(|| D::Error::invalid_value(Unexpected::Str(&raw), &"a colour"))
}

/// A colour written the way the theme files write them: `#rrggbb`, `ansiN`,
/// or `reset` - so a saved theme reads back through the same parser.
pub fn write_color(color: Color) -> String {
    match color {
        Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
        Color::Indexed(index) => format!("ansi{index}"),
        Color::Reset => "reset".to_owned(),
        // Every colour a theme can PARSE is one of the above; anything else
        // is best-effort through its RGB.
        other => {
            let (red, green, blue) = rgb_parts(other);
            format!("#{red:02x}{green:02x}{blue:02x}")
        }
    }
}

fn se_color<S>(color: &Color, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&write_color(*color))
}

fn se_optional_color<S>(color: &Option<Color>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match color {
        Some(color) => serializer.serialize_str(&write_color(*color)),
        None => serializer.serialize_none(),
    }
}

fn de_color<'de, D>(deserializer: D) -> Result<Color, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Unexpected};
    let raw = String::deserialize(deserializer)?;
    parse_color(&raw).ok_or_else(|| {
        D::Error::invalid_value(Unexpected::Str(&raw), &"a #rrggbb, CSS or ansiN colour")
    })
}

fn de_color_option<'de, D>(deserializer: D) -> Result<Option<Color>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Unexpected};
    let Some(raw) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    if raw.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    parse_color(&raw).map(Some).ok_or_else(|| {
        D::Error::invalid_value(Unexpected::Str(&raw), &"a #rrggbb, CSS or ansiN colour")
    })
}

/// Parse `{ key: value, key2: "text" }` option text carried by a visual call.
///
/// An option object is a JavaScript literal, not JSON: keys are
/// unquoted and strings may use single quotes. This is a tolerant reader for
/// the flat, scalar subset the terminal renderers understand; anything more
/// complex is ignored rather than refused, because an unreadable option must
/// never stop a visualizer from drawing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisualOptions {
    entries: BTreeMap<String, OptionValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OptionValue {
    Number(f64),
    Bool(bool),
    Text(String),
}

impl VisualOptions {
    pub fn parse(source: &str) -> Self {
        let mut entries = BTreeMap::new();
        let trimmed = source.trim();
        let body = trimmed
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .unwrap_or("");
        for field in split_top_level(body) {
            let Some((key, value)) = field.split_once(':') else {
                continue;
            };
            let key = key.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
            if key.is_empty() {
                continue;
            }
            let value = value.trim();
            let parsed = if let Some(text) = strip_quotes(value) {
                OptionValue::Text(text.to_owned())
            } else if value == "true" {
                OptionValue::Bool(true)
            } else if value == "false" {
                OptionValue::Bool(false)
            } else if let Ok(number) = value.parse::<f64>() {
                OptionValue::Number(number)
            } else {
                continue;
            };
            entries.insert(key, parsed);
        }
        Self { entries }
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        match self.entries.get(key)? {
            OptionValue::Number(value) => Some(*value),
            OptionValue::Bool(value) => Some(f64::from(u8::from(*value))),
            OptionValue::Text(text) => text.parse().ok(),
        }
    }

    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.entries.get(key)? {
            OptionValue::Bool(value) => Some(*value),
            OptionValue::Number(value) => Some(*value != 0.0),
            OptionValue::Text(_) => None,
        }
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        match self.entries.get(key)? {
            OptionValue::Text(value) => Some(value.as_str()),
            _ => None,
        }
    }

    pub fn color(&self, key: &str) -> Option<Color> {
        self.text(key).and_then(parse_color)
    }
}

fn strip_quotes(value: &str) -> Option<&str> {
    for quote in ['"', '\'', '`'] {
        if let Some(inner) = value.strip_prefix(quote)
            && let Some(inner) = inner.strip_suffix(quote)
        {
            return Some(inner);
        }
    }
    None
}

/// Split on commas that are not inside brackets, braces or quotes.
fn split_top_level(body: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    for (index, character) in body.char_indices() {
        match (quote, character) {
            (Some(open), current) if current == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'' | '`') => quote = Some(character),
            (None, '{' | '[' | '(') => depth += 1,
            (None, '}' | ']' | ')') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                fields.push(&body[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if start < body.len() {
        fields.push(&body[start..]);
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;

    /// The one row whose arrow keys do something draws the arrows. It is
    /// also the rainbow row, and the rainbow paints the text the row built,
    /// so the arrows must be part of that text.
    #[test]
    fn the_randomize_row_draws_its_arrows_through_the_rainbow() {
        use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 60, 30);

        let row_text = |picker: &ThemePicker| -> String {
            let mut buffer = Buffer::empty(area);
            ThemePickerView {
                picker,
                theme: &theme,
                camera: None,
            }
            .render(area, &mut buffer);
            (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                        .collect::<String>()
                })
                .find(|line| line.contains(RANDOMIZE_THEME))
                .expect("the randomize row is drawn")
        };

        let mut picker = ThemePicker::open(&theme);
        let at = picker
            .matches()
            .position(|name| name == RANDOMIZE_THEME)
            .expect("randomize is in the list");
        picker.selected = at;
        let before = row_text(&picker);
        assert!(
            before.contains('\u{25b6}'),
            "no forward arrow on the randomize row: {before:?}"
        );
        assert!(
            !before.contains('\u{25c0}'),
            "a back arrow before anything has been rolled: {before:?}"
        );

        picker.roll_randomize(true);
        let after = row_text(&picker);
        assert!(
            after.contains('\u{25c0}') && after.contains('\u{25b6}'),
            "rolling did not grow the back arrow: {after:?}"
        );
    }

    /// A roll is only worth keeping if it can be had again, and only worth
    /// showing if the code on it can be read. `built_in_names` already sweeps
    /// one random roll per run; this sweeps many, deterministically.
    #[test]
    fn every_randomize_roll_is_reproducible_and_readable() {
        let luminance = |color: Color| match color {
            Color::Rgb(red, green, blue) => {
                let channel = |value: u8| {
                    let value = f64::from(value) / 255.0;
                    if value <= 0.040_45 {
                        value / 12.92
                    } else {
                        ((value + 0.055) / 1.055).powf(2.4)
                    }
                };
                0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
            }
            other => panic!("a rolled palette is always RGB, got {other:?}"),
        };
        let contrast = |left: Color, right: Color| {
            let (high, low) = {
                let (a, b) = (luminance(left), luminance(right));
                if a > b { (a, b) } else { (b, a) }
            };
            (high + 0.05) / (low + 0.05)
        };

        let mut visuals = (0usize, 0usize);
        let mut lights = 0usize;
        for step in 0..200_u32 {
            let seed = step.wrapping_mul(2_654_435_761) | 1;
            let theme = randomize_theme(seed);
            assert_eq!(
                theme.name, RANDOMIZE_THEME,
                "a roll keeps the row's identity"
            );
            let written = |theme: &Theme| serde_json::to_string(theme).expect("a theme serialises");
            assert_eq!(
                written(&theme),
                written(&randomize_theme(seed)),
                "seed {seed:08x} must roll the same theme twice"
            );

            // The point of the whole HSL construction: text on ground.
            let ratio = contrast(theme.foreground, theme.background);
            assert!(
                ratio >= 7.0,
                "seed {seed:08x} rolled unreadable code: {ratio:.1}:1"
            );
            for (what, colour) in [
                ("muted", theme.muted),
                ("accent", theme.accent),
                ("error", theme.error),
            ] {
                let ratio = contrast(colour, theme.background);
                assert!(
                    ratio >= 3.0,
                    "seed {seed:08x} rolled {what} at {ratio:.1}:1 on its own ground"
                );
            }
            if luminance(theme.background) > 0.5 {
                lights += 1;
            }
            match theme.visual.as_ref().expect("a roll always has a renderer") {
                ThemeVisual::Hydra {
                    code,
                    camera,
                    opacity,
                } => {
                    visuals.0 += 1;
                    assert!(!camera, "a roll must never ask for the webcam by itself");
                    assert!(
                        opacity.is_some_and(|value| (10..=60).contains(&value)),
                        "seed {seed:08x}: furniture, not the show - {opacity:?}"
                    );
                    assert!(!code.trim().is_empty(), "seed {seed:08x}: an empty sketch");
                    #[cfg(feature = "hydra")]
                    {
                        let node = rustel_hydra::glsl::parse_chain(code)
                            .unwrap_or_else(|error| panic!("seed {seed:08x}: {error}\n{code}"));
                        rustel_hydra::glsl::compose(&node, "highp")
                            .unwrap_or_else(|error| panic!("seed {seed:08x}: {error}\n{code}"));
                    }
                }
                ThemeVisual::TachyonFx { density, speed, .. } => {
                    visuals.1 += 1;
                    assert!((1..=100).contains(density) && (1..=100).contains(speed));
                }
            }
        }
        // When both renderers are available, neither may win every roll.
        #[cfg(feature = "hydra")]
        assert!(visuals.0 > 20 && visuals.1 > 20, "{visuals:?}");
        #[cfg(not(feature = "hydra"))]
        assert_eq!(visuals, (0, 200));
        assert!(
            (10..190).contains(&lights),
            "light and dark rolls must both happen: {lights} light of 200"
        );

        // Walking the rolls has to be walking, not scattering: → then ←
        // lands where it started, which is what makes a roll recoverable
        // after one press too many.
        let mut picker = ThemePicker {
            names: vec![RANDOMIZE_THEME.to_owned()],
            query: String::new(),
            selected: 0,
            original: Box::new(Theme::built_in_default()),
            deleting: None,
            randomize_seed: 12_345 | 1,
            rolled: false,
            scroll: std::cell::Cell::new(0),
            hold_scroll: false,
        };
        assert!(picker.on_randomize());
        let start = picker.randomize_seed;
        let forwards = picker.roll_randomize(true);
        assert_ne!(forwards, start, "a roll must actually change something");
        assert_eq!(
            picker.roll_randomize(false),
            start,
            "← walks back to the roll"
        );
    }

    /// The camera theme that the studio paints itself declares a camera
    /// without a Hydra sketch, so it reads the same in a build with no Hydra.
    #[test]
    fn the_braille_camera_theme_declares_a_camera_without_a_hydra_chain() {
        let theme = Theme::built_in("webcam-braille").expect("the theme is compiled in");
        assert!(theme.native_camera_enabled());
        assert!(theme.camera_enabled());
        assert!(!theme.hydra_camera_enabled(), "no sketch samples s0 here");
        assert_eq!(theme.hydra_code(), None);
        assert_eq!(
            theme.cell_visual().map(|cell| cell.effect),
            Some(CellEffect::Camera)
        );
        assert_eq!(
            theme.cell_visual().map(|cell| cell.mode),
            Some(TachyonMode::Text),
            "image mode is one colour a cell and would throw the dots away"
        );
        // And the Hydra camera themes still say what they said.
        let hydra = Theme::built_in("webcam-clean").expect("webcam-clean");
        assert!(hydra.hydra_camera_enabled());
        assert!(hydra.camera_enabled());
        assert!(!hydra.native_camera_enabled());
        assert!(!Theme::built_in_default().camera_enabled());
    }

    #[test]
    fn every_built_in_theme_parses() {
        for name in Theme::built_in_names() {
            let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is missing"));
            assert_eq!(theme.name, name);
        }
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn camera_themes_are_original_visible_s0_chains_and_invisible_capture_is_refused() {
        for name in ["webcam-clean", "webcam-distort", "webcam-hacker"] {
            let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is missing"));
            assert!(
                theme.hydra_camera_enabled(),
                "{name} declares camera acquisition"
            );
            let node = rustel_hydra::glsl::parse_chain(theme.hydra_code().unwrap()).unwrap();
            assert!(hydra_node_uses_s0(&node), "{name} visibly consumes s0");
            rustel_hydra::glsl::compose(&node, "highp").unwrap();
        }

        let mut value = serde_json::to_value(Theme::built_in_default()).unwrap();
        value["hydra_camera"] = serde_json::Value::Bool(true);
        for invisible in [
            "osc(3).out()",
            "osc(3).out(s0)",
            "osc(3).out(src(s0))",
            "osc(src(s0), 0.1, 0).out()",
            "src(src(s0)).out()",
            "src(s0).blend(1).out()",
            "src(s0).blend(src(1)).out()",
            "src(s0).sum().out()",
            "osc(3, 0.1, 0, s0).out()",
            "osc(3).color(1, 1, 1, 1, s0).out()",
            "render(s0)",
            "render(src(s0))",
        ] {
            value["hydra"] = serde_json::Value::String(invisible.into());
            let error = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap_err();
            assert!(
                error.to_string().contains("visibly samples s0"),
                "{invisible}: {error}"
            );
        }

        value["hydra"] = serde_json::Value::Null;
        let error = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("visibly samples s0"), "{error}");
    }

    #[test]
    fn theme_documents_are_bounded_before_json_allocation_grows_without_limit() {
        let oversized = " ".repeat(MAX_THEME_DOCUMENT_BYTES + 1);
        let error = Theme::from_json(&oversized).unwrap_err();
        assert!(error.to_string().contains("maximum"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn named_theme_discovery_does_not_follow_json_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real.json");
        let linked = directory.path().join("linked.json");
        std::fs::write(&real, include_str!("themes/synthwave.json")).unwrap();
        symlink(&real, &linked).unwrap();

        assert!(is_real_theme_file(&real));
        assert!(!is_real_theme_file(&linked));
    }

    #[test]
    fn the_default_theme_tints_events_and_preserves_syntax() {
        let theme = Theme::built_in_default();
        assert_eq!(theme.event_mark, MarkStyle::Tint);
        assert_eq!(theme.mini_fill, None);
    }

    #[test]
    fn the_matrix_theme_uses_the_native_cell_renderer() {
        let theme = Theme::built_in("matrix").expect("matrix is compiled in");
        assert_eq!(
            theme.cell_visual(),
            Some(CellVisual {
                effect: CellEffect::Matrix,
                mode: TachyonMode::Text,
                density: 76,
                speed: 58,
            })
        );
        assert!(
            theme.hydra_code().is_none(),
            "Matrix is no longer fake Hydra"
        );
    }

    /// Every built-in says how long its sounding marks linger, and each
    /// says something its own: a sharp theme cuts, a dusk theme lets go.
    #[test]
    fn every_built_in_theme_names_its_own_highlight_fade() {
        for name in Theme::built_in_names() {
            let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is built in"));
            let named = theme
                .event_fade
                .unwrap_or_else(|| panic!("{name} names no event_fade"));
            assert!(
                (0.05..=1.0).contains(&named),
                "{name} fades over {named}s, which is not a fade"
            );
            assert_eq!(theme.event_fade(), named);
        }
        // The sharp end and the slow end are both represented, so the key
        // is doing work rather than being copied everywhere.
        let fade = |name: &str| Theme::built_in(name).and_then(|theme| theme.event_fade);
        assert!(fade("powershell") < fade("rustel-dark"));
        assert!(fade("rustel-dark") < fade("campfire"));
        assert!(fade("campfire") < fade("lsd"));
        // A theme that names none still has the studio's own.
        let mut plain = Theme::built_in_default();
        plain.event_fade = None;
        assert_eq!(plain.event_fade(), 0.3);
    }

    #[test]
    fn the_picker_exposes_only_the_requested_native_effect_themes() {
        let mut names = Theme::built_in_names()
            // `randomize` is a different theme on every resolution, so whether
            // it carries a cell effect is a coin toss rather than a fact
            // about the vocabulary. Counting it would make this test pass
            // roughly three runs in five.
            .filter(|name| *name != RANDOMIZE_THEME)
            .filter(|name| {
                Theme::built_in(name)
                    .and_then(|theme| theme.cell_visual())
                    .is_some()
            })
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();

        assert_eq!(SHOWROOM_EFFECTS.len(), 42);
        assert_eq!(names.len(), 46);
        assert!(names.contains(&"matrix"));
        assert!(names.contains(&"mode7"));
        assert!(names.contains(&"dvd-bounce"));
        // `randomize` is the roll now, so the cell effect that used to wear
        // that name has one of its own.
        assert!(names.contains(&"scramble"));
        assert!(names.contains(&"vectrex"));
        assert!(names.contains(&"rustel-live"));
        assert!(names.contains(&"snow"));
        assert!(names.contains(&"space"));
        assert!(names.contains(&"campfire"));
        assert!(names.contains(&"hell"));
        assert!(names.contains(&"basketball"));
        assert!(names.contains(&"waves-reactive"));
        assert_eq!(
            Theme::built_in("burn").and_then(|theme| theme.tachyon_mode()),
            Some(TachyonMode::Image),
            "burn demonstrates the stretched backdrop mode"
        );
        assert_eq!(
            Theme::built_in("bubbles").and_then(|theme| theme.tachyon_mode()),
            Some(TachyonMode::Text),
            "character bubbles keep the direct text mode"
        );
        for (name, effect) in SHOWROOM_EFFECTS {
            let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is missing"));
            assert_eq!(
                theme.cell_visual().map(|visual| visual.effect),
                Some(*effect),
                "{name} selects the wrong effect"
            );
        }

        let picker_names = Theme::built_in_names().collect::<Vec<_>>();
        for removed in [
            "slide",
            "randomsequence",
            "bubblegum",
            "gold",
            "laseretch",
            "middleout",
            "snes",
            "prism-current",
        ] {
            assert!(
                !picker_names.contains(&removed),
                "{removed} remains in the picker"
            );
        }
        assert!(picker_names.contains(&"prism"));
        assert_eq!(
            Theme::built_in("prism-current").map(|theme| theme.name),
            Some("prism".into()),
            "the previous name remains a compatibility alias"
        );
        assert_eq!(
            Theme::built_in("retro-3d").map(|theme| theme.name),
            Some("vectrex".into()),
            "the vector tunnel's old name still finds it"
        );
        assert_eq!(
            Theme::built_in("spray").map(|theme| theme.name),
            Some("snow".into()),
            "the old spray name remains a compatibility alias"
        );
        assert_eq!(
            Theme::built_in("randomsequence").map(|theme| theme.name),
            Some("scramble".into()),
            "the cell effect's old name still resolves to it"
        );
        assert_eq!(
            Theme::built_in("randomize").map(|theme| theme.name),
            Some("randomize".into()),
            "`randomize` is the roll, not the cell effect"
        );
        assert_eq!(
            Theme::built_in("mode7").and_then(|theme| theme.cell_visual()),
            Some(CellVisual {
                effect: CellEffect::Mode7,
                mode: TachyonMode::Text,
                density: 60,
                speed: 65,
            })
        );
        assert_eq!(
            Theme::built_in("vectrex").and_then(|theme| theme.cell_visual()),
            Some(CellVisual {
                effect: CellEffect::Vectrex,
                mode: TachyonMode::Text,
                density: 58,
                speed: 54,
            })
        );
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn the_prism_theme_matches_its_kaleidoscope_example_and_composes() {
        let theme = Theme::built_in("prism").expect("prism is compiled in");
        let sketch = theme
            .hydra_code()
            .expect("prism carries its feedback current");
        let shelf_sketch = crate::examples::SECTIONS
            [crate::examples::section_of(crate::examples::Kind::Hydra).unwrap()]
        .shelves
        .iter()
        .find(|category| category.name == "Kaleidoscope")
        .and_then(|category| {
            category
                .snippets
                .iter()
                .find(|snippet| snippet.name == "Prism Current")
        })
        .expect("Prism Current is on the kaleidoscope shelf")
        .code;
        assert_eq!(
            sketch, shelf_sketch,
            "the theme and example use the same Hydra chain"
        );
        let node = rustel_hydra::glsl::parse_chain(sketch).expect("prism parses");
        rustel_hydra::glsl::compose_full(&node, "highp").expect("prism composes");
    }

    /// Under a camera theme the picker says what stands between the reader
    /// and the picture, in place of the browsing hint.
    #[test]
    fn the_picker_says_what_a_camera_theme_needs() {
        let theme = Theme::built_in_default();
        let picker = ThemePicker::open(&theme);
        let area = Rect::new(0, 0, 80, 24);
        let footer = |camera: Option<CameraNote>| {
            let mut buffer = Buffer::empty(area);
            ThemePickerView {
                picker: &picker,
                theme: &theme,
                camera,
            }
            .render(area, &mut buffer);
            let (panel, list) = picker.geometry(area).expect("room");
            (0..2)
                .map(|row| {
                    (list.x..list.right())
                        .map(|x| {
                            buffer
                                .cell((x, panel.bottom() - 3 + row))
                                .map_or(" ", |cell| cell.symbol())
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(footer(None).contains("Enter keep"));
        assert!(footer(Some(CameraNote::Off)).contains("Hydra webcam"));
        assert!(footer(Some(CameraNote::Starting)).contains("webcam starting"));
        assert!(footer(Some(CameraNote::Live)).contains("webcam live"));
        assert!(footer(Some(CameraNote::Failed)).contains("could not open"));
    }

    #[test]
    fn the_theme_picker_title_tracks_the_selection_and_total() {
        let theme = Theme::built_in_default();
        let mut picker = ThemePicker {
            names: (1..=30).map(|index| format!("theme-{index}")).collect(),
            query: String::new(),
            selected: 3,
            original: Box::new(theme.clone()),
            deleting: None,
            randomize_seed: 1,
            rolled: false,
            scroll: std::cell::Cell::new(0),
            hold_scroll: false,
        };
        let area = Rect::new(0, 0, 80, 30);
        let render = |picker: &ThemePicker| {
            let mut buffer = Buffer::empty(area);
            ThemePickerView {
                picker,
                theme: &theme,
                camera: None,
            }
            .render(area, &mut buffer);
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };

        let fourth = render(&picker);
        assert!(fourth.contains(" 4/30 "), "{fourth}");

        picker.move_by(1);
        let fifth = render(&picker);
        assert!(fifth.contains(" 5/30 "), "{fifth}");
    }

    #[test]
    fn typing_in_the_theme_picker_filters_names_and_resets_navigation() {
        let theme = Theme::built_in_default();
        let mut picker = ThemePicker {
            names: vec![
                "matrix".into(),
                "rustel-dark".into(),
                "rustel-live".into(),
                "snow".into(),
            ],
            query: String::new(),
            selected: 3,
            original: Box::new(theme.clone()),
            deleting: None,
            randomize_seed: 1,
            rolled: false,
            scroll: std::cell::Cell::new(0),
            hold_scroll: false,
        };
        for character in "RUSTEL".chars() {
            picker.push_query(character);
        }
        assert_eq!(picker.query, "rustel");
        assert_eq!(picker.match_count(), 2);
        assert_eq!(picker.selected_name(), Some("rustel-dark"));
        picker.move_by(1);
        assert_eq!(picker.selected_name(), Some("rustel-live"));
        picker.pop_query();
        assert_eq!(picker.query, "ruste");
        assert_eq!(picker.selected, 0);

        let area = Rect::new(0, 0, 80, 30);
        let mut buffer = Buffer::empty(area);
        ThemePickerView {
            picker: &picker,
            theme: &theme,
            camera: None,
        }
        .render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("search: ruste_"), "{text}");
        assert!(text.contains(" 1/2 "), "{text}");
    }

    #[test]
    fn underline_marking_preserves_syntax_colours_and_never_paints_a_background() {
        let base = Style::default().fg(Color::Rgb(120, 220, 150));
        let underlined =
            MarkStyle::Underline.apply(base, Color::Rgb(255, 202, 40), Color::Rgb(18, 20, 25));
        assert_eq!(underlined.fg, base.fg);
        assert_eq!(underlined.bg, None);
        assert_eq!(underlined.underline_color, Some(Color::Rgb(255, 202, 40)));
        assert!(underlined.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!underlined.add_modifier.contains(Modifier::BOLD));

        let style = MarkStyle::Outline.apply(
            Style::default(),
            Color::Rgb(255, 202, 40),
            Color::Rgb(18, 20, 25),
        );
        assert_eq!(style.bg, None);
        assert_eq!(style.fg, Some(Color::Rgb(255, 202, 40)));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));

        let filled = MarkStyle::Fill.apply(
            Style::default(),
            Color::Rgb(255, 202, 40),
            Color::Rgb(18, 20, 25),
        );
        assert_eq!(filled.bg, Some(Color::Rgb(255, 202, 40)));
    }

    #[test]
    fn every_bundled_theme_tints_sounding_syntax_without_losing_contrast() {
        for (name, document) in BUILT_INS.iter().chain(SHOWROOM_THEMES) {
            let theme = Theme::from_json(document).unwrap();
            assert_eq!(theme.event_mark, MarkStyle::Tint, "{name}");
            let syntax_colors = [
                theme.syntax.text,
                theme.syntax.comment,
                theme.syntax.string,
                theme.syntax.number,
                theme.syntax.punctuation,
                theme.syntax.keyword.unwrap_or(theme.syntax.punctuation),
                theme.syntax.function.unwrap_or(theme.accent),
            ];
            for event_color in [theme.event, Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0)] {
                for syntax_color in syntax_colors {
                    let base = Style::default().fg(syntax_color);
                    let marked = theme.event_mark.apply(base, event_color, theme.background);
                    assert!(marked.add_modifier.contains(Modifier::BOLD), "{name}");
                    assert!(
                        !marked.add_modifier.contains(Modifier::UNDERLINED),
                        "{name}"
                    );
                    if true_rgb(theme.background).is_some() && true_rgb(event_color).is_some() {
                        let tinted = marked.bg.expect("RGB themes have a tint");
                        let light = contrast_ratio(Color::Black, theme.background)
                            > contrast_ratio(Color::White, theme.background);
                        let marked_fg = marked.fg.expect("syntax foreground remains set");
                        let floor = if light {
                            assert!(
                                contrast_ratio(tinted, theme.background) >= 1.28,
                                "{name}: tint is invisible on a light background"
                            );
                            4.5
                        } else {
                            assert_eq!(marked.fg, base.fg, "{name}: syntax color");
                            contrast_ratio(syntax_color, theme.background).min(4.5)
                        };
                        assert!(
                            contrast_ratio(marked_fg, tinted) + 0.02 >= floor,
                            "{name}: {marked_fg:?} loses contrast on {tinted:?}"
                        );
                        if tinted != theme.background {
                            assert_ne!(tinted, event_color, "{name}: solid event fill");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn colors_accept_hex_short_hex_names_and_ansi_indices() {
        assert_eq!(parse_color("#ffca28"), Some(Color::Rgb(255, 202, 40)));
        assert_eq!(parse_color("#fc0"), Some(Color::Rgb(255, 204, 0)));
        assert_eq!(parse_color("Salmon"), Some(Color::Rgb(250, 128, 114)));
        assert_eq!(parse_color("ansi5"), Some(Color::Indexed(5)));
        assert_eq!(parse_color("rgb(1,2,3)"), None);
    }

    /// A score controls the colour string and the studio parses it every
    /// frame, so multi-byte characters that land in a hex branch by byte
    /// count must come back `None`, never panic on a slice splitting a
    /// character.
    #[test]
    fn colors_with_multi_byte_characters_are_refused_without_panicking() {
        // 3 bytes, but not three ASCII digits.
        assert_eq!(parse_color("#éa"), None);
        assert_eq!(parse_color("#aé"), None);
        // 6 bytes, but not six ASCII digits.
        assert_eq!(parse_color("#a€bc"), None);
        assert_eq!(parse_color("#ab€c"), None);
        // `from_str_radix` takes a leading `+`; a colour does not.
        assert_eq!(parse_color("#+f+f+f"), None);
        assert_eq!(parse_color("#+ffabc"), None);
        // Valid hex parses exactly as before.
        assert_eq!(parse_color("#abc"), Some(Color::Rgb(170, 187, 204)));
        assert_eq!(parse_color("#aabbcc"), Some(Color::Rgb(170, 187, 204)));
    }

    /// A theme that carries a sketch must carry one that runs: a chain that
    /// fails to parse or compose draws a warning line where the furniture
    /// should be, in a theme the reader chose for its look.
    #[cfg(feature = "hydra")]
    #[test]
    fn every_theme_owned_sketch_parses_and_composes() {
        let mut sketches = 0;
        for name in Theme::built_in_names() {
            let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is missing"));
            let Some(sketch) = theme.hydra_code() else {
                continue;
            };
            sketches += 1;
            let node = rustel_hydra::glsl::parse_chain(sketch)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            rustel_hydra::glsl::compose(&node, "highp")
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        assert!(sketches >= 2, "the themes that own a sketch went missing");

        // Drift resolves all opacity controls before it reaches the picker.
        let drift = Theme::built_in("rustel-drift").expect("drift is compiled in");
        assert!(drift.hydra_opacity_percent().is_some());
        assert!(drift.opacity.interface.is_some());
    }

    /// A theme survives the round trip through its own file format: every
    /// built-in serializes, reparses through the same strict parser, and
    /// comes back saying the same colours. This is what makes save-as and
    /// the theme editor's code tab honest.
    #[test]
    fn every_built_in_round_trips_through_its_own_file_format() {
        for name in Theme::available_names() {
            let Some(theme) = Theme::built_in(&name) else {
                continue;
            };
            let json = theme.to_json();
            let back: Theme = serde_json::from_str(&json)
                .unwrap_or_else(|error| panic!("{name} reparses: {error}\n{json}"));
            assert_eq!(write_color(back.background), write_color(theme.background));
            assert_eq!(write_color(back.selection), write_color(theme.selection));
            assert_eq!(back.visual, theme.visual, "{name}");
            assert_eq!(back.hydra, theme.hydra, "{name}");
            assert_eq!(back.hydra_camera, theme.hydra_camera, "{name}");
            assert_eq!(back.hydra_opacity, theme.hydra_opacity, "{name}");
            assert_eq!(back.event_mark, theme.event_mark, "{name}");
        }
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn explicit_hydra_renderer_and_legacy_hydra_are_both_supported() {
        let mut value = serde_json::to_value(Theme::built_in_default()).unwrap();
        value["visual"] = serde_json::json!({
            "renderer": "hydra",
            "code": "osc(4, 0.1, 0.8).out()",
            "opacity": 31
        });
        let theme = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(theme.hydra_code(), Some("osc(4, 0.1, 0.8).out()"));
        assert_eq!(theme.hydra_opacity_percent(), Some(31));

        value["hydra"] = serde_json::Value::String("osc(2).out()".into());
        let error = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("cannot be combined"), "{error}");
    }

    #[test]
    fn tachyonfx_modes_are_per_theme_strict_and_backward_compatible() {
        let mut value = serde_json::to_value(Theme::built_in_default()).unwrap();
        value["visual"] = serde_json::json!({
            "renderer": "cells",
            "effect": "matrix",
            "density": 0,
            "speed": 55
        });
        let error = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("between 1 and 100"), "{error}");

        value["visual"]["density"] = serde_json::json!(70);
        let legacy = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(legacy.tachyon_mode(), Some(TachyonMode::Text));

        value["visual"]["renderer"] = serde_json::json!("tachyonfx");
        value["visual"]["mode"] = serde_json::json!("image");
        let image = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(image.tachyon_mode(), Some(TachyonMode::Image));

        value["visual"]["extra"] = serde_json::json!(true);
        let error = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn character_effects_are_independent_strict_and_shipped_in_three_styles() {
        assert_eq!(
            Theme::built_in("lavender").and_then(|theme| theme.character_visual()),
            Some(CharacterVisual {
                effect: CharacterEffect::Fade,
                strength: 62,
                speed: 38,
            })
        );
        assert_eq!(
            Theme::built_in("synthwave")
                .and_then(|theme| theme.character_visual())
                .map(|visual| visual.effect),
            Some(CharacterEffect::Aurora),
            "a character effect can coexist with Hydra"
        );
        assert_eq!(
            Theme::built_in("synthwave2").map(|theme| theme.name),
            Some("synthwave".into()),
            "the old second name still resolves"
        );

        let mut value = serde_json::to_value(Theme::built_in_default()).unwrap();
        value["characters"] = serde_json::json!({ "effect": "fade" });
        let defaults = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(
            defaults.character_visual(),
            Some(CharacterVisual {
                effect: CharacterEffect::Fade,
                strength: 60,
                speed: 50,
            })
        );

        value["characters"]["strength"] = serde_json::json!(0);
        let error = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("between 1 and 100"), "{error}");
        value["characters"]["strength"] = serde_json::json!(60);
        value["characters"]["script"] = serde_json::json!("nope");
        let error = Theme::from_json(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    /// A theme name is a file stem, never a path.
    /// Built-ins are a fixed vocabulary: a save can never take their name,
    /// so nothing on disk ever shadows what the executable ships.
    #[test]
    fn a_built_ins_name_cannot_be_saved_over() {
        let theme = Theme::built_in_default();
        let error = save_named(&theme, "rustel-dark").expect_err("refused");
        assert!(error.to_string().contains("built in"), "{error}");
    }

    /// A palette colour cannot be blended, so a mix that touches one snaps
    /// to the side with more weight. It does not invent a mid-grey.
    #[test]
    fn a_mix_with_the_terminals_palette_snaps_instead_of_inventing_grey() {
        let tint = mix(Color::Reset, Color::Rgb(255, 0, 0), 0.25);
        assert_eq!(tint, Color::Reset, "a light tint quietly disappears");
        let heavy = mix(Color::Indexed(8), Color::Indexed(3), 0.8);
        assert_eq!(heavy, Color::Indexed(3), "a heavy blend takes the target");
        // Real RGB still blends, exactly as before.
        assert_eq!(
            mix(Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255), 0.5),
            Color::Rgb(128, 128, 128)
        );
    }

    #[test]
    fn a_theme_name_cannot_walk_out_of_the_directory() {
        assert!(valid_theme_name("night-drive").is_ok());
        assert!(valid_theme_name("  ").is_err());
        assert!(valid_theme_name("../escape").is_err());
        assert!(valid_theme_name("a/b").is_err());
        assert!(valid_theme_name("a.json").is_err());
        // ':' is a Windows drive prefix ("C:name" replaces the base in
        // Path::join) or an NTFS stream. Either one leaves the directory.
        assert!(valid_theme_name("C:escape").is_err());
        assert!(valid_theme_name("streams:hidden").is_err());
    }

    #[test]
    fn a_typo_in_a_theme_file_is_refused_not_ignored() {
        let document = r##"{"backgruond": "#000000"}"##;
        assert!(matches!(
            Theme::from_json(document),
            Err(ThemeError::Parse(_))
        ));
    }

    #[test]
    fn unknown_theme_names_list_what_is_available() {
        let error = Theme::resolve(Some("does-not-exist")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("rustel-dark"), "{message}");
    }

    #[test]
    fn javascript_option_objects_read_as_flat_scalars() {
        let options = VisualOptions::parse("{ cycles: 8, labels: true, active: '#ff0000' }");
        assert_eq!(options.number("cycles"), Some(8.0));
        assert_eq!(options.flag("labels"), Some(true));
        assert_eq!(options.color("active"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(options.number("missing"), None);
    }

    #[test]
    fn nested_option_values_do_not_split_the_field_list() {
        let options = VisualOptions::parse("{ pos: [0, 1], min: -80, label: 'a, b' }");
        assert_eq!(options.number("min"), Some(-80.0));
        assert_eq!(options.text("label"), Some("a, b"));
        assert_eq!(options.number("pos"), None);
    }
}

#[cfg(test)]
mod confinement_tests {
    use super::*;

    #[test]
    fn a_relative_theme_file_beside_the_score_still_loads() {
        // docs/studio.md documents `--theme ./midnight.json` - a theme file
        // in the working directory, not in the theme directory.
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("midnight.json");
        // Any real theme file will do; copy a built-in's JSON.
        let source = include_str!("themes/synthwave.json");
        std::fs::write(&path, source).expect("write theme");
        let selector = format!("{}", path.display());
        Theme::resolve(Some(&selector)).expect("an explicit theme file must load");
    }

    #[test]
    fn a_remembered_selector_cannot_name_a_file_outside_the_theme_directory() {
        // studio.json is synced and hand-edited. A path in it is held to the
        // theme directory so it cannot turn into an arbitrary read.
        let directory = tempfile::tempdir().expect("tempdir");
        let themes = directory.path().join("themes");
        std::fs::create_dir_all(&themes).expect("theme directory");
        let outside = directory.path().join("outside.json");
        std::fs::write(&outside, include_str!("themes/synthwave.json")).expect("write");

        // SAFETY: single-threaded test; the variable is read on this thread.
        unsafe { std::env::set_var("RUSTEL_THEME_DIR", &themes) };
        let escape = format!("{}", outside.display());
        let refused = Theme::resolve_remembered(&escape);
        let traversal = Theme::resolve_remembered("../outside.json");
        unsafe { std::env::remove_var("RUSTEL_THEME_DIR") };

        assert!(refused.is_err(), "an absolute escape must be refused");
        assert!(traversal.is_err(), "a `..` escape must be refused");
    }

    /// Walking the picker keeps names in sight past the selection, and a
    /// click selects without scrolling until the next arrow.
    #[test]
    fn walking_the_picker_keeps_names_in_sight_past_the_selection() {
        let mut picker = ThemePicker {
            names: (1..=30).map(|index| format!("theme-{index}")).collect(),
            query: String::new(),
            selected: 0,
            original: Box::new(Theme::built_in_default()),
            deleting: None,
            randomize_seed: 1,
            rolled: false,
            scroll: std::cell::Cell::new(0),
            hold_scroll: false,
        };
        let shown = 12;
        let margin = crate::scroll::margin(shown);
        for _ in 0..20 {
            picker.move_by(1);
            let first = picker.first_row(shown);
            assert!(
                first + shown >= (picker.selected + margin + 1).min(30),
                "{} keeps {margin} names below it from {first}",
                picker.selected
            );
        }
        let first = picker.first_row(shown);
        picker.selected = first + shown - 1;
        picker.hold_scroll = true;
        assert_eq!(picker.first_row(shown), first, "a click holds the list");
        picker.move_by(-1);
        picker.move_by(1);
        assert!(
            picker.first_row(shown) > first,
            "the arrows bring the margin back"
        );
    }

    /// `MarkStyle::apply` is the ground truth for what a sounding mark
    /// actually draws; the contrast audit below reads a different pair of
    /// colours per style, and this pins down that it read the right one.
    #[test]
    fn mark_style_apply_pairs_the_colours_the_contrast_audit_assumes() {
        let syntax = Style::default().fg(Color::Rgb(10, 200, 10));
        let event = Color::Rgb(220, 40, 40);
        let surface = Color::Rgb(20, 20, 20);

        // Outline and Text leave whatever ground was already there alone -
        // no `bg` is set - and only recolour the foreground to the event
        // colour. The audit reads that as (event, background).
        for style in [MarkStyle::Outline, MarkStyle::Text] {
            let applied = style.apply(syntax, event, surface);
            assert_eq!(applied.fg, Some(event), "{style:?} recolours the text");
            assert_eq!(applied.bg, None, "{style:?} must not paint a fill");
        }

        let underlined = MarkStyle::Underline.apply(syntax, event, surface);
        assert_eq!(underlined.fg, syntax.fg, "underline keeps syntax colours");
        assert_eq!(underlined.bg, None, "underline leaves the ground alone");
        assert_eq!(underlined.underline_color, Some(event));

        // Fill paints the event colour as the background and uses `surface`
        // as the foreground. The audit reads that as (surface, event).
        let filled = MarkStyle::Fill.apply(syntax, event, surface);
        assert_eq!(filled.bg, Some(event), "fill paints the event colour");
        assert_eq!(
            filled.fg,
            Some(surface),
            "fill reads surface over its own fill"
        );

        // Invert swaps fg and bg at draw time, so the style's foreground is
        // still `event`. Contrast is symmetric, so the audit reads it the
        // same way as Outline and Text.
        let inverted = MarkStyle::Invert.apply(syntax, event, surface);
        assert_eq!(inverted.fg, Some(event));
        assert!(inverted.add_modifier.contains(Modifier::REVERSED));
    }

    /// Body text is read letter by letter: the source itself, a sounding
    /// mark's own text, and a selection's text. It is held to WCAG's AA
    /// floor for normal text.
    const AUDIT_BODY_TEXT_FLOOR: f32 = 4.5;
    /// `accent`, `warn` and `error` mark short labels, single glyphs and
    /// underlines rather than paragraphs of text, so they are held to
    /// WCAG 1.4.11's floor for large text and non-text UI components
    /// instead of the body-text one above.
    const AUDIT_UI_FLOOR: f32 = 3.0;
    /// `muted` is the quietest colour in a theme (comments, line numbers,
    /// inactive labels), so it is exempt from the body-text floor. Below
    /// about 2:1 a colour no longer reads as text at all.
    const AUDIT_MUTED_FLOOR: f32 = 2.0;

    #[test]
    fn automatic_bracket_fills_are_quiet_and_readable_across_palettes() {
        let themes = Theme::built_in_names()
            .filter(|&name| name != RANDOMIZE_THEME)
            .map(|name| Theme::built_in(name).unwrap())
            .chain((0..200).map(randomize_theme));
        for theme in themes {
            assert_eq!(
                theme.bracket, None,
                "{} must use automatic colours",
                theme.name
            );
            assert_eq!(theme.bracket_mark, BracketMark::Auto);
            for ground in [Some(theme.background), theme.current_line, theme.mini_fill]
                .into_iter()
                .flatten()
            {
                for matched in [true, false] {
                    let style = theme.bracket_style(
                        Style::default().fg(theme.syntax.punctuation).bg(ground),
                        CaretShape::SteadyUnderline,
                        matched,
                    );
                    if true_rgb(ground).is_none() {
                        continue;
                    }
                    let fill = style.bg.unwrap();
                    let contrast = contrast_ratio(fill, ground);
                    assert!(
                        (1.4..1.55).contains(&contrast),
                        "{}: fill {contrast}",
                        theme.name
                    );
                    assert!(
                        contrast_ratio(style.fg.unwrap(), fill) >= 4.5,
                        "{}: text must remain readable",
                        theme.name
                    );
                }
            }
        }
    }

    #[test]
    fn matched_brackets_never_borrow_error_red_from_the_palette() {
        let mut theme = Theme::built_in_default();
        theme.background = Color::Rgb(35, 10, 15);
        theme.accent = Color::Rgb(240, 60, 80);
        theme.foreground = Color::Rgb(250, 200, 210);
        for custom in [
            None,
            Some(Color::Red),
            Some(Color::LightRed),
            Some(Color::Indexed(196)),
        ] {
            theme.bracket = custom;
            for caret in [
                CaretShape::SteadyBar,
                CaretShape::SteadyBlock,
                CaretShape::SteadyUnderline,
                CaretShape::BlinkingBar,
                CaretShape::BlinkingBlock,
                CaretShape::BlinkingUnderline,
            ] {
                let style = theme.bracket_style(Style::default().fg(Color::Red), caret, true);
                for color in [style.fg, style.bg, style.underline_color]
                    .into_iter()
                    .flatten()
                {
                    assert_eq!(
                        color,
                        bracket_colour(color),
                        "red matched bracket: {color:?}"
                    );
                }
            }
        }
        let unmatched = theme.bracket_style(Style::default(), CaretShape::SteadyBar, false);
        assert_eq!(unmatched.underline_color, Some(theme.error));
    }

    #[test]
    fn caret_locator_is_quieter_than_bracket_fills_across_palettes() {
        let themes = Theme::built_in_names()
            .filter(|&name| name != RANDOMIZE_THEME)
            .map(|name| Theme::built_in(name).unwrap())
            .chain((0..200).map(randomize_theme));
        for theme in themes {
            if let Some(fill) = theme.caret_line_fill() {
                let contrast = contrast_ratio(fill, theme.background);
                assert!((1.1..1.2).contains(&contrast), "{}: {contrast}", theme.name);
            }
        }
    }

    #[test]
    fn bracket_auto_uses_theme_caret_overrides_and_handles_terminal_colours() {
        let mut theme = Theme::built_in(DEFAULT_THEME).unwrap();
        theme.caret_shape = Some(CaretShape::BlinkingBlock);
        let style = theme.bracket_style(Style::default(), CaretShape::SteadyUnderline, true);
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        theme.caret_shape = Some(CaretShape::BlinkingUnderline);
        let style = theme.bracket_style(Style::default(), CaretShape::SteadyBar, true);
        assert!(style.bg.is_some());
        assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
        theme.background = Color::Reset;
        let style = theme.bracket_style(Style::default(), CaretShape::SteadyBar, true);
        assert_eq!(style.bg, None);
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        theme.bracket = Some(Color::Rgb(80, 100, 120));
        let style = theme.bracket_style(Style::default(), CaretShape::SteadyBar, true);
        assert_eq!(
            style.bg, theme.bracket,
            "an explicit custom override is still supported"
        );
    }

    /// Every built-in theme's colours, checked against WCAG's relative
    /// luminance and contrast ratio (via [`contrast_ratio`], the same
    /// function `readable` uses to place a randomized palette) for the
    /// pairs that are actually drawn one over the other while using the
    /// studio.
    ///
    /// `randomize` is excluded: it rolls a fresh palette from the clock on
    /// every call, and its own readability is already the subject of
    /// `every_randomize_roll_is_reproducible_and_readable`, sampled over
    /// 200 fixed seeds rather than whatever the wall clock hands this run.
    #[test]
    fn every_built_in_theme_keeps_its_colours_legible_against_what_they_sit_on() {
        // `reset` and `ansiN` defer to the terminal palette and cannot be
        // measured here. Like `selection_wash`, the audit does not fail them.
        let measurable = |color: Color| true_rgb(color).is_some();

        let mut failures = Vec::new();
        for name in Theme::built_in_names().filter(|&name| name != RANDOMIZE_THEME) {
            let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is missing"));
            assert!(
                theme.caret.is_some(),
                "{name}: every bundled theme must choose its caret deliberately"
            );
            assert_eq!(
                theme.caret_shape, None,
                "{name}: bundled themes inherit the user's caret shape"
            );
            let mut check = |what: &str, fg: Color, bg: Color, floor: f32| {
                if !measurable(fg) || !measurable(bg) {
                    return;
                }
                let ratio = contrast_ratio(fg, bg);
                if ratio < floor {
                    failures.push(format!("{name}: {what} is {ratio:.2}:1, needs {floor}:1"));
                }
            };

            // Solarized Light's body text is base00 on base3, the published
            // palette, at about 4.1:1: under WCAG AA by design. Only its
            // body text is exempt; its other colours meet the floors below.
            if name != "solarized-light" {
                check(
                    "foreground/background",
                    theme.foreground,
                    theme.background,
                    AUDIT_BODY_TEXT_FLOOR,
                );
                check(
                    "foreground/surface",
                    theme.foreground,
                    theme.surface,
                    AUDIT_BODY_TEXT_FLOOR,
                );
            }
            check(
                "selection_text/selection",
                theme.selection_text,
                theme.selection,
                AUDIT_BODY_TEXT_FLOOR,
            );
            for (ground_name, ground) in [
                ("background", Some(theme.background)),
                ("surface", Some(theme.surface)),
                ("current_line", theme.current_line),
            ] {
                if let Some(ground) = ground {
                    check(
                        &format!("caret/{ground_name}"),
                        theme.caret(),
                        ground,
                        AUDIT_BODY_TEXT_FLOOR,
                    );
                    check(
                        &format!("bracket/{ground_name}"),
                        theme.bracket(),
                        ground,
                        AUDIT_UI_FLOOR,
                    );
                    check(
                        &format!("error/{ground_name}"),
                        theme.error,
                        ground,
                        AUDIT_UI_FLOOR,
                    );
                }
            }
            check(
                "muted/background",
                theme.muted,
                theme.background,
                AUDIT_MUTED_FLOOR,
            );
            for (label, color) in [
                ("accent", theme.accent),
                ("warn", theme.warn),
                ("error", theme.error),
            ] {
                check(
                    &format!("{label}/background"),
                    color,
                    theme.background,
                    AUDIT_UI_FLOOR,
                );
                check(
                    &format!("{label}/surface"),
                    color,
                    theme.surface,
                    AUDIT_UI_FLOOR,
                );
            }

            // Tint adapts to each syntax colour and is checked separately.
            // Other styles have a single foreground/background pair.
            let mark_pair = match theme.event_mark {
                MarkStyle::Tint => None,
                MarkStyle::Underline => Some((theme.event, theme.background, AUDIT_UI_FLOOR)),
                MarkStyle::Fill => Some((theme.surface, theme.event, AUDIT_BODY_TEXT_FLOOR)),
                MarkStyle::Outline | MarkStyle::Text | MarkStyle::Invert => {
                    Some((theme.event, theme.background, AUDIT_BODY_TEXT_FLOOR))
                }
            };
            if let Some((mark_fg, mark_bg, floor)) = mark_pair {
                check("sounding mark", mark_fg, mark_bg, floor);
            }
        }

        assert!(
            failures.is_empty(),
            "themes with an unreadable colour pairing:\n{}",
            failures.join("\n")
        );
    }
}

#[cfg(test)]
mod documented_theme_tests {
    use super::Theme;

    #[test]
    fn the_documented_minimal_user_theme_loads() {
        // A Windows checkout (core.autocrlf) hands include_str! CRLF endings;
        // the documented fences below are matched with LF.
        let docs = include_str!("../../../docs/studio.md").replace("\r\n", "\n");
        let example = docs
            .split("### Minimal user theme")
            .nth(1)
            .expect("minimal user theme section")
            .split("```json\n")
            .nth(1)
            .expect("theme JSON example")
            .split("```")
            .next()
            .expect("theme JSON contents");
        Theme::from_json(example).expect("the documented theme has every required field");
    }
}

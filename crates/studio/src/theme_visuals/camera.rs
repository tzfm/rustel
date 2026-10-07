//! The camera, drawn as Braille dots behind the code.
//!
//! A Braille cell has eight dots: two across and four down. That is twice
//! the detail of a half block in each direction, and a face stays
//! recognisable at that resolution. The eight dots of a cell share one
//! colour, so the picture is drawn between the theme's muted colour and
//! its foreground. The shading comes from the number of lit dots, not
//! from their brightness.
//!
//! It paints the ground and only the ground: a cell holding a glyph, or
//! carrying a background of its own, is left exactly as it was. The score
//! stays readable over its own camera, and the continuation half of a wide
//! glyph is never taken for an empty cell.
//!
//! The theme also draws when there is no camera frame. Until the webcam
//! switch in Settings allows a camera, and while a camera opens, which can
//! be slow on macOS, a slow band sweeps down a dark ground. The missing
//! camera is then visible on screen and not only in the log.

use std::sync::Arc;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::super::visuals::braille_luma_glyph;
use super::{ActiveVisual, CameraSignal, NativeScene, ReactiveAudio, blend_rgb};

/// Dots per cell, across and down: what Braille gives.
const DOTS_ACROSS: usize = 2;
const DOTS_DOWN: usize = 4;

#[derive(Clone, Debug)]
pub(super) struct BrailleCamera {
    background: Color,
    foreground: Color,
    muted: Color,
    /// The theme's density, which bends the camera's luminance: a dim room
    /// still fills the dots, a bright one does not blow out to solid.
    density: u8,
    /// The theme's speed, which is how fast the no-signal band sweeps.
    speed: u8,
    frames: Arc<CameraSignal>,
}

impl BrailleCamera {
    pub(super) fn new(active: &ActiveVisual, frames: Arc<CameraSignal>) -> Self {
        Self {
            background: active.background,
            foreground: active.foreground,
            muted: active.muted,
            density: active.config.density,
            speed: active.config.speed,
            frames,
        }
    }

    /// How the theme's density bends the camera's luminance before it
    /// becomes dots.
    ///
    /// A lift would have been simpler and is wrong: a room's dark corners
    /// sit around a tenth of full, the lowest dot of the ordered matrix
    /// lights at a sixteenth, and adding a constant to everything crosses
    /// that everywhere at once - the whole pane stipples and the picture
    /// has no black to sit in. A curve leaves the dark end dark and lifts
    /// the middle, which is where a face is. Fifty is very nearly the
    /// camera as it came.
    fn contrast(&self) -> f32 {
        2.2 - f32::from(self.density.clamp(1, 100)) / 100.0
    }

    /// One sample of luminance as a level, 0 through 1.
    fn level(&self, luma: u8, contrast: f32) -> f32 {
        (f32::from(luma) / 255.0).powf(contrast)
    }

    /// What a dot shows while there is no camera: a slow band sweeping
    /// down, the way a screen with no signal on it still has something to
    /// look at. Dark between passes, so code stays readable over it.
    fn no_signal_level(&self, elapsed_ms: u64, y: usize, height: usize) -> f32 {
        let height = height.max(1) as f32;
        let place = y as f32 / height;
        let period = 6_000.0 - f32::from(self.speed.clamp(1, 100)) * 40.0;
        let sweep = ((elapsed_ms as f32 % period) / period + place) % 1.0;
        let band = (1.0 - (sweep - 0.5).abs() * 2.0).powi(6);
        (0.02 + 0.42 * band).clamp(0.0, 1.0)
    }
}

impl NativeScene for BrailleCamera {
    fn name(&self) -> &'static str {
        "rustel_braille_camera"
    }

    fn render(&self, elapsed_ms: u64, _audio: ReactiveAudio, buffer: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        let across = usize::from(area.width) * DOTS_ACROSS;
        let down = usize::from(area.height) * DOTS_DOWN;
        let contrast = self.contrast();
        let now = Instant::now();
        self.frames.with(now, |picture| {
            for row in 0..area.height {
                for column in 0..area.width {
                    let Some(cell) = buffer.cell_mut((area.x + column, area.y + row)) else {
                        continue;
                    };
                    // Never repaint source text, a selection, a visualizer
                    // or a panel. This also avoids the continuation half of
                    // a wide glyph, whose symbol is empty rather than one
                    // blank cell.
                    if cell.symbol() != " " || cell.bg != self.background {
                        continue;
                    }
                    let mut patch = [[0u8; DOTS_ACROSS]; DOTS_DOWN];
                    let mut total = 0.0f32;
                    for (dy, patch_row) in patch.iter_mut().enumerate() {
                        for (dx, sample) in patch_row.iter_mut().enumerate() {
                            let y = usize::from(row) * DOTS_DOWN + dy;
                            let level = match picture {
                                // Capture already mirrors the shared feed.
                                // Keep the same orientation as Hydra themes.
                                Some(picture) => self.level(
                                    picture.cover_luma(
                                        usize::from(column) * DOTS_ACROSS + dx,
                                        y,
                                        across,
                                        down,
                                    ),
                                    contrast,
                                ),
                                None => self.no_signal_level(elapsed_ms, y, down),
                            };
                            *sample = (level.clamp(0.0, 1.0) * 255.0) as u8;
                            total += level;
                        }
                    }
                    let glyph = braille_luma_glyph(patch);
                    if glyph == '\u{2800}' {
                        continue;
                    }
                    let mean = total / (DOTS_ACROSS * DOTS_DOWN) as f32;
                    // A lit face is not the same flat colour as its own
                    // shadow: the mean carries the tone the dots cannot.
                    cell.set_char(glyph)
                        .set_fg(blend_rgb(self.muted, self.foreground, mean));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::theme::{CellEffect, CellVisual, TachyonMode, Theme};
    use super::super::CameraPicture;
    use super::*;

    fn visual(theme: &Theme) -> ActiveVisual {
        ActiveVisual {
            config: CellVisual {
                effect: CellEffect::Camera,
                mode: TachyonMode::Text,
                density: 72,
                speed: 40,
            },
            background: theme.background,
            foreground: theme.foreground,
            accent: theme.accent,
            secondary: theme.syntax.number,
            muted: theme.muted,
        }
    }

    /// A ground seeded the way a finished frame is: the theme's background,
    /// some source text, a panel cell carrying its own background, and a
    /// wide glyph with its empty continuation.
    fn ground(theme: &Theme, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, ratatui::style::Style::default().bg(theme.background));
        buffer
            .cell_mut((1, 1))
            .expect("text cell")
            .set_symbol("s")
            .set_fg(theme.foreground);
        buffer
            .cell_mut((2, 2))
            .expect("panel cell")
            .set_symbol("P")
            .set_bg(theme.surface);
        buffer.cell_mut((4, 2)).expect("wide").set_symbol("界");
        buffer
            .cell_mut((5, 2))
            .expect("continuation")
            .set_symbol("");
        buffer
    }

    fn picture() -> CameraPicture {
        let (width, height) = (32usize, 18usize);
        CameraPicture {
            width: width as u16,
            height: height as u16,
            luma: (0..width * height)
                .map(|at| {
                    let (x, y) = (at % width, at / width);
                    // A bright disc on a dark ground: something with a
                    // middle, so the dots have shading to find.
                    let dx = x as i32 - width as i32 / 2;
                    let dy = y as i32 - height as i32 / 2;
                    let distance = ((dx * dx + dy * dy) as f32).sqrt();
                    (255.0 - distance * 22.0).clamp(0.0, 255.0) as u8
                })
                .collect(),
        }
    }

    #[test]
    fn the_camera_preserves_the_shared_feeds_orientation() {
        let theme = Theme::built_in_default();
        let area = Rect::new(3, 5, 2, 2);
        // The camera feed is already mirrored, just like the Hydra input.
        // This asymmetric image distinguishes both axes and light from dark.
        let mut luma = vec![0; 4 * 8];
        for (x, y) in [(0, 0), (1, 1), (3, 2), (0, 3), (2, 4), (3, 7)] {
            luma[y * 4 + x] = 255;
        }
        let signal = Arc::new(CameraSignal::default());
        signal.store(
            CameraPicture {
                width: 4,
                height: 8,
                luma,
            },
            Instant::now(),
        );
        let painter = BrailleCamera::new(&visual(&theme), signal);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, ratatui::style::Style::default().bg(theme.background));
        painter.render(0, ReactiveAudio::default(), &mut buffer, area);

        assert_eq!(buffer[(3, 5)].symbol(), "\u{2851}", "top-left dots 1, 5, 7");
        assert_eq!(buffer[(4, 5)].symbol(), "\u{2820}", "top-right dot 6");
        assert_eq!(buffer[(3, 6)].symbol(), " ", "bottom-left stays dark");
        assert_eq!(
            buffer[(4, 6)].symbol(),
            "\u{2881}",
            "bottom-right dots 1, 8"
        );
    }

    /// The camera paints the ground and only the ground, in Braille and
    /// nothing else, and never writes a background: the score stays
    /// readable over its own picture and a wide glyph keeps both halves.
    #[test]
    fn the_camera_paints_only_the_ground_and_only_in_braille() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 12, 6);
        let signal = Arc::new(CameraSignal::default());
        signal.store(picture(), Instant::now());
        let painter = BrailleCamera::new(&visual(&theme), Arc::clone(&signal));
        let before = ground(&theme, area);
        let mut after = before.clone();
        painter.render(120, ReactiveAudio::default(), &mut after, area);

        let mut shaded = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let (was, now) = (&before[(x, y)], &after[(x, y)]);
                if was == now {
                    continue;
                }
                let glyph = now.symbol().chars().next().expect("a glyph");
                assert!(
                    (0x2800..=0x28ff).contains(&(glyph as u32)),
                    "the camera draws Braille and nothing else, saw {glyph:?}"
                );
                shaded |= glyph != '\u{2800}' && glyph != '\u{28ff}';
                assert_eq!(now.bg, was.bg, "a background is never written");
            }
        }
        assert!(shaded, "the picture keeps its intermediate shades");
        for cell in [(1, 1), (2, 2), (4, 2), (5, 2)] {
            assert_eq!(
                before[cell], after[cell],
                "text, a panel and both halves of a wide glyph are left alone"
            );
        }
    }

    /// With no camera frame the theme still draws a moving sweep. A fresh
    /// frame becomes the picture, and a frame that ages out gives the
    /// sweep back.
    #[test]
    fn the_camera_moves_with_no_frame_and_resolves_when_one_lands() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 16, 6);
        let signal = Arc::new(CameraSignal::default());
        let painter = BrailleCamera::new(&visual(&theme), Arc::clone(&signal));
        let draw = |elapsed: u64| {
            let mut buffer = ground(&theme, area);
            painter.render(elapsed, ReactiveAudio::default(), &mut buffer, area);
            buffer
        };
        let early = draw(0);
        let later = draw(2_400);
        assert_ne!(early, later, "the no-signal sweep moves");

        signal.store(picture(), Instant::now());
        let live = draw(2_400);
        assert_ne!(live, later, "a frame that lands becomes the picture");

        signal.store(
            picture(),
            Instant::now()
                - super::super::CAMERA_FRAME_LIFETIME
                - std::time::Duration::from_secs(1),
        );
        let stale = draw(2_400);
        assert_eq!(stale, later, "a frame that ages out gives the sweep back");
    }
}

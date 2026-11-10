//! TachyonFX effects selected by a Studio theme.
//!
//! Text mode transforms Ratatui's completed buffer. Image mode reduces the
//! same transformation to RGBA pixels for the shared backdrop compositor.
//! Individual presets still own stricter rules: Matrix, for example, writes
//! only unused cells on the theme's background, so syntax wins.
//!
//! The engines live in `rustel-studio-themes`. This module keeps the studio
//! paths and turns a [`Theme`] into the paint those engines take.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use rustel_studio_themes::visuals::ThemeVisualEngine as Engine;
use rustel_studio_themes::{TachyonMode, ThemePaint};

use super::theme::Theme;

#[cfg(feature = "hydra")]
pub use rustel_studio_themes::visuals::CameraPicture;
pub use rustel_studio_themes::visuals::{
    CharacterVisualSurfaces, ThemeVisualImage, ThemeVisualSurfaces,
};

#[derive(Debug, Default)]
pub struct ThemeVisualEngine(Engine);

impl ThemeVisualEngine {
    pub fn update_audio(&self, frame: Option<&rustel_runtime::ui_analysis::UiAudioAnalysisFrame>) {
        let audio = reactive_audio(frame);
        self.0
            .store_audio(audio.rms, audio.bass, audio.mid, audio.treble);
    }

    /// Hand a camera frame to whichever painter wants one. A frame that
    /// does not arrive is not the same as a camera that has gone: the last
    /// one is held until it ages out, so a camera delivering at fifteen
    /// frames a second does not flicker at sixty.
    #[cfg(feature = "hydra")]
    pub fn update_camera(&self, frame: CameraPicture, now: Instant) {
        self.0.update_camera(frame, now);
    }

    /// The camera is gone: blocked, closed, or a theme that never wanted
    /// one. The picture goes at once rather than ageing out.
    #[cfg(feature = "hydra")]
    pub fn clear_camera(&self) {
        self.0.clear_camera();
    }

    /// Whether a camera frame is on hand and still recent. The studio says
    /// so in the header's camera chip.
    #[cfg(feature = "hydra")]
    pub fn camera_frame_live(&self, now: Instant) -> bool {
        self.0.camera_frame_live(now)
    }

    pub fn sync(&mut self, theme: &Theme, now: Instant) {
        self.0.sync(
            &ThemePaint {
                cell: theme.cell_visual(),
                characters: theme.character_visual(),
                background: theme.background,
                foreground: theme.foreground,
                accent: theme.accent,
                secondary: theme.syntax.number,
                muted: theme.muted,
            },
            now,
        );
    }

    pub fn mode(&self) -> Option<TachyonMode> {
        self.0.mode()
    }

    /// Transform the terminal cells themselves. The effect is seeded with
    /// the finished editor frame, while interface backgrounds are temporarily
    /// normalized so a low `ui opacity` can reveal the same moving layer.
    pub fn paint_text(
        &mut self,
        now: Instant,
        buffer: &mut Buffer,
        area: Rect,
        surfaces: ThemeVisualSurfaces<'_>,
    ) {
        self.0.paint_text(now, buffer, area, surfaces);
    }

    /// Render the same effect as a backdrop. Changed cells become solid RGBA
    /// pixels and unchanged cells remain transparent; the shared backdrop
    /// compositor later applies editor and interface opacity.
    pub fn image(&mut self, now: Instant, buffer: &Buffer, area: Rect) -> Option<ThemeVisualImage> {
        self.0.image(now, buffer, area)
    }

    /// Animate score glyphs after its backdrop has been composed. Interface
    /// text, modal overlays, selections, the caret and sounding marks remain
    /// stable so the treatment never takes control away from the editor.
    pub fn paint_characters(
        &self,
        now: Instant,
        buffer: &mut Buffer,
        area: Rect,
        surfaces: CharacterVisualSurfaces<'_>,
    ) {
        self.0.paint_characters(now, buffer, area, surfaces);
    }
}

struct ReactiveAudio {
    rms: f32,
    bass: f32,
    mid: f32,
    treble: f32,
}

fn reactive_audio(
    frame: Option<&rustel_runtime::ui_analysis::UiAudioAnalysisFrame>,
) -> ReactiveAudio {
    let Some(frame) = frame else {
        return ReactiveAudio {
            rms: 0.0,
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
        };
    };
    let rms = (frame
        .scope
        .iter()
        .map(|sample| sample * sample)
        .sum::<f32>()
        / frame.scope.len().max(1) as f32)
        .sqrt();
    let band = |from: usize, to: usize| {
        if frame.spectrum.len() <= from {
            return 0.0;
        }
        let high = to.min(frame.spectrum.len()).max(from + 1);
        let peak = frame.spectrum[from..high]
            .iter()
            .copied()
            .fold(-120.0f32, f32::max);
        ((peak + 60.0) / 60.0).clamp(0.0, 1.0)
    };
    ReactiveAudio {
        rms: (rms * 4.0).clamp(0.0, 1.0),
        bass: band(1, 8),
        mid: band(8, 64),
        treble: band(64, 220),
    }
}

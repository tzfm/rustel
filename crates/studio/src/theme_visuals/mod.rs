//! TachyonFX effects selected by a Studio theme.
//!
//! Text mode transforms Ratatui's completed buffer. Image mode reduces the
//! same transformation to RGBA pixels for the shared backdrop compositor.
//! Individual presets still own stricter rules: Matrix, for example, writes
//! only unused cells on the theme's background, so syntax wins.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration as WallDuration, Instant};

use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use tachyonfx::{CellFilter, Duration, Effect, EffectManager, Shader};
use unicode_width::UnicodeWidthStr;

use super::theme::{CellEffect, CellVisual, CharacterEffect, CharacterVisual, TachyonMode, Theme};

mod basketball;
mod bear;
mod brand;
mod camera;
mod campfire;
mod canvas;
mod characters;
mod cyberpunk;
mod dvd;
mod forest;
mod glitch;
mod hell;
mod leak;
mod matrix;
mod mode7;
mod motion;
mod particles;
mod reveals;
mod showroom;
mod space;
mod vectrex;
mod weather;

#[cfg(test)]
mod small_layout_tests {
    use super::test_support::showroom_test_visual;
    use super::*;
    use ratatui::style::Style;

    fn render(effect: CellEffect, area: Rect, buffer: &mut Buffer, background: Color) {
        let active = showroom_test_visual(effect, background);
        let audio = ReactiveAudio {
            rms: 0.4,
            bass: 0.6,
            mid: 0.5,
            treble: 0.5,
        };
        if effect == CellEffect::RustelBrand {
            RustelBrandScene::new(&active).render(1_850, audio, buffer, area);
        } else {
            let signal = Arc::new(ReactiveAudioSignal::default());
            signal.store(audio);
            let mut scene = ShowroomFx::new(&active, signal);
            scene.elapsed_ms = 1_850;
            scene.forest.advance(0.05, audio);
            scene.render(buffer, area);
        }
    }

    #[test]
    fn small_and_empty_theme_frames_stay_inside_their_area() {
        let background = Color::Rgb(6, 16, 10);
        for effect in [CellEffect::Forest, CellEffect::RustelBrand] {
            for width in [0, 1, 3, 4, 30, 31, 120] {
                for height in [0, 1, 6, 10, 11, 12, 40] {
                    let area = Rect::new(2, 2, width, height);
                    let outer = Rect::new(0, 0, width + 4, height + 4);
                    let mut buffer = Buffer::empty(outer);
                    buffer.set_style(outer, Style::default().bg(background));
                    let before = buffer.clone();
                    render(effect, area, &mut buffer, background);
                    assert_eq!(buffer.area, outer);
                    assert_eq!(buffer.content.len(), before.content.len());
                    for y in 0..outer.height {
                        for x in 0..outer.width {
                            let cell = buffer.cell((x, y)).expect("cell");
                            if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
                                assert_eq!(
                                    Some(cell),
                                    before.cell((x, y)),
                                    "{effect:?} changed a cell outside {area:?}"
                                );
                            } else {
                                assert_eq!(cell.symbol().width(), 1, "{effect:?} at {area:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn small_theme_frames_draw_and_preserve_protected_cells() {
        let background = Color::Rgb(6, 16, 10);
        for effect in [CellEffect::Forest, CellEffect::RustelBrand] {
            let area = Rect::new(2, 3, 20, 8);
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            buffer
                .cell_mut((3, 4))
                .expect("panel cell")
                .set_symbol("p")
                .set_bg(Color::Rgb(30, 20, 40));
            buffer.cell_mut((5, 4)).expect("wide cell").set_symbol("界");
            buffer.cell_mut((8, 4)).expect("text cell").set_symbol("x");
            let before = buffer.clone();
            render(effect, area, &mut buffer, background);
            assert_eq!(buffer.cell((3, 4)), before.cell((3, 4)), "{effect:?}");
            assert_eq!(buffer.cell((5, 4)), before.cell((5, 4)), "{effect:?}");
            assert_eq!(buffer.cell((8, 4)).expect("text cell").symbol(), "x");
            assert!(
                buffer
                    .content
                    .iter()
                    .zip(&before.content)
                    .any(|(after, before)| before.symbol() == " " && after.symbol() != " "),
                "{effect:?} must still draw in the small area"
            );
        }
    }
}

use basketball::CourtMemo;
use bear::BearMemo;
use brand::RustelBrandScene;
use camera::BrailleCamera;
use canvas::{SceneCanvas, SceneFrame, SpaceBody, SpaceSprite};
use characters::{CharacterShader, character_effect, fade_cell, paint_character_effect};
use cyberpunk::CyberpunkLandscape;
use dvd::DvdBounce;
use forest::ForestMemo;
use leak::LeakWater;
use matrix::MatrixRain;
use particles::{
    BUBBLE_COUNT, BubbleFrame, FUME_COUNT, FumeFrame, SMOKE_COUNT, SmokeFrame, bubble_frames,
    fume_frames, smoke_frames,
};
use showroom::{Frame, ShowroomFx, Spot};
use vectrex::VectrexFlight;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActiveVisual {
    pub(super) config: CellVisual,
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) secondary: Color,
    pub(super) muted: Color,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActiveCharacters {
    pub(super) config: CharacterVisual,
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) secondary: Color,
    pub(super) muted: Color,
}

/// Renderer-neutral audio energy shared by every native scene. The UI owns
/// the analyser frame; scenes receive four bounded scalars and never depend
/// on the engine, device, or Hydra protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ReactiveAudio {
    pub(super) rms: f32,
    pub(super) bass: f32,
    pub(super) mid: f32,
    pub(super) treble: f32,
}

#[derive(Debug, Default)]
pub(super) struct ReactiveAudioSignal {
    pub(super) rms: AtomicU32,
    pub(super) bass: AtomicU32,
    pub(super) mid: AtomicU32,
    pub(super) treble: AtomicU32,
}

impl ReactiveAudioSignal {
    pub(super) fn store(&self, audio: ReactiveAudio) {
        self.rms.store(audio.rms.to_bits(), Ordering::Relaxed);
        self.bass.store(audio.bass.to_bits(), Ordering::Relaxed);
        self.mid.store(audio.mid.to_bits(), Ordering::Relaxed);
        self.treble.store(audio.treble.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn load(&self) -> ReactiveAudio {
        ReactiveAudio {
            rms: f32::from_bits(self.rms.load(Ordering::Relaxed)),
            bass: f32::from_bits(self.bass.load(Ordering::Relaxed)),
            mid: f32::from_bits(self.mid.load(Ordering::Relaxed)),
            treble: f32::from_bits(self.treble.load(Ordering::Relaxed)),
        }
    }
}

pub(super) fn reactive_audio(
    frame: Option<&rustel_runtime::ui_analysis::UiAudioAnalysisFrame>,
) -> ReactiveAudio {
    let Some(frame) = frame else {
        return ReactiveAudio::default();
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

/// The camera as a native painter needs it: one luminance plane, small
/// enough to hand over whole and large enough for a terminal's dots.
///
/// Luminance and nothing else, because the only painter that wants it draws
/// Braille, where a cell's eight dots share one colour and the shading is
/// how many of them light. It says nothing about where the bytes came
/// from: the painter never learns whether a camera, a file or a test wrote
/// them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraPicture {
    pub width: u16,
    pub height: u16,
    /// One byte a pixel, row major from the top left, in the same display
    /// orientation as the Hydra camera feed (already mirrored by capture).
    pub luma: Vec<u8>,
}

impl CameraPicture {
    /// The luminance at dot `(x, y)` of a `width` by `height` grid of dots,
    /// the picture covering that grid rather than being stretched over it:
    /// the surplus of whichever axis has one is cropped away evenly, so a
    /// face keeps its proportions on any shape of terminal.
    pub(super) fn cover_luma(&self, x: usize, y: usize, width: usize, height: usize) -> u8 {
        let (source_width, source_height) = (usize::from(self.width), usize::from(self.height));
        if width == 0
            || height == 0
            || source_width == 0
            || source_height == 0
            || self.luma.len() != source_width * source_height
        {
            return 0;
        }
        let scale = (source_width as f32 / width as f32).max(source_height as f32 / height as f32);
        let left = (source_width as f32 - width as f32 * scale) / 2.0;
        let top = (source_height as f32 - height as f32 * scale) / 2.0;
        let source_x = ((left + x as f32 * scale) as usize).min(source_width - 1);
        let source_y = ((top + y as f32 * scale) as usize).min(source_height - 1);
        self.luma[source_y * source_width + source_x]
    }
}

/// How long a camera frame stays the picture. An older frame means that
/// the camera is closed, revoked, or never opened, and the painter draws
/// the theme's no-camera picture again.
pub(super) const CAMERA_FRAME_LIFETIME: WallDuration = WallDuration::from_secs(1);

/// Where the newest camera frame waits for the painter.
///
/// The studio writes it from wherever a camera is open and the painter
/// reads whichever frame was last written, so a camera that stalls stalls
/// nothing: the picture holds, and then ages out.
#[derive(Debug, Default)]
pub(super) struct CameraSignal {
    frame: std::sync::Mutex<Option<(Instant, CameraPicture)>>,
}

impl CameraSignal {
    // A build with no camera stack has nothing to write here, and its
    // painter draws the no-signal sweep for ever. The tests write frames
    // in either build, which is how the sweep and the picture are both
    // covered without a camera in the room.
    #[cfg(any(feature = "hydra", test))]
    pub(super) fn store(&self, frame: CameraPicture, now: Instant) {
        *self
            .frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((now, frame));
    }

    #[cfg(feature = "hydra")]
    pub(super) fn clear(&self) {
        *self
            .frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Read the frame without copying it, if one is still fresh.
    pub(super) fn with<R>(
        &self,
        now: Instant,
        read: impl FnOnce(Option<&CameraPicture>) -> R,
    ) -> R {
        let held = self
            .frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = held
            .as_ref()
            .filter(|(stamp, _)| now.saturating_duration_since(*stamp) < CAMERA_FRAME_LIFETIME);
        read(fresh.map(|(_, picture)| picture))
    }

    #[cfg(feature = "hydra")]
    pub(super) fn live(&self, now: Instant) -> bool {
        self.with(now, |frame| frame.is_some())
    }
}

/// Persistent state for the visual backend. The manager is the same
/// post-render buffer pipeline used by TachyonFX applications such as
/// Exabind; presets remain Rustel-owned and bounded by the theme schema.
#[derive(Debug, Default)]
pub struct ThemeVisualEngine {
    active: Option<ActiveVisual>,
    characters: Option<ActiveCharacters>,
    character_shader: Option<Box<dyn CharacterShader>>,
    character_started: Option<Instant>,
    effects: EffectManager<()>,
    effect_buffer: Option<Buffer>,
    last_frame: Option<Instant>,
    audio: Arc<ReactiveAudioSignal>,
    camera: Arc<CameraSignal>,
}

/// A TachyonFX pass reduced to one RGBA pixel per terminal cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThemeVisualImage {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

pub struct ThemeVisualSurfaces<'a> {
    pub editor_strength: f32,
    pub interface_strength: f32,
    pub interface: &'a [Rect],
    pub protected: &'a [Rect],
}

pub struct CharacterVisualSurfaces<'a> {
    pub strength: f32,
    pub interface: &'a [Rect],
    pub protected: &'a [Rect],
    pub selected: &'a HashSet<(u16, u16)>,
    pub highlighted: &'a HashSet<(u16, u16)>,
}

impl ThemeVisualEngine {
    pub fn update_audio(&self, frame: Option<&rustel_runtime::ui_analysis::UiAudioAnalysisFrame>) {
        self.audio.store(reactive_audio(frame));
    }

    /// Hand a camera frame to whichever painter wants one. A frame that
    /// does not arrive is not the same as a camera that has gone: the last
    /// one is held until it ages out, so a camera delivering at fifteen
    /// frames a second does not flicker at sixty.
    #[cfg(feature = "hydra")]
    pub fn update_camera(&self, frame: CameraPicture, now: Instant) {
        self.camera.store(frame, now);
    }

    /// The camera is gone: blocked, closed, or a theme that never wanted
    /// one. The picture goes at once rather than ageing out.
    #[cfg(feature = "hydra")]
    pub fn clear_camera(&self) {
        self.camera.clear();
    }

    /// Whether a camera frame is on hand and still recent. The studio says
    /// so in the header's camera chip.
    #[cfg(feature = "hydra")]
    pub fn camera_frame_live(&self, now: Instant) -> bool {
        self.camera.live(now)
    }

    pub fn sync(&mut self, theme: &Theme, now: Instant) {
        let active = theme.cell_visual().map(|config| ActiveVisual {
            config,
            background: theme.background,
            foreground: theme.foreground,
            accent: theme.accent,
            secondary: theme.syntax.number,
            muted: theme.muted,
        });
        let characters = theme.character_visual().map(|config| ActiveCharacters {
            config,
            background: theme.background,
            foreground: theme.foreground,
            accent: theme.accent,
            secondary: theme.syntax.number,
            muted: theme.muted,
        });
        if self.characters != characters {
            self.character_shader = characters.as_ref().map(character_effect);
            self.characters = characters;
            self.character_started = Some(now);
        }
        if self.active != active {
            self.effects = EffectManager::default();
            self.last_frame = Some(now);
            if let Some(active) = active.as_ref() {
                self.effects.add_effect(theme_effect(
                    active,
                    Arc::clone(&self.audio),
                    Arc::clone(&self.camera),
                ));
            }
            self.active = active;
        }
    }

    pub fn mode(&self) -> Option<TachyonMode> {
        self.active.as_ref().map(|active| active.config.mode)
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
        if self.mode() != Some(TachyonMode::Text) {
            return;
        }
        let Some(effect_buffer) = self.process(now, buffer, area, surfaces.interface, false) else {
            return;
        };
        composite_effect(
            buffer,
            effect_buffer,
            area,
            surfaces.editor_strength,
            surfaces.interface_strength,
            surfaces.interface,
            surfaces.protected,
        );
    }

    /// Render the same effect as a backdrop. Changed cells become solid RGBA
    /// pixels and unchanged cells remain transparent; the shared backdrop
    /// compositor later applies editor and interface opacity.
    pub fn image(&mut self, now: Instant, buffer: &Buffer, area: Rect) -> Option<ThemeVisualImage> {
        if self.mode() != Some(TachyonMode::Image) {
            return None;
        }
        let effect_buffer = self.process(now, buffer, area, &[], true)?;
        Some(effect_image(buffer, effect_buffer, area))
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
        let (Some(shader), Some(started)) = (&self.character_shader, self.character_started) else {
            return;
        };
        let elapsed = now.saturating_duration_since(started).as_secs_f32();
        paint_character_effect(shader.as_ref(), elapsed, buffer, area, surfaces);
    }

    pub(super) fn process<'a>(
        &'a mut self,
        now: Instant,
        buffer: &Buffer,
        area: Rect,
        normalize: &[Rect],
        normalize_all: bool,
    ) -> Option<&'a Buffer> {
        let background = self.active.as_ref()?.background;
        let elapsed = self
            .last_frame
            .replace(now)
            .map_or(WallDuration::ZERO, |last| {
                now.saturating_duration_since(last)
            })
            // Do not teleport an effect across the screen after the process
            // was suspended or the terminal stopped drawing.
            .min(WallDuration::from_millis(100));
        let effect_buffer = reuse(&mut self.effect_buffer, buffer);
        let normalized_area = area.intersection(effect_buffer.area);
        for y in normalized_area.y..normalized_area.bottom() {
            for x in normalized_area.x..normalized_area.right() {
                if (normalize_all || inside_any(normalize, x, y))
                    && let Some(cell) = effect_buffer.cell_mut((x, y))
                {
                    cell.set_bg(background);
                }
            }
        }
        self.effects
            .process_effects(elapsed.into(), effect_buffer, area);
        // Normalizing a surface is only permission for the shader to see it;
        // it is not itself a visual change, so the real background goes back
        // before composition.
        //
        // Normalization changes only `bg`, so restoring that field undoes it
        // without comparing each cell's style and symbol.
        let reconciled = area
            .intersection(buffer.area)
            .intersection(effect_buffer.area);
        if reconciled == buffer.area && reconciled == effect_buffer.area {
            for (effect, before) in effect_buffer.content.iter_mut().zip(&buffer.content) {
                effect.bg = before.bg;
            }
        } else {
            for y in reconciled.y..reconciled.bottom() {
                for x in reconciled.x..reconciled.right() {
                    let Some(bg) = buffer.cell((x, y)).map(|before| before.bg) else {
                        continue;
                    };
                    if let Some(effect) = effect_buffer.cell_mut((x, y)) {
                        effect.bg = bg;
                    }
                }
            }
        }
        Some(effect_buffer)
    }
}

/// Refill a kept buffer from `source` without reallocating it.
///
/// `Buffer` only derives `Clone`, and a derived `clone_from` is
/// `*self = source.clone()` - a brand-new `Vec<Cell>` every call. At 200x50
/// that is 480 KB allocated and freed per frame, sixty times a second,
/// while an audio callback is running. `Vec::clone_from` is specialized to
/// reuse its allocation, so the copy goes one field deeper to reach it.
pub(super) fn reuse<'a>(slot: &'a mut Option<Buffer>, source: &Buffer) -> &'a mut Buffer {
    let buffer = slot.get_or_insert_with(|| Buffer {
        area: source.area,
        content: Vec::new(),
    });
    buffer.area = source.area;
    buffer.content.clone_from(&source.content);
    buffer
}

pub(super) fn composite_effect(
    buffer: &mut Buffer,
    effect_buffer: &Buffer,
    area: Rect,
    editor_strength: f32,
    interface_strength: f32,
    interface: &[Rect],
    protected: &[Rect],
) {
    let area = area
        .intersection(buffer.area)
        .intersection(effect_buffer.area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if inside_any(protected, x, y) {
                continue;
            }
            let is_interface = inside_any(interface, x, y);
            let strength = if is_interface {
                interface_strength
            } else {
                editor_strength
            }
            .clamp(0.0, 1.0);
            let Some(effect) = effect_buffer.cell((x, y)) else {
                continue;
            };
            let Some(cell) = buffer.cell_mut((x, y)) else {
                continue;
            };
            if cell == effect || strength <= 0.0 {
                continue;
            }
            if is_interface && cell.symbol() != " " {
                // Controls must keep their labels, colours and emphasis even
                // when a text effect decrypts or dims the score. Opacity can
                // soften a colour, but cannot make a substituted menu letter
                // readable. Keep its moving ground and let blank interface
                // cells carry particles, while preserving the control itself.
                cell.bg = blend_rgb(cell.bg, effect.bg, strength);
                continue;
            }
            if strength >= 1.0 {
                cell.clone_from(effect);
                continue;
            }
            fade_cell(
                cell,
                effect,
                strength,
                hash_unit(hash(u64::from(x), u64::from(y), 0xfade)),
            );
        }
    }
}

pub(super) fn effect_image(
    buffer: &Buffer,
    effect_buffer: &Buffer,
    area: Rect,
) -> ThemeVisualImage {
    let area = area
        .intersection(buffer.area)
        .intersection(effect_buffer.area);
    let mut rgba = Vec::with_capacity(usize::from(area.width) * usize::from(area.height) * 4);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let (Some(before), Some(effect)) = (buffer.cell((x, y)), effect_buffer.cell((x, y)))
            else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            };
            if before == effect {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let color = if before.bg != effect.bg {
                effect.bg
            } else {
                effect.fg
            };
            let (red, green, blue) = super::graphics::rgb(color);
            rgba.extend_from_slice(&[red, green, blue, 255]);
        }
    }
    ThemeVisualImage {
        width: area.width,
        height: area.height,
        rgba,
    }
}

pub(super) fn inside_any(rects: &[Rect], x: u16, y: u16) -> bool {
    rects
        .iter()
        .any(|rect| x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom())
}

/// Native scenes have one narrow contract: render a bounded frame for an
/// elapsed time. The factory below is the only place that chooses one. A
/// future script-backed scene can implement the same contract without adding
/// another branch to the frame loop or the theme compositor.
pub(super) trait NativeScene: std::fmt::Debug + Send {
    fn name(&self) -> &'static str;
    fn render(&self, elapsed_ms: u64, audio: ReactiveAudio, buffer: &mut Buffer, area: Rect);
}

#[derive(Clone, Debug)]
pub(super) struct SceneEffect<S> {
    pub(super) scene: S,
    pub(super) audio: Arc<ReactiveAudioSignal>,
    pub(super) elapsed_ms: u64,
    pub(super) area: Option<Rect>,
}

impl<S> SceneEffect<S> {
    pub(super) fn new(scene: S, audio: Arc<ReactiveAudioSignal>) -> Self {
        Self {
            scene,
            audio,
            elapsed_ms: 0,
            area: None,
        }
    }
}

impl<S: NativeScene + Clone + 'static> Shader for SceneEffect<S> {
    fn name(&self) -> &'static str {
        self.scene.name()
    }

    fn process(&mut self, duration: Duration, buffer: &mut Buffer, area: Rect) -> Option<Duration> {
        self.elapsed_ms = self
            .elapsed_ms
            .wrapping_add(u64::from(duration.as_millis()));
        self.scene.render(
            self.elapsed_ms,
            self.audio.load(),
            buffer,
            self.area.unwrap_or(area).intersection(area),
        );
        None
    }

    fn done(&self) -> bool {
        false
    }

    fn clone_box(&self) -> Box<dyn Shader> {
        Box::new(self.clone())
    }

    fn area(&self) -> Option<Rect> {
        self.area
    }

    fn set_area(&mut self, area: Rect) {
        self.area = Some(area);
    }

    fn filter(&mut self, _filter: CellFilter) {}
}

pub(super) fn theme_effect(
    active: &ActiveVisual,
    audio: Arc<ReactiveAudioSignal>,
    camera: Arc<CameraSignal>,
) -> Effect {
    match active.config.effect {
        CellEffect::Camera => {
            Effect::new(SceneEffect::new(BrailleCamera::new(active, camera), audio))
        }
        CellEffect::Matrix => Effect::new(MatrixRain::new(active)),
        CellEffect::Cyberpunk => {
            Effect::new(SceneEffect::new(CyberpunkLandscape::new(active), audio))
        }
        CellEffect::Vectrex => Effect::new(SceneEffect::new(VectrexFlight::new(active), audio)),
        CellEffect::Dvd => Effect::new(SceneEffect::new(DvdBounce::new(active), audio)),
        CellEffect::RustelBrand => {
            Effect::new(SceneEffect::new(RustelBrandScene::new(active), audio))
        }
        _ => Effect::new(ShowroomFx::new(active, audio)),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Point2 {
    pub(super) x: f32,
    pub(super) y: f32,
}

pub(super) fn point_distance(a: Point2, b: Point2, aspect: f32) -> f32 {
    let dx = (a.x - b.x) * aspect;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

pub(super) fn segment_distance(point: Point2, from: Point2, to: Point2, aspect: f32) -> f32 {
    let px = point.x * aspect;
    let ax = from.x * aspect;
    let bx = to.x * aspect;
    let py = point.y;
    let ay = from.y;
    let by = to.y;
    let dx = bx - ax;
    let dy = by - ay;
    let length = dx * dx + dy * dy;
    if length <= f32::EPSILON {
        return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
    }
    let position = (((px - ax) * dx + (py - ay) * dy) / length).clamp(0.0, 1.0);
    let nearest_x = ax + dx * position;
    let nearest_y = ay + dy * position;
    ((px - nearest_x).powi(2) + (py - nearest_y).powi(2)).sqrt()
}

pub(super) const DIGITAL_GLYPHS: &[&str] = &[
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F", "#", "%", "&",
    "+", "-", "/", ":", "=", "?", "@", "[", "]", "{", "}", "<", ">",
];

#[cfg(test)]
pub(super) const SHOWROOM_GLYPHS: &[&str] = &[
    "▁", "▂", "▃", "▄", "▅", "▆", "▇", "╲", "╱", "0", "1", "○", "·", "◌", "●", "░", "▒", "▪", "▫",
    "─", "│", "*", "▼", "•", "▌", "╷", "∙", "▶", "◀", "»", "~", "O", "o", "V", "◆", ".", "+", "┼",
    "◇", "█", "◉", "∘", "✦", "▓", "▀", "▲", "'", ",", "\"", "═", "/", "\\", "2", "3", "4", "5",
    "6", "7", "8", "9", "S", "W", "I", "H", "C", "L", "A", "N", "K", "◖", "◗", "▐", "✿", "❀", "^",
    "v", "z", "Z", "!", "°", "♪", "♫",
];

pub(super) fn hash(a: u64, b: u64, c: u64) -> u64 {
    let mut value = a
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(b.rotate_left(21))
        .wrapping_add(c.rotate_left(43));
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

pub(super) fn hash_unit(value: u64) -> f32 {
    (value & 0xffff) as f32 / 65_535.0
}

pub(super) fn fract(value: f32) -> f32 {
    value.rem_euclid(1.0)
}

pub(super) fn wrapped_distance(a: f32, b: f32) -> f32 {
    let distance = (fract(a) - fract(b)).abs();
    distance.min(1.0 - distance)
}

pub(super) fn cycle_color(first: Color, second: Color, third: Color, phase: f32) -> Color {
    let phase = fract(phase) * 3.0;
    if phase < 1.0 {
        blend_rgb(first, second, phase)
    } else if phase < 2.0 {
        blend_rgb(second, third, phase - 1.0)
    } else {
        blend_rgb(third, first, phase - 2.0)
    }
}

pub(super) fn blend_rgb(from: Color, to: Color, amount: f32) -> Color {
    let (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) = (from, to) else {
        return if amount < 0.5 { from } else { to };
    };
    let blend = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * amount.clamp(0.0, 1.0)).round() as u8
    };
    Color::Rgb(blend(fr, tr), blend(fg, tg), blend(fb, tb))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn matrix_test_visual(background: Color) -> ActiveVisual {
        ActiveVisual {
            config: CellVisual {
                effect: CellEffect::Matrix,
                mode: TachyonMode::Text,
                density: 100,
                speed: 58,
            },
            background,
            foreground: Color::Rgb(184, 255, 200),
            accent: Color::Rgb(0, 255, 65),
            secondary: Color::Rgb(127, 255, 0),
            muted: Color::Rgb(31, 122, 55),
        }
    }

    pub(crate) fn showroom_test_visual(effect: CellEffect, background: Color) -> ActiveVisual {
        ActiveVisual {
            config: CellVisual {
                effect,
                mode: TachyonMode::Text,
                density: 100,
                speed: 58,
            },
            background,
            foreground: Color::Rgb(238, 246, 255),
            accent: Color::Rgb(0, 220, 255),
            secondary: Color::Rgb(255, 92, 216),
            muted: Color::Rgb(75, 82, 112),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::brand::{RUSTEL_LOGO, RUSTEL_LOGO_HEIGHT, RUSTEL_LOGO_STRIDE, RUSTEL_LOGO_WIDTH};
    use super::dvd::DVD_VIDEO_ART;
    use super::test_support::*;
    use super::*;
    use ratatui::style::Style;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn every_showroom_glyph_occupies_exactly_one_cell() {
        for glyph in DIGITAL_GLYPHS.iter().chain(SHOWROOM_GLYPHS) {
            assert_eq!(glyph.width(), 1, "{glyph:?} would damage the next cell");
        }
    }

    #[test]
    fn text_mode_applies_editor_and_interface_strengths_and_protects_previews() {
        let area = Rect::new(0, 0, 3, 1);
        let background = Color::Rgb(10, 20, 30);
        let mut baseline = Buffer::empty(area);
        baseline.set_style(area, Style::default().bg(background));
        baseline
            .cell_mut((1, 0))
            .expect("text cell")
            .set_symbol("x")
            .set_fg(Color::Rgb(100, 110, 120));

        let mut effect = baseline.clone();
        effect
            .cell_mut((0, 0))
            .expect("particle cell")
            .set_symbol("*")
            .set_fg(Color::Rgb(210, 220, 230));
        effect
            .cell_mut((1, 0))
            .expect("text cell")
            .set_fg(Color::Rgb(200, 210, 220));
        effect
            .cell_mut((2, 0))
            .expect("preview cell")
            .set_symbol("!")
            .set_fg(Color::Rgb(210, 220, 230));
        let preview = Rect::new(2, 0, 1, 1);
        let interface = Rect::new(1, 0, 1, 1);

        let mut hidden = baseline.clone();
        composite_effect(
            &mut hidden,
            &effect,
            area,
            0.0,
            0.0,
            &[interface],
            &[preview],
        );
        assert_eq!(hidden, baseline, "solid surfaces hide the effect");

        let mut full = baseline.clone();
        composite_effect(&mut full, &effect, area, 1.0, 1.0, &[interface], &[preview]);
        assert_eq!(full.cell((0, 0)), effect.cell((0, 0)));
        assert_eq!(full.cell((1, 0)), baseline.cell((1, 0)));
        assert_eq!(full.cell((2, 0)), baseline.cell((2, 0)));

        let mut half = baseline.clone();
        composite_effect(
            &mut half,
            &effect,
            area,
            0.5,
            0.25,
            &[interface],
            &[preview],
        );
        assert_eq!(
            half.cell((0, 0)).expect("particle cell").fg,
            Color::Rgb(110, 120, 130)
        );
        assert_eq!(
            half.cell((0, 0)).expect("particle cell").symbol(),
            "*",
            "opacity dims new geometry without perforating it"
        );
        assert_eq!(
            half.cell((1, 0)).expect("text cell").fg,
            Color::Rgb(100, 110, 120)
        );
        assert_eq!(half.cell((2, 0)), baseline.cell((2, 0)));
    }

    #[test]
    fn text_effects_keep_controls_sharp_but_leave_their_backdrop_and_score_animated() {
        let area = Rect::new(0, 0, 4, 1);
        let interface = Rect::new(0, 0, 2, 1);
        let mut baseline = Buffer::empty(area);
        baseline.set_style(
            area,
            Style::default()
                .bg(Color::Rgb(10, 20, 30))
                .fg(Color::Rgb(220, 230, 240)),
        );
        baseline.cell_mut((0, 0)).unwrap().set_symbol("M");
        baseline.cell_mut((0, 0)).unwrap().modifier = Modifier::BOLD | Modifier::UNDERLINED;
        baseline.cell_mut((2, 0)).unwrap().set_symbol("x");
        let mut effect = baseline.clone();
        for cell in &mut effect.content {
            cell.set_symbol("?")
                .set_fg(Color::Rgb(100, 110, 120))
                .set_bg(Color::Rgb(210, 220, 230));
            cell.modifier = Modifier::DIM;
        }

        for strength in [0.25, 1.0] {
            let mut painted = baseline.clone();
            composite_effect(
                &mut painted,
                &effect,
                area,
                strength,
                strength,
                &[interface],
                &[],
            );
            let mut label = baseline.cell((0, 0)).unwrap().clone();
            label.bg = blend_rgb(label.bg, effect.cell((0, 0)).unwrap().bg, strength);
            assert_eq!(painted.cell((0, 0)), Some(&label), "strength {strength}");
            assert_ne!(label.bg, baseline.cell((0, 0)).unwrap().bg);
            assert_eq!(
                painted.cell((1, 0)).unwrap().symbol(),
                "?",
                "blank chrome still carries the animated geometry"
            );
            assert_eq!(
                painted.cell((1, 0)).unwrap().fg,
                painted.cell((3, 0)).unwrap().fg,
                "blank chrome and score cells follow the same opacity"
            );
            assert_ne!(
                painted.cell((2, 0)).unwrap().fg,
                baseline.cell((2, 0)).unwrap().fg,
                "the score still receives its text effect"
            );
            if strength == 1.0 {
                assert_eq!(painted.cell((2, 0)), effect.cell((2, 0)));
            }
        }
    }

    #[test]
    fn text_mode_normalizes_interface_ground_without_repainting_it() {
        let mut theme = Theme::built_in("burn").expect("burn theme");
        assert!(theme.set_tachyon_mode(TachyonMode::Text));
        let area = Rect::new(0, 0, 40, 12);
        let panel = Rect::new(0, 10, 40, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, Style::default().bg(theme.background));
        buffer.set_style(panel, Style::default().bg(theme.surface));
        let baseline = buffer.clone();
        let started = Instant::now();
        let mut engine = ThemeVisualEngine::default();
        engine.sync(&theme, started);

        engine.paint_text(
            started + WallDuration::from_millis(100),
            &mut buffer,
            area,
            ThemeVisualSurfaces {
                editor_strength: 0.0,
                interface_strength: 1.0,
                interface: &[panel],
                protected: &[],
            },
        );

        assert!(
            (panel.x..panel.right()).any(|x| buffer.cell((x, 11)).unwrap().symbol() != " "),
            "a transparent interface reveals the flame layer"
        );
        assert!(
            (panel.x..panel.right()).all(|x| buffer.cell((x, 11)).unwrap().bg == theme.surface),
            "revealing an effect does not replace the panel's own ground"
        );
        let mut solid = baseline.clone();
        let mut solid_engine = ThemeVisualEngine::default();
        solid_engine.sync(&theme, started);
        solid_engine.paint_text(
            started + WallDuration::from_millis(100),
            &mut solid,
            area,
            ThemeVisualSurfaces {
                editor_strength: 0.0,
                interface_strength: 0.0,
                interface: &[panel],
                protected: &[],
            },
        );
        assert_eq!(solid, baseline, "a solid interface remains untouched");
    }

    #[test]
    fn image_mode_emits_rgba_without_mutating_terminal_characters() {
        let theme = Theme::built_in("burn").expect("burn theme");
        assert_eq!(theme.tachyon_mode(), Some(TachyonMode::Image));
        let area = Rect::new(0, 0, 40, 12);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, Style::default().bg(theme.background));
        buffer.cell_mut((3, 3)).unwrap().set_symbol("x");
        let baseline = buffer.clone();
        let started = Instant::now();
        let mut engine = ThemeVisualEngine::default();
        engine.sync(&theme, started);

        let image = engine
            .image(started + WallDuration::from_millis(100), &buffer, area)
            .expect("image mode returns a backdrop frame");

        assert_eq!(image.width, area.width);
        assert_eq!(image.height, area.height);
        assert_eq!(image.rgba.len(), usize::from(area.width * area.height) * 4);
        assert!(
            image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] == 255)
        );
        assert_eq!(buffer, baseline, "the text frame stays available above it");
    }

    #[test]
    fn cyberpunk_vectrex_and_dvd_are_distinct_moving_scenes() {
        let area = Rect::new(0, 0, 120, 40);
        let background = Color::Rgb(7, 7, 19);
        let render = |scene: &dyn NativeScene, elapsed_ms| {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            scene.render(elapsed_ms, ReactiveAudio::default(), &mut buffer, area);
            buffer
        };

        let cyber_active = showroom_test_visual(CellEffect::Cyberpunk, background);
        let cyber = CyberpunkLandscape::new(&cyber_active);
        let cyber_first = render(&cyber, 700);
        let cyber_later = render(&cyber, 1_900);
        assert!(
            cyber_first.content.iter().any(|cell| cell.symbol() == "━"),
            "the landscape has a striped neon sun"
        );
        assert!(
            cyber_first
                .content
                .iter()
                .any(|cell| matches!(cell.symbol(), "┼" | "─" | "╱" | "╲")),
            "the landscape has mountains and a perspective grid"
        );
        assert_ne!(cyber_first, cyber_later, "the ground advances indefinitely");

        let vectrex_active = showroom_test_visual(CellEffect::Vectrex, background);
        let vectrex = VectrexFlight::new(&vectrex_active);
        let vectrex_first = render(&vectrex, 700);
        let vectrex_later = render(&vectrex, 1_900);
        assert!(
            vectrex_first
                .content
                .iter()
                .filter(|cell| cell.symbol() != " ")
                .count()
                > 80,
            "the vector flight fills the field with rails and depth rings"
        );
        assert_ne!(
            vectrex_first, vectrex_later,
            "the vector tunnel flies forward"
        );

        let dvd_active = showroom_test_visual(CellEffect::Dvd, background);
        let dvd = DvdBounce::new(&dvd_active);
        let dvd_first = render(&dvd, 0);
        let dvd_later = render(&dvd, 2_800);
        // Half the size it shipped at: the logo read as a wall rather than a
        // bouncing object on anything short of a very large terminal.
        assert_eq!(DvdBounce::art_dimensions(), (70, 17));
        assert!(
            DVD_VIDEO_ART
                .lines()
                .all(|line| line.bytes().all(|pixel| matches!(pixel, b' ' | b'.')))
        );
        assert!(
            dvd_first
                .content
                .iter()
                .filter(|cell| cell.symbol() == ".")
                .count()
                > 190,
            "the supplied DVD VIDEO dot art remains large after fitting"
        );
        let (quick_x, quick_y, _) = dvd.position(500, area);
        assert!(
            quick_x >= 10 && quick_y >= 4,
            "the logo crosses several cells between visible frames"
        );
        assert_ne!(dvd_first, dvd_later, "the DVD logo crosses the screen");
    }

    #[test]
    fn rustel_logo_asset_and_native_music_response_are_real_inputs() {
        assert_eq!(RUSTEL_LOGO.len(), RUSTEL_LOGO_HEIGHT * RUSTEL_LOGO_STRIDE);
        assert!(
            RUSTEL_LOGO
                .as_chunks::<RUSTEL_LOGO_STRIDE>()
                .0
                .iter()
                .all(|row| {
                    row[RUSTEL_LOGO_WIDTH] == b'\n'
                        && row[..RUSTEL_LOGO_WIDTH]
                            .iter()
                            .all(|pixel| matches!(pixel, b'.' | b'M' | b'C' | b'W'))
                })
        );

        let area = Rect::new(0, 0, 120, 40);
        let theme = Theme::built_in("rustel-live").expect("brand theme");
        let background = theme.background;
        let active = ActiveVisual {
            config: theme.cell_visual().expect("brand scene"),
            background,
            foreground: theme.foreground,
            accent: theme.accent,
            secondary: theme.syntax.number,
            muted: theme.muted,
        };
        let scene = RustelBrandScene::new(&active);
        let render = |audio| {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(background));
            scene.render(1_850, audio, &mut buffer, area);
            buffer
        };
        let silent = render(ReactiveAudio::default());
        let loud = render(ReactiveAudio {
            rms: 0.95,
            bass: 0.9,
            mid: 0.8,
            treble: 1.0,
        });
        assert_ne!(silent, loud, "the exact logo deforms with live audio bands");
        assert!(
            silent
                .content
                .iter()
                .any(|cell| cell.fg == Color::Rgb(0, 238, 251))
        );
        assert!(
            silent
                .content
                .iter()
                .any(|cell| cell.fg == Color::Rgb(242, 2, 247))
        );
    }

    #[test]
    fn every_built_in_cell_effect_moves_without_overwriting_protected_cells() {
        let area = Rect::new(0, 0, 120, 40);
        let background = Color::Rgb(7, 7, 19);
        let panel_background = Color::Rgb(24, 24, 52);
        let mut effects = Vec::new();
        for name in super::super::theme::Theme::built_in_names() {
            let Some(effect) = super::super::theme::Theme::built_in(name)
                .and_then(|theme| theme.cell_visual())
                .map(|visual| visual.effect)
            else {
                continue;
            };
            if effect != CellEffect::Matrix && !effects.contains(&effect) {
                effects.push(effect);
            }
        }

        for effect in effects {
            let mut baseline = Buffer::empty(area);
            baseline.set_style(area, Style::default().bg(background));
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if (x + y) % 4 == 0 {
                        baseline
                            .cell_mut((x, y))
                            .expect("text cell")
                            .set_symbol("x")
                            .set_fg(Color::Rgb(170, 180, 200));
                    }
                }
            }
            baseline
                .cell_mut((2, 2))
                .expect("panel cell")
                .set_symbol("P")
                .set_bg(panel_background);
            baseline
                .cell_mut((4, 2))
                .expect("wide cell")
                .set_symbol("界");
            baseline
                .cell_mut((5, 2))
                .expect("wide continuation")
                .set_symbol("");

            let active = showroom_test_visual(effect, background);
            let mut manager = EffectManager::<()>::default();
            manager.add_effect(theme_effect(&active, Arc::default(), Arc::default()));
            let mut changed = false;
            for elapsed in [90, 160, 450, 900, 1_600, 3_200] {
                let mut buffer = baseline.clone();
                manager.process_effects(Duration::from_millis(elapsed), &mut buffer, area);
                changed |= buffer != baseline;
                assert_eq!(buffer.cell((2, 2)), baseline.cell((2, 2)), "{effect:?}");
                assert_eq!(buffer.cell((4, 2)), baseline.cell((4, 2)), "{effect:?}");
                assert_eq!(buffer.cell((5, 2)), baseline.cell((5, 2)), "{effect:?}");
            }
            assert!(changed, "{effect:?} never changed the live buffer");
        }
    }
}

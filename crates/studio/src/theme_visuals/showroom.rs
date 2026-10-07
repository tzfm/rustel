//! The showroom: one shader for every cell effect a theme can ask for.
//! It works out what a frame knows, then hands each cell to the effect
//! that paints it; the effects live in the sibling modules.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct ShowroomFx {
    pub(super) elapsed_ms: u64,
    pub(super) audio: Arc<ReactiveAudioSignal>,
    pub(super) kind: CellEffect,
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) secondary: Color,
    pub(super) muted: Color,
    pub(super) density: u8,
    pub(super) speed: u8,
    pub(super) area: Option<Rect>,
    /// The leak's container. Idle for every other effect.
    pub(super) leak: LeakWater,
    /// The basketball court's throws so far. Idle for every other effect.
    pub(super) court: CourtMemo,
    /// How awake the bear is. Idle for every other effect.
    pub(super) bear: BearMemo,
    /// What the forest has felt of the music. Idle for every other effect.
    pub(super) forest: ForestMemo,
}

/// What one frame of the showroom knows before its cells are painted:
/// the pane, the clock, the theme's dials, the music, and the pictures the
/// scene effects drew for the frame.
pub(super) struct Frame {
    pub(super) area: Rect,
    pub(super) height: f32,
    pub(super) time: f32,
    pub(super) step: u64,
    pub(super) density: f32,
    pub(super) aspect: f32,
    pub(super) stroke: f32,
    pub(super) reactive: ReactiveAudio,
    pub(super) bubbles: Option<[BubbleFrame; BUBBLE_COUNT]>,
    pub(super) fumes: Option<[FumeFrame; FUME_COUNT]>,
    pub(super) smoke: Option<[SmokeFrame; SMOKE_COUNT]>,
    pub(super) leak: Option<(usize, f32)>,
    pub(super) space: Option<SceneFrame>,
    pub(super) campfire: Option<SceneFrame>,
    pub(super) hell: Option<SceneFrame>,
    pub(super) basketball: Option<SceneFrame>,
    pub(super) forest: Option<SceneFrame>,
    pub(super) bear: Option<SceneFrame>,
}

/// One cell of a frame: where it is, and whether it is blank or a
/// character the effect may recolour.
#[derive(Clone, Copy)]
pub(super) struct Spot {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) px: f32,
    pub(super) py: f32,
    pub(super) nx: f32,
    pub(super) ny: f32,
    pub(super) noise: u64,
    pub(super) blank: bool,
    pub(super) text: bool,
}

impl ShowroomFx {
    pub(super) fn new(active: &ActiveVisual, audio: Arc<ReactiveAudioSignal>) -> Self {
        Self {
            elapsed_ms: 0,
            audio,
            kind: active.config.effect,
            leak: LeakWater::default(),
            court: CourtMemo::default(),
            bear: BearMemo::default(),
            forest: ForestMemo::default(),
            background: active.background,
            foreground: active.foreground,
            accent: active.accent,
            secondary: active.secondary,
            muted: active.muted,
            density: active.config.density,
            speed: active.config.speed,
            area: None,
        }
    }

    pub(super) fn render(&self, buffer: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        let width = f32::from(area.width.max(1));
        let height = f32::from(area.height.max(1));
        let seconds = self.elapsed_ms as f32 / 1000.0;
        let time = seconds * (0.45 + f32::from(self.speed) / 80.0);
        let step = self.elapsed_ms / 90;
        let density = f32::from(self.density) / 100.0;
        let aspect = (width * 0.5 / height).clamp(0.55, 3.0);
        let stroke = (0.72 / height).max(0.007);
        let reactive = self.audio.load();
        let bubbles = (self.kind == CellEffect::Bubbles).then(|| bubble_frames(time, self.density));
        let fumes = (self.kind == CellEffect::Burn).then(|| fume_frames(time, self.density));
        let smoke = (self.kind == CellEffect::Smoke).then(|| smoke_frames(time, self.density));
        let leak = (self.kind == CellEffect::Leak).then(|| self.leak.breach());
        let space =
            (self.kind == CellEffect::Space).then(|| self.space_frame(area, time, reactive.rms));
        let campfire = (self.kind == CellEffect::Campfire)
            .then(|| self.campfire_frame(area, time, reactive.rms));
        let hell =
            (self.kind == CellEffect::Hell).then(|| self.hell_frame(area, time, reactive.rms));
        let basketball =
            (self.kind == CellEffect::Basketball).then(|| self.basketball_frame(area, time));
        let forest = (self.kind == CellEffect::Forest).then(|| self.forest_frame(area, time));
        let bear = (self.kind == CellEffect::Bear).then(|| self.bear_frame(area, time, reactive));
        let frame = Frame {
            area,
            height,
            time,
            step,
            density,
            aspect,
            stroke,
            reactive,
            bubbles,
            fumes,
            smoke,
            leak,
            space,
            campfire,
            hell,
            basketball,
            forest,
            bear,
        };

        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let Some(cell) = buffer.cell_mut((x, y)) else {
                    continue;
                };
                if cell.bg != self.background {
                    continue;
                }
                let symbol = cell.symbol();
                let blank = symbol == " ";
                let text = !blank && symbol.width() == 1;
                if !blank && !text {
                    continue;
                }
                let px = f32::from(x - area.x);
                let py = f32::from(y - area.y);
                let nx = px / width;
                let ny = py / height;
                let noise = hash(u64::from(x), u64::from(y), step);
                let spot = Spot {
                    x,
                    y,
                    px,
                    py,
                    nx,
                    ny,
                    noise,
                    blank,
                    text,
                };
                match self.kind {
                    CellEffect::Beams => self.paint_beams(&frame, &spot, cell),
                    CellEffect::BinaryPath => self.paint_binary_path(&frame, &spot, cell),
                    CellEffect::Blackhole => self.paint_blackhole(&frame, &spot, cell),
                    CellEffect::BouncyBalls => self.paint_bouncy_balls(&frame, &spot, cell),
                    CellEffect::Bubbles => self.paint_bubbles(&frame, &spot, cell),
                    CellEffect::Burn => self.paint_burn(&frame, &spot, cell),
                    CellEffect::ColorShift => self.paint_color_shift(&frame, &spot, cell),
                    CellEffect::Crumble => self.paint_crumble(&frame, &spot, cell),
                    CellEffect::Cyberpunk
                    | CellEffect::Vectrex
                    | CellEffect::Dvd
                    | CellEffect::RustelBrand
                    | CellEffect::Camera => {
                        unreachable!("dedicated scene effects are selected by the factory")
                    }
                    CellEffect::Decrypt => self.paint_decrypt(&frame, &spot, cell),
                    CellEffect::ErrorCorrect => self.paint_error_correct(&frame, &spot, cell),
                    CellEffect::Expand => self.paint_expand(&frame, &spot, cell),
                    CellEffect::Fireworks => self.paint_fireworks(&frame, &spot, cell),
                    CellEffect::Highlight => self.paint_highlight(&frame, &spot, cell),
                    CellEffect::LaserEtch => self.paint_laser_etch(&frame, &spot, cell),
                    CellEffect::Matrix => unreachable!("Matrix has its own shader"),
                    CellEffect::MiddleOut => self.paint_middle_out(&frame, &spot, cell),
                    CellEffect::OrbittingVolley => self.paint_orbitting_volley(&frame, &spot, cell),
                    CellEffect::Overflow => self.paint_overflow(&frame, &spot, cell),
                    CellEffect::Mode7 => self.paint_mode7(&frame, &spot, cell),
                    CellEffect::Basketball => self.paint_basketball(&frame, &spot, cell),
                    CellEffect::Forest => self.paint_forest(&frame, &spot, cell),
                    CellEffect::Bear => self.paint_bear(&frame, &spot, cell),
                    CellEffect::Hell => self.paint_hell(&frame, &spot, cell),
                    CellEffect::Campfire => self.paint_campfire(&frame, &spot, cell),
                    CellEffect::Space => self.paint_space(&frame, &spot, cell),
                    CellEffect::Leak => self.paint_leak(&frame, &spot, cell),
                    CellEffect::Print => self.paint_print(&frame, &spot, cell),
                    CellEffect::Rain => self.paint_rain(&frame, &spot, cell),
                    CellEffect::RandomSequence => self.paint_random_sequence(&frame, &spot, cell),
                    CellEffect::Rings => self.paint_rings(&frame, &spot, cell),
                    CellEffect::Scattered => self.paint_scattered(&frame, &spot, cell),
                    CellEffect::Slice => self.paint_slice(&frame, &spot, cell),
                    CellEffect::Slide => self.paint_slide(&frame, &spot, cell),
                    CellEffect::Smoke => self.paint_smoke(&frame, &spot, cell),
                    CellEffect::Spotlights => self.paint_spotlights(&frame, &spot, cell),
                    CellEffect::Snow => self.paint_snow(&frame, &spot, cell),
                    CellEffect::Swarm => self.paint_swarm(&frame, &spot, cell),
                    CellEffect::Sweep => self.paint_sweep(&frame, &spot, cell),
                    CellEffect::SynthGrid => self.paint_synth_grid(&frame, &spot, cell),
                    CellEffect::Thunderstorm => self.paint_thunderstorm(&frame, &spot, cell),
                    CellEffect::Unstable => self.paint_unstable(&frame, &spot, cell),
                    CellEffect::VhsTape => self.paint_vhs_tape(&frame, &spot, cell),
                    CellEffect::Waves | CellEffect::WavesReactive => {
                        self.paint_waves(&frame, &spot, cell)
                    }
                    CellEffect::Wipe => self.paint_wipe(&frame, &spot, cell),
                }
            }
        }
    }
}

impl Shader for ShowroomFx {
    fn name(&self) -> &'static str {
        "rustel_showroom"
    }

    fn process(&mut self, duration: Duration, buffer: &mut Buffer, area: Rect) -> Option<Duration> {
        self.elapsed_ms = self
            .elapsed_ms
            .wrapping_add(u64::from(duration.as_millis()));
        let area = self.area.unwrap_or(area).intersection(area);
        if self.kind == CellEffect::Leak {
            // Scaled by the same factor `time` uses, so the theme's speed
            // moves the physics and the cycle together.
            let dt = duration.as_secs_f32() * (0.45 + f32::from(self.speed) / 80.0);
            self.leak.advance(area, dt, self.density);
        }
        if self.kind == CellEffect::Basketball {
            let seconds = self.elapsed_ms as f32 / 1000.0;
            let time = seconds * (0.45 + f32::from(self.speed) / 80.0);
            self.extend_court(area, time);
        }
        if self.kind == CellEffect::Forest {
            let dt = duration.as_secs_f32() * (0.45 + f32::from(self.speed) / 80.0);
            self.forest.advance(dt, self.audio.load());
        }
        if self.kind == CellEffect::Bear {
            // The bear listens between frames: what he hears now, for how
            // long, is what wakes him or lets him sleep.
            let scale = 0.45 + f32::from(self.speed) / 80.0;
            let dt = duration.as_secs_f32() * scale;
            let time = self.elapsed_ms as f32 / 1000.0 * scale;
            let rms = self.audio.load().rms;
            self.bear.advance(dt, rms, time);
        }
        self.render(buffer, area);
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

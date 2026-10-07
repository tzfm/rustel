//! Musical grammars for Studio's live Generator.
//!
//! Each direction shares a key and motifs across its layers. Controls edit
//! that arrangement; each direction retains generations and their variation paths.

mod music;
use music::Music;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Direction {
    Glass,
    Bloom,
    Acid,
    Circuit,
    Trance,
    Scrub,
    Groove,
    Dub,
    Harmonic,
    Machinery,
}
impl Direction {
    pub const ALL: [Self; 10] = [
        Self::Glass,
        Self::Bloom,
        Self::Acid,
        Self::Circuit,
        Self::Trance,
        Self::Scrub,
        Self::Groove,
        Self::Dub,
        Self::Harmonic,
        Self::Machinery,
    ];
    pub fn controls(self) -> &'static [usize] {
        match self {
            Self::Glass => &[0, 1, 2, 3, VARIATION, DRUMS, DELAY],
            Self::Acid | Self::Circuit => &[0, 1, 2, 3, VARIATION, DRUMS],
            Self::Bloom | Self::Trance | Self::Groove => &[0, 1, 2, 3, VARIATION],
            Self::Scrub => &[0, 1, 2, 3, VARIATION, POSITION, FRAGMENT],
            Self::Dub => &[0, 1, 2, 3, VARIATION, LENGTH, DELAY],
            Self::Harmonic => &[0, 1, 2, 3, VARIATION, LENGTH],
            Self::Machinery => &[0, 1, 2, 3, VARIATION, DRUMS],
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Glass => "Glassy pulse",
            Self::Bloom => "Slow bloom",
            Self::Acid => "Acid current",
            Self::Circuit => "Broken circuit",
            Self::Trance => "Trance spark",
            Self::Scrub => "Scrub garden",
            Self::Groove => "Fractured groove",
            Self::Dub => "Dub drift",
            Self::Harmonic => "Harmonic bloom",
            Self::Machinery => "Odd machinery",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Generate,
    Similar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Row {
    Direction(Direction),
    Generate(Direction),
    Similar,
    Copy,
    ControlsTop,
    Control(usize),
    ControlsBottom,
}
impl Row {
    pub fn action(self) -> Option<Action> {
        match self {
            Self::Generate(_) => Some(Action::Generate),
            Self::Similar => Some(Action::Similar),
            _ => None,
        }
    }
    pub fn selectable(self) -> bool {
        !matches!(self, Self::ControlsTop | Self::ControlsBottom)
    }
}
pub const CONTROLS: [&str; 10] = [
    "Activity",
    "Tone",
    "Motion",
    "Space",
    "Variation",
    "Drums",
    "Delay",
    "Position",
    "Fragment",
    "Length",
];
const VARIATION: usize = 4;
const DRUMS: usize = 5;
const DELAY: usize = 6;
const POSITION: usize = 7;
const FRAGMENT: usize = 8;
const LENGTH: usize = 9;
const HISTORY_LIMIT: usize = 64;

#[derive(Clone, Debug, PartialEq)]
struct Idea {
    direction: Direction,
    root: i32,
    music: Music,
    ratio: f64,
    sweep: f64,
    phase: usize,
    source: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    idea: Idea,
    controls: [u8; CONTROLS.len()],
}

#[derive(Clone, Debug, PartialEq)]
struct History<T> {
    items: Vec<T>,
    at: usize,
}
impl<T> History<T> {
    fn new(first: T) -> Self {
        Self {
            items: vec![first],
            at: 0,
        }
    }
    fn current(&self) -> &T {
        &self.items[self.at]
    }
    fn current_mut(&mut self) -> &mut T {
        &mut self.items[self.at]
    }
    fn can_step(&self, forward: bool) -> bool {
        if forward {
            self.at + 1 < self.items.len()
        } else {
            self.at > 0
        }
    }
    fn step(&mut self, forward: bool) -> bool {
        if !self.can_step(forward) {
            return false;
        }
        if forward {
            self.at += 1;
        } else {
            self.at -= 1;
        }
        true
    }
    fn push(&mut self, next: T) {
        self.items.truncate(self.at + 1);
        self.items.push(next);
        if self.items.len() > HISTORY_LIMIT {
            self.items.remove(0);
        }
        self.at = self.items.len() - 1;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Generator {
    idea: Idea,
    rng: Rng,
    pub controls: [u8; CONTROLS.len()],
    pub open: bool,
    pub saved_row: usize,
    pub code: String,
    history: History<History<Snapshot>>,
    saved: std::collections::HashMap<Direction, History<History<Snapshot>>>,
}

impl Default for Generator {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self::seeded(time ^ SERIAL.fetch_add(1, Ordering::Relaxed))
    }
}

impl Generator {
    pub fn seeded(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let idea = Idea::new(Direction::Glass, &mut rng);
        let controls = Self::random_controls(&mut rng);
        let history = History::new(History::new(Snapshot {
            idea: idea.clone(),
            controls,
        }));
        let mut result = Self {
            idea,
            rng,
            controls,
            open: true,
            saved_row: 0,
            code: String::new(),
            history,
            saved: Default::default(),
        };
        result.render();
        result
    }
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for direction in Direction::ALL {
            rows.push(Row::Direction(direction));
            if self.open && direction == self.idea.direction {
                rows.extend([
                    Row::Generate(direction),
                    Row::Similar,
                    Row::Copy,
                    Row::ControlsTop,
                ]);
                rows.extend(direction.controls().iter().copied().map(Row::Control));
                rows.push(Row::ControlsBottom);
            }
        }
        rows
    }
    pub fn direction(&self) -> Direction {
        self.idea.direction
    }
    fn remember(&mut self) {
        *self.history.current_mut().current_mut() = Snapshot {
            idea: self.idea.clone(),
            controls: self.controls,
        };
    }
    fn restore(&mut self) {
        let snapshot = self.history.current().current();
        self.idea = snapshot.idea.clone();
        self.controls = snapshot.controls;
        self.render();
    }
    pub fn choose(&mut self, direction: Direction) -> bool {
        if direction == self.idea.direction {
            self.open = !self.open;
            return false;
        }
        self.remember();
        let history = self.saved.remove(&direction).unwrap_or_else(|| {
            History::new(History::new(Snapshot {
                idea: Idea::new(direction, &mut self.rng),
                controls: Self::random_controls(&mut self.rng),
            }))
        });
        self.saved.insert(
            self.direction(),
            std::mem::replace(&mut self.history, history),
        );
        self.open = true;
        self.restore();
        true
    }
    fn random_controls(rng: &mut Rng) -> [u8; CONTROLS.len()] {
        [
            25 + rng.index(61) as u8,
            20 + rng.index(66) as u8,
            15 + rng.index(71) as u8,
            10 + rng.index(61) as u8,
            rng.index(101) as u8,
            35 + rng.index(66) as u8,
            15 + rng.index(66) as u8,
            15 + rng.index(61) as u8,
            20 + rng.index(61) as u8,
            15 + rng.index(66) as u8,
        ]
    }
    pub fn fresh(&mut self) {
        self.remember();
        let idea = Idea::new(self.direction(), &mut self.rng);
        let mut controls = Self::random_controls(&mut self.rng);
        if self
            .direction()
            .controls()
            .iter()
            .all(|&index| controls[index] == self.controls[index])
        {
            controls[0] = 25 + (controls[0] - 24) % 61;
        }
        self.history.push(History::new(Snapshot { idea, controls }));
        self.restore();
    }
    pub fn similar(&mut self) {
        self.remember();
        // Keep the hook, tonal centre and source; develop its response and colour.
        let mut idea = self.idea.clone();
        idea.music.similar(&mut self.rng);
        let ratios = [0.5, 1.0, 1.5, 2.0, 3.0];
        let at = ratios
            .iter()
            .position(|ratio| *ratio == idea.ratio)
            .unwrap_or(2);
        let neighbour = if self.rng.index(2) == 0 {
            at.saturating_sub(1)
        } else {
            (at + 1).min(4)
        };
        idea.ratio = ratios[neighbour];
        idea.sweep = (idea.sweep + self.rng.unit() * 0.16 - 0.08).clamp(0.08, 0.8);
        self.history.current_mut().push(Snapshot {
            idea,
            controls: self.controls,
        });
        self.restore();
    }
    pub fn can_history(&self, action: Action, forward: bool) -> bool {
        match action {
            Action::Generate => self.history.can_step(forward),
            Action::Similar => self.history.current().can_step(forward),
        }
    }
    pub fn step_history(&mut self, action: Action, forward: bool) -> bool {
        self.remember();
        let changed = match action {
            Action::Generate => self.history.step(forward),
            Action::Similar => self.history.current_mut().step(forward),
        };
        if changed {
            self.restore();
        }
        changed
    }
    /// Keep exact history recall while the forward arrow can always explore.
    pub fn step_or_generate(&mut self, action: Action, forward: bool) -> bool {
        if self.step_history(action, forward) {
            return true;
        }
        if !forward {
            return false;
        }
        match action {
            Action::Generate => self.fresh(),
            Action::Similar => self.similar(),
        }
        true
    }

    pub fn set(&mut self, control: usize, value: u8) -> bool {
        let value = value.min(100);
        if self.controls[control] == value {
            return false;
        }
        self.controls[control] = value;
        self.remember();
        self.render();
        true
    }
    pub fn adjust(&mut self, control: usize, delta: i16) -> bool {
        self.set(
            control,
            (i16::from(self.controls[control]) + delta).clamp(0, 100) as u8,
        )
    }
    fn render(&mut self) {
        let mut idea = self.idea.clone();
        let variation = f64::from(self.controls[VARIATION]) / 100.0;
        // A deterministic fine variation: dragging away and back recovers exactly
        // the same result, without adding entries to either history.
        idea.music.vary(self.controls[VARIATION]);
        idea.ratio = (idea.ratio + (variation - 0.5) * 0.6).clamp(0.5, 4.0);
        idea.sweep = (idea.sweep + (variation - 0.5) * 0.16).clamp(0.04, 0.9);
        self.code = idea.render(self.controls);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn index(&mut self, len: usize) -> usize {
        (self.next() % len as u64) as usize
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

impl Idea {
    fn new(direction: Direction, rng: &mut Rng) -> Self {
        Self {
            direction,
            root: 36 + rng.index(12) as i32,
            music: Music::new(rng),
            ratio: [0.5, 1.0, 1.5, 2.0, 3.0][rng.index(5)],
            sweep: 0.2 + rng.unit() * 0.35,
            phase: rng.index(5),
            source: rng.index(60),
        }
    }
    fn render(&self, knobs: [u8; CONTROLS.len()]) -> String {
        let [
            activity,
            tone,
            motion,
            space,
            _,
            drums,
            echoes,
            position,
            fragment,
            length,
        ] = knobs.map(|v| f64::from(v) / 100.0);
        let pitch = |degree: i32, octave: i32| self.music.pitch(self.root, degree, octave);
        let root = self.root;
        let octave = if self.direction == Direction::Glass {
            2
        } else {
            1
        };
        let a = pitch(self.music.notes[0], octave);
        let b = pitch(self.music.notes[2], octave);
        let d = pitch(self.music.notes[4], octave);
        let e = pitch(self.music.notes[7], octave);
        let melody = self.music.phrase(root, octave, activity);
        let bass = self.music.bass(root, activity);
        let response = self.music.response;
        let warp_mode = ["asym", "spin", "bendp"][self.source % 3];
        let echo_time = [0.1875, 0.375, 0.28125, 0.25][self.source % 4];
        let articulation = [0.08, 0.12, 0.18, 0.24][self.source % 4];
        let fm_low = 0.35 + tone * 0.6;
        let fm_high = 1.4 + tone * 5.0;
        let ratio = self.ratio;
        let period = 7 + self.phase;
        let pan_lo = 0.5 - motion * 0.35;
        let pan_hi = 0.5 + motion * 0.35;
        let room = space * 0.65;
        let delay = if self.direction == Direction::Glass {
            echoes * 0.36
        } else {
            space * 0.36
        };
        let sweep = self.sweep;
        let cutoff = 500.0 + tone * 1700.0;
        let sweep_high = cutoff + motion * 1700.0;
        let wtdepth = 0.03 + motion * 0.4;
        let warp = 0.05 + tone * 0.25;
        match self.direction {
            Direction::Glass => {
                let hits = 4 + (activity * 12.0).round() as u32;
                format!(
                    r#"$: stack(
  note("{melody}").s("sine")
    .fm(sine.range({fm_low:.2}, {fm_high:.2}).slow({period}))
    .fmh("{ratio:.2} {ratio:.2} 2 {ratio:.2}")
    .attack(0.003).decay({articulation:.3}).sustain(0).release(0.18)
    .pan(sine.range({pan_lo:.2}, {pan_hi:.2}).slow(7)).gain(0.23),
  note("{bass}").s("basique").bank("wt_digital")
    .wt({sweep:.3}).wtrate(0.12).wtdepth({wtdepth:.3})
    .warp({warp:.3}).warpmode("{warp_mode}")
    .lpf(sine.range({cutoff:.0}, {sweep_high:.0}).slow(6))
    .attack(0.008).decay(0.18).sustain(0.2).release(0.12).gain(0.27),
  s("white*{hits}").hpf(7500)
    .attack(0.001).decay(0.025).sustain(0).release(0.02)
    .gain("0.025 0.055 0.02 0.04 0.025 0.07 0.02 0.045")
    .pan("{pan_lo:.2} {pan_hi:.2}").postgain({drums:.3})
).room({room:.3}).delay({delay:.3}).delaytime({echo_time:.4}).delayfeedback(0.3)"#
                )
            }
            Direction::Bloom => {
                let chords = self.music.chords(root, 1, true);
                let slow = if activity < 0.34 {
                    4
                } else if activity < 0.67 {
                    3
                } else {
                    2
                };
                format!(
                    r#"$: stack(
  note("{chords}").slow(2).s("basique").bank("wt_digital")
    .wt({sweep:.3}).wtrate(0.12).wtdepth({wtdepth:.3})
    .warp({warp:.3}).warpmode("{warp_mode}")
    .lpf(sine.range({cutoff:.0}, {sweep_high:.0}).slow(16))
    .attack(0.9).decay(0.5).sustain(0.6).release(1.8)
    .pan(sine.range({pan_lo:.2}, {pan_hi:.2}).slow({period})).gain(0.09),
  note("{melody}").slow({slow}).s("sine")
    .fm({fm_high:.2}).fmh({ratio:.2}).fmdecay(0.09)
    .attack(0.002).decay(0.16).sustain(0).release(0.12)
    .lpf({sweep_high:.0}).pan("{pan_lo:.2} {pan_hi:.2}").gain(0.2)
).room({room:.3}).delay({delay:.3}).delaytime(0.375).delayfeedback(0.3)"#
                )
            }
            Direction::Acid => {
                let lead = self.music.phrase(root, 0, activity);
                let acid_wave = ["sawtooth", "square"][self.source % 2];
                let envelope = 0.25 + tone * 0.6;
                let accent = 0.13 + motion * 0.18;
                let shift = self.phase as f64 / 8.0;
                format!(
                    r#"$: stack(
  note("{lead}").early({shift:.3}).s("{acid_wave}").acidenv({envelope:.3})
    .lpq(4).attack(.003).decay(.14).sustain(.15).release(.06)
    .gain(".22 {accent:.3} .16 .25").pan(sine.range({pan_lo:.2}, {pan_hi:.2}).slow({period})),
  note("{root}*4").s("sine").attack(.001).decay(.09).sustain(0).release(.02).gain(.25).postgain({drums:.3}),
  s("white*8").hpf(8000).attack(.001).decay(.018).sustain(0).release(.01)
    .gain(".025 .05 .02 .045").postgain({drums:.3})
).room({room:.3}).delay({delay:.3}).delaytime(.1875).delayfeedback(.25)"#
                )
            }
            Direction::Circuit => {
                let base = root + 24;
                let metallic = base + 19;
                let steps = [8, 12, 16][self.music.groove % 3];
                let hits = 3 + (activity * (steps - 3) as f64).round() as usize;
                format!(
                    r#"$: stack(
  note("{base} {metallic} {d} {e}").struct("x({hits},{steps})").s("sine")
    .fm({fm_high:.2}).fmh({ratio:.2}).fmdecay(.035)
    .attack(.001).decay(.065).sustain(0).release(.025).gain(.2)
    .pan(sine.range({pan_lo:.2}, {pan_hi:.2}).slow({period})).postgain({drums:.3}),
  note("{bass}").s("triangle")
    .lpf({cutoff:.0}).attack(.003).decay(.16).sustain(0).release(.08).gain(.25),
  s("pink*8").hpf(6200).attack(.001).decay(.025).sustain(0).release(.01)
    .gain("0 .04 .015 .025 0 .05 .02 .035").postgain({drums:.3})
).room({room:.3}).delay({delay:.3}).delaytime(.28125).delayfeedback(.3)"#
                )
            }
            Direction::Trance => {
                let notes = [0, 1, 2, 4, 6, 7].map(|i| pitch(self.music.notes[i], 2));
                let reset = 2 + self.phase % 4;
                let rhythm = if activity < 0.34 {
                    2
                } else if activity < 0.67 {
                    1
                } else {
                    0
                };
                let detune = 0.08 + motion * 0.2;
                format!(
                    r#"$: stack(
  trancearp({notes:?}, {reset}, {rhythm}).note().s("supersaw")
    .unison(3).detune({detune:.3}).spread(.65)
    .lpf(sine.range({cutoff:.0}, {sweep_high:.0}).slow({period}))
    .attack(.005).decay(.12).sustain(0).release(.09).gain(.14),
  note("{bass}").s("triangle")
    .attack(.005).decay(.16).sustain(.1).release(.08).gain(.24)
).room({room:.3}).delay({delay:.3}).delaytime(.1875).delayfeedback(.32)"#
                )
            }
            Direction::Scrub => {
                // A window through one of the five default Switch Angel pads.
                // Scrub supplies each fragment's position and playback direction;
                // end and clip bound its extent without sharing cut groups with a score.
                let sample = self.source % 5;
                let count = if activity < 0.34 {
                    4
                } else if activity < 0.67 {
                    8
                } else {
                    12
                };
                let window = 0.015 + fragment * 0.25;
                let centre = 0.03 + position * (0.9 - window);
                let spread = 0.02 + motion * 0.24;
                let mut starts = Vec::new();
                let mut ends = Vec::new();
                for step in 0..count {
                    let slot = if count == 4 {
                        [0, 2, 6, 7][step]
                    } else {
                        step % 8
                    };
                    let degree = self.music.notes[slot];
                    let begin =
                        (centre + (degree as f64 / 6.0 - 0.5) * spread).clamp(0.01, 0.98 - window);
                    let reverse = motion > 0.3 && (step + self.phase).is_multiple_of(4);
                    let semitones = pitch(degree, 0) - root;
                    let rate = 2.0_f64.powf(f64::from(semitones) / 12.0)
                        * if reverse { -1.0 } else { 1.0 };
                    starts.push(format!("{begin:.3}:{rate:.5}"));
                    ends.push(format!("{:.3}", begin + window));
                }
                let starts = starts.join(" ");
                let ends = ends.join(" ");
                let clip = 0.15 + fragment * 0.8;
                let low = 700.0 + tone * 3800.0;
                let high = low + motion * 1800.0;
                let drone_begin = (centre + self.sweep * 0.08).min(0.75);
                let drone_end = (drone_begin + 0.12 + fragment * 0.12).min(0.99);
                let follow = pitch(self.music.notes[7], 0);
                format!(
                    r#"$: stack(
  s("swpad").n({sample}).scrub("{starts}").end("{ends}")
    .note({root}).slow(2).clip({clip:.3})
    .attack(.008).release(.04).gain(.24)
    .lpf(sine.range({low:.0}, {high:.0}).slow({period}))
    .pan(sine.range({pan_lo:.2}, {pan_hi:.2}).slow(5)),
  s("swpad").n({sample}).scrub("{drone_begin:.3}:0.5").end({drone_end:.3})
    .note("<{root} {follow}>").slow(4).clip(.35)
    .attack(.09).release(.25).lpf({low:.0}).gain(.11)
    .pan(sine.range({pan_hi:.2}, {pan_lo:.2}).slow({period}))
).room({room:.3}).delay({delay:.3}).delaytime(.375).delayfeedback(.28)"#
                )
            }
            Direction::Groove => {
                // Real default drum banks; the same kit survives Similar.
                let kit = ["RolandTR909", "RolandTR808", "AkaiLinn", "BossDR110"][self.source % 4];
                let mut kick = ["~"; 16];
                let anchors: &[usize] = match self.music.groove {
                    0 => &[0, 4, 8, 12],
                    1 => &[0, 6, 10],
                    2 => &[0, 7, 14],
                    _ => &[0, 3, 10],
                };
                for &slot in anchors {
                    kick[slot] = "bd";
                }
                kick[[7, 11, 14, 15, 9, 10][response % 6]] = "bd";
                if activity > 0.34 {
                    kick[(self.phase * 2 + 5) % 16] = "bd";
                }
                if activity > 0.67 {
                    kick[14] = "[bd ~ bd]";
                }
                let kick = kick.join(" ");
                let mut snare = ["~"; 16];
                if self.music.groove == 2 {
                    snare[8] = "sd";
                } else {
                    snare[4] = "sd";
                    snare[12] = if activity > 0.8 { "[sd ~ sd]" } else { "sd" };
                }
                let snare = snare.join(" ");
                let mut ghosts = ["~"; 16];
                ghosts[[3, 7, 11, 15, 2, 10, 14][response % 7]] = "sd";
                if activity > 0.5 {
                    ghosts[(response * 2 + 1) % 16] = "cp";
                }
                let ghosts = ghosts.join(" ");
                let hats = if activity < 0.34 {
                    "hh ~ hh ~"
                } else if activity < 0.67 {
                    "hh hh ~ hh hh ~ hh hh"
                } else {
                    "hh hh [hh hh] hh hh ~ hh [hh hh hh]"
                };
                let swing = 0.02 + motion * 0.22;
                let brightness = 1200.0 + tone * 10000.0;
                let ghost_gain = 0.035 + activity * 0.085;
                format!(
                    r#"$: stack(
  s("{kick}").gain(.58),
  s("{snare}").gain(.34),
  s("{hats}").gain(".075 .12 .065 .1").pan("{pan_lo:.2} {pan_hi:.2}"),
  s("{ghosts}").gain({ghost_gain:.3}).pan({pan_hi:.2})
).bank("{kit}").swingBy({swing:.3}, 8).lpf({brightness:.0}).postgain(.4)
  .room({room:.3}).delay({delay:.3}).delaytime(.1875).delayfeedback(.22)"#
                )
            }
            Direction::Dub => {
                let chords = self.music.chords(root, 1, false);
                let rhythm = match (self.music.groove, activity < 0.34, activity > 0.67) {
                    (_, true, _) => "~ x ~ ~ ~ ~ x ~",
                    (0, _, false) => "~ x ~ x ~ ~ x ~",
                    (1, _, false) => "~ ~ x ~ ~ x ~ x",
                    (2, _, false) => "~ x ~ ~ x ~ ~ x",
                    (_, _, false) => "~ ~ x x ~ ~ x ~",
                    (0 | 2, _, true) => "~ x [~ x] x ~ x x [~ x]",
                    (_, _, true) => "~ [~ x] x ~ x [~ x] ~ x",
                };
                let shift = self.phase as f64 / 16.0;
                let clip = 0.07 + length * 0.58;
                let release = 0.04 + length * 0.3;
                let low = 350.0 + tone * 2400.0;
                let high = low + motion * 1300.0;
                let echo = echoes * 0.58;
                let feedback = 0.22 + motion * 0.35;
                let time = [0.1875, 0.375, 0.28125][self.source % 3];
                format!(
                    r#"$: stack(
  note("{chords}").struct("{rhythm}").early({shift:.4}).s("sawtooth")
    .clip({clip:.3}).attack(.004).decay(.12).sustain(.24).release({release:.3})
    .lpf(sine.range({low:.0}, {high:.0}).slow({period})).lpq(1.2)
    .pan("{pan_lo:.2} {pan_hi:.2}").gain(.13)
    .delay({echo:.3}).delaytime({time:.4}).delayfeedback({feedback:.3}),
  note("{bass}").s("triangle")
    .attack(.006).decay(.18).sustain(.12).release(.12).lpf(650).gain(.28)
).room({room:.3})"#
                )
            }
            Direction::Harmonic => {
                // Eight useful partials per voice, with a distinct, quieter answer.
                let spectra = [
                    [1.0, 0.1, 0.48, 0.05, 0.22, 0.0, 0.13, 0.02],
                    [1.0, 0.42, 0.05, 0.26, 0.0, 0.08, 0.0, 0.13],
                    [1.0, 0.06, 0.32, 0.48, 0.03, 0.0, 0.28, 0.09],
                    [1.0, 0.28, 0.2, 0.13, 0.1, 0.075, 0.05, 0.025],
                ];
                let spectrum = |which: usize| {
                    spectra[which % spectra.len()]
                        .iter()
                        .enumerate()
                        .map(|(i, magnitude)| {
                            let brightness = if i == 0 { 1.0 } else { 0.12 + tone * 1.8 };
                            format!("{:.3}", magnitude * brightness)
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let main = spectrum(self.source);
                let answer = spectrum(self.source + 1 + response % 3);
                let chords = self.music.chords(root, 1, false);
                let slow = if activity < 0.34 {
                    4
                } else if activity < 0.67 {
                    2
                } else {
                    1
                };
                let attack = 0.008 + length * 0.45;
                let release = 0.1 + length * 1.3;
                let clip = 0.12 + length * 0.6;
                let high = cutoff + 600.0 + motion * 2600.0;
                let shimmer = motion * 0.35;
                format!(
                    r#"$: stack(
  note("{chords}").slow({slow}).s("user")
    .partials([{main}]).clip({clip:.3})
    .attack({attack:.3}).decay(.3).sustain(.38).release({release:.3})
    .lpf(sine.range({cutoff:.0}, {high:.0}).slow({period})).phaser({shimmer:.3})
    .pan(sine.range({pan_lo:.2}, {pan_hi:.2}).slow(9)).gain(.1),
  note("{d} ~ ~ {e} ~ {b} ~ ~").slow(2).s("user")
    .partials([{answer}]).attack(.003).decay(.18).sustain(0).release({release:.3})
    .pan("{pan_hi:.2} {pan_lo:.2}").gain(.12)
).room({room:.3}).delay({delay:.3}).delaytime(.375).delayfeedback(.26)"#
                )
            }
            Direction::Machinery => {
                let sound = ["z_sine", "z_triangle", "z_sawtooth"][self.source % 3];
                let slide = 0.04 + motion * 0.28;
                let fall = -slide * 0.7;
                let curve = 0.6 + tone * 1.5;
                let modulation = self.ratio * motion * 3.0;
                let jump = motion * 38.0;
                let noise = 0.01 + tone * 0.12;
                let knocks = 3 + (activity * 4.0).round() as usize;
                format!(
                    r#"$: stack(
  note("{melody}").s("{sound}").zrand(0).curve({curve:.3})
    .slide("{slide:.3} {fall:.3} 0 {slide:.3}").zmod({modulation:.3})
    .pitchJump({jump:.2}).pitchJumpTime(.06)
    .clip(.55).attack(.003).decay(.07).sustain(.16).release(.05)
    .lpf({sweep_high:.0}).pan("{pan_lo:.2} {pan_hi:.2}").gain(.4),
  note("{bass}").s("z_triangle").zrand(0)
    .curve(1.2).slide({fall:.3}).clip(.6)
    .attack(.003).decay(.1).sustain(.2).release(.04).gain(.55),
  note("{a} {d} {b}").struct("x({knocks},8)").s("z_noise").zrand(0)
    .znoise({noise:.3}).slide(-.15).clip(.25)
    .attack(.001).decay(.06).sustain(0).release(.02)
    .gain(.6).postgain({drums:.3}).pan({pan_hi:.2})
).room({room:.3}).delay({delay:.3}).delaytime(.1875).delayfeedback(.24)"#
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn musical_events(session: &mut rustel_runtime::Session, code: &str) -> Vec<String> {
        session
            .evaluate(code)
            .unwrap_or_else(|error| panic!("{error}\n{code}"));
        let events = session.query(0.into(), 8.into()).unwrap();
        let mut notes: Vec<_> = events
            .iter()
            .map(|event| {
                let controls: Vec<_> = ["s", "note", "n", "speed", "begin"]
                    .map(|key| format!("{key}={:?}", event.value.get(key)))
                    .into();
                format!("{:?} {}", event.whole, controls.join(" "))
            })
            .collect();
        notes.sort();
        notes
    }

    #[test]
    fn generate_varies_composition_even_with_fixed_key_source_and_sliders() {
        let mut session = rustel_runtime::Session::new().unwrap();
        for direction in Direction::ALL {
            let mut compositions = std::collections::HashSet::new();
            for seed in 0..24 {
                let mut generator = Generator::seeded(seed);
                if direction != generator.direction() {
                    generator.choose(direction);
                }
                generator.idea.root = 36;
                generator.idea.source = 0;
                generator.idea.phase = 0;
                generator.controls = [50; CONTROLS.len()];
                generator.render();
                compositions.insert(musical_events(&mut session, &generator.code));
            }
            assert!(
                compositions.len() >= 18,
                "{direction:?}: only {} compositions",
                compositions.len()
            );
        }
    }

    #[test]
    fn similar_changes_played_music_and_preserves_source_key_and_layer_budget() {
        let mut session = rustel_runtime::Session::new().unwrap();
        for direction in Direction::ALL {
            for seed in [3, 17, 29] {
                let mut generator = Generator::seeded(seed);
                if direction != generator.direction() {
                    generator.choose(direction);
                }
                for &control in direction.controls() {
                    generator.set(control, 50);
                }
                let root = generator.idea.root;
                let source = generator.idea.source;
                let layers = generator.code.matches(".s(\"").count();
                let mut before = musical_events(&mut session, &generator.code);
                for _ in 0..12 {
                    generator.similar();
                    let after = musical_events(&mut session, &generator.code);
                    assert_ne!(
                        before, after,
                        "{direction:?}, seed {seed}: only cosmetic changes"
                    );
                    assert_eq!(generator.idea.root, root);
                    assert_eq!(generator.idea.source, source);
                    assert_eq!(generator.code.matches(".s(\"").count(), layers);
                    before = after;
                }
            }
        }
    }

    #[test]
    fn generator_is_reproducible_and_new_ideas_are_distinct() {
        let mut a = Generator::seeded(40);
        let mut b = Generator::seeded(40);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..128 {
            assert_eq!(a.code, b.code);
            assert!(seen.insert(a.code.clone()));
            assert!(!a.code.contains("//"));
            a.fresh();
            b.fresh();
        }
    }
    #[test]
    fn every_direction_retains_its_idea_controls_and_history() {
        let mut generator = Generator::seeded(91);
        let mut saved = Vec::new();
        for direction in Direction::ALL {
            if generator.direction() != direction {
                generator.choose(direction);
            }
            assert!(!generator.code.is_empty());
            generator.set(1, 73);
            generator.similar();
            saved.push((
                direction,
                generator.code.clone(),
                generator.history.clone(),
                generator.controls,
            ));
            let before = generator.code.clone();
            generator.choose(direction); // collapse
            generator.choose(direction); // reopen
            assert_eq!(generator.code, before);
        }
        for (direction, code, history, controls) in saved.into_iter().rev() {
            if generator.direction() != direction {
                generator.choose(direction);
            }
            assert_eq!(generator.code, code);
            assert_eq!(generator.history, history);
            assert_eq!(generator.controls, controls);
        }
    }

    #[test]
    fn generate_shuffles_controls_and_similar_preserves_them() {
        let mut generator = Generator::seeded(22);
        for _ in 0..64 {
            let before = generator.controls;
            generator.fresh();
            assert_ne!(generator.controls, before);
            let generated = generator.controls;
            generator.similar();
            assert_eq!(generator.controls, generated);
        }
    }

    #[test]
    fn similar_explores_motif_rhythm_and_sound_without_changing_key() {
        for direction in Direction::ALL {
            let mut generator = Generator::seeded(9);
            if generator.direction() != direction {
                generator.choose(direction);
            }
            let root = generator.idea.root;
            let source = generator.idea.source;
            for _ in 0..20 {
                let before = generator.idea.clone();
                let code = generator.code.clone();
                generator.similar();
                assert_eq!(generator.idea.root, root);
                assert_eq!(generator.idea.source, source);
                assert_eq!(generator.idea.music.notes[..4], before.music.notes[..4]);
                assert_ne!(generator.idea.music.notes[4..], before.music.notes[4..]);
                assert_eq!(generator.idea.phase, before.phase);
                assert_ne!(generator.code, code, "{direction:?}");
            }
        }
    }
    #[test]
    fn histories_restore_the_exact_code_and_controls_and_support_branching() {
        let mut generator = Generator::seeded(41);
        let first = generator.code.clone();
        generator.fresh();
        generator.similar();
        generator.set(DRUMS, 0);
        let nearby = generator.code.clone();
        generator.similar();
        let farther = generator.code.clone();
        assert!(generator.step_history(Action::Similar, false));
        assert_eq!(generator.code, nearby);
        assert_eq!(generator.controls[DRUMS], 0);
        assert!(generator.step_history(Action::Similar, true));
        assert_eq!(generator.code, farther);
        assert!(generator.step_history(Action::Generate, false));
        assert_eq!(generator.code, first);
        assert!(!generator.step_history(Action::Generate, false));
        assert!(generator.step_history(Action::Generate, true));
        assert_eq!(generator.code, farther);
        assert!(generator.step_history(Action::Similar, false));
        generator.similar(); // Start another path from the result we preferred.
        let branch = generator.code.clone();
        assert_ne!(branch, farther);
        assert!(!generator.can_history(Action::Similar, true));
        generator.choose(Direction::Circuit);
        generator.choose(Direction::Glass);
        assert_eq!(generator.code, branch);
        assert!(generator.step_history(Action::Similar, false));
        assert_eq!(generator.code, nearby);
    }

    #[test]
    fn variation_is_reversible_without_creating_history_entries() {
        for direction in Direction::ALL {
            let mut generator = Generator::seeded(2);
            if generator.direction() != direction {
                generator.choose(direction);
            }
            generator.set(VARIATION, 0);
            let before = generator.code.clone();
            generator.set(VARIATION, 100);
            assert_ne!(generator.code, before);
            generator.set(VARIATION, 0);
            assert_eq!(generator.code, before);
            assert!(!generator.can_history(Action::Similar, false));
            assert!(!generator.can_history(Action::Generate, false));
        }
    }

    #[test]
    fn percussion_can_be_muted_without_muting_the_musical_layers() {
        for direction in [
            Direction::Glass,
            Direction::Acid,
            Direction::Circuit,
            Direction::Machinery,
        ] {
            let mut generator = Generator::seeded(3);
            if generator.direction() != direction {
                generator.choose(direction);
            }
            for level in [0, 100] {
                generator.set(DRUMS, level);
                let mut session = rustel_runtime::Session::new().unwrap();
                session.evaluate(&generator.code).unwrap();
                let events = session.query(0.into(), 2.into()).unwrap();
                let mut percussion = 0;
                let mut melodic = 0;
                for event in events {
                    let sound = event
                        .value
                        .get("s")
                        .and_then(rustel_core::Value::as_str)
                        .unwrap();
                    let is_drum = sound == "white"
                        || sound == "pink"
                        || (sound == "sine" && direction != Direction::Glass)
                        || (sound == "z_noise" && direction == Direction::Machinery);
                    if is_drum {
                        percussion += 1;
                        assert_eq!(
                            event
                                .value
                                .get("postgain")
                                .and_then(rustel_core::Value::as_f64),
                            Some(f64::from(level) / 100.0)
                        );
                    } else {
                        melodic += 1;
                        assert!(event.value.get("postgain").is_none());
                    }
                }
                assert!(percussion > 0 && melodic > 0, "{direction:?}");
            }
        }
    }

    #[test]
    fn machinery_drums_have_audible_range_without_changing_the_tonal_layers() {
        use rustel_audio::{ScalarBackend, render_pcm};

        let rms = |pcm: &[f32]| {
            (pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt()
        };
        for seed in [3, 7, 29] {
            let mut generator = Generator::seeded(seed);
            generator.choose(Direction::Machinery);
            let mut tonal_reference = None;
            let mut drum_levels = Vec::new();
            for level in [0, 50, 100] {
                generator.set(DRUMS, level);
                let mut session = rustel_runtime::Session::new().unwrap();
                session.evaluate(&generator.code).unwrap();
                let report = session.play(4.0).unwrap();
                let (drums, tones): (Vec<_>, Vec<_>) = report
                    .onsets
                    .iter()
                    .partition(|onset| matches!(&onset.value, rustel_runtime::ValueJson::Raw(values) if values.get("postgain").is_some()));
                assert!(!drums.is_empty() && !tones.is_empty());
                let render = |onsets: Vec<&rustel_runtime::OnsetEventJson>| {
                    let events: Vec<_> = onsets
                        .into_iter()
                        .map(|onset| {
                            rustel_runtime::test_support::scalar_event(
                                onset,
                                24_000,
                                report.cps,
                                &RecipeSamples,
                            )
                            .unwrap()
                        })
                        .collect();
                    render_pcm(&mut ScalarBackend::new(), 24_000, 96_000, &events).unwrap()
                };
                let drum_pcm = render(drums);
                let tonal_pcm = render(tones);
                assert!(drum_pcm.iter().all(|s| s.is_finite() && s.abs() < 1.0));
                if level == 0 {
                    assert!(
                        drum_pcm.iter().all(|&s| s == 0.0),
                        "seed {seed}: muted drums"
                    );
                    tonal_reference = Some(tonal_pcm);
                } else {
                    assert_eq!(
                        Some(&tonal_pcm),
                        tonal_reference.as_ref(),
                        "Drums changes only percussion"
                    );
                }
                drum_levels.push(rms(&drum_pcm));
            }
            let tonal_rms = rms(tonal_reference.as_ref().unwrap());
            eprintln!("seed {seed}: drums={drum_levels:?}, tones={tonal_rms}");
            assert!(
                (drum_levels[1] / drum_levels[2] - 0.5).abs() < 0.01,
                "the middle of the slider must halve percussion amplitude"
            );
            assert!(
                drum_levels[2] >= tonal_rms * 0.25,
                "seed {seed}: full percussion must not be buried more than 12 dB below the tonal layers"
            );
        }
    }

    #[test]
    fn glass_delay_can_be_removed_independently_of_drums_and_room() {
        let mut generator = Generator::seeded(1);
        generator.set(DRUMS, 100);
        generator.set(3, 100);
        for (level, expected) in [(0, 0.0), (100, 0.36)] {
            generator.set(DELAY, level);
            let mut session = rustel_runtime::Session::new().unwrap();
            session.evaluate(&generator.code).unwrap();
            let events = session.query(0.into(), 1.into()).unwrap();
            assert!(!events.is_empty());
            for event in events {
                assert_eq!(
                    event
                        .value
                        .get("delay")
                        .and_then(rustel_core::Value::as_f64),
                    Some(expected)
                );
                assert_eq!(
                    event.value.get("room").and_then(rustel_core::Value::as_f64),
                    Some(0.65)
                );
            }
        }
    }

    #[test]
    fn every_control_changes_the_music_and_both_formats_evaluate() {
        for direction in Direction::ALL {
            for seed in 0..8 {
                let mut generator = Generator::seeded(seed);
                if generator.direction() != direction {
                    generator.choose(direction);
                }
                for &index in direction.controls() {
                    generator.set(index, 50);
                }
                let original = generator.code.clone();
                for &index in direction.controls() {
                    for value in [0, 100] {
                        generator.set(index, value);
                        assert_ne!(generator.code, original, "{direction:?} control {index}");
                        for code in [
                            super::super::examples::laned(&generator.code),
                            super::super::examples::stacked(&generator.code),
                        ] {
                            let mut session = rustel_runtime::Session::new().unwrap();
                            session
                                .evaluate(&code)
                                .unwrap_or_else(|error| panic!("{error}\n{code}"));
                            let events = session
                                .query(
                                    rustel_fraction::Fraction::from(0),
                                    rustel_fraction::Fraction::from(4),
                                )
                                .unwrap();
                            assert!(!events.is_empty());
                        }
                    }
                    generator.set(index, 50);
                }
            }
        }
    }
    #[test]
    fn generated_native_layers_render_finite_audible_audio() {
        for direction in Direction::ALL
            .into_iter()
            .filter(|direction| !matches!(direction, Direction::Scrub | Direction::Groove))
        {
            let mut generator = Generator::seeded(7);
            if generator.direction() != direction {
                generator.choose(direction);
            }
            let mut session = rustel_runtime::Session::new().unwrap();
            session.evaluate(&generator.code).unwrap();
            let pcm = session.render_pcm(0.5).unwrap();
            assert!(pcm.iter().all(|sample| sample.is_finite()));
            assert!(pcm.iter().any(|sample| sample.abs() > 0.0001));
        }
    }

    /// These fixtures exercise real sampler controls without networking in CI.
    /// Production names are deliberately enumerated, rather than accepting any
    /// unknown sound and silently masking a typo as an audible sample.
    struct RecipeSamples;
    impl rustel_voice::SampleLookup for RecipeSamples {
        fn resolve(&self, s: &str, n: f64, midi: f64) -> rustel_voice::SampleResolution {
            let pad = s == "swpad" && (0.0..5.0).contains(&n);
            let drum = ["RolandTR909", "RolandTR808", "AkaiLinn", "BossDR110"]
                .iter()
                .any(|kit| {
                    ["bd", "sd", "hh", "cp"]
                        .iter()
                        .any(|voice| s.eq_ignore_ascii_case(&format!("{kit}_{voice}")))
                })
                && n == 0.0;
            if s == "wt_digital_basique" && n == 0.0 {
                return rustel_voice::SampleResolution::Found {
                    id: rustel_audio::SampleId(2),
                    transpose: 0.0,
                    duration_secs: 4096.0 / 24_000.0,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    soundfont: false,
                };
            }
            if pad || drum {
                rustel_voice::SampleResolution::Found {
                    id: rustel_audio::SampleId(1),
                    transpose: midi - 36.0,
                    duration_secs: 2.0,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    soundfont: false,
                }
            } else {
                rustel_voice::SampleResolution::Unknown
            }
        }
    }

    #[test]
    fn all_directions_resolve_every_voice_and_render_bounded_audio_at_control_extremes() {
        use rustel_audio::{DecodedSample, ScalarBackend};
        for direction in Direction::ALL {
            for seed in [7, 29] {
                for level in [0, 50, 100] {
                    let mut generator = Generator::seeded(seed);
                    if direction != generator.direction() {
                        generator.choose(direction);
                    }
                    if seed == 29 {
                        generator.similar();
                    }
                    for &control in direction.controls() {
                        generator.set(control, level);
                    }
                    let mut session = rustel_runtime::Session::new().unwrap();
                    session.evaluate(&generator.code).unwrap();
                    let report = session.play(4.0).unwrap();
                    assert!(!report.onsets.is_empty(), "{direction:?}");
                    let events: Vec<_> = report
                        .onsets
                        .iter()
                        .map(|onset| {
                            rustel_runtime::test_support::scalar_event(
                                onset,
                                24_000,
                                report.cps,
                                &RecipeSamples,
                            )
                            .unwrap_or_else(|error| {
                                panic!("{direction:?} {level}: {error}\n{}", generator.code)
                            })
                        })
                        .collect();
                    // A quiet, sustained test source makes even a late/reversed
                    // window audible, and exercises the sampler rather than a synth fallback.
                    let samples = (0..48_000)
                        .map(|frame| {
                            let phase = frame as f32 * std::f32::consts::TAU * 220.0 / 24_000.0;
                            0.25 * phase.sin() + 0.07 * (phase * 3.0).sin()
                        })
                        .collect();
                    let sample = DecodedSample::from_parts(24_000, 1, samples).unwrap();
                    let mut backend = ScalarBackend::new();
                    assert!(
                        backend
                            .install_sample(rustel_audio::SampleId(1), Box::new(sample))
                            .is_ok()
                    );
                    let table = (0..4096)
                        .map(|i| {
                            let phase = (i % 2048) as f32 * std::f32::consts::TAU / 2048.0;
                            0.4 * phase.sin()
                                + if i < 2048 {
                                    0.0
                                } else {
                                    0.15 * (phase * 3.0).sin()
                                }
                        })
                        .collect();
                    assert!(
                        backend
                            .install_sample(
                                rustel_audio::SampleId(2),
                                Box::new(DecodedSample::from_parts(24_000, 1, table).unwrap())
                            )
                            .is_ok()
                    );
                    let pcm =
                        rustel_audio::render_pcm(&mut backend, 24_000, 96_000, &events).unwrap();
                    assert!(
                        pcm.iter().all(|sample| sample.is_finite()),
                        "{direction:?} {level}"
                    );
                    let peak = pcm
                        .iter()
                        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                    assert!(
                        peak > 0.0001 && peak < 1.0,
                        "{direction:?} {level}: peak {peak}"
                    );
                    assert_eq!(backend.missing_sample_events(), 0, "{direction:?}");
                }
            }
        }
    }

    #[test]
    fn sample_windows_and_additive_spectra_stay_bounded_across_variations() {
        for direction in [Direction::Scrub, Direction::Harmonic, Direction::Machinery] {
            let mut generator = Generator::seeded(33);
            generator.choose(direction);
            for _ in 0..6 {
                generator.similar();
                let mut session = rustel_runtime::Session::new().unwrap();
                session.evaluate(&generator.code).unwrap();
                let events = session.query(0.into(), 8.into()).unwrap();
                assert!(!events.is_empty());
                for event in events {
                    let n = |name| event.value.get(name).and_then(rustel_core::Value::as_f64);
                    match direction {
                        Direction::Scrub => {
                            let begin = n("begin").unwrap();
                            let end = n("end").unwrap();
                            assert!(begin >= 0.0 && begin < end && end <= 1.0);
                            assert!(n("speed").unwrap().abs() > 0.1);
                        }
                        Direction::Harmonic => {
                            let Some(rustel_core::Value::List(partials)) =
                                event.value.get("partials")
                            else {
                                panic!("missing spectrum: {:?}", event.value);
                            };
                            assert_eq!(partials.len(), 8);
                            assert_eq!(partials[0].as_f64(), Some(1.0));
                        }
                        Direction::Machinery => assert_eq!(n("zrand"), Some(0.0)),
                        _ => unreachable!(),
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "uses installed default sample banks; set RUSTEL_GENERATOR_AUDITION_DIR for WAV/score output"]
    fn export_generator_comparisons_with_real_default_banks() {
        let output = std::path::PathBuf::from(
            std::env::var_os("RUSTEL_GENERATOR_AUDITION_DIR")
                .expect("set RUSTEL_GENERATOR_AUDITION_DIR to an audition output folder"),
        );
        std::fs::create_dir_all(&output).unwrap();
        for direction in Direction::ALL {
            let mut generator = Generator::seeded(17);
            if generator.direction() != direction {
                generator.choose(direction);
            }
            for variant in ["original", "similar"] {
                if variant == "similar" {
                    generator.similar();
                }
                let library = std::sync::Arc::new(
                    rustel_runtime::samples::SampleLibrary::load_default().unwrap(),
                );
                let mut session = rustel_runtime::Session::new().unwrap();
                session.set_direct_diagnostic_logging(false);
                session.set_sample_library_for_test(library.clone());
                session.evaluate(&generator.code).unwrap();
                session.prefetch_samples(4.0, std::time::Duration::from_secs(30));
                let report = session.play(8.0).unwrap();
                for onset in &report.onsets {
                    rustel_runtime::test_support::scalar_event(
                        onset,
                        48_000,
                        report.cps,
                        library.as_ref(),
                    )
                    .unwrap_or_else(|error| panic!("{}: {error}", direction.name()));
                }
                let name = format!(
                    "{}-{variant}",
                    direction.name().to_lowercase().replace(' ', "-")
                );
                std::fs::write(output.join(format!("{name}.strudel")), &generator.code).unwrap();
                let wav = output.join(format!("{name}.wav"));
                // Render once, then validate the exact artifact. A fresh library per
                // render owns its decoded samples through that render's backend.
                session
                    .render(8.0, &wav, rustel_runtime::RenderFormat::ScalarF32Wav)
                    .unwrap();
                let decoded = rustel_audio::decode_wav(&std::fs::read(wav).unwrap()).unwrap();
                let pcm = decoded.pcm();
                let peak = pcm
                    .iter()
                    .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                assert_eq!(decoded.channels(), 2);
                assert_eq!(decoded.frames(), decoded.sample_rate() as usize * 8);
                assert!(pcm.iter().all(|sample| sample.is_finite()));
                assert!(
                    peak > 0.0001 && peak < 1.0,
                    "{}: peak {peak}",
                    direction.name()
                );
                assert!(
                    session
                        .take_diagnostics()
                        .iter()
                        .all(|diagnostic| diagnostic.kind != "voice-refused")
                );
                println!("{name}: {} onsets, peak {peak:.4}", report.onsets.len());
            }
        }
    }
}

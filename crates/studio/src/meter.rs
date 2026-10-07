//! The master dock: a mixing-desk fader and level meter for the output bus.
//!
//! This is an engine control. Moving the fader multiplies the final mix on its
//! way to the device; it never edits, re-evaluates or otherwise touches the
//! score. The meter is post-fader and post-limiter, so its peak shows what
//! leaves the machine. The hot input warning is measured before the limiter.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use rustel_audio::{MasterLevels, SILENCE_LUFS, db_to_linear, linear_to_db};

use super::theme::{Theme, mix};

/// Loudest level the fader can reach, in dB.
pub const MAX_GAIN_DB: f32 = 6.0;
/// Quietest fader position before it snaps to silence.
pub const MIN_GAIN_DB: f32 = -60.0;
/// Bottom of the meter scale.
const FLOOR_DB: f32 = -60.0;
/// Top of the meter scale.
const CEILING_DB: f32 = 6.0;
/// Where the scale changes resolution. A mixing meter spends most of its
/// travel on the top twenty decibels, where mix decisions actually happen.
const KNEE_DB: f32 = -20.0;
/// Fraction of the scale given to everything below the knee.
const KNEE_POSITION: f32 = 0.4;
/// How long a peak marker stays before it starts falling.
const PEAK_HOLD: Duration = Duration::from_millis(1_200);
/// Fall rate of the peak marker once the hold expires.
const PEAK_FALL_DB_PER_SECOND: f32 = 24.0;
/// How long the clip indicator stays lit after the last full-scale sample.
const CLIP_HOLD: Duration = Duration::from_millis(2_000);

/// Position of a level on the meter scale, 0 at the floor and 1 at the top.
pub fn scale_position(db: f32) -> f32 {
    if !db.is_finite() {
        return 0.0;
    }
    if db <= FLOOR_DB {
        return 0.0;
    }
    if db >= CEILING_DB {
        return 1.0;
    }
    if db <= KNEE_DB {
        (db - FLOOR_DB) / (KNEE_DB - FLOOR_DB) * KNEE_POSITION
    } else {
        KNEE_POSITION + (db - KNEE_DB) / (CEILING_DB - KNEE_DB) * (1.0 - KNEE_POSITION)
    }
}

/// Inverse of [`scale_position`], for turning a click into a level.
pub fn scale_decibels(position: f32) -> f32 {
    let position = position.clamp(0.0, 1.0);
    if position <= KNEE_POSITION {
        FLOOR_DB + position / KNEE_POSITION * (KNEE_DB - FLOOR_DB)
    } else {
        KNEE_DB + (position - KNEE_POSITION) / (1.0 - KNEE_POSITION) * (CEILING_DB - KNEE_DB)
    }
}

/// UI-side fader position and meter ballistics.
#[derive(Clone, Debug)]
pub struct MasterState {
    gain_db: f32,
    peak_db: f32,
    hold_db: f32,
    hold_until: Instant,
    last_advanced: Instant,
    lufs: f32,
    clipped_blocks: u64,
    clip_until: Option<Instant>,
    /// How far the limiter is pulling the output down, in dB, never
    /// negative. Zero is a limiter doing nothing, or none at all.
    reduction_db: f32,
    /// Silence at the output, with the fader left where it stands.
    muted: bool,
}

impl MasterState {
    pub fn new(now: Instant) -> Self {
        Self {
            gain_db: 0.0,
            muted: false,
            peak_db: FLOOR_DB,
            hold_db: FLOOR_DB,
            hold_until: now,
            last_advanced: now,
            lufs: SILENCE_LUFS,
            clipped_blocks: 0,
            clip_until: None,
            reduction_db: 0.0,
        }
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Linear gain for the engine.
    pub fn gain(&self) -> f32 {
        if self.muted || self.gain_db <= MIN_GAIN_DB {
            0.0
        } else {
            db_to_linear(self.gain_db)
        }
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Mute or unmute, reporting whether anything changed.
    ///
    /// `gain_db` is untouched: the fader stays where the hand left it, the
    /// readout goes on saying the level the set is played at, and unmuting
    /// is the level coming back rather than a number being guessed at.
    pub fn set_muted(&mut self, muted: bool) -> bool {
        let changed = self.muted != muted;
        self.muted = muted;
        changed
    }

    /// How much the limiter is taking off, in dB and never negative, so a
    /// meter can draw it without deciding a sign.
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db
    }

    pub fn peak_db(&self) -> f32 {
        self.peak_db
    }

    pub fn hold_db(&self) -> f32 {
        self.hold_db
    }

    pub fn lufs(&self) -> f32 {
        self.lufs
    }

    pub fn clipping(&self, now: Instant) -> bool {
        self.clip_until.is_some_and(|until| now < until)
    }

    /// Move the fader to an absolute level, clamped to the fader's range.
    /// Returns true when the position actually changed.
    pub fn set_gain_db(&mut self, db: f32) -> bool {
        let clamped = if db.is_finite() {
            db.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
        } else {
            0.0
        };
        // Quantise to a tenth of a decibel so a drag produces stable
        // readouts and does not republish the gain on every pixel of travel.
        let clamped = (clamped * 10.0).round() / 10.0;
        let changed = (clamped - self.gain_db).abs() > f32::EPSILON;
        self.gain_db = clamped;
        changed
    }

    pub fn nudge_gain_db(&mut self, delta: f32) -> bool {
        self.set_gain_db(self.gain_db + delta)
    }

    /// Fold in a new engine reading and advance the meter's ballistics.
    /// TEST. Put a reduction on the state without an engine to make one.
    #[cfg(test)]
    pub(super) fn set_reduction_for_test(&mut self, reduction_db: f32) {
        self.reduction_db = reduction_db;
    }

    pub fn observe(&mut self, levels: MasterLevels, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_advanced)
            .as_secs_f32();
        self.last_advanced = now;
        self.peak_db = linear_to_db(levels.peak).max(FLOOR_DB);
        // A reduction of 1 is no reduction, so its dB is 0; the reading is
        // in (0, 1], and anything outside that is read as "did nothing"
        // rather than as an enormous amount of work.
        self.reduction_db = if levels.reduction.is_finite() && levels.reduction > 0.0 {
            (-linear_to_db(levels.reduction.min(1.0))).max(0.0)
        } else {
            0.0
        };
        self.lufs = if levels.lufs.is_finite() {
            levels.lufs
        } else {
            SILENCE_LUFS
        };
        if self.peak_db >= self.hold_db {
            self.hold_db = self.peak_db;
            self.hold_until = now + PEAK_HOLD;
        } else if now >= self.hold_until {
            self.hold_db = (self.hold_db - PEAK_FALL_DB_PER_SECOND * elapsed).max(FLOOR_DB);
        }
        if levels.clipped_blocks > self.clipped_blocks {
            self.clipped_blocks = levels.clipped_blocks;
            self.clip_until = Some(now + CLIP_HOLD);
        }
    }

    /// Forget level history. Called when playback stops so a stale peak does
    /// not sit lit above a silent bus.
    pub fn silence(&mut self, now: Instant) {
        self.peak_db = FLOOR_DB;
        self.hold_db = FLOOR_DB;
        self.hold_until = now;
        self.lufs = SILENCE_LUFS;
        self.clip_until = None;
    }
}

/// The master dock: a horizontal level meter with the fader riding on it,
/// and a second row of readouts. It lives at the bottom right of the
/// footer, beside the master scope.
pub struct MasterDock<'a> {
    pub keybinds: &'a super::keybinds::Keybinds,
    pub state: &'a MasterState,
    pub theme: &'a Theme,
    pub now: Instant,
    pub playing: bool,
    /// The master limiter's character, or `None` while it is off.
    ///
    /// The reduction gauge appears only while the limiter reduces the
    /// gain. At rest the row shows `lim off` or `lim <character>` in its
    /// place, when there is room.
    pub limiter: Option<&'static str>,
}

/// Columns kept for the gain readout to the right of the bar.
const GAIN_READOUT_WIDTH: u16 = 8;

impl MasterDock<'_> {
    /// What the limiter is holding back, on the row under the master's
    /// own bar, as a half-height rule.
    ///
    /// A terminal row cannot be made shorter, so the glyph is: `▁` sits on
    /// the floor of its cell and reads as a slim gauge rather than a
    /// second meter competing with the one above it. It grows leftwards
    /// from its reading as the limiter works, fills `span` at the point
    /// past which the number is the thing to read anyway, and is not
    /// drawn at all when nothing is being held.
    fn draw_reduction(&self, span: Rect, buffer: &mut Buffer) {
        if span.width < 5 {
            return;
        }
        let reduction = self.state.reduction_db();
        if !self.playing || reduction <= 0.1 {
            // Nothing held back is still worth a word, because a limiter
            // has two ways of holding back nothing and they are not the
            // same: off, and on and idle. The gauge says neither, so at
            // rest the state goes here instead, in the muted colour of
            // something that is not happening.
            let resting = match self.limiter {
                Some(character) => format!("lim {character}"),
                None => "lim off".to_owned(),
            };
            let width = resting.chars().count() as u16;
            if width <= span.width {
                buffer.set_stringn(
                    span.right() - width,
                    span.y,
                    &resting,
                    usize::from(width),
                    Style::default().fg(self.theme.muted),
                );
            }
            return;
        }
        let reading = format!("-{reduction:.1}");
        let reading_width = reading.chars().count() as u16;
        let style = Style::default().fg(self.theme.meter.peak);
        buffer.set_stringn(
            span.right().saturating_sub(reading_width),
            span.y,
            &reading,
            usize::from(reading_width),
            style,
        );
        let rail = span.width.saturating_sub(reading_width + 1);
        let full = (reduction / super::viz_panel::REDUCTION_FULL_DB).clamp(0.0, 1.0);
        let lit = (full * f32::from(rail)).round() as u16;
        for column in rail.saturating_sub(lit)..rail {
            if let Some(cell) = buffer.cell_mut((span.x + column, span.y)) {
                cell.set_symbol(super::terminal::symbol("\u{2581}"))
                    .set_style(style);
            }
        }
    }

    /// The draggable bar inside `area`: its first row, minus the readout.
    pub fn geometry(area: Rect) -> Option<Rect> {
        (area.width >= 20 && area.height >= 1)
            .then(|| {
                Rect::new(
                    area.x,
                    area.y,
                    area.width.saturating_sub(GAIN_READOUT_WIDTH),
                    1,
                )
            })
            .filter(|bar| bar.width >= 8)
    }

    /// Level a pointer at `x` is asking for.
    pub fn decibels_at(bar: Rect, x: u16) -> f32 {
        Self::decibels_within(bar, x, 0.0)
    }

    /// The same, told how far ALONG the column the pointer actually was.
    ///
    /// `subcell` is a fraction in `[0, 1)`, nought at the column's left
    /// edge. A terminal that reports pixels knows it; one that reports
    /// cells passes nought and every press means what it did.
    pub fn decibels_within(bar: Rect, x: u16, subcell: f64) -> f32 {
        let clamped = x.clamp(bar.x, bar.right().saturating_sub(1));
        let travel = f32::from(bar.width.saturating_sub(1)).max(1.0);
        let along = (f32::from(clamped - bar.x) + subcell as f32).clamp(0.0, travel);
        let db = scale_decibels(along / travel);
        // A detent at unity: a fader that lands at +0.3 or −1.3 but never
        // 0.0 cannot be set to the one value everyone wants. The band is at
        // least half a column of the above-knee range, so no bar is too
        // narrow to land on 0 dB.
        let step = (CEILING_DB - KNEE_DB) / ((1.0 - KNEE_POSITION) * travel);
        if db.abs() <= (step * 0.5).max(0.75) {
            0.0
        } else {
            db
        }
    }
}

impl Widget for MasterDock<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some(bar) = MasterDock::geometry(area) else {
            return;
        };
        let clipping = self.state.clipping(self.now);
        let travel = f32::from(bar.width.saturating_sub(1)).max(1.0);
        let column_of = |db: f32| (scale_position(db) * travel).round() as u16;
        let level_columns = if self.playing && self.state.peak_db() > FLOOR_DB {
            scale_position(self.state.peak_db()) * travel
        } else {
            -1.0
        };
        let hold_column = (self.playing && self.state.hold_db() > FLOOR_DB)
            .then(|| column_of(self.state.hold_db()));
        let fader_column = column_of(self.state.gain_db());
        let unity_column = column_of(0.0);
        for column in 0..bar.width {
            let db = scale_decibels(f32::from(column) / travel);
            let color = level_color(db, self.theme);
            // The unlit trough keeps a faint tint of the colour it will
            // become, so the danger zone is visible before it is reached.
            // Muted, the rail loses the tint of the colour it would have
            // become: the bar reads as a thing switched off rather than a
            // thing waiting for signal.
            let trough = if self.state.muted() {
                mix(self.theme.meter.track, self.theme.muted, 0.35)
            } else {
                mix(self.theme.meter.track, color, 0.22)
            };
            let lit = f32::from(column) <= level_columns;
            let style = Style::default().bg(if lit { color } else { trough });
            let (symbol, style) = if column == fader_column {
                // The cap is a block of half its own colour over the
                // ground it covers, the same handle as the desk's strips,
                // so the two read as the same control. The blend keeps the
                // lit or unlit meter visible behind the cap.
                //
                // One cell, and only the meter's own row. The desk's caps
                // stand a column past their rail either side because a
                // vertical rail has room beside it. This rail is the meter
                // itself, so a wider cap would blur the position and a
                // taller cap would cover the line of text under it.
                //
                // Muted at the floor: the fader is at zero there, and a cap
                // in its working colour would look like one lit tick.
                let cap = if self.state.muted() || self.state.gain_db() <= MIN_GAIN_DB {
                    self.theme.muted
                } else {
                    self.theme.meter.fader
                };
                let behind = if lit { color } else { trough };
                (
                    " ",
                    Style::default().bg(mix(behind, cap, super::theme::FADER_HANDLE_OPACITY)),
                )
            } else if hold_column == Some(column) {
                (
                    super::terminal::symbol("▏"),
                    style.fg(self.theme.meter.peak),
                )
            } else if column == unity_column {
                ("·", style.fg(self.theme.rule))
            } else {
                (" ", style)
            };
            if let Some(cell) = buffer.cell_mut((bar.x + column, bar.y)) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
        buffer.set_stringn(
            bar.right(),
            bar.y,
            format!(
                "{:>width$}",
                format_gain(self.state.gain_db()),
                width = usize::from(GAIN_READOUT_WIDTH)
            ),
            usize::from(area.right().saturating_sub(bar.right())),
            Style::default().fg(self.theme.meter.fader),
        );
        if area.height < 2 {
            return;
        }
        // A clip still outranks it: a clip is a thing that just happened
        // and stops mattering, a mute is a state the player put the desk
        // in and will wonder about until it is lifted.
        let volume_keys = [
            super::keybinds::BindAction::MasterUp,
            super::keybinds::BindAction::MasterDown,
        ]
        .into_iter()
        .filter_map(|action| self.keybinds.binding(action))
        .map(|binding| binding.hint())
        .collect::<Vec<_>>()
        .join("/");
        let readouts = format!(
            "{}  {}",
            format_level(self.state.hold_db(), self.playing),
            format_lufs(self.state.lufs(), self.playing)
        );
        let readouts_width = readouts.chars().count() as u16;
        let title_room = area.width.saturating_sub(readouts_width).saturating_sub(1);
        let master_title = if volume_keys.is_empty() {
            "MASTER".to_owned()
        } else {
            format!("MASTER {volume_keys}")
        };
        let pre_limiter_hot = clipping && self.limiter.is_some();
        let title = if pre_limiter_hot {
            // The warning measures the limiter input. The peak measures its output.
            "PRE-LIM HOT"
        } else if clipping {
            "CLIP"
        } else if self.state.muted() {
            "MUTED"
        } else if self.limiter.is_none() && master_title.chars().count() <= usize::from(title_room)
        {
            // The same active defaults and rebindings as Settings and dispatch.
            &master_title
        } else {
            // An active limiter needs the middle of this row for its gain
            // reduction gauge, so keep the ordinary compact title.
            "MASTER"
        };
        // Omit loudness when the warning and peak need the available width.
        let readouts = if pre_limiter_hot
            && title.chars().count() + 1 + usize::from(readouts_width) > usize::from(area.width)
        {
            format_level(self.state.hold_db(), self.playing)
        } else {
            readouts
        };
        let readouts_width = readouts.chars().count() as u16;
        buffer.set_stringn(
            area.x,
            area.y + 1,
            title,
            usize::from(area.width),
            Style::default()
                .fg(if pre_limiter_hot {
                    self.theme.warn
                } else if clipping {
                    self.theme.meter.peak
                } else if self.state.muted() {
                    // Warn, not error: nothing has gone wrong, and the
                    // only way out is the key that got here.
                    self.theme.warn
                } else {
                    self.theme.accent
                })
                .add_modifier(Modifier::BOLD),
        );
        buffer.set_stringn(
            area.right().saturating_sub(readouts_width),
            area.y + 1,
            readouts,
            usize::from(readouts_width.min(area.width)),
            Style::default().fg(if clipping && !pre_limiter_hot {
                self.theme.meter.peak
            } else {
                self.theme.muted
            }),
        );
        // Between the two, where the row is empty: the title has the left
        // and the readouts have the right, so the limiter takes what is
        // left and can never write over either.
        let from = area.x + title.chars().count() as u16 + 2;
        let to = area.right().saturating_sub(readouts_width + 1);
        if to > from {
            self.draw_reduction(Rect::new(from, area.y + 1, to - from, 1), buffer);
        }
    }
}

pub(super) fn level_color(db: f32, theme: &Theme) -> Color {
    if db >= -1.0 {
        theme.meter.peak
    } else if db >= -6.0 {
        theme.meter.high
    } else if db >= -18.0 {
        theme.meter.mid
    } else {
        theme.meter.low
    }
}

fn format_gain(db: f32) -> String {
    if db <= MIN_GAIN_DB {
        "-inf".to_owned()
    } else if db.abs() < 0.05 {
        "0.0dB".to_owned()
    } else {
        format!("{db:+.1}dB")
    }
}

fn format_level(db: f32, playing: bool) -> String {
    if !playing || db <= FLOOR_DB {
        "pk  --".to_owned()
    } else {
        format!("pk{db:>5.1}")
    }
}

fn format_lufs(lufs: f32, playing: bool) -> String {
    if !playing || lufs <= SILENCE_LUFS {
        "LUFS --".to_owned()
    } else {
        format!("{lufs:>5.1} LUFS")
    }
}

#[cfg(test)]
mod tests {
    /// A muted master meters silence, so its bar is empty, like the bar of
    /// a stopped score. The word is the only signal that it is muted.
    #[test]
    fn a_muted_master_says_so_where_it_would_say_master() {
        use ratatui::widgets::Widget;
        let theme = super::super::theme::Theme::default();
        let mut master = super::MasterState::new(std::time::Instant::now());
        let draw = |state: &super::MasterState| {
            let area = ratatui::layout::Rect::new(0, 0, 40, 2);
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            super::MasterDock {
                keybinds: &crate::keybinds::Keybinds::default(),
                state,
                theme: &theme,
                now: std::time::Instant::now(),
                playing: true,
                limiter: None,
            }
            .render(area, &mut buffer);
            buffer
                .content()
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        };

        assert!(draw(&master).contains("MASTER"));
        assert!(master.set_muted(true));
        let muted = draw(&master);
        assert!(muted.contains("MUTED"), "{muted}");
        assert!(!muted.contains("MASTER"), "{muted}");
        assert!(master.set_muted(false));
        assert!(draw(&master).contains("MASTER"));
    }

    #[test]
    fn the_clip_warning_names_the_pre_limiter_signal_when_limited() {
        use ratatui::widgets::Widget;
        let now = std::time::Instant::now();
        let mut master = super::MasterState::new(now);
        master.observe(
            rustel_audio::MasterLevels {
                peak: 0.9999,
                clipped_blocks: 1,
                ..Default::default()
            },
            now,
        );
        let theme = super::super::theme::Theme::default();
        let draw = |limiter| {
            let area = ratatui::layout::Rect::new(0, 0, 40, 2);
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            super::MasterDock {
                keybinds: &crate::keybinds::Keybinds::default(),
                state: &master,
                theme: &theme,
                now,
                playing: true,
                limiter,
            }
            .render(area, &mut buffer);
            buffer
                .content()
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        };

        let limited = draw(Some("punch"));
        assert!(limited.contains("PRE-LIM HOT"), "{limited}");
        assert!(limited.contains("pk -0.0"), "{limited}");
        let unlimited = draw(None);
        assert!(unlimited.contains("CLIP"), "{unlimited}");
        assert!(!unlimited.contains("PRE-LIM HOT"), "{unlimited}");
    }

    /// Mute is a flag, not a level: the output goes silent and the fader
    /// stays where the hand left it, so unmuting brings back the level the
    /// set is played at rather than one that had to be guessed.
    #[test]
    fn a_mute_silences_the_output_and_keeps_the_faders_level() {
        let mut master = super::MasterState::new(std::time::Instant::now());
        assert!(master.set_gain_db(-6.0));
        let heard = master.gain();
        assert!(heard > 0.0);

        assert!(master.set_muted(true), "the flag moved");
        assert!(master.muted());
        assert_eq!(master.gain(), 0.0, "silence at the output");
        assert!(
            (master.gain_db() - -6.0).abs() < 0.05,
            "and the fader has not moved: {}",
            master.gain_db()
        );

        assert!(
            !master.set_muted(true),
            "muting a muted desk changes nothing"
        );
        assert!(master.set_muted(false));
        assert!(
            (master.gain() - heard).abs() < f32::EPSILON,
            "the level comes back exactly"
        );
    }

    #[test]
    fn the_fader_reaches_true_silence_at_the_far_left() {
        use super::*;
        // A drag to the bar's first cell means the floor, and the floor
        // means zero gain, not a low level.
        let bar = ratatui::layout::Rect::new(10, 0, 20, 1);
        let db = MasterDock::decibels_at(bar, bar.x);
        assert!(db <= MIN_GAIN_DB, "far left is the floor, got {db}");
        let mut state = MasterState::new(std::time::Instant::now());
        state.set_gain_db(db);
        assert_eq!(state.gain(), 0.0, "the floor is silence");
        assert_eq!(format_gain(state.gain_db()), "-inf");
    }

    use super::*;

    #[test]
    fn the_scale_gives_most_of_its_travel_to_the_top_twenty_decibels() {
        assert_eq!(scale_position(-60.0), 0.0);
        assert_eq!(scale_position(6.0), 1.0);
        assert!((scale_position(-20.0) - KNEE_POSITION).abs() < 1e-6);
        assert!(
            scale_position(0.0) - scale_position(-20.0) > scale_position(-20.0),
            "the loud end must be the roomy end"
        );
    }

    #[test]
    fn scale_position_and_decibels_are_inverses() {
        for db in [-60.0, -48.0, -20.0, -12.0, -6.0, 0.0, 6.0] {
            let round_trip = scale_decibels(scale_position(db));
            assert!((round_trip - db).abs() < 0.01, "{db} became {round_trip}");
        }
    }

    #[test]
    fn a_drag_can_land_exactly_on_unity() {
        // The detent: some column of any plausible bar means exactly 0 dB,
        // the default - a fader that only offers +0.3 or −1.3 cannot be
        // put back where it started.
        for width in [12_u16, 20, 40, 80] {
            let bar = ratatui::layout::Rect::new(0, 0, width, 1);
            let exact = (bar.x..bar.right())
                .map(|x| MasterDock::decibels_at(bar, x))
                .filter(|db| *db == 0.0)
                .count();
            assert!(exact >= 1, "no column means 0 dB on a {width}-wide bar");
        }
    }

    #[test]
    fn the_fader_is_clamped_quantised_and_reaches_silence() {
        let mut state = MasterState::new(Instant::now());
        assert!(state.set_gain_db(-3.04));
        assert_eq!(state.gain_db(), -3.0);
        assert!(
            !state.set_gain_db(-3.01),
            "a tenth of a decibel is one step"
        );

        state.set_gain_db(99.0);
        assert_eq!(state.gain_db(), MAX_GAIN_DB);
        state.set_gain_db(-400.0);
        assert_eq!(state.gain_db(), MIN_GAIN_DB);
        assert_eq!(state.gain(), 0.0, "the bottom of the fader is silence");

        state.set_gain_db(f32::NAN);
        assert_eq!(state.gain_db(), 0.0);
        assert!((state.gain() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_peak_marker_holds_then_falls() {
        let started = Instant::now();
        let mut state = MasterState::new(started);
        state.observe(
            MasterLevels {
                peak: 1.0,
                lufs: -8.0,
                clipped_blocks: 1,
                reduction: 1.0,
            },
            started,
        );
        assert!((state.peak_db() - 0.0).abs() < 0.01);
        assert!(state.clipping(started));

        // Silence right after: the bar drops immediately, the marker holds.
        let held = started + Duration::from_millis(600);
        state.observe(MasterLevels::default(), held);
        assert_eq!(state.peak_db(), FLOOR_DB);
        assert!((state.hold_db() - 0.0).abs() < 0.01);

        let fallen = started + Duration::from_millis(1_700);
        state.observe(MasterLevels::default(), fallen);
        assert!(
            state.hold_db() < -5.0 && state.hold_db() > FLOOR_DB,
            "the marker should be falling, not gone: {}",
            state.hold_db()
        );
        assert!(!state.clipping(started + Duration::from_secs(3)));
    }

    #[test]
    fn stopping_clears_the_meter_but_not_the_fader() {
        let now = Instant::now();
        let mut state = MasterState::new(now);
        state.set_gain_db(-6.0);
        state.observe(
            MasterLevels {
                peak: 0.5,
                lufs: -12.0,
                clipped_blocks: 0,
                reduction: 1.0,
            },
            now,
        );
        state.silence(now);
        assert_eq!(state.peak_db(), FLOOR_DB);
        assert_eq!(state.lufs(), SILENCE_LUFS);
        assert_eq!(state.gain_db(), -6.0);
    }

    #[test]
    fn the_dock_maps_columns_to_levels_left_to_right() {
        let area = Rect::new(10, 5, 40, 2);
        let bar = MasterDock::geometry(area).expect("dock bar");
        assert_eq!(bar.y, 5);
        assert_eq!(bar.x, 10);
        assert!((MasterDock::decibels_at(bar, bar.x) - FLOOR_DB).abs() < 0.01);
        assert!((MasterDock::decibels_at(bar, bar.right() - 1) - CEILING_DB).abs() < 0.01);
        // A pointer outside the bar still resolves, clamped to its ends.
        assert!((MasterDock::decibels_at(bar, 0) - FLOOR_DB).abs() < 0.01);
        assert!(MasterDock::geometry(Rect::new(0, 0, 12, 1)).is_none());
    }

    #[test]
    fn the_dock_draws_the_fader_the_hold_marker_and_the_readouts() {
        let theme = Theme::built_in_default();
        let now = Instant::now();
        let mut state = MasterState::new(now);
        state.set_gain_db(-12.0);
        state.observe(
            MasterLevels {
                peak: 0.5,
                lufs: -14.0,
                clipped_blocks: 0,
                reduction: 1.0,
            },
            now,
        );
        let area = Rect::new(0, 0, 44, 2);
        let mut buffer = Buffer::empty(area);
        MasterDock {
            keybinds: &crate::keybinds::Keybinds::default(),
            state: &state,
            theme: &theme,
            now,
            playing: true,
            limiter: Some("clean"),
        }
        .render(area, &mut buffer);
        let row = |y: u16| {
            (0..area.width)
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        // The cap is a background blend of the meter colour and the cap
        // colour, not a glyph. It is the one cell on the meter's row with
        // a background that no other cell has.
        let bar = MasterDock::geometry(area).expect("the dock has a bar");
        let bg = |x: u16, y: u16| buffer.cell((x, y)).unwrap().style().bg;
        let capped: Vec<u16> = (bar.x..bar.right())
            .filter(|x| {
                let here = bg(*x, bar.y);
                here != Some(theme.meter.fader)
                    && (bar.x..bar.right())
                        .filter(|other| bg(*other, bar.y) == here)
                        .count()
                        == 1
            })
            .collect();
        assert_eq!(capped.len(), 1, "one cell, no wider than its meter");
        assert!(
            bg(capped[0], bar.y) != Some(theme.meter.fader),
            "a solid cap would have hidden the meter under it"
        );
        assert!(
            area.height < 2 || bg(capped[0], bar.y + 1) != bg(capped[0], bar.y),
            "and it does not bleed onto the line of text below"
        );
        assert!(!row(0).contains('◆'), "the diamond is gone: {}", row(0));
        assert!(row(0).contains('▏'), "{}", row(0));
        assert!(row(0).contains("-12.0dB"), "{}", row(0));
        assert!(row(1).contains("MASTER"), "{}", row(1));
        assert!(row(1).contains("-14.0 LUFS"), "{}", row(1));
        assert!(row(1).contains("pk -6.0"), "{}", row(1));
    }

    #[test]
    fn readouts_stay_short_and_say_when_nothing_is_playing() {
        assert_eq!(format_gain(0.0), "0.0dB");
        assert_eq!(format_gain(-6.0), "-6.0dB");
        assert_eq!(format_gain(MIN_GAIN_DB), "-inf");
        assert_eq!(format_level(-12.0, false), "pk  --");
        assert_eq!(format_lufs(-14.0, false), "LUFS --");
        assert_eq!(format_lufs(-14.0, true), "-14.0 LUFS");
    }

    #[test]
    fn the_hot_warning_and_peak_keep_their_text_and_colors_on_narrow_docks() {
        let now = Instant::now();
        let mut state = MasterState::new(now);
        state.observe(
            MasterLevels {
                peak: 0.9999,
                lufs: -12.0,
                clipped_blocks: 1,
                reduction: 1.0,
            },
            now,
        );
        let theme = Theme::built_in_default();
        for width in [28, 40] {
            for limiter in [Some("punch"), None] {
                let area = Rect::new(0, 0, width, 2);
                let mut buffer = Buffer::empty(area);
                MasterDock {
                    keybinds: &crate::keybinds::Keybinds::default(),
                    state: &state,
                    theme: &theme,
                    now,
                    playing: true,
                    limiter,
                }
                .render(area, &mut buffer);
                let row = (0..width)
                    .map(|x| buffer.cell((x, 1)).unwrap().symbol())
                    .collect::<String>();
                let limited = limiter.is_some();
                let warning = if limited { "PRE-LIM HOT" } else { "CLIP" };
                assert!(row.starts_with(warning), "{width} columns: {row}");
                assert!(row.contains("pk -0.0"), "{width} columns: {row}");
                if limited && width == 28 {
                    assert!(!row.contains("LUFS"), "{row}");
                } else {
                    assert!(row.contains("-12.0 LUFS"), "{row}");
                }

                let warning_color = if limited {
                    theme.warn
                } else {
                    theme.meter.peak
                };
                for x in 0..warning.len() as u16 {
                    assert_eq!(buffer.cell((x, 1)).unwrap().fg, warning_color);
                }
                let peak = row.find("pk -0.0").expect("peak readout") as u16;
                let peak_color = if limited {
                    theme.muted
                } else {
                    theme.meter.peak
                };
                for x in peak..peak + "pk -0.0".len() as u16 {
                    assert_eq!(buffer.cell((x, 1)).unwrap().fg, peak_color);
                }
            }
        }
    }
}

#[cfg(test)]
mod reduction_row_tests {
    use super::*;
    use ratatui::layout::Rect;

    fn drawn(state: &MasterState, playing: bool) -> Vec<String> {
        let area = Rect::new(0, 0, 40, 2);
        let mut buffer = Buffer::empty(area);
        let theme = Theme::built_in_default();
        MasterDock {
            keybinds: &crate::keybinds::Keybinds::default(),
            state,
            theme: &theme,
            now: Instant::now(),
            playing,
            limiter: Some("clean"),
        }
        .render(area, &mut buffer);
        (0..area.height)
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                    .collect()
            })
            .collect()
    }

    /// The limiter is visible in the footer, on a row of its own.
    ///
    /// `MasterState` carried the reduction all along and the dock never
    /// drew it, so on the one row a player watches while performing, a
    /// limiter pulling six decibels off looked exactly like one doing
    /// nothing. It goes under the master's bar rather than on it: a half-
    /// height glyph so it reads as a slim gauge and not as a second meter
    /// competing with the one above.
    #[test]
    fn the_footer_shows_what_the_limiter_is_holding_back() {
        let mut state = MasterState::new(Instant::now());
        // Nothing held: the row stays empty, so a limiter doing nothing
        // costs no ink at all.
        let quiet = drawn(&state, true);
        assert!(!quiet[1].contains('\u{2581}'), "{quiet:?}");

        state.set_reduction_for_test(6.0);
        let working = drawn(&state, true);
        assert!(
            working[1].contains('\u{2581}'),
            "the rule is drawn: {working:?}"
        );
        assert!(working[1].contains("-6.0"), "and read: {working:?}");
        // It is the row under the bar, and the bar is untouched: the two
        // are different facts and must not be mistaken for each other.
        assert!(!working[0].contains('\u{2581}'), "{working:?}");
        // And it wrote over neither of the row's own tenants.
        assert!(working[1].starts_with("MASTER"), "{working:?}");
        assert!(working[1].contains("LUFS"), "{working:?}");

        // More reduction reaches further back, and still writes over
        // neither neighbour.
        let six = working[1].matches('\u{2581}').count();
        state.set_reduction_for_test(12.0);
        let more = drawn(&state, true);
        let twelve = more[1].matches('\u{2581}').count();
        assert!(
            twelve > six,
            "twelve reaches further than six: {twelve} {six}"
        );
        assert!(more[1].starts_with("MASTER"), "{more:?}");
        assert!(more[1].contains("LUFS"), "{more:?}");
        assert!(more[1].contains("-12.0"), "{more:?}");

        // Stopped, there is nothing being held and nothing drawn.
        let stopped = drawn(&state, false);
        assert!(!stopped[1].contains('\u{2581}'), "{stopped:?}");
    }

    #[test]
    fn the_limiter_gauge_has_a_conhost_safe_rail() {
        let _symbols = super::super::terminal::ForceSymbolsForTest::set(false);
        let mut state = MasterState::new(Instant::now());
        state.set_reduction_for_test(6.0);
        let working = drawn(&state, true);
        assert!(
            working[1].contains('_'),
            "the fallback rail is visible: {working:?}"
        );
        assert!(!working[1].contains('\u{2581}'), "{working:?}");
    }

    #[test]
    fn master_volume_hint_tracks_effective_bindings() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds, Reach};
        use ratatui::widgets::Widget;
        let theme = super::super::theme::Theme::default();
        let master = super::MasterState::new(std::time::Instant::now());
        let draw = |keybinds: &Keybinds, width| {
            let area = ratatui::layout::Rect::new(0, 0, width, 2);
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            super::MasterDock {
                keybinds,
                state: &master,
                theme: &theme,
                now: std::time::Instant::now(),
                playing: false,
                limiter: None,
            }
            .render(area, &mut buffer);
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let mut bindings = Keybinds::default();
        bindings.set_reach(Reach {
            enhanced: true,
            terminal: "Windows Terminal".into(),
        });
        let wide = draw(&bindings, 80);
        assert!(wide.contains(&bindings.hint(BindAction::MasterUp)));
        let narrow = draw(&bindings, 28);
        assert!(narrow.contains("MASTER"), "{narrow}");
        assert!(
            !narrow.contains(&bindings.hint(BindAction::MasterUp)),
            "a narrow dock omits rather than clips its shortcut: {narrow}"
        );
        assert!(
            narrow.contains("pk  --"),
            "the readout stays intact: {narrow}"
        );
        bindings.learn(BindAction::MasterUp, KeyCombo::parse("f2"));
        bindings.learn(BindAction::MasterDown, KeyCombo::parse("f3"));
        assert!(draw(&bindings, 80).contains("MASTER F2/F3"));
        bindings.unbind(BindAction::MasterUp);
        bindings.unbind(BindAction::MasterDown);
        assert!(!draw(&bindings, 80).contains("F2"));
        assert!(!draw(&bindings, 80).contains("F3"));
    }
}

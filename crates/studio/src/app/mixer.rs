//! The mixer desk (F4 / Ctrl+Shift+M) and the master output level. You'll find
//! the strips for the limiter, master, orbits and input, and the fader gains
//! sent to the audio worker. It also covers the desk's keys, clicks, drags and
//! wheel, the score faders shown on the desk, and the limiter's slot, bypass
//! and character. Master mute and nudge are here too, along with restoring the
//! master and input levels a set saved.

use super::*;

/// Which strip of the mixer the keys drive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MixerTarget {
    Input,
    Orbit(u8),
    Master,
    /// The master limiter on the output. Its fader is a ceiling in dBFS
    /// rather than a gain, so it never rides the gain queue with the others.
    Limiter,
}

impl MixerTarget {
    /// Whether this strip's column is something you set.
    ///
    /// An orbit's level is the score's - `gain()` and `postgain()` on the
    /// line - so it is a meter and there is nothing for a wheel or a drag
    /// to turn. Everything else has a fader, the limiter's being its
    /// ceiling. Asked here rather than spelled out at each hit test, so
    /// the panel and the dock cannot disagree about which strips take the
    /// wheel.
    pub(super) const fn has_fader(self) -> bool {
        !matches!(self, Self::Orbit(_))
    }
}

/// The mixer's faders, and the strip under the keys. The faders are the
/// engine's: they scale an orbit or the input on its way to the mix and
/// never touch what the score evaluates to, so they survive every
/// evaluation. The input's is remembered between launches - a microphone
/// that needs a boost needs it every time - the orbits' are a set's
/// business and start at unity.
pub(super) struct MixerState {
    pub(super) selected: MixerTarget,
    pub(super) orbit_gain_db: [f32; rustel_audio::MAX_ORBITS],
    pub(super) input_gain_db: f32,
    /// Retire each accepted command separately: the worker queue holds only
    /// two commands, so replaying the accepted prefix can starve later gains.
    pub(super) input_gain_pending: bool,
    pub(super) orbit_gain_pending: [bool; rustel_audio::MAX_ORBITS],
}

/// How far a fader may go: an orbit a little above unity, the input far
/// enough to bring a phone's microphone up to the music - a quiet one
/// needs forty decibels and more.
const ORBIT_FADER_DB: std::ops::RangeInclusive<f32> = -60.0..=12.0;
const INPUT_FADER_DB: std::ops::RangeInclusive<f32> = -60.0..=48.0;

/// The master's fader ends where the footer's scale does.
const MASTER_FADER_CEILING_DB: f32 = 6.0;

/// One press of a fader key.
pub(super) const FADER_KEY_STEP_DB: f32 = 1.0;

impl MixerState {
    pub(super) fn new(input_gain_tenths_db: Option<i32>) -> Self {
        Self {
            selected: MixerTarget::Master,
            orbit_gain_db: [0.0; rustel_audio::MAX_ORBITS],
            input_gain_db: input_gain_tenths_db.map_or(0.0, |tenths| {
                (tenths as f32 / 10.0).clamp(*INPUT_FADER_DB.start(), *INPUT_FADER_DB.end())
            }),
            input_gain_pending: true,
            orbit_gain_pending: [false; rustel_audio::MAX_ORBITS],
        }
    }

    pub(super) fn set_orbit_gain_db(&mut self, orbit: usize, db: f32) {
        if let Some(slot) = self.orbit_gain_db.get_mut(orbit)
            && *slot != db
        {
            *slot = db;
            self.orbit_gain_pending[orbit] = true;
        }
    }

    /// Keep unsent targets current, including a return to unity. A full
    /// queue leaves just the unaccepted targets for the next UI poll.
    pub(super) fn send_pending(&mut self, mut send: impl FnMut(MixerTarget, f32) -> bool) {
        if self.input_gain_pending {
            if !send(MixerTarget::Input, fader_gain(self.input_gain_db)) {
                return;
            }
            self.input_gain_pending = false;
        }
        for (orbit, pending) in self.orbit_gain_pending.iter_mut().enumerate() {
            if *pending {
                if !send(
                    MixerTarget::Orbit(orbit as u8),
                    fader_gain(self.orbit_gain_db[orbit]),
                ) {
                    return;
                }
                *pending = false;
            }
        }
    }
}

/// The orbits a score's text names: every number inside an `.orbit(…)`
/// or its short form `.o(…)` - a literal or a pattern of them - and
/// orbit 1, the one every line lands on unless it says otherwise,
/// whenever there is a line at all. Read off the text rather than the
/// evaluation so a strip is there as the line is typed; comments are
/// skipped, since a line commented out is not a line.
pub(super) fn orbits_in_source(source: &str) -> Vec<u8> {
    let mut orbits: Vec<u8> = Vec::new();
    let mut any_line = false;
    for line in source.lines() {
        let code = line.split("//").next().unwrap_or("");
        if !code.trim().is_empty() {
            any_line = true;
        }
        let mut rest = code;
        while let Some((at, call)) = [".orbit(", ".o("]
            .into_iter()
            .filter_map(|call| rest.find(call).map(|at| (at, call)))
            .min_by_key(|(at, _)| *at)
        {
            rest = &rest[at + call.len()..];
            let Some(close) = rest.find(')') else {
                break;
            };
            let inside = &rest[..close];
            let mut number = String::new();
            for character in inside.chars().chain(std::iter::once(' ')) {
                if character.is_ascii_digit() {
                    number.push(character);
                } else if !number.is_empty() {
                    if let Ok(orbit) = number.parse::<u8>()
                        && usize::from(orbit) < rustel_audio::MAX_ORBITS
                    {
                        orbits.push(orbit);
                    }
                    number.clear();
                }
            }
            rest = &rest[close..];
        }
    }
    if any_line {
        orbits.push(1);
    }
    orbits.sort_unstable();
    orbits.dedup();
    orbits
}

/// A fader in dB as the engine takes it.
fn fader_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A meter's linear peak as dB, floored where the meter's travel ends.
fn peak_db(peak: f32) -> f32 {
    if peak <= 0.0 {
        METER_FLOOR_DB
    } else {
        (20.0 * peak.log10()).max(METER_FLOOR_DB)
    }
}

/// How long a limiter mode `c` has landed on waits before the sound and the
/// set take it.
///
/// A held `c` walks the ladder on the strip at the key's own repeat rate,
/// the way held arrows walk the theme picker, and only the mode the finger
/// lifts on is applied. Applying every step was a run of audible changes on
/// the way to the one that was wanted; ignoring the repeats instead made a
/// held key do nothing at all.
const LIMITER_MODE_SETTLE: Duration = THEME_APPLY_SETTLE;

/// One decibel per keyboard nudge, a third of that per scroll notch.
pub(super) const VOLUME_KEY_STEP_DB: f32 = 1.0;
pub(super) const VOLUME_SCROLL_STEP_DB: f32 = 0.5;

impl App {
    /// A score can lose all of its live faders on the next evaluation. Do not
    /// leave the mixer claiming the keyboard for a fader that is no longer on
    /// the desk: the next arrow must select a real strip (or the next panel),
    /// and reopening the mixer must not resurrect a dead selection.
    pub(super) fn settle_mixer_fader_selection(&mut self) {
        let count = self
            .audible_scene
            .map_or(0, |scene| self.live_chips(scene).len());
        if count == 0 {
            self.mixer_on_faders = false;
            self.mixer_fader = 0;
        } else if self.mixer_fader >= count {
            self.mixer_fader = count - 1;
        }
    }

    /// Fold the engine's latest level reading into the meter, and push the
    /// fader down to the engine if it moved.
    pub(super) fn observe_master(&mut self, now: Instant) {
        if self.is_playing() {
            self.master.observe(self.worker.master().take_levels(), now);
        } else {
            self.master.silence(now);
        }
    }

    pub(super) fn set_master_gain_db(&mut self, db: f32) {
        // A hand that moves the fader is asking to hear the result, so the
        // mute lifts rather than swallowing the gesture and leaving the
        // player to wonder which of the two controls is the silent one.
        let unmuted = self.master.set_muted(false);
        if self.master.set_gain_db(db) || unmuted {
            self.worker.master().set_gain(self.master.gain());
            self.status = format!("master {:+.1} dB", self.master.gain_db());
            self.fader_tape_due = Some(Instant::now() + TAPE_GESTURE_SETTLE);
            self.dirty_frame = true;
            // Kept in the set, which is where a level belongs: the loudness a
            // set is played at is part of how it plays. What is recorded is
            // the fader AFTER its own clamp, so a set reopens at the level
            // that was heard rather than at one the fader refused.
            if self.scenes.set_master_gain_db(self.master.gain_db()) {
                self.save_manifest_soon();
            }
        }
    }

    /// Bring the faders to where this set left them.
    ///
    /// Straight onto the desk, and deliberately NOT through the setters: those
    /// record the new level back into the set (it came from there) and put a
    /// "master -6.0 dB" on the status line for a fader nobody touched, which
    /// would overwrite the "opened <set>" line a player is actually waiting
    /// to read.
    ///
    /// A set that never moved the master opens at unity rather than keeping
    /// the level the previous set was left at, for the reason the limiter
    /// does the same: a set brings its own sound, and a set opened
    /// mid-performance inheriting the last one's level is a quiet set that
    /// nobody chose to make quiet.
    pub(super) fn restore_set_levels(&mut self) {
        let master_db = self.scenes.master_gain_db().unwrap_or(0.0);
        if self.master.set_gain_db(master_db) {
            self.worker.master().set_gain(self.master.gain());
            self.dirty_frame = true;
        }
        self.restore_input_level();
    }

    /// The input fader for whichever input is chosen now, from this set if
    /// it ever kept one for that device.
    ///
    /// A device the set never set a level for leaves the fader where it is.
    /// The studio used to keep ONE input level for every device, and a set
    /// that predates per-device levels should go on sounding the way it did,
    /// not have its microphone dropped to unity on the first open.
    pub(super) fn restore_input_level(&mut self) {
        let Some(device) = self.prefs.audio_input.clone() else {
            return;
        };
        let Some(db) = self.scenes.input_gain_db(&device) else {
            return;
        };
        let db = db.clamp(*INPUT_FADER_DB.start(), *INPUT_FADER_DB.end());
        if self.mixer.input_gain_db != db {
            self.mixer.input_gain_db = db;
            self.mixer.input_gain_pending = true;
            self.dirty_frame = true;
        }
    }

    /// `m` on the master strip: silence at the output, the fader left
    /// where it stands.
    ///
    /// Deliberately NOT kept in the set. A level is part of how a set
    /// plays and belongs in its file; a mute is a thing a player does for
    /// the next thirty seconds, and a set that reopened muted would be the
    /// silent-set problem `restore_set_levels` already refuses to create,
    /// with no fader moved to explain it.
    fn toggle_master_mute(&mut self) {
        let muted = !self.master.muted();
        if !self.master.set_muted(muted) {
            return;
        }
        self.worker.master().set_gain(self.master.gain());
        self.status = if muted {
            format!(
                "master muted - m unmutes, {:+.1} dB waiting",
                self.master.gain_db()
            )
        } else {
            format!("master {:+.1} dB", self.master.gain_db())
        };
        self.dirty_frame = true;
    }

    pub(super) fn nudge_master_gain_db(&mut self, delta: f32) {
        let mut next = self.master.gain_db() + delta;
        // A fader detent at unity: nudging past zero lands on it exactly.
        if next.signum() != self.master.gain_db().signum() && next.abs() < delta.abs() {
            next = 0.0;
        }
        self.set_master_gain_db(next);
    }

    /// How wide each strip is drawn, in order. The drawing and every hit
    /// test must agree about it, because the strips are not all one width.
    pub(super) fn mixer_strip_widths(&self) -> Vec<u16> {
        self.mixer_targets()
            .into_iter()
            .map(|target| {
                super::super::mixer_panel::strip_width(match target {
                    MixerTarget::Limiter => super::super::viz_panel::MixerStripKind::Reduction {
                        reduction_db: 0.0,
                        bypassed: false,
                    },
                    _ => super::super::viz_panel::MixerStripKind::Level,
                })
            })
            .collect()
    }

    /// The strips the mixer has right now: the input, every orbit the set
    /// has used lately, the master.
    pub(super) fn mixer_targets(&self) -> Vec<MixerTarget> {
        // The limiter, then the master: the desk reads left to right into
        // the output, so the last thing before it goes out sits against
        // it. Both are at the head, so the strips that come and go with
        // the score never move either. No limiter on this set, no strip:
        // a slot nothing is in is not a control, and a strip drawn for it
        // would show a mode and a ceiling that neither the set nor the
        // studio holds.
        let mut targets = if self.has_limiter_slot() {
            vec![MixerTarget::Limiter, MixerTarget::Master]
        } else {
            vec![MixerTarget::Master]
        };
        // The orbits the text names and the orbits that have sounded
        // lately, together: a strip appears as the line is typed and
        // stays while the sound it made rings out.
        let mut orbits: Vec<u8> = self.orbits_in_code.clone();
        if let Some(snapshot) = self.snapshot.as_ref() {
            orbits.extend(snapshot.orbits.iter().map(|level| level.orbit));
        }
        orbits.sort_unstable();
        orbits.dedup();
        targets.extend(orbits.into_iter().map(MixerTarget::Orbit));
        // No input chosen, no `in` strip: nothing is open to meter, and a
        // fader on silence would only ask to be explained.
        if self.input_chosen() {
            targets.push(MixerTarget::Input);
        }
        targets
    }

    /// Whether the mixer is on screen - the panel, or a dock's widget:
    /// only then is its picture assembled.
    pub(super) fn shows_mixer(&self) -> bool {
        self.mixer_panel.is_some()
            || (0..DOCKS).any(|index| {
                self.viz_docks[index].is_some()
                    && self.prefs.visuals[index]
                        .widgets
                        .iter()
                        .any(|widget| widget.kind == WidgetKind::Mixer)
            })
    }

    /// The mixer widget's picture: one strip a target, with its meter
    /// from the last snapshot and its fader from here; the devices beside
    /// them, with what they are doing.
    pub(super) fn mixer_facts(&self) -> Option<MixerFacts> {
        if !self.shows_mixer() {
            return None;
        }
        let snapshot = self.snapshot.as_ref();
        let strips = self
            .mixer_targets()
            .into_iter()
            .map(|target| {
                let selected = target == self.mixer.selected;
                match target {
                    MixerTarget::Input => MixerStrip {
                        label: "in".to_owned(),
                        detail: snapshot
                            .and_then(|snapshot| snapshot.input_device.clone())
                            .unwrap_or_else(|| "no input open".to_owned()),
                        peak_db: self.input_meter.peak_db(),
                        hold_db: Some(self.input_meter.hold_db()),
                        gain_db: Some(self.mixer.input_gain_db),
                        fader_range: Some((*INPUT_FADER_DB.start(), *INPUT_FADER_DB.end())),
                        clipping: false,
                        muted: false,
                        selected,
                        kind: MixerStripKind::Level,
                    },
                    // An orbit's level is the score's - `gain` and `postgain`
                    // on the line - so its strip is a meter alone.
                    MixerTarget::Orbit(orbit) => MixerStrip {
                        label: format!("orbit {orbit}"),
                        detail: String::new(),
                        peak_db: peak_db(
                            snapshot
                                .and_then(|snapshot| {
                                    snapshot.orbits.iter().find(|level| level.orbit == orbit)
                                })
                                .map_or(0.0, |level| level.peak),
                        ),
                        hold_db: None,
                        gain_db: None,
                        fader_range: None,
                        clipping: false,
                        muted: false,
                        selected,
                        kind: MixerStripKind::Level,
                    },
                    MixerTarget::Master => MixerStrip {
                        label: "master".to_owned(),
                        // A muted desk says so on the strip. The fader does
                        // not move for a mute, so without this the one
                        // visible difference between a muted master and a
                        // score that has simply stopped is the silence -
                        // and a player looking for the reason has nowhere
                        // to look.
                        detail: if self.master.muted() {
                            "muted".to_owned()
                        } else {
                            String::new()
                        },
                        peak_db: self.master.peak_db(),
                        hold_db: Some(self.master.hold_db()),
                        gain_db: Some(self.master.gain_db()),
                        fader_range: Some((METER_FLOOR_DB, MASTER_FADER_CEILING_DB)),
                        clipping: self.master.clipping(Instant::now()),
                        muted: self.master.muted(),
                        selected,
                        kind: MixerStripKind::Level,
                    },
                    // The limiter reads as a gain-reduction meter: no level
                    // to show, a ceiling to set, and a fill that grows as it
                    // works.
                    MixerTarget::Limiter => {
                        // What the strip edits, which is what it holds
                        // whether or not it is switched in: a bypass keeps
                        // its ceiling and its character so there is
                        // something to switch back to.
                        let held = self.desk_limiter();
                        MixerStrip {
                            // Short enough for a narrow strip: the mode is
                            // the label, because the mode is what `c`
                            // changes and what a glance has to confirm.
                            label: held.character.short().to_owned(),
                            detail: String::new(),
                            peak_db: METER_FLOOR_DB,
                            hold_db: None,
                            gain_db: Some(held.threshold_db),
                            fader_range: Some((LIMITER_FLOOR_DB, 0.0)),
                            clipping: false,
                            muted: false,
                            selected,
                            kind: MixerStripKind::Reduction {
                                reduction_db: self.master.reduction_db(),
                                bypassed: self.live_master_limiter().is_none(),
                            },
                        }
                    }
                }
            })
            .collect();
        let pads = rustel_core::gamepad::connected_pads();
        let pads_live = pads
            .iter()
            .map(|(slot, name)| {
                let mut line = format!("gamepad({slot}) {name}");
                if let Some(pad) = rustel_core::gamepad::pad(*slot) {
                    let held: Vec<&str> = rustel_core::gamepad::BUTTON_NAMES
                        .iter()
                        .filter(|(_, index)| pad.button(usize::from(*index)) >= 0.5)
                        .map(|(names, _)| names[0])
                        .collect();
                    if !held.is_empty() {
                        line.push_str(" · ");
                        line.push_str(&held.join(" "));
                    }
                    for (axis, label) in ["x1", "y1", "x2", "y2"].into_iter().enumerate() {
                        let value = pad.axis(axis);
                        if value.abs() > 0.05 {
                            line.push_str(&format!(" · {label} {value:+.2}"));
                        }
                    }
                }
                line
            })
            .collect();
        // The desk's device readout is pinned exactly where the footer's is,
        // so a golden of the desk never depends on what is plugged into the
        // machine that recorded it.
        let pinned = self.pinned_frame_inputs();
        Some(MixerFacts {
            strips,
            faders: self.mixer_faders(),
            midi_ports: if pinned {
                MidiPortCounts::default()
            } else {
                self.devices.inventory().midi_port_counts()
            },
            midi_active: !pinned && self.pads.active_within(ACTIVITY_LIGHT_MILLIS),
            // The event feed too. It was the one field left unpinned, so a
            // host with a controller attached filled it with live aftertouch
            // and the desk's golden became a golden of whatever hardware that
            // machine happened to have switched on.
            midi_recent: if pinned {
                Vec::new()
            } else {
                self.pads.recent_events()
            },
            pads: if pinned { 0 } else { pads.len() },
            pad_active: !pinned
                && pads
                    .iter()
                    .filter_map(|(slot, _)| rustel_core::gamepad::pad(*slot))
                    .any(|pad| {
                        pad.idle_for_millis()
                            .is_some_and(|idle| idle < ACTIVITY_LIGHT_MILLIS)
                    }),
            // The per-pad lines render live button and axis state, so they
            // belong to the host too - pinned to nothing, beside the count
            // and the light that were already pinned.
            pads_live: if pinned { Vec::new() } else { pads_live },
            // A press or a stick's move, read the same way MIDI's own feed
            // is: unpinned, for the reason `midi_recent` is above.
            gamepad_recent: if pinned {
                Vec::new()
            } else {
                rustel_core::gamepad::recent_activity()
            },
        })
    }

    /// The evaluated score's sliders, as the mixer's desk shows them.
    ///
    /// Read from `live_chips` every frame rather than cached: that list is
    /// already followed through every edit since the evaluation and already
    /// drops a slider whose text was deleted, so an evaluation rebuilds the
    /// desk with nothing to invalidate. Source order, which is what makes
    /// mapping slot N to fader N mean the same thing twice running.
    pub(super) fn mixer_faders(&self) -> Vec<super::super::viz_panel::MixerFader> {
        let Some(scene) = self.audible_scene else {
            return Vec::new();
        };
        self.live_chips(scene)
            .into_iter()
            .enumerate()
            .map(|(at, chip)| {
                let span = &chip.span;
                super::super::viz_panel::MixerFader {
                    // A score that never named it still needs something on
                    // the strip, and its place is the only fact left.
                    label: chip
                        .label
                        .clone()
                        .unwrap_or_else(|| format!("slider {}", at + 1)),
                    notch: chip.travel.ratio(span.value, span.min, span.max) as f32,
                    value: slider::format_value(span.value),
                    min: slider::format_value(span.min),
                    max: slider::format_value(span.max),
                    selected: self.mixer_on_faders && self.mixer_fader == at,
                    // The number is the slot that drives it, and a slot
                    // nobody has bound drives nothing: saying "1" beside a
                    // fader no knob reaches would be a promise the studio
                    // does not keep.
                    slot: self
                        .ui_settings
                        .mappings
                        .get(at)
                        .and_then(|source| source.map(|_| at)),
                }
            })
            .collect()
    }

    /// F4, ^⇧M or View ▸ Mixer: the desk along the bottom (or the top),
    /// or hidden again. It is a fixture like the set panel: it stays while
    /// the score is edited, Esc hands the keyboard back and leaves it, and
    /// it comes back the next launch the way it was left.
    pub(super) fn toggle_mixer_panel(&mut self) {
        if self.mixer_panel.is_some() {
            self.close_panel(PanelKind::Mixer);
            return;
        }
        self.open_mixer_panel(true);
    }

    /// Show the desk, with the keyboard or without it. On a terminal too
    /// short for the band the desk is opened but not given the keyboard,
    /// and the status line says why nothing appeared.
    pub(super) fn open_mixer_panel(&mut self, focus: bool) {
        let panel = MixerPanel::new(
            MixerEdge::from_top(self.ui_settings.mixer_top),
            self.prefs.mixer_rows,
        );
        self.mixer_panel = Some(panel);
        self.prefs.mixer_panel = Some(true);
        self.save_prefs_soon();
        self.invalidate_maps();
        let room = self.frame.is_empty()
            || !self
                .layout_with_fixtures(
                    Some(panel.dock()),
                    self.log_dock_request(),
                    self.memory_dock_request(),
                )
                .mixer
                .is_empty();
        if !room {
            self.status = format!(
                "mixer: no room for {} rows on this terminal - make it taller, or {} hides it",
                panel.rows,
                self.shortcut_or_menu(BindAction::Mixer, "View > Mixer")
            );
            self.dirty_frame = true;
            return;
        }
        // The desk has room but no painted frame yet: the region is not
        // yet the truth about the room it got, so the keys stay with the
        // desk until the next draw lays it (the `viz_unlaid` bargain).
        self.mixer_unlaid = true;
        if focus {
            self.focus_panel(PanelKind::Mixer);
        }
        // Zen has no docked furniture: the desk that just opened is a
        // popup over the score, and Esc closes it outright rather than
        // merely handing the keyboard back to a desk left standing.
        self.status = if self.ui_settings.zen {
            "mixer: ←/→ strip · ↑/↓ fader · 0 unity · e top/bottom · Esc closes it".into()
        } else {
            format!(
                "mixer: ←/→ strip · ↑/↓ fader · 0 unity · e top/bottom · Esc back to the score · {} hides",
                self.keybinds.hint(BindAction::Mixer)
            )
        };
        self.dirty_frame = true;
    }

    /// The desk's keys: ←/→ choose a strip, ↑/↓ move its fader a decibel
    /// (a tenth with ⇧), 0 puts it back, e flips the desk to the other
    /// edge, + and - make it taller or shorter, Esc gives the score the
    /// keyboard and leaves the desk standing.
    pub(super) fn handle_mixer_panel_key(
        &mut self,
        code: KeyCode,
        primary: bool,
        shift: bool,
    ) -> bool {
        if primary || self.mixer_panel.is_none() {
            return false;
        }
        // A dragged selection in the devices block is what a copy key
        // takes, ahead of every other meaning - `c` on the limiter strip
        // cycles its mode, and with a selection held it must copy instead.
        // With nothing dragged, every key keeps its ordinary meaning.
        if matches!(code, KeyCode::Char('c' | 'C' | 'y' | 'Y'))
            && let Some(text) = self.mixer_selection_text()
        {
            let lines = text.lines().count();
            match self.clipboard.set_text(text) {
                Ok(()) => {
                    if lines > 1 {
                        self.toast(format!("copied {lines} lines"));
                    } else {
                        self.toast("copied");
                    }
                }
                Err(error) => self.status = format!("cannot copy: {error}"),
            }
            self.dirty_frame = true;
            return true;
        }
        let step = if shift { 0.1 } else { FADER_KEY_STEP_DB };
        match code {
            KeyCode::Esc if self.mapping_learn.is_some() => {
                self.mapping_learn = None;
                self.status = "learn cancelled".into();
            }
            // Zen has no docked furniture: the desk came up as a popup,
            // so Esc closes it outright, unlike docked, where the desk
            // is a fixture on the stage and Esc only gives the keyboard
            // back to the score standing beside it.
            KeyCode::Esc if self.ui_settings.zen => {
                self.close_panel(PanelKind::Mixer);
                return true;
            }
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                self.status = format!(
                    "the mixer stays - {} hides it",
                    self.keybinds.hint(BindAction::Mixer)
                );
            }
            // Tab crosses the desk: the strips, then the score's faders,
            // then round again. On the strips the arrows are the strip's,
            // as they were; on a score fader they are the fader's, which
            // is drawn lying down - ← → move it, ↑ ↓ pick the next one.
            KeyCode::Tab => self.mixer_step_desk(true),
            // A terminal reports Shift+Tab as its own code, so without this
            // arm it falls through to the score and outdents a line.
            KeyCode::BackTab => self.mixer_step_desk(false),
            KeyCode::Left if self.mixer_on_faders => {
                self.nudge_mixer_fader(if shift { -0.1 } else { -1.0 });
            }
            KeyCode::Right if self.mixer_on_faders => {
                self.nudge_mixer_fader(if shift { 0.1 } else { 1.0 });
            }
            KeyCode::Up if self.mixer_on_faders => {
                self.step_mixer_fader(-1);
            }
            KeyCode::Down | KeyCode::Enter if self.mixer_on_faders => {
                self.step_mixer_fader(1);
            }
            // Learn the fader under the keys, here, without opening
            // Settings. Mid-set the mixer is where your hands already are,
            // and a mapping you have to leave the desk to make is a
            // mapping you do not make.
            KeyCode::Char('l' | 'L') if self.mixer_on_faders => {
                self.learn_focused_fader();
            }
            KeyCode::Left => self.mixer_step_strip(false),
            // Enter on the limiter is its bypass switch, which is the one
            // strip where stepping to the next one is not the useful thing
            // a hand already on it wants; Tab and the arrows still walk.
            KeyCode::Enter if self.limiter_strip_selected() => {
                self.toggle_master_limiter_bypass();
            }
            KeyCode::Right | KeyCode::Enter => self.mixer_step_strip(true),
            KeyCode::Up => self.nudge_mixer(step),
            KeyCode::Down => self.nudge_mixer(-step),
            KeyCode::Char('0') => self.reset_mixer_strip(),
            // `m`, which a mixer has taught every hand to read as mute,
            // and which `c` below was kept off for exactly this. The master
            // strip alone: an orbit is a meter, not a fader, and has no
            // level of its own to silence.
            KeyCode::Char('m') if self.mixer.selected == MixerTarget::Master => {
                self.toggle_master_mute();
            }
            // `c`, for character, cycles the limiter's mode, and only
            // there: on any other strip it would be a key that silently
            // does nothing.
            KeyCode::Char('c') if self.limiter_strip_selected() => {
                self.cycle_limiter_character();
            }
            // `b` is the same switch as Enter, under the finger that is
            // already walking the modes with `c`: Enter is the desk's
            // general "do the thing on this strip", and a letter is what a
            // hand reaches for without leaving the two keys beside it.
            KeyCode::Char('b') if self.limiter_strip_selected() => {
                self.toggle_master_limiter_bypass();
            }
            // Backspace over the limiter takes it off this set, the way
            // Backspace takes anything else away. The menu says it in
            // words; this is the hand already on the desk.
            KeyCode::Backspace | KeyCode::Delete if self.limiter_strip_selected() => {
                self.set_limiter_slot(false);
            }
            KeyCode::Char('e') => {
                let Some(panel) = self.mixer_panel.as_mut() else {
                    return false;
                };
                panel.edge = panel.edge.flipped();
                let edge = panel.edge;
                self.ui_settings.mixer_top = edge.is_top();
                self.prefs.set_ui_settings(&self.ui_settings);
                self.save_prefs_soon();
                self.invalidate_maps();
                self.mixer_unlaid = true;
                self.status = format!("mixer along the {}", edge.name());
            }
            KeyCode::Char('+' | '=' | '-') => {
                let Some(panel) = self.mixer_panel.as_mut() else {
                    return false;
                };
                if panel.resize(code != KeyCode::Char('-')) {
                    let rows = panel.rows;
                    self.prefs.mixer_rows = Some(rows);
                    self.save_prefs_soon();
                    self.invalidate_maps();
                    self.mixer_unlaid = true;
                    self.status = format!("mixer {rows} rows tall");
                }
            }
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// The desk on screen - its strips' rows and columns - while the
    /// panel is drawn.
    fn mixer_desk(&self) -> Option<Rect> {
        let panel = self.mixer_panel?;
        Some(super::super::mixer_panel::parts(self.regions.mixer, panel.edge)?.desk)
    }

    /// The devices block as drawn: where the MIDI log and the pads stand,
    /// and the rows they draw, for hit tests and copying.
    pub(super) fn mixer_devices_block(&self) -> Option<(Rect, Vec<String>)> {
        let desk = self.mixer_desk()?;
        let facts = self.mixer_facts()?;
        super::super::mixer_panel::devices_block(
            &facts,
            desk,
            &self.mixer_strip_widths(),
            facts.faders.len(),
            &self.theme,
        )
    }

    /// What `c` copies: the text a drag through the mixer's devices block
    /// took, when it took some.
    pub(super) fn mixer_selection_text(&self) -> Option<String> {
        let held = self.mixer_selection.as_ref()?;
        let text = held.selection.text(&held.lines);
        (!text.trim().is_empty()).then_some(text)
    }

    /// Extend the devices-block drag to wherever the pointer is now, at
    /// the granularity the press chose. When the block has moved or
    /// resized since the press, the band is dropped but the snapshot the
    /// drag took stays for `c` - it is the text that was selected.
    pub(super) fn drag_mixer_selection(
        &mut self,
        x: u16,
        y: u16,
        granularity: super::super::textblock::Granularity,
    ) {
        use super::super::textblock::{Clamp, TextBlock};
        let Some((area, lines)) = self.mixer_devices_block() else {
            self.pointer = None;
            return;
        };
        let Some(held) = self.mixer_selection.as_mut() else {
            self.pointer = None;
            return;
        };
        if area != held.area {
            self.pointer = None;
            return;
        }
        let block = TextBlock {
            lines: &lines,
            area,
            first_line: 0,
        };
        let Some(point) = block.point_at(x, y, Clamp::Extend) else {
            return;
        };
        held.selection.head = point;
        held.selection = widen_selection(held.selection, granularity, &held.lines);
        self.dirty_frame = true;
    }

    /// The block of score faders on the drawn desk, and the chips it is
    /// showing, in the order it shows them.
    fn mixer_fader_desk(&self) -> Option<(super::super::mixer_panel::FaderDesk, Vec<LiveChip>)> {
        let desk = self.mixer_desk()?;
        let chips = self.live_chips(self.audible_scene?);
        let layout =
            super::super::mixer_panel::fader_desk(desk, &self.mixer_strip_widths(), chips.len())?;
        Some((layout, chips))
    }

    /// The score fader under a point, and its rail when the point is on
    /// the rail itself - a press on the label or the ends chooses the
    /// fader without moving it, the way a press on a strip's label does.
    fn score_fader_at(&self, x: u16, y: u16) -> Option<(usize, LiveChip, Option<SliderTrack>)> {
        let (layout, mut chips) = self.mixer_fader_desk()?;
        let at = layout.at(x, y, chips.len())?;
        let rail = layout
            .cell(at)
            .and_then(super::super::mixer_panel::FaderDesk::rail)
            .filter(|rail| rail.y == y && (rail.x..rail.right()).contains(&x))
            .map(|rail| SliderTrack {
                x: rail.x,
                width: rail.width,
            });
        Some((at, chips.swap_remove(at), rail))
    }

    /// The mixer's pointer over a fader a press would drive: a hand on the
    /// score rail, matching the inline pill, and a hand on a strip's meter
    /// that takes a fader, matching the footer's. Labels, the MIDI log and
    /// the rest of the desk keep the studio's ordinary shapes.
    pub(super) fn mixer_pointer_shape(&self, x: u16, y: u16) -> Option<&'static str> {
        if self.mixer_panel.is_none() || !within(self.regions.mixer, x, y) {
            return None;
        }
        if self
            .score_fader_at(x, y)
            .is_some_and(|(_, _, rail)| rail.is_some())
        {
            return Some(SHAPE_POINTER);
        }
        if let Some(target) = self.mixer_panel_strip_at(x, y) {
            let on_meter = self
                .mixer_desk()
                .and_then(super::super::mixer_panel::meter_rows)
                .is_some_and(|(top, bottom)| (top..=bottom).contains(&y));
            if on_meter && target.has_fader() {
                return Some(SHAPE_POINTER);
            }
        }
        None
    }

    /// The strip under a point of the desk.
    fn mixer_panel_strip_at(&self, x: u16, y: u16) -> Option<MixerTarget> {
        let desk = self.mixer_desk()?;
        if !within(desk, x, y) {
            return None;
        }
        let strip = super::super::mixer_panel::strip_at(desk, &self.mixer_strip_widths(), x)?;
        self.mixer_targets().get(strip).copied()
    }

    /// A press on the desk gives it the keyboard and chooses the strip
    /// under the pointer; on a strip with a fader - the input, the master -
    /// it takes the fader to the row and keeps it for the drag, the way
    /// the footer's fader does. Returns true when it took the press.
    pub(super) fn click_mixer_panel(&mut self, x: u16, y: u16) -> bool {
        if self.mixer_panel.is_none() || !within(self.regions.mixer, x, y) {
            return false;
        }
        self.focus_panel(PanelKind::Mixer);
        self.pointer = Some(Pointer::Panel);
        // A press on the devices block starts a character selection there:
        // the MIDI log and the pads are text to drag out and copy, like
        // the reference column's entries. The rows the drag is made on
        // are captured then, because the log keeps scrolling under them.
        if let Some((area, lines)) = self.mixer_devices_block()
            && within(area, x, y)
        {
            use super::super::textblock::{Clamp, TextBlock, TextSelection};
            let block = TextBlock {
                lines: &lines,
                area,
                first_line: 0,
            };
            if let Some(point) = block.point_at(x, y, Clamp::Inside) {
                let granularity = self.clicks.press(x, y, self.moment());
                let selection = widen_selection(TextSelection::caret(point), granularity, &lines);
                self.mixer_selection = Some(MixerSelection {
                    lines,
                    area,
                    selection,
                });
                self.own_text_selection(TextSurface::Mixer);
                self.pointer = Some(Pointer::MixerText { granularity });
                self.dirty_frame = true;
                return true;
            }
        }
        // The score's own faders sit to the right of the strips: a press
        // chooses one, and a press on its rail takes it and keeps it for
        // the drag, exactly as the inline pill does.
        if let Some((at, chip, rail)) = self.score_fader_at(x, y) {
            self.mixer_fader = at;
            self.mixer_on_faders = true;
            if let Some((scene, rail)) = self.audible_scene.zip(rail) {
                self.pointer = Some(Pointer::MixerFader {
                    scene,
                    call_from: chip.call.start,
                    rail,
                });
                self.drag_mixer_fader(scene, chip.call.start, rail, x);
            } else {
                self.status = format!(
                    "{}: {} - drag the rail, or ← → with the desk focused",
                    chip.label.clone().unwrap_or_else(|| "slider".to_owned()),
                    slider::format_value(chip.span.value)
                );
            }
            self.dirty_frame = true;
            return true;
        }
        if let Some(target) = self.mixer_panel_strip_at(x, y) {
            self.mixer.selected = target;
            self.mixer_on_faders = false;
            // Only a press on the meter itself takes the fader there: the
            // label and the readout choose the strip and move nothing, or a
            // click on `in` would slam a live microphone to +48 dB.
            let on_meter = self
                .mixer_desk()
                .and_then(super::super::mixer_panel::meter_rows)
                .is_some_and(|(top, bottom)| (top..=bottom).contains(&y));
            if on_meter && target.has_fader() {
                self.pointer = Some(Pointer::MixerPanelFader);
                self.drag_mixer_panel_fader(y);
            }
        }
        self.dirty_frame = true;
        true
    }

    /// The pressed fader follows the pointer's row on its own scale,
    /// clamped to the meter's travel, so a drag that slides off the strip
    /// keeps driving it.
    pub(super) fn drag_mixer_panel_fader(&mut self, y: u16) {
        let Some(desk) = self.mixer_desk() else {
            return;
        };
        let range = match self.mixer.selected {
            MixerTarget::Input => (*INPUT_FADER_DB.start(), *INPUT_FADER_DB.end()),
            MixerTarget::Master => (METER_FLOOR_DB, MASTER_FADER_CEILING_DB),
            MixerTarget::Limiter => (LIMITER_FLOOR_DB, 0.0),
            MixerTarget::Orbit(_) => return,
        };
        // Where the pointer actually was, not merely which row it landed
        // in: a strip is a column, so without this a desk ten rows tall
        // gave the master's whole travel ten stops.
        let db = super::super::mixer_panel::fader_db_within(desk, y, self.pointer_subrow, range);
        match self.mixer.selected {
            MixerTarget::Master => self.set_master_gain_db(db),
            target => self.set_mixer_fader_db(target, db),
        }
    }

    /// A pressed score fader follows the pointer along its rail, and
    /// writes through `set_slider_with_smoothing` - the one path that
    /// keeps the score's text, the engine's value, the undo group and the
    /// inline pill in step. The mixer draws no value of its own.
    pub(super) fn drag_mixer_fader(
        &mut self,
        scene: SceneId,
        call_from: usize,
        rail: SliderTrack,
        x: u16,
    ) {
        let Some(chip) = self.live_chip_at(scene, call_from) else {
            return;
        };
        let cell_width = self
            .features
            .pixel_mouse
            .then_some(self.features.cell_pixels)
            .flatten()
            .map(|(width, _)| width);
        let ratio = rail.ratio(x, self.pointer_subcell, cell_width);
        let value = chip.travel.at(ratio, chip.span.min, chip.span.max);
        self.set_slider_with_smoothing(scene, &chip, value, self.ui_settings.slider_smoothing);
        self.status = format!(
            "{} {}",
            chip.label.clone().unwrap_or_else(|| "slider".to_owned()),
            slider::format_value(chip.span.clamp(value))
        );
    }

    /// One notch of the wheel, or one press of ← →, over the desk's score
    /// faders: a step of the slider's own grain.
    fn nudge_mixer_fader(&mut self, notches: f64) {
        let Some(scene) = self.audible_scene else {
            return;
        };
        let chips = self.live_chips(scene);
        let Some(chip) = chips.get(self.mixer_fader) else {
            return;
        };
        let span = &chip.span;
        let step = if span.step > 0.0 {
            span.step
        } else {
            (span.max - span.min) / 100.0
        };
        let value = slider::snap_value(span.value + notches * step, span.min, span.max, span.step);
        let chip = chip.clone();
        self.set_slider(scene, &chip, value);
        self.status = format!(
            "{} {}",
            chip.label.clone().unwrap_or_else(|| "slider".to_owned()),
            slider::format_value(chip.span.clamp(value))
        );
        self.dirty_frame = true;
    }

    /// `l` on a score fader: wait for a control and give it to this
    /// fader's slot.
    ///
    /// The same learn the Settings page runs, reached from where the
    /// faders are. A fader past the twelfth has no slot to learn into and
    /// says so rather than appearing to arm.
    fn learn_focused_fader(&mut self) {
        let faders = self
            .audible_scene
            .map_or(0, |scene| self.live_chips(scene).len());
        if faders == 0 {
            self.status = "no faders to learn - the score has no slider yet".into();
            return;
        }
        let slot = self.mixer_fader.min(faders - 1);
        if slot >= MAPPING_SLOTS {
            self.status = format!(
                "only the first {MAPPING_SLOTS} faders have a slot - this one is {}",
                slot + 1
            );
            return;
        }
        self.mapping_learn = Some(slot);
        self.status = format!(
            "slot {}: move the knob, fader or stick that should drive it - Esc cancels",
            slot + 1
        );
        self.dirty_frame = true;
    }

    /// Tab across the whole desk: every strip, then every score fader,
    /// then round again. Which block the keys are on is what the arrows
    /// then mean.
    fn mixer_step_desk(&mut self, forward: bool) {
        let strips = self.mixer_targets().len();
        let faders = self
            .audible_scene
            .map_or(0, |scene| self.live_chips(scene).len());
        if strips + faders == 0 {
            return;
        }
        // One line of places: the strips, then the faders.
        let at = if self.mixer_on_faders {
            strips + self.mixer_fader.min(faders.saturating_sub(1))
        } else {
            self.mixer_targets()
                .iter()
                .position(|target| *target == self.mixer.selected)
                .unwrap_or(0)
        };
        let next = (at as isize + if forward { 1 } else { -1 })
            .rem_euclid((strips + faders) as isize) as usize;
        if next >= strips {
            self.mixer_on_faders = true;
            self.mixer_fader = next - strips;
        } else {
            self.mixer_on_faders = false;
            if let Some(target) = self.mixer_targets().get(next) {
                self.mixer.selected = *target;
            }
        }
        self.dirty_frame = true;
    }

    /// ↑ ↓ or Tab over the desk moves between the score's faders.
    fn step_mixer_fader(&mut self, delta: isize) -> bool {
        let count = self
            .audible_scene
            .map_or(0, |scene| self.live_chips(scene).len());
        if count == 0 {
            return false;
        }
        let at = self.mixer_fader.min(count - 1) as isize + delta;
        self.mixer_fader = at.rem_euclid(count as isize) as usize;
        self.dirty_frame = true;
        true
    }

    /// The wheel over a strip with a fader nudges it, half a decibel a
    /// notch, the master's way; over the rest of the desk it is the
    /// desk's, and does nothing.
    pub(super) fn scroll_mixer_panel(&mut self, x: u16, y: u16, direction: f32) -> bool {
        if self.mixer_panel.is_none() || !within(self.regions.mixer, x, y) {
            return false;
        }
        if let Some((at, _, _)) = self.score_fader_at(x, y) {
            self.mixer_fader = at;
            self.nudge_mixer_fader(f64::from(direction));
            return true;
        }
        // Every strip, and `nudge_mixer` decides what that means: the
        // limiter's ceiling was left out of the panel's wheel while the
        // dock's turned it, and an orbit says it is a meter rather than
        // turning under the hand and doing nothing.
        if let Some(target) = self.mixer_panel_strip_at(x, y) {
            self.mixer.selected = target;
            self.nudge_mixer(direction * VOLUME_SCROLL_STEP_DB);
        }
        true
    }

    /// Enter on the mixer: the next strip along, round the end.
    pub(super) fn mixer_next_strip(&mut self) {
        self.mixer_step_strip(true);
    }

    /// The next strip along, or the one before, round either end.
    fn mixer_step_strip(&mut self, forwards: bool) {
        let targets = self.mixer_targets();
        let len = targets.len();
        let at = targets
            .iter()
            .position(|target| *target == self.mixer.selected)
            .map_or(0, |at| {
                if forwards {
                    (at + 1) % len
                } else {
                    (at + len - 1) % len
                }
            });
        self.mixer.selected = targets[at];
        self.status = match self.mixer.selected {
            MixerTarget::Input => "mixer: the input".to_owned(),
            MixerTarget::Orbit(orbit) => format!("mixer: orbit {orbit}"),
            MixerTarget::Master => "mixer: the master".to_owned(),
            MixerTarget::Limiter => {
                let held = self.desk_limiter();
                let state = if self.live_master_limiter().is_none() {
                    "bypassed"
                } else {
                    "in"
                };
                format!(
                    "mixer: the limiter, {state} \u{b7} {} at {:.1} dBFS \u{b7} c for the mode, b or Enter to bypass, Backspace to remove",
                    held.character.key(),
                    held.threshold_db
                )
            }
        };
        self.dirty_frame = true;
    }

    /// ← and → on the mixer: the selected strip's fader by a step.
    pub(super) fn nudge_mixer(&mut self, delta_db: f32) {
        match self.mixer.selected {
            MixerTarget::Master => self.nudge_master_gain_db(delta_db),
            MixerTarget::Input => {
                let target = MixerTarget::Input;
                let current = self.mixer_fader_db(target);
                self.set_mixer_fader_db(target, current + delta_db);
            }
            MixerTarget::Limiter => {
                let target = MixerTarget::Limiter;
                let current = self.mixer_fader_db(target);
                self.set_mixer_fader_db(target, current + delta_db);
            }
            // Orbit strips display the score's level and have no fader.
            MixerTarget::Orbit(orbit) => {
                self.status = format!(
                    "orbit {orbit} is a meter; set its level with gain() or postgain() in the score"
                );
                self.dirty_frame = true;
            }
        }
    }

    /// `0` on the mixer: the selected strip back to unity.
    ///
    /// The limiter's is not a gain, so unity means nothing there; what it
    /// has instead is the studio's default ceiling and mode, and going
    /// back to those is the same gesture. The limiter stays on the set -
    /// `0` is a fader coming home, not a plugin being pulled out, and the
    /// menu is where a set gives one up.
    pub(super) fn reset_mixer_strip(&mut self) {
        match self.mixer.selected {
            MixerTarget::Master => self.set_master_gain_db(0.0),
            MixerTarget::Limiter => self.reset_limiter_to_default(),
            target => self.set_mixer_fader_db(target, 0.0),
        }
    }

    /// This set's limiter back to the ceiling and mode the studio hands
    /// out, still switched in and still on the set.
    fn reset_limiter_to_default(&mut self) {
        // The default outranks a mode still settling, which would otherwise
        // land on top of it a moment later.
        self.limiter_mode_due = None;
        let settings = self.ui_settings.limiter_default();
        if self.scenes.set_limiter(SetLimiter::Says {
            bypassed: false,
            settings,
        }) {
            self.save_manifest_soon();
        }
        self.worker.master().set_limiter(self.live_master_limiter());
        self.status = format!(
            "limiter back to the studio's default \u{b7} {} at {:.1} dBFS",
            settings.character.key(),
            settings.threshold_db
        );
        self.send_mixer_gains();
        self.dirty_frame = true;
    }

    /// The limiter this set actually plays with.
    ///
    /// Two layers, because a limiter is part of a sound and also a house
    /// preference: the settings sheet holds what a set starts with, and a
    /// set given one of its own on the desk keeps it. A set carried to
    /// another machine and never touched plays with that machine's
    /// default rather than bringing one.
    pub(super) fn live_master_limiter(&self) -> Option<rustel_audio::LimiterSettings> {
        self.scenes
            .limiter()
            .resolve()
            .unwrap_or_else(|| self.ui_settings.master_limiter())
    }

    /// The desk moved the limiter, so this set moved with it - not the
    /// studio's default.
    ///
    /// Without this the strip would change the sound and nothing would
    /// remember, so a limiter switched off mid-set would be back at the
    /// next open. It goes to the set file rather than the preferences
    /// because the ceiling and the character are part of how a set
    /// sounds, and the next set should not inherit them.
    pub(super) fn remember_master_limiter(&mut self, settings: rustel_audio::LimiterSettings) {
        // What is written was read off the desk, a mode still settling
        // included, so nothing is left owed.
        self.limiter_mode_due = None;
        let bypassed = self.scenes.limiter().bypassed();
        if self
            .scenes
            .set_limiter(SetLimiter::Says { bypassed, settings })
        {
            self.save_manifest_soon();
        }
    }

    /// Whether the limiter's strip is the one under the keys. The slot
    /// is checked as well as the selection: a set can lose its limiter
    /// while the desk is open, and a stale selection must not leave `c`
    /// and Enter acting on a strip that is no longer drawn.
    fn limiter_strip_selected(&self) -> bool {
        self.mixer.selected == MixerTarget::Limiter && self.has_limiter_slot()
    }

    /// Whether this set has a limiter on its desk: the strip, and every
    /// key that acts on it.
    pub(super) fn has_limiter_slot(&self) -> bool {
        self.scenes
            .limiter()
            .has_slot(self.ui_settings.master_limiter_on)
    }

    /// The ceiling and character the desk is editing.
    ///
    /// This set's if it has any, bypassed or not - a bypass is a switch,
    /// and the thing it switches has to still be there - and otherwise
    /// the studio's default, which is what a set that has never said
    /// anything opened with and so what a first turn of the desk should
    /// start from. A mode `c` is still walking through is shown ahead of
    /// the one sounding, since it is the one the desk is choosing.
    pub(super) fn desk_limiter(&self) -> rustel_audio::LimiterSettings {
        let held = self
            .scenes
            .limiter()
            .held()
            .unwrap_or_else(|| self.ui_settings.limiter_default());
        match self.limiter_mode_due {
            Some((character, _)) => rustel_audio::LimiterSettings {
                threshold_db: held.threshold_db,
                character,
            },
            None => held,
        }
    }

    /// Give this set a limiter, or take the one it has away.
    ///
    /// The slot, not the sound: adding gives the set the studio's default
    /// ceiling and mode to start from, which the strip then edits, and
    /// removing leaves the set saying it has none - so a studio default
    /// switched on later does not hand one back to a set it was taken
    /// off.
    pub(super) fn set_limiter_slot(&mut self, present: bool) {
        if present == self.has_limiter_slot() {
            self.status = if present {
                format!(
                    "this set already has a limiter \u{b7} {} for its strip",
                    self.shortcut_or_menu(BindAction::Mixer, "View > Mixer")
                )
            } else {
                "this set has no limiter".to_owned()
            };
            self.dirty_frame = true;
            return;
        }
        // Added or taken away, a mode still settling was the old slot's.
        self.limiter_mode_due = None;
        let held = self.desk_limiter();
        let limiter = if present {
            SetLimiter::Says {
                bypassed: false,
                settings: held,
            }
        } else {
            SetLimiter::None
        };
        if self.scenes.set_limiter(limiter) {
            self.save_manifest_soon();
        }
        // The desk follows the slot. Gone, the selection cannot stay on a
        // strip nobody can see - every key the desk has would be acting on
        // a control that is not drawn. Added, the new strip is what the
        // player just asked for, and the status line's `c` and Enter are
        // about to be aimed at it.
        self.mixer.selected = if present {
            MixerTarget::Limiter
        } else if self.mixer.selected == MixerTarget::Limiter {
            MixerTarget::Master
        } else {
            self.mixer.selected
        };
        self.worker.master().set_limiter(self.live_master_limiter());
        self.status = if present {
            format!(
                "limiter added \u{b7} {} at {:.1} dBFS \u{b7} c for the mode, b to bypass",
                held.character.key(),
                held.threshold_db
            )
        } else {
            "limiter removed from this set".to_owned()
        };
        self.send_mixer_gains();
        self.dirty_frame = true;
    }

    /// Enter on the limiter's strip: the bypass switch.
    ///
    /// A switch rather than a fader taken to its floor. Reaching zero to
    /// turn something off means the one gesture both sets the ceiling and
    /// destroys it, and coming back gives a default rather than what was
    /// there - so the ceiling and the character stay put and only the
    /// switch moves.
    pub(super) fn toggle_master_limiter_bypass(&mut self) {
        // What the strip shows is what the switch keeps, a mode still
        // settling included, so the switch settles it.
        let settings = self.desk_limiter();
        self.limiter_mode_due = None;
        // Toggle from what is actually in the signal, which is what the strip
        // shows as `byp` - not from the set's recorded opinion. A set that
        // says nothing defers to the house, and the house opens with the
        // limiter off. Toggling the recorded opinion there would switch out
        // a limiter that is already out, and only a second Enter would
        // switch it in.
        let bypassed = self.live_master_limiter().is_some();
        if self
            .scenes
            .set_limiter(SetLimiter::Says { bypassed, settings })
        {
            self.save_manifest_soon();
        }
        self.worker.master().set_limiter(self.live_master_limiter());
        self.status = if bypassed {
            format!(
                "limiter bypassed \u{b7} {} at {:.1} dBFS is kept",
                settings.character.key(),
                settings.threshold_db
            )
        } else {
            format!(
                "limiter in \u{b7} {} at {:.1} dBFS",
                settings.character.key(),
                settings.threshold_db
            )
        };
        self.send_mixer_gains();
        self.dirty_frame = true;
    }

    /// `c` on the limiter's strip: the next character, round the ladder.
    ///
    /// Only the character. Whether the limiter is in is the bypass switch's
    /// question: a mode key that also turned it on would give the desk two
    /// controls for one thing. Bypassed, `c` still walks the ladder - you
    /// are choosing what it will sound like when you switch it back in.
    ///
    /// Each press is a step on the strip at once, repeats included; the
    /// sound and the set take the mode once the key has rested for
    /// `LIMITER_MODE_SETTLE`.
    fn cycle_limiter_character(&mut self) {
        let ladder = rustel_audio::LimiterCharacter::ALL;
        let shown = self.desk_limiter();
        let at = ladder
            .iter()
            .position(|character| *character == shown.character)
            .unwrap_or(0);
        let character = ladder[(at + 1) % ladder.len()];
        self.limiter_mode_due = Some((character, Instant::now() + LIMITER_MODE_SETTLE));
        self.status = if self.scenes.limiter().bypassed() {
            format!(
                "limiter {} at {:.1} dBFS \u{b7} bypassed, Enter switches it in",
                character.key(),
                shown.threshold_db
            )
        } else {
            format!(
                "limiter {} at {:.1} dBFS \u{b7} c for the next mode",
                character.key(),
                shown.threshold_db
            )
        };
        self.dirty_frame = true;
    }

    /// The mode `c` landed on, into the sound and the set file.
    ///
    /// Called once the key has rested, and before the set is put down, so
    /// a mode chosen on one set never lands on the next.
    pub(super) fn settle_limiter_mode(&mut self) {
        let Some((character, _)) = self.limiter_mode_due.take() else {
            return;
        };
        // The strip it was chosen on can be gone by now, and a mode is not
        // a reason to hand a set back the limiter it was just relieved of.
        if !self.has_limiter_slot() {
            return;
        }
        let held = self.desk_limiter();
        self.remember_master_limiter(rustel_audio::LimiterSettings {
            threshold_db: held.threshold_db,
            character,
        });
        self.worker.master().set_limiter(self.live_master_limiter());
        self.send_mixer_gains();
        self.dirty_frame = true;
    }

    fn mixer_fader_db(&self, target: MixerTarget) -> f32 {
        match target {
            MixerTarget::Input => self.mixer.input_gain_db,
            MixerTarget::Orbit(orbit) => self
                .mixer
                .orbit_gain_db
                .get(usize::from(orbit))
                .copied()
                .unwrap_or(0.0),
            MixerTarget::Master => self.master.gain_db(),
            // The held ceiling, not the live one. `desk_limiter` is what the
            // strip draws, so it is what a notch has to start from. When the
            // limiter is out of the signal there is no live value at all. A
            // fallback to the rail's floor would let one arrow press, wheel
            // notch or dock arrow move a bypassed ceiling to the floor, write
            // that into the set, and switch the limiter back in there.
            MixerTarget::Limiter => self.desk_limiter().threshold_db,
        }
    }

    pub(super) fn set_mixer_fader_db(&mut self, target: MixerTarget, db: f32) {
        let db = if db.is_finite() {
            (db * 10.0).round() / 10.0
        } else {
            0.0
        };
        match target {
            MixerTarget::Input => {
                let db = db.clamp(*INPUT_FADER_DB.start(), *INPUT_FADER_DB.end());
                if self.mixer.input_gain_db != db {
                    self.mixer.input_gain_db = db;
                    self.mixer.input_gain_pending = true;
                    // Still the studio's default for a device a set has
                    // never set: the microphone that needs a boost needs it
                    // in a new set too.
                    self.prefs.input_gain_tenths_db = Some((db * 10.0).round() as i32);
                    self.save_prefs_soon();
                    // And the set's own, per device, which wins when it
                    // exists: a condenser and a line-level synth want levels
                    // tens of decibels apart, and a set played through each
                    // must give each its own back.
                    if let Some(device) = self.prefs.audio_input.clone()
                        && self.scenes.set_input_gain_db(&device, db)
                    {
                        self.save_manifest_soon();
                    }
                }
                self.status = format!("input {db:+.1} dB");
            }
            MixerTarget::Orbit(orbit) => {
                let db = db.clamp(*ORBIT_FADER_DB.start(), *ORBIT_FADER_DB.end());
                self.mixer.set_orbit_gain_db(usize::from(orbit), db);
                self.status = format!("orbit {orbit} {db:+.1} dB");
            }
            MixerTarget::Master => self.set_master_gain_db(db),
            // The ceiling, not a gain, and only ever the ceiling: taking
            // the fader to its floor used to turn the limiter off, so one
            // gesture both set the ceiling and threw it away, and coming
            // back gave a default rather than what had been there. Off is
            // the bypass switch under the strip.
            MixerTarget::Limiter => {
                let threshold_db = db.clamp(LIMITER_FLOOR_DB, 0.0);
                let dragged = rustel_audio::LimiterSettings {
                    threshold_db,
                    character: self.desk_limiter().character,
                };
                self.remember_master_limiter(dragged);
                self.worker.master().set_limiter(self.live_master_limiter());
                self.status = if self.scenes.limiter().bypassed() {
                    format!(
                        "limiter {} at {threshold_db:.1} dBFS \u{b7} bypassed, Enter switches it in",
                        dragged.character.key()
                    )
                } else {
                    format!(
                        "limiter {} at {threshold_db:.1} dBFS",
                        dragged.character.key()
                    )
                };
            }
        }
        self.send_mixer_gains();
        self.dirty_frame = true;
    }

    /// Every changed fader, retained until the worker accepts its command.
    pub(super) fn send_mixer_gains(&mut self) {
        if self.output_latency_pending {
            self.apply_output_latency();
        }
        self.mixer.send_pending(|target, gain| match target {
            MixerTarget::Input => self.worker.try_set_input_gain(gain),
            MixerTarget::Orbit(orbit) => self.worker.try_set_orbit_gain(orbit, gain),
            MixerTarget::Master => {
                unreachable!("master gain uses its shared atomic")
            }
            MixerTarget::Limiter => {
                unreachable!("the limiter's ceiling is not a gain and rides its own atomic")
            }
        });
    }
}

/// The bottom of the limiter's ceiling fader. Below it the strip reads
/// "off", which is how the limiter is taken out without a second control -
/// and "off" still clips the output at full scale.
///
/// The settings' own, read here rather than restated: the sheet offers the
/// same range the strip does, and a file that named a ceiling outside it
/// would be a third opinion about one number.
use super::super::settings::MASTER_LIMITER_FLOOR_DB as LIMITER_FLOOR_DB;

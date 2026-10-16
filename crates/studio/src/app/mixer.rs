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

#[cfg(test)]
mod tests {
    mod mixer {
        //! Tests for `app/mixer.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;

        #[test]
        fn f4_mixer_panel_takes_slider_focus_and_keeps_its_edge() {
            use crate::mixer_panel::MixerEdge;

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.prefs.audio_input = Some("Mic".into());
            app.focus = Focus::Editor;
            app.armed_slider = Some((scene, call));
            let source = app.editor().source();

            precision_key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Mixer));
            assert!(app.armed_slider.is_none(), "the mixer owns the next arrows");
            app.regions.mixer = Rect::new(0, 18, 120, 16);
            // The limiter leads into the master, so from the master the input is
            // the next strip along and the limiter is the one before.
            precision_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Input);
            precision_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
            assert_eq!(app.mixer.input_gain_db, 1.0);
            assert_eq!(app.editor().source(), source, "the score slider stayed put");

            // Any unrelated Settings change reapplies the whole UI snapshot, and
            // the desk keeps the edge the settings say.
            app.ui_settings.show_full_paths = true;
            app.ui_settings.mixer_top = true;
            app.apply_ui_settings();
            app.prefs.set_ui_settings(&app.ui_settings);
            assert_eq!(
                app.mixer_panel.map(|panel| panel.edge),
                Some(MixerEdge::Top)
            );
            assert_eq!(app.prefs.mixer_top, Some(true));
            assert_eq!(app.prefs.input_gain_tenths_db, Some(10));
        }

        #[test]
        fn mixer_fader_focus_is_released_when_the_score_has_no_faders() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.open_mixer_panel(true);
            app.regions.mixer = Rect::new(0, 18, 120, 12);
            app.focus = Focus::Panel(PanelKind::Mixer);
            app.mixer_on_faders = true;
            app.mixer_fader = 7;

            // This is the state left when a previous score had a mixer fader and the
            // next score evaluates without one. The first arrow must go to a real
            // strip instead of being swallowed by the vanished fader block.
            precision_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert!(!app.mixer_on_faders, "a vanished fader cannot keep focus");
            assert_eq!(app.mixer_fader, 0);
        }

        /// The mixer's desk is not a picture of the score's sliders: it is the
        /// sliders. A press on a fader's rail drives the same control the inline
        /// pill does, through the same write path, so the text, the running
        /// engine and both pictures never disagree.
        #[test]
        fn a_score_fader_on_the_mixer_desk_drags_the_slider_itself() {
            use crate::mixer_panel::{FaderDesk, MixerEdge, fader_desk, parts};

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            precision_key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.regions.mixer = Rect::new(0, 18, 120, 12);
            let desk = parts(app.regions.mixer, MixerEdge::Bottom)
                .expect("a band that tall is a desk")
                .desk;
            let strips = app.mixer_targets().len();
            let widths = app.mixer_strip_widths();
            let layout = fader_desk(desk, &widths, 1).expect("room for the score's one fader");
            let cell = layout.cell(0).expect("the fader is drawn");
            let rail = FaderDesk::rail(cell).expect("and so is its rail");

            // The fader is named after the call it sits in, so a performer reading
            // the desk knows what the knob does.
            let faders = app.mixer_faders();
            assert_eq!(faders.len(), 1);
            assert_eq!(faders[0].label, "gain");

            // Pressing the floor of the rail takes the slider to its floor; the
            // drag follows, and the release carries the last motion.
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                rail.x,
                rail.y,
            );
            assert!(
                matches!(app.pointer, Some(Pointer::MixerFader { .. })),
                "the press keeps the fader for the drag: {:?}",
                app.pointer
            );
            assert_value(&app, scene, call, 0.0);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                rail.x + rail.width / 2,
                rail.y,
            );
            let middle = value(&app, scene, call);
            assert!(middle > 0.4 && middle < 0.6, "halfway along: {middle}");
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                rail.right() - 1,
                rail.y,
            );
            assert_value(&app, scene, call, 1.0);
            assert!(app.pointer.is_none(), "the release lets it go");

            // A press on the fader's label chooses it without moving it - the same
            // manners a press on a strip's label has.
            let before = value(&app, scene, call);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                cell.x,
                cell.y,
            );
            assert_value(&app, scene, call, before);
            assert!(app.mixer_on_faders, "the keys are on the fader block");

            // And the keys drive it: Tab crosses the desk, ← → move the fader.
            app.focus = Focus::Panel(PanelKind::Mixer);
            precision_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
            let stepped = value(&app, scene, call);
            assert!(stepped < before, "← lowered it: {before} → {stepped}");
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            assert!(!app.mixer_on_faders, "Tab wrapped back to the strips");
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            assert!(app.mixer_on_faders || strips > 1, "and walks back across");
        }

        /// The limiter's strip answers the pointer and the mode key. Its handle
        /// is a ceiling, not a gain, and a press can take it.
        #[test]
        fn the_limiter_strip_takes_the_pointer_and_the_mode_key() {
            use crate::mixer_panel::{MixerEdge, meter_rows, parts};

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd*4\")\n");
            // The strip exists because this set has a limiter; the pointer and
            // the mode key are what it is for.
            app.set_limiter_slot(true);
            precision_key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.regions.mixer = Rect::new(0, 18, 120, 12);
            let desk = parts(app.regions.mixer, MixerEdge::Bottom)
                .expect("a band that tall is a desk")
                .desk;
            let (top, bottom) = meter_rows(desk).expect("meter rows");
            let strip = app
                .mixer_targets()
                .iter()
                .position(|target| *target == MixerTarget::Limiter)
                .expect("the desk has a limiter strip");
            let x = desk.x + strip as u16 * crate::mixer_panel::STRIP_WIDTH;

            use crate::scenes::SetLimiter;
            app.mixer.selected = MixerTarget::Limiter;
            app.focus = Focus::Panel(PanelKind::Mixer);

            // `c` walks the characters and leaves everything else alone. The mode
            // key used to double as an on switch, so the desk had two controls for
            // whether the limiter was in and neither said which.
            let before = app.desk_limiter();
            // Not `m`, which on a mixer is mute: a hand reaching to silence a
            // strip must not find it has changed how the limiter sounds instead.
            precision_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
            assert_eq!(app.desk_limiter(), before, "m is not the mode key");
            precision_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            let stepped = app.desk_limiter();
            assert_ne!(stepped.character, before.character, "c walks the ladder");
            assert_eq!(
                stepped.threshold_db, before.threshold_db,
                "and leaves the ceiling"
            );

            // A held key repeats, and every repeat is a step on the strip, the way
            // held arrows walk the theme picker. The sound waits for the key to
            // rest: switching the character on every repeat was a run of audible
            // changes on the way to the one that was wanted.
            let ladder = rustel_audio::LimiterCharacter::ALL;
            let rung = |character| {
                ladder
                    .iter()
                    .position(|rung| *rung == character)
                    .expect("on the ladder")
            };
            for _ in 0..2 {
                precision_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            }
            let landed = app.desk_limiter();
            assert_eq!(
                rung(landed.character),
                (rung(before.character) + 3) % ladder.len(),
                "every press is a step, repeats included"
            );
            assert_eq!(
                app.worker.master().limiter(),
                Some(before),
                "and the sound has not moved while the key is still going"
            );
            assert_eq!(app.scenes.limiter().held(), Some(before), "nor has the set");
            // Rested, the sound and the set take the mode the key stopped on.
            app.settle_limiter_mode();
            assert_eq!(app.worker.master().limiter(), Some(landed));
            assert_eq!(app.scenes.limiter().held(), Some(landed));

            // A press on its meter takes the handle and a drag moves the
            // ceiling. A drag to the floor does not turn the limiter off.
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, bottom);
            assert!(
                matches!(app.pointer, Some(Pointer::MixerPanelFader)),
                "the press kept the handle: {:?}",
                app.pointer
            );
            assert_eq!(app.mixer.selected, MixerTarget::Limiter);
            let floor = app
                .worker
                .master()
                .limiter()
                .expect("the floor is a ceiling");
            assert!(
                (floor.threshold_db - crate::settings::MASTER_LIMITER_FLOOR_DB).abs() < 1e-6,
                "the bottom row is the lowest ceiling, not off: {floor:?}"
            );
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, top);
            let raised = app.worker.master().limiter().expect("still in");
            assert!(
                raised.threshold_db > -1.0,
                "the top row is the ceiling: {}",
                raised.threshold_db
            );

            // Enter is the bypass switch, and what it switches out it keeps.
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.scenes.limiter().bypassed(), "{}", app.status);
            assert_eq!(
                app.worker.master().limiter(),
                None,
                "bypassed is out of the signal"
            );
            assert_eq!(
                app.scenes.limiter().held(),
                Some(raised),
                "and the ceiling it was holding is still there to switch back in"
            );
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.worker.master().limiter(),
                Some(raised),
                "switched back in at exactly what it was, not at a default"
            );

            // And every one of those edits belongs to THIS SET, owed to its file
            // rather than to the preferences: a ceiling and a character are part
            // of how a set sounds, so the next set does not inherit them and this
            // one does not lose them.
            assert_eq!(
                *app.scenes.limiter(),
                SetLimiter::Says {
                    bypassed: false,
                    settings: raised
                },
                "the desk reached the set"
            );
            assert!(app.manifest_pending, "and the set file is owed a write");
            assert_eq!(
                app.ui_settings.master_limiter(),
                None,
                "while the studio's own default is untouched"
            );
        }

        /// The studio opens with the master limiter off, and the settings turn
        /// it on. The limiter delays all output by its lookahead, and the
        /// output clips at full scale with or without it.
        #[test]
        fn the_master_limiter_is_off_until_the_settings_ask_for_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd*4\")\n");
            assert_eq!(app.ui_settings.master_limiter(), None);
            assert_eq!(
                app.worker.master().limiter(),
                None,
                "and the bus agrees, so the output is undelayed"
            );

            let asked = rustel_audio::LimiterSettings {
                threshold_db: -2.5,
                character: rustel_audio::LimiterCharacter::Warm,
            };
            app.ui_settings.master_limiter_on = true;
            app.ui_settings.master_limiter_character = asked.character;
            app.ui_settings.master_limiter_ceiling_db = asked.threshold_db;
            app.apply_ui_settings();
            assert_eq!(
                app.worker.master().limiter(),
                Some(asked),
                "the sheet reaches the bus"
            );

            app.ui_settings.master_limiter_on = false;
            app.apply_ui_settings();
            assert_eq!(
                app.worker.master().limiter(),
                None,
                "and takes it away again"
            );

            // A set with a limiter of its own is not overwritten by the default:
            // the sheet says what a set STARTS with, and this one has moved on.
            let mine = rustel_audio::LimiterSettings {
                threshold_db: -9.0,
                character: rustel_audio::LimiterCharacter::Hard,
            };
            app.remember_master_limiter(mine);
            app.ui_settings.master_limiter_on = true;
            app.ui_settings.master_limiter_character = asked.character;
            app.apply_ui_settings();
            assert_eq!(
                app.worker.master().limiter(),
                Some(mine),
                "the set's own limiter outranks the studio's default"
            );

            // And a set that bypasses says so, rather than falling back to a
            // default that is in.
            app.toggle_master_limiter_bypass();
            app.apply_ui_settings();
            assert_eq!(
                app.worker.master().limiter(),
                None,
                "bypassed is a thing a set can say"
            );
            assert_eq!(
                app.scenes.limiter().held(),
                Some(mine),
                "and it is still holding what it switched out"
            );
        }

        /// Opening a set puts THAT set's limiter on the bus.
        ///
        /// A limiter is part of a sound, so a set opened mid-performance has to
        /// bring its own rather than keep playing through whatever the last set
        /// was left on. `switch_set` is the only place that can know, because it
        /// is the only place the set changes.
        #[test]
        fn opening_a_set_brings_its_own_limiter_to_the_bus() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd*4\")\n");

            // The set in hand is loud and unlimited; the studio's default is off.
            assert_eq!(app.worker.master().limiter(), None);

            // Another folder, whose set file asks for a limiter of its own.
            let theirs = rustel_audio::LimiterSettings {
                threshold_db: -6.5,
                character: rustel_audio::LimiterCharacter::Warm,
            };
            let elsewhere = directory.path().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).expect("another folder");
            std::fs::write(elsewhere.join("other.strudel"), "$: s(\"hh\")\n").expect("a score");
            let mut written = SceneSet::open(&elsewhere, "// starter").expect("set");
            written.set_limiter(crate::scenes::SetLimiter::Says {
                bypassed: false,
                settings: theirs,
            });
            written.persist_manifest().expect("write the set file");

            let opened = SceneSet::open(&elsewhere, "// starter").expect("reopen");
            app.switch_set(opened, "opened set");
            assert_eq!(
                app.worker.master().limiter(),
                Some(theirs),
                "the set that opened is playing with the limiter it asked for"
            );

            // `0` on the strip resets the ceiling and the mode to the studio
            // default. The limiter stays on the set.
            app.mixer.selected = MixerTarget::Limiter;
            app.reset_mixer_strip();
            assert_eq!(
                *app.scenes.limiter(),
                crate::scenes::SetLimiter::Says {
                    bypassed: false,
                    settings: app.ui_settings.limiter_default(),
                },
                "back to the studio's ceiling and mode, still this set's limiter"
            );
            assert_eq!(
                app.worker.master().limiter(),
                Some(app.ui_settings.limiter_default()),
                "and the bus hears it at once"
            );
            assert!(app.manifest_pending, "and the set file is owed that news");

            // Removing it is the other answer, and it sticks: the set says it has
            // none, so a studio default switched on afterwards does not hand one
            // back to it.
            app.set_limiter_slot(false);
            assert_eq!(*app.scenes.limiter(), crate::scenes::SetLimiter::None);
            assert_eq!(app.worker.master().limiter(), None);
            app.ui_settings.master_limiter_on = true;
            app.apply_ui_settings();
            assert_eq!(
                app.worker.master().limiter(),
                None,
                "the studio's default does not reach a set that has answered"
            );
        }

        /// F4 in zen pops the mixer up exactly like every other zen popup: it
        /// opens focused, and Esc closes it outright - unlike docked, where
        /// Esc only gives the keyboard back and leaves the desk standing.
        #[test]
        fn f4_pops_up_the_mixer_in_zen_and_esc_closes_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);

            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            assert!(
                app.mixer_panel.is_some(),
                "F4 did not open the mixer in zen"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Mixer));

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.mixer_panel.is_none(),
                "Esc did not close the mixer in zen"
            );
            assert_eq!(app.focus, Focus::Editor);
        }

        /// The mixer's strips follow the text: an orbit typed is a strip
        /// before anything is evaluated, an orbit deleted is not, a comment
        /// is nothing, and orbit 1 is there whenever a line is.
        #[test]
        fn orbits_are_read_off_the_text_as_it_is_typed() {
            assert_eq!(orbits_in_source(""), Vec::<u8>::new());
            assert_eq!(
                orbits_in_source("// $: s(\"bd\").orbit(3)"),
                Vec::<u8>::new()
            );
            assert_eq!(orbits_in_source("$: s(\"bd\")"), vec![1]);
            assert_eq!(
                orbits_in_source(
                    "$: s(\"bd\").orbit(2)\n$: note(\"c\").orbit(\"<3 4>\") // .orbit(9)"
                ),
                vec![1, 2, 3, 4]
            );
            assert_eq!(
                orbits_in_source("$: s(\"bd\").orbit(99)"),
                vec![1],
                "past the last orbit"
            );
            assert_eq!(
                orbits_in_source(
                    "$: s(\"in:1\").lpf(200)._scope().o(3)\n$: s(\"hh\").o(\"<2 4>\")"
                ),
                vec![1, 2, 3, 4],
                "the short form counts too"
            );
            assert_eq!(
                orbits_in_source("$: s(\"bd\").room(.5).o(2).orbit(5)"),
                vec![1, 2, 5]
            );

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prefs.audio_input = Some("Mic".into());
            // A set that has never said takes the studio's answer, and the
            // studio here hands one out - so the limiter's strip is on the
            // desk to lead the rest.
            app.ui_settings.master_limiter_on = true;
            app.scenes.current_mut().editor =
                Editor::new("$: s(\"bd\").orbit(2)\n$: s(\"hh\").orbit(5)").expect("editor");
            app.submit_lint();
            assert_eq!(app.orbits_in_code, vec![1, 2, 5]);
            assert_eq!(
                app.mixer_targets(),
                vec![
                    MixerTarget::Limiter,
                    MixerTarget::Master,
                    MixerTarget::Orbit(1),
                    MixerTarget::Orbit(2),
                    MixerTarget::Orbit(5),
                    MixerTarget::Input
                ],
                "the limiter leads into the master, and the score's strips follow"
            );
            app.scenes.current_mut().editor = Editor::new("$: s(\"bd\")").expect("editor");
            app.submit_lint();
            assert_eq!(app.orbits_in_code, vec![1], "gone with the line");
        }

        #[test]
        fn mixer_orbit_reset_reaches_the_engine_after_a_full_command_queue() {
            let (sender, receiver) = std::sync::mpsc::sync_channel(2);
            let send = |target, gain| sender.try_send((target, gain)).is_ok();
            let mut mixer = MixerState::new(None);
            mixer.set_orbit_gain_db(1, -4.0);
            mixer.send_pending(send);
            assert_eq!(receiver.try_recv().unwrap(), (MixerTarget::Input, 1.0));
            let (target, mut engine_gain) = receiver.try_recv().unwrap();
            assert_eq!(target, MixerTarget::Orbit(1));
            assert!((engine_gain - 0.630_957_37).abs() < 1e-6);

            // Other commands occupy the same two-slot worker queue when reset
            // is pressed. The reset must remain pending until there is room.
            sender.try_send((MixerTarget::Master, 0.0)).unwrap();
            sender.try_send((MixerTarget::Master, 0.0)).unwrap();
            mixer.set_orbit_gain_db(1, 0.0);
            mixer.send_pending(send);
            assert_eq!(mixer.orbit_gain_db[1], 0.0);
            assert!(mixer.orbit_gain_pending[1]);
            assert!(engine_gain < 1.0);
            assert_eq!(receiver.try_iter().count(), 2);

            mixer.send_pending(send);
            let (target, gain) = receiver.try_recv().unwrap();
            assert_eq!(target, MixerTarget::Orbit(1));
            engine_gain = gain;
            assert_eq!(engine_gain, 1.0, "the engine must actually return to unity");
            assert!(!mixer.orbit_gain_pending[1]);
            mixer.send_pending(send);
            assert!(
                receiver.try_recv().is_err(),
                "accepted gains are not replayed"
            );
        }

        #[test]
        fn mixer_gain_retries_drain_each_changed_fader_without_replaying_the_prefix() {
            let (sender, receiver) = std::sync::mpsc::sync_channel(2);
            let send = |target, gain| sender.try_send((target, gain)).is_ok();
            let mut mixer = MixerState::new(None);
            for orbit in 1..=5 {
                mixer.set_orbit_gain_db(orbit, -6.0);
            }
            mixer.send_pending(send);
            let mut delivered: Vec<_> = receiver.try_iter().collect();
            assert_eq!(delivered.len(), 2);

            // An unsent fader keeps only its newest value. A return to zero
            // dB is still a command, even if the engine never saw the old value.
            mixer.set_orbit_gain_db(5, -12.0);
            mixer.set_orbit_gain_db(5, 0.0);
            for _ in 0..2 {
                mixer.send_pending(send);
                delivered.extend(receiver.try_iter());
            }
            assert_eq!(
                delivered
                    .iter()
                    .map(|(target, _)| *target)
                    .collect::<Vec<_>>(),
                vec![
                    MixerTarget::Input,
                    MixerTarget::Orbit(1),
                    MixerTarget::Orbit(2),
                    MixerTarget::Orbit(3),
                    MixerTarget::Orbit(4),
                    MixerTarget::Orbit(5),
                ],
                "every changed fader fits after three drains of the two-slot queue"
            );
            assert_eq!(delivered.last().unwrap().1, 1.0);
            assert!(!mixer.input_gain_pending);
            assert!(mixer.orbit_gain_pending.iter().all(|pending| !*pending));
            mixer.send_pending(send);
            assert!(receiver.try_recv().is_err());
        }

        #[test]
        fn mixer_meter_only_orbits_ignore_fader_keys_and_can_reset_old_attenuation() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            app.mixer.selected = MixerTarget::Orbit(1);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
            assert_eq!(app.mixer.orbit_gain_db[1], 0.0);
            assert!(!app.mixer.orbit_gain_pending[1]);

            app.mixer.set_orbit_gain_db(1, -4.0);
            press(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
            assert_eq!(app.mixer.orbit_gain_db[1], 0.0);
        }

        /// No input chosen means no `in` strip: the mixer starts with the
        /// orbits and the master alone, and the strip comes with a choice -
        /// remembered, from the command line, or an input still open.
        #[test]
        fn the_in_strip_is_there_only_while_an_input_is_chosen() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prefs.audio_input = None;
            app.options.input = None;
            assert!(!app.input_chosen(), "none by default");
            assert_eq!(
                app.mixer_targets(),
                vec![MixerTarget::Master],
                "nothing to meter, no `in` strip, and no limiter on this set"
            );
            // The limiter's strip is there when the set has a limiter, and
            // this set takes the studio's answer because it has never said.
            app.ui_settings.master_limiter_on = true;
            assert_eq!(
                app.mixer_targets(),
                vec![MixerTarget::Limiter, MixerTarget::Master],
                "the output's own two"
            );
            app.prefs.audio_input = Some("Mic".into());
            assert_eq!(
                app.mixer_targets().last(),
                Some(&MixerTarget::Input),
                "chosen: the strip joins the end, behind the score's orbits"
            );
            app.prefs.audio_input = None;
            app.options.input = Some("Scarlett".into());
            assert!(app.input_chosen(), "the command line's choice counts");
            app.options.input = None;
            let snapshot = crate::StudioSnapshot {
                input_device: Some("Mic".into()),
                ..Default::default()
            };
            app.snapshot = Some(snapshot);
            assert!(app.input_chosen(), "and one still open");
        }

        /// F4 puts the desk along the bottom - the mixer panel, the keys on
        /// it - and takes it away again; Esc leaves it standing; View ▸ Mixer
        /// is the same switch, and the panel comes back the way it was left.
        #[test]
        fn f4_opens_the_mixer_panel_and_hides_it_again() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prefs.audio_input = Some("Mic".into());
            assert!(app.mixer_panel.is_none());
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            let panel = app.mixer_panel.expect("the panel opened");
            assert_eq!(panel.edge, MixerEdge::Bottom);
            assert_eq!(panel.rows, crate::mixer_panel::DEFAULT_ROWS);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Mixer));
            assert_eq!(app.prefs.mixer_panel, Some(true));
            assert!(app.shows_mixer(), "the picture is assembled for the panel");
            assert!(app.mixer_facts().is_some());
            // Esc hands the keyboard back and leaves the desk.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.focus, Focus::Editor);
            assert!(app.mixer_panel.is_some(), "sticky");
            assert_eq!(
                app.front_panel(),
                None,
                "and Esc from the score has nothing to close"
            );
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            assert!(app.mixer_panel.is_none(), "F4 again hides it");
            assert_eq!(app.prefs.mixer_panel, Some(false));
            app.dispatch_menu_action(crate::menu::MenuAction::Mixer)
                .expect("menu");
            assert!(app.mixer_panel.is_some(), "View ▸ Mixer is the same switch");
            // ^⇧M too.
            press(
                &mut app,
                KeyCode::Char('M'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert!(app.mixer_panel.is_none(), "^⇧M hides it");
            // Its edge and size are remembered.
            app.prefs.mixer_rows = Some(20);
            app.ui_settings.mixer_top = true;
            app.open_mixer_panel(false);
            let panel = app.mixer_panel.expect("open");
            assert_eq!((panel.edge, panel.rows), (MixerEdge::Top, 20));
            assert_eq!(app.focus, Focus::Editor, "opened without the keyboard");
        }

        /// On the desk ←/→ choose a strip, ↑/↓ move its fader, 0 puts it
        /// back, e flips the edge and +/- resize it; the input's fader climbs
        /// to +48 dB and is remembered; the master's is the master's.
        #[test]
        fn the_desks_keys_choose_strips_and_move_faders() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prefs.audio_input = Some("Mic".into());
            // This set is given a limiter, which is what puts its strip on the
            // desk at all. Adding one selects it; the desk is put back on the
            // master here, which is where it opens.
            app.set_limiter_slot(true);
            app.mixer.selected = MixerTarget::Master;
            app.scenes.current_mut().editor = Editor::new("$: s(\"bd\").orbit(2)").expect("editor");
            app.submit_lint();
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            // The desk has room on screen; without it the keys are the score's.
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            // Strips: limiter · master · orbit 1 · orbit 2 · in. The desk
            // reads left to right into the output, so the last thing before it
            // goes out sits against it; the master is still chosen first.
            assert_eq!(app.mixer.selected, MixerTarget::Master);
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(
                app.mixer.selected,
                MixerTarget::Limiter,
                "the limiter is beside the master it holds"
            );
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Master);
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Input, "round the end");
            // Round from the end: limiter, master, the orbits, back to `in`.
            // With → rather than Enter, which on the limiter is its bypass
            // switch - the one strip where the next strip is not what a hand
            // already on it wants.
            for _ in 0..5 {
                press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            }
            assert_eq!(app.mixer.selected, MixerTarget::Input);
            // Enter there really is the switch, and does not move on. `in` is
            // the last strip, so one step round lands on the limiter.
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Limiter);
            let held = app.desk_limiter();
            // A strip is drawn only for a limiter the set has, so the limiter
            // under the keys is in the signal. One Enter takes it out.
            assert!(
                app.worker.master().limiter().is_some(),
                "a strip means a limiter, and a limiter means in the signal"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Limiter, "Enter stayed put");
            assert!(
                app.worker.master().limiter().is_none(),
                "and one Enter took it out"
            );
            assert!(app.scenes.limiter().bypassed(), "the switch is thrown");
            assert_eq!(
                app.scenes.limiter().held(),
                Some(held),
                "keeping its ceiling"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.worker.master().limiter().is_some(),
                "and the next one brings it back"
            );
            // And the limiter leads, so one step back is round to `in`.
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Input);
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            assert_eq!(app.mixer.input_gain_db, 2.0);
            assert_eq!(app.prefs.input_gain_tenths_db, Some(20), "remembered");
            press(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
            assert_eq!(app.mixer.input_gain_db, 1.9, "a tenth with shift");
            press(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
            assert_eq!(app.mixer.input_gain_db, 0.0);
            for _ in 0..100 {
                press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            }
            assert_eq!(
                app.mixer.input_gain_db, 48.0,
                "the input's fader stops at its top, high enough for a phone's microphone"
            );
            // `in` is the last strip, so one step round is the limiter and
            // the next is the master.
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Limiter);
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(app.mixer.selected, MixerTarget::Master);
            let before = app.master.gain_db();
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert!(
                app.master.gain_db() < before,
                "the master's fader is the master's"
            );
            press(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
            assert_eq!(app.master.gain_db(), 0.0);
            // The desk itself: e flips it, +/- resize it, both remembered.
            press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            assert_eq!(
                app.mixer_panel.map(|panel| panel.edge),
                Some(MixerEdge::Top)
            );
            assert!(app.ui_settings.mixer_top);
            assert_eq!(app.prefs.mixer_top, Some(true));
            press(&mut app, KeyCode::Char('+'), KeyModifiers::NONE);
            assert_eq!(
                app.mixer_panel.map(|panel| panel.rows),
                Some(crate::mixer_panel::DEFAULT_ROWS + 1)
            );
            assert_eq!(
                app.prefs.mixer_rows,
                Some(crate::mixer_panel::DEFAULT_ROWS + 1)
            );
            press(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
            assert_eq!(
                app.mixer_panel.map(|panel| panel.rows),
                Some(crate::mixer_panel::DEFAULT_ROWS)
            );
        }

        /// A press on the desk's `in` strip takes the input fader to that
        /// row - on the input's own scale, up to +48 at the top - and keeps
        /// it for the drag; an orbit's strip is chosen but takes no fader;
        /// the wheel over the master's strip nudges the master.
        #[test]
        fn a_press_on_a_strip_takes_its_fader_and_a_drag_keeps_it() {
            use crate::mixer_panel::{fader_db_at, parts, strip_width};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prefs.audio_input = Some("Mic".into());
            app.ui_settings.master_limiter_on = true;
            app.scenes.current_mut().editor = Editor::new("$: s(\"bd\").orbit(2)").expect("editor");
            app.submit_lint();
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            // The desk along the bottom of a hundred-and-twenty-column frame.
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.focus, Focus::Editor);
            let desk = parts(app.regions.mixer, MixerEdge::Bottom)
                .expect("the desk has a place on screen")
                .desk;
            // Strips: limiter, master, orbit 1, orbit 2, in. The limiter's
            // strip is narrower, so the test sums the strip widths. A fixed
            // stride would aim every later press at the wrong strip.
            let kinds: Vec<_> = app
                .mixer_targets()
                .into_iter()
                .map(|target| match target {
                    MixerTarget::Limiter => MixerStripKind::Reduction {
                        reduction_db: 0.0,
                        bypassed: false,
                    },
                    _ => MixerStripKind::Level,
                })
                .collect();
            let strip_x = |strip: usize| {
                desk.x + kinds[..strip].iter().copied().map(strip_width).sum::<u16>() + 1
            };
            // By what a strip IS rather than where it sits, so re-ordering the
            // desk moves the presses with it instead of quietly aiming them at
            // the neighbour.
            let at = |target: MixerTarget| {
                app.mixer_targets()
                    .iter()
                    .position(|candidate| *candidate == target)
                    .expect("the desk has that strip")
            };
            let (master, orbit_one, input, limiter) = (
                at(MixerTarget::Master),
                at(MixerTarget::Orbit(1)),
                at(MixerTarget::Input),
                at(MixerTarget::Limiter),
            );
            let (top, bottom) = (desk.y + 1, desk.bottom() - 2);
            let row = top + (bottom - top) / 4;
            let expected = fader_db_at(desk, row, (-60.0, 48.0));
            assert!(
                app.click_mixer_panel(strip_x(input), row),
                "the press is the desk's"
            );
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Mixer),
                "and gives it the keyboard"
            );
            assert_eq!(app.mixer.selected, MixerTarget::Input);
            assert_eq!(app.mixer.input_gain_db, (expected * 10.0).round() / 10.0);
            assert!(matches!(app.pointer, Some(Pointer::MixerPanelFader)));
            // The drag follows the row, even off the strip.
            app.drag_mixer_panel_fader(bottom + 3);
            assert_eq!(app.mixer.input_gain_db, -60.0, "dragged to the floor");
            app.drag_mixer_panel_fader(top);
            assert_eq!(
                app.mixer.input_gain_db, 48.0,
                "the top row is the input's own ceiling"
            );
            // An orbit's strip is chosen, and is a meter alone.
            assert!(app.click_mixer_panel(strip_x(orbit_one), row));
            assert_eq!(app.mixer.selected, MixerTarget::Orbit(1));
            assert!(matches!(app.pointer, Some(Pointer::Panel)));
            // The wheel over the master's strip nudges the master.
            let before = app.master.gain_db();
            assert!(app.scroll_mixer_panel(strip_x(master), row, -1.0));
            assert_eq!(app.mixer.selected, MixerTarget::Master);
            assert!(app.master.gain_db() < before);
            // And over every other strip that has a fader. The limiter's is a
            // ceiling, and the panel used to leave it out of the wheel while
            // the dock's mixer turned it.
            let ceiling = app.desk_limiter().threshold_db;
            assert!(app.scroll_mixer_panel(strip_x(limiter), row, -1.0));
            assert_eq!(app.mixer.selected, MixerTarget::Limiter);
            assert!(
                app.desk_limiter().threshold_db < ceiling,
                "the wheel moved the limiter's ceiling: {ceiling} -> {}",
                app.desk_limiter().threshold_db
            );
            // An orbit is a meter, so its strip says so rather than moving.
            assert!(app.scroll_mixer_panel(strip_x(orbit_one), row, -1.0));
            assert!(app.status.contains("is a meter"), "{}", app.status);
            // The master's press is on the footer's scale: the top row is +6.
            assert!(app.click_mixer_panel(strip_x(master), top));
            assert_eq!(app.master.gain_db(), 6.0);
            // The label and the readout rows choose a strip and move nothing.
            app.mixer.input_gain_db = 3.0;
            assert!(app.click_mixer_panel(strip_x(input), desk.y));
            assert_eq!(app.mixer.selected, MixerTarget::Input);
            assert_eq!(app.mixer.input_gain_db, 3.0, "the label is not the fader");
            assert!(matches!(app.pointer, Some(Pointer::Panel)));
            assert!(app.click_mixer_panel(strip_x(input), desk.bottom() - 1));
            assert_eq!(app.mixer.input_gain_db, 3.0, "nor is the readout");
            // Off the desk the press is somebody else's.
            assert!(!app.click_mixer_panel(5, 5));
        }

        /// The MIDI log and the pads on the desk's devices block are text to
        /// drag out and copy: a press starts a character selection there, the
        /// drag extends it, and `c` copies what was taken - ahead of `c`'s
        /// limiter meaning, which must not steal the key mid-copy.
        #[test]
        fn the_devices_block_selects_and_copies_what_it_shows() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            let (area, lines) = app.mixer_devices_block().expect("the desk draws the block");
            assert!(!lines.is_empty(), "the block has rows: {lines:?}");

            // A press on the first row starts the selection.
            assert!(app.click_mixer_panel(area.x, area.y));
            assert!(matches!(app.pointer, Some(Pointer::MixerText { .. })));
            assert!(app.mixer_selection.is_some(), "the press began a drag");

            // The drag takes a run of characters across the row.
            let taken = lines[0].chars().take(4).collect::<String>();
            app.drag_mixer_selection(
                area.x + taken.chars().count() as u16,
                area.y,
                crate::textblock::Granularity::Character,
            );
            let text = app.mixer_selection_text().expect("something was selected");
            assert_eq!(text, taken, "the drag took the characters it crossed");

            // `c` copies it, and the selection stays for another copy.
            assert!(app.handle_mixer_panel_key(KeyCode::Char('c'), false, false));
            assert_eq!(app.clipboard.get_text().expect("copied"), text);
            assert_eq!(app.mixer_selection_text().as_deref(), Some(text.as_str()));
        }

        /// A mode still settling does not outlive what the desk did next: a
        /// reset puts the studio's default in its place, and a limiter taken
        /// off the set is not handed back by a mode it no longer has.
        #[test]
        fn a_settling_limiter_mode_gives_way_to_a_reset_and_a_removal() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.set_limiter_slot(true);
            app.mixer.selected = MixerTarget::Limiter;
            app.focus = Focus::Panel(PanelKind::Mixer);
            let default = app.ui_settings.limiter_default();

            press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            assert_ne!(
                app.desk_limiter().character,
                default.character,
                "c stepped the strip"
            );
            press(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
            app.settle_limiter_mode();
            assert_eq!(app.desk_limiter(), default, "the reset held");
            assert_eq!(app.worker.master().limiter(), Some(default));

            press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            app.settle_limiter_mode();
            assert!(!app.has_limiter_slot(), "the removal held");
            assert!(app.worker.master().limiter().is_none());
        }

        /// The wheel over the limiter's strip moves its ceiling.
        #[test]
        fn the_wheel_over_the_limiter_strip_moves_its_ceiling() {
            use crate::mixer_panel::parts;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.ui_settings.master_limiter_on = true;
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            let desk = parts(app.regions.mixer, MixerEdge::Bottom)
                .expect("the desk has a place on screen")
                .desk;
            // Strips: limiter · master · orbits · in. The limiter leads, because
            // the desk reads left to right into the output.
            let limiter_x = desk.x + 1;
            let (top, bottom) = (desk.y + 1, desk.bottom() - 2);
            let row = top + (bottom - top) / 4;
            // The desk opens with the limiter off now: it became a setting, and a
            // setting is asked for rather than assumed. `c` walks the characters
            // and, once the key rests, gives this SET a limiter of its own, and
            // the ceiling that comes with it is what the wheel moves.
            app.mixer.selected = MixerTarget::Limiter;
            app.focus = Focus::Panel(PanelKind::Mixer);
            press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            app.settle_limiter_mode();
            let before = app
                .worker
                .master()
                .limiter()
                .expect("c turned it on")
                .threshold_db;

            assert!(
                app.scroll_mixer_panel(limiter_x, row, -1.0),
                "the wheel over the desk is the desk's"
            );
            assert_eq!(app.mixer.selected, MixerTarget::Limiter);
            assert_eq!(
                app.worker
                    .master()
                    .limiter()
                    .expect("still on")
                    .threshold_db,
                before - VOLUME_SCROLL_STEP_DB,
                "and it moved the ceiling by one notch"
            );
        }

        /// A notch on a bypassed limiter starts from its held ceiling, not from
        /// the rail's floor, and the limiter comes back in at the new ceiling.
        #[test]
        fn a_bypassed_ceiling_survives_one_notch() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            // This set is given a limiter, so there is a ceiling to bypass and
            // a ceiling to keep.
            app.set_limiter_slot(true);
            app.mixer.selected = MixerTarget::Limiter;
            app.focus = Focus::Panel(PanelKind::Mixer);
            assert!(
                app.worker.master().limiter().is_some(),
                "added means in the signal"
            );
            let held = app.desk_limiter().threshold_db;

            // Enter is the bypass switch: out of the signal, ceiling kept.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.worker.master().limiter().is_none(),
                "bypassed means out of the signal"
            );
            assert_eq!(
                app.desk_limiter().threshold_db,
                held,
                "and what it switched out it kept"
            );

            // One notch down moves the held ceiling one notch - not to the floor
            // it would have started from if the read had been the live one.
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(
                app.desk_limiter().threshold_db,
                held - FADER_KEY_STEP_DB,
                "one notch down is one notch down, while bypassed"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.worker
                    .master()
                    .limiter()
                    .expect("switched back in")
                    .threshold_db,
                held - FADER_KEY_STEP_DB,
                "and switching back in brings in that ceiling"
            );
        }

        /// The desk draws a limiter strip only when the set has a limiter. Each
        /// Enter on that strip changes the sound.
        #[test]
        fn the_limiter_strip_is_there_exactly_when_the_set_has_one() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            assert!(
                app.worker.master().limiter().is_none(),
                "the studio opens with the limiter out"
            );
            assert!(
                !app.mixer_targets().contains(&MixerTarget::Limiter),
                "and with no strip for one"
            );
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.mixer.selected = MixerTarget::Limiter;
            app.focus = Focus::Panel(PanelKind::Mixer);
            // A selection left on a strip that is not drawn is not a control:
            // the keys that act on it do nothing at all.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            assert!(
                app.worker.master().limiter().is_none(),
                "keys on a strip nobody can see change nothing"
            );
            assert_eq!(
                *app.scenes.limiter(),
                crate::scenes::SetLimiter::Defer,
                "and say nothing"
            );

            assert_eq!(
                app.mixer.selected,
                MixerTarget::Master,
                "and a key that walks the strips lands on one that is drawn"
            );

            // Added, it is in the signal - adding one is asking for one - and
            // the desk is on the strip that just appeared.
            app.set_limiter_slot(true);
            assert!(app.mixer_targets().contains(&MixerTarget::Limiter));
            assert_eq!(app.mixer.selected, MixerTarget::Limiter);
            let held = app.desk_limiter();
            assert_eq!(app.worker.master().limiter(), Some(held));

            // And from there each Enter changes a sound.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.worker.master().limiter().is_none(),
                "one Enter takes it out"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.worker.master().limiter(),
                Some(held),
                "and the next brings back the limiter it was, not a default"
            );

            // `b` is the same switch, for the hand already on `c`.
            press(&mut app, KeyCode::Char('b'), KeyModifiers::NONE);
            assert!(app.worker.master().limiter().is_none(), "b bypasses");
            press(&mut app, KeyCode::Char('b'), KeyModifiers::NONE);
            assert_eq!(
                app.worker.master().limiter(),
                Some(held),
                "and switches back in"
            );

            // Taken away, the strip goes with it, the selection comes off it,
            // and the set says so - so a studio default switched on later does
            // not hand one back.
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            assert!(!app.mixer_targets().contains(&MixerTarget::Limiter));
            assert_eq!(app.mixer.selected, MixerTarget::Master);
            assert_eq!(*app.scenes.limiter(), crate::scenes::SetLimiter::None);
            assert!(app.worker.master().limiter().is_none());
            app.ui_settings.master_limiter_on = true;
            assert!(
                !app.has_limiter_slot(),
                "the studio's default does not overrule a set that has answered"
            );
        }

        /// Shift+Tab on the focused desk crosses the desk backwards. It arrives
        /// as `BackTab`, not as `Tab` with a modifier. The score is not
        /// outdented and the desk keeps the keyboard.
        #[test]
        fn shift_tab_on_the_desk_stays_on_the_desk() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.mixer = Rect::new(0, 22, 120, 16);
            app.scenes.current_mut().editor = Editor::new("    s(\"bd\")").expect("editor");
            let before = app.editor().source();
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Mixer),
                "the desk has the keyboard"
            );
            assert!(
                app.dispatch_panel_key(PanelKind::Mixer, KeyCode::BackTab, false, false, false)
                    .expect("dispatch"),
                "the desk answers Shift+Tab itself"
            );
            press(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), before, "the score is untouched");
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Mixer),
                "and the desk keeps the keys"
            );
        }

        /// A desk the terminal has no room for is opened but not given the
        /// keyboard, and its keys fall through to the score: nothing on screen
        /// must never own the arrows.
        #[test]
        fn a_desk_with_no_room_takes_no_keys() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 80, 24);
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            assert!(app.mixer_panel.is_some(), "opened");
            assert_eq!(app.focus, Focus::Editor, "but not focused");
            assert!(app.status.contains("no room"), "{}", app.status);
            // Even focused by hand, an empty region means the keys are not its.
            app.focus = Focus::Panel(PanelKind::Mixer);
            app.regions.mixer = Rect::default();
            assert!(
                !app.dispatch_panel_key(PanelKind::Mixer, KeyCode::Up, false, false, false)
                    .expect("dispatch")
            );
            assert_eq!(
                app.master.gain_db(),
                0.0,
                "the master fader was not touched"
            );
        }

        /// The mixer's keys drive its strips: Enter walks them, the arrows
        /// move the selected fader a decibel, 0 puts it back; the input's
        /// fader is remembered, the master's is the master's.
        #[test]
        fn the_mixers_keys_walk_the_strips_and_move_their_faders() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prefs.audio_input = Some("Mic".into());
            // Given a limiter, so the strip it leads with is on the desk.
            app.set_limiter_slot(true);
            app.toggle_viz_dock(0);
            app.viz_add(WidgetKind::Mixer);
            let dock = app.viz_docks[0].as_mut().expect("dock");
            dock.selected = app.prefs.visuals[0]
                .widgets
                .iter()
                .position(|widget| widget.kind == WidgetKind::Mixer)
                .expect("the mixer joined the dock");
            app.focus = Focus::Panel(PanelKind::Viz);
            app.viz_focus = 0;
            assert_eq!(app.focused_widget_kind(), Some(WidgetKind::Mixer));
            assert!(
                app.mixer_facts().is_some(),
                "the picture is assembled once a dock shows it"
            );

            // Round the strips: limiter, master, (no orbits yet), input, and
            // round again. The limiter leads because the desk reads left to
            // right into the output, so the last thing before it goes out sits
            // against it.
            app.mixer.selected = MixerTarget::Master;
            assert!(app.handle_viz_key(KeyCode::Enter, false, false));
            assert_eq!(app.mixer.selected, MixerTarget::Input);
            assert!(app.handle_viz_key(KeyCode::Right, false, false));
            assert!(app.handle_viz_key(KeyCode::Right, false, false));
            assert_eq!(app.mixer.input_gain_db, 2.0);
            assert_eq!(app.prefs.input_gain_tenths_db, Some(20), "remembered");
            assert!(app.handle_viz_key(KeyCode::Left, false, false));
            assert_eq!(app.mixer.input_gain_db, 1.0);
            assert!(app.handle_viz_key(KeyCode::Char('0'), false, false));
            assert_eq!(app.mixer.input_gain_db, 0.0);
            for _ in 0..100 {
                assert!(app.handle_viz_key(KeyCode::Right, false, false));
            }
            assert_eq!(
                app.mixer.input_gain_db, 48.0,
                "the input's fader stops at its top"
            );

            assert!(app.handle_viz_key(KeyCode::Enter, false, false));
            assert_eq!(app.mixer.selected, MixerTarget::Limiter, "round the end");
            assert!(app.handle_viz_key(KeyCode::Enter, false, false));
            assert_eq!(app.mixer.selected, MixerTarget::Master);
            let before = app.master.gain_db();
            assert!(app.handle_viz_key(KeyCode::Left, false, false));
            assert!(
                app.master.gain_db() < before,
                "the master's fader is the master's"
            );
            assert!(app.handle_viz_key(KeyCode::Char('0'), false, false));
            assert_eq!(app.master.gain_db(), 0.0);

            // Shift keeps the dock's own arrows: the mixer can still be moved.
            let kind_before = app.focused_widget_kind();
            app.handle_viz_key(KeyCode::Left, false, true);
            assert_eq!(app.mixer.input_gain_db, 48.0, "shift did not touch a fader");
            let _ = kind_before;
        }

        /// The complaint this exists for: a set was reopened at unity every
        /// time, so the fader had to be found and set again before the first
        /// bar. It now reopens at the level it was left at.
        #[test]
        fn a_set_reopens_at_the_master_level_it_was_left_at() {
            let club = tempfile::tempdir().expect("temp dir");
            write(club.path(), "one.strudel");
            write(club.path(), "two.strudel");
            let mut app = app_over(club.path());
            let set = SceneSet::open(club.path(), "// starter").expect("set");
            app.switch_set(set, "opened set");

            app.set_master_gain_db(-7.5);
            assert!((app.master.gain_db() - -7.5).abs() < 0.05);
            app.flush_dirty_scenes();
            app.wait_for_saves();
            app.persist_manifest();

            // Away to a set that never moved its fader, which opens at unity
            // rather than inheriting the club set's -7.5.
            let quiet = tempfile::tempdir().expect("temp dir");
            write(quiet.path(), "a.strudel");
            write(quiet.path(), "b.strudel");
            let other = SceneSet::open(quiet.path(), "// starter").expect("set");
            app.switch_set(other, "opened set");
            assert!(
                app.master.gain_db().abs() < 0.05,
                "a set that never said opens at unity, not at the last set's level: {}",
                app.master.gain_db()
            );

            // And back: the level it was left at.
            let again = SceneSet::open(club.path(), "// starter").expect("set");
            app.switch_set(again, "opened set");
            assert!(
                (app.master.gain_db() - -7.5).abs() < 0.05,
                "back at its own level: {}",
                app.master.gain_db()
            );
            // Restored quietly: the status is the set being opened, not a fader
            // nobody touched announcing itself over it.
            assert!(
                !app.status.starts_with("master"),
                "the open says itself, not the restored fader: {}",
                app.status
            );
        }

        /// Each input device gets its own level back. A microphone that needs
        /// +12 and a synth that needs -3 are one set's two sources.
        #[test]
        fn each_input_device_keeps_its_own_level_in_the_set() {
            let directory = tempfile::tempdir().expect("temp dir");
            write(directory.path(), "one.strudel");
            write(directory.path(), "two.strudel");
            let mut app = app_over(directory.path());
            let set = SceneSet::open(directory.path(), "// starter").expect("set");
            app.switch_set(set, "opened set");

            app.remember_audio_input(Some("Mic"));
            app.set_mixer_fader_db(MixerTarget::Input, 12.0);
            app.remember_audio_input(Some("Synth"));
            app.set_mixer_fader_db(MixerTarget::Input, -3.0);

            assert_eq!(app.scenes.input_gain_db("Mic"), Some(12.0));
            assert_eq!(app.scenes.input_gain_db("Synth"), Some(-3.0));

            // Switching source brings that source's level straight back.
            app.remember_audio_input(Some("Mic"));
            assert!((app.mixer.input_gain_db - 12.0).abs() < 0.05);
            app.remember_audio_input(Some("Synth"));
            assert!((app.mixer.input_gain_db - -3.0).abs() < 0.05);
        }

        /// Letting go of a desk fader is the desk's alone. A score drag whose
        /// release never arrived (let go outside the window) must not be pulled to
        /// where the desk's fader was let go.
        #[test]
        fn the_desk_fader_release_never_reaches_the_score() {
            use crate::mixer_panel::{meter_rows, parts};
            let directory = tempfile::tempdir().unwrap();
            let (mut app, _scene) = plain_app(
                directory.path(),
                "$: s(\"bd hh sd hh\")\n$: s(\"cp ~ cp ~\")\n$: s(\"~ rim\")\n",
            );
            app.ui_settings.master_limiter_on = false;
            app.open_mixer_panel(false);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))
                .expect("test terminal");
            let mut draw = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                    .expect("draw");
            };
            draw(&mut app);
            assert!(!app.regions.mixer.is_empty(), "the desk is on screen");

            // A drag in the score whose release never arrives.
            let score = app.focused_map().expect("the score is drawn").area();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                score.x + 2,
                score.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                score.x + 6,
                score.y,
            );
            draw(&mut app);
            let selection = app.editor().primary_selection();
            let viewport = app.editor().viewport();
            assert!(!selection.is_empty(), "a score drag is live");

            let desk = parts(app.regions.mixer, app.mixer_panel.unwrap().edge)
                .expect("a desk")
                .desk;
            let (top, bottom) = meter_rows(desk).expect("meter rows");
            let x = (desk.x..desk.right())
                .find(|&x| app.mixer_pointer_shape(x, top).is_some())
                .expect("a strip with a fader");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, bottom);
            assert!(
                matches!(app.pointer, Some(Pointer::MixerPanelFader)),
                "{:?}",
                app.pointer
            );
            assert_eq!(app.mixer.selected, MixerTarget::Master);
            let before = app.master.gain_db();
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, top);
            assert_ne!(app.master.gain_db(), before, "the fader moved");
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, top);

            assert_eq!(
                app.editor().primary_selection(),
                selection,
                "the score's selection did not follow the desk's fader"
            );
            assert_eq!(app.editor().viewport(), viewport);
            assert!(app.pointer.is_none());
        }

        /// Selecting in the desk's devices block is the desk's alone, down to the
        /// release: a score drag whose release never arrived stays where it was.
        #[test]
        fn the_devices_block_drag_never_reaches_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))
                .expect("test terminal");
            let mut draw = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                    .expect("draw");
            };
            draw(&mut app);

            // A press in the score whose release never arrives. A caret, not a
            // range: pressing the desk collapses a range, which ends the drag too.
            let score = app.focused_map().expect("the score is drawn").area();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                score.x + 2,
                score.y,
            );
            draw(&mut app);
            let selection = app.editor().primary_selection();
            assert!(selection.is_empty());
            let viewport = app.editor().viewport();

            let (area, lines) = app
                .mixer_devices_block()
                .expect("the desk draws the devices block");
            assert!(
                area.y >= score.y + score.height,
                "the devices block is below the score"
            );
            let reach = lines[0].chars().count().min(4) as u16;
            assert!(reach > 0);
            let taken: String = lines[0].chars().take(usize::from(reach)).collect();

            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y,
            );
            assert!(matches!(app.pointer, Some(Pointer::MixerText { .. })));
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                area.x + reach,
                area.y,
            );
            draw(&mut app);
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                area.x + reach,
                area.y,
            );

            assert_eq!(
                app.mixer_selection_text().as_deref(),
                Some(taken.as_str()),
                "the drag selected the devices block's characters"
            );
            assert!(
                app.pointer.is_none(),
                "the release let go of the desk's text"
            );
            assert_eq!(
                app.editor().primary_selection(),
                selection,
                "the score's selection did not follow the desk's release"
            );
            assert_eq!(
                app.editor().viewport(),
                viewport,
                "the score did not scroll"
            );
        }
    }
}

//! Hardware controllers driving the studio: MIDI launch pads that start scenes
//! (learning, forgetting, and telling the engine which notes are pads), and the
//! mapping slots that let a MIDI knob or a gamepad stick move a fader in the
//! audible score. This file covers learning a control into a slot, knob
//! takeover modes (jump, relative, scaled), stick speed and deadzone, and the
//! slot boxes the Settings sheet shows.

use super::*;

/// A mapped fader under a stick: how long a stick held all the way over
/// takes to cross the fader's whole travel - running; a stick held a
/// little walks, the speed being the square of the push. A knob is
/// absolute and a stick cannot be: it springs back to the middle, and an
/// absolute stick would slam every fader to half on release.
const STICK_FULL_TRAVEL_SECONDS: f64 = 2.0;

/// The push under which a stick is at rest: drift, or a thumb resting.
const STICK_DEADZONE: f64 = 0.2;

/// The push that says "this one" while a slot is being learnt.
const STICK_LEARN_PUSH: f64 = 0.6;

/// The most one message from an endless encoder moves a fader, in the
/// slider's own steps. A driver that batches a fast spin into one large
/// value must not fling the fader across the room.
const KNOB_PUSH_MAX_NOTCHES: f64 = 8.0;

/// How long a slot's box stays lit after its control moved.
const MAPPING_LIT: Duration = Duration::from_millis(400);

/// How long a note must be held before its ending reads as a latching
/// button's next press rather than a hand's release. A hand releases a
/// press well inside this; a button that latches - the Maschine's
/// transport in MIDI mode - says its second press as a note-off, a
/// separate human action that comes after it.
const LATCH_PRESS_AFTER: Duration = Duration::from_millis(300);

/// A stick's run on one fader: where the push has taken it along the
/// travel, kept between frames so a slow push adds up to a step rather
/// than rounding away to nothing every frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct StickRun {
    scene: SceneId,
    call_from: usize,
    ratio: f64,
}

impl App {
    pub(super) fn begin_learn(&mut self) {
        if self.scenes.current().is_replay() {
            self.status = "a replay has no pad - pads launch scenes".into();
            self.dirty_frame = true;
            return;
        }
        if let Some(scope) = self.current_prebake() {
            self.status = format!("{} has no pad - pads launch scenes", scope.tab_name());
            self.dirty_frame = true;
            return;
        }
        if self.pads.open_count() == 0 {
            self.sync_pad_listeners();
        }
        if self.pads.open_count() == 0 {
            self.status = "no MIDI input to learn from - plug a controller in".into();
            self.dirty_frame = true;
            return;
        }
        self.strip_mode = SceneStripMode::Learning;
        self.status = format!(
            "learning - hit the pad that should launch {:?} (Esc cancels)",
            self.scenes.current().name()
        );
        self.dirty_frame = true;
    }

    pub(super) fn forget_pad(&mut self) {
        if self.scenes.current().is_replay() {
            self.status = "a replay has no pad - pads launch scenes".into();
            self.dirty_frame = true;
            return;
        }
        if let Some(scope) = self.current_prebake() {
            self.status = format!("{} has no pad - pads launch scenes", scope.tab_name());
            self.dirty_frame = true;
            return;
        }
        match self.scenes.forget_pad() {
            Some(pad) => {
                self.persist_manifest();
                self.status = format!(
                    "{:?} no longer launches from {}",
                    self.scenes.current().name(),
                    pad.label()
                );
            }
            None => {
                self.status = format!("{:?} has no pad", self.scenes.current().name());
            }
        }
        self.dirty_frame = true;
    }

    /// Open a listener on every MIDI input the dock knows about.
    pub(super) fn sync_pad_listeners(&mut self) {
        let ports = self
            .devices
            .inventory()
            .midi_inputs
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        let failed = self.pads.sync(&ports);
        if !failed.is_empty() {
            self.status = format!("could not listen to {}", failed.join(", "));
            self.dirty_frame = true;
        }
    }

    pub(super) fn drain_pads(&mut self) {
        self.sync_launch_pads();
        for _ in 0..MAX_EVENTS_PER_TURN {
            let press = match self.pads.try_recv() {
                Ok(press) => press,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            if let Err(error) = self.handle_pad(press) {
                self.set_error(ErrorOwner::Interface, error.to_string());
            }
        }
        for _ in 0..MAX_EVENTS_PER_TURN {
            match self.pads.try_recv_cc() {
                Ok(turn) => self.handle_knob(turn),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Whether a MIDI message arriving now would do anything: a scene has a
    /// pad, a knob drives a mapping slot, a message fires a Keybinds row,
    /// or a pad or knob is being learnt. Otherwise a message only lights
    /// the activity light or enters the mixer's event feed; the visible
    /// feed independently asks the loop for the same low-latency polling
    /// deadline.
    pub(super) fn pad_press_can_act(&self) -> bool {
        self.strip_mode == SceneStripMode::Learning
            || self.mapping_learn.is_some()
            || self.keybind_learn.is_some()
            || self.keybinds.midi_bound()
            || self.ui_settings.mappings.iter().any(|source| {
                matches!(source, Some(super::super::settings::MappingSource::Knob(_)))
            })
            || self.scenes.scenes().iter().any(|scene| scene.pad.is_some())
    }

    /// Tell the engine which notes are bound to scene launch or to a
    /// Keybinds row, whenever that set changes. Such a note is a transport
    /// button, not a key: if its press entered the musical keys ring, the
    /// press-count watcher would arm an at-once requery milliseconds
    /// before the launch's own install, and the from-zero flip would chop
    /// the fresh placement and re-attack it - the pad-rewind glitch. The
    /// engine forwards the set to the live ports, where the driver thread
    /// drops those NoteOns before they can count. Cheap on the no-change
    /// path: a small scan of the scene list, and one compare against the
    /// last set sent.
    fn sync_launch_pads(&mut self) {
        let mut pads: Vec<(u8, u8)> = self
            .scenes
            .scenes()
            .iter()
            .filter_map(|scene| scene.pad.map(|pad| (pad.note, pad.channel)))
            .collect();
        // A note a Keybinds row answers to is the same kind of button.
        // A binding on any channel is the 0xFF the words read as "every".
        pads.extend(
            super::super::keybinds::BindAction::ALL
                .into_iter()
                .filter_map(|action| self.keybinds.midi_binding(action))
                .filter(|bind| bind.note)
                .map(|bind| (bind.number, bind.channel.unwrap_or(0xFF))),
        );
        pads.sort_unstable();
        pads.dedup();
        if self.launch_pads_sent.as_ref() == Some(&pads) {
            return;
        }
        if self.worker.try_set_launch_pads(pads.clone()) {
            self.launch_pads_sent = Some(pads);
        }
    }

    /// The fader a mapping slot drives: the slot-th slider of the audible
    /// score, in the order the mixer's desk shows them. Positional on
    /// purpose - a mapping outlives the score that was open when it was
    /// made, so slot 1 is whatever the score's first slider is now.
    fn mapping_fader(&self, slot: usize) -> Option<(SceneId, LiveChip)> {
        let scene = self.audible_scene?;
        let chip = self.live_chips(scene).into_iter().nth(slot)?;
        Some((scene, chip))
    }

    /// The slot a control belongs to, if a player gave it one.
    fn mapping_slot(
        &self,
        matches: impl Fn(super::super::settings::MappingSource) -> bool,
    ) -> Option<usize> {
        self.ui_settings
            .mappings
            .iter()
            .position(|source| source.is_some_and(&matches))
    }

    /// Bind what just moved to the slot the sheet is waiting on, and say
    /// so. A control already on another slot moves to this one rather than
    /// driving two faders at once, which is never what was meant.
    fn learn_mapping(&mut self, slot: usize, source: super::super::settings::MappingSource) {
        for (at, held) in self.ui_settings.mappings.iter_mut().enumerate() {
            if at != slot && *held == Some(source) {
                *held = None;
            }
        }
        self.ui_settings.mappings[slot] = Some(source);
        self.mapping_learn = None;
        self.mapping_lit[slot] = Some(Instant::now());
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        self.status = format!("slot {} · {}", slot + 1, source.label());
        self.dirty_frame = true;
    }

    /// A learn belongs to the open settings sheet: the sheet gone by any
    /// door, the learn goes with it, or the next knob turned days later
    /// would be bound in silence.
    pub(super) fn settle_mapping_learn(&mut self) {
        // The desk can arm one too, so a learn survives while the mixer
        // holds the faders. Both gone, the learn goes: the next knob
        // turned days later must not be bound in silence.
        let armed_here = self.mixer_panel.is_some()
            && self.mixer_on_faders
            && self.focus == Focus::Panel(PanelKind::Mixer);
        if self.mapping_learn.is_some() && self.settings_sheet.is_none() && !armed_here {
            self.mapping_learn = None;
        }
        // The keybind learn is the sheet's only: the sheet gone, the
        // learn goes, for the same reason.
        if self.keybind_learn.is_some() && self.settings_sheet.is_none() {
            self.keybind_learn = None;
        }
    }

    /// Where each slot stands, for the settings sheet's boxes: what drives
    /// it, what it drives, and whether its control moved just now.
    pub(super) fn mapping_slot_views(&self) -> [super::super::settings::SlotView; MAPPING_SLOTS] {
        let chips = self
            .audible_scene
            .map(|scene| self.live_chips(scene))
            .unwrap_or_default();
        let now = Instant::now();
        std::array::from_fn(|slot| {
            let chip = chips.get(slot);
            super::super::settings::SlotView {
                chip: self.ui_settings.mappings[slot]
                    .map_or_else(String::new, |source| source.chip()),
                takeover: self
                    .ui_settings
                    .takeover
                    .get(slot)
                    .copied()
                    .unwrap_or_default()
                    .chip(),
                fader: chip
                    .map(|chip| {
                        chip.label
                            .clone()
                            .unwrap_or_else(|| format!("slider {}", slot + 1))
                    })
                    .unwrap_or_default(),
                notch: chip.map(|chip| {
                    chip.travel
                        .ratio(chip.span.value, chip.span.min, chip.span.max)
                        as f32
                }),
                live: self.mapping_lit[slot]
                    .is_some_and(|at| now.saturating_duration_since(at) < MAPPING_LIT),
                learning: self.mapping_learn == Some(slot),
            }
        })
    }

    /// Every turn of the loop: each slot bound to a stick pushes its fader
    /// along - a little push walks it, a full push runs it across in two
    /// seconds - and lets go when the stick does. While the settings sheet
    /// is learning, the first control moved is the one.
    pub(super) fn drive_mapped_faders(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.stick_ticked_at)
            .as_secs_f64()
            .min(0.1);
        self.stick_ticked_at = now;
        self.settle_mapping_learn();
        if let Some(slot) = self.mapping_learn {
            self.learn_stick_for(slot);
            return;
        }
        let mut pushing = false;
        for slot in 0..MAPPING_SLOTS {
            let Some(super::super::settings::MappingSource::Stick(axis)) =
                self.ui_settings.mappings[slot]
            else {
                self.stick_run[slot] = None;
                continue;
            };
            let push = stick_push(axis);
            if push.abs() <= STICK_DEADZONE {
                self.stick_run[slot] = None;
                continue;
            }
            pushing = true;
            self.mapping_lit[slot] = Some(now);
            let Some((scene, chip)) = self.mapping_fader(slot) else {
                self.stick_run[slot] = None;
                continue;
            };
            let past = (push.abs() - STICK_DEADZONE) / (1.0 - STICK_DEADZONE);
            let speed = past * past * push.signum() / STICK_FULL_TRAVEL_SECONDS;
            let (min, max) = (chip.span.min, chip.span.max);
            let current = chip.travel.ratio(chip.span.value, min, max);
            // The run carries the fraction of a step a slow push has earned
            // so far - unless something else moved the fader meanwhile, in
            // which case the run starts again from where the fader is.
            let from = match self.stick_run[slot] {
                Some(run)
                    if run.scene == scene
                        && run.call_from == chip.call.start
                        && (chip.travel.at(run.ratio, min, max) - chip.span.value).abs()
                            <= chip.span.step * 1.01 =>
                {
                    run.ratio
                }
                _ => current,
            };
            let ratio = (from + speed * elapsed).clamp(0.0, 1.0);
            self.stick_run[slot] = Some(StickRun {
                scene,
                call_from: chip.call.start,
                ratio,
            });
            self.set_slider(scene, &chip, chip.travel.at(ratio, min, max));
        }
        if self.stick_pushing != pushing {
            self.stick_pushing = pushing;
            self.dirty_frame = true;
        }
    }

    /// The settings sheet is waiting on a slot: the first axis pushed past
    /// the learning threshold, on any pad, becomes that slot's.
    fn learn_stick_for(&mut self, slot: usize) {
        for (pad_slot, _) in rustel_core::gamepad::connected_pads() {
            let Some(pad) = rustel_core::gamepad::pad(pad_slot) else {
                continue;
            };
            for axis in 0..rustel_core::gamepad::AXES {
                if pad.axis(axis).abs() >= STICK_LEARN_PUSH {
                    let learnt = super::super::settings::StickAxis::from_index(axis);
                    if learnt != super::super::settings::StickAxis::Off {
                        self.learn_mapping(
                            slot,
                            super::super::settings::MappingSource::Stick(learnt),
                        );
                    }
                    return;
                }
            }
        }
    }

    /// A MIDI knob turned. While the settings sheet is waiting on a slot it
    /// becomes that slot's; while the Keybinds page is waiting on a row it
    /// becomes that row's message; otherwise, if a row or a slot holds it,
    /// the row fires on the press - a rise to 64 or above, or a fall from
    /// one - and the slot's fader goes where the knob points.
    fn handle_knob(&mut self, turn: super::super::pads::CcTurn) {
        self.settle_mapping_learn();
        if let Some(slot) = self.mapping_learn {
            self.learn_mapping(
                slot,
                super::super::settings::MappingSource::Knob(super::super::settings::SliderCc {
                    controller: turn.controller,
                    channel: Some(turn.channel),
                }),
            );
            return;
        }
        if let Some(super::super::keybinds::KeybindLearn::Armed(action)) = self.keybind_learn {
            self.learn_keybind_midi(
                action,
                super::super::keybinds::MidiBind {
                    number: turn.controller,
                    note: false,
                    channel: Some(turn.channel),
                },
            );
            return;
        }
        // A bound row reads the crossings of the line at 64, not every
        // message: the rise is a press, and a fall from a high that
        // outlasted a hand's release is the next press of a button that
        // toggles between 127 and 0. A fall soon after the rise is a
        // release or a knob sweeping back, and fires nothing; a knob
        // swept across the line crosses once each way at speed. A
        // control's first word, whichever side of the line it lands on,
        // is that control's press - a button that toggled before the
        // studio did says its next press as a bare 0.
        let key = (turn.controller, turn.channel);
        let now = Instant::now();
        let pressed_again = match self.midi_cc_last.get(&key) {
            Some(&(value, since)) => {
                (turn.value >= 64 && value < 64)
                    || (turn.value < 64
                        && value >= 64
                        && now.duration_since(since) >= LATCH_PRESS_AFTER)
            }
            None => true,
        };
        self.midi_cc_last.insert(key, (turn.value, now));
        if pressed_again
            && self.keybind_learn.is_none()
            && let Some(action) =
                self.keybinds
                    .midi_action_for(false, turn.controller, turn.channel)
        {
            self.log.push(
                LogLevel::Debug,
                "pad",
                format!(
                    "cc{}/ch{} fires {}",
                    turn.controller,
                    turn.channel,
                    action.label()
                ),
            );
            if let Err(error) = self.dispatch_bind_action(action, false) {
                self.set_error(ErrorOwner::Interface, error.to_string());
            }
            return;
        }
        let Some(slot) = self.mapping_slot(|source| match source {
            super::super::settings::MappingSource::Knob(knob) => {
                knob.matches(turn.controller, turn.channel)
            }
            super::super::settings::MappingSource::Stick(_) => false,
        }) else {
            return;
        };
        // Lit whether or not the score has a fader that far along: the
        // point of the light is to confirm the mapping arrived.
        self.mapping_lit[slot] = Some(Instant::now());
        self.dirty_frame = true;
        self.turn_mapping_slot(slot, turn.value);
    }

    /// A knob moved: what that means to the fader on its slot.
    ///
    /// The problem this exists for: a set is opened, its faders are
    /// wherever the score says, and the knobs are wherever a hand last
    /// left them. Reading the knob as a position makes the first touch
    /// slam the fader across - which is fine on a controller you have just
    /// set up and awful in front of an audience.
    fn turn_mapping_slot(&mut self, slot: usize, value: u8) {
        use super::super::settings::Takeover;
        let mode = self
            .ui_settings
            .takeover
            .get(slot)
            .copied()
            .unwrap_or_default();
        let knob = f64::from(value) / 127.0;
        // Where the knob was when it last spoke. A first message has no
        // previous position, so there is no sweep to scale by yet.
        let previous = self.mapping_knob[slot].replace(knob);
        let Some((scene, chip)) = self.mapping_fader(slot) else {
            return;
        };
        let (min, max) = (chip.span.min, chip.span.max);
        let here = chip.travel.ratio(chip.span.value, min, max);
        let ratio = match mode {
            Takeover::Jump => knob,
            Takeover::Relative => {
                // Binary offset: 64 is still, each unit either side a
                // notch. The step is the slider's own grain, so a coarse
                // control moves by one of its steps and a fine one does
                // not crawl.
                let notches = f64::from(i32::from(value) - 64)
                    .clamp(-KNOB_PUSH_MAX_NOTCHES, KNOB_PUSH_MAX_NOTCHES);
                if notches == 0.0 {
                    return;
                }
                let value = slider_stepped(&chip, notches);
                self.set_slider(scene, &chip, value);
                return;
            }
            Takeover::Scaled => {
                let Some(previous) = previous else {
                    // Nothing has been turned yet, so nothing is known
                    // about where the hand is. Stay put and learn the
                    // position; the next message is the first that moves
                    // anything, and it moves from here.
                    return;
                };
                let moved = knob - previous;
                if moved == 0.0 {
                    return;
                }
                // The share of the remaining sweep that this turn covered.
                // Turning up, the room left is what is above the fader and
                // the sweep left is what is above the knob; the fader
                // arrives exactly as the knob reaches its end, and cannot
                // overshoot because the share cannot exceed one.
                let (room, sweep) = if moved > 0.0 {
                    (1.0 - here, 1.0 - previous)
                } else {
                    (here, previous)
                };
                if sweep <= f64::EPSILON {
                    // The knob is already at the end it is moving toward:
                    // there is no sweep left to scale by, so follow it
                    // outright rather than dividing by nothing.
                    knob
                } else {
                    (here + room * (moved / sweep)).clamp(0.0, 1.0)
                }
            }
        };
        let value = chip.travel.at(ratio, min, max);
        self.set_slider(scene, &chip, value);
    }

    fn handle_pad(&mut self, press: PadPress) -> Result<(), RuntimeError> {
        self.settle_mapping_learn();
        let pad = Pad {
            note: press.note,
            channel: press.channel,
        };
        // The note's clock: a press stamps when it began, an ending takes
        // the stamp away and says whether the note outlasted a hand's
        // release - the ending of a latching button's press, not a
        // release at all. With no stamp, the note began before the studio
        // did: the state a latching button is left in across a restart,
        // and this ending is that button's next press.
        let key = (press.note, press.channel);
        let pressed_again = if press.velocity > 0 {
            self.note_pressed_at.insert(key, Instant::now());
            false
        } else {
            self.note_pressed_at
                .remove(&key)
                .is_none_or(|pressed| pressed.elapsed() >= LATCH_PRESS_AFTER)
        };
        if self.strip_mode == SceneStripMode::Learning {
            let learnt = self.scenes.learn_pad(pad);
            self.strip_mode = SceneStripMode::Idle;
            self.status = if learnt {
                self.persist_manifest();
                format!(
                    "{} launches {:?}",
                    pad.label(),
                    self.scenes.current().name()
                )
            } else {
                "a prebake has no pad - pads launch scenes".to_owned()
            };
            self.dirty_frame = true;
            return Ok(());
        }
        if self.strip_mode != SceneStripMode::Idle {
            return Ok(());
        }
        if let Some(super::super::keybinds::KeybindLearn::Armed(action)) = self.keybind_learn {
            self.learn_keybind_midi(
                action,
                super::super::keybinds::MidiBind {
                    number: press.note,
                    note: true,
                    channel: Some(press.channel),
                },
            );
            return Ok(());
        }
        // A message a Keybinds row holds is that row's, ahead of any pad a
        // scene was given: one message, one meaning. A row answers to the
        // press, and to the ending of a note held past a hand's release -
        // a latching button's next press. A plain release, ending the
        // note soon after it began, is still nothing.
        if self.keybind_learn.is_none()
            && let Some(action) = self
                .keybinds
                .midi_action_for(true, press.note, press.channel)
        {
            if press.velocity > 0 || pressed_again {
                self.log.push(
                    LogLevel::Debug,
                    "pad",
                    format!("{} fires {}", pad.label(), action.label()),
                );
                self.dispatch_bind_action(action, false)?;
                self.dirty_frame = true;
            }
            return Ok(());
        }
        // A pad launches on the press, never on the release.
        if press.velocity > 0
            && let Some(index) = self.scenes.scene_for_pad(pad.note, press.channel)
        {
            self.log.push(
                LogLevel::Debug,
                "pad",
                format!(
                    "launch from pad {} → {:?}",
                    pad.label(),
                    self.scenes.scenes()[index].name()
                ),
            );
            self.launch_scene(index)?;
        }
        Ok(())
    }
}

/// A stick's push on one axis right now, from whichever pad pushes it
/// furthest; up and down read up as positive, so a thumb pushed up raises
/// the fader.
fn stick_push(axis: super::super::settings::StickAxis) -> f64 {
    let Some(index) = axis.index() else {
        return 0.0;
    };
    let push = rustel_core::gamepad::connected_pads()
        .into_iter()
        .filter_map(|(slot, _)| rustel_core::gamepad::pad(slot))
        .map(|pad| pad.axis(index))
        .fold(0.0_f64, |best, value| {
            if value.abs() > best.abs() {
                value
            } else {
                best
            }
        });
    if axis.is_vertical() { -push } else { push }
}

#[cfg(test)]
mod tests {
    mod controllers {
        //! Tests for `app/controllers.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::Selection;

        /// The pads are process-wide state: the tests that move them take turns.
        static PAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

        /// A stick bound to a slot drives that slot's fader. A full push
        /// crosses the travel in two seconds, a small push moves it slowly, the
        /// dead zone is rest, and up on a vertical axis raises the value.
        /// Release stops it. The fader is the score's first slider, not the
        /// focused one.
        #[test]
        fn a_stick_on_a_slot_pushes_that_slots_fader() {
            use crate::settings::{MappingSource, StickAxis};
            let _pads = PAD_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            app.armed_slider = None;
            let pad = rustel_core::gamepad::pad(3).expect("pad 3");
            pad.set_connected(true);
            app.ui_settings.mappings[0] = Some(MappingSource::Stick(StickAxis::X1));
            let mut now = Instant::now();
            app.stick_ticked_at = now;
            // Full push right for one second, in tenths: half the travel.
            pad.set_axis(0, 1.0);
            for _ in 0..10 {
                now += Duration::from_millis(100);
                app.drive_mapped_faders(now);
            }
            assert!(app.stick_pushing, "the loop keeps frames coming");
            let after_a_second = value(&app, scene, call);
            assert!(
                (after_a_second - 1.0).abs() < 0.02,
                "0.5 plus half the travel: {after_a_second}"
            );
            // At rest nothing moves, and the run is let go.
            pad.set_axis(0, 0.1);
            now += Duration::from_millis(100);
            app.drive_mapped_faders(now);
            assert!(!app.stick_pushing);
            assert!(app.stick_run[0].is_none());
            assert_eq!(value(&app, scene, call), after_a_second);
            // A small push walks: half over is about a sixth of full speed.
            pad.set_axis(0, -0.6);
            for _ in 0..10 {
                now += Duration::from_millis(100);
                app.drive_mapped_faders(now);
            }
            let walked = after_a_second - value(&app, scene, call);
            assert!(walked > 0.05 && walked < 0.2, "walked {walked}");
            // Up on a vertical axis raises the value: the page reads up as negative.
            app.ui_settings.mappings[0] = Some(MappingSource::Stick(StickAxis::Y2));
            pad.set_axis(0, 0.0);
            pad.set_axis(3, -1.0);
            let before = value(&app, scene, call);
            now += Duration::from_millis(100);
            app.drive_mapped_faders(now);
            assert!(value(&app, scene, call) > before, "up raises");
            // A slot nobody bound drives nothing.
            app.ui_settings.mappings[0] = None;
            let before = value(&app, scene, call);
            now += Duration::from_millis(100);
            app.drive_mapped_faders(now);
            assert_eq!(value(&app, scene, call), before);
            pad.set_axis(3, 0.0);
            pad.set_connected(false);
        }

        /// A slow push still gets there: the run carries the fraction of a step
        /// between frames, so a coarse slider moves a step at a time instead of
        /// rounding every frame's sliver away.
        #[test]
        fn a_slow_push_adds_up_on_a_coarse_slider() {
            use crate::settings::{MappingSource, StickAxis};
            let _pads = PAD_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) =
                slider_app_with_source(directory.path(), "$: s(\"bd\").n(slider(5, 0, 10, 1))\n");
            app.audible_scene = Some(scene);
            let pad = rustel_core::gamepad::pad(2).expect("pad 2");
            pad.set_connected(true);
            app.ui_settings.mappings[0] = Some(MappingSource::Stick(StickAxis::X1));
            let mut now = Instant::now();
            app.stick_ticked_at = now;
            pad.set_axis(0, 0.5);
            let start = value(&app, scene, call);
            for _ in 0..120 {
                now += Duration::from_millis(16);
                app.drive_mapped_faders(now);
            }
            assert!(
                value(&app, scene, call) > start,
                "two seconds of a half push moved a step"
            );
            pad.set_axis(0, 0.0);
            pad.set_connected(false);
        }

        /// A Keybinds row answers to a latching button's whole vocabulary.
        /// The press fires it, the release a moment later does nothing, and
        /// the off-press of a button that latches - the Maschine's
        /// transport in MIDI mode, held past a hand's release - fires it
        /// again. The mirror port's copy of that off, arriving with the
        /// stamp already spent, fires nothing: one press, one fire.
        #[test]
        fn a_bound_note_fires_on_the_press_and_on_a_latching_off_press() {
            use crate::app::controllers::LATCH_PRESS_AFTER;
            use crate::keybinds::{BindAction, KeybindLearn};
            use rustel_midi::input::MidiEvent;
            let _pads = PAD_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene, _call) = slider_app(directory.path());
            // The learn is the open sheet's, and the button's own press is
            // the learn: arming by hand stands in for Enter.
            app.settings_sheet = Some(Default::default());
            app.keybind_learn = Some(KeybindLearn::arm(BindAction::PianoMode));
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::NoteOn {
                    channel: 1,
                    note: 94,
                    velocity: 100,
                },
                1_000,
            );
            app.drain_pads();
            assert!(app.keybind_learn.is_none(), "the press was the learn");
            assert!(!app.piano.open, "the learn does not fire the row");
            app.settings_sheet = None;

            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::NoteOn {
                    channel: 1,
                    note: 94,
                    velocity: 100,
                },
                2_000,
            );
            app.drain_pads();
            assert!(app.piano.open, "the press fired the row");

            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::NoteOff {
                    channel: 1,
                    note: 94,
                    velocity: 0,
                },
                2_010,
            );
            app.drain_pads();
            assert!(app.piano.open, "a release fires nothing");

            // The hold is the fixture, not a sleep: the stamp is set back
            // past a hand's release before the off arrives.
            app.note_pressed_at.insert(
                (94, 1),
                Instant::now() - LATCH_PRESS_AFTER - Duration::from_millis(50),
            );
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::NoteOff {
                    channel: 1,
                    note: 94,
                    velocity: 0,
                },
                3_000,
            );
            app.drain_pads();
            assert!(!app.piano.open, "the latching off-press fired the row");

            app.pads.driver_message_for_test(
                "mirror",
                MidiEvent::NoteOff {
                    channel: 1,
                    note: 94,
                    velocity: 0,
                },
                3_002,
            );
            app.drain_pads();
            assert!(!app.piano.open, "the mirror's copy fired nothing");

            // A restart leaves a latching button standing on, its next
            // press an off with no stamp on the clock: that press fires
            // like any other.
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::NoteOff {
                    channel: 1,
                    note: 94,
                    velocity: 0,
                },
                3_100,
            );
            app.drain_pads();
            assert!(app.piano.open, "the stampless off-press fired the row");
        }

        /// A Keybinds row answers a CC button that toggles - the
        /// Maschine's transport as this one sends it, one message a
        /// press, alternating 127 and 0. Both presses fire, whether the
        /// learn caught the 127 or the 0. A knob swept across the line
        /// fires once, and its sweep back, soon after, fires nothing.
        #[test]
        fn a_bound_row_answers_a_cc_that_toggles_between_127_and_0() {
            use crate::app::controllers::LATCH_PRESS_AFTER;
            use crate::keybinds::{BindAction, KeybindLearn};
            use rustel_midi::input::MidiEvent;
            let _pads = PAD_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene, _call) = slider_app(directory.path());
            // The learn is the open sheet's, and the button's own press is
            // the learn: arming by hand stands in for Enter.
            app.settings_sheet = Some(Default::default());
            app.keybind_learn = Some(KeybindLearn::arm(BindAction::PianoMode));
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::ControlChange {
                    channel: 1,
                    controller: 59,
                    value: 127,
                },
                1_000,
            );
            app.drain_pads();
            assert!(app.keybind_learn.is_none(), "the press was the learn");
            assert!(!app.piano.open, "the learn does not fire the row");
            app.settings_sheet = None;

            // The toggle's next press says 0, with nothing on record for
            // it: that first word is the press.
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::ControlChange {
                    channel: 1,
                    controller: 59,
                    value: 0,
                },
                2_000,
            );
            app.drain_pads();
            assert!(app.piano.open, "the 0-press fired the row");

            // The press after that says 127: the crossing up.
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::ControlChange {
                    channel: 1,
                    controller: 59,
                    value: 127,
                },
                3_000,
            );
            app.drain_pads();
            assert!(!app.piano.open, "the 127-press fired the row");

            // The 0 of a momentary press, or the toggle pressed again too
            // soon: too soon after the 127 to be a press of its own, it
            // only stands the control low.
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::ControlChange {
                    channel: 1,
                    controller: 59,
                    value: 0,
                },
                4_000,
            );
            app.drain_pads();
            assert!(!app.piano.open, "the quick 0 fired nothing");

            // A knob swept up across the line says 64 then 70: one
            // crossing, one fire. Its sweep back soon after says 0 then
            // 40: the fall is too soon to be a toggle's next press, and
            // the rest is the same side of the line.
            for (value, at) in [(64, 4_010), (70, 4_020), (0, 4_030), (40, 4_040)] {
                app.pads.driver_message_for_test(
                    "ctrl",
                    MidiEvent::ControlChange {
                        channel: 1,
                        controller: 59,
                        value,
                    },
                    at,
                );
                app.drain_pads();
            }
            assert!(app.piano.open, "the sweep fired once, on the way up");

            // The long fall: a high left standing past a hand's release
            // is the toggle's next press. The hold is the fixture, not a
            // sleep - the crossing's stamp is set back before the 0.
            app.midi_cc_last.insert(
                (59, 1),
                (
                    127,
                    Instant::now() - LATCH_PRESS_AFTER - Duration::from_millis(50),
                ),
            );
            app.pads.driver_message_for_test(
                "ctrl",
                MidiEvent::ControlChange {
                    channel: 1,
                    controller: 59,
                    value: 0,
                },
                5_000,
            );
            app.drain_pads();
            assert!(!app.piano.open, "the aged 0-press fired the row");
        }

        /// A MIDI knob bound to a slot drives that slot's fader, absolutely: the
        /// fader goes where the knob points. The caret is not consulted - a
        /// mapping is positional, so what it drives does not move when the caret
        /// does. Another knob, or another channel, does nothing.
        #[test]
        fn a_midi_knob_on_a_slot_drives_that_slots_fader() {
            use crate::pads::CcTurn;
            use crate::settings::{MappingSource, SliderCc};
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            // Deliberately away from the slider: this is not focus-follows-knob.
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(0)))
                .expect("caret at the top");
            app.ui_settings.mappings[0] = Some(MappingSource::Knob(SliderCc {
                controller: 74,
                channel: Some(2),
            }));
            // This is about where a message lands, not about how a turn is read:
            // pin the absolute mode so one message is one value.
            app.ui_settings.takeover[0] = crate::settings::Takeover::Jump;
            app.pads.turn_for_test(CcTurn {
                controller: 74,
                channel: 2,
                value: 127,
            });
            app.drain_pads();
            assert_eq!(
                value(&app, scene, call),
                1.0,
                "the knob's top is the fader's"
            );
            assert!(app.mapping_slot_views()[0].live, "the slot says it arrived");
            app.pads.turn_for_test(CcTurn {
                controller: 74,
                channel: 2,
                value: 0,
            });
            app.drain_pads();
            assert_eq!(value(&app, scene, call), 0.0);
            app.pads.turn_for_test(CcTurn {
                controller: 75,
                channel: 2,
                value: 64,
            });
            app.pads.turn_for_test(CcTurn {
                controller: 74,
                channel: 1,
                value: 64,
            });
            app.drain_pads();
            assert_eq!(
                value(&app, scene, call),
                0.0,
                "another knob, another channel: nothing"
            );
            // A slot past the score's last slider has nothing to drive, and says
            // so by moving nothing rather than by panicking.
            app.ui_settings.mappings[5] = Some(MappingSource::Knob(SliderCc {
                controller: 90,
                channel: None,
            }));
            app.pads.turn_for_test(CcTurn {
                controller: 90,
                channel: 7,
                value: 127,
            });
            app.drain_pads();
            assert_eq!(value(&app, scene, call), 0.0);
            assert!(
                app.mapping_slot_views()[5].live,
                "the light is the mapping arriving, not the fader moving"
            );
            // No mapping at all.
            app.ui_settings.mappings[0] = None;
            app.pads.turn_for_test(CcTurn {
                controller: 74,
                channel: 2,
                value: 127,
            });
            app.drain_pads();
            assert!(value(&app, scene, call).abs() < 1e-9);
        }

        /// Learning a slot from the settings sheet: Enter waits, the first stick
        /// pushed past the threshold or the next control change is kept, channel
        /// included; Esc cancels; both are remembered; and a control already on
        /// another slot moves rather than driving two faders at once.
        #[test]
        fn a_settings_slot_learns_a_knob_or_a_stick() {
            use crate::pads::CcTurn;
            use crate::settings::{MappingSource, SliderCc, StickAxis};
            let _pads = PAD_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene, _call) = slider_app(directory.path());
            // A learn is the open sheet's: without it, nothing is waiting.
            app.dispatch_settings_action_for_test(SettingsAction::LearnSlot(0));
            app.drive_mapped_faders(Instant::now());
            assert_eq!(app.mapping_learn, None, "no sheet, no learn");
            app.settings_sheet = Some(Default::default());
            app.dispatch_settings_action_for_test(SettingsAction::LearnSlot(0));
            assert_eq!(app.mapping_learn, Some(0));
            let pad = rustel_core::gamepad::pad(1).expect("pad 1");
            pad.set_connected(true);
            pad.set_axis(2, 0.3);
            app.drive_mapped_faders(Instant::now());
            assert_eq!(app.mapping_learn, Some(0), "a nudge is not a choice");
            pad.set_axis(2, 0.9);
            app.drive_mapped_faders(Instant::now());
            assert_eq!(app.mapping_learn, None);
            assert_eq!(
                app.ui_settings.mappings[0],
                Some(MappingSource::Stick(StickAxis::X2))
            );
            pad.set_axis(2, 0.0);
            pad.set_connected(false);

            app.dispatch_settings_action_for_test(SettingsAction::LearnSlot(1));
            assert_eq!(app.mapping_learn, Some(1));
            app.pads.turn_for_test(CcTurn {
                controller: 21,
                channel: 5,
                value: 40,
            });
            app.drain_pads();
            assert_eq!(app.mapping_learn, None);
            let knob = MappingSource::Knob(SliderCc {
                controller: 21,
                channel: Some(5),
            });
            assert_eq!(app.ui_settings.mappings[1], Some(knob));
            // Twelve slots, always written in order, so a slot's place survives.
            assert_eq!(
                app.prefs.mappings.as_deref().map(<[String]>::to_vec),
                Some(
                    [
                        "x2", "cc21/ch5", "off", "off", "off", "off", "off", "off", "off", "off",
                        "off", "off"
                    ]
                    .map(str::to_owned)
                    .to_vec()
                )
            );

            // The same knob learnt onto another slot leaves the first: one control
            // driving two faders is never what was meant.
            app.dispatch_settings_action_for_test(SettingsAction::LearnSlot(4));
            app.pads.turn_for_test(CcTurn {
                controller: 21,
                channel: 5,
                value: 90,
            });
            app.drain_pads();
            assert_eq!(app.ui_settings.mappings[1], None, "it moved");
            assert_eq!(app.ui_settings.mappings[4], Some(knob));

            // Space unbinds it again.
            app.dispatch_settings_action_for_test(SettingsAction::ClearSlot(4));
            assert_eq!(app.ui_settings.mappings[4], None);

            // Esc on the sheet cancels a learn and keeps the sheet.
            app.focus = Focus::Panel(PanelKind::Settings);
            app.dispatch_settings_action_for_test(SettingsAction::LearnSlot(2));
            assert!(app.handle_settings_key(KeyCode::Esc, false, false));
            assert_eq!(app.mapping_learn, None);
            assert!(app.settings_sheet.is_some(), "the sheet stays");
        }

        /// `l` on a fader learns a controller for it, without leaving the desk.
        ///
        /// Mid-set the mixer is where your hands already are, and a mapping you
        /// have to open Settings to make is a mapping you do not make. It is the
        /// same learn the Settings page arms, into the slot this fader answers to.
        #[test]
        fn l_on_a_mixer_fader_learns_a_knob_for_it() {
            use crate::pads::CcTurn;
            use crate::settings::{MappingSource, SliderCc};

            // drive_mapped_faders reads every pad's axes while learning, and the
            // learn-stick tests write those axes behind this lock. Held here too,
            // or a sibling mid-write has this desk's learn stolen by a stick that
            // no one pushed.
            let _pads = PAD_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            precision_key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.regions.mixer = Rect::new(0, 18, 120, 12);
            app.focus = Focus::Panel(PanelKind::Mixer);
            app.mixer_on_faders = true;
            app.mixer_fader = 0;

            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_eq!(app.mapping_learn, Some(0));
            assert!(app.status.contains("slot 1"), "{}", app.status);

            // With no settings sheet anywhere: the desk's own learn has to survive
            // the turn that settles it, or it is disarmed before a knob can move.
            app.drive_mapped_faders(Instant::now());
            assert_eq!(app.mapping_learn, Some(0), "the desk keeps its own learn");

            app.pads.turn_for_test(CcTurn {
                controller: 91,
                channel: 3,
                value: 64,
            });
            app.drain_pads();
            assert_eq!(app.mapping_learn, None, "and the knob lands");
            assert_eq!(
                app.ui_settings.mappings[0],
                Some(MappingSource::Knob(SliderCc {
                    controller: 91,
                    channel: Some(3)
                }))
            );

            // It drives that fader from now on, from the desk it was learnt at.
            // Two turns, because a freshly learnt knob has not been read as a
            // movement yet: the first says where the hand is, the second moves.
            for value in [64, 127] {
                app.pads.turn_for_test(CcTurn {
                    controller: 91,
                    channel: 3,
                    value,
                });
                app.drain_pads();
            }
            assert_value(&app, scene, call, 1.0);

            // Esc cancels an armed learn before it gives the keyboard back.
            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_eq!(app.mapping_learn, Some(0));
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.mapping_learn, None);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Mixer), "the desk stays");
        }

        /// Opening a set must not jump its faders to the knob positions.
        ///
        /// A score's faders are where the score says; the knobs are where a hand
        /// last left them. If the first touch after a set opens is read as a
        /// position, the fader jumps to the knob.
        #[test]
        fn a_knob_takes_over_its_fader_without_jumping() {
            use crate::pads::CcTurn;
            use crate::settings::{MappingSource, SliderCc, Takeover};

            let directory = tempfile::tempdir().expect("temp dir");
            let turn = |app: &mut App, value: u8| {
                app.pads.turn_for_test(CcTurn {
                    controller: 74,
                    channel: 2,
                    value,
                });
                app.drain_pads();
            };
            let setup = |app: &mut App, mode: Takeover| {
                app.ui_settings.mappings[0] = Some(MappingSource::Knob(SliderCc {
                    controller: 74,
                    channel: Some(2),
                }));
                app.ui_settings.takeover[0] = mode;
                app.mapping_knob[0] = None;
            };

            // Scaled: the score sits at 0.5 and the knob is down at a quarter.
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            setup(&mut app, Takeover::Scaled);
            assert_value(&app, scene, call, 0.5);
            turn(&mut app, 32);
            assert_value(&app, scene, call, 0.5);
            // Turning up from a quarter: the fader moves by the share of the sweep
            // covered, and cannot pass the knob's own end.
            turn(&mut app, 64);
            let midway = value(&app, scene, call);
            assert!(
                midway > 0.5 && midway < 1.0,
                "it moved toward the knob without arriving: {midway}"
            );
            turn(&mut app, 127);
            assert_value(&app, scene, call, 1.0);
            // Coming back down it tracks the knob the same way.
            turn(&mut app, 64);
            let back = value(&app, scene, call);
            assert!(back > 0.0 && back < 1.0, "back down toward it: {back}");

            // Jump: the first message is the value, which is the old behaviour and
            // still what somebody with a matching controller wants.
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            setup(&mut app, Takeover::Jump);
            turn(&mut app, 32);
            let jumped = value(&app, scene, call);
            assert!((jumped - 32.0 / 127.0).abs() < 0.01, "{jumped}");

            // Relative: 64 is still, and each unit either side is one of the
            // slider's own steps - the endless-encoder mode.
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            setup(&mut app, Takeover::Relative);
            let before = value(&app, scene, call);
            turn(&mut app, 64);
            assert_value(&app, scene, call, before);
            turn(&mut app, 65);
            let up = value(&app, scene, call);
            assert!(up > before, "one notch up: {before} -> {up}");
            turn(&mut app, 63);
            assert!(
                (value(&app, scene, call) - before).abs() < 1e-9,
                "and one back down returns"
            );
            // A knob at rest does not creep, however long it sits there.
            for _ in 0..32 {
                turn(&mut app, 64);
            }
            assert!((value(&app, scene, call) - before).abs() < 1e-9);
        }

        /// Opening a set clears the knob position that the scaled takeover
        /// measures from. If the position carries over, the first turn on the new
        /// set moves the fader by the distance between the two sets.
        #[test]
        fn opening_a_set_forgets_where_the_knob_was() {
            use crate::pads::CcTurn;
            use crate::settings::{MappingSource, SliderCc, Takeover};

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, _) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            app.ui_settings.mappings[0] = Some(MappingSource::Knob(SliderCc {
                controller: 74,
                channel: Some(2),
            }));
            app.ui_settings.takeover[0] = Takeover::Scaled;
            let turn = |app: &mut App, value: u8| {
                app.pads.turn_for_test(CcTurn {
                    controller: 74,
                    channel: 2,
                    value,
                });
                app.drain_pads();
            };

            // A session's worth of turning leaves the knob near the top.
            turn(&mut app, 8);
            turn(&mut app, 120);
            assert!(
                app.mapping_knob[0].is_some(),
                "the knob has said where it is"
            );

            // Then another set opens, its own fader at the half where its score
            // puts it.
            let elsewhere = directory.path().join("elsewhere.strudel");
            std::fs::write(&elsewhere, SOURCE).expect("the other set's score");
            let set = SceneSet::open(&elsewhere, "// starter").expect("set");
            app.switch_set(set, "opened set");
            let scene = app.scenes.current().id;
            app.audible_scene = Some(scene);
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision: app.editor().revision(),
                    sliders: rustel_runtime::ui_events::literal_sliders(&app.editor().source()),
                },
            );
            let call = SOURCE.find("slider(").expect("slider call");
            assert_value(&app, scene, call, 0.5);

            // The hand comes back to the middle of the knob's travel. Measured
            // against the old position that is most of a turn downward, and the
            // fader would follow it down; measured against nothing, which is all
            // this set knows, it stays where the score put it and learns the
            // position instead.
            turn(&mut app, 64);
            assert_value(&app, scene, call, 0.5);
            // The turn after that is a real move, from where the hand actually is.
            turn(&mut app, 127);
            assert_value(&app, scene, call, 1.0);
        }

        /// Listening to a port with nothing bound to it is no reason to wake the
        /// loop every few milliseconds; a pad, a knob mapping or a learn is.
        #[test]
        fn only_a_bound_or_learning_control_keeps_the_pad_cap_on() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            assert!(!app.pad_press_can_act(), "nothing bound, nothing learning");
            app.mapping_learn = Some(0);
            assert!(app.pad_press_can_act(), "a knob is being learnt");
            app.mapping_learn = None;
            app.ui_settings.mappings[0] = Some(crate::settings::MappingSource::Knob(
                crate::settings::SliderCc {
                    controller: 74,
                    channel: None,
                },
            ));
            assert!(app.pad_press_can_act(), "a knob drives a slot");
            app.ui_settings.mappings[0] = None;
            app.strip_mode = SceneStripMode::Learning;
            assert!(app.pad_press_can_act(), "a pad is being learnt");
        }
    }
}

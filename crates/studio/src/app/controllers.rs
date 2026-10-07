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
    /// pad, a knob drives a mapping slot, or a pad or knob is being learnt.
    /// Otherwise a message only lights the activity light or enters the
    /// mixer's event feed; the visible feed independently asks the loop for
    /// the same low-latency polling deadline.
    pub(super) fn pad_press_can_act(&self) -> bool {
        self.strip_mode == SceneStripMode::Learning
            || self.mapping_learn.is_some()
            || self.ui_settings.mappings.iter().any(|source| {
                matches!(source, Some(super::super::settings::MappingSource::Knob(_)))
            })
            || self.scenes.scenes().iter().any(|scene| scene.pad.is_some())
    }

    /// Tell the engine which notes are bound to scene launch, whenever
    /// that set changes. A launch pad is a transport button, not a key:
    /// if its press entered the musical keys ring, the press-count watcher
    /// would arm an at-once requery milliseconds before the launch's own
    /// install, and the from-zero flip would chop the fresh placement and
    /// re-attack it - the pad-rewind glitch. The engine forwards the set
    /// to the live ports, where the driver thread drops those NoteOns
    /// before they can count. Cheap on the no-change path: a small scan
    /// of the scene list, and one compare against the last set sent.
    fn sync_launch_pads(&mut self) {
        let mut pads: Vec<(u8, u8)> = self
            .scenes
            .scenes()
            .iter()
            .filter_map(|scene| scene.pad.map(|pad| (pad.note, pad.channel)))
            .collect();
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
    /// becomes that slot's; otherwise, if a slot holds it, that slot's
    /// fader goes where the knob points.
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
        let pad = Pad {
            note: press.note,
            channel: press.channel,
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
        if let Some(index) = self.scenes.scene_for_pad(pad.note, pad.channel) {
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

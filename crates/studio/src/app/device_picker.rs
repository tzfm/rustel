//! The footer's device picker: the popover that opens from the output or MIDI
//! chip, its tabs and rows, and the keys and clicks that choose an audio output
//! or input, tick MIDI ports, step the MIDI clock or copy a port or gamepad
//! name. It also holds the facts the footer and devices panel show: the chosen
//! and resting device names, input channels and lag, the latency report, device
//! activity lights, MIDI enablement kept in step with the engine, orbit routing
//! chips and the drag-to-set master volume bar.

use super::*;

/// How long a device's light stays on after it last did something.
pub(super) const ACTIVITY_LIGHT_MILLIS: u64 = 250;

/// The input peak that counts as the device doing something. Low enough
/// for a quiet room to register, which is why the light has to be held:
/// a live microphone crosses this several times a second and the chip
/// strobed at the frame rate.
pub(super) const INPUT_LIGHT_PEAK: f32 = 0.02;

impl App {
    /// How many rows the device panel lists on a tab.
    ///
    /// The MIDI tab counts the rows [`Self::midi_rows`] gives it, so the
    /// panel that is drawn and the panel that is clicked agree about where a
    /// row is - including in a pinned frame, where those rows are empty.
    pub(super) fn panel_row_count(&self, kind: DeviceKind) -> usize {
        if kind == DeviceKind::Midi {
            self.midi_rows().len() + super::super::devices::MIDI_CLOCK_ROWS
        } else {
            self.devices.inventory().row_count(kind)
        }
    }

    /// Bring the MIDI gate up to date with the ports the host reports.
    ///
    /// On the first scan it is resolved: the preferences are read, and a
    /// studio that has never said takes every port present as enabled. That
    /// answer is written back straight away, so the next launch reads a list
    /// that says so rather than repeating the guess against whatever happens
    /// to be plugged in then - which would silently enable a device that was
    /// only connected later, the exact thing off-by-default is for.
    ///
    /// On every later scan only the host's lists are refreshed. What is
    /// ticked is kept by name, so a synth unplugged and plugged back in
    /// returns with its box still ticked.
    pub(super) fn sync_midi_enablement(&mut self) {
        // A pinned frame stands in for the host: resolving the gate against
        // the runner's own ports would write the runner's hardware into the
        // preferences under test, and make a test pass on one desk and fail
        // on the next.
        if !self.devices.has_scanned() || self.pinned_frame_inputs() {
            return;
        }
        let inventory = self.devices.inventory();
        if let Some(enabled) = self.midi_enabled.as_mut() {
            enabled.refresh_present(inventory);
            return;
        }
        let enabled = super::super::devices::MidiEnablement::from_prefs(
            self.prefs.midi_out_enabled.as_deref(),
            self.prefs.midi_in_enabled.as_deref(),
            inventory,
        );
        if self.prefs.midi_out_enabled.is_none() || self.prefs.midi_in_enabled.is_none() {
            use super::super::devices::MidiDirection;
            self.prefs.midi_out_enabled = Some(enabled.kept(MidiDirection::Out));
            self.prefs.midi_in_enabled = Some(enabled.kept(MidiDirection::In));
            self.save_prefs_soon();
        }
        self.midi_enabled = Some(enabled);
    }

    /// Tell the engine, when what it was last told is out of date.
    pub(super) fn push_midi_enablement(&mut self) {
        let Some(enabled) = self.midi_enabled.as_ref() else {
            return;
        };
        if self.applied_midi_enabled.as_ref() == Some(enabled) {
            return;
        }
        let enabled = enabled.clone();
        if self.worker.try_midi_enablement(enabled.clone()) {
            self.applied_midi_enabled = Some(enabled);
        }
    }

    /// The ports of one family, as the last probe saw them. The dock
    /// refreshes on its own cadence, so this is whatever is plugged in
    /// now rather than whatever was there at launch.
    pub(super) fn midi_port_names(&self, direction: MidiDirection) -> Vec<String> {
        self.devices
            .inventory()
            .midi_ports(direction)
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// The input `s("in")` is meant to play: the one chosen in the devices
    /// dock and remembered, or the one the command line named; none by
    /// default, and none is no input at all.
    fn chosen_input(&self) -> Option<&str> {
        self.prefs
            .audio_input
            .as_deref()
            .or(self.options.input.as_deref())
    }

    /// Whether an input is chosen or still open: the mixer shows its
    /// strip only then.
    pub(super) fn input_chosen(&self) -> bool {
        self.chosen_input().is_some()
            || self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.input_device.is_some())
    }

    /// How many channels the open audio input has, from the last snapshot;
    /// none open, none.
    pub(super) fn input_channels(&self) -> usize {
        self.snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.input_channels)
    }

    /// What the footer names while no stream is open: the output the last
    /// play used or the one picked since, else the one the studio was
    /// started on, else the host's default.
    ///
    /// "No output" is for the one case it describes - playing with nothing
    /// open. While stopped it would read as a fault, when it only means
    /// that nothing has asked for the device yet.
    pub(super) fn resting_output_name(&self) -> Option<String> {
        if self.is_playing() {
            return None;
        }
        let chosen = self
            .resting_output
            .as_deref()
            .or(self.options.output.as_deref());
        // `silent` is the choice of no device at all, and "no output" is
        // exactly what it is.
        if chosen == Some(rustel_audio::SILENT_OUTPUT_NAME) {
            return None;
        }
        chosen
            .map(|name| self.device_name_for(name, false))
            .or_else(|| {
                self.devices
                    .inventory()
                    .audio_outputs
                    .iter()
                    .find(|entry| entry.is_default)
                    .map(|entry| entry.name.clone())
            })
    }

    /// What a musician calls the device the engine named, when the list
    /// still has it: the friendly name for a backend handle.
    pub(super) fn device_name_for(&self, reported: &str, input: bool) -> String {
        let inventory = self.devices.inventory();
        let entries = if input {
            &inventory.audio_inputs
        } else {
            &inventory.audio_outputs
        };
        entries
            .iter()
            .find(|entry| entry.is_named(Some(reported)))
            .map_or_else(|| reported.to_owned(), |entry| entry.name.clone())
    }

    /// Where the input's readers actually sit behind its writer, from the
    /// last snapshot; none open, nought.
    fn input_lag_frames(&self) -> u64 {
        self.snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.input_lag_frames)
    }

    /// What the open stream costs, end to end.
    ///
    /// Three numbers that lived in three places and appeared in none: the
    /// device's own buffer, the master limiter's runway, and the input lag
    /// being paid. Only this side knows all three.
    pub(super) fn latency_report(&self) -> Option<super::super::devices::LatencyReport> {
        let device = self.snapshot.as_ref()?.device.as_ref()?;
        let output = device.audio.output();
        Some(super::super::devices::LatencyReport::for_output(
            output,
            // Off is nought: a limiter that is not running holds nothing
            // back, so it costs nothing to wait for. On, it is what the
            // runway comes to at this rate - the character's milliseconds
            // are what it asks for, and the ring can hand it less.
            self.worker.master().limiter().map_or(0.0, |settings| {
                settings
                    .character
                    .lookahead_millis_at(output.sample_rate_hz())
            }),
            // Only when an input is actually open: an unopened input pays
            // nothing. What is paid is where the reader actually sits,
            // which sizes itself to the driver - there is no setting to
            // report instead.
            self.input_chosen().then(|| self.input_lag_frames()),
        ))
    }

    /// The input `s("in")` plays is remembered, so a set that listens opens
    /// listening. See `StudioPrefs::audio_input` for why the output is not.
    pub(super) fn remember_audio_input(&mut self, name: Option<&str>) {
        let remembered = name.map(str::to_owned);
        // What the command line named is where the studio starts, not what
        // it must keep saying: once a choice has been made here it owns the
        // answer, or an input let go would still read as chosen - and there
        // would still be no way to turn `--input` off.
        self.options.input = None;
        if self.prefs.audio_input != remembered {
            self.prefs.audio_input = remembered;
            self.save_prefs_soon();
            // A different source, so its own level, if this set kept one.
            self.restore_input_level();
        }
    }

    pub(super) fn click_panel(&mut self, x: u16, y: u16) {
        let Some(mut panel) = self.panel else {
            return;
        };
        let entries = self.panel_row_count(panel.kind).max(1);
        let Some(geometry) = DevicePanelView::geometry(self.frame, panel, entries) else {
            self.panel = None;
            self.dirty_frame = true;
            return;
        };
        // The picker now sits over the chip that opened it, so the chip is
        // underneath it and the dismiss-on-click-outside below can no
        // longer reach it. A toggle whose off-switch is buried under itself
        // is worse than one that opened in the wrong corner, so the chip is
        // checked first and closes what it opened.
        if self
            .footer_chip_at(x, y)
            .is_some_and(|(chip, _)| chip == panel.kind)
        {
            self.panel = None;
            self.settle_focus();
            self.dirty_frame = true;
            return;
        }
        if !within(geometry.area, x, y) {
            // A click anywhere else dismisses the picker, as a popover should.
            self.panel = None;
            self.dirty_frame = true;
            return;
        }
        if let Some(kind) = geometry.tab_at(x, y) {
            panel.kind = kind;
            panel.selected = 0;
            self.panel = Some(panel);
        } else if panel.kind == DeviceKind::Midi {
            self.click_midi_row(&mut panel, geometry, x, y);
        } else if let Some(index) = geometry.entry_at(x, y)
            && index < self.devices.inventory().entries(panel.kind).len()
        {
            panel.selected = index;
            panel.first = geometry.first;
            panel.hold_scroll = true;
            self.panel = Some(panel);
            // A click on a row does not close the list: a MIDI port is
            // copied, not opened, and the next one is one click away.
            self.activate_device(panel.kind, index, false);
        }
        self.dirty_frame = true;
    }

    /// A click on the MIDI tab: on a box it ticks that box, on a name it
    /// copies the name, on a clock row it steps the port.
    ///
    /// The box a click lands on also becomes the column the keys are on, so
    /// a click followed by Down and Space carries on down the same column
    /// instead of jumping to the other one.
    fn click_midi_row(
        &mut self,
        panel: &mut DevicePanel,
        geometry: super::super::devices::PanelGeometry,
        x: u16,
        y: u16,
    ) {
        let rows = self.midi_rows();
        let Some(index) = geometry.entry_at(x, y) else {
            return;
        };
        let Some(row) = MidiPanelRow::at(index, rows.len()) else {
            return;
        };
        panel.selected = index;
        panel.first = geometry.first;
        panel.hold_scroll = true;
        match row {
            MidiPanelRow::Port(port) => {
                let Some(device) = rows.get(port) else {
                    return;
                };
                match geometry.midi_box_at(x, y) {
                    Some((_, direction)) if device.has(direction) => {
                        panel.midi_column = direction;
                        self.panel = Some(*panel);
                        self.toggle_midi_port(&device.name, direction);
                    }
                    // A dash: there is no port that way to tick.
                    Some(_) => self.panel = Some(*panel),
                    None => {
                        self.panel = Some(*panel);
                        self.copy_midi_port_name(&device.name);
                    }
                }
            }
            MidiPanelRow::ClockIn | MidiPanelRow::ClockOut => {
                self.panel = Some(*panel);
                self.step_clock_port(row, true);
            }
        }
    }

    /// The orbit chip under the pointer, with the pair it is on.
    pub(super) fn orbit_chip_at(&self, x: u16, y: u16) -> Option<(u8, u8)> {
        let hits = self.footer_hits();
        let (orbit, _) = hits
            .orbits
            .iter()
            .take(hits.orbit_count)
            .find(|(_, rect)| within(*rect, x, y))?;
        let pair = self
            .snapshot
            .as_ref()?
            .orbits
            .iter()
            .find(|level| level.orbit == *orbit)?
            .pair;
        Some((*orbit, pair))
    }

    /// A click on an orbit chip sends the orbit to the next output pair,
    /// round the pairs the device has.
    pub(super) fn route_orbit_next(&mut self, orbit: u8, pair: u8) {
        let pairs = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.output_pairs)
            .unwrap_or(1);
        if pairs <= 1 {
            self.status =
                "this output has one stereo pair - pick a multichannel device to route orbits"
                    .into();
            self.dirty_frame = true;
            return;
        }
        let next = ((u16::from(pair) + 1) % pairs) as u8;
        if self.worker.try_route_orbit(orbit, next) {
            let (left, right) = (u16::from(next) * 2 + 1, u16::from(next) * 2 + 2);
            self.status = format!("orbit {orbit} → outputs {left}/{right}");
            self.log.push(
                LogLevel::Info,
                "route",
                format!("orbit {orbit} → {left}/{right}"),
            );
        }
        self.dirty_frame = true;
    }

    /// The chip a point is over, and the column it starts at - the picker
    /// opens over the chip that named it, not in the far corner.
    pub(super) fn footer_chip_at(&self, x: u16, y: u16) -> Option<(DeviceKind, u16)> {
        let hits = self.footer_hits();
        if within(hits.device_chip, x, y) {
            Some((DeviceKind::AudioOutput, hits.device_chip.x))
        } else if within(hits.midi_chip, x, y) {
            Some((DeviceKind::Midi, hits.midi_chip.x))
        } else {
            None
        }
    }

    pub(super) fn open_device_panel_on(&mut self, kind: DeviceKind, anchor_x: Option<u16>) {
        self.devices.refresh_now(Instant::now());
        self.panel = Some(DevicePanel {
            kind,
            anchor_x,
            ..DevicePanel::default()
        });
        // Opened from a chip, the popover takes the keyboard exactly as the
        // chord-opened one does - Enter must pick a device, not edit the
        // score underneath.
        self.focus_panel(PanelKind::Devices);
        self.status = "devices - Enter picks, Tab switches list, Esc closes".into();
        self.dirty_frame = true;
    }

    /// The level a pointer at this position is asking for, if it is over the
    /// master dock's bar.
    pub(super) fn volume_at(&self, x: u16, y: u16) -> Option<f32> {
        let hits = self.footer_hits();
        let bar = hits.meter;
        // The fader rides on the first row, but both dock rows are its hit
        // target. Legacy conhost only reports whole cells and a one-row rail
        // is unnecessarily hard to keep hold of while dragging.
        let target = Rect::new(bar.x, hits.dock.y, bar.width, hits.dock.height);
        within(target, x, y).then(|| MasterDock::decibels_within(bar, x, self.pointer_subcell))
    }

    pub(super) fn drag_volume(&mut self, x: u16, _y: u16) {
        // A drag that slides outside the bar keeps controlling it, clamped
        // to its ends, which is what every mixer does.
        let bar = self.footer_hits().meter;
        let db = if bar.is_empty() {
            self.master.gain_db()
        } else {
            MasterDock::decibels_within(bar, x, self.pointer_subcell)
        };
        self.set_master_gain_db(db);
    }

    pub(super) fn toggle_device_panel(&mut self) {
        if self.panel.is_some() && self.focus != Focus::Panel(PanelKind::Devices) {
            self.focus_panel(PanelKind::Devices);
            return;
        }
        if self.panel.is_none() {
            self.dismiss_dialogs(Some(PanelKind::Devices));
        }
        self.panel = match self.panel {
            Some(_) => None,
            None => {
                self.devices.refresh_now(Instant::now());
                self.status = "devices - Enter picks, Tab switches list, Esc closes".into();
                Some(DevicePanel::default())
            }
        };
        if self.panel.is_some() {
            self.focus_panel(PanelKind::Devices);
        } else {
            self.focus = Focus::Editor;
        }
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.dirty_frame = true;
    }

    /// Returns true when the picker consumed the key.
    pub(super) fn handle_panel_key(
        &mut self,
        code: KeyCode,
        primary: bool,
    ) -> Result<bool, RuntimeError> {
        let Some(mut panel) = self.panel else {
            return Ok(false);
        };
        self.devices.refresh_gamepads();
        let len = self.panel_row_count(panel.kind);
        // The MIDI tab keeps Left and Right for itself: they pick which of a
        // row's two boxes Space toggles, or step a clock port. Tab still
        // switches tabs, so nothing is lost - a panel whose arrows moved you
        // off the tab while you were choosing a column would be a trap.
        if panel.kind == DeviceKind::Midi
            && let Some(keep_open) = self.handle_midi_tab_key(&mut panel, code, primary)
        {
            self.panel = keep_open.then_some(panel);
            if !keep_open {
                self.settle_focus();
            }
            self.dirty_frame = true;
            return Ok(true);
        }
        match code {
            KeyCode::Esc => {
                self.panel = None;
                self.status = "devices closed".into();
                // The keys go back to the score, not to a panel that is
                // gone.
                self.settle_focus();
            }
            KeyCode::Char('p') if primary => {
                self.panel = None;
                self.settle_focus();
            }
            KeyCode::Up => {
                panel.move_selection(-1, len);
                panel.settle_scroll(self.frame, len);
                self.panel = Some(panel);
            }
            KeyCode::Down => {
                panel.move_selection(1, len);
                panel.settle_scroll(self.frame, len);
                self.panel = Some(panel);
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let rows = DevicePanelView::geometry(self.frame, panel, len)
                    .map_or(1, |geometry| geometry.list.height.max(1))
                    as isize;
                panel.move_selection(if code == KeyCode::PageUp { -rows } else { rows }, len);
                panel.settle_scroll(self.frame, len);
                self.panel = Some(panel);
            }
            // Tab and Shift+Tab, and only those. The arrows do not switch
            // tabs here: the reference column gives them to its rows, the
            // settings sheet to its values, and the MIDI tab to its two
            // boxes.
            KeyCode::Tab => {
                panel.next_tab();
                self.panel = Some(panel);
            }
            KeyCode::BackTab => {
                panel.previous_tab();
                self.panel = Some(panel);
            }
            // Plain Enter picks; Ctrl+Enter still belongs to the transport,
            // so a score can be re-evaluated without closing the picker.
            KeyCode::Enter if !primary => self.activate_device(panel.kind, panel.selected, true),
            _ => return Ok(false),
        }
        self.dirty_frame = true;
        Ok(true)
    }

    /// A key on the MIDI tab, or `None` for one that belongs to the panel.
    /// `Some` says whether the panel stays open afterwards.
    ///
    /// On a port row, Left and Right choose the column, Space ticks the box
    /// in it, and Enter copies the port name for a score: a port name has
    /// to be typed exactly, and this is the easiest place to get it. On a
    /// clock row, Left and Right step the port, as do Space and Enter.
    fn handle_midi_tab_key(
        &mut self,
        panel: &mut DevicePanel,
        code: KeyCode,
        primary: bool,
    ) -> Option<bool> {
        let rows = self.midi_rows();
        let row = MidiPanelRow::at(panel.selected, rows.len())?;
        match (code, row) {
            (KeyCode::Left, MidiPanelRow::Port(_)) => panel.midi_column = MidiDirection::In,
            (KeyCode::Right, MidiPanelRow::Port(_)) => panel.midi_column = MidiDirection::Out,
            (KeyCode::Char(' '), MidiPanelRow::Port(index)) => {
                let device = rows.get(index)?;
                self.toggle_midi_port(&device.name, panel.midi_column);
            }
            // Copies and closes, as the MIDI tabs always did: a name is
            // fetched to be pasted, and Enter is the press that says "that
            // one, I'm done". A click copies and leaves the list up.
            (KeyCode::Enter, MidiPanelRow::Port(index)) if !primary => {
                let name = rows.get(index)?.name.clone();
                self.copy_midi_port_name(&name);
                return Some(false);
            }
            (KeyCode::Left, MidiPanelRow::ClockIn | MidiPanelRow::ClockOut) => {
                self.step_clock_port(row, false);
            }
            (
                KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter,
                MidiPanelRow::ClockIn | MidiPanelRow::ClockOut,
            ) if !primary => self.step_clock_port(row, true),
            _ => return None,
        }
        Some(true)
    }

    /// The MIDI tab's rows as they should be drawn: merged, with boxes.
    ///
    /// Before the first device scan there is no gate to read the boxes from,
    /// so every box reads unticked - which is also what they are about to be
    /// shown as while the probe is still out, rather than a guess.
    pub(super) fn midi_rows(&self) -> Vec<super::super::devices::MidiRow> {
        // A pinned frame lists no host ports, as its footer counts none: the
        // runner's own MIDI hardware must not leak into what a test sees.
        // The clock rows are still listed, with nothing to choose between.
        if self.pinned_frame_inputs() {
            return Vec::new();
        }
        let empty = super::super::devices::MidiEnablement::default();
        let enabled = self.midi_enabled.as_ref().unwrap_or(&empty);
        self.devices.inventory().midi_rows(enabled)
    }

    /// Tick or untick one direction of one device, and keep it.
    ///
    /// Unticking the port a clock is running through releases the clock as
    /// well. A clock opens a sender of its own rather than going through the
    /// gate, and a box that said "off" while clock kept pouring into the port
    /// would be the one control on the tab that did not mean what it showed.
    fn toggle_midi_port(&mut self, name: &str, direction: MidiDirection) {
        let Some(enabled) = self.midi_enabled.as_mut() else {
            self.status = "still scanning for MIDI ports - try again in a moment".into();
            return;
        };
        let on = enabled.toggle(direction, name);
        self.prefs.midi_out_enabled = Some(enabled.kept(MidiDirection::Out));
        self.prefs.midi_in_enabled = Some(enabled.kept(MidiDirection::In));
        let clock = match direction {
            MidiDirection::Out => &mut self.ui_settings.clock_out,
            MidiDirection::In => &mut self.ui_settings.clock_in,
        };
        let released = !on && clock.as_deref() == Some(name);
        if released {
            *clock = None;
            self.prefs.set_ui_settings(&self.ui_settings);
            self.apply_ui_settings();
        }
        self.save_prefs_soon();
        let way = direction.label();
        self.status = match (on, released) {
            (true, _) => format!("{name} - MIDI {way} on"),
            (false, false) => format!("{name} - MIDI {way} off; a score naming it is refused"),
            (false, true) => format!("{name} - MIDI {way} off, and clock {way} with it"),
        };
    }

    /// Step clock in or out to the next ENABLED port that way, or to none.
    ///
    /// Only enabled ports, because a clock sends through a sender of its own
    /// and would otherwise be the one way round the gate.
    fn step_clock_port(&mut self, row: MidiPanelRow, forwards: bool) {
        let direction = if row == MidiPanelRow::ClockIn {
            MidiDirection::In
        } else {
            MidiDirection::Out
        };
        let ports: Vec<String> = self
            .midi_rows()
            .into_iter()
            .filter(|device| device.enabled(direction))
            .map(|device| device.name)
            .collect();
        let clock = match direction {
            MidiDirection::Out => &mut self.ui_settings.clock_out,
            MidiDirection::In => &mut self.ui_settings.clock_in,
        };
        *clock = super::super::settings::step_port(clock, &ports, forwards);
        let now = clock.clone();
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        self.apply_ui_settings();
        let way = direction.label();
        self.status = match (now, ports.is_empty()) {
            (Some(port), _) => format!("clock {way} → {port}"),
            (None, true) => format!("clock {way} - tick a port's {way} box to offer it here"),
            (None, false) => format!("clock {way} off"),
        };
    }

    /// Put a MIDI port name on the clipboard, for a score to use.
    fn copy_midi_port_name(&mut self, name: &str) {
        match self.clipboard.set_text(name.to_owned()) {
            Ok(()) => {
                self.status = format!("copied \"{name}\" - paste it into .midi() or midin()");
            }
            Err(error) => self.set_error(
                ErrorOwner::Editor,
                format!("could not copy {name}: {error}"),
            ),
        }
    }

    /// Act on a chosen device: audio outputs move playback, MIDI ports go to
    /// the clipboard by name.
    ///
    /// `dismiss` says whether the picker should close afterwards. A MIDI
    /// port and a pad are not *activated* by choosing them - nothing is
    /// opened, the name is copied - so a click leaves the list up to be
    /// read and clicked again, and Enter is the one that says "that one,
    /// I'm done". Audio in and out really do commit a device, and close
    /// either way.
    pub(super) fn activate_device(&mut self, kind: DeviceKind, index: usize, dismiss: bool) {
        let Some(entry) = self.devices.inventory().entries(kind).get(index).cloned() else {
            return;
        };
        match kind {
            DeviceKind::AudioOutput => {
                if self.worker.try_set_output(&entry.name) {
                    // Stopped, the engine only records the choice; the
                    // footer names it from now, not from the next play.
                    self.resting_output = Some(entry.name.clone());
                    self.status = format!("switching output to {}…", entry.name);
                    self.log
                        .push(LogLevel::Info, "device", format!("output → {}", entry.name));
                } else {
                    self.set_error(
                        ErrorOwner::Engine,
                        format!("the engine is busy; {} was not selected", entry.name),
                    );
                }
            }
            // The `none` row, and Enter on the input already chosen, which
            // says the same thing: no input is open, `s("in")` is silence,
            // and the mixer's `in` strip goes.
            DeviceKind::AudioInput
                if entry.name == NO_INPUT_NAME
                    || self.chosen_input() == Some(entry.name.as_str()) =>
            {
                if self.worker.try_set_input(None) {
                    self.remember_audio_input(None);
                    self.status =
                        "audio in off - s(\"in\") is silence until an input is chosen".into();
                    self.log
                        .push(LogLevel::Info, "device", "input → off".to_owned());
                    self.panel = None;
                } else {
                    self.set_error(
                        ErrorOwner::Engine,
                        "the engine is busy; the input was not let go".into(),
                    );
                }
            }
            DeviceKind::AudioInput => {
                if self.worker.try_set_input(Some(entry.name.clone())) {
                    self.remember_audio_input(Some(&entry.name));
                    self.status = format!(
                        "audio in → {} - s(\"in\") plays it; in:1, in:2… its other channels; none turns it off",
                        entry.name
                    );
                    self.log
                        .push(LogLevel::Info, "device", format!("input → {}", entry.name));
                    self.panel = None;
                } else {
                    self.set_error(
                        ErrorOwner::Engine,
                        format!("the engine is busy; {} was not selected", entry.name),
                    );
                }
            }
            // A MIDI row is acted on by `handle_midi_tab_key`, which knows
            // whether it is a port or a clock row; there is no plain entry to
            // copy here.
            DeviceKind::Midi => {}
            DeviceKind::Gamepad => {
                let snippet = kind.snippet(&entry.name);
                match self.clipboard.set_text(snippet.clone()) {
                    Ok(()) => {
                        self.status =
                            format!("copied \"{snippet}\" - paste it into .midi() or midin()");
                        if dismiss {
                            self.panel = None;
                        }
                    }
                    Err(error) => self.set_error(
                        ErrorOwner::Editor,
                        format!("could not copy {snippet}: {error}"),
                    ),
                }
            }
        }
        self.dirty_frame = true;
    }
}

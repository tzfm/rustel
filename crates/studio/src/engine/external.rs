use super::*;

impl StudioEngine {
    /// Hand this turn's `.osc()` bundles to the network through
    /// [`Self::dispatch_osc`], as the CLI's live loop does.
    #[cfg(feature = "osc")]
    pub(super) fn send_pending_osc(&mut self, device_now: f64, step_failed: bool) {
        let pending = self.session.take_pending_osc();
        // A failed query pass sends nothing, as `.midi()` does: what it
        // staged is not a schedule, and a receiver that sounded it would
        // disagree with the audio for that cycle.
        if step_failed {
            return;
        }
        self.dispatch_osc(pending, device_now);
    }

    /// Put `.osc()` bundles on the wire: each carries the NTP time its onset
    /// is due - the device clock's distance to the onset's target, from now -
    /// and the receiver schedules on it, which is how Tidal drives
    /// SuperDirt and keeps the sender's jitter out of the sound. A socket
    /// that cannot be opened is reported once; the bundles of that turn are
    /// dropped rather than queued against a port that is not coming. A bundle
    /// once sent is not recallable.
    #[cfg(feature = "osc")]
    fn dispatch_osc(
        &mut self,
        pending: Vec<(f64, rustel_runtime::osc_bridge::OscOnset)>,
        device_now: f64,
    ) {
        if pending.is_empty() {
            return;
        }
        if self.osc_sender.is_none() && !self.osc_open_failed {
            match rustel_osc::OscSender::new() {
                Ok(sender) => self.osc_sender = Some(sender),
                Err(message) => {
                    self.osc_open_failed = true;
                    queue_diagnostic(
                        &mut self.pending_diagnostics,
                        StudioDiagnostic::message(
                            "osc",
                            format!("cannot open a socket for .osc(): {message}"),
                        ),
                    );
                }
            }
        }
        let Some(sender) = self.osc_sender.as_ref() else {
            return;
        };
        for (_lead_secs, intent) in pending {
            let Some(destination) = intent.destination else {
                continue;
            };
            let when = std::time::SystemTime::now() + output_wait(intent.target_time, device_now);
            if sender.send_dirt(destination, when, &intent.args) {
                // The bundle is out: it stands for the copy that a takeover
                // stages of the same onset.
                self.session
                    .note_osc_handed_out(intent.generation, intent.target_time);
                self.pending_accepted_audio.push(UiAcceptedOnset {
                    generation: intent.generation,
                    onset_id: intent.onset_id,
                    frequency_hz: None,
                    gain: None,
                });
            }
        }
    }

    #[cfg(not(feature = "osc"))]
    pub(super) fn send_pending_osc(&mut self, _device_now: f64, _step_failed: bool) {}

    /// How `.midi()` ports are opened. The default opens the platform's;
    /// a test hands in one that captures what is written.
    pub fn set_midi_port_opener(
        &mut self,
        opener: std::sync::Arc<rustel_runtime::midi_bridge::MidiOpenFn>,
    ) {
        self.midi_outputs = rustel_runtime::midi_bridge::MidiOutputs::with_opener(opener);
        // The replacement counts from zero, so the snapshot the counters
        // are compared against has to start again too: left as it was, no
        // new trouble ever exceeds the old one and none is ever reported.
        self.midi_report = rustel_midi::MidiReport::default();
        self.midi_trouble_reported = [false; 4];
    }

    /// How `.serial()` ports are opened. The default opens the platform's;
    /// a test hands in one that captures what is written.
    ///
    /// Only the opener is swapped, unlike the MIDI hook, which replaces its
    /// outputs: ports already open, remembered or still opening stay as
    /// they are, so an open in flight is still adopted and no port is asked
    /// for twice. Ports named from here on open through `opener`.
    #[cfg(feature = "serial")]
    pub fn set_serial_port_opener(
        &mut self,
        opener: std::sync::Arc<rustel_runtime::serial_bridge::SerialOpenFn>,
    ) {
        self.serial_outputs.set_opener(opener);
    }

    /// How a score's `midin`/`midikeys` ports are opened. A test hands in
    /// one that records what was asked for instead of touching hardware.
    #[cfg(test)]
    pub(crate) fn set_midi_input_opener(
        &mut self,
        opener: std::sync::Arc<dyn rustel_runtime::midi_input::InputOpener>,
    ) {
        self.midi_inputs = Some(rustel_runtime::midi_input::quiet_inputs(opener));
    }

    /// The wall-clock time this turn's `.midi()` onset should sound: the
    /// device-clock instant the intent asked for, plus the master limiter's
    /// runway. The limiter holds the audio back after the device clock, so
    /// MIDI timed straight off that clock would fire ahead of the sound it
    /// belongs to. Off holds nothing back and adds nothing; a character
    /// change lands on the next scheduling turn, never on notes already
    /// placed. The offline `render`/`--export` path needs none of this: it
    /// consumes no pending MIDI, and runs the limiter only when asked.
    pub(super) fn midi_instant_for(
        &mut self,
        device_now: f64,
        target_time: f64,
        sample_rate: u32,
    ) -> Instant {
        let runway = self.master.limiter_latency_frames(sample_rate);
        self.midi_clock.instant_for(device_now, target_time)
            + Duration::from_secs_f64(runway as f64 / f64::from(sample_rate))
    }

    /// Hand this turn's `.midi()` onsets to their ports through
    /// [`Self::dispatch_midi`], as the CLI's live loop does. A failed query
    /// pass sends nothing: what it staged is not a schedule.
    pub(super) fn send_pending_midi(&mut self, device_now: f64, step_failed: bool) {
        let mut pending = self.session.take_pending_midi();
        if step_failed {
            pending.clear();
        }
        self.dispatch_midi(pending, device_now);
    }

    /// Hand `.midi()` onsets to their ports. Every intent of the audible or
    /// the session generation is planned whole, note-on and note-off
    /// together, timed from one clock that follows the device's, and
    /// admitted atomically; a port is reserved for its generation before
    /// anything is sent, so the retirement of the old generation's notes
    /// never cuts the new ones.
    pub(super) fn dispatch_midi(
        &mut self,
        pending: Vec<rustel_runtime::midi_bridge::MidiOnset>,
        device_now: f64,
    ) {
        // Settled before the device is borrowed, because saying a port is
        // not enabled needs the diagnostics queue and the device is held for
        // the rest of this function. One pass over the distinct ports also
        // means the refusal is decided once a turn rather than once an
        // onset, whatever the score is striking.
        let mut refused_ports = std::collections::BTreeSet::new();
        let mut checked = std::collections::BTreeSet::new();
        for port in pending.iter().map(|intent| intent.port.clone()) {
            if !checked.insert(port.clone()) {
                continue;
            }
            if !self.midi_out_allowed(&port) {
                refused_ports.insert(port);
            }
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let device = &live.device;
        let audible = device.generation();
        let session_generation = self.session.generation();
        let live_generation =
            |generation: u64| generation == audible || generation == session_generation;
        for intent in &pending {
            if live_generation(intent.generation)
                && rustel_midi::has_output(&intent.controls)
                && !refused_ports.contains(&intent.port)
                && let Err(message) = self
                    .midi_outputs
                    .reserve_generation_port(intent.generation, &intent.port)
            {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("midi", message),
                );
            }
        }
        for notice in
            self.midi_outputs
                .poll_notices_at(audible, session_generation, device.clock_frames())
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                midi_output_diagnostic(notice),
            );
        }
        let sample_rate = device.sample_rate();
        for intent in pending {
            if !live_generation(intent.generation) {
                continue;
            }
            if refused_ports.contains(&intent.port) {
                continue;
            }
            let planned = rustel_midi::plan(&intent.controls, intent.duration_secs);
            if planned.is_empty() {
                continue;
            }
            let base = self.midi_instant_for(device_now, intent.target_time, sample_rate);
            let batch =
                self.midi_note_order
                    .stamp_batch(&intent.port, intent.target_time, base, &planned);
            match self.midi_outputs.submit_batch_for_generation_at(
                intent.generation,
                &intent.port,
                rustel_runtime::midi_bridge::onset_frame_at(intent.target_time, sample_rate),
                batch,
            ) {
                Ok(true) => self.pending_accepted_audio.push(UiAcceptedOnset {
                    generation: intent.generation,
                    onset_id: intent.onset_id,
                    frequency_hz: None,
                    gain: None,
                }),
                Ok(false) => {}
                Err(message) => queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("midi", message),
                ),
            }
        }
        self.note_midi_troubles();
    }

    /// Ask the pattern again, from close to now, when a key has been
    /// pressed since the last look.
    ///
    /// A `midikeys` press is heard when the scheduler next queries the
    /// pattern, and the scheduler queries up to its half-second horizon
    /// ahead. Without this re-query a key press can wait most of that
    /// horizon before it sounds.
    ///
    /// A slider move does the same: the score is re-queried from one
    /// continuity margin ahead, so the change is heard at once. A key
    /// press gets the same re-query, and its delay is that margin: a few
    /// milliseconds, not half a second.
    ///
    /// The engine counts presses to detect a key. The driver thread writes
    /// the ring and the producer reads it, and a counter is the cheapest
    /// value that crosses between the two threads.
    pub(super) fn hear_midi_keys_now(&mut self) {
        let audible = self.live.as_ref().map_or_else(
            || self.session.generation(),
            |live| live.device.generation(),
        );
        let session = self.session.generation();
        let pressed: u64 = self
            .session
            .midi_input_bus()
            .snapshot_for(audible, session)
            .iter()
            .map(|port| port.keys.presses())
            .sum();
        if pressed == self.midi_keys_seen {
            return;
        }
        self.midi_keys_seen = pressed;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // A start's first query reads the keys as they stand.
        if live.initial_start_generation.is_some() {
            return;
        }
        let now = live.device.clock_seconds();
        if let Ok(Some((before, after))) = self.session.requery_active_at(now) {
            let live = self.live.as_mut().expect("checked");
            live.producer.arm_control_requery(before, after);
        }
    }

    /// Open the ports the evaluated score asks to listen to, and close the
    /// ones it stopped asking for.
    ///
    /// Both generations go in, as the CLI's live loop passes them. A
    /// candidate score can be accepted and still miss its first scheduling
    /// window, and its input generation has to retire the failed one's
    /// ports.
    pub(super) fn open_score_midi_inputs(&mut self) {
        let audible = self.live.as_ref().map_or_else(
            || self.session.generation(),
            |live| live.device.generation(),
        );
        let session = self.session.generation();
        let bus = self.session.midi_input_bus();
        // A studio that never opens a keyboard never starts the manager's
        // thread. Once a score has asked for a port the manager stays, so
        // that it is still there to close what the next score stops asking
        // for.
        if self.midi_inputs.is_none() && bus.snapshot_for(audible, session).is_empty() {
            return;
        }
        // Said once per port, before the gate drops it from what is opened:
        // a controller that simply never answers, with nothing in the log,
        // is the silence a player spends the first half of a set blaming on
        // their cable.
        use crate::devices::MidiDirection;
        for port in bus.snapshot_for(audible, session) {
            if self.midi_enabled.allows(MidiDirection::In, &port.selector) {
                continue;
            }
            let name = self
                .midi_enabled
                .port_named(MidiDirection::In, &port.selector)
                .unwrap_or(&port.selector)
                .to_owned();
            if self.midi_disabled_reported.insert(format!("in:{name}")) {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message(
                        "midi",
                        format!(
                            "{name} is not enabled for MIDI in - switch it on in the devices panel"
                        ),
                    ),
                );
            }
        }
        let enabled = &self.midi_enabled;
        let messages = self
            .midi_inputs
            .get_or_insert_with(rustel_runtime::midi_input::MidiInputs::new)
            .sync_allowed(&bus, audible, session, |selector| {
                enabled.allows(MidiDirection::In, selector)
            });
        for message in messages {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("midi", message),
            );
        }
    }

    /// Whether a score may play this port, saying why once when it may not.
    ///
    /// Said once per port rather than once per onset: a disabled port struck
    /// four times a bar would otherwise fill the log with the same sentence
    /// faster than anyone could read the first one, and bury whatever else
    /// the set was trying to say.
    ///
    /// The panel is named rather than the key that opens it, because that key
    /// is rebindable and a message naming the wrong one is worse than a
    /// message naming none.
    ///
    /// The refusal names the port the selector LANDED on rather than what the
    /// score wrote: `.midi("Maschine")` refused as "Maschine MK3 EXT MIDI" is
    /// the row a player then has to find and tick, and "Maschine" is not.
    fn midi_out_allowed(&mut self, selector: &str) -> bool {
        use crate::devices::MidiDirection;
        if self.midi_enabled.allows(MidiDirection::Out, selector) {
            return true;
        }
        let port = self
            .midi_enabled
            .port_named(MidiDirection::Out, selector)
            .unwrap_or(selector)
            .to_owned();
        if self.midi_disabled_reported.insert(format!("out:{port}")) {
            let message =
                format!("{port} is not enabled for MIDI out - switch it on in the devices panel");
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("midi", message),
            );
        }
        false
    }

    /// What the output ports' counters say went wrong since last looked,
    /// each kind told once a set: a full queue, late drops, a device
    /// rejecting messages, and errors the ports could not even keep.
    fn note_midi_troubles(&mut self) {
        let report = self.midi_outputs.report();
        let troubles = [
            (
                report.refused_full > self.midi_report.refused_full,
                "MIDI output queue filled; a complete onset was refused",
            ),
            (
                report.dropped_late > self.midi_report.dropped_late,
                "MIDI messages arrived too late and were dropped",
            ),
            (
                report.send_errors > self.midi_report.send_errors,
                "a MIDI output device rejected messages or disconnected",
            ),
            (
                report.errors_dropped > self.midi_report.errors_dropped,
                "MIDI errors were dropped before they could be reported",
            ),
        ];
        for (index, (happened, text)) in troubles.into_iter().enumerate() {
            if happened && !self.midi_trouble_reported[index] {
                self.midi_trouble_reported[index] = true;
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("midi", text),
                );
            }
        }
        self.midi_report = report;
    }

    /// Hand this turn's `.serial()` writes to their ports through
    /// [`Self::dispatch_serial`]. The platform open runs on a thread of the
    /// bridge's own, so a port that cannot be opened, or is still opening
    /// after a couple of seconds, is told here a turn later.
    #[cfg(feature = "serial")]
    pub(super) fn send_pending_serial(&mut self, device_now: f64, step_failed: bool) {
        let pending = self.session.take_pending_serial();
        // Taken even on a failed pass, as MIDI's poll is, so a completed
        // open installs and a failure or a stalled open is told without
        // waiting for a good pass.
        for news in self.serial_outputs.poll() {
            // Once a set by text. The bridge tells each thing once, but two
            // selectors ("" and "default") can fail in the same words.
            if self.serial_reported.insert(news.message.clone()) {
                // A port still opening is news, not a fault - the CLI prints
                // it as waiting - so it goes to the log and the status line
                // at Info. A failed open and refused or dropped writes are
                // warnings, as MIDI's are.
                let diagnostic = match news.kind {
                    rustel_runtime::serial_bridge::SerialNewsKind::StillOpening => {
                        StudioDiagnostic::info("serial", news.message)
                    }
                    rustel_runtime::serial_bridge::SerialNewsKind::Failure
                    | rustel_runtime::serial_bridge::SerialNewsKind::Trouble => {
                        StudioDiagnostic::message("serial", news.message)
                    }
                };
                queue_diagnostic(&mut self.pending_diagnostics, diagnostic);
            }
        }
        // A failed query pass writes nothing, as `.midi()` does.
        if step_failed {
            return;
        }
        self.dispatch_serial(pending, device_now);
    }

    /// Put `.serial()` writes on the wire. The wire has no notion of later,
    /// so the sender schedules each write for its onset, plus the fixed
    /// latency strudel.cc adds, kept so a sketch tuned in the browser is not
    /// early here. A write in the sender or in the pre-open queue is not
    /// recallable within a set.
    #[cfg(feature = "serial")]
    fn dispatch_serial(
        &mut self,
        pending: Vec<(f64, rustel_runtime::serial_bridge::SerialOnset)>,
        device_now: f64,
    ) {
        for (_lead_secs, intent) in pending {
            let due = Instant::now()
                + output_wait(intent.target_time, device_now)
                + rustel_serial::SERIAL_LATENCY;
            let submitted =
                match self
                    .serial_outputs
                    .submit(&intent.port, intent.baud, due, intent.bytes)
                {
                    Ok(submitted) => submitted,
                    Err(message) => {
                        // Once a set. The refusal repeats on every onset of
                        // every tick otherwise, and the studio log is a bounded
                        // ring, so the repeat evicts everything else in it.
                        if self.serial_reported.insert(message.clone()) {
                            queue_diagnostic(
                                &mut self.pending_diagnostics,
                                StudioDiagnostic::message("serial", message),
                            );
                        }
                        continue;
                    }
                };
            // A baud note is news, not a refusal: the write still goes out,
            // at the baud the port has for this set, and the bridge dedupes
            // the note. That includes a restart's note that the last set's
            // open is being replaced at this set's baud.
            if let Some(note) = submitted.note {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("serial", note),
                );
            }
            // Accepted when written to an open port or held while it opens;
            // either way this turn has nothing left to send.
            if submitted.accepted {
                // The sender has the write: it stands for the copy that a
                // takeover stages of the same onset.
                self.session
                    .note_serial_handed_out(intent.generation, intent.target_time);
                self.pending_accepted_audio.push(UiAcceptedOnset {
                    generation: intent.generation,
                    onset_id: intent.onset_id,
                    frequency_hz: None,
                    gain: None,
                });
            }
        }
    }

    #[cfg(not(feature = "serial"))]
    pub(super) fn send_pending_serial(&mut self, _device_now: f64, _step_failed: bool) {}

    /// Send what the [`ShieldHold`] has due within the steady cover of
    /// `device_now`, the span a steady drain sends ahead. A line cut that
    /// has fired takes its span out first, and one still ahead keeps what
    /// lies at or past it waiting. A stopped transport sounds nothing more,
    /// so its hold is dropped.
    pub(super) fn release_shield_hold(&mut self, device_now: f64) {
        if self.session.transport().is_stopped() {
            self.shield_hold.clear();
            return;
        }
        let Some((armed_line, sample_rate)) = self
            .live
            .as_ref()
            .map(|live| (live.device.armed_line_frame(), live.device.sample_rate()))
        else {
            return;
        };
        let unfired_line = match armed_line {
            Some(line) if self.retire_past_fired_line(line) => None,
            line => line,
        };
        let released = self.shield_hold.release(
            device_now,
            self.session.live_producer_schedule_cover(),
            unfired_line,
            sample_rate,
        );
        if !released.midi.is_empty() {
            self.dispatch_midi(released.midi, device_now);
        }
        #[cfg(feature = "osc")]
        self.dispatch_osc(released.osc, device_now);
        #[cfg(feature = "serial")]
        self.dispatch_serial(released.serial, device_now);
    }

    /// Retire what the external outputs owe the outgoing score from
    /// `line_frame` once the render frontier has passed it, since a cut
    /// armed there has retired the score's audio. Returns whether the
    /// frontier had passed it.
    pub(super) fn retire_past_fired_line(&mut self, line_frame: u64) -> bool {
        let Some((generation, sample_rate)) = self
            .live
            .as_ref()
            .filter(|live| line_passed(&live.device, line_frame))
            .map(|live| (live.device.generation(), live.device.sample_rate()))
        else {
            return false;
        };
        self.external_outputs()
            .retire_from(generation, line_frame, sample_rate);
        true
    }

    /// The outputs outside the device that follow the score's audio.
    pub(super) fn external_outputs(&mut self) -> ExternalOutputs<'_> {
        ExternalOutputs {
            midi: &mut self.midi_outputs,
            hold: &mut self.shield_hold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;
    mod midi_diagnostics {
        use super::*;
        use rustel_runtime::midi_bridge::MidiOutputNotice;

        fn failed(port: &str) -> StudioDiagnostic {
            midi_output_diagnostic(MidiOutputNotice::OpenFailed {
                port: port.into(),
                message: format!("could not open {port}"),
            })
        }

        fn recovered(port: &str) -> StudioDiagnostic {
            midi_output_diagnostic(MidiOutputNotice::Opened { port: port.into() })
        }

        fn deliver_alerts(pending: &mut PendingDiagnostics, log: &mut crate::log::StudioLog) {
            emit_pending_diagnostics(pending, &mut |update| {
                let StudioUpdate::Diagnostic(diagnostic) = update else {
                    panic!("expected a diagnostic");
                };
                match diagnostic.alert {
                    Some(DiagnosticAlert::Raise(key)) => log.push_alert(
                        crate::log::Level::Warn,
                        &diagnostic.kind,
                        diagnostic.message,
                        key,
                    ),
                    Some(DiagnosticAlert::Resolve(key)) => {
                        assert!(log.resolve_alert(&key) > 0, "the warning must exist");
                    }
                    None => {}
                }
                Ok(())
            });
        }

        fn block_delivery(pending: &mut PendingDiagnostics) {
            emit_pending_diagnostics(pending, &mut |update| {
                Err((UiEventSendStatus::DroppedFull, update))
            });
        }

        #[test]
        fn recovered_midi_port_clears_only_its_warning_even_when_diagnostics_are_full() {
            assert_eq!(
                failed("Wavetable").alert,
                Some(DiagnosticAlert::Raise("midi:output:Wavetable".into()))
            );
            assert_eq!(
                recovered("Wavetable").alert,
                Some(DiagnosticAlert::Resolve("midi:output:Wavetable".into()))
            );
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(&mut pending, failed("Wavetable"));
            queue_diagnostic(&mut pending, failed("Other"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 2);

            queue_diagnostic(&mut pending, recovered("Wavetable"));
            for index in 0..(MAX_PENDING_DIAGNOSTICS * 2) {
                queue_diagnostic(
                    &mut pending,
                    StudioDiagnostic::message("test", format!("other warning {index}")),
                );
                assert!(pending.len() <= MAX_PENDING_DIAGNOSTICS);
            }
            block_delivery(&mut pending);
            assert_eq!(log.unseen(), 2, "the UI still has both warnings");
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 1, "the other port still needs attention");
        }

        #[test]
        fn successful_opens_without_warnings_do_not_fill_diagnostics() {
            let mut pending = PendingDiagnostics::default();
            for index in 0..(MAX_PENDING_DIAGNOSTICS * 4) {
                queue_diagnostic(&mut pending, recovered(&format!("port-{index}")));
                block_delivery(&mut pending);
                assert!(pending.is_empty());
            }
        }

        #[test]
        fn recovery_slots_stay_bounded_until_the_ui_accepts_them() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            for index in 0..MAX_PENDING_DIAGNOSTICS {
                queue_diagnostic(&mut pending, failed(&format!("port-{index}")));
                deliver_alerts(&mut pending, &mut log);
            }
            assert_eq!(log.unseen(), MAX_PENDING_DIAGNOSTICS);
            for index in 0..MAX_PENDING_DIAGNOSTICS {
                queue_diagnostic(&mut pending, recovered(&format!("port-{index}")));
            }
            for index in MAX_PENDING_DIAGNOSTICS..(MAX_PENDING_DIAGNOSTICS * 4) {
                let port = format!("port-{index}");
                queue_diagnostic(&mut pending, failed(&port));
                queue_diagnostic(&mut pending, recovered(&port));
                queue_diagnostic(
                    &mut pending,
                    StudioDiagnostic::message("test", "other warning"),
                );
                block_delivery(&mut pending);
                assert_eq!(pending.len(), MAX_PENDING_DIAGNOSTICS);
            }
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0, "every admitted warning clears");
            assert!(pending.is_empty());

            queue_diagnostic(&mut pending, failed("later-port"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(
                log.unseen(),
                1,
                "successful handoff releases admission capacity"
            );
            queue_diagnostic(&mut pending, recovered("later-port"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0);
        }

        #[test]
        fn renewed_failure_supersedes_recovery_while_the_ui_is_blocked() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(&mut pending, failed("Wavetable"));
            deliver_alerts(&mut pending, &mut log);
            queue_diagnostic(&mut pending, recovered("Wavetable"));
            block_delivery(&mut pending);
            queue_diagnostic(&mut pending, failed("Wavetable"));
            block_delivery(&mut pending);
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0], failed("Wavetable"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(
                log.unseen(),
                1,
                "the renewed failure must not clear the warning"
            );
            queue_diagnostic(&mut pending, recovered("Wavetable"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0, "recovery clears the warning for this port");
        }

        #[test]
        fn sample_recovery_survives_midi_diagnostic_pressure() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(
                &mut pending,
                StudioDiagnostic::message("sample-failed", "missing sample")
                    .raising("samples:import:kit"),
            );
            deliver_alerts(&mut pending, &mut log);
            queue_diagnostic(
                &mut pending,
                StudioDiagnostic::resolve("samples:import:kit"),
            );
            for index in 0..(MAX_PENDING_DIAGNOSTICS * 2) {
                queue_diagnostic(&mut pending, failed(&format!("port-{index}")));
                block_delivery(&mut pending);
                assert!(pending.len() <= MAX_PENDING_DIAGNOSTICS);
            }
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(
                log.resolve_alert("samples:import:kit"),
                0,
                "the queued recovery was delivered"
            );
            assert_eq!(log.unseen(), MAX_PENDING_DIAGNOSTICS - 1);
        }

        #[test]
        fn a_sample_warning_and_its_immediate_recovery_both_reach_the_log() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(
                &mut pending,
                StudioDiagnostic::message("sample-loading", "notes skipped while loading")
                    .raising("samples:late:1"),
            );
            queue_diagnostic(&mut pending, StudioDiagnostic::resolve("samples:late:1"));
            queue_diagnostic(&mut pending, StudioDiagnostic::resolve("samples:late:1"));
            block_delivery(&mut pending);
            assert_eq!(pending.len(), 2, "retain the warning and one recovery");
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0);
            assert!(pending.is_empty());
        }
    }

    /// The studio opens the ports a score asks to listen to. The pad
    /// listener does not count: it opens ports for pads and CC and never
    /// feeds the score's bus.
    #[test]
    fn a_score_that_asks_to_listen_gets_its_port_opened() {
        use std::sync::Arc;
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        let opener = Arc::new(rustel_runtime::midi_input::RecordingOpener {
            asked: std::sync::Mutex::new(Vec::new()),
        });
        engine.set_midi_input_opener(opener.clone());
        engine
            .session
            .evaluate("const keys = await midikeys('Bass Station II')\n$: keys().s(\"piano\")\n")
            .expect("the score the docs teach");
        // Through the stopped turn, because that is when a musician
        // evaluates: the keyboard must be live before the transport is.
        assert!(!engine.is_playing());
        engine.idle_turn(|_| Ok(()));
        // The manager opens on its own thread; a bounded wait is the whole
        // of the asynchrony here.
        let asked = std::time::Instant::now();
        loop {
            let seen = opener
                .asked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if seen.iter().any(|port| port == "Bass Station II") {
                break;
            }
            assert!(
                asked.elapsed() < Duration::from_secs(5),
                "the studio never asked to open the port the score named: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A score that stops asking for a keyboard releases the port, and the
    /// transport does not have to run for that.
    #[test]
    fn a_score_that_stops_asking_lets_the_port_go() {
        use std::sync::Arc;
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        engine.set_midi_input_opener(Arc::new(rustel_runtime::midi_input::RecordingOpener {
            asked: std::sync::Mutex::new(Vec::new()),
        }));
        engine
            .session
            .evaluate("const keys = await midikeys('Bass Station II')\n$: keys().s(\"piano\")\n")
            .expect("score");
        engine.open_score_midi_inputs();
        assert!(
            engine
                .session
                .midi_input_bus()
                .find("Bass Station II")
                .is_some(),
            "the score asked for it"
        );
        engine
            .session
            .evaluate("$: s(\"bd*4\")\n")
            .expect("a score with no keyboard in it");
        // Use the turn the worker runs while stopped, not the sync called
        // directly: the release must not depend on playback.
        assert!(!engine.is_playing(), "this is the stopped path");
        engine.idle_turn(|_| Ok(()));
        let generation = engine.session.generation();
        let wanted = engine
            .session
            .midi_input_bus()
            .snapshot_for(generation, generation);
        assert!(
            wanted.is_empty(),
            "a score that names no port wants none, and nothing had to be \
             playing for it to say so"
        );
    }

    /// And a studio that never opens a keyboard never starts the manager's
    /// thread: the cost of this belongs to the scores that use it.
    #[test]
    fn a_score_that_listens_to_nothing_starts_no_input_manager() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        engine.session.evaluate("$: s(\"bd*4\")").expect("score");
        engine.idle_turn(|_| Ok(()));
        assert!(engine.midi_inputs.is_none());
    }

    /// A line cut the render frontier has reached retires what the shield holds
    /// from its line; one still ahead retires nothing.
    #[cfg(feature = "osc")]
    #[test]
    fn a_fired_line_cut_retires_what_the_shield_holds_from_it() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        let now = device_of(&engine).clock_seconds();
        let rate = device_of(&engine).sample_rate();
        engine
            .shield_hold
            .hold_osc_for_test(&[now - 0.2, now - 0.05, now + 1.0]);
        assert!(!engine.retire_past_fired_line(frame_at(now + 5.0, rate)));
        assert_eq!(
            engine.shield_hold.held_targets_for_test(),
            [now - 0.2, now - 0.05, now + 1.0]
        );
        assert!(engine.retire_past_fired_line(frame_at(now - 0.1, rate)));
        assert_eq!(engine.shield_hold.held_targets_for_test(), [now - 0.2]);
    }

    /// An output recycle discards the audio horizon, and what the shield held
    /// for it goes too.
    #[cfg(feature = "osc")]
    #[test]
    fn an_output_recycle_forgets_what_the_shield_held() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let now = device_of(&engine).clock_seconds();
        engine
            .shield_hold
            .hold_osc_for_test(&[now + 1.0, now + 2.0]);
        engine
            .set_output_device_with(
                rustel_audio::SILENT_OUTPUT_NAME,
                |device, selector, samples| {
                    device.recycle_output_to_with_samples(selector, samples, || false)
                },
            )
            .expect("the output recycles");
        assert!(engine.shield_hold.held_targets_for_test().is_empty());
    }

    /// Withdrawing a line that a published flip already cleared retires nothing:
    /// the audio from that line is the launched score's. An arm still set when
    /// it is withdrawn has fired, and takes what the shield holds past it.
    #[cfg(feature = "osc")]
    #[test]
    fn withdrawing_a_line_retires_only_an_arm_still_set() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        let now = device_of(&engine).clock_seconds();
        let line = frame_at(now - 0.1, device_of(&engine).sample_rate());
        engine.shield_hold.hold_osc_for_test(&[now + 1.0]);
        let device = device_of(&engine);
        device.arm_line_cut(line, true);
        device.set_generation(device.generation() + 1, line, TakeoverCut::AtTakeover);
        engine.withdraw_line_cut(false);
        assert_eq!(engine.shield_hold.held_targets_for_test(), [now + 1.0]);

        device_of(&engine).arm_line_cut(line, true);
        engine.withdraw_line_cut(false);
        assert!(engine.shield_hold.held_targets_for_test().is_empty());
    }

    /// A flip retires the outgoing score's external output from its takeover,
    /// or from the line of an earlier cut that has fired.
    #[test]
    fn a_flip_retires_from_a_fired_line_before_its_takeover() {
        let engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        let device = device_of(&engine);
        let rate = device.sample_rate();
        let now = device.clock_seconds();
        let takeover = frame_at(now + 1.0, rate);
        assert_eq!(retire_frame(device, takeover), takeover);
        device.arm_line_cut(frame_at(now + 5.0, rate), true);
        assert_eq!(retire_frame(device, takeover), takeover);
        device.arm_line_cut(frame_at(now - 0.1, rate), true);
        assert_eq!(retire_frame(device, takeover), frame_at(now - 0.1, rate));
    }

    /// Before a drain releases what the shield holds, a fired line takes what
    /// lies past it, and a line still ahead keeps what lies past it waiting.
    #[cfg(feature = "osc")]
    #[test]
    fn the_shield_releases_nothing_past_an_armed_line() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        hold_clock(&engine, true);
        let now = device_of(&engine).clock_seconds();
        let rate = device_of(&engine).sample_rate();
        engine
            .shield_hold
            .hold_osc_for_test(&[now + 0.1, now + 0.3]);
        device_of(&engine).arm_line_cut(frame_at(now + 0.2, rate), true);
        engine.release_shield_hold(now);
        assert_eq!(engine.shield_hold.held_targets_for_test(), [now + 0.3]);

        engine.shield_hold.hold_osc_for_test(&[now + 5.0]);
        device_of(&engine).arm_line_cut(frame_at(now - 0.1, rate), true);
        engine.release_shield_hold(now);
        assert!(engine.shield_hold.held_targets_for_test().is_empty());
        hold_clock(&engine, false);
    }

    /// The consumer fires a line cut in the first block that ends past its
    /// line, so the render frontier has passed a line only once it is beyond it.
    #[test]
    fn a_line_is_passed_once_the_frontier_is_beyond_it() {
        let engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.1);
        hold_clock(&engine, true);
        let device = device_of(&engine);
        let frontier = device.render_frontier_frames();
        assert!(!line_passed(device, frontier));
        assert!(line_passed(device, frontier - 1));
        hold_clock(&engine, false);
    }
}

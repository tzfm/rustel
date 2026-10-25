use super::*;

impl Session {
    /// Tell the session that the host handed an `.osc()` bundle to the
    /// network: a bundle of `generation` for the onset at `target_time`, in
    /// seconds on the session clock.
    ///
    /// A bundle that is out cannot be recalled. It stands for the bundle a
    /// later generation stages for the same onset, and the live schedule
    /// leaves that one out. So an edit reaches OSC from the latest onset
    /// handed out, up to one schedule cover after it reaches the audio, and
    /// an onset the edit adds before that is not sent. A takeover with a cut
    /// and a transport start send all their bundles.
    ///
    /// A host that does not call this sends twice each onset that the
    /// outgoing generation handed out past a takeover.
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    pub fn note_osc_handed_out(&mut self, generation: u64, target_time: f64) {
        self.osc_handed_out.note(generation, target_time);
    }

    /// Tell the session that the host handed a `.serial()` write to its
    /// sender: a write of `generation` for the onset at `target_time`, in
    /// seconds on the session clock. A write in the sender cannot be
    /// recalled, and it stands for a later generation's write of the same
    /// onset as an OSC bundle does ([`Self::note_osc_handed_out`]).
    #[cfg(all(feature = "device-audio", feature = "serial"))]
    pub fn note_serial_handed_out(&mut self, generation: u64, target_time: f64) {
        self.serial_handed_out.note(generation, target_time);
    }

    /// Leave out the staged OSC and serial intents of each onset that a
    /// message of an earlier generation is already out for. `onsets` are
    /// the onsets of the window that staged them.
    ///
    /// A takeover with a cut begins a new timeline: until it is published,
    /// the intents of its first window all stay.
    ///
    /// The intents of a key press that this window is the first to place
    /// stay too: no message is out for that press.
    #[cfg(all(feature = "device-audio", any(feature = "osc", feature = "serial")))]
    pub(super) fn leave_out_handed_out_intents(
        &mut self,
        sample_rate: u32,
        onsets: &[OnsetEventJson],
    ) {
        let mut staged = false;
        #[cfg(feature = "osc")]
        {
            staged |= !self.pending_osc.is_empty();
        }
        #[cfg(feature = "serial")]
        {
            staged |= !self.pending_serial.is_empty();
        }
        // A window that staged no intent hands no message out.
        if !staged {
            return;
        }
        // Before the cut is read: a window whose intents all stay counts
        // its presses as placed too.
        let placed = self.js.midi_input_bus().placed_keys();
        let new_presses = self.placed_presses.claim_new(&placed, onsets);
        if self.requery_takeover_cut != TakeoverCut::None {
            return;
        }
        #[cfg(feature = "osc")]
        {
            let handed_out = &self.osc_handed_out;
            self.pending_osc.retain(|(_, intent)| {
                new_presses.contains(&intent.onset_id)
                    || !handed_out.stands_for(intent.generation, intent.target_time, sample_rate)
            });
        }
        #[cfg(feature = "serial")]
        {
            let handed_out = &self.serial_handed_out;
            self.pending_serial.retain(|(_, intent)| {
                new_presses.contains(&intent.onset_id)
                    || !handed_out.stands_for(intent.generation, intent.target_time, sample_rate)
            });
        }
    }

    /// Take the serial intents from the last scheduling pass. A live host
    /// tells the session of each write its sender takes, with
    /// `note_serial_handed_out`.
    #[cfg(feature = "serial")]
    pub fn take_pending_serial(&mut self) -> Vec<(f64, crate::serial_bridge::SerialOnset)> {
        std::mem::take(&mut self.pending_serial)
    }

    /// Convert one scheduler batch into bounded, owned serial intents.
    ///
    /// Score-controlled port names and message bytes are measured before they
    /// are retained, matching the MIDI/OSC per-pass ceilings so a dense
    /// `.serial()` pattern cannot grow the producer queue without bound.
    #[cfg(feature = "serial")]
    pub(super) fn collect_pending_serial(&mut self, onsets: &[OnsetEventJson], now: f64) {
        let mut pending = Vec::new();
        let mut routed = 0usize;
        let mut retained_bytes = 0usize;
        let mut output_limited = false;

        for onset in onsets {
            if crate::serial_bridge::serial_route(onset).is_none() {
                continue;
            }
            routed = routed.saturating_add(1);
            if routed > MAX_SERIAL_ROUTE_ATTEMPTS_PER_PASS {
                output_limited = true;
                break;
            }
            let Some(intent) = crate::serial_bridge::serial_onset(onset) else {
                continue;
            };
            let owned_bytes = intent.port.len().saturating_add(intent.bytes.len());
            let next_bytes = match retained_bytes.checked_add(owned_bytes) {
                Some(next) => next,
                None => {
                    output_limited = true;
                    break;
                }
            };
            if pending.len() >= MAX_SERIAL_INTENTS_PER_PASS
                || next_bytes > MAX_SERIAL_BYTES_PER_PASS
            {
                output_limited = true;
                break;
            }
            retained_bytes = next_bytes;
            pending.push((intent.target_time - now, intent));
        }
        self.pending_serial = pending;

        if output_limited {
            let message = format!(
                "serial output exceeded the per-pass limit of {MAX_SERIAL_ROUTE_ATTEMPTS_PER_PASS} routed onsets, {MAX_SERIAL_INTENTS_PER_PASS} intents, or {MAX_SERIAL_BYTES_PER_PASS} retained bytes"
            );
            self.report_diagnostic(
                "serial-refused",
                message.clone(),
                serde_json::json!({ "serial_refused": { "message": message } }),
            );
        }
    }

    /// The MIDI-input tables this Session's scores have named with `midin()`.
    ///
    /// The live loop attaches platform listeners to these; `render` and `query`
    /// never do, so an offline bounce stays reproducible and never grabs a
    /// musician's controller out from under a live set.
    ///
    /// Not feature-gated: the tables are dependency-free atomics, and a build
    /// without the `midi` feature simply leaves them at zero.
    pub fn midi_input_bus(&self) -> std::sync::Arc<rustel_core::midi_in::InputBus> {
        self.js.midi_input_bus()
    }

    /// Take the MIDI intents from the last scheduling pass.
    ///
    /// Each carries its `target_time` on the session clock, which the live loop
    /// maps to a wall-clock instant through one stable anchor
    /// (`midi_bridge::MidiClock`) rather than per pass.
    #[cfg(feature = "midi")]
    pub fn take_pending_midi(&mut self) -> Vec<crate::midi_bridge::MidiOnset> {
        std::mem::take(&mut self.pending_midi)
    }

    /// Convert one scheduler batch into bounded, owned MIDI intents.
    ///
    /// Route discovery deliberately happens before cloning score-controlled
    /// strings. A score can produce arbitrarily large values, while this queue
    /// lives outside the JavaScript watchdog on the live producer thread.
    #[cfg(feature = "midi")]
    pub(super) fn collect_pending_midi(&mut self, onsets: &[OnsetEventJson]) {
        let mut pending = Vec::new();
        let mut routed = 0usize;
        let mut messages = 0usize;
        let mut retained_bytes = 0usize;
        let mut oversized_route = false;
        let mut output_limited = false;

        // `midi_onset_with_port` resolves midimap registrations, which belong
        // to this Session's detached runtime settings rather than process
        // globals. Keep the entire conversion pass inside that binding.
        self.js.with_runtime_settings(|| {
            for onset in onsets {
                let Some(port) = crate::midi_bridge::midi_route(onset) else {
                    continue;
                };
                routed = routed.saturating_add(1);
                if routed > MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS {
                    output_limited = true;
                    break;
                }
                if port.len() > crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES {
                    oversized_route = true;
                    continue;
                }

                let Some(intent) =
                    crate::midi_bridge::midi_onset_with_port(onset, port.into_owned())
                else {
                    continue;
                };
                let planned = rustel_midi::plan(&intent.controls, intent.duration_secs);
                if planned.is_empty() {
                    continue;
                }

                let next_messages = match messages.checked_add(planned.len()) {
                    Some(next) => next,
                    None => {
                        output_limited = true;
                        break;
                    }
                };
                // The only score-sized allocation retained by an intent is
                // its route. Known MIDI commands are copied from fixed tiny
                // literals and registered maps are independently bounded.
                let owned_bytes = intent.port.len();
                let next_bytes = match retained_bytes.checked_add(owned_bytes) {
                    Some(next) => next,
                    None => {
                        output_limited = true;
                        break;
                    }
                };
                if pending.len() >= MAX_MIDI_INTENTS_PER_PASS
                    || next_messages > MAX_MIDI_MESSAGES_PER_PASS
                    || next_bytes > MAX_MIDI_RETAINED_BYTES_PER_PASS
                {
                    output_limited = true;
                    break;
                }
                messages = next_messages;
                retained_bytes = next_bytes;
                pending.push(intent);
            }
        });
        self.pending_midi = pending;

        if oversized_route && !self.midi_oversized_route_reported {
            self.midi_oversized_route_reported = true;
            let message = format!(
                "MIDI output port names must be at most {} bytes",
                crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES
            );
            self.report_diagnostic(
                "midi-refused",
                message.clone(),
                serde_json::json!({ "midi_refused": { "message": message } }),
            );
        }
        if output_limited && !self.midi_output_limit_reported {
            self.midi_output_limit_reported = true;
            let message = format!(
                "MIDI output exceeded the per-pass limit of {MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS} routed onsets, {MAX_MIDI_INTENTS_PER_PASS} intents, {MAX_MIDI_MESSAGES_PER_PASS} wire messages, or {MAX_MIDI_RETAINED_BYTES_PER_PASS} retained bytes"
            );
            self.report_diagnostic(
                "midi-refused",
                message.clone(),
                serde_json::json!({ "midi_refused": { "message": message } }),
            );
        }
    }

    /// Take the OSC intents from the last scheduling pass, each paired with
    /// its lead in seconds ahead of the scheduled `now`. A live host tells
    /// the session of each bundle it sends, with `note_osc_handed_out`.
    #[cfg(feature = "osc")]
    pub fn take_pending_osc(&mut self) -> Vec<(f64, crate::osc_bridge::OscOnset)> {
        std::mem::take(&mut self.pending_osc)
    }

    #[cfg(feature = "osc")]
    pub(super) fn collect_pending_osc(&mut self, onsets: &[OnsetEventJson], now: f64, cps: f64) {
        self.pending_osc.clear();
        let mut refused = Vec::new();
        let mut encoded_bytes = 0usize;
        let mut route_attempts = 0usize;
        let mut output_limited = false;
        for onset in onsets {
            let Some((host, port)) = crate::osc_bridge::osc_route(onset) else {
                continue;
            };
            route_attempts += 1;
            if route_attempts > MAX_OSC_ROUTE_ATTEMPTS_PER_PASS {
                output_limited = true;
                break;
            }
            let refusal_key = osc_refusal_key(host, port);
            if self.reported_osc_refusals.contains(&refusal_key) {
                continue;
            }
            match self.config.score_osc_access.approve(host, port) {
                Ok(destination) => {
                    // Only an approved route may cause score-controlled values
                    // to be cloned or JSON-serialized.
                    let Some(mut intent) = crate::osc_bridge::osc_onset(onset, cps) else {
                        // One malformed or individually oversized onset is
                        // silent, just like an unsupported OSC control. It
                        // must not suppress independent valid onsets later in
                        // the same scheduler batch or masquerade as aggregate
                        // producer pressure.
                        continue;
                    };
                    if self.pending_osc.len() == MAX_OSC_INTENTS_PER_PASS {
                        output_limited = true;
                        break;
                    }
                    let Some(next_bytes) = encoded_bytes.checked_add(intent.encoded_bytes) else {
                        output_limited = true;
                        break;
                    };
                    if next_bytes > MAX_OSC_BYTES_PER_PASS {
                        output_limited = true;
                        break;
                    }
                    encoded_bytes = next_bytes;
                    intent.destination = Some(destination);
                    self.pending_osc.push((intent.target_time - now, intent));
                }
                Err(message) => {
                    if self.reported_osc_refusals.len() < MAX_PENDING_SESSION_DIAGNOSTICS {
                        self.reported_osc_refusals.push(refusal_key);
                        refused.push(message);
                    }
                }
            }
        }
        if output_limited && !self.osc_output_limit_reported {
            self.osc_output_limit_reported = true;
            refused.push(format!(
                "OSC output exceeded the per-pass limit of {MAX_OSC_ROUTE_ATTEMPTS_PER_PASS} routed onsets, {MAX_OSC_INTENTS_PER_PASS} packets, or {MAX_OSC_BYTES_PER_PASS} encoded bytes"
            ));
        }
        for message in refused {
            self.report_diagnostic(
                "osc-refused",
                message.clone(),
                serde_json::json!({ "osc_refused": { "message": message } }),
            );
        }
    }

    #[cfg(feature = "device-audio")]
    pub(super) fn has_pending_external_output(&self) -> bool {
        #[cfg(feature = "serial")]
        if !self.pending_serial.is_empty() {
            return true;
        }
        #[cfg(feature = "midi")]
        if self
            .pending_midi
            .iter()
            .any(|intent| rustel_midi::has_output(&intent.controls))
        {
            return true;
        }
        #[cfg(feature = "osc")]
        if !self.pending_osc.is_empty() {
            return true;
        }
        false
    }

    /// Accepted intents from this pass, not route names or stale queue totals.
    /// The underlying MIDI/OSC/serial collectors already bound these vectors.
    /// Sorting once keeps prefill accounting linearithmic even for dense mixed
    /// audio/external windows; steady conversion does not build this index.
    #[cfg(feature = "device-audio")]
    pub(super) fn pending_external_onset_keys(&self) -> Vec<(u64, u64)> {
        #[allow(unused_mut)]
        let mut keys = Vec::new();
        #[cfg(feature = "midi")]
        keys.extend(
            self.pending_midi
                .iter()
                .filter(|intent| rustel_midi::has_output(&intent.controls))
                .map(|intent| (intent.generation, intent.onset_id)),
        );
        #[cfg(feature = "osc")]
        keys.extend(
            self.pending_osc
                .iter()
                .map(|(_, intent)| (intent.generation, intent.onset_id)),
        );
        #[cfg(feature = "serial")]
        keys.extend(
            self.pending_serial
                .iter()
                .map(|(_, intent)| (intent.generation, intent.onset_id)),
        );
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::*;
    #[test]
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    fn external_only_replacement_is_renderable_without_a_scalar_voice() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"s("not-a-native-sample").osc(57120).fast(8)"#)
            .expect("external-only score");

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("external-only replacement was rejected: {error}")
            }
        };

        assert!(
            batch.events.is_empty(),
            "fixture unexpectedly rendered locally"
        );
        let pending = session.take_pending_osc();
        assert!(!pending.is_empty(), "OSC intent was not retained");
        assert!(pending.iter().all(|(_, intent)| {
            intent.port == 57120
                && intent
                    .destination
                    .is_some_and(|destination| destination.ip().is_loopback())
        }));
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    fn score_chosen_non_loopback_osc_is_dropped_and_reported_once_per_score() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session
            .evaluate(
                r#"stack(
                    s("not-a-native-sample").osc(57120).oschost("10.0.0.5"),
                    s("not-a-native-sample").osc(57120).oschost("10.0.0.6")
                ).fast(8)"#,
            )
            .expect("score");

        let _ = session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        );
        let pending = session.take_pending_osc();
        assert!(
            pending.is_empty(),
            "an ungranted LAN destination was queued ({} intents)",
            pending.len()
        );
        let diagnostics = session.take_diagnostics();
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == "osc-refused")
                .count(),
            2,
            "one notice per refused destination: {diagnostics:?}"
        );

        // The same score can schedule many dense windows without turning its
        // refused OSC destinations into producer-thread log spam.
        let _ = session.schedule_audio_live_at(
            0.25,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        );
        assert!(session.take_pending_osc().is_empty());
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "osc-refused")
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn osc_refusal_cache_never_retains_score_sized_host_text() {
        assert_eq!(
            osc_refusal_key(" 10.0.0.5 ", 57120),
            osc_refusal_key("::ffff:10.0.0.5", 57120),
            "equivalent spellings must deduplicate as one destination"
        );
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let huge_host = "x".repeat(rustel_osc::MAX_OSC_HOST_BYTES + 1024 * 1024);
        let onset = OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 4],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({
                "oscport": 57120,
                "oschost": huge_host,
                "s": "bd"
            })),
            value_show: String::new(),
            log_line: None,
        };

        session.collect_pending_osc(&[onset.clone(), onset], 0.0, 0.5);

        assert_eq!(
            session.reported_osc_refusals,
            vec![ReportedOscRefusal::OversizedHost]
        );
        assert_eq!(
            session
                .take_diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.kind == "osc-refused")
                .count(),
            1
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn one_oversized_osc_onset_does_not_drop_a_later_valid_onset() {
        let onset = |onset_id, value| OnsetEventJson {
            onset_id,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 4],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        };
        let oversized = onset(
            1,
            serde_json::json!({
                "oscport": 57120,
                "s": "x".repeat(rustel_osc::MAX_DATAGRAM_BYTES * 2)
            }),
        );
        let valid = onset(2, serde_json::json!({ "oscport": 57120, "s": "bd" }));
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_osc(&[oversized, valid], 0.0, 0.5);

        assert_eq!(session.pending_osc.len(), 1);
        assert_eq!(session.pending_osc[0].1.onset_id, 2);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("per-pass limit"))
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn cached_osc_refusals_are_still_bounded_by_the_route_attempt_cap() {
        let onset = OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 4],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({
                "oscport": 57120,
                "oschost": "10.0.0.5",
                "s": "bd"
            })),
            value_show: String::new(),
            log_line: None,
        };
        let onsets = vec![onset; MAX_OSC_ROUTE_ATTEMPTS_PER_PASS + 1];
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_osc(&onsets, 0.0, 0.5);

        let diagnostics = session.take_diagnostics();
        assert_eq!(session.reported_osc_refusals.len(), 1);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("routed onsets"))
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn osc_packet_cap_counts_only_valid_osc_intents() {
        let onset = |onset_id| OnsetEventJson {
            onset_id,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 4],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({ "oscport": 57120, "s": "bd" })),
            value_show: String::new(),
            log_line: None,
        };
        let mut onsets = (0..=MAX_OSC_INTENTS_PER_PASS as u64)
            .map(onset)
            .collect::<Vec<_>>();
        // A non-OSC tail after an exactly full batch does not itself claim
        // another OSC packet or trigger a false cap report.
        let mut exactly_full = onsets[..MAX_OSC_INTENTS_PER_PASS].to_vec();
        exactly_full.push(OnsetEventJson {
            onset_id: u64::MAX,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 4],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({ "s": "bd" })),
            value_show: String::new(),
            log_line: None,
        });
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.collect_pending_osc(&exactly_full, 0.0, 0.5);
        assert_eq!(session.pending_osc.len(), MAX_OSC_INTENTS_PER_PASS);
        assert!(session.take_diagnostics().is_empty());

        session.collect_pending_osc(&onsets, 0.0, 0.5);
        assert_eq!(session.pending_osc.len(), MAX_OSC_INTENTS_PER_PASS);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("per-pass limit"))
        );
        // Avoid retaining the deliberately large fixture beyond this test's
        // assertions on constrained CI runners.
        onsets.clear();
    }

    #[test]
    #[cfg(feature = "osc")]
    fn osc_aggregate_byte_cap_truncates_before_retaining_an_unbounded_batch() {
        let payload = "x".repeat(7_000);
        let onsets = (0..64u64)
            .map(|onset_id| OnsetEventJson {
                onset_id,
                generation: 1,
                whole_begin: "0".into(),
                duration_secs: 0.25,
                target_time: 0.0,
                live_controls: [0; 4],
                ui_visuals: 0,
                value: ValueJson::Raw(serde_json::json!({
                    "oscport": 57120,
                    "s": payload.clone()
                })),
                value_show: String::new(),
                log_line: None,
            })
            .collect::<Vec<_>>();
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_osc(&onsets, 0.0, 0.5);

        assert!(session.pending_osc.len() < onsets.len());
        assert!(
            session
                .pending_osc
                .iter()
                .map(|(_, intent)| intent.encoded_bytes)
                .sum::<usize>()
                <= MAX_OSC_BYTES_PER_PASS
        );
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("per-pass limit"))
        );
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    fn a_granted_osc_host_is_queued_with_a_resolved_address() {
        let mut config = SessionConfig::default();
        config
            .score_osc_access
            .permit_host("10.0.0.5")
            .expect("grant");
        let mut session = Session::with_config(config).expect("session");
        session
            .evaluate(r#"s("not-a-native-sample").osc(57120).oschost("10.0.0.5").fast(8)"#)
            .expect("score");

        let _ = session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        );
        let pending = session.take_pending_osc();
        assert!(!pending.is_empty(), "granted OSC intent was dropped");
        assert!(pending.iter().all(|(_, intent)| {
            intent.destination.is_some_and(|destination| {
                destination.ip() == std::net::IpAddr::from([10, 0, 0, 5])
                    && destination.port() == 57120
            })
        }));
    }

    #[test]
    #[cfg(feature = "midi")]
    fn oversized_and_unplannable_midi_onsets_do_not_hide_a_later_valid_one() {
        let oversized = "x".repeat(crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES + 1);
        let unknown_command = "x".repeat(1024 * 1024);
        let onsets = [
            midi_collection_onset(1, serde_json::json!({ "midiport": oversized, "note": 60 })),
            midi_collection_onset(
                2,
                serde_json::json!({ "midiport": "silent", "midicmd": unknown_command }),
            ),
            midi_collection_onset(3, serde_json::json!({ "midiport": "valid", "note": 64 })),
        ];
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_midi(&onsets);

        assert_eq!(session.pending_midi.len(), 1);
        assert_eq!(session.pending_midi[0].onset_id, 3);
        assert_eq!(
            session
                .take_diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.kind == "midi-refused")
                .count(),
            1
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_route_attempt_cap_bounds_even_silent_routed_haps() {
        let onset =
            midi_collection_onset(1, serde_json::json!({ "midiport": "silent", "s": "bd" }));
        let onsets = vec![onset; MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS + 1];
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_midi(&onsets);
        assert!(session.pending_midi.is_empty());
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("routed onsets"))
        );

        session.collect_pending_midi(&onsets);
        assert!(
            session.take_diagnostics().is_empty(),
            "limit report repeated"
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_intent_cap_counts_only_plannable_intents() {
        let silent =
            midi_collection_onset(1, serde_json::json!({ "midiport": "silent", "s": "bd" }));
        let mut onsets = vec![silent; 600];
        onsets.extend((0..MAX_MIDI_INTENTS_PER_PASS).map(|index| {
            midi_collection_onset(
                index as u64 + 2,
                serde_json::json!({ "midiport": "valid", "note": 60 }),
            )
        }));
        // A non-routed tail after an exactly full batch must not manufacture a
        // false MIDI-cap report.
        onsets.push(midi_collection_onset(
            u64::MAX,
            serde_json::json!({ "s": "bd" }),
        ));
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(session.take_diagnostics().is_empty());

        onsets.push(midi_collection_onset(
            u64::MAX - 1,
            serde_json::json!({ "midiport": "valid", "note": 60 }),
        ));
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("per-pass limit"))
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_retained_byte_cap_accepts_its_exact_boundary() {
        let route = "x".repeat(crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES);
        let mut onsets = (0..MAX_MIDI_INTENTS_PER_PASS)
            .map(|index| {
                midi_collection_onset(
                    index as u64,
                    serde_json::json!({ "midiport": route.clone(), "note": 60 }),
                )
            })
            .collect::<Vec<_>>();
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(session.take_diagnostics().is_empty());

        onsets.push(midi_collection_onset(
            u64::MAX,
            serde_json::json!({ "midiport": route, "note": 60 }),
        ));
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("retained bytes"))
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_message_cap_bounds_mapped_controls_before_retention() {
        let mut canonical = std::collections::BTreeSet::new();
        for name in rustel_core::controls::default_control_registry().names() {
            let name = rustel_core::controls::default_control_registry()
                .canonical_name(name)
                .unwrap_or(name);
            if !matches!(
                name,
                "note"
                    | "midiport"
                    | "midimap"
                    | "midichan"
                    | "ccn"
                    | "ccv"
                    | "ctlNum"
                    | "progNum"
                    | "midibend"
                    | "miditouch"
                    | "midicmd"
            ) {
                canonical.insert(name.to_string());
            }
            if canonical.len() == 32 {
                break;
            }
        }
        assert_eq!(canonical.len(), 32, "control registry fixture is too small");

        let mapping = canonical
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), serde_json::json!(index % 128)))
            .collect::<serde_json::Map<_, _>>();
        let mut value = canonical
            .iter()
            .map(|name| (name.clone(), serde_json::json!(0.5)))
            .collect::<serde_json::Map<_, _>>();
        value.insert("midiport".into(), serde_json::json!("mapped"));
        value.insert("note".into(), serde_json::json!(60));

        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let map_json = serde_json::Value::Object(mapping).to_string();
        session.js.with_runtime_settings(|| {
            rustel_core::midimap::register_midi_map_json("default", &map_json).expect("bounded map")
        });
        let template = midi_collection_onset(1, serde_json::Value::Object(value));
        let messages_per_intent = session.js.with_runtime_settings(|| {
            let intent = crate::midi_bridge::midi_onset(&template).expect("MIDI intent");
            rustel_midi::plan(&intent.controls, intent.duration_secs).len()
        });
        assert!(messages_per_intent > 16, "fixture is not message-dense");
        let admitted = MAX_MIDI_MESSAGES_PER_PASS / messages_per_intent;
        let onsets = (0..=admitted)
            .map(|index| {
                let mut onset = template.clone();
                onset.onset_id = index as u64;
                onset
            })
            .collect::<Vec<_>>();

        session.collect_pending_midi(&onsets);

        assert_eq!(session.pending_midi.len(), admitted);
        assert!(
            session
                .pending_midi
                .iter()
                .map(|intent| rustel_midi::plan(&intent.controls, intent.duration_secs).len())
                .sum::<usize>()
                <= MAX_MIDI_MESSAGES_PER_PASS
        );
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("wire messages"))
        );
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "midi"))]
    fn midiport_without_a_plannable_message_does_not_make_a_replacement_renderable() {
        for source in [
            r#"s("not-a-native-sample").midi('missing-port').fast(8)"#,
            r#"s("not-a-native-sample").note(999).midi('missing-port').fast(8)"#,
        ] {
            let mut session = Session::new().expect("session");
            session.evaluate(source).expect("MIDI-only score");

            match session.schedule_audio_live_at(
                0.0,
                48_000,
                Duration::from_secs(1),
                LiveQueryBudgetMode::ReplacementPrefill,
            ) {
                Err(
                    LiveAudioScheduleError::CandidateCommitted(_)
                    | LiveAudioScheduleError::CandidateRefusedScore(_),
                ) => {}
                Err(LiveAudioScheduleError::Retryable(error))
                | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                    panic!("unplannable MIDI intent was retryable: {error}")
                }
                Ok(batch) => panic!(
                    "unplannable MIDI intent made the replacement renderable: {:?}",
                    batch.events
                ),
            }

            let pending = session.take_pending_midi();
            assert!(
                pending.is_empty(),
                "unplannable MIDI intents must not consume the bounded live queue"
            );
        }
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "midi"))]
    fn note_producing_midi_replacement_is_renderable_without_a_scalar_voice() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"s("not-a-native-sample").note(60).midi('missing-port').fast(8)"#)
            .expect("MIDI note score");

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("note-producing MIDI replacement was rejected: {error}")
            }
        };

        assert!(
            batch.events.is_empty(),
            "fixture unexpectedly rendered scalar audio"
        );
        let pending = session.take_pending_midi();
        assert!(!pending.is_empty(), "MIDI note intent was not retained");
        assert!(
            pending
                .iter()
                .all(|intent| rustel_midi::has_output(&intent.controls)),
            "note-producing MIDI intent was not recognized"
        );
    }
}

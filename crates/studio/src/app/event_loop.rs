//! The studio's main run loop. Each turn it drains the engine, pads and
//! workers, runs the timed housekeeping in `pump_turn`, decides whether a frame
//! is due, works out how long to block (frame pacing, capped by the MIDI pad
//! deadline), and then reads and dispatches the terminal input waiting. It also
//! handles quitting on hangup or a cancel signal, and writes the performance
//! and runtime snapshot records to stderr.

use super::*;

pub(super) const IDLE_POLL: Duration = Duration::from_millis(50);

#[cfg(feature = "remote-control")]
const REMOTE_ACTIVITY_DURATION: Duration = Duration::from_millis(150);

/// How long after the pads were last drained a press may wait to be
/// picked up, while a MIDI press can act (see [`App::pad_press_can_act`]).
/// The press cannot wake the loop's wait the way a key does (the wait
/// blocks on the terminal alone), so a deadline stands in for the wake: a
/// drain every 15 ms costs a handful of try_recv calls a second and keeps a
/// pad launch within 15 ms of its keyboard twin, at any frame rate
/// including the slowest. See [`pad_wait_cap`].
const PAD_ARRIVAL_GRANULARITY: Duration = Duration::from_millis(15);

/// The shortest wait the pad deadline asks for once it has passed, so a
/// turn that ran long never becomes a zero-timeout poll that spins.
const PAD_WAIT_FLOOR: Duration = Duration::from_millis(1);

pub(super) const MAX_EVENTS_PER_TURN: usize = 64;

pub(super) struct StudioRuntimeSnapshotContext<'a> {
    pub(super) snapshot: Option<&'a StudioSnapshot>,
    pub(super) evaluating: bool,
    pub(super) visible_error: Option<String>,
    pub(super) process: ProcessStats,
    pub(super) master_peak_db: f32,
    pub(super) master_lufs: f32,
    pub(super) hydra_frames: Option<u64>,
}

pub(super) fn studio_runtime_snapshot(
    publication: Publication,
    frame_sequence: u64,
    context: StudioRuntimeSnapshotContext<'_>,
) -> StudioRuntimeSnapshot {
    let StudioRuntimeSnapshotContext {
        snapshot,
        evaluating,
        visible_error,
        process,
        master_peak_db,
        master_lufs,
        hydra_frames,
    } = context;
    let device_report = snapshot
        .and_then(|snapshot| snapshot.pressure)
        .map(|pressure| pressure.device);
    let pressure = snapshot.and_then(|snapshot| {
        let pressure = snapshot.pressure?;
        let output = snapshot.device.as_ref().map(|device| device.audio.output());
        Some(
            pressure.report_v1(rustel_runtime::EnginePressureReportContext {
                device_name: output.map(|output| output.device_id().to_owned()),
                requested_buffer_frames: output.map(|output| output.requested_buffer_frames()),
                reported_buffer_frames: output.and_then(|output| output.reported_buffer_frames()),
                process_cpu_percent: process.cpu_percent.map(f64::from),
                process_resident_bytes: process.resident_bytes,
            }),
        )
    });
    StudioRuntimeSnapshot {
        schema_version: 2,
        publication,
        frame_sequence,
        playing: snapshot.is_some_and(|snapshot| snapshot.playing),
        evaluating,
        visible_error,
        session_generation: snapshot.map_or(0, |snapshot| snapshot.session_generation),
        published_generation: snapshot.and_then(|snapshot| snapshot.audible_generation),
        confirmed_audio_generation: snapshot
            .and_then(|snapshot| snapshot.confirmed_audio_generation),
        source_revision: snapshot.and_then(|snapshot| snapshot.source_revision.clone()),
        cps: snapshot.map_or(0.0, |snapshot| snapshot.cps),
        device_time_seconds: snapshot.map_or(0.0, |snapshot| snapshot.device_time),
        submitted_frames: device_report.map(|report| report.submitted_frames),
        callbacks: device_report.map(|report| report.callbacks),
        accepted_events: device_report.map(|report| report.accepted_events),
        stale_events_filtered: device_report.map(|report| report.stale_events_filtered),
        late_events: device_report.map(|report| report.late_events),
        ring_refusals: device_report.map(|report| report.ring_refusals),
        callback_errors: device_report.map(|report| report.callback_errors),
        callback_scope_misses: device_report.map(|report| report.callback_scope_misses),
        callback_allocations: device_report.map(|report| report.callback_allocations),
        callback_frees: device_report.map(|report| report.callback_frees),
        allocator_tripwire_armed: snapshot
            .and_then(|snapshot| snapshot.device.as_ref())
            .map(|device| device.allocator_tripwire_armed),
        hydra_frames,
        master_peak_db,
        master_lufs,
        process_cpu_percent: process.cpu_percent,
        process_resident_bytes: process.resident_bytes,
        pressure,
    }
}

impl App {
    /// One turn of the loop's housekeeping, between frames: every drain,
    /// the debounce timers, the timed decorations. This is the sequence the
    /// event loop runs each turn and the method the end-to-end harness
    /// drives directly, so a test advances the studio through exactly the
    /// turns a live loop would take - one copy of the sequence, not one
    /// per caller, so they cannot drift.
    ///
    /// Returns whether a timed flash is still active and the turn's `now`;
    /// the event loop needs both to decide whether a frame is due.
    /// Debounced work (prefs, readiness, marks) waits on wall-clock time
    /// inside, so a harness's fast settle sees the same state a paused
    /// musician would.
    pub(super) fn pump_turn(&mut self) -> (bool, Instant) {
        self.pump_piano(Instant::now());
        // This turn drains the pads below; see `pads_drained_at`.
        self.pads_drained_at = Instant::now();
        self.flush_pending_prebake();
        self.flush_pending_evaluation();
        #[cfg(feature = "hydra")]
        self.flush_generator_preview(Instant::now());
        self.flush_pending_slider();
        self.tick_replays();
        self.drain_engine();
        self.drain_save();
        self.drain_pads();
        if self.pads.poll_visual_change(ACTIVITY_LIGHT_MILLIS) {
            self.dirty_frame = true;
        }
        // The pads: on macOS they are read here, on the main thread.
        #[cfg(feature = "gamepad")]
        rustel_runtime::gamepad::pump();
        self.drive_mapped_faders(Instant::now());
        self.drain_lint();
        self.pump_theme_editor();
        #[cfg(feature = "hydra")]
        self.sync_theme_sketch();
        // A sketch drawing for the terminal is a moving picture, so the
        // frame is always stale while one is running. Nothing else here
        // would know that: the code has not changed and neither has the
        // status bar.
        #[cfg(feature = "hydra")]
        {
            if self.hydra_backdrop.as_ref().is_some_and(|f| f.wanted())
                || self.hydra_shelf.as_ref().is_some_and(|f| f.wanted())
                || self.hydra_theme.as_ref().is_some_and(|f| f.wanted())
            {
                self.dirty_frame = true;
            }
            self.sync_snippet_preview();
            self.sync_hydra_frame_size();
        }

        let now = Instant::now();
        #[cfg(feature = "remote-control")]
        self.expire_remote_activity(now);
        self.follow_loading_line(now);
        self.dirty_frame |= if self.stop_requested {
            self.audio_advisory.reset()
        } else {
            self.audio_advisory.expire(now)
        };
        for owner in self.errors.expire(now) {
            if let Some(alert) = owner.transient_alert() {
                self.log.retire_alert(alert);
            }
            self.dirty_frame = true;
        }
        if self.prefs_pending
            && now
                >= self
                    .prefs_retry_at
                    .unwrap_or(self.prefs_changed_at + PREFS_DEBOUNCE)
        {
            self.flush_prefs();
        }
        if self.manifest_pending && now.duration_since(self.manifest_changed_at) >= PREFS_DEBOUNCE {
            self.flush_manifest();
        }
        if self.theme_apply_due.is_some_and(|due| now >= due) {
            self.theme_apply_due = None;
            self.preview_theme();
        }
        if self.limiter_mode_due.is_some_and(|(_, due)| now >= due) {
            self.settle_limiter_mode();
        }
        if self.lint_pending && now.duration_since(self.last_edit_at) >= LINT_DEBOUNCE {
            self.submit_lint();
        }
        let catalogue_refresh = if self.library_loading {
            CATALOGUE_LOADING_REFRESH
        } else {
            CATALOGUE_REFRESH
        };
        if self.worker.poll_catalogue() {
            self.install_catalogue();
        }
        if now.duration_since(self.catalogue_refreshed_at) >= catalogue_refresh {
            self.catalogue_refreshed_at = now;
            self.worker.refresh_catalogue();
            self.refresh_catalogue_metadata();
        }
        self.settle_readiness(now);
        self.sync_live_material(now);
        self.send_sample_memory();
        #[cfg(feature = "hydra")]
        self.warm_selected_snippet();
        self.settle_marks(now);
        if let Some(take) = self.sample_take.as_mut() {
            let second = take.started.elapsed().as_secs();
            if second != take.shown_second {
                take.shown_second = second;
                self.dirty_frame = true;
            }
        }
        if now.duration_since(self.readiness_polled_at) >= READINESS_POLL {
            self.poll_readiness();
        }
        self.settle_tape_gestures(now);
        self.watch_take();
        self.poll_export();
        self.poll_reveal();
        if self
            .take_notice
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) >= TAKE_NOTICE_DURATION)
        {
            self.take_notice = None;
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) >= TOAST_DURATION)
        {
            self.toast = None;
            self.dirty_frame = true;
        }
        // Nothing reads this: it is what makes everything else's
        // enumeration tell the truth on macOS, where the port list is a
        // per-process snapshot until somebody holds a client and pumps a
        // run loop. Idempotent, so asking every frame costs an atomic.
        rustel_runtime::midi_hotplug::ensure_watching();
        if self.devices.poll(now, self.panel.is_some()) {
            self.dirty_frame = true;
            self.sync_midi_enablement();
            // A pinned frame opens no real ports: the harness stands in for
            // the host, and the runner's hardware must not leak into what a
            // test sees. Pad learn still syncs on demand, on the chord.
            if !self.pinned_frame_inputs() {
                self.sync_pad_listeners();
            }
        }
        self.push_midi_enablement();
        let flashing = self.advance_evaluation_flash(now);
        // The rotation's own accent decays the same way: still lit keeps
        // the loop painting at the frame rate rather than waiting on the
        // next unrelated event, and its own expiry asks for one more
        // repaint so the flash does not stick on screen past its window.
        let (rotation_flashing, rotation_flash_expired) =
            advance_focus_rotation_flash(&mut self.focus_rotation_flash, now);
        self.dirty_frame |= rotation_flash_expired;
        (flashing || rotation_flashing, now)
    }

    /// The window closed, the tab hung up, the connection dropped.
    ///
    /// There is nothing to draw on and nobody to ask, so this is a way out
    /// like any other: silence, then what is owed to disk. Watched rather
    /// than left to `SIGHUP`, because the terminal can go without the
    /// studio being signalled at all - and because nothing may touch the
    /// input again once it has.
    pub(super) fn pump_terminal_hangup(&mut self, hung_up: bool) -> bool {
        if hung_up {
            self.quit_now();
        }
        hung_up
    }

    /// Whether a signal has asked the studio to stop, and if so, stop it -
    /// writing what is on its way to disk first, exactly as quitting does.
    pub(super) fn pump_cancellation(&mut self) -> bool {
        let cancelled = self
            .options
            .cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire));
        if cancelled {
            self.quit_now();
        }
        cancelled
    }

    pub(super) fn event_loop(
        &mut self,
        terminal: &mut TerminalSession,
    ) -> Result<(), RuntimeError> {
        let mut raw_paste_continuation = false;
        while !self.quit {
            // Art coloured by the clock or the mix is a moving picture:
            // the frame is stale as soon as it is drawn.
            if self.viz_animated() {
                self.dirty_frame = true;
            }
            // A signal is a way out like any other, and what is on the way
            // to disk goes with it: the scene being typed, the set's own
            // file, the preferences. `quit_now` is what ^Q does, and a
            // window closed under the studio must not cost more than
            // quitting it deliberately does.
            //
            // Setting `quit` alone would leave every debounced write
            // unwritten. The signal that reaches a studio in a terminal is
            // almost never one somebody typed: it is the tab being closed
            // (SIGHUP), the session ending, the machine going down.
            if self.pump_cancellation() {
                continue;
            }
            if self.pump_terminal_hangup(terminal.hung_up()) {
                continue;
            }
            let (flashing, now) = self.pump_turn();
            let caret_visible = self
                .theme
                .caret_shape(self.ui_settings.caret_shape)
                .visible_after(now.saturating_duration_since(self.caret_activity));
            self.dirty_frame |= caret_visible != self.caret_visible;
            self.caret_visible = caret_visible;
            // How long a frame is allowed to take. Read each turn, so the
            // setting takes hold on the frame after it is changed rather
            // than at the next launch. Only the animated repaint is paced:
            // a keystroke sets `input_dirty` and paints at once whatever
            // this says, and the engine's own thread and the device's
            // callback are not paced by it at all - which is why a rung
            // above the loop's own 60 is allowed to stand: it can cost
            // the terminal, never a note.
            let frame_interval = self.ui_settings.frame_rate.interval();
            let animating = self.is_playing()
                // The piano indicator fades at the configured frame rate,
                // including before the first note, without starting transport.
                || self.piano.open
                || self.stick_pushing
                || self.replays.values().any(|tab| tab.run.is_some())
                || self.is_evaluating()
                || flashing
                // The preview's picture ends by itself: when it does, one
                // more repaint puts the keys out. (The engine's own frames
                // keep the browser's meter moving while it sounds.)
                || match self.preview_lit_until {
                    Some(until) if now < until => true,
                    Some(_) => {
                        self.preview_lit_until = None;
                        self.dirty_frame = true;
                        false
                    }
                    None => false,
                }
                || self.theme.animates_native_visual()
                || (super::super::settings::animation()
                    && (self.downloads_active()
                        || self.export_job.is_some()
                        || self.cache_clear.is_some()))
                || {
                    #[cfg(feature = "hydra")]
                    {
                        self.settings_webcam_preview_visible()
                    }
                    #[cfg(not(feature = "hydra"))]
                    {
                        false
                    }
                };
            // Whether the screen may paint this turn lives in
            // `frame_is_due`; how long the loop may block lives in
            // `pacing_wait`. Both read the setting each turn so it takes
            // hold on the frame after it is changed.
            #[cfg(feature = "remote-control")]
            if self.remote.is_some() {
                self.poll_remote()?;
            }
            if frame_is_due(
                animating,
                self.dirty_frame,
                self.input_dirty,
                now,
                self.next_frame,
            ) {
                // Set before drawing, so the draw spends the interval rather
                // than extending it.
                self.next_frame =
                    next_frame_deadline(self.next_frame, Instant::now(), frame_interval);
                // Sampling the meter immediately before painting is what
                // makes the peak per frame rather than per loop turn: the
                // engine folds every producer reading in with a maximum, and
                // reading it resets that accumulator.
                self.observe_master(now);
                self.draw(terminal)?;
                self.dirty_frame = false;
                self.input_dirty = false;
            }

            // A pad press has no way to wake this wait (only the terminal
            // owns the descriptor it blocks on), so while a press can act
            // the wait ends by the pad's arrival deadline: a press is picked
            // up within that span of the last drain whatever the loop was
            // otherwise waiting for. A keyboard key needs no such cap - the
            // wait ends on the key itself. With no listener open, or
            // listeners open but nothing bound to what they carry (the IAC
            // bus, ALSA's "Midi Through"), the loop keeps its long idle
            // sleep.
            let wait_from = Instant::now();
            let wait = loop_wait(
                pacing_wait(
                    animating,
                    self.dirty_frame,
                    self.input_dirty,
                    wait_from,
                    self.next_frame,
                    frame_interval,
                ),
                midi_wait_cap(
                    self.pads.open_count(),
                    self.pad_press_can_act(),
                    self.mixer_panel.is_some(),
                    wait_from.saturating_duration_since(self.pads_drained_at),
                ),
            );
            let wait = self.evaluation_flash.as_ref().map_or(wait, |flash| {
                wait.min(flash.next_transition().saturating_duration_since(wait_from))
            });
            let wait = self.piano_wait(wait, wait_from);
            #[cfg(feature = "hydra")]
            let wait = if self.generator_preview_pending {
                wait.min(Duration::from_millis(10))
            } else {
                wait
            };
            #[cfg(feature = "remote-control")]
            let wait = if self.remote.is_some() {
                wait.min(crate::remote::POLL)
            } else {
                wait
            };
            if terminal.poll_event(wait)? {
                // What this batch of input marks stale is told apart from
                // what the turn's animation already had: only the input
                // jumps the frame interval.
                let animated_dirty = std::mem::take(&mut self.dirty_frame);
                let mut terminal_event = terminal.read_event()?;
                self.refresh_cell_pixels(terminal.input_cell_pixels());
                self.pointer_to_cells(&mut terminal_event);
                for event_index in 0..MAX_EVENTS_PER_TURN {
                    if self.accepts_raw_paste() && is_plain_text_key_event(&terminal_event) {
                        for mut event in read_raw_paste_chunk(
                            terminal_event,
                            &mut raw_paste_continuation,
                            MAX_RAW_PASTE_KEY_EVENTS_PER_TURN,
                            MAX_RAW_PASTE_TERMINAL_EVENTS_PER_TURN,
                            |wait| terminal.poll_event(wait),
                            || terminal.read_event(),
                        )? {
                            self.refresh_cell_pixels(terminal.input_cell_pixels());
                            self.pointer_to_cells(&mut event);
                            self.performance.observe_input(&event, Instant::now());
                            self.handle_terminal_event(event)?;
                        }
                    } else {
                        if !matches!(&terminal_event, Event::Key(key) if key.kind == KeyEventKind::Release)
                        {
                            raw_paste_continuation = false;
                        }
                        self.performance
                            .observe_input(&terminal_event, Instant::now());
                        self.handle_terminal_event(terminal_event)?;
                    }
                    if !poll_after_input(&mut raw_paste_continuation, |wait| {
                        terminal.poll_event(wait)
                    })? {
                        break;
                    }
                    // Leave the next event queued when this turn is full.
                    // Reading it here would drop it as the loop exits.
                    if event_index + 1 == MAX_EVENTS_PER_TURN {
                        break;
                    }
                    terminal_event = terminal.read_event()?;
                    self.refresh_cell_pixels(terminal.input_cell_pixels());
                    self.pointer_to_cells(&mut terminal_event);
                }
                self.input_dirty |= self.dirty_frame;
                self.dirty_frame |= animated_dirty;
            } else {
                raw_paste_continuation = false;
            }
        }
        Ok(())
    }

    #[cfg(feature = "remote-control")]
    pub(super) fn expire_remote_activity(&mut self, now: Instant) {
        if let Some(remote) = self.remote.as_mut()
            && remote.activity_until.is_some_and(|until| now >= until)
        {
            remote.activity_until = None;
            self.dirty_frame = true;
        }
    }

    #[cfg(feature = "remote-control")]
    #[cold]
    #[inline(never)]
    pub(super) fn poll_remote(&mut self) -> Result<(), RuntimeError> {
        let batch = match self.remote.as_ref() {
            Some(remote) => remote.poll(),
            None => return Ok(()),
        };
        if batch.is_empty() {
            return Ok(());
        }
        if let Some(remote) = self.remote.as_mut() {
            // Flash for accepted commands; a burst extends the same flash.
            remote.activity_until = Some(Instant::now() + REMOTE_ACTIVITY_DURATION);
            self.dirty_frame = true;
        }
        for event in batch {
            match event {
                crate::remote::RemoteEvent::Key(key) => {
                    let event = Event::Key(key);
                    self.performance.observe_input(&event, Instant::now());
                    self.handle_terminal_event(event)?;
                    self.input_dirty = true;
                    // Disabling the listener also discards its queued keys.
                    if self.remote.is_none() {
                        break;
                    }
                }
                crate::remote::RemoteEvent::Screen(reply) => {
                    if let Some(remote) = self.remote.as_mut() {
                        remote.queue_screen(reply);
                    }
                    // Request a fresh frame at the normal animation rate.
                    self.dirty_frame = true;
                }
            }
        }
        Ok(())
    }

    /// Reply with the completed frame after Ratatui swaps its buffers.
    #[cfg(feature = "remote-control")]
    #[cold]
    #[inline(never)]
    pub(super) fn reply_remote_screen(&mut self, buffer: &ratatui::buffer::Buffer) {
        let Some(remote) = self.remote.as_mut() else {
            return;
        };
        if remote.pending_screens.is_empty() {
            return;
        }
        let json = crate::remote::screen_json(buffer);
        for reply in remote.pending_screens.drain(..) {
            let _ = reply.send(json.clone());
        }
    }

    pub(super) fn emit_performance_snapshot(&self, publication: Publication) {
        let Some(snapshot) = self.performance.snapshot(publication) else {
            return;
        };
        self.emit_performance_records(snapshot, ProcessStats::default());
    }

    pub(super) fn emit_performance_records(
        &self,
        performance: StudioPerformanceSnapshot,
        process: ProcessStats,
    ) {
        let runtime = studio_runtime_snapshot(
            performance.publication,
            performance.frame_sequence,
            StudioRuntimeSnapshotContext {
                snapshot: self.snapshot.as_ref(),
                evaluating: self.is_evaluating(),
                visible_error: self.errors.visible().map(str::to_owned),
                process,
                master_peak_db: self.master.peak_db(),
                master_lufs: self.master.lufs(),
                hydra_frames: self.hydra_frame_count(),
            },
        );
        eprintln!(
            "{}",
            serde_json::json!({ "studio_performance": performance })
        );
        eprintln!("{}", serde_json::json!({ "studio_runtime": runtime }));
    }
}

/// Whether the loop may repaint this turn. The frame interval owns the
/// animated draw while anything animates: per-turn dirty sources (viz
/// docks, engine visuals) do not bypass it, or an animated set would
/// repaint every loop turn and ignore the fps setting. While animating, a
/// due frame paints whether or not something marked the screen dirty - an
/// animation's own advance is that mark - and an animated dirty frame
/// waits for the interval instead of jumping the queue. Input is not
/// animation: a keystroke paints at once, playing or stopped, or typed
/// text trails the hands by up to a whole interval (an eighth of a second
/// at the thriftiest rate). Stopped, a dirty frame paints at once too.
pub(super) fn frame_is_due(
    animating: bool,
    dirty_frame: bool,
    input_dirty: bool,
    now: Instant,
    next_frame: Instant,
) -> bool {
    input_dirty
        || if animating {
            now >= next_frame
        } else {
            dirty_frame
        }
}

/// When the frame after a repaint at `now` falls. A repaint within one
/// interval of `previous` keeps the cadence, so wake-up lateness and drawing
/// time come out of the interval; an early repaint (input) or a missed
/// interval starts a fresh interval from `now`, without catch-up frames.
pub(super) fn next_frame_deadline(
    previous: Instant,
    now: Instant,
    frame_interval: Duration,
) -> Instant {
    let following = previous + frame_interval;
    if now < previous || now >= following {
        now + frame_interval
    } else {
        following
    }
}

/// How long the event loop should block before its next turn. Input waiting
/// to be painted never waits. While anything animates the wait runs to the
/// next due frame whether or not an animated repaint is pending: a dirty
/// frame drawn early would defeat the interval the setting asks for, and a
/// ZERO wait here would spin the loop hot until the frame came due.
/// Stopped, a dirty frame repaints at once and an idle loop keeps its long
/// poll.
pub(super) fn pacing_wait(
    animating: bool,
    dirty_frame: bool,
    input_dirty: bool,
    now: Instant,
    next_frame: Instant,
    frame_interval: Duration,
) -> Duration {
    if input_dirty {
        Duration::ZERO
    } else if animating {
        next_frame
            .saturating_duration_since(now)
            .min(frame_interval)
    } else if dirty_frame {
        Duration::ZERO
    } else {
        IDLE_POLL
    }
}

/// The longest the loop may wait while a MIDI press can act, `since_drain`
/// after the turn that last drained the pads began: whatever remains of the
/// arrival deadline, and never less than a millisecond.
///
/// A deadline rather than a fixed span, because of how a wait ends. On
/// Windows a timeout expires on the system clock tick - 15.6 ms by default,
/// so a fixed 15 ms rounded up to one or two ticks and a press waited up to
/// 31 ms - and the tick is only 15.6 ms while nothing in the process has
/// raised the timer resolution (an audio thread's MMCSS registration can, a
/// dependency's timeBeginPeriod can, and before Windows 10 2004 any other
/// process could). A fixed one-millisecond ask ended on the next default
/// tick but spun the loop a thousand turns a second under a raised
/// resolution. Counted from the draining turn's start, the wait still ends
/// on the next tick at the default resolution (the turn starts on the tick
/// the previous wait ended on) and near 15 ms at a fine one, on every
/// platform.
pub(super) fn pad_wait_cap(since_drain: Duration) -> Duration {
    PAD_ARRIVAL_GRANULARITY
        .saturating_sub(since_drain)
        .max(PAD_WAIT_FLOOR)
}

/// A listener needs the short MIDI deadline while a message can perform an
/// action or while the mixer is visibly presenting the incoming event feed.
pub(super) fn midi_wait_cap(
    open_count: usize,
    can_act: bool,
    feed_visible: bool,
    since_drain: Duration,
) -> Option<Duration> {
    (open_count > 0 && (can_act || feed_visible)).then(|| pad_wait_cap(since_drain))
}

/// The loop's wait this turn: the frame pacing's, capped to `pad_cap`
/// while a MIDI press can act.
pub(super) fn loop_wait(pacing: Duration, pad_cap: Option<Duration>) -> Duration {
    match pad_cap {
        Some(cap) => pacing.min(cap),
        None => pacing,
    }
}

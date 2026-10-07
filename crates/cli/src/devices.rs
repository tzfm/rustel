use super::*;

/// Watch a MIDI input and report what it sends.
#[cfg(feature = "midi")]
pub(super) fn run_midi_monitor(
    port: Option<&str>,
    duration: Option<f64>,
    json: bool,
    quiet: bool,
    learn: bool,
    timing: bool,
) -> Result<(), RuntimeError> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use rustel_midi::input::{InputSummary, MidiEvent, MidiListener, note_name};

    let summary: Arc<Mutex<InputSummary>> = Arc::new(Mutex::new(InputSummary::default()));
    // `--learn` waits for the first controller that moves. A knob often
    // reports its resting value when the port opens, so only a change of
    // value shows that the musician touched the control.
    // channel, controller, and the low/high it was seen to move between.
    type LearnedControl = Option<(u8, u8, u8, u8)>;
    let learned: Arc<Mutex<LearnedControl>> = Arc::new(Mutex::new(None));
    let first_seen: Arc<Mutex<Vec<(u8, u8, u8)>>> = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    let started = Instant::now();

    // Through the same bound as every other window (see `run_gamepad_monitor`):
    // `from_secs_f64` panics on `inf` and on a finite value too large to hold,
    // an absurd-but-representable window would run forever, and user input
    // must never crash us. Validated before a device is touched, so a bad
    // number is refused, and named, even on a machine with no ports at all.
    let duration = duration
        .map(|secs| parse_positive_seconds("duration", secs))
        .transpose()?;

    let listener = {
        let summary = Arc::clone(&summary);
        let learned = Arc::clone(&learned);
        let first_seen = Arc::clone(&first_seen);
        let done = Arc::clone(&done);
        MidiListener::open(port, move |_micros, event, bytes| {
            let is_timing = matches!(event, MidiEvent::Clock | MidiEvent::ActiveSensing);
            if is_timing && !timing {
                return;
            }
            summary
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .observe(event);

            if learn {
                if let MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } = event
                {
                    let mut seen = first_seen.lock().unwrap_or_else(|e| e.into_inner());
                    match seen
                        .iter_mut()
                        .find(|(c, n, _)| *c == channel && *n == controller)
                    {
                        // Seen before at a different value: it moved, so the
                        // musician is turning THIS one.
                        Some((_, _, first)) if *first != value => {
                            let low = (*first).min(value);
                            let high = (*first).max(value);
                            *learned.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some((channel, controller, low, high));
                            done.store(true, Ordering::Relaxed);
                        }
                        Some(_) => {}
                        None => seen.push((channel, controller, value)),
                    }
                }
                return;
            }

            if quiet {
                return;
            }
            let at = started.elapsed().as_secs_f64();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "t": (at * 1000.0).round() / 1000.0,
                        "kind": event.kind(),
                        "channel": event.channel(),
                        "bytes": bytes,
                        "event": format!("{event:?}"),
                    })
                );
                return;
            }
            let line = match event {
                MidiEvent::NoteOn {
                    channel,
                    note,
                    velocity,
                } => format!(
                    "note on   ch{channel:<2} {:<4} ({note:>3})  vel {velocity:>3}",
                    note_name(note)
                ),
                MidiEvent::NoteOff {
                    channel,
                    note,
                    velocity,
                } => format!(
                    "note off  ch{channel:<2} {:<4} ({note:>3})  vel {velocity:>3}",
                    note_name(note)
                ),
                MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } => format!(
                    "cc        ch{channel:<2} cc{controller:<3}        {value:>3}  {}",
                    bar(value)
                ),
                MidiEvent::PitchBend { channel, value } => {
                    format!("bend      ch{channel:<2} {value:>5}")
                }
                MidiEvent::ChannelAftertouch { channel, pressure } => {
                    format!("aftertouch ch{channel:<2} {pressure:>3}")
                }
                MidiEvent::PolyAftertouch {
                    channel,
                    note,
                    pressure,
                } => format!(
                    "poly-at   ch{channel:<2} {:<4} {pressure:>3}",
                    note_name(note)
                ),
                MidiEvent::ProgramChange { channel, program } => {
                    format!("program   ch{channel:<2} {program:>3}")
                }
                other => other.kind().to_string(),
            };
            println!("{at:8.3}  {line}");
        })
        .map_err(|error| RuntimeError::Message(format!("MIDI: {error}")))?
    };

    notice(
        serde_json::json!({ "midi_monitor": { "status": "listening", "port": listener.port_name() } }),
        || {
            if learn {
                format!(
                    "listening on \"{}\" - turn the knob you want to map (Ctrl-C to give up)",
                    listener.port_name()
                )
            } else {
                format!(
                    "listening on \"{}\" - play or turn something. Ctrl-C for the summary.",
                    listener.port_name()
                )
            }
        },
    );

    // Ctrl-C is recorded rather than killing us, so the summary still prints -
    // which is the whole point of the command.
    install_signal_handlers();
    // `duration` already passed `parse_positive_seconds`, so this cannot see
    // a negative, non-finite, or unrepresentable value.
    let deadline = duration.map(|secs| started + Duration::from_secs_f64(secs));
    while !done.load(Ordering::Relaxed)
        && INTERRUPTED.load(Ordering::SeqCst) == 0
        && deadline.is_none_or(|deadline| Instant::now() < deadline)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(listener);

    if learn {
        return match *learned.lock().unwrap_or_else(|e| e.into_inner()) {
            Some((channel, controller, low, high)) => {
                println!();
                println!("  cc{controller} on channel {channel}  (moved {low}..{high})");
                println!();
                println!("  const cc = await midin('{}')", port.unwrap_or("0"));
                println!("  $: note(\"c3 e3 g3\").lpf(cc({controller}).range(200, 4000))");
                println!();
                Ok(())
            }
            None => Err(RuntimeError::Audio(
                "no control moved; nothing to learn".into(),
            )),
        };
    }

    let summary = summary.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if json {
        println!(
            "{}",
            midi_summary_json(&summary, started.elapsed().as_secs_f64())
        );
        return Ok(());
    }
    println!();
    println!(
        "  {} message(s) in {:.1}s",
        summary.total,
        started.elapsed().as_secs_f64()
    );
    if summary.total == 0 {
        println!("  nothing arrived - is the device sending, and is this the right port?");
        return Ok(());
    }
    println!(
        "  channels: {}",
        summary
            .channels
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  kinds:    {}",
        summary
            .by_kind
            .iter()
            .map(|(k, n)| format!("{k} x{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !summary.controls.is_empty() {
        println!();
        println!("  controls seen (this is your mapping):");
        for (channel, controller, low, high, count) in &summary.controls {
            println!(
                "    ch{channel:<2} cc{controller:<3}  range {low:>3}..{high:<3}  x{count}   ->  cc({controller})"
            );
        }
    }
    if !summary.notes.is_empty() {
        let lowest = summary.notes.first().map(|(n, _)| *n).unwrap_or(0);
        let highest = summary.notes.last().map(|(n, _)| *n).unwrap_or(0);
        println!();
        println!(
            "  notes seen: {} distinct, {} ({lowest}) .. {} ({highest})",
            summary.notes.len(),
            note_name(lowest),
            note_name(highest),
        );
    }
    println!();
    Ok(())
}

#[cfg(feature = "midi")]
pub(super) fn midi_summary_json(
    summary: &rustel_midi::input::InputSummary,
    seconds: f64,
) -> serde_json::Value {
    serde_json::json!({ "midi_summary": {
        "total": summary.total, "seconds": seconds,
        "channels": summary.channels, "by_kind": summary.by_kind,
        "controls": summary.controls, "notes": summary.notes,
    } })
}

#[cfg(not(feature = "midi"))]
pub(super) fn run_midi_monitor(
    _port: Option<&str>,
    _duration: Option<f64>,
    _json: bool,
    _quiet: bool,
    _learn: bool,
    _timing: bool,
) -> Result<(), RuntimeError> {
    Err(RuntimeError::Audio(
        "This option requires the 'midi' feature to be enabled at compile time.".into(),
    ))
}

/// A CC value as a little bar, so a knob sweep reads at a glance.
#[cfg(feature = "midi")]
fn bar(value: u8) -> String {
    let filled = usize::from(value) * 24 / 127;
    format!("{}{}", "#".repeat(filled), "-".repeat(24 - filled))
}

/// Report one immutable capability snapshot, with live audio facts when the
/// default output can be opened. Failure to open hardware is data rather than
/// a failed diagnostic command.
pub(super) fn run_doctor(
    json: bool,
    dispatch: rustel_audio::DspDispatch,
) -> Result<(), RuntimeError> {
    #[cfg(feature = "device-audio")]
    let report = match rustel_audio::LiveScalarDevice::start_output_with_options(
        None,
        0,
        rustel_audio::LiveOutputOptions::default().with_dispatch(dispatch),
    ) {
        Ok(mut device) => {
            let allocator_tripwire_armed = device.arm_callback_tripwire();
            let report = doctor_device_report(&device, allocator_tripwire_armed);
            device.stop();
            report
        }
        Err(error) => doctor_unavailable_report(dispatch, &error.to_string()),
    };
    #[cfg(not(feature = "device-audio"))]
    let report =
        doctor_unavailable_report(dispatch, "device audio is not compiled into this binary");

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(json_err)?
        );
    } else {
        for line in report.human_lines() {
            println!("{line}");
        }
    }
    Ok(())
}

pub(super) fn doctor_unavailable_report(
    dispatch: rustel_audio::DspDispatch,
    reason: &str,
) -> CapabilityReportV1 {
    let registry = rustel_runtime::capability_registry_for_dispatch(dispatch);
    CapabilityReportV1::capture(
        CapabilityReportContext::new(&registry)
            .with_build_features(BUILD_FEATURES)
            .with_audio_unavailable(reason),
    )
}

#[cfg(feature = "device-audio")]
pub(super) fn doctor_device_report(
    device: &rustel_audio::LiveScalarDevice,
    allocator_tripwire_armed: bool,
) -> CapabilityReportV1 {
    let registry = rustel_runtime::capability_registry_for_dispatch(device.dispatch());
    let audio = device.audio_facts();
    let safety = CapabilitySafetyFacts::with_tripwire(Some(allocator_tripwire_armed))
        .with_live_report(&device.report(), None);
    CapabilityReportV1::capture(
        CapabilityReportContext::new(&registry)
            .with_build_features(BUILD_FEATURES)
            .with_audio(&audio)
            .with_safety(safety),
    )
}

/// Report gamepad input until the duration expires or shutdown is requested.
#[cfg(feature = "gamepad")]
pub(super) fn run_gamepad_monitor(duration: Option<f64>) -> Result<(), RuntimeError> {
    // Through the same bound as every other window: `from_secs_f64` panics on
    // a finite value too large to hold, and user input must never crash us.
    let seconds = match duration {
        Some(secs) => parse_positive_seconds("duration", secs)?,
        None => 30.0,
    };
    rustel_runtime::gamepad::monitor(
        std::time::Duration::from_secs_f64(seconds),
        &|| interrupted_by().is_some(),
        |line| {
            println!("{line}");
        },
    )
    .map_err(|message| RuntimeError::Audio(format!("gamepads: {message}")))
}

#[cfg(not(feature = "gamepad"))]
pub(super) fn run_gamepad_monitor(_duration: Option<f64>) -> Result<(), RuntimeError> {
    Err(RuntimeError::Audio(
        "gamepads are not compiled; rebuild rustel with --features gamepad".into(),
    ))
}

/// List MIDI and audio devices, reporting each section's errors independently.
pub(super) fn run_devices(json: bool) -> Result<(), RuntimeError> {
    #[cfg(feature = "midi")]
    let (midi_inputs, midi_outputs) = (
        rustel_midi::MidiSender::input_ports(),
        rustel_midi::MidiSender::ports(),
    );
    #[cfg(not(feature = "midi"))]
    let (midi_inputs, midi_outputs): (
        Result<Vec<String>, String>,
        Result<Vec<String>, String>,
    ) = (
        Err("MIDI is not compiled; rebuild with --features midi".into()),
        Err("MIDI is not compiled; rebuild with --features midi".into()),
    );

    #[cfg(feature = "device-audio")]
    let audio = rustel_audio::device::audio_devices().map_err(|error| error.to_string());

    if json {
        let named = |ports: &Result<Vec<String>, String>| match ports {
            Ok(names) => serde_json::json!(
                names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| serde_json::json!({ "index": index, "name": name }))
                    .collect::<Vec<_>>()
            ),
            Err(message) => serde_json::json!({ "error": message }),
        };
        #[cfg(feature = "device-audio")]
        let audio_json = match &audio {
            Ok((outputs, inputs)) => {
                let describe = |list: &[rustel_audio::device::AudioDeviceInfo]| {
                    list.iter()
                        .map(|device| {
                            serde_json::json!({
                                "id": device.id,
                                "name": device.name,
                                "default": device.is_default,
                                "sample_rate": device.sample_rate,
                                "channels": device.channels,
                            })
                        })
                        .collect::<Vec<_>>()
                };
                serde_json::json!({ "outputs": describe(outputs), "inputs": describe(inputs) })
            }
            Err(message) => serde_json::json!({ "error": message }),
        };
        #[cfg(not(feature = "device-audio"))]
        let audio_json = serde_json::json!({
            "error": "audio devices are not compiled; rebuild with --features device-audio"
        });
        #[cfg(feature = "gamepad")]
        let gamepads_json = match rustel_runtime::gamepad::list() {
            Ok(pads) => serde_json::json!(
                pads.iter()
                    .map(|(index, name)| serde_json::json!({ "index": index, "name": name }))
                    .collect::<Vec<_>>()
            ),
            Err(message) => serde_json::json!({ "error": message }),
        };
        #[cfg(not(feature = "gamepad"))]
        let gamepads_json = serde_json::json!({
            "error": "gamepads are not compiled; rebuild with --features gamepad"
        });
        println!(
            "{}",
            serde_json::json!({
                "midi_inputs": named(&midi_inputs),
                "midi_outputs": named(&midi_outputs),
                "audio": audio_json,
                "gamepads": gamepads_json,
                "hint": "`.midi()` takes an OUTPUT name (one distinctive word) or its index; `gamepad(n)` takes the pad's index",
            })
        );
        return Ok(());
    }

    let on = style::stdout_on();
    let ports_section = |title: &str, ports: &Result<Vec<String>, String>| {
        println!("{}", style::bold(on, title));
        match ports {
            Ok(names) if names.is_empty() => println!("  {}", style::dim(on, "(none)")),
            Ok(names) => {
                for (index, name) in names.iter().enumerate() {
                    println!("  {} {name}", style::cyan(on, &index.to_string()));
                }
            }
            Err(message) => println!("  {}", style::yellow(on, message)),
        }
    };
    ports_section("MIDI inputs", &midi_inputs);
    ports_section("MIDI outputs", &midi_outputs);
    println!("{}", style::bold(on, "Audio"));
    #[cfg(feature = "device-audio")]
    match &audio {
        Ok((outputs, inputs)) => {
            for (label, list) in [("output", outputs), ("input", inputs)] {
                if list.is_empty() {
                    println!("  {label}s: {}", style::dim(on, "(none)"));
                }
                for device in list {
                    let default = if device.is_default {
                        style::green(on, " (default)")
                    } else {
                        String::new()
                    };
                    let rate = device
                        .sample_rate
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "?".into());
                    let channels = device
                        .channels
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "?".into());
                    println!("  {label}: {}{default} {rate}Hz {channels}ch", device.name);
                }
            }
        }
        Err(message) => println!("  {}", style::yellow(on, message)),
    }
    #[cfg(not(feature = "device-audio"))]
    println!(
        "  {}",
        style::yellow(
            on,
            "audio devices are not compiled; rebuild with --features device-audio"
        )
    );
    println!(
        "{}",
        style::dim(
            on,
            "hint: `.midi()` takes an OUTPUT name (one distinctive word) or its index"
        )
    );
    println!("{}", style::bold(on, "Gamepads"));
    #[cfg(feature = "gamepad")]
    match rustel_runtime::gamepad::list() {
        Ok(pads) if pads.is_empty() => println!("  {}", style::dim(on, "(none)")),
        Ok(pads) => {
            for (index, name) in &pads {
                println!("  {} {name}", style::cyan(on, &index.to_string()));
            }
            println!(
                "  {}",
                style::dim(
                    on,
                    "a score reads them as gamepad(0), gamepad(1)…, in this order"
                )
            );
        }
        Err(message) => println!("  {}", style::yellow(on, &message)),
    }
    #[cfg(not(feature = "gamepad"))]
    println!(
        "  {}",
        style::yellow(on, "not compiled; rebuild with --features gamepad")
    );
    Ok(())
}

/// Print the MIDI ports: outputs first, then inputs, each in the order a
/// numeric selector indexes them - `midiport` on the output side, `midin`
/// and `midikeys` on the input side.
///
/// An empty list is a normal answer, not an error: a machine with no MIDI
/// hardware and no loopback driver genuinely has none. It says so and exits
/// zero, so a script can tell "none plugged in" from "MIDI is not built in".
/// Human-readable by default; `--json` emits the raw lists for scripting.
#[cfg(feature = "midi")]
pub(super) fn run_midi_list(json: bool) -> Result<(), RuntimeError> {
    let outputs = rustel_midi::MidiSender::ports().map_err(RuntimeError::Audio)?;
    let inputs = rustel_midi::MidiSender::input_ports().map_err(RuntimeError::Audio)?;
    let named = |ports: &[String]| {
        ports
            .iter()
            .enumerate()
            .map(|(index, name)| serde_json::json!({ "index": index, "name": name }))
            .collect::<Vec<_>>()
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "midi_outputs": named(&outputs),
                "midi_inputs": named(&inputs),
                "hint": if outputs.is_empty() {
                    product::MIDI_NO_OUTPUTS_HINT
                } else {
                    "use a name or an index: .midi('IAC Driver Bus 1') or .midi(0)"
                },
                "inputs_hint": if inputs.is_empty() {
                    product::MIDI_NO_INPUTS_HINT
                } else {
                    "use a name or an index: midin('MiniLab') or midin(0)"
                },
            })
        );
    } else {
        let on = style::stdout_on();
        println!("{}", style::bold(on, "MIDI outputs"));
        if outputs.is_empty() {
            println!("  {}", style::dim(on, product::MIDI_NO_OUTPUTS_HINT));
        } else {
            for (index, name) in outputs.iter().enumerate() {
                println!("  {} {name}", style::cyan(on, &index.to_string()));
            }
            println!(
                "{}",
                style::dim(
                    on,
                    "hint: use a name or an index: .midi('IAC Driver Bus 1') or .midi(0)"
                )
            );
        }
        println!("{}", style::bold(on, "MIDI inputs"));
        if inputs.is_empty() {
            println!("  {}", style::dim(on, product::MIDI_NO_INPUTS_HINT));
        } else {
            for (index, name) in inputs.iter().enumerate() {
                println!("  {} {name}", style::cyan(on, &index.to_string()));
            }
            println!(
                "{}",
                style::dim(
                    on,
                    "hint: a score listens with midin('MiniLab') or midin(0); \
                     midikeys('MiniLab') turns keys into notes"
                )
            );
        }
    }
    Ok(())
}

#[cfg(not(feature = "midi"))]
pub(super) fn run_midi_list(_json: bool) -> Result<(), RuntimeError> {
    Err(RuntimeError::Audio(
        "This option requires the 'midi' feature to be enabled at compile time.".into(),
    ))
}

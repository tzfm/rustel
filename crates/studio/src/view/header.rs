use super::*;

/// Where the header drew its warning badge this frame, packed x/y/width, so
/// the pointer can ask without replicating the header's flowing layout.
/// Zero means no badge is on screen.
static WARNING_BADGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn set_warning_badge(rect: Option<(u16, u16, u16)>) {
    let packed = rect.map_or(0, |(x, y, width)| {
        (u64::from(x) << 32) | (u64::from(y) << 16) | u64::from(width)
    });
    WARNING_BADGE.store(packed, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a pointer position is on the header's warning badge - the icon
/// and its count, not the bar around them.
pub fn warning_badge_at(x: u16, y: u16) -> bool {
    let packed = WARNING_BADGE.load(std::sync::atomic::Ordering::Relaxed);
    if packed == 0 {
        return false;
    }
    let badge_x = (packed >> 32) as u16;
    let badge_y = (packed >> 16) as u16;
    let width = packed as u16;
    y == badge_y && x >= badge_x && x < badge_x.saturating_add(width)
}

/// Where the header draws its readiness chip - `✓ ready`, `✗ 5 problems`.
///
/// Computed the way the header renders it, so a click lands on the words a
/// reader is looking at. Empty when the chip has nothing to say.
pub fn header_go_chip(area: Rect, word: TransportWord, go: GoState) -> Rect {
    let name_width = UnicodeWidthStr::width(format!(" ● {} ", product::NAME).as_str())
        .min(usize::from(area.width)) as u16;
    let state_x = area.x.saturating_add(name_width);
    let chip_x =
        state_x.saturating_add(UnicodeWidthStr::width(format!("{} ", word.text()).as_str()) as u16);
    let width = UnicodeWidthStr::width(go.text().as_str()) as u16;
    Rect::new(
        chip_x,
        area.y,
        width.min(area.right().saturating_sub(chip_x)),
        u16::from(!area.is_empty()),
    )
}

/// The colour the loading line's underline takes: the accent, moved until
/// it reads against the header.
pub fn loading_line_colour(theme: &Theme) -> Color {
    crate::theme::legible_against_floor(theme.accent, theme.surface, 3.0)
}

/// The countdown chip fitted to `room` cells. A plain guillemet, not a
/// clock emoji: the studio is a terminal, and a colour glyph in the header
/// reads as a sticker on it. The number is the point of the chip, so a
/// long scene name gives way before the number does.
pub(crate) fn launch_chip(scene: &str, cycles: f64, room: usize) -> String {
    let tail = format!(" in {cycles:.1}");
    let full = format!("» {scene}{tail}");
    if UnicodeWidthStr::width(full.as_str()) <= room {
        return full;
    }
    let name_room = room.saturating_sub(UnicodeWidthStr::width(tail.as_str()) + 3);
    if name_room >= 2 {
        let short: String = scene.chars().take(name_room).collect();
        return format!("» {short}…{tail}");
    }
    format!("»{tail}")
}

pub(super) fn render_launch_corner(
    buffer: &mut Buffer,
    area: Rect,
    scene: &str,
    cycles: f64,
    theme: &Theme,
) {
    if area.height == 0 || area.width < 12 {
        return;
    }
    let text = format!(
        " {} ",
        super::launch_chip(scene, cycles, usize::from(area.width.saturating_sub(4)))
    );
    let width = UnicodeWidthStr::width(text.as_str()) as u16;
    let x = area.right().saturating_sub(width);
    buffer.set_stringn(
        x,
        area.y,
        &text,
        usize::from(width),
        Style::default()
            .fg(theme.warn)
            .bg(theme.background)
            .add_modifier(Modifier::BOLD),
    );
}

pub(super) struct Header<'a, 'b> {
    pub(super) chrome: &'a StudioChrome<'b>,
}

impl Widget for Header<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.chrome.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.foreground),
        );
        let word = TransportWord {
            stopping: self.chrome.stopping,
            playing: self.chrome.playing,
        };
        let state = word.text();
        let state_color = if self.chrome.stopping {
            theme.warn
        } else if self.chrome.playing {
            theme.ok
        } else {
            theme.muted
        };
        // The dot beats: lit on every beat, most on the downbeat, fading
        // between - the studio's own pulse, or the clock it follows.
        // While a job runs it breathes amber on wall-clock time instead:
        // too brief to read as a word, and the transport keeps doing
        // what the word says.
        let (dot, dot_color) = match self.chrome.evaluating_for {
            Some(elapsed) => busy_pulse(elapsed, theme),
            None => beat_pulse(self.chrome.playing, self.chrome.cycle, theme),
        };
        let name = format!(" {dot} {} ", product::NAME);
        let name_width = UnicodeWidthStr::width(name.as_str()).min(usize::from(area.width)) as u16;
        buffer.set_stringn(
            area.x,
            area.y,
            &name,
            usize::from(area.width),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        buffer.set_stringn(
            area.x + 1,
            area.y,
            dot,
            1,
            Style::default().fg(dot_color).add_modifier(Modifier::BOLD),
        );
        let state_x = area.x.saturating_add(name_width);
        buffer.set_stringn(
            state_x,
            area.y,
            format!("{state} "),
            usize::from(area.right().saturating_sub(state_x)),
            Style::default()
                .fg(state_color)
                .add_modifier(Modifier::BOLD),
        );

        // Ready-to-go sits right after the transport word: the one thing to
        // glance at before pressing update.
        let go = self.chrome.go.text();
        // The same reckoning the pointer uses, so a click lands on the
        // words rather than beside them.
        let chip = header_go_chip(area, word, self.chrome.go);
        let go_x = chip.x;
        let go_width = UnicodeWidthStr::width(go.as_str()) as u16;
        if go_width > 0 {
            buffer.set_stringn(
                go_x,
                area.y,
                &go,
                usize::from(area.right().saturating_sub(go_x)),
                Style::default()
                    .fg(self.chrome.go.color(theme))
                    .add_modifier(Modifier::BOLD),
            );
        }
        // A take in progress is the one thing a performer must never lose
        // track of: red, next to the state, with its clock.
        let mut after_go = go_x.saturating_add(go_width + u16::from(go_width > 0) * 3);
        #[cfg(feature = "hydra")]
        if let Some(state) = self.chrome.webcam {
            let (text, colour) = match state {
                rustel_runtime::hydra::HydraWebcamState::Blocked => ("CAM OFF", theme.warn),
                rustel_runtime::hydra::HydraWebcamState::Allowed => ("CAM", theme.muted),
                rustel_runtime::hydra::HydraWebcamState::Requested => ("CAM WAIT", theme.warn),
                rustel_runtime::hydra::HydraWebcamState::Opening => ("CAM OPEN", theme.warn),
                rustel_runtime::hydra::HydraWebcamState::Ready => ("CAM ●", theme.ok),
                rustel_runtime::hydra::HydraWebcamState::Error => ("CAM !", theme.error),
            };
            buffer.set_stringn(
                after_go,
                area.y,
                text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text) as u16 + 3);
        }
        if let Some(recording) = self.chrome.recording {
            let text = recording.text();
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        } else if let Some(notice) = self.chrome.take_notice {
            let text = format!("○ {notice}");
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.muted),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        }
        if let Some((bpm, locked)) = self.chrome.clock_in {
            let text = format!("⇄ {bpm:.1}");
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default()
                    .fg(if locked { theme.ok } else { theme.warn })
                    .add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        } else if self.chrome.clock_out {
            buffer.set_stringn(
                after_go,
                area.y,
                "⇄ out",
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.muted),
            );
            after_go = after_go.saturating_add(5 + 3);
        }
        if let Some((scene, cycles)) = self.chrome.launch
            && !self.chrome.stopping
        {
            let room = usize::from(area.right().saturating_sub(after_go));
            let text = launch_chip(scene, cycles, room);
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        }
        if let Some(glance) = self.chrome.exporting {
            // Progress and a scope, as a render in a DAW shows: how far, and
            // whether there is still anything to hear.
            let total = glance.seconds.max(0.0) as u64;
            let how_far = match glance.percent {
                Some(percent) => format!("{percent}%"),
                None => format!("{}:{:02}", total / 60, total % 60),
            };
            let scope = glance
                .peaks
                .iter()
                .map(|peak| {
                    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
                    let level = peak.clamp(0.0, 1.0).sqrt();
                    BARS[((level * 7.0).round() as usize).min(7)]
                })
                .collect::<String>();
            let text = format!("⇣ {} {how_far} {scope}", glance.scene_name);
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.muted),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16 + 3);
        }
        if let Some(jobs) = self.chrome.jobs {
            let width = UnicodeWidthStr::width(jobs) as u16;
            crate::jobs::set_jobs_chip(Some((after_go, area.y, width)));
            buffer.set_stringn(
                after_go,
                area.y,
                jobs,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.accent),
            );
            after_go = after_go.saturating_add(width + 3);
        } else {
            crate::jobs::set_jobs_chip(None);
        }
        // Something went on under the scene: a small count of what still
        // stands - a condition whose alert has not resolved, or keyless
        // news inside its stay. Opening the log takes the count down, and
        // what nothing resolved lets go of it on its own. Small on purpose,
        // and a button - clicking it opens the log.
        if self.chrome.unseen_warnings > 0 {
            let text = format!("⚠ {}", self.chrome.unseen_warnings);
            let width = UnicodeWidthStr::width(text.as_str()) as u16;
            set_warning_badge(Some((after_go, area.y, width)));
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(area.right().saturating_sub(after_go)),
                Style::default().fg(theme.warn),
            );
            after_go = after_go.saturating_add(width + 3);
        } else {
            set_warning_badge(None);
        }
        // The right-hand block names the scene; the loading label stops
        // short of its short name.
        let dirty = if self.chrome.dirty {
            format!(" {}", crate::terminal::symbol("●"))
        } else {
            String::new()
        };
        // A prebake tab has no score file, so the header says what it is
        // rather than naming a path that is not the music.
        let short = match self.chrome.prebake {
            Some(scope) => format!("{PREBAKE_GLYPH} {}{dirty}", scope.tab_name()),
            None if self.chrome.replay => format!(
                "{REPLAY_GLYPH} {}{dirty}",
                crate::scenes::replay_name(self.chrome.path)
            ),
            None => format!("{}{dirty}", crate::scenes::score_name(self.chrome.path)),
        };
        // A load that has lasted says what it waits for where the tempo
        // sits, as one more chip in the row.
        if let Some(label) = self
            .chrome
            .loading
            .as_ref()
            .and_then(|line| line.label.as_deref())
        {
            let short_width = UnicodeWidthStr::width(short.as_str()) as u16;
            let room = area
                .right()
                .saturating_sub(after_go)
                .saturating_sub(short_width + 3);
            let text = ellipsize(label, room);
            buffer.set_stringn(
                after_go,
                area.y,
                &text,
                usize::from(room),
                Style::default().fg(theme.accent),
            );
            after_go = after_go.saturating_add(UnicodeWidthStr::width(text.as_str()) as u16);
        } else if let Some(cps) = self.chrome.cps {
            // Tempo sits next to that, where a musician looks for it,
            // rather than among the process counters.
            let tempo = format!(
                "{:.1} bpm · {cps:.2} cps{}",
                beats_per_minute(cps),
                self.chrome
                    .cycle
                    // The clock can sit a hair before zero while stopped
                    // (the onset lead); a negative cycle is a nonsense to
                    // read, so the header floors it.
                    .map(|cycle| format!(" · cycle {:.2}", cycle.max(0.0)))
                    .unwrap_or_default()
            );
            let tempo_x = after_go;
            let room = usize::from(area.right().saturating_sub(tempo_x)) / 2;
            buffer.set_stringn(
                tempo_x,
                area.y,
                &tempo,
                room,
                Style::default().fg(theme.muted),
            );
            after_go =
                tempo_x.saturating_add(UnicodeWidthStr::width(tempo.as_str()).min(room) as u16);
        }

        // The right-hand block - counters, then the scene - takes what the
        // transport left, never a cell of it. The scene is named the way
        // the strip names it, under its set: `opening night ▸ live`. The
        // path is not here - a set's folder is nested deep and its name
        // says everything the row has room to say.
        let left_end = after_go.saturating_add(2);
        let underrun = self
            .chrome
            .pressure
            .filter(|pressure| pressure.cause == EnginePressureCause::DeviceUnderrun);
        let warning_room = underrun.map_or(0, |pressure| {
            UnicodeWidthStr::width(pressure.cause.label()) + 3
        });
        let room = usize::from(area.right().saturating_sub(left_end + 1));
        let room = room.checked_sub(warning_room).unwrap_or(room);
        // The set's name goes before the scene while there is room for
        // both; the scene wins when there is not.
        let set = self.chrome.set_name;
        let candidates = [
            if set.is_empty() {
                short.clone()
            } else {
                format!("{set} \u{25b8} {short}")
            },
            short.clone(),
        ];
        let mut file = candidates
            .iter()
            .find(|candidate| UnicodeWidthStr::width(candidate.as_str()) <= room)
            .cloned()
            .unwrap_or(short);
        while UnicodeWidthStr::width(file.as_str()) > room && !file.is_empty() {
            file.remove(0);
        }
        let file_width = UnicodeWidthStr::width(file.as_str()) as u16;
        let file_x = area.right().saturating_sub(file_width + 1);
        buffer.set_stringn(
            file_x,
            area.y,
            &file,
            usize::from(file_width),
            Style::default().fg(if self.chrome.dirty {
                theme.warn
            } else if self.chrome.prebake.is_some() {
                theme.mini
            } else if self.chrome.replay {
                theme.replay_colour()
            } else {
                theme.muted
            }),
        );

        // Detailed engine pressure has priority over process-wide counters: this is
        // the deadline information that says whether playback can keep up.
        // It only appears at Full - the deadline detail a tuning session
        // wants, not a glance mid-set.
        let detail = self.chrome.metric_detail;
        let mut counters_right = file_x;
        // A recent backend underrun stays visible when metrics are hidden.
        if let Some(pressure) = underrun {
            let warning = pressure.cause.label();
            let warning_width = UnicodeWidthStr::width(warning) as u16;
            if counters_right >= left_end + warning_width + 3 {
                let warning_x = counters_right.saturating_sub(warning_width + 3);
                buffer.set_stringn(
                    warning_x,
                    area.y,
                    warning,
                    usize::from(warning_width),
                    Style::default().fg(engine_pressure_color(pressure.level, theme)),
                );
                counters_right = warning_x;
            }
        }
        if detail.shows_pressure()
            && let Some(pressure) = self.chrome.pressure
        {
            let engine = engine_pressure_text(pressure);
            let engine_width = UnicodeWidthStr::width(engine.as_str()) as u16;
            if area.width >= 100 && counters_right > left_end + engine_width + 3 {
                let engine_x = counters_right.saturating_sub(engine_width + 3);
                buffer.set_stringn(
                    engine_x,
                    area.y,
                    engine,
                    usize::from(engine_width),
                    Style::default().fg(engine_pressure_color(pressure.level, theme)),
                );
                counters_right = engine_x;
            }
        }

        // Process counters remain useful secondary context when there is
        // room, and are skipped entirely at None - a performance screen
        // that would rather say nothing about the machine than something.
        // The whole run is the memory breakdown's button: cleared first,
        // so a frame that draws no counters leaves no button behind them.
        crate::memory::set_memory_chip(None);
        if detail.shows_process() {
            let stats = stats_text(self.chrome, detail);
            let stats_width = UnicodeWidthStr::width(stats.as_str()) as u16;
            if area.width >= 100 && counters_right > left_end + stats_width + 3 {
                let stats_x = counters_right.saturating_sub(stats_width + 3);
                crate::memory::set_memory_chip(Some((stats_x, area.y, stats_width)));
                buffer.set_stringn(
                    stats_x,
                    area.y,
                    stats,
                    usize::from(stats_width),
                    Style::default().fg(theme.muted),
                );
            }
        }
        if let Some(line) = &self.chrome.loading {
            draw_loading_line(buffer, area, line, theme);
        }
    }
}

/// The loading line over the header row as drawn: an underline in the
/// accent from the left edge to `line.reach` (two cells at least), or a
/// faint accent tint where the terminal cannot colour an underline. The
/// text and its colours stay.
fn draw_loading_line(buffer: &mut Buffer, area: Rect, line: &LoadingLine, theme: &Theme) {
    // A stub from the start, so a load of one file shows before it lands.
    let reach = ((f64::from(area.width) * line.reach.clamp(0.0, 1.0)).round() as u16).max(2);
    let underline = loading_line_colour(theme);
    let tint = crate::theme::mix(theme.surface, theme.accent, 0.2);
    for x in area.x..area.x.saturating_add(reach.min(area.width)) {
        let Some(cell) = buffer.cell_mut((x, area.y)) else {
            continue;
        };
        if line.underline {
            cell.modifier.insert(Modifier::UNDERLINED);
            cell.underline_color = underline;
        } else if tint != theme.surface {
            cell.bg = tint;
        } else {
            // A palette theme has no tint to blend: a plain underline.
            cell.modifier.insert(Modifier::UNDERLINED);
        }
    }
}

/// One orbit on the footer: its number, a four-cell meter, its output
/// pair - `3 ▮▮▮▯ 5/6`. The pair is the click target when there is more
/// than one.
pub(super) fn render_orbit_chip(
    buffer: &mut Buffer,
    rect: Rect,
    level: &OrbitLevel,
    output_pairs: u16,
    theme: &Theme,
) {
    if rect.is_empty() {
        return;
    }
    let db = if level.peak > 0.0 {
        20.0 * level.peak.log10()
    } else {
        -120.0
    };
    let lit = if db >= -3.0 {
        4
    } else if db >= -12.0 {
        3
    } else if db >= -24.0 {
        2
    } else if db >= -48.0 {
        1
    } else {
        0
    };
    let color = if db >= -1.0 {
        theme.meter.peak
    } else if db >= -6.0 {
        theme.meter.high
    } else if db >= -18.0 {
        theme.meter.mid
    } else {
        theme.meter.low
    };
    buffer.set_stringn(
        rect.x,
        rect.y,
        format!("{}", level.orbit),
        2,
        Style::default().fg(theme.accent),
    );
    for cell in 0..4u16 {
        let (symbol, fg) = if cell < lit {
            (crate::terminal::symbol("▮"), color)
        } else {
            ("▯", theme.rule)
        };
        buffer.set_stringn(
            rect.x + 2 + cell,
            rect.y,
            symbol,
            1,
            Style::default().fg(fg),
        );
    }
    let pair = u16::from(level.pair);
    let label = format!("{}/{}", pair * 2 + 1, pair * 2 + 2);
    let style = if output_pairs > 1 {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().fg(theme.muted)
    };
    buffer.set_stringn(
        rect.x + 7,
        rect.y,
        &label,
        usize::from(rect.width.saturating_sub(7)),
        style,
    );
}

/// The beat as a dot: four beats to the cycle, `●` on the beat fading to
/// `◉`, the downbeat in the ok colour and the others in the accent, all of
/// it muted while nothing plays.
pub fn beat_pulse(playing: bool, cycle: Option<f64>, theme: &Theme) -> (&'static str, Color) {
    let Some(cycle) = cycle.filter(|_| playing) else {
        return (crate::terminal::symbol("◉"), theme.muted);
    };
    let beat = cycle * 4.0;
    let phase = beat.rem_euclid(1.0);
    let downbeat = beat.rem_euclid(4.0) < 1.0;
    let brightness = (1.0 - phase).powi(3) as f32;
    let color = if downbeat { theme.ok } else { theme.accent };
    let symbol = if phase < 0.2 { "●" } else { "◉" };
    (
        crate::terminal::symbol(symbol),
        mix(theme.muted, color, brightness),
    )
}

/// The logo dot while a job runs: the beat's own glyphs and fade, amber,
/// on wall-clock time - the transport clock stands still while stopped,
/// so the work is timed by the wall. The word beside it keeps saying
/// what the transport is doing; the dot is what says the studio is busy.
pub fn busy_pulse(elapsed: Duration, theme: &Theme) -> (&'static str, Color) {
    const BREATH_MS: u128 = 200;
    let phase = (elapsed.as_millis() % BREATH_MS) as f64 / BREATH_MS as f64;
    let brightness = (1.0 - phase).powi(3) as f32;
    let symbol = if phase < 0.2 { "●" } else { "◉" };
    (
        crate::terminal::symbol(symbol),
        mix(theme.muted, theme.warn, brightness),
    )
}

fn stats_text(chrome: &StudioChrome<'_>, detail: settings::MetricDetail) -> String {
    let cpu = chrome
        .stats
        .cpu_percent
        .map(format_cpu_percent)
        .unwrap_or_else(|| "-".to_owned());
    let memory = chrome
        .stats
        .memory_bytes()
        .map(format_bytes)
        .unwrap_or_else(|| "-".to_owned());
    // `sys` sits right beside `cpu` rather than after `mem`, so the two
    // numbers a player actually wants to compare - mine against the whole
    // machine's - read next to each other.
    let machine = if detail.shows_machine() {
        let machine = chrome
            .stats
            .machine_cpu_percent
            .map(format_cpu_percent)
            .unwrap_or_else(|| "-".to_owned());
        format!(" · sys {machine}")
    } else {
        String::new()
    };
    format!(
        "cpu {cpu}{machine} · mem {memory} · {:.0}fps {:.1}ms",
        chrome.fps, chrome.render_ms
    )
}

fn engine_pressure_text(pressure: &EnginePressureSnapshot) -> String {
    let percent = |basis_points: u64| basis_points.saturating_add(50) / 100;
    format!(
        "DSP {}%  sched {}%  voices {}/{}  cover {}ms",
        percent(pressure.dsp_load_basis_points()),
        percent(pressure.scheduler_load_basis_points()),
        pressure.active_voices(),
        pressure.semantic_voice_capacity(),
        pressure.cover_millis(),
    )
}

fn engine_pressure_color(level: EnginePressureLevel, theme: &Theme) -> Color {
    match level {
        EnginePressureLevel::Normal => theme.ok,
        EnginePressureLevel::Caution => theme.accent,
        EnginePressureLevel::Warning => theme.warn,
        EnginePressureLevel::Error => theme.error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::tests::*;

    /// The header names a prebake tab rather than passing off the file it
    /// is kept in as the music.
    #[test]
    fn the_header_names_a_prebake_tab() {
        let theme = theme();
        let path = std::path::Path::new("/x/config/prebake.strudel");
        let area = Rect::new(0, 0, 120, 1);

        let master = MasterState::new(Instant::now());
        let mut buffer = Buffer::empty(area);
        let mut tab = chrome(&theme, &master, path);
        tab.prebake = Some(PrebakeScope::Global);
        Header { chrome: &tab }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("⚙ prebake (global)"), "{text}");
        assert!(
            !text.contains("/x/config"),
            "not the file it is kept in: {text}"
        );

        // A score reads as its own name.
        let mut buffer = Buffer::empty(area);
        let score = chrome(&theme, &master, std::path::Path::new("/x/song.strudel"));
        Header { chrome: &score }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("song"), "{text}");
        assert!(!text.contains("prebake") && !text.contains("/x/"), "{text}");
    }

    /// The header names the scene under its set, the way the strip names
    /// it and never as a path; when the row runs short the set goes and
    /// the scene's name is what remains.
    #[test]
    fn the_header_names_the_scene_under_its_set() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(
            &theme,
            &master,
            Path::new("/Users/EXAMPLE/.rustel/sets/opening night/live.strudel"),
        );
        chrome.set_name = "opening night";
        let render = |width: u16| {
            let area = Rect::new(0, 0, width, 1);
            let mut buffer = Buffer::empty(area);
            Header { chrome: &chrome }.render(area, &mut buffer);
            text_of(&buffer)
        };
        let wide = render(200);
        assert!(wide.contains("opening night \u{25b8} live"), "{wide}");
        assert!(
            !wide.contains(".rustel") && !wide.contains(".strudel"),
            "no path, no extension: {wide}"
        );
        let mut width = 200;
        while render(width).contains("opening night \u{25b8}") {
            width -= 1;
        }
        let shortest = render(width);
        assert!(
            shortest.contains("live"),
            "the scene's name is what remains: {shortest}"
        );
    }

    #[test]
    fn the_header_says_whether_the_scene_is_good_to_go() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 140, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.cps = Some(0.5);
        for (go, expected, color) in [
            (GoState::Ready, "✓ ready", theme.ok),
            (
                GoState::Loading { ready: 3, total: 7 },
                "◐ loading 3/7",
                theme.warn,
            ),
            (GoState::Problems { count: 2 }, "✗ 2 problems", theme.error),
            (
                GoState::Failed { failed: 1 },
                "⚠ 1 sound failed",
                theme.warn,
            ),
        ] {
            chrome.go = go;
            let mut buffer = Buffer::empty(area);
            Header { chrome: &chrome }.render(area, &mut buffer);
            let text = text_of(&buffer);
            assert!(text.contains(expected), "{text}");
            assert!(text.contains("120.0 bpm"), "tempo still follows: {text}");
            let at = text.find(expected).unwrap();
            let cell = text[..at].chars().count() as u16;
            assert_eq!(buffer.cell((cell, 0)).unwrap().fg, color);
        }
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn the_header_keeps_camera_acquisition_visible_outside_settings() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 100, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        for (state, expected) in [
            (rustel_runtime::hydra::HydraWebcamState::Blocked, "CAM OFF"),
            (rustel_runtime::hydra::HydraWebcamState::Opening, "CAM OPEN"),
            (rustel_runtime::hydra::HydraWebcamState::Ready, "CAM ●"),
            (rustel_runtime::hydra::HydraWebcamState::Error, "CAM !"),
        ] {
            chrome.webcam = Some(state);
            let mut buffer = Buffer::empty(area);
            Header { chrome: &chrome }.render(area, &mut buffer);
            assert!(text_of(&buffer).contains(expected), "{}", text_of(&buffer));
        }
    }

    /// The countdown is the one header chip the eye waits on; it is drawn
    /// in the warning colour, keeps its number under any width, and goes
    /// out with a stop.
    #[test]
    fn the_header_counts_a_launch_down_and_keeps_its_number() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.cps = Some(0.5);
        chrome.launch = Some(("intro", 3.2));
        let area = Rect::new(0, 0, 140, 1);
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("» intro in 3.2"), "{text}");
        let at = text.find("» intro").unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.warn);

        chrome.stopping = true;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        assert!(
            !text_of(&buffer).contains("» intro"),
            "a stop ends the countdown"
        );

        assert_eq!(launch_chip("intro", 3.2, 40), "» intro in 3.2");
        assert_eq!(
            launch_chip("ambient-intro-take-two", 3.2, 16),
            "» ambien… in 3.2",
            "the name gives way, the number stays"
        );
        assert_eq!(launch_chip("intro", 0.4, 7), "» in 0.4");
    }

    #[test]
    fn the_header_prioritizes_engine_pressure_with_exact_values_and_severity() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 160, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        let mut pressure = EnginePressureSnapshot {
            level: EnginePressureLevel::Warning,
            cause: EnginePressureCause::DeviceUnderrun,
            ..EnginePressureSnapshot::default()
        };
        pressure.device.realtime_load.slow_load_basis_points = 2_400;
        pressure.device.realtime_pressure.active_voices = 18;
        pressure.producer.slow_load_basis_points = 1_100;
        pressure.producer.cover_end_nanos = 420_000_000;
        chrome.pressure = Some(&pressure);
        // Detailed pressure only draws at Full - the deadline detail a tuning
        // session wants, not what a glance mid-set gets by default.
        chrome.metric_detail = settings::MetricDetail::Full;
        chrome.stats = ProcessStats {
            cpu_percent: Some(99.0),
            machine_cpu_percent: Some(80.0),
            resident_bytes: Some(3 << 30),
            footprint_bytes: None,
        };

        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        let expected = "DSP 24%  sched 11%  voices 18/128  cover 420ms";
        assert!(text.contains(expected), "{text}");
        let at = text.find(expected).unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.warn);
        let warning = "audio device underrun";
        let at = text.find(warning).expect("the recent underrun is visible");
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.warn);

        chrome.metric_detail = settings::MetricDetail::None;
        let long_name = format!("{}.strudel", "scene".repeat(40));
        chrome.path = Path::new(&long_name);
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains(warning), "{text}");
        assert!(!text.contains(expected), "{text}");

        chrome.path = Path::new("live.strudel");
        let narrow = Rect::new(0, 0, 32, 1);
        let mut buffer = Buffer::empty(narrow);
        Header { chrome: &chrome }.render(narrow, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("live"), "{text}");
        assert!(!text.contains(warning), "{text}");
    }

    #[test]
    fn the_header_shows_a_take_in_red_with_its_clock_and_size() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let area = Rect::new(0, 0, 160, 1);
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.cps = Some(0.5);
        chrome.go = GoState::Ready;
        chrome.recording = Some(RecordingChip {
            seconds: 754.0,
            bytes: 140 << 20,
            dropped_seconds: 0.0,
            sample: false,
        });
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("✓ ready"), "{text}");
        assert!(text.contains("● REC 12:34 · 140 MB"), "{text}");
        assert!(text.contains("120.0 bpm"), "{text}");
        let at = text.find("● REC").unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.error);
        assert_eq!(
            RecordingChip {
                seconds: 3725.0,
                bytes: 3 << 30,
                dropped_seconds: 0.4,
                sample: false,
            }
            .text(),
            "● REC 1:02:05 · 3.0 GB · dropped 0.4s"
        );
        assert_eq!(
            RecordingChip {
                seconds: 4.0,
                bytes: 0,
                dropped_seconds: 0.0,
                sample: true,
            }
            .text(),
            "● REC SAMPLE 0:04",
            "a sample says so, in the same red"
        );
        // A take that ended by itself is said in the same place, muted -
        // never in the error colour on a screen an audience may see.
        chrome.recording = None;
        chrome.take_notice = Some("take ended · disk full");
        chrome.unseen_warnings = 3;
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("○ take ended · disk full"), "{text}");
        assert!(text.contains("⚠ 3"), "{text}");
        chrome.unseen_warnings = 0;
        let mut peaks = [0.0f32; crate::export::SCOPE_COLUMNS];
        peaks[15] = 1.0;
        peaks[14] = 0.25;
        let glance = ExportGlance {
            scene_name: "drums".into(),
            seconds: 42.0,
            percent: Some(42),
            peaks,
        };
        chrome.exporting = Some(&glance);
        let mut buffer = Buffer::empty(area);
        Header { chrome: &chrome }.render(area, &mut buffer);
        let text = text_of(&buffer);
        assert!(text.contains("⇣ drums 42% ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▅█"), "{text}");
        let at = text.find("○ take").unwrap();
        let cell = text[..at].chars().count() as u16;
        assert_eq!(buffer.cell((cell, 0)).unwrap().fg, theme.muted);
    }

    #[test]
    fn the_dot_beats_with_the_cycle() {
        let theme = theme();
        assert_eq!(beat_pulse(false, Some(2.0), &theme), ("◉", theme.muted));
        let (on_downbeat, color) = beat_pulse(true, Some(2.0), &theme);
        assert_eq!(on_downbeat, "●");
        assert_eq!(color, theme.ok);
        let (on_beat, color) = beat_pulse(true, Some(2.25), &theme);
        assert_eq!(on_beat, "●");
        assert_eq!(color, theme.accent);
        let (between, faded) = beat_pulse(true, Some(2.6), &theme);
        assert_eq!(between, "◉");
        assert_ne!(faded, theme.accent, "faded towards the muted colour");
    }

    #[test]
    fn the_dot_uses_safe_frames_when_symbols_are_unavailable() {
        let _symbols = crate::terminal::ForceSymbolsForTest::set(false);
        let theme = theme();
        assert_eq!(beat_pulse(false, Some(2.0), &theme).0, "o");
        assert_eq!(beat_pulse(true, Some(2.0), &theme).0, "\u{b7}");
        assert_eq!(beat_pulse(true, Some(2.6), &theme).0, "o");
    }

    #[test]
    fn the_header_reports_tempo_and_process_counters() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.stats = ProcessStats {
            cpu_percent: Some(12.0),
            machine_cpu_percent: Some(40.0),
            resident_bytes: Some(84 * 1024 * 1024),
            footprint_bytes: None,
        };
        chrome.cps = Some(0.5);
        chrome.cycle = Some(4.0);

        let header_area = Rect::new(0, 0, 140, 1);
        let mut header_buffer = Buffer::empty(header_area);
        Header { chrome: &chrome }.render(header_area, &mut header_buffer);
        let header_text = text_of(&header_buffer);
        assert!(header_text.contains("120.0 bpm"), "{header_text}");
        assert!(header_text.contains("0.50 cps"), "{header_text}");
        assert!(header_text.contains("cpu 12%"), "{header_text}");
        assert!(header_text.contains("mem 84.0MB"), "{header_text}");
        assert!(header_text.contains("live"), "{header_text}");
        // Advanced is the default, so the machine's own share stands
        // beside rustel's: a player watching a low number while the
        // laptop underneath them is pinned has nothing on the screen
        // telling them so.
        assert!(header_text.contains("sys "), "{header_text}");
    }

    /// The header's memory is the footprint, which is what the process
    /// owns, and not its resident set. Where there is no footprint, the
    /// resident set stands in.
    #[test]
    fn the_header_shows_the_memory_footprint_over_the_resident_set() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        let area = Rect::new(0, 0, 140, 1);
        let header = |chrome: &StudioChrome<'_>| {
            let mut buffer = Buffer::empty(area);
            Header { chrome }.render(area, &mut buffer);
            text_of(&buffer)
        };

        chrome.stats = ProcessStats {
            cpu_percent: Some(12.0),
            machine_cpu_percent: None,
            resident_bytes: Some(300 * 1024 * 1024),
            footprint_bytes: Some(120 * 1024 * 1024),
        };
        let text = header(&chrome);
        assert!(text.contains("mem 120MB"), "{text}");
        assert!(!text.contains("300MB"), "{text}");

        chrome.stats.footprint_bytes = None;
        let text = header(&chrome);
        assert!(text.contains("mem 300MB"), "{text}");
    }

    /// The four levels add detail in strict order: nothing, rustel's own
    /// figures, the machine's beside them, then the engine's deadline
    /// pressure too. Each level shows all that the level below it shows.
    #[test]
    fn the_metric_detail_setting_shows_a_ladder_of_header_counters() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.stats = ProcessStats {
            cpu_percent: Some(12.0),
            machine_cpu_percent: Some(55.0),
            resident_bytes: Some(84 * 1024 * 1024),
            footprint_bytes: None,
        };
        let mut pressure = EnginePressureSnapshot {
            level: EnginePressureLevel::Warning,
            ..EnginePressureSnapshot::default()
        };
        pressure.device.realtime_load.slow_load_basis_points = 2_400;
        chrome.pressure = Some(&pressure);
        let area = Rect::new(0, 0, 160, 1);

        use settings::MetricDetail::{Advanced, Basic, Full, None as Off};
        let cases: &[(settings::MetricDetail, &[&str], &[&str])] = &[
            (Off, &[], &["cpu 12%", "mem 84", "sys 55%", "DSP"]),
            (Basic, &["cpu 12%", "mem 84"], &["sys 55%", "DSP"]),
            (Advanced, &["cpu 12%", "sys 55%"], &["DSP"]),
            (Full, &["cpu 12%", "sys 55%", "DSP 24%"], &[]),
        ];
        for &(detail, present, absent) in cases {
            chrome.metric_detail = detail;
            let mut buffer = Buffer::empty(area);
            Header { chrome: &chrome }.render(area, &mut buffer);
            let text = text_of(&buffer);
            for token in present {
                assert!(text.contains(token), "{detail:?}: {text}");
            }
            for token in absent {
                assert!(!text.contains(token), "{detail:?}: {text}");
            }
        }
    }

    /// The counters are the memory breakdown's button, and the rect the header
    /// records is exactly the run it drew: `cpu … · mem … · fps`, whole.
    #[test]
    fn memory_chip_is_recorded_where_counters_paint() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        chrome.metric_detail = settings::MetricDetail::Basic;
        chrome.stats.resident_bytes = Some(84 * 1024 * 1024);
        let area = Rect::new(0, 1, 160, 1);
        let mut buffer = Buffer::empty(area);
        crate::memory::set_memory_chip(None);

        Header { chrome: &chrome }.render(area, &mut buffer);

        let chip = crate::memory::memory_chip().expect("the counters drew");
        assert_eq!(chip.y, 1);
        let drawn = (chip.x..chip.right())
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();
        assert!(drawn.starts_with("cpu "), "{drawn}");
        assert!(drawn.contains("mem 84.0MB"), "{drawn}");
        assert!(drawn.ends_with("ms"), "the whole run and no more: {drawn}");
        assert!(crate::memory::memory_chip_at(chip.x, 1));
        assert!(!crate::memory::memory_chip_at(chip.right(), 1));
    }

    /// A header that draws no counters leaves no button behind: at metric
    /// detail "none", and on a header too narrow for them. (Zen, with no
    /// header at all, is the app's frame to paint; its test is there.)
    #[test]
    fn memory_chip_is_cleared_at_metric_detail_none_and_narrow() {
        let theme = theme();
        let master = MasterState::new(Instant::now());
        let mut chrome = chrome(&theme, &master, Path::new("live.strudel"));
        let wide = Rect::new(0, 1, 160, 1);
        let chip_after = |chrome: &StudioChrome<'_>, area: Rect| {
            crate::memory::set_memory_chip(Some((1, 1, 30)));
            Header { chrome }.render(area, &mut Buffer::empty(area));
            crate::memory::memory_chip()
        };

        chrome.metric_detail = settings::MetricDetail::Basic;
        assert!(chip_after(&chrome, wide).is_some());
        chrome.metric_detail = settings::MetricDetail::None;
        assert_eq!(chip_after(&chrome, wide), None);
        chrome.metric_detail = settings::MetricDetail::Basic;
        assert_eq!(chip_after(&chrome, Rect::new(0, 1, 99, 1)), None);
    }
    #[cfg(test)]
    mod loading_line_tests {
        //! The loading line preserves text, remains readable in each theme, and clears
        //! obsolete underlines when progress shrinks or the line disappears.

        use super::*;

        fn header_row(width: u16, theme: &Theme) -> Buffer {
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
            for x in 0..width {
                buffer[(x, 0)].set_symbol("x").set_bg(theme.surface);
            }
            buffer
        }

        fn underlined(buffer: &Buffer, colour: Color) -> u16 {
            (0..buffer.area.width)
                .filter(|x| {
                    let cell = &buffer[(*x, 0)];
                    cell.modifier.contains(Modifier::UNDERLINED) && cell.underline_color == colour
                })
                .count() as u16
        }

        /// The readiness chip keeps its column while the transport word
        /// changes: every word fills a field as wide as the widest of them,
        /// so the chip and what follows it never move sideways.
        #[test]
        fn the_go_chip_keeps_its_column_while_the_transport_word_changes() {
            let area = Rect::new(0, 0, 80, 1);
            let widest = TransportWord {
                stopping: true,
                playing: false,
            };
            let column = header_go_chip(area, widest, GoState::Unchecked).x;
            for stopping in [false, true] {
                for playing in [false, true] {
                    let word = TransportWord { stopping, playing };
                    assert_eq!(
                        UnicodeWidthStr::width(word.text().as_str()),
                        TransportWord::FIELD,
                        "the word fills its field"
                    );
                    assert_eq!(
                        header_go_chip(area, word, GoState::Unchecked).x,
                        column,
                        "the chip keeps its column"
                    );
                }
            }
        }

        /// The busy dot is the beat's own two glyphs in the warn colour,
        /// breathing on wall-clock time, so a job too brief to read as a
        /// word is still seen - and never moves anything on the row.
        #[test]
        fn the_busy_dot_breathes_between_the_beat_glyphs() {
            let theme = Theme::default();
            let (lit, lit_colour) = busy_pulse(Duration::ZERO, &theme);
            let (half, half_colour) = busy_pulse(Duration::from_millis(100), &theme);
            let (again, _) = busy_pulse(Duration::from_millis(400), &theme);
            assert_eq!(lit, crate::terminal::symbol("●"));
            assert_eq!(half, crate::terminal::symbol("◉"));
            assert_eq!(again, lit, "the breath wraps");
            assert_eq!(lit_colour, theme.warn);
            assert_ne!(half_colour, lit_colour, "the colour fades inside a breath");
        }

        /// The loading line has at least 3:1 contrast against an RGB header surface.
        #[test]
        fn the_line_reads_against_the_header_in_every_theme() {
            for name in Theme::built_in_names() {
                let theme = Theme::built_in(name).unwrap_or_else(|| panic!("{name} is missing"));
                let colour = loading_line_colour(&theme);
                if let (Color::Rgb(..), Color::Rgb(..)) = (colour, theme.surface) {
                    let ratio = crate::theme::contrast_ratio(colour, theme.surface);
                    assert!(ratio >= 3.0, "{name}: {ratio}");
                }
            }
        }

        /// The underline grows with progress, starting at two cells. The fallback
        /// tints the same fraction of the row. Both preserve the header text.
        #[test]
        fn the_line_reaches_as_far_as_the_load_and_falls_back_to_a_tint() {
            let theme = Theme::default();
            let colour = loading_line_colour(&theme);
            let area = Rect::new(0, 0, 40, 1);
            let mut reaches = Vec::new();
            for reach in [0.0, 0.25, 0.75, 1.0] {
                let mut buffer = header_row(40, &theme);
                draw_loading_line(
                    &mut buffer,
                    area,
                    &LoadingLine {
                        reach,
                        label: None,
                        underline: true,
                    },
                    &theme,
                );
                assert_eq!(buffer[(0, 0)].symbol(), "x", "the text stays");
                reaches.push(underlined(&buffer, colour));
            }
            assert_eq!(reaches, [2, 10, 30, 40]);

            let mut buffer = header_row(40, &theme);
            draw_loading_line(
                &mut buffer,
                area,
                &LoadingLine {
                    reach: 0.5,
                    label: None,
                    underline: false,
                },
                &theme,
            );
            assert_eq!(underlined(&buffer, colour), 0);
            let tint = crate::theme::mix(theme.surface, theme.accent, 0.2);
            let tinted = (0..40).filter(|x| buffer[(*x, 0)].bg == tint).count();
            assert_eq!(tinted, 20);
        }

        /// What a backend wrote, kept where the test can read it.
        #[derive(Clone, Default)]
        struct Sent(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

        impl std::io::Write for Sent {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        /// Which cells of a one-row screen show an underline after `sent` is
        /// played into it: cursor moves, SGR 4 / 24 / 0, and one-column glyphs.
        fn underlines_on_screen(sent: &str, screen: &mut [bool]) {
            let mut underline = false;
            let mut column = 0usize;
            let mut chars = sent.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch != '\u{1b}' {
                    if let Some(cell) = screen.get_mut(column) {
                        *cell = underline;
                    }
                    column += 1;
                    continue;
                }
                if chars.next() != Some('[') {
                    continue;
                }
                let mut body = String::new();
                let end = loop {
                    match chars.next() {
                        Some(c) if c.is_ascii_digit() || c == ';' || c == ':' || c == '?' => {
                            body.push(c)
                        }
                        other => break other,
                    }
                };
                match end {
                    Some('H') => {
                        let col = body.split(';').nth(1).and_then(|c| c.parse::<usize>().ok());
                        column = col.unwrap_or(1).saturating_sub(1);
                    }
                    Some('m') => {
                        let mut params = body.split(';');
                        while let Some(param) = params.next() {
                            match param {
                                "" | "0" | "24" => underline = false,
                                "4" => underline = true,
                                // Colours carry their own arguments.
                                "38" | "48" | "58" => {
                                    let skip = if params.next() == Some("5") { 1 } else { 3 };
                                    for _ in 0..skip {
                                        params.next();
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        /// Redrawing a shorter or absent line removes the previous underline.
        #[test]
        fn the_terminal_keeps_no_underline_where_the_line_no_longer_reaches() {
            use ratatui::backend::Backend;

            let theme = Theme::default();
            let area = Rect::new(0, 0, 20, 1);
            let drawn = |reach: Option<f64>| {
                let mut buffer = header_row(20, &theme);
                if let Some(reach) = reach {
                    draw_loading_line(
                        &mut buffer,
                        area,
                        &LoadingLine {
                            reach,
                            label: None,
                            underline: true,
                        },
                        &theme,
                    );
                }
                buffer
            };
            for (from, to) in [(1.0, Some(0.5)), (0.5, None)] {
                let before = drawn(Some(from));
                let after = drawn(to);
                let sent = Sent::default();
                let mut backend = crate::terminal::StudioBackend::new(sent.clone());
                backend
                    .draw(Buffer::empty(area).diff(&before).into_iter())
                    .expect("the first frame");
                backend
                    .draw(before.diff(&after).into_iter())
                    .expect("the diff is drawn");
                let mut screen = [false; 20];
                underlines_on_screen(&String::from_utf8_lossy(&sent.0.borrow()), &mut screen);
                let expected: Vec<bool> = (0..20)
                    .map(|x| after[(x, 0)].modifier.contains(Modifier::UNDERLINED))
                    .collect();
                assert_eq!(screen.to_vec(), expected, "{from} to {to:?}");
            }
        }
    }
}

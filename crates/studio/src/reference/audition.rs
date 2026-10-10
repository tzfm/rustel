//! Preview keyboards, waveforms, meters, and gain conversion.

use super::*;

/// Keyboard space for the chords and scales tabs: three rows for a piano
/// when the panel is tall enough, otherwise one for a chromatic strip.
pub(super) fn keyboard_rows(area: Rect) -> u16 {
    if area.height >= 16 { 3 } else { 1 }
}

const BLACK_KEY: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

fn is_black_key(midi: i32) -> bool {
    BLACK_KEY[midi.rem_euclid(12) as usize]
}

fn is_white_key(midi: i32) -> bool {
    !is_black_key(midi)
}

fn white_keys_before(start: i32, midi: i32) -> u16 {
    (start..midi).filter(|&step| is_white_key(step)).count() as u16
}

/// The selected notes at the foot of the chords and scales tabs.
///
/// Three rows allow a piano; less height or width falls back to a chromatic
/// strip. All selected notes share the sounding colour during a preview.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_keyboard(
    buffer: &mut Buffer,
    inner: Rect,
    y: u16,
    rows: u16,
    _chord: Option<&str>,
    notes: &[f64],
    sounding: Option<usize>,
    theme: &Theme,
) {
    if rows == 0 {
        return;
    }
    let (lowest, highest) = notes
        .iter()
        .fold((i32::MAX, i32::MIN), |(low, high), note| {
            let midi = (*note as i32).clamp(0, 127);
            (low.min(midi), high.max(midi))
        });
    let base = if lowest == i32::MAX {
        48
    } else {
        lowest / 12 * 12
    };
    let lit = |midi: i32| notes.iter().any(|note| (*note as i32) == midi);
    let live = |midi: i32| sounding.is_some() && notes.iter().any(|note| (*note as i32) == midi);
    if rows >= 3 {
        render_piano_keyboard(buffer, inner, y, base, highest, &lit, &live, theme);
    } else {
        render_chromatic_strip(buffer, inner, y, base, highest, &lit, &live, theme);
    }
}

fn piano_end(base: i32, highest: i32, inner_width: u16) -> i32 {
    let mut fitted = base;
    for octaves in 1..=3 {
        let end = base + octaves * 12;
        let whites = (base..=end).filter(|&midi| is_white_key(midi)).count() as u16;
        // `|C` per white plus a closing `|`.
        if inner_width < whites.saturating_mul(2).saturating_add(1) {
            break;
        }
        fitted = end;
        if end >= highest {
            break;
        }
    }
    fitted
}

fn sounding_style(theme: &Theme) -> Style {
    Style::default().fg(theme.ok).add_modifier(Modifier::BOLD)
}

fn white_letter(midi: i32) -> char {
    match midi.rem_euclid(12) {
        0 => 'C',
        2 => 'D',
        4 => 'E',
        5 => 'F',
        7 => 'G',
        9 => 'A',
        11 => 'B',
        _ => ' ',
    }
}

#[allow(clippy::too_many_arguments)]
fn render_piano_keyboard(
    buffer: &mut Buffer,
    inner: Rect,
    y: u16,
    base: i32,
    highest: i32,
    lit: &dyn Fn(i32) -> bool,
    live: &dyn Fn(i32) -> bool,
    theme: &Theme,
) {
    let end = piano_end(base, highest, inner.width);
    if end <= base {
        render_chromatic_strip(buffer, inner, y + 1, base, highest, lit, live, theme);
        return;
    }
    let whites = white_keys_before(base, end) + u16::from(is_white_key(end));
    let width = whites.saturating_mul(2).saturating_add(1);
    if inner.width < width {
        render_chromatic_strip(buffer, inner, y + 1, base, highest, lit, live, theme);
        return;
    }
    let x = inner.right() - width;
    let pipe = Style::default().fg(theme.rule);
    for midi in base..=end {
        if !is_white_key(midi) {
            continue;
        }
        let col = x + white_keys_before(base, midi) * 2;
        let on = lit(midi);
        let now = live(midi);
        let letter = white_letter(midi);
        let letter_style = if now {
            sounding_style(theme)
        } else if on {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if midi.rem_euclid(12) == 0 {
            Style::default().fg(theme.foreground)
        } else {
            Style::default().fg(theme.muted)
        };
        buffer.set_stringn(col, y, "│", 1, pipe);
        buffer.set_stringn(col, y + 1, "│", 1, pipe);
        buffer.set_stringn(col, y + 2, "│", 1, pipe);
        buffer.set_stringn(col + 1, y + 2, letter.to_string(), 1, letter_style);
    }
    buffer.set_stringn(x + width - 1, y, "│", 1, pipe);
    buffer.set_stringn(x + width - 1, y + 1, "│", 1, pipe);
    buffer.set_stringn(x + width - 1, y + 2, "│", 1, pipe);
    for midi in base..=end {
        if !is_black_key(midi) {
            continue;
        }
        // Sit on the pipe between this black's lower white and the next,
        // the way a black key hangs between C and D.
        let col = x + white_keys_before(base, midi + 1) * 2;
        let colour = if live(midi) {
            sounding_style(theme)
        } else if lit(midi) {
            Style::default().fg(theme.accent)
        } else {
            Style::default().fg(theme.muted)
        };
        buffer.set_stringn(col, y, "█", 1, colour);
        buffer.set_stringn(col, y + 1, "│", 1, colour);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_chromatic_strip(
    buffer: &mut Buffer,
    inner: Rect,
    y: u16,
    base: i32,
    highest: i32,
    lit: &dyn Fn(i32) -> bool,
    live: &dyn Fn(i32) -> bool,
    theme: &Theme,
) {
    let keys: u16 = if highest >= base + 24 && inner.width >= 40 {
        36
    } else {
        24
    };
    let width = keys + 2;
    if inner.width < width {
        return;
    }
    let x = inner.right() - width;
    let cap = Style::default().fg(theme.rule);
    buffer.set_stringn(x, y, super::super::terminal::symbol("▏"), 1, cap);
    buffer.set_stringn(
        x + width - 1,
        y,
        super::super::terminal::symbol("▕"),
        1,
        cap,
    );
    for step in 0..keys {
        let midi = base + i32::from(step);
        let black = is_black_key(midi);
        let glyph = if black { "▀" } else { "█" };
        let style = if live(midi) {
            sounding_style(theme)
        } else {
            Style::default().fg(match (lit(midi), black, midi.rem_euclid(12) == 0) {
                (true, _, _) => theme.accent,
                (false, true, _) => theme.rule,
                (false, false, true) => theme.foreground,
                (false, false, false) => theme.muted,
            })
        };
        buffer.set_stringn(x + 1 + step, y, glyph, 1, style);
    }
}

/// One sample as its own envelope, mirrored about the middle and lit as
/// far as it has played.
///
/// The played part uses the accent colour and the rest uses the rule colour,
/// so playback advances across the full shape without scrolling it.
pub(super) fn render_sample_shape(
    shape: &[u8],
    played: f32,
    area: Rect,
    buffer: &mut Buffer,
    theme: &Theme,
) {
    if area.is_empty() || shape.is_empty() {
        return;
    }
    // The line canvas preserves detail in this short strip: 2×4 Braille
    // dots per cell on glyph tiers, or pixels when available.
    let mut canvas = super::super::visuals::Canvas::lines(area);
    let (width, height) = (canvas.width(), canvas.height());
    if width < 2 || height < 2 {
        return;
    }
    // Normalize to this sample's peak so quiet recordings still have a
    // readable shape; this changes only the drawing, not playback gain.
    let loudest = shape.iter().copied().max().unwrap_or(0);
    if loudest == 0 {
        return;
    }
    let middle = (height - 1) as f32 / 2.0;
    let centre = middle.round() as usize;
    let lit = (played.clamp(0.0, 1.0) * width as f32).round() as usize;
    for x in 0..width {
        // Every column reads the peak of the buckets it stands for, so a
        // narrow panel loses no transient - the same reduction the shape
        // itself was made with.
        let from = x * shape.len() / width;
        let to = ((x + 1) * shape.len() / width).clamp(from + 1, shape.len());
        let peak = shape[from..to].iter().copied().max().unwrap_or(0);
        let reach = (f32::from(peak) / f32::from(loudest) * middle).round() as usize;
        let colour = if x < lit { theme.accent } else { theme.rule };
        // Zero-height buckets keep a centre line so quiet passages do not
        // split the waveform into disconnected sections.
        for y in centre.saturating_sub(reach)..=(centre + reach).min(height - 1) {
            canvas.set(x, y, colour);
        }
    }
    canvas.paint(buffer, theme.surface);
}

/// Keep preview drawing and pointer handling enabled for the same tabs.
pub fn tab_previews(tab: Tab) -> bool {
    match tab {
        Tab::Samples | Tab::Chords | Tab::Scales => true,
        Tab::Reference => false,
        #[cfg(feature = "vst")]
        Tab::Vst => false,
        #[cfg(feature = "hydra")]
        Tab::Examples | Tab::Generator => false,
    }
}

/// The sample shape, when available, above a combined peak meter and
/// preview gain fader.
pub(super) fn render_samples_pulse(
    pulse: &SamplesPulse<'_>,
    inner: Rect,
    tab: Tab,
    buffer: &mut Buffer,
    theme: &Theme,
) {
    if inner.width < 16 || inner.height < 6 {
        return;
    }
    let (scope_y, meter_y) = if tab == Tab::Samples {
        sample_pulse_rows(inner)
    } else {
        samples_pulse_rows(inner)
    };
    // Use the rows reserved by the layout: samples get one or three;
    // chords and scales keep one below their keyboard.
    let wave = if tab == Tab::Samples {
        samples_wave_area(inner)
    } else {
        Rect::new(inner.x, scope_y, inner.width, 1)
    };

    // Keep the waveform rows reserved when no shape is available so the
    // list does not move when a preview starts.
    if let Some((shape, played)) = pulse.shape {
        render_sample_shape(shape, played, wave, buffer, theme);
    }

    // The meter background shows peak level; the knob and label show
    // preview gain in decibels.
    let gain = format_preview_gain(pulse.preview_gain);
    let arrow = super::super::terminal::symbol("▸");
    // Keep the samples tab's volume and reveal shortcuts beside the fader.
    let label = if tab == Tab::Samples {
        format!(" {arrow} {gain} ⌥↑↓ ⌥O file ")
    } else {
        format!(" {arrow} {gain} ")
    };
    let label_width = label.chars().count() as u16;
    let bar_width = inner.width.saturating_sub(label_width);
    let floor = -60.0_f32;
    let up = ((pulse.peak_db.max(floor) - floor) / -floor).clamp(0.0, 1.0);
    let filled = (f32::from(bar_width) * up).round() as u16;
    let knob = (preview_gain_position(pulse.preview_gain) * f32::from(bar_width.saturating_sub(1)))
        .round() as u16;
    for column in 0..bar_width {
        if let Some(cell) = buffer.cell_mut((inner.x + column, meter_y)) {
            let level = super::super::meter::level_color(pulse.peak_db, theme);
            // On the panel's own ground, not the editor's: a meter track
            // painted in the editor's background is a strip of the wrong
            // colour across a surface-coloured panel.
            let ground = if column < filled {
                let tinted = super::super::theme::mix(theme.surface, level, 0.3);
                // A palette theme cannot blend, and a bar snapped back to
                // the surface would be an invisible meter.
                if tinted == theme.surface {
                    level
                } else {
                    tinted
                }
            } else {
                theme.surface
            };
            cell.set_bg(ground);
            if column == knob {
                cell.set_char(super::super::terminal::symbol("▮").chars().next().unwrap());
                cell.set_fg(theme.accent);
            } else {
                cell.set_char('─');
                cell.set_fg(theme.rule);
            }
        }
    }
    buffer.set_stringn(
        inner.x + bar_width,
        meter_y,
        &label,
        usize::from(label_width),
        Style::default().fg(theme.muted),
    );
}

/// The one spinner the whole studio turns while something is on its way:
/// four braille frames on the wall clock, the same pace wherever it stands.
/// A wait that says its name in words is a wait the reader must learn;
/// a shared glyph says "on its way" everywhere, and the context says what.
pub fn spinner_glyph() -> &'static str {
    ["⠋", "⠙", "⠸", "⠴"][(std::time::UNIX_EPOCH
        .elapsed()
        .map(|age| age.as_millis() / 160)
        .unwrap_or(0)
        % 4) as usize]
}

/// The preview's current peak in dBFS, read straight off its scope frame:
/// the meter row tints with the sound the browser is making, not the mix.
pub fn audition_peak_db(audio: Option<&rustel_runtime::ui_analysis::UiAudioAnalysisFrame>) -> f32 {
    let peak = audio
        .map(|frame| {
            frame
                .scope
                .iter()
                .fold(0.0_f32, |peak, s| peak.max(s.abs()))
        })
        .unwrap_or(0.0);
    if peak <= 0.0 {
        f32::NEG_INFINITY
    } else {
        rustel_audio::linear_to_db(peak)
    }
}

/// The preview fader's decibel range: silence at the far left, +12 dB of
/// real headroom at the right (the engine's own audition cap).
pub const PREVIEW_FLOOR_DB: f32 = -60.0;
pub const PREVIEW_CEIL_DB: f32 = 12.0;

/// The preview gain a pointer position on the meter row means. The far
/// left is true silence, like the master fader's.
pub fn preview_gain_at(inner: Rect, x: u16) -> f32 {
    // The label the meter draws: gain, and - on the samples tab only, the
    // one place a reveal hint earns its room - the shortcut beside it.
    // Drawing and hit-testing must agree on this width, or the knob sits
    // where the click did not.
    let label_width = " ▸ +12.0dB ⌥↑↓ ⌥O file ".chars().count() as u16;
    let bar_width = inner.width.saturating_sub(label_width).max(1);
    let along = f32::from(x.saturating_sub(inner.x).min(bar_width - 1));
    let fraction = along / f32::from(bar_width.saturating_sub(1).max(1));
    if fraction <= 0.0 {
        return 0.0;
    }
    let db = PREVIEW_FLOOR_DB + fraction * (PREVIEW_CEIL_DB - PREVIEW_FLOOR_DB);
    // The unity detent, exactly as the master fader has it: at least half
    // a column wide, so no bar is too narrow to land on 0 dB.
    let step = (PREVIEW_CEIL_DB - PREVIEW_FLOOR_DB) / f32::from(bar_width.saturating_sub(1).max(1));
    let db = if db.abs() <= (step * 0.5).max(0.75) {
        0.0
    } else {
        db
    };
    rustel_audio::db_to_linear(db)
}

/// Where a gain sits on the bar, 0..=1.
fn preview_gain_position(gain: f32) -> f32 {
    if gain <= 0.0 {
        return 0.0;
    }
    let db = rustel_audio::linear_to_db(gain).clamp(PREVIEW_FLOOR_DB, PREVIEW_CEIL_DB);
    (db - PREVIEW_FLOOR_DB) / (PREVIEW_CEIL_DB - PREVIEW_FLOOR_DB)
}

/// The preview volume as a mixer readout.
pub fn format_preview_gain(gain: f32) -> String {
    if gain <= 0.0 {
        return "muted".to_owned();
    }
    let db = rustel_audio::linear_to_db(gain);
    if db.abs() < 0.05 {
        "0.0dB".to_owned()
    } else {
        format!("{db:+.1}dB")
    }
}

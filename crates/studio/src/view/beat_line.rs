//! The beat line: a thin strip under the header that keeps the studio's
//! pulse visible from across the room. It reads the same clock the
//! header's beat dot reads and the theme's own colours, whatever they
//! are, and rests as a dim flat line while no set plays, so its row never
//! flickers in and out with the transport.

use super::*;

pub(super) struct BeatLine<'a, 'b> {
    pub(super) chrome: &'a StudioChrome<'b>,
}

/// How much of the line the travelling segment and the wave's swell cover.
const SPAN: f32 = 0.16;
/// The rungs the wave's cells step through, low to high.
const WAVE_RUNGS: [&str; 5] = ["▁", "▂", "▃", "▄", "▅"];

impl Widget for BeatLine<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let area = Rect { height: 1, ..area };
        if area.is_empty() {
            return;
        }
        let theme = self.chrome.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        for x in area.left()..area.right() {
            let (glyph, level) = beat_cell(
                self.chrome.beat_line,
                self.chrome.playing,
                self.chrome.cycle,
                x - area.left(),
                area.width,
            );
            buffer.set_stringn(
                x,
                area.y,
                glyph,
                1,
                Style::default()
                    .fg(mix(theme.muted, theme.accent, level))
                    .bg(theme.surface),
            );
        }
    }
}

/// One cell of the strip: the glyph it shows and how lit it is, 0 to 1.
/// The beat maths is the pulse the header's dot runs on - four beats to
/// the cycle, brightest on the downbeat, fading between - carried across
/// the width instead of packed into one glyph.
fn beat_cell(
    style: crate::settings::BeatLineStyle,
    playing: bool,
    cycle: Option<f64>,
    x: u16,
    width: u16,
) -> (&'static str, f32) {
    use crate::settings::BeatLineStyle as Line;
    let Some(cycle) = cycle.filter(|_| playing) else {
        return ("─", 0.0);
    };
    let beat = cycle * 4.0;
    let phase = beat.rem_euclid(1.0) as f32;
    let downbeat = beat.rem_euclid(4.0) < 1.0;
    let pulse = (1.0 - phase).powi(3);
    let across = f32::from(x) / f32::from(width.max(1));
    match style {
        Line::Off => ("─", 0.0),
        Line::Pulse => {
            let level = 0.06 + 0.94 * pulse * if downbeat { 1.0 } else { 0.6 };
            ("━", level)
        }
        Line::Travel => {
            // The segment wraps: what leaves the right edge enters the
            // left, once per cycle.
            let position = cycle.rem_euclid(1.0) as f32;
            let offset = (across - position).abs();
            let distance = offset.min(1.0 - offset);
            let band = (1.0 - (distance / SPAN).min(1.0)).powi(2);
            let level = 0.06 + 0.94 * band * (0.4 + 0.6 * pulse);
            (if band > 0.6 { "━" } else { "─" }, level)
        }
        Line::Wave => {
            // A swell rolls along the line once per cycle, breathing with
            // the beat on top of travelling.
            let turn = cycle.rem_euclid(1.0) as f32;
            let swell = (std::f32::consts::TAU * (across * 2.0 - turn * 2.0)).sin();
            let height =
                (0.5 + 0.5 * swell) * (0.25 + 0.75 * pulse) * if downbeat { 1.0 } else { 0.7 };
            let rung = (height * WAVE_RUNGS.len() as f32) as usize;
            (
                WAVE_RUNGS[rung.min(WAVE_RUNGS.len() - 1)],
                0.15 + 0.85 * height,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The beat maths the header's dot runs on, carried across the width:
    /// brightest exactly on the beat, dimmer between, brightest of all on
    /// the downbeat.
    #[test]
    fn the_pulse_breathes_on_the_beat() {
        let (glyph, on) = beat_cell(
            crate::settings::BeatLineStyle::Pulse,
            true,
            Some(0.0),
            5,
            40,
        );
        assert_eq!(glyph, "━");
        assert!(on > 0.99, "the downbeat starts whole: {on}");
        let (_, mid) = beat_cell(
            crate::settings::BeatLineStyle::Pulse,
            true,
            Some(0.125),
            5,
            40,
        );
        assert!(mid < on * 0.7, "mid-beat falls away: {mid}");
        let (_, offbeat) = beat_cell(
            crate::settings::BeatLineStyle::Pulse,
            true,
            Some(0.25),
            5,
            40,
        );
        assert!(offbeat < on, "a later beat never outshines the downbeat");
    }

    /// The travelling segment crosses once per cycle and wraps around,
    /// and the rest of the line rests dim.
    #[test]
    fn the_travelling_segment_crosses_once_a_cycle() {
        let (leading, bright) = beat_cell(
            crate::settings::BeatLineStyle::Travel,
            true,
            Some(0.0),
            0,
            40,
        );
        assert_eq!(leading, "━");
        assert!(bright > 0.5, "the cycle's start lights the left edge");
        let (_, ahead) = beat_cell(
            crate::settings::BeatLineStyle::Travel,
            true,
            Some(0.0),
            20,
            40,
        );
        assert!(
            ahead < 0.2,
            "half a line ahead of the segment rests: {ahead}"
        );
        let (_, wrapped) = beat_cell(
            crate::settings::BeatLineStyle::Travel,
            true,
            Some(0.99),
            0,
            40,
        );
        assert!(wrapped > 0.1, "the leaving tail wraps into the left edge");
    }

    /// The wave rolls: cells differ, heights come from the rungs, and the
    /// whole thing swells on the beat.
    #[test]
    fn the_wave_rolls_along_the_line() {
        let cells: Vec<&str> = (0..40)
            .map(|x| beat_cell(crate::settings::BeatLineStyle::Wave, true, Some(0.0), x, 40).0)
            .collect();
        assert!(
            cells.iter().any(|glyph| *glyph != cells[0]),
            "the line is not one flat rung: {cells:?}"
        );
        assert!(
            cells.iter().all(|glyph| WAVE_RUNGS.contains(glyph)),
            "{cells:?}"
        );
        let (_, on_beat) = beat_cell(crate::settings::BeatLineStyle::Wave, true, Some(0.0), 0, 40);
        let (_, between) = beat_cell(
            crate::settings::BeatLineStyle::Wave,
            true,
            Some(0.125),
            0,
            40,
        );
        assert!(on_beat > between, "the swell breathes with the beat");
    }

    /// No clock, no transport: the line rests flat and dim, so its room
    /// never blinks in and out with play and stop.
    #[test]
    fn a_resting_line_lies_flat() {
        for style in [
            crate::settings::BeatLineStyle::Pulse,
            crate::settings::BeatLineStyle::Travel,
            crate::settings::BeatLineStyle::Wave,
        ] {
            let (glyph, level) = beat_cell(style, false, Some(0.0), 5, 40);
            assert_eq!(glyph, "─", "{style:?}");
            assert_eq!(level, 0.0, "{style:?}");
            let (glyph, level) = beat_cell(style, true, None, 5, 40);
            assert_eq!((glyph, level), ("─", 0.0), "{style:?}");
        }
    }
}

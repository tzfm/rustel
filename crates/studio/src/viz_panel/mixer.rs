/*
mixer.rs - The mixer widget: a row a strip, meters and faders
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! One row a thing that carries sound - the audio input, each orbit the
//! score names or has sounded lately, the master - with a meter lit to
//! its level along the row, the level, and the fader on the strips that
//! have one. The faders are the engine's, not the score's: an orbit's
//! level is the score's own `gain` and `postgain`, so its row is a meter
//! alone. The devices are the mixer panel's business (F4); the widget is
//! the sound alone.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use crate::devices::MidiPortCounts;
use crate::theme::Theme;

/// The floor of every meter here: the footer's.
pub const METER_FLOOR_DB: f32 = -60.0;

/// The reduction a limiter's meter draws as a full bar. Past this it
/// stays full: the number is in the readout, and a meter that never
/// fills cannot be read at a glance.
///
/// Lives here with `MixerStripKind` because all three places that draw a
/// limiter - this widget, the panel's strip and the footer's master dock -
/// have to fill by the same amount for the same decibel, or the same
/// limiter reads as working harder in one of them.
pub const REDUCTION_FULL_DB: f32 = 12.0;

/// One strip: what it is, where its meter stands, where its fader is.
/// Shared by the widget and the mixer panel, which draw the same facts
/// two ways.
#[derive(Clone, Debug, PartialEq)]
pub struct MixerStrip {
    /// `in`, `orbit 1`, `master`.
    pub label: String,
    /// What is behind it: the input's name, an orbit's pair.
    pub detail: String,
    /// The meter, in dBFS, `METER_FLOOR_DB` and below for silence.
    pub peak_db: f32,
    /// The held peak, where the strip keeps one.
    pub hold_db: Option<f32>,
    /// The fader, in dB; none for a strip whose level is the score's.
    pub gain_db: Option<f32>,
    /// How far the fader goes, for the strips that have one: the master's
    /// is the footer's scale, the input's climbs far enough to lift a
    /// quiet microphone.
    pub fader_range: Option<(f32, f32)>,
    /// The strip has clipped lately.
    pub clipping: bool,
    /// The strip is muted: metering silence on purpose, which otherwise
    /// looks exactly like metering nothing.
    pub muted: bool,
    /// The strip the keys drive.
    pub selected: bool,
    /// What the strip's column shows.
    pub kind: MixerStripKind,
}

/// What a strip's column means, which decides how it is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum MixerStripKind {
    /// A level, metered from the bottom up on the footer's colour scale.
    #[default]
    Level,
    /// Gain reduction, filling from the top down on the plain ground: the
    /// way a hardware limiter reads, and the opposite direction to a level
    /// so the two are never mistaken for each other at a glance.
    Reduction {
        /// How much the limiter is pulling down, in dB, never negative.
        reduction_db: f32,
        /// Whether the limiter is switched out of the signal.
        ///
        /// A bypassed limiter keeps its ceiling and its character, and
        /// the strip still shows them. This flag alone says whether the
        /// limiter is in the signal.
        bypassed: bool,
    },
}

/// One fader of the evaluated score, on the mixer's own desk.
///
/// The same control as the pill drawn inline in the score, seen from
/// somewhere a performer can reach without hunting through the text - and
/// somewhere a knob can be pointed at, which is what the mapping slots
/// drive. Everything here is read from the score's own slider each frame,
/// so an evaluation rebuilds the desk and nothing has to be invalidated.
#[derive(Clone, Debug, PartialEq)]
pub struct MixerFader {
    /// The call it sits in - `lpf` - or its position when the score never
    /// named it.
    pub label: String,
    /// Where the knob stands, 0 through 1, along its own travel: a
    /// frequency control's is logarithmic, like the pill's.
    pub notch: f32,
    /// What it reads now, formatted the way the score spells it.
    pub value: String,
    /// The ends of its range, formatted the same way.
    pub min: String,
    pub max: String,
    /// The fader the keys drive.
    pub selected: bool,
    /// Which mapping slot moves it, when a slot is set for its place.
    pub slot: Option<usize>,
}

/// Everything the mixer draws, assembled by the studio each frame: the
/// strips, and beside them what the devices are doing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixerFacts {
    pub strips: Vec<MixerStrip>,
    /// The evaluated score's sliders, in source order.
    pub faders: Vec<MixerFader>,
    pub midi_ports: MidiPortCounts,
    pub midi_active: bool,
    /// The last MIDI messages, readable, newest last.
    pub midi_recent: Vec<String>,
    pub pads: usize,
    pub pad_active: bool,
    /// One line a pad: its number and name, and what is held or moved.
    pub pads_live: Vec<String>,
    /// The last pad presses and stick moves, readable, newest last - the
    /// feed MIDI's own `midi_recent` has, for a gamepad.
    pub gamepad_recent: Vec<String>,
}

/// What a limiter's row is pulling down right now, or `None` for a row
/// that meters a level.
///
/// Bypassed reads as nothing: the limiter is out of the signal, so
/// whatever it was taking off a moment ago is not what is happening now,
/// and a bar left standing there would say it was.
fn reduction_working(kind: MixerStripKind) -> Option<f32> {
    match kind {
        MixerStripKind::Level => None,
        MixerStripKind::Reduction { bypassed: true, .. } => Some(0.0),
        MixerStripKind::Reduction { reduction_db, .. } => Some(reduction_db.max(0.0)),
    }
}

/// The strip's meter as a fraction of its travel.
fn travel(peak_db: f32) -> f32 {
    ((peak_db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

/// A level as the row says it.
fn db_text(db: f32) -> String {
    if db <= METER_FLOOR_DB {
        "  -∞ ".to_owned()
    } else {
        format!("{db:>4.0} ")
    }
}

/// The widget's rows: label · meter · level · fader.
pub const LABEL_WIDTH: u16 = 9;
const LEVEL_WIDTH: u16 = 6;
const FADER_WIDTH: u16 = 8;

pub struct MixerView<'a> {
    pub facts: Option<&'a MixerFacts>,
    pub theme: &'a Theme,
}

impl Widget for MixerView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        let Some(facts) = self.facts else {
            buffer.set_stringn(
                area.x,
                area.y,
                "nothing to mix yet",
                usize::from(area.width),
                Style::default().fg(theme.muted),
            );
            return;
        };
        let meter_width = area
            .width
            .saturating_sub(LABEL_WIDTH + LEVEL_WIDTH + FADER_WIDTH + 2)
            .max(4);
        for (y, strip) in (area.y..).zip(&facts.strips) {
            if y >= area.bottom() {
                return;
            }
            let label_style = if strip.selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else if strip.clipping {
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            let marker = if strip.selected {
                crate::terminal::symbol("▸")
            } else {
                " "
            };
            buffer.set_stringn(
                area.x,
                y,
                format!(
                    "{marker}{:<width$}",
                    strip.label,
                    width = usize::from(LABEL_WIDTH) - 1
                ),
                usize::from(LABEL_WIDTH),
                label_style,
            );
            // The meter: a level lit from the left in the colour of the
            // level. On a limiter's row it is the gain reduction, filled
            // back from the right as in the footer. The opposite
            // direction keeps the two apart at a glance. A limiter's own
            // level is silence, so its row does not meter a level.
            let meter_x = area.x + LABEL_WIDTH + 1;
            let reduction = reduction_working(strip.kind);
            for cell in 0..meter_width {
                let (glyph, style) = match reduction {
                    Some(reduction_db) => {
                        let full = (reduction_db / REDUCTION_FULL_DB).clamp(0.0, 1.0);
                        let lit = (full * f32::from(meter_width)).round() as u16;
                        if cell >= meter_width - lit {
                            ("█", Style::default().fg(theme.meter.peak))
                        } else {
                            ("░", Style::default().fg(theme.muted))
                        }
                    }
                    None => {
                        let lit = (travel(strip.peak_db) * f32::from(meter_width)).round() as u16;
                        let colour = if strip.peak_db > 0.0 {
                            theme.error
                        } else if strip.peak_db > -6.0 {
                            theme.warn
                        } else {
                            theme.ok
                        };
                        if cell < lit {
                            ("█", Style::default().fg(colour))
                        } else {
                            ("░", Style::default().fg(theme.muted))
                        }
                    }
                };
                buffer.set_stringn(meter_x + cell, y, glyph, 1, style);
            }
            // The held peak, a tick on the meter.
            if let Some(hold) = strip.hold_db.filter(|hold| *hold > METER_FLOOR_DB) {
                let at = ((travel(hold) * f32::from(meter_width)).round() as u16)
                    .min(meter_width.saturating_sub(1));
                buffer.set_stringn(
                    meter_x + at,
                    y,
                    "▌",
                    1,
                    Style::default().fg(theme.foreground),
                );
            }
            // The number beside the meter is what the row is doing: a
            // level, or what a limiter is taking off. Lit while it works,
            // because that is the number a person watches.
            let (level, working) = match reduction {
                Some(reduction_db) if reduction_db >= 0.05 => {
                    (format!("{:>4.1} ", -reduction_db), true)
                }
                Some(_) => (" 0.0 ".to_owned(), false),
                None => (db_text(strip.peak_db), false),
            };
            buffer.set_stringn(
                meter_x + meter_width + 1,
                y,
                &level,
                usize::from(LEVEL_WIDTH),
                Style::default().fg(if working {
                    theme.meter.peak
                } else {
                    theme.muted
                }),
            );
            // A bypassed limiter keeps its ceiling and its character, so its
            // number reads exactly as an engaged one's does. The desk says
            // `byp` for this; the dock is the other place the mixer is drawn
            // and has to say it too, or the two disagree about whether the
            // limiter is in the signal at all.
            let bypassed = matches!(strip.kind, MixerStripKind::Reduction { bypassed: true, .. });
            let fader = match strip.gain_db {
                Some(_) if bypassed => {
                    format!("{:>width$}", "byp", width = usize::from(FADER_WIDTH))
                }
                Some(db) if db <= METER_FLOOR_DB => "  -∞ dB".to_owned(),
                Some(db) => format!("{db:>+5.1} dB"),
                None => String::new(),
            };
            buffer.set_stringn(
                meter_x + meter_width + 1 + LEVEL_WIDTH,
                y,
                &fader,
                usize::from(FADER_WIDTH),
                Style::default().fg(if strip.selected {
                    theme.accent
                } else {
                    theme.foreground
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(buffer: &Buffer, area: Rect) -> Vec<String> {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    /// Every strip is a row - its label with the orbit's number, a meter
    /// lit to its level, the level and its fader - and nothing else: the
    /// devices are the panel's.
    #[test]
    fn the_mixer_draws_a_strip_a_row() {
        let facts = MixerFacts {
            faders: Vec::new(),
            strips: vec![
                MixerStrip {
                    label: "in".into(),
                    detail: "Mic".into(),
                    peak_db: -18.0,
                    hold_db: Some(-12.0),
                    gain_db: Some(3.0),
                    fader_range: Some((-60.0, 48.0)),
                    clipping: false,
                    muted: false,
                    selected: true,
                    kind: MixerStripKind::Level,
                },
                MixerStrip {
                    label: "orbit 12".into(),
                    detail: String::new(),
                    peak_db: METER_FLOOR_DB,
                    hold_db: None,
                    gain_db: None,
                    fader_range: None,
                    clipping: false,
                    muted: false,
                    selected: false,
                    kind: MixerStripKind::Level,
                },
                MixerStrip {
                    label: "master".into(),
                    detail: String::new(),
                    peak_db: -2.0,
                    hold_db: None,
                    gain_db: Some(-1.5),
                    fader_range: Some((-60.0, 6.0)),
                    clipping: false,
                    muted: false,
                    selected: false,
                    kind: MixerStripKind::Level,
                },
            ],
            midi_ports: MidiPortCounts {
                outputs: 1,
                inputs: 1,
            },
            midi_active: true,
            midi_recent: vec!["MiniLab · cc74 63 ch2".into()],
            pads: 1,
            pad_active: false,
            pads_live: vec!["gamepad(0) Controller".into()],
            gamepad_recent: Vec::new(),
        };
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 60, 5);
        let mut buffer = Buffer::empty(area);
        MixerView {
            facts: Some(&facts),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let rows = drawn(&buffer, area);
        assert!(rows[0].starts_with("▸in "), "{}", rows[0]);
        assert!(
            rows[0].contains("█"),
            "the input's meter is lit: {}",
            rows[0]
        );
        assert!(rows[0].contains("▌"), "the held peak: {}", rows[0]);
        assert!(rows[0].contains(" -18 "), "{}", rows[0]);
        assert!(rows[0].ends_with("+3.0 dB"), "{}", rows[0]);
        assert!(
            rows[1].starts_with(" orbit 12"),
            "the orbit's number is on the row: {}",
            rows[1]
        );
        assert!(rows[1].contains("-∞"), "{}", rows[1]);
        assert!(
            !rows[1].contains("█"),
            "silence lights nothing: {}",
            rows[1]
        );
        assert!(
            rows[1].trim_end().ends_with("-∞"),
            "no fader on an orbit: {}",
            rows[1]
        );
        assert!(rows[2].starts_with(" master"), "{}", rows[2]);
        assert!(rows[2].ends_with("-1.5 dB"), "{}", rows[2]);
        assert!(
            rows[3].trim().is_empty() && rows[4].trim().is_empty(),
            "no lights, no hint: the widget is the sound alone"
        );
    }

    /// A limiter's row meters the gain reduction and not its level, which
    /// is silence. The bar fills back from the right, as in the footer.
    #[test]
    fn a_limiter_row_meters_the_reduction_and_not_its_level() {
        let strip = |reduction_db, bypassed| MixerStrip {
            label: "warm".into(),
            detail: String::new(),
            peak_db: METER_FLOOR_DB,
            hold_db: None,
            gain_db: Some(-1.0),
            fader_range: Some((-24.0, 0.0)),
            clipping: false,
            muted: false,
            selected: false,
            kind: MixerStripKind::Reduction {
                reduction_db,
                bypassed,
            },
        };
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 60, 1);
        let row = |strip: MixerStrip| {
            let facts = MixerFacts {
                strips: vec![strip],
                ..MixerFacts::default()
            };
            let mut buffer = Buffer::empty(area);
            MixerView {
                facts: Some(&facts),
                theme: &theme,
            }
            .render(area, &mut buffer);
            drawn(&buffer, area).remove(0)
        };

        // Working: a bar against the right end of the meter, the number
        // beside it, and the ceiling still in the fader column.
        let working = row(strip(6.0, false));
        assert!(working.contains('█'), "nothing lit: {working}");
        let lit = working.chars().filter(|glyph| *glyph == '█').count();
        let track = working.chars().filter(|glyph| *glyph == '░').count();
        assert_eq!(
            lit,
            ((lit + track) as f32 / 2.0).round() as usize,
            "half the bar at half of full"
        );
        let meter: String = working
            .chars()
            .filter(|glyph| *glyph == '█' || *glyph == '░')
            .collect();
        assert!(
            meter.starts_with('░') && meter.ends_with('█'),
            "it fills back from the right: {working}"
        );
        assert!(working.contains("-6.0"), "what it is taking off: {working}");
        assert!(working.ends_with("-1.0 dB"), "the ceiling: {working}");
        assert!(
            !working.contains("-∞"),
            "and not its level, which is silence: {working}"
        );

        // Idle and bypassed both read as nothing held back - a bar left
        // standing after the switch was thrown would say it was working.
        for quiet in [strip(0.0, false), strip(6.0, true)] {
            let bypassed = matches!(quiet.kind, MixerStripKind::Reduction { bypassed: true, .. });
            let row = row(quiet);
            assert!(!row.contains('█'), "nothing held back, nothing lit: {row}");
            assert!(row.contains(" 0.0 "), "{row}");
            if bypassed {
                assert!(row.trim_end().ends_with("byp"), "{row}");
            }
        }

        // Full at the top of the scale, and no further.
        let hard = row(strip(REDUCTION_FULL_DB * 2.0, false));
        assert!(!hard.contains('░'), "past the scale it stays full: {hard}");
    }

    #[test]
    fn a_missing_picture_says_so() {
        let theme = Theme::built_in_default();
        let area = Rect::new(0, 0, 30, 2);
        let mut buffer = Buffer::empty(area);
        MixerView {
            facts: None,
            theme: &theme,
        }
        .render(area, &mut buffer);
        assert!(drawn(&buffer, area)[0].starts_with("nothing to mix yet"));
        assert_eq!(db_text(-12.4), " -12 ");
        assert_eq!(db_text(-80.0), "  -∞ ");
    }
}

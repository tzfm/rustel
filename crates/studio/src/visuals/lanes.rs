//! The lanes view: the score's lanes as rows on a time axis that scrolls
//! past a fixed playhead. What has played runs off to the left, dimmed, and
//! the engine's lookahead fills the right with what is coming. A Rustel
//! extension, drawn by `_lanes()` under a pattern, by `lanes()` behind the
//! score, and by the Lanes panel.
//!
//! ```text
//!            ▼
//! kick ·▮···▮┊··▮···▮···▮···
//! hats ¦·¦·¦·┊¦·¦·¦·¦·¦·¦·¦·
//! bass ▃▃▃·▅▅┊▅▅·▂▂▂▂·▃▃▃·▅▅
//!       12.4 ◆
//!            ▲
//! ```
//!
//! A row is one sound of one lane. A lane is a labeled statement of the
//! score, found from the source ranges an event carries. An event whose
//! ranges all lie outside the labeled statements, as a pattern kept in a
//! variable does, still gets a row for its sound, without a lane label.
//! The rows of a lane share its colour, and a sound keeps its mark in every
//! lane.

use std::ops::Range;
use std::sync::Arc;

use super::*;

/// The marks of the sounds without a pitch, one per sound in turn.
const HITS: [char; 4] = ['▮', '¦', '×', '▪'];
/// A pitched row draws each note as a block whose height is its pitch
/// within the row's range.
const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// The most cells a row's label takes from the past.
const MAX_LABEL: usize = 16;

pub(super) struct Lanes<'a, 'b> {
    request: &'a VisualRequest<'b>,
    cycles: f64,
    playhead: f64,
    labels: bool,
}

impl<'a, 'b> Lanes<'a, 'b> {
    /// Three cycles with the playhead a quarter of the way in: the engine
    /// looks 2.5 cycles ahead, so the right edge stays filled.
    pub(super) fn new(request: &'a VisualRequest<'b>) -> Self {
        let options = request.options;
        Self {
            request,
            cycles: options.number("cycles").unwrap_or(3.0).clamp(0.25, 64.0),
            playhead: options.number("playhead").unwrap_or(0.25).clamp(0.0, 1.0),
            labels: options.flag("labels").unwrap_or(true),
        }
    }
}

/// One row: the events of one sound in one lane.
struct Row<'e> {
    /// The labeled statement, by its place in the score. `None` outside
    /// every one.
    lane: Option<usize>,
    /// The `s` the events name, without its `:n`. Empty when they name none.
    sound: &'e str,
    /// The lowest and highest pitch, in MIDI notes, when the events have one.
    pitch: Option<(f32, f32)>,
}

impl Widget for Lanes<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let request = self.request;
        let state = request.state;
        let theme = request.theme;
        let Some((now_seconds, cycle_now, cps)) = state.current_clock() else {
            note(request, buffer, area, "waiting for the first beat");
            return;
        };
        if area.width < 4 || area.height == 0 {
            return;
        }
        let begin = cycle_now - self.cycles * self.playhead;
        let end = begin + self.cycles;
        let placed = placed_events(state, request.slot, begin, end, now_seconds, cps);

        // Each event's row, and its pitch when it has one.
        let mut rows: Vec<Row<'_>> = Vec::new();
        let mut marks = Vec::with_capacity(placed.len());
        for entry in &placed {
            let lane = lane_of(&state.lanes, &entry.event.context);
            let sound = sound_of(entry.event);
            let index = rows
                .iter()
                .position(|row| row.lane == lane && row.sound == sound)
                .unwrap_or_else(|| {
                    rows.push(Row {
                        lane,
                        sound,
                        pitch: None,
                    });
                    rows.len() - 1
                });
            let midi = event_pitch(entry.event);
            if let Some(midi) = midi {
                let row = &mut rows[index];
                row.pitch = Some(
                    row.pitch
                        .map_or((midi, midi), |(low, high)| (low.min(midi), high.max(midi))),
                );
            }
            marks.push((index, midi));
        }
        // Lanes in score order with the unlabeled last, and sounds by name
        // within a lane.
        let mut order = (0..rows.len()).collect::<Vec<_>>();
        order.sort_by_key(|&index| {
            (
                rows[index].lane.is_none(),
                rows[index].lane,
                rows[index].sound,
            )
        });
        let mut ranks = vec![0; rows.len()];
        for (rank, &index) in order.iter().enumerate() {
            ranks[index] = rank;
        }
        let palette = [
            theme.event,
            theme.accent,
            theme.syntax.string,
            theme.syntax.number,
            theme.syntax.keyword.unwrap_or(theme.warn),
        ];
        // Each row's colour, by its lane, and its mark, by its sound.
        let mut colors = vec![theme.event; rows.len()];
        let mut hits = vec![HITS[0]; rows.len()];
        let mut sounds: Vec<&str> = Vec::new();
        let mut lane_count = 0;
        for (rank, &index) in order.iter().enumerate() {
            let row = &rows[index];
            let previous = rank.checked_sub(1).map(|rank| rows[order[rank]].lane);
            if rank > 0 && (row.lane.is_none() || previous != Some(row.lane)) {
                lane_count += 1;
            }
            colors[index] = palette[lane_count % palette.len()];
            let sound = sounds
                .iter()
                .position(|sound| *sound == row.sound)
                .unwrap_or_else(|| {
                    sounds.push(row.sound);
                    sounds.len() - 1
                });
            hits[index] = HITS[sound % HITS.len()];
        }

        // The caps take the first and last rows when the rows leave room
        // for them. The rows share what is left, each on the middle line of
        // its share.
        let caps = usize::from(area.height) >= rows.len().max(1) + 2;
        let inner = if caps {
            Rect::new(area.x, area.y + 1, area.width, area.height - 2)
        } else {
            area
        };
        let shown = rows.len().clamp(1, usize::from(inner.height));
        let share = inner.height / shown as u16;
        let line = |rank: usize| inner.y + rank as u16 * share + (share - 1) / 2;
        let width = f64::from(area.width);
        // A cell column, one past either edge at most, so an event that
        // started long ago still has a start to draw from.
        let column = |cycle: f64| {
            ((cycle - begin) / self.cycles * width)
                .floor()
                .clamp(-1.0, width + 1.0) as i64
        };
        let playhead_x = area.x + ((self.playhead * width) as u16).min(area.width - 1);

        // A dotted line under each row, with a colon where a cycle starts.
        for rank in 0..shown {
            let y = line(rank);
            for x in area.x..area.right() {
                put(buffer, x, y, '·', theme.grid);
            }
            let mut cycle = begin.ceil();
            while cycle < end {
                if let Ok(x) = u16::try_from(column(cycle))
                    && x < area.width
                {
                    put(buffer, area.x + x, y, ':', theme.grid);
                }
                cycle += 1.0;
            }
        }
        for y in area.y..area.bottom() {
            put(buffer, playhead_x, y, '┊', theme.playhead);
        }

        // What sounds now is drawn last, over anything it shares a cell
        // with.
        for active in [false, true] {
            for (entry, &(index, midi)) in placed.iter().zip(&marks) {
                let rank = ranks[index];
                if entry.active != active || rank >= shown {
                    continue;
                }
                let base = request.event_color(entry.event).unwrap_or(colors[index]);
                let base = mix(request.background, base, gain_alpha(entry.event));
                let color = if entry.active {
                    brighten(base)
                } else if entry.end <= cycle_now {
                    dim(base, theme)
                } else {
                    base
                };
                let from = column(entry.begin);
                let (glyph, to) = match (midi, rows[index].pitch) {
                    (Some(midi), Some((low, high))) => {
                        let level = if high - low < 1.0 {
                            3
                        } else {
                            (((midi - low) / (high - low)) * 7.0).round() as usize
                        };
                        // A held note fills its cells, less one at its end
                        // so repeated notes stay apart.
                        let to = column(entry.end).max(from + 1);
                        (
                            LEVELS[level.min(7)],
                            if to - from >= 3 { to - 1 } else { to },
                        )
                    }
                    _ => (hits[index], from + 1),
                };
                for x in from.max(0)..to.min(i64::from(area.width)) {
                    put(buffer, area.x + x as u16, line(rank), glyph, color);
                }
            }
        }

        if self.labels {
            let room = usize::from(playhead_x - area.x)
                .saturating_sub(1)
                .min(MAX_LABEL);
            for (rank, &index) in order.iter().take(shown).enumerate() {
                let label = row_label(&rows, index, &state.lanes);
                if label.is_empty() || room == 0 {
                    continue;
                }
                buffer.set_stringn(
                    area.x,
                    line(rank),
                    format!("{label} "),
                    room,
                    Style::default().fg(colors[index]),
                );
            }
        }

        // The cycle the playhead is on, beside it, on the free line nearest
        // half way down.
        if inner.height >= 3 {
            let middle = inner.y + (inner.height - 1) / 2;
            let taken = |y: u16| (0..shown).any(|rank| line(rank) == y);
            let y = (inner.y..inner.bottom())
                .filter(|&y| !taken(y))
                .min_by_key(|&y| y.abs_diff(middle))
                .unwrap_or(middle);
            let text = format!("{cycle_now:.1} ");
            let from = playhead_x.saturating_sub(text.len() as u16).max(area.x);
            buffer.set_stringn(
                from,
                y,
                &text,
                usize::from(playhead_x - from),
                Style::default().fg(theme.accent),
            );
            let (diamond, color) = if state.running() {
                ('◆', theme.ok)
            } else {
                ('◇', theme.muted)
            };
            put(buffer, playhead_x, y, diamond, color);
        }
        if caps {
            put(buffer, playhead_x, area.y, '▼', theme.playhead);
            put(buffer, playhead_x, area.bottom() - 1, '▲', theme.playhead);
        }
    }
}

fn put(buffer: &mut Buffer, x: u16, y: u16, symbol: char, color: Color) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.set_char(symbol).set_fg(color);
    }
}

/// The labeled statement an event comes from: the first of its source
/// ranges that starts inside one.
fn lane_of(lanes: &[(Range<usize>, Arc<str>)], context: &[(usize, usize)]) -> Option<usize> {
    context.iter().find_map(|&(from, _)| {
        let index = lanes
            .partition_point(|(span, _)| span.start <= from)
            .checked_sub(1)?;
        lanes[index].0.contains(&from).then_some(index)
    })
}

/// The sound an event names: `bd` for `s:bd n:3` and for `s:bd:3`.
fn sound_of(event: &UiScheduledEvent) -> &str {
    event
        .value
        .as_deref()
        .and_then(|value| {
            value
                .split_whitespace()
                .find_map(|field| field.strip_prefix("s:"))
        })
        .map_or("", |sound| sound.split(':').next().unwrap_or(sound))
}

/// What a row is called. A row of a lane the score named takes the lane's
/// label, followed by the sound when the lane has more than one row. Any
/// other row takes its sound, since `$` lanes have no name of their own.
fn row_label(rows: &[Row<'_>], index: usize, lanes: &[(Range<usize>, Arc<str>)]) -> String {
    let row = &rows[index];
    let named = row
        .lane
        .map(|lane| &*lanes[lane].1)
        .filter(|label| !label.contains('$'));
    match named {
        Some(label) if rows.iter().filter(|other| other.lane == row.lane).count() > 1 => {
            format!("{label} {}", row.sound).trim_end().to_owned()
        }
        Some(label) => label.to_owned(),
        None => row.sound.to_owned(),
    }
}
